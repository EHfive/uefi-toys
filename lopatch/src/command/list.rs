use super::*;

pub fn list_loop_devices(bt: &BootServices) -> Result {
    let loop_handles = bt.locate_handle_buffer(SearchType::ByProtocol(&LoopProtocol::GUID))?;

    for &handle in loop_handles.iter() {
        let loop_pt = bt.open_protocol_exclusive::<LoopProtocol>(handle)?;
        let mut info = uefi_loopdrv::LoopInfo::default();
        unsafe {
            (loop_pt.get_info)(loop_pt.get_mut().unwrap(), &mut info).to_result()?;
        }

        println!(
            "loop({}) 0x{:x}",
            info.unit_number,
            handle.as_ptr() as usize
        );
    }

    Ok(())
}
