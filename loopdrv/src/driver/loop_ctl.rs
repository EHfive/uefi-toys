use super::*;

#[repr(C)]
#[derive(Debug)]
#[unsafe_protocol("ff0e5a30-438e-11ee-9113-2cf05d73e0d3")]
pub struct LoopControlProtocol {
    pub get_free: unsafe extern "efiapi" fn(this: *mut Self, loop_handle: *mut RawHandle) -> Status,
    pub add: unsafe extern "efiapi" fn(
        this: *mut Self,
        unit_number: u32,
        loop_handle: *mut RawHandle,
    ) -> Status,
    pub find: unsafe extern "efiapi" fn(
        this: *mut Self,
        unit_number: u32,
        loop_handle: *mut RawHandle,
    ) -> Status,
    pub remove: unsafe extern "efiapi" fn(this: *mut Self, loop_handle: RawHandle) -> Status,
}

fn add_loopback(ctx: &mut ControlContext, unit_number: u32) -> Result<Handle> {
    let (handle, loop_ctx) = loopback::install_loopback(ctx.bus_handle, None, unit_number)?;
    ctx.loop_list.push((unit_number, handle, loop_ctx));
    ctx.loop_list.sort_by_key(|i| i.0);

    log::debug!("added loopback({}) {:?}", unit_number, handle);
    Ok(handle)
}

unsafe extern "efiapi" fn get_free(
    this: *mut LoopControlProtocol,
    loop_handle: *mut RawHandle,
) -> Status {
    if this.is_null() || loop_handle.is_null() {
        return Status::INVALID_PARAMETER;
    }

    let ctx = &mut *container_of!(this, ControlContext, loop_ctl);

    let mut free_number = 0;
    for (u, h, loop_ctx) in &ctx.loop_list {
        let loop_ctx = &**loop_ctx;
        if loop_ctx.is_free() {
            *loop_handle = h.as_ptr();
            return Status::SUCCESS;
        }
        if *u == free_number {
            let Some(next) = free_number.checked_add(1) else {
                return Status::ABORTED;
            };
            free_number = next;
        }
    }

    match add_loopback(ctx, free_number) {
        Err(e) => return e.status(),
        Ok(h) => *loop_handle = h.as_ptr(),
    };

    Status::SUCCESS
}

unsafe extern "efiapi" fn find(
    this: *mut LoopControlProtocol,
    unit_number: u32,
    loop_handle: *mut RawHandle,
) -> Status {
    if this.is_null() || loop_handle.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let ctx = &mut *container_of!(this, ControlContext, loop_ctl);

    let res = ctx.loop_list.binary_search_by_key(&unit_number, |i| i.0);
    if let Ok(idx) = res {
        *loop_handle = ctx.loop_list[idx].1.as_ptr();
        return Status::SUCCESS;
    };

    Status::NOT_FOUND
}

unsafe extern "efiapi" fn add(
    this: *mut LoopControlProtocol,
    unit_number: u32,
    loop_handle: *mut RawHandle,
) -> Status {
    if this.is_null() || loop_handle.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let ctx = &mut *container_of!(this, ControlContext, loop_ctl);

    let res = ctx.loop_list.binary_search_by_key(&unit_number, |i| i.0);
    if let Ok(idx) = res {
        log::error!(
            "unit number {} already used for {:?}",
            unit_number,
            ctx.loop_list[idx]
        );
        return Status::INVALID_PARAMETER;
    };

    match add_loopback(ctx, unit_number) {
        Err(e) => return e.status(),
        Ok(h) => *loop_handle = h.as_ptr(),
    };

    Status::SUCCESS
}

unsafe extern "efiapi" fn remove(this: *mut LoopControlProtocol, loop_handle: RawHandle) -> Status {
    if this.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let Some(loop_handle) = Handle::from_ptr(loop_handle) else {
        return Status::INVALID_PARAMETER;
    };

    let ctx = &mut *container_of!(this, ControlContext, loop_ctl);

    let Some((idx, &(unit_number, ..))) = ctx
        .loop_list
        .iter()
        .enumerate()
        .find(|(_, (_, h, _))| (*h == loop_handle))
    else {
        log::error!("handle {:?} not found", loop_handle);
        return Status::NOT_FOUND;
    };

    match loopback::uninstall_loopback(ctx.bus_handle, loop_handle) {
        Err(e) => return e.status(),
        Ok(_h) => {}
    }

    ctx.loop_list.remove(idx);

    log::debug!("removed loopback({}) {:?}", unit_number, loop_handle);

    Status::SUCCESS
}

pub(super) fn remove_children(ctx: &mut ControlContext) -> Result {
    while let Some((_, child, _)) = ctx.loop_list.last() {
        loopback::uninstall_loopback(ctx.bus_handle, *child)?;
        ctx.loop_list.pop();
    }
    Ok(())
}

pub fn create_loop_control() -> LoopControlProtocol {
    LoopControlProtocol {
        get_free,
        add,
        find,
        remove,
    }
}
