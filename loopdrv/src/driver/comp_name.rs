use super::*;

use uefi::{CStr16, CStr8};
use uefi_raw::protocol::driver::ComponentName2Protocol;

const SUPPORTED_LANGUAGES: &CStr8 = cstr8!("en-us;en");
const DRIVER_NAME: &CStr16 = cstr16!("Loopback Driver");
const BUS_NAME: &CStr16 = cstr16!("Loopback Controller");

unsafe extern "efiapi" fn get_driver_name(
    _this: *const ComponentName2Protocol,
    _language: *const u8,
    driver_name: *mut *const u16,
) -> Status {
    *driver_name = DRIVER_NAME.as_ptr() as _;
    Status::SUCCESS
}

unsafe extern "efiapi" fn get_controller_name(
    _this: *const ComponentName2Protocol,
    _controller_handle: uefi_raw::Handle,
    child_handle: RawHandle,
    _language: *const u8,
    controller_name: *mut *const u16,
) -> Status {
    let bt = system_table().as_ref().boot_services();

    if let Some(child_handle) = Handle::from_ptr(child_handle) {
        let loop_pt_ptr = match get_protocol_mut::<LoopProtocol>(bt, child_handle) {
            Err(e) => return e.status(),
            Ok(Some(p)) => p,
            _ => return Status::INVALID_PARAMETER,
        };
        let ctx = loopback::LoopContext::from_loop_pt_ptr(loop_pt_ptr);
        *controller_name = ctx.name_ptr() as _;
    } else {
        *controller_name = BUS_NAME.as_ptr() as _;
    }

    Status::SUCCESS
}

pub fn create_comp_name() -> ComponentName2Protocol {
    ComponentName2Protocol {
        get_driver_name,
        get_controller_name,
        supported_languages: SUPPORTED_LANGUAGES.as_ptr() as _,
    }
}
