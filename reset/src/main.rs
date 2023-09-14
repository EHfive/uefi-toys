#![no_main]
#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use bitflags::{bitflags, Flags};
use bytemuck::{Pod, Zeroable};
use core::option_env;
use getargs::{Arg, Options};
use uefi::prelude::*;
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::shell_params::ShellParameters;
use uefi::table::runtime::{ResetType, VariableAttributes, VariableVendor};
use uefi::Guid;
use uefi_services::println;

bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
    struct OsIndications: u64 {
        const BOOT_TO_FW_UI = 0x0001;
        const TIMESTAMP_REVOCATION = 0x0002;
        const FILE_CAPSULE_DELIVERY_SUPPORTED = 0x0004;
        const FMP_CAPSULE_SUPPORTED = 0x0008;
        const CAPSULE_RESULT_VAR_SUPPORTED = 0x0010;
        const START_OS_RECOVERY = 0x0020;
        const START_PLATFORM_RECOVERY = 0x0040;
        const JSON_CONFIG_DATA_REFRESH = 0x0080;
    }
}

const MIN_UEFI_REVISION: uefi::table::Revision = uefi::table::Revision::EFI_2_00;

macro_rules! format_help {
    ($name:expr) => {
        ::core::format_args!(
            "\
Usage: {name} <COMMAND> [OPTIONS]

  Reset the system with OS indications flag set

  -h, --help            Print this help and exit

Commands:
  reset                 Reset system only
  firmware              Boot to firmware
  os-recovery           Start OS recovery
  platform-recovery     Start platform recovery
  flags                 List OS indication flags

Options:
  -t, --type TYPE       Reset type, should be one of `cold`, `warm`, `shutdown`
                        or GUID that describe platform specific reset type,
                        defaults to `cold`
  -f, --force           Force the operation even the support was not announced
  -c, --clear           Clear OS indication flags for \"reset\" command

EXAMPLE:
  * Example
  {name}
",
            name = $name
        )
    };
}

#[derive(Debug)]
enum ArgsError<'a> {
    Invalid,
    GetArgs(getargs::Error<&'a str>),
}
impl core::fmt::Display for ArgsError<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::GetArgs(e) => e.fmt(f),
            Self::Invalid => f.write_str("Invalid argument"),
        }
    }
}

enum Command {
    NoOp,
    ListOsIndications,
    Reset {
        indication: Option<OsIndications>,
        force: bool,
        reset_type: ResetType,
        platform_guid: Option<Guid>,
    },
}

