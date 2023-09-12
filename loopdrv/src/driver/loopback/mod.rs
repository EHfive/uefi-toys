mod block_io;
mod loop_pt;

use super::*;
pub use loop_pt::*;

use ptr_meta::Pointee;
use uefi::proto::device_path::DevicePath;
use uefi::proto::media::file::{File, FileInfo, RegularFile};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::table::boot::ScopedProtocol;
use uefi::{CString16, Char16};

#[repr(C)]
pub(super) struct LoopContext {
    dev_path: dev_path::LoopbackPath,
    loop_pt: LoopProtocol,
    block_io: block_io::BlockIoProtocol,
    media: block_io::BlockIoMedia,
    unit_number: u32,
    name: CString16,
    device_handle: Handle,
    loop_ctl: Option<ScopedProtocol<'static, LoopControlProtocol>>,
    protocols: Vec<(Guid, *mut c_void)>,
    table: Vec<PrivMappingItem>,
}
impl LoopContext {
    #[inline]
    pub unsafe fn from_loop_pt_ptr<'a>(ptr: *mut LoopProtocol) -> &'a mut Self {
        &mut *container_of!(ptr, loopback::LoopContext, loop_pt)
    }
    #[inline]
    pub unsafe fn from_block_io_ptr<'a>(ptr: *mut block_io::BlockIoProtocol) -> &'a mut Self {
        &mut *container_of!(ptr, loopback::LoopContext, block_io)
    }
    #[inline]
    pub fn name_ptr(&self) -> *const Char16 {
        self.name.as_ptr()
    }
    #[inline]
    pub fn is_free(&self) -> bool {
        !self.media.media_present
    }
}

const POOL_ALIGN: usize = 8;
#[repr(C, align(8))]
#[derive(Debug)]
struct PoolHeader {
    ctx: *const LoopContext,
    /// size excluding meta
    pool_size: usize,
}

#[repr(C, align(8))]
#[derive(Pointee, Debug)]
struct Pool {
    header: PoolHeader,
    data: [u8],
}
impl Pool {
    #[inline]
    #[must_use]
    unsafe fn boxed_from_data_ptr(data: *mut u8) -> Option<Box<Self>> {
        if data.align_offset(POOL_ALIGN) != 0 {
            return None;
        }
        let header_size = mem::size_of::<PoolHeader>();
        let ptr = data.sub(header_size);
        let header = &*ptr.cast::<PoolHeader>();
        let pool = &mut *ptr_meta::from_raw_parts_mut::<Pool>(ptr as _, header.pool_size);
        Some(Box::from_raw(pool))
    }
}

#[allow(unused)]
#[derive(Debug)]
enum PrivTarget {
    Zero,
    LoopPool {
        pool: Box<Pool>,
    },
    File {
        fs_device: Handle,
        path: Box<DevicePath>,
        fs_interface: *mut SimpleFileSystem,
        file: RegularFile,
        info: Box<FileInfo>,
    },
}

#[derive(Debug)]
struct PrivMappingItem {
    start_sector: u64,
    num_sectors: u64,
    target: PrivTarget,
    target_start_sector: u64,
}

fn open_loop_ctl_by_child(
    bus_handle: Handle,
    device_handle: Handle,
) -> Result<ScopedProtocol<'static, LoopControlProtocol>> {
    unsafe {
        let bt = system_table().as_ref().boot_services();
        bt.open_protocol::<LoopControlProtocol>(
            OpenProtocolParams {
                handle: bus_handle,
                // XXX: image handle or bus handle if they are not equal?
                agent: bus_handle,
                controller: Some(device_handle),
            },
            OpenProtocolAttributes::ByChildController,
        )
    }
}

pub(super) fn install_loopback(
    bus_handle: Handle,
    handle: Option<Handle>,
    unit_number: u32,
) -> Result<(Handle, *mut LoopContext)> {
    let bt = unsafe { system_table().as_ref().boot_services() };
    let invalid_handle = unsafe { Handle::from_ptr(mem::align_of::<Handle>() as _).unwrap() };
    let name = alloc::format!("Loopback Device #{}", unit_number);
    let name = CString16::try_from(name.as_str()).unwrap();

    let mut ctx = Box::new(LoopContext {
        dev_path: dev_path::LoopbackPath::new(unit_number),
        loop_pt: loop_pt::create_loopback(),
        block_io: block_io::create_block_io(ptr::null()),
        media: block_io::create_default_media(),
        unit_number,
        name,
        device_handle: invalid_handle,
        loop_ctl: None,
        protocols: vec![],
        table: vec![],
    });
    ctx.block_io.media = ptr::addr_of_mut!(ctx.media);

    let res = unsafe {
        ctx.protocols = vec![
            (DevicePath::GUID, ptr::addr_of_mut!(ctx.dev_path) as _),
            (LoopProtocol::GUID, ptr::addr_of_mut!(ctx.loop_pt) as _),
            (
                block_io::BlockIoProtocol::GUID,
                ptr::addr_of_mut!(ctx.block_io) as _,
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
    ctx.device_handle = handle;

    match open_loop_ctl_by_child(bus_handle, handle) {
        Ok(pt) => ctx.loop_ctl = Some(pt),
        Err(e) => {
            unsafe { uninstall_multiple_protocols(bt, handle, &ctx.protocols).unwrap() };
            return Err(e);
        }
    }

    Ok((handle, Box::into_raw(ctx)))
}

pub(super) fn uninstall_loopback(bus_handle: Handle, device_handle: Handle) -> Result {
    unsafe {
        let bt = system_table().as_ref().boot_services();
        let loop_pt_ptr = get_protocol_mut::<LoopProtocol>(bt, device_handle)?.unwrap();
        let mut ctx = Box::from_raw(container_of!(loop_pt_ptr, LoopContext, loop_pt));

        // close loop control protocol
        ctx.loop_ctl = None;

        if let Err(e) = uninstall_multiple_protocols(bt, device_handle, &ctx.protocols) {
            let (protocol, interface) = e.data();
            log::error!("failed to uninstall protocol {} {:?}", protocol, interface);

            // re-open bus protocol
            ctx.loop_ctl = Some(open_loop_ctl_by_child(bus_handle, device_handle)?);

            return Err(e.to_err_without_payload());
        };
        Ok(())
    }
}
