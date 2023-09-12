mod binding;
mod comp_name;
mod dev_path;
mod loop_ctl;
mod loopback;

pub use loop_ctl::LoopControlProtocol;
pub use loopback::{LoopInfo, LoopMappingItem, LoopProtocol, LoopTarget, SECTOR_SIZE};

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::{mem, ptr};

use uefi::prelude::*;
use uefi::proto::device_path::FfiDevicePath;
use uefi::proto::unsafe_protocol;

use uefi::proto::device_path::DevicePath;
use uefi::table::boot::{OpenProtocolAttributes, OpenProtocolParams};
use uefi::Result;
use uefi::{Identify, Status};
use uefi_raw::protocol::driver::ComponentName2Protocol;
use uefi_raw::Guid;
use uefi_raw::Handle as RawHandle;
use uefi_services::system_table;

#[repr(C)]
struct ControlContext {
    dev_path: dev_path::LoopControlPath,
    driver_binding: binding::DriverBindingProtocol,
    comp_name: ComponentName2Protocol,
    loop_ctl: LoopControlProtocol,
    bus_handle: Handle,
    protocols: Vec<(Guid, *mut c_void)>,
    loop_list: Vec<(u32, Handle, *mut loopback::LoopContext)>,
}

pub fn install_loop_control(handle: Option<Handle>) -> Result<Handle> {
    let bt = unsafe { system_table().as_ref().boot_services() };
    let invalid_handle = unsafe { Handle::from_ptr(mem::align_of::<Handle>() as _).unwrap() };

    if bt.get_handle_for_protocol::<LoopControlProtocol>().is_ok() {
        log::error!("Loop control protocol already exists, aborting");
        return Err(uefi::Error::new(Status::ALREADY_STARTED, ()));
    }

    let mut ctx = Box::new(ControlContext {
        dev_path: dev_path::LoopControlPath::new(),
        driver_binding: binding::create_driver_binding(invalid_handle),
        comp_name: comp_name::create_comp_name(),
        loop_ctl: loop_ctl::create_loop_control(),
        bus_handle: invalid_handle,
        loop_list: vec![],
        protocols: vec![],
    });

    let res = unsafe {
        ctx.protocols = vec![
            (DevicePath::GUID, ptr::addr_of_mut!(ctx.dev_path).cast()),
            (
                binding::DriverBindingProtocol::GUID,
                ptr::addr_of_mut!(ctx.driver_binding).cast(),
            ),
            (
                ComponentName2Protocol::GUID,
                ptr::addr_of_mut!(ctx.comp_name).cast(),
            ),
            (
                LoopControlProtocol::GUID,
                ptr::addr_of_mut!(ctx.loop_ctl).cast(),
            ),
        ];
        install_multiple_protocols(bt, handle, &ctx.protocols)
    };
    let handle = match res {
        Ok(handle) => handle.expect("no protocol specified"),
        Err(e) => {
            let (protocol, interface) = e.data();
            log::error!("failed to install protocol {} {:?}", protocol, interface);
            return Err(e.to_err_without_payload());
        }
    };

    ctx.driver_binding.driver_binding_handle = handle.as_ptr();
    ctx.bus_handle = handle;

    let _ = Box::into_raw(ctx);
    Ok(handle)
}

pub fn uninstall_loop_control(bus_handle: Handle) -> Result {
    unsafe {
        let bt = system_table().as_ref().boot_services();
        let loop_ctl_ptr = get_protocol_mut::<LoopControlProtocol>(bt, bus_handle)?.unwrap();
        let ctx = &mut *container_of!(loop_ctl_ptr, ControlContext, loop_ctl);

        loop_ctl::remove_children(ctx)?;

        if let Err(e) = uninstall_multiple_protocols(bt, bus_handle, &ctx.protocols) {
            let (protocol, interface) = e.data();
            log::error!("failed to uninstall protocol {} {:?}", protocol, interface);
            return Err(e.to_err_without_payload());
        };

        let _ = Box::from_raw(ctx);
        Ok(())
    }
}

unsafe fn install_multiple_protocols<'a>(
    bt: &BootServices,
    mut handle: Option<Handle>,
    pairs: &'a [(Guid, *mut c_void)],
) -> Result<Option<Handle>, &'a (Guid, *mut c_void)> {
    let Some((curr, pairs)) = pairs.split_last() else {
        return Ok(None);
    };
    if !pairs.is_empty() {
        handle = install_multiple_protocols(bt, handle, pairs)?
    }

    let (protocol, interface) = curr;
    let res = bt.install_protocol_interface(handle, protocol, *interface);
    match res {
        Ok(h) => handle = Some(h),
        Err(e) => {
            if let Some(handle) = handle {
                uninstall_multiple_protocols(bt, handle, pairs).unwrap();
            }
            return Err(uefi::Error::new(e.status(), curr));
        }
    }
    Ok(handle)
}

unsafe fn uninstall_multiple_protocols<'a>(
    bt: &BootServices,
    handle: Handle,
    pairs: &'a [(Guid, *mut c_void)],
) -> Result<(), &'a (Guid, *mut c_void)> {
    let Some((curr, pairs)) = pairs.split_first() else {
        return Ok(());
    };
    if !pairs.is_empty() {
        uninstall_multiple_protocols(bt, handle, pairs)?;
    }

    let (protocol, interface) = curr;
    let res = bt.uninstall_protocol_interface(handle, protocol, *interface);
    if let Err(e) = res {
        install_multiple_protocols(bt, Some(handle), pairs).unwrap();
        return Err(uefi::Error::new(e.status(), curr));
    }
    Ok(())
}

#[allow(clippy::missing_safety_doc)]
#[inline]
pub unsafe fn get_protocol_mut<P: uefi::proto::Protocol>(
    bt: &BootServices,
    handle: Handle,
) -> Result<Option<*mut P>> {
    let pt = bt.open_protocol::<P>(
        OpenProtocolParams {
            handle,
            agent: handle,
            controller: None,
        },
        OpenProtocolAttributes::GetProtocol,
    )?;
    Ok(pt.get_mut().map(|r| r as *mut _))
}

#[inline]
pub fn get_boot_service_raw(bt: &BootServices) -> &uefi_raw::table::boot::BootServices {
    unsafe { &*(bt as *const BootServices as *const _) }
}

/// Validate if handle is validate and if protocol interface is still the same
#[inline]
fn validate_handle_protocol(
    bt: &BootServices,
    handle: RawHandle,
    protocol: &Guid,
    interface: *const c_void,
) -> bool {
    unsafe {
        let bt = get_boot_service_raw(bt);
        let mut out_interface: *mut c_void = ptr::null_mut();
        let status = (bt.handle_protocol)(handle, protocol, &mut out_interface);
        status == Status::SUCCESS && interface == out_interface
    }
}