fn parse_args<'a, I: Iterator<Item = &'a str>>(mut argv_iter: I) -> Result<Command, ArgsError<'a>> {
    let Some(name) = argv_iter.next() else {
        return Err(ArgsError::Invalid);
    };
    let mut opts = Options::new(argv_iter);

    #[inline]
    fn w<T>(res: getargs::Result<&str, T>) -> Result<T, ArgsError<'_>> {
        res.map_err(ArgsError::GetArgs)
    }

    enum CommandType {
        NoOp,
        ListOsIndications,
        Reset,
        Firmware,
        OsRecovery,
        PlatformRecovery,
    }

    let mut command_type = CommandType::NoOp;
    let mut reset_type = ResetType::COLD;
    let mut platform_guid = None;
    let mut clear = false;
    let mut force = false;
    while let Some(arg) = w(opts.next_arg())? {
        match arg {
            Arg::Short('h') | Arg::Long("help") => {
                println!("{}", format_help!(name));
                return Ok(Command::NoOp);
            }
            Arg::Short('t') | Arg::Long("type") => {
                let t = w(opts.value())?;
                reset_type = if t.eq_ignore_ascii_case("cold") {
                    ResetType::COLD
                } else if t.eq_ignore_ascii_case("warm") {
                    ResetType::WARM
                } else if t.eq_ignore_ascii_case("shutdown") {
                    ResetType::SHUTDOWN
                } else {
                    let Ok(guid) = Guid::try_parse(t) else {
                        println!("Unknown reset type: {}", t);
                        return Err(ArgsError::Invalid);
                    };
                    platform_guid = Some(guid);
                    ResetType::PLATFORM_SPECIFIC
                };
            }
            Arg::Short('f') | Arg::Long("force") => {
                force = true;
            }
            Arg::Short('c') | Arg::Long("clear") => {
                clear = true;
            }
            Arg::Positional(cmd) => {
                command_type = if cmd.eq_ignore_ascii_case("flags") {
                    CommandType::ListOsIndications
                } else if cmd.eq_ignore_ascii_case("reset") {
                    CommandType::Reset
                } else if cmd.eq_ignore_ascii_case("firmware") {
                    CommandType::Firmware
                } else if cmd.eq_ignore_ascii_case("os-recovery") {
                    CommandType::OsRecovery
                } else if cmd.eq_ignore_ascii_case("platform-recovery") {
                    CommandType::PlatformRecovery
                } else {
                    println!("Unexpected argument {}", arg);
                    return Err(ArgsError::Invalid);
                };
            }
            _ => {
                println!("Unexpected argument {}", arg);
                return Err(ArgsError::Invalid);
            }
        }
    }

    let indication = match command_type {
        CommandType::NoOp => {
            println!("{}", format_help!(name));
            return Ok(Command::NoOp);
        }
        CommandType::ListOsIndications => return Ok(Command::ListOsIndications),
        CommandType::Reset => clear.then_some(OsIndications::empty()),
        CommandType::Firmware => Some(OsIndications::BOOT_TO_FW_UI),
        CommandType::OsRecovery => Some(OsIndications::START_OS_RECOVERY),
        CommandType::PlatformRecovery => Some(OsIndications::START_PLATFORM_RECOVERY),
    };

    Ok(Command::Reset {
        indication,
        force,
        reset_type,
        platform_guid,
    })
}

