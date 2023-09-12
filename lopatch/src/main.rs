#![no_main]
#![no_std]

mod command;
mod utils;
use command::attach::PatchAction;

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use getargs::{Arg, Options};
use regex::{Regex, RegexBuilder};
use uefi::prelude::*;
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::shell_params::ShellParameters;
use uefi_services::println;

const MIN_UEFI_REVISION: uefi::table::Revision = uefi::table::Revision::EFI_2_00;

macro_rules! format_help {
    ($name:expr) => {
        ::core::format_args!(
            "\
Usage: {name} [OPTIONS] IMAGE_FILE

  Setup a loopback device for IMAGE_FILE with optional ISO file
  patching for IMAGE_FILE contains an iso9660 filesystem

  -h, --help            Print this help and exit
  -i, --id NUM          Loopback ID to use, find a free one if omitted
  -r, --read-only       Mark read-only
  -P                    Mark that IMAGE_FILE has disk partitioning
  -l, --list            List all loopback devices
  -d, --detach          Detach the loopback device specified by -i/--id

ISO Patching Options:
  -s, --search PATH     Search file in ISO to patch, each --search/--pattern
                        should followed with one or more action options, i.e.
                        --append, --meta-cpio or --replace. A file matches if
                        PATH is a valid file path relative to any parent
                        directory. The action would applies to all files found.
  -p, --pattern REGEX   Use regular expression instead to match file path
  -a, --append FILE     Append FILE data to end of the matched ISO file
  -m, --meta-cpio       Append mapping metadata file as CPIO
  -R, --replace FILE    Replace data of the matched ISO file with FILE data

EXAMPLE:
  * Append a cpio to initramfs file in Live CD ISO and setup loopback
  {name} -r -s initramfs-linux.img -a patch-init.cpio archlinux.iso

  * Attach an FAT image to a free loopback device
  {name} fat.img
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

enum Command<'a> {
    NoOp,
    List,
    Detach(u32),
    Attach {
        loop_id: Option<u32>,
        read_only: bool,
        is_parted_disk: bool,
        patch: Vec<(Regex, Vec<PatchAction<'a>>)>,
        image_file: &'a str,
    },
}

fn parse_args<'a, I: Iterator<Item = &'a str>>(
    mut argv_iter: I,
) -> Result<Command<'a>, ArgsError<'a>> {
    let Some(name) =  argv_iter.next() else {
        return Err(ArgsError::Invalid);
    };
    let mut opts = Options::new(argv_iter);

    let mut loop_id: Option<u32> = None;
    let mut read_only: bool = false;
    let mut is_parted_disk: bool = false;
    let mut patch_list = Vec::<(Regex, Vec<PatchAction<'a>>)>::new();
    let mut image_file = "";

    let mut is_list = false;
    let mut is_detach = false;

    #[inline]
    fn w<T>(res: getargs::Result<&str, T>) -> Result<T, ArgsError<'_>> {
        res.map_err(ArgsError::GetArgs)
    }

    let build_regex = |pat: &str| RegexBuilder::new(pat).case_insensitive(true).build();

    let mut count = 0;
    while let Some(arg) = w(opts.next_arg())? {
        match arg {
            Arg::Short('h') | Arg::Long("help") => {
                println!("{}", format_help!(name));
                return Ok(Command::NoOp);
            }
            Arg::Short('i') | Arg::Long("id") => {
                let id = match w(opts.value())?.parse() {
                    Ok(v) => v,
                    Err(e) => {
                        println!("{}", e);
                        return Err(ArgsError::Invalid);
                    }
                };
                loop_id = Some(id);
            }
            Arg::Short('r') | Arg::Long("read-only") => read_only = true,
            Arg::Short('P') => is_parted_disk = true,
            Arg::Short('l') | Arg::Long("list") => is_list = true,
            Arg::Short('d') | Arg::Long("detach") => is_detach = true,
            Arg::Short('s') | Arg::Long("search") => {
                let path = w(opts.value())?.trim();
                let pat = alloc::format!(
                    "{}{}$",
                    if path.starts_with('/') { "^" } else { "/" },
                    regex::escape(path)
                );
                match build_regex(&pat) {
                    Err(e) => {
                        log::error!("{}", e);
                        return Err(ArgsError::Invalid);
                    }
                    Ok(re) => patch_list.push((re, Vec::new())),
                };
            }
            Arg::Short('p') | Arg::Long("pattern") => {
                match build_regex(w(opts.value())?) {
                    Err(e) => {
                        log::error!("{}", e);
                        return Err(ArgsError::Invalid);
                    }
                    Ok(re) => patch_list.push((re, Vec::new())),
                };
            }
            Arg::Short('m') | Arg::Long("meta-cpio") => {
                let last = patch_list.last_mut().ok_or(ArgsError::Invalid)?;
                last.1.push(PatchAction::MetaCpio)
            }
            Arg::Short('a') | Arg::Long("append") => {
                let last = patch_list.last_mut().ok_or(ArgsError::Invalid)?;
                last.1.push(PatchAction::Append(w(opts.value())?))
            }
            Arg::Short('R') | Arg::Long("replace") => {
                let last = patch_list.last_mut().ok_or(ArgsError::Invalid)?;
                last.1.push(PatchAction::Replace(w(opts.value())?))
            }
            Arg::Positional(path) => {
                image_file = path;
            }
            _ => {
                println!("Unexpected argument {}", arg);
                return Err(ArgsError::Invalid);
            }
        }
        count += 1;
    }
    if count == 0 {
        println!("{}", format_help!(name));
        return Ok(Command::NoOp);
    }

    if is_detach && is_list {
        return Err(ArgsError::Invalid);
    }
    if is_detach {
        let id = match loop_id {
            None => {
                println!("Specify ID of loopback to detach with -i/--id");
                return Err(ArgsError::Invalid);
            }
            Some(v) => v,
        };
        return Ok(Command::Detach(id));
    }
    if is_list {
        return Ok(Command::List);
    }

    if image_file.is_empty() {
        println!("{}", format_help!(name));
        return Err(ArgsError::Invalid);
    }

    patch_list.retain(|i| !i.1.is_empty());

    Ok(Command::Attach {
        loop_id,
        read_only,
        is_parted_disk,
        patch: patch_list,
        image_file,
    })
}

#[entry]
fn main(_handle: Handle, mut system_table: SystemTable<Boot>) -> Status {
    let event = uefi_services::init(&mut system_table).unwrap();
    let bt = system_table.boot_services();

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

    let load_options = image.load_options_as_cstr16().unwrap();
    let mut load_options_str = String::new();
    load_options_str.reserve(load_options.num_chars());
    load_options.as_str_in_buf(&mut load_options_str).unwrap();

    let sh_params = bt
        .open_protocol_exclusive::<ShellParameters>(bt.image_handle())
        .ok();
    let argv: Vec<String> = if let Some(sh_params) = sh_params {
        sh_params
            .args()
            .map(|arg| {
                let mut buf = String::new();
                arg.as_str_in_buf(&mut buf).unwrap();
                buf
            })
            .collect()
    } else {
        // FIXME: use a UEFI-split
        winsplit::split(&load_options_str)
    };
    if argv.is_empty() {
        log::error!("Command-line options not passed");
        return Status::INVALID_PARAMETER;
    }
    let argv = argv.iter().map(|i| i.as_str());

    match parse_args(argv) {
        Err(e) => {
            println!("{}", e);
            return Status::INVALID_PARAMETER;
        }
        Ok(Command::NoOp) => {}
        Ok(Command::List) => {
            if let Err(e) = command::list::list_loop_devices(bt) {
                println!("Failed to list loop devices: {}", e);
                return e.status();
            }
        }
        Ok(Command::Detach(id)) => {
            if let Err(e) = command::detach::detach_loop_device(bt, id) {
                println!("Failed to detach loop device #{}: {}", id, e);
                return e.status();
            }
        }
        Ok(Command::Attach {
            loop_id,
            read_only,
            is_parted_disk,
            patch,
            image_file,
        }) => {
            if let Err(e) = command::attach::attach_loop_device(
                bt,
                loop_id,
                read_only,
                !is_parted_disk,
                &patch,
                image_file,
            ) {
                println!("Failed to setup loop device: {}", e);
                return e.status();
            }
        }
    };
    if let Some(event) = event {
        bt.close_event(event).unwrap();
    }
    Status::SUCCESS
}
