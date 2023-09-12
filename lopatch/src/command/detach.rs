use super::*;

pub fn detach_loop_device(bt: &BootServices, id: u32) -> Result {
    let handle = bt.get_handle_for_protocol::<LoopControlProtocol>()?;
    let loop_ctl = bt.open_protocol_exclusive::<LoopControlProtocol>(handle)?;

    let handle = unsafe {
        let mut handle: RawHandle = ptr::null_mut();
        (loop_ctl.find)(loop_ctl.get_mut().unwrap(), id, &mut handle).to_result()?;
        Handle::from_ptr(handle).unwrap()
    };

    let loop_pt = bt.open_protocol_exclusive::<LoopProtocol>(handle)?;
    unsafe {
        (loop_pt.clear)(loop_pt.get_mut().unwrap()).to_result()?;
    }

    Ok(())
}