#[entry]
fn main(_handle: Handle, mut system_table: SystemTable<Boot>) -> Status {
    uefi_services::init(&mut system_table).unwrap();
    let bt = system_table.boot_services();
    let rt = system_table.runtime_services();

    if system_table.uefi_revision() < MIN_UEFI_REVISION {
        log::error!(
            "system UEFI revision {} smaller than required {}",
            system_table.uefi_revision(),
            MIN_UEFI_REVISION
        );
        return Status::INCOMPATIBLE_VERSION;
    }

    let image = bt
        .open_protocol_exclusive::<LoadedImage>(bt.image_handle())
        .unwrap();

    let sh_params = bt
        .open_protocol_exclusive::<ShellParameters>(bt.image_handle())
        .ok();
    let mut argv: Vec<String> = if let Some(sh_params) = sh_params {
        sh_params
            .args()
            .map(|arg| {
                let mut buf = String::new();
                arg.as_str_in_buf(&mut buf).unwrap();
                buf
            })
            .collect()
    } else if let Ok(load_options) = image.load_options_as_cstr16() {
        let mut load_options_str = String::new();
        load_options_str.reserve(load_options.num_chars());
        if load_options.as_str_in_buf(&mut load_options_str).is_ok() {
            uefi_shell_split::split(load_options_str.as_str())
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    if argv.len() <= 1 {
        if let Some(default) = option_env!("UEFI_RESET_DEFAULT_CMD") {
            argv = uefi_shell_split::split(default)
        }
    }
    if argv.is_empty() {
        log::error!("Command-line options not passed");
        return Status::INVALID_PARAMETER;
    }
    let argv = argv.iter().map(|i| i.as_str());

    let res = match parse_args(argv) {
        Err(e) => {
            println!("{}", e);
            return Status::INVALID_PARAMETER;
        }
        Ok(Command::NoOp) => uefi::Result::Ok(()),
        Ok(Command::ListOsIndications) => list_os_indications(rt),
        Ok(Command::Reset {
            indication,
            force,
            reset_type,
            platform_guid,
        }) => reset(rt, indication, force, reset_type, platform_guid),
    };

    res.status()
}

const OS_INDICATIONS_SUPPORTED: &uefi::CStr16 = cstr16!("OsIndicationsSupported");
const OS_INDICATIONS: &uefi::CStr16 = cstr16!("OsIndications");

fn list_os_indications(rt: &RuntimeServices) -> uefi::Result {
    let mut supported = OsIndications::empty();
    if let Err(e) = rt.get_variable(
        OS_INDICATIONS_SUPPORTED,
        &VariableVendor::GLOBAL_VARIABLE,
        bytemuck::bytes_of_mut(&mut supported),
    ) {
        if e.status() == Status::NOT_FOUND {
            println!("UEFI variable \"OsIndicationsSupported\" not set")
        }
        return Err(e);
    }
    let mut os_indications = OsIndications::empty();
    if let Err(e) = rt.get_variable(
        OS_INDICATIONS,
        &VariableVendor::GLOBAL_VARIABLE,
        bytemuck::bytes_of_mut(&mut os_indications),
    ) {
        if e.status() != Status::NOT_FOUND {
            return Err(e);
        }
    }

    for flag in OsIndications::FLAGS {
        let supported = supported.contains(*flag.value());
        let set = os_indications.contains(*flag.value());
        println!("{}", flag.name());
        println!("    Flag: 0x{:08x}", flag.value().bits(),);
        if supported || set {
            println!(
                "    {}{}",
                if supported { "Supported" } else { "" },
                if set { ", Set" } else { "" }
            );
        }
        println!("")
    }
    Ok(())
}

fn reset(
    rt: &RuntimeServices,
    indication: Option<OsIndications>,
    no_check: bool,
    reset_type: ResetType,
    platform_guid: Option<Guid>,
) -> uefi::Result {
    if let Some(indication) = indication {
        let supported = if no_check {
            OsIndications::all()
        } else {
            let mut supported = OsIndications::empty();
            rt.get_variable(
                OS_INDICATIONS_SUPPORTED,
                &VariableVendor::GLOBAL_VARIABLE,
                bytemuck::bytes_of_mut(&mut supported),
            )
            .map_err(|e| {
                println!("UEFI variable \"OsIndicationsSupported\" not set: {}", e);
                e
            })?;
            supported
        };
        if !supported.contains(indication) {
            println!("Flag {:?} not supported", indication);
            return Status::ABORTED.to_result();
        }
        rt.set_variable(
            OS_INDICATIONS,
            &VariableVendor::GLOBAL_VARIABLE,
            VariableAttributes::NON_VOLATILE
                | VariableAttributes::BOOTSERVICE_ACCESS
                | VariableAttributes::RUNTIME_ACCESS,
            bytemuck::bytes_of(&indication),
        )
        .map_err(|e| {
            println!("Failed to set UEFI variable OSIndications: {}", e);
            e
        })?;
    }

    let reason = match reset_type {
        ResetType::COLD => cstr16!("cold"),
        ResetType::WARM => cstr16!("warm"),
        ResetType::SHUTDOWN => cstr16!("shutdown"),
        ResetType::PLATFORM_SPECIFIC => cstr16!("platform"),
        _ => unimplemented!(),
    };

    let mut data: Vec<u8>;
    let data = if let Some(guid) = platform_guid {
        assert_eq!(reset_type, ResetType::PLATFORM_SPECIFIC);
        let guid = guid.to_bytes();
        data = Vec::new();
        data.reserve(reason.as_bytes().len() + guid.len());
        data.extend(reason.as_bytes());
        data.extend(guid);
        data.as_slice()
    } else {
        reason.as_bytes()
    };

    // TODO: wait for several seconds to cancel on any keyboard input
    rt.reset(reset_type, Status::SUCCESS, Some(data))
}
