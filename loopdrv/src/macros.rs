macro_rules! container_of {
    ($ptr:expr, $Container:ty, $($fields:tt).+) => {{
        let container = ::core::mem::align_of::<$Container>() as *const $Container;
        let member = ::core::ptr::addr_of!((*container).$($fields).+);
        if false {
            // loose static type check
            let _ = member == $ptr;
        }
        let offset = container.cast::<u8>().offset_from(member.cast::<u8>());
        ($ptr.cast::<u8>())
            .offset(offset)
            .cast::<$Container>()
    }};
}
