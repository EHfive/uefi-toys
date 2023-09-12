use super::*;

use uefi_services::system_table;

#[repr(C)]
#[derive(Debug)]
#[unsafe_protocol("18a031ab-b443-4d1a-a5c0-0c09261e9f71")]
pub struct DriverBindingProtocol {
    pub supported: unsafe extern "efiapi" fn(
        this: *mut DriverBindingProtocol,
        controller: RawHandle,
        remaining: *mut FfiDevicePath,
    ) -> Status,
    pub start: unsafe extern "efiapi" fn(
        this: *mut DriverBindingProtocol,
        controller: RawHandle,
        remaining: *mut FfiDevicePath,
    ) -> Status,
    pub stop: unsafe extern "efiapi" fn(
        this: *mut DriverBindingProtocol,
        controller: RawHandle,
        num_children: usize,
        child_handle_buf: *mut RawHandle,
    ) -> Status,
    pub version: u32,
    pub image_handle: RawHandle,
    pub driver_binding_handle: RawHandle,
}

unsafe extern "efiapi" fn supported(
    this: *mut DriverBindingProtocol,
    controller: RawHandle,
    remaining: *mut FfiDevicePath,
) -> Status {
    if this.is_null() || controller.is_null() {
        return Status::INVALID_PARAMETER;
    }

    let ctx = &*container_of!(this, ControlContext, driver_binding);
    let remaining = (!remaining.is_null()).then(|| DevicePath::from_ffi_ptr(remaining));

    // this driver managing itself
    if controller == ctx.bus_handle.as_ptr() {
        if let Some(remaining) = remaining {
            if remaining.node_iter().next().is_some() {
                return Status::UNSUPPORTED;
            }
        }
        return Status::SUCCESS;
    }
    Status::UNSUPPORTED
}

unsafe extern "efiapi" fn start(
    this: *mut DriverBindingProtocol,
    controller: RawHandle,
    remaining: *mut FfiDevicePath,
) -> Status {
    if this.is_null() || controller.is_null() {
        return Status::INVALID_PARAMETER;
    }

    let _ctx = &mut *container_of!(this, ControlContext, driver_binding);
    let bt = system_table().as_ref().boot_services();
    let remaining = (!remaining.is_null()).then(|| DevicePath::from_ffi_ptr(remaining));

    use uefi::proto::device_path::text::{AllowShortcuts, DisplayOnly};

    log::debug!(
        "{:?} {}",
        controller,
        remaining
            .map(|i| i
                .to_string(bt, DisplayOnly(true), AllowShortcuts(false))
                .unwrap()
                .unwrap())
            .unwrap_or_default()
    );

    log::debug!("start");
    Status::SUCCESS
}

unsafe extern "efiapi" fn stop(
    this: *mut DriverBindingProtocol,
    controller: RawHandle,
    num_children: usize,
    child_handle_buf: *mut RawHandle,
) -> Status {
    if this.is_null() || controller.is_null() {
        return Status::INVALID_PARAMETER;
    }

    let ctx = &mut *container_of!(this, ControlContext, driver_binding);
    let children = core::slice::from_raw_parts(child_handle_buf, num_children);

    for &child in children {
        let status = (ctx.loop_ctl.remove)(ptr::addr_of_mut!(ctx.loop_ctl), child);
        if status != Status::SUCCESS {
            log::error!("failed to stop loop {:?}", child);
            return status;
        }
    }

    log::debug!("stop {}", num_children);
    Status::SUCCESS
}

pub fn create_driver_binding(bus_handle: Handle) -> DriverBindingProtocol {
    let bt = unsafe { system_table().as_ref().boot_services() };
    DriverBindingProtocol {
        supported,
        start,
        stop,
        version: 0x10,
        image_handle: bt.image_handle().as_ptr(),
        driver_binding_handle: bus_handle.as_ptr(),
    }
}
