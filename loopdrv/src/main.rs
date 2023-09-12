#![no_main]
#![no_std]

extern crate alloc;

use uefi::prelude::*;
use uefi::proto::loaded_image::LoadedImage;
use uefi_services::system_table;

const MIN_UEFI_REVISION: uefi::table::Revision = uefi::table::Revision::EFI_2_00;

static mut EVENT: Option<uefi::Event> = None;

#[entry]
fn main(_handle: Handle, mut system_table: SystemTable<Boot>) -> Status {
    unsafe {
        EVENT = uefi_services::init(&mut system_table).unwrap();
    }
    let bt = system_table.boot_services();

    if system_table.uefi_revision() < MIN_UEFI_REVISION {
        log::error!(
            "system UEFI revision {} smaller than required {}",
            system_table.uefi_revision(),
            MIN_UEFI_REVISION
        );
        return Status::INCOMPATIBLE_VERSION;
    }

    match uefi_loopdrv::install_loop_control(Some(bt.image_handle())) {
        Err(e) => return e.status(),
        Ok(_h) => {}
    }

    let mut image = bt
        .open_protocol_exclusive::<LoadedImage>(bt.image_handle())
        .unwrap();
    unsafe { image.set_unload(unload) };
    Status::SUCCESS
}

extern "efiapi" fn unload(_handle: Handle) -> Status {
    let bt = unsafe { system_table().as_ref().boot_services() };
    if let Some(event) = unsafe { EVENT.take() } {
        bt.close_event(event).unwrap();
    }
    uefi_loopdrv::uninstall_loop_control(bt.image_handle()).status()
}
