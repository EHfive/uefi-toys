use super::*;

use alloc::alloc::{alloc, Layout};

use uefi::proto::device_path::{DevicePath, DeviceSubType, DeviceType};
use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode, RegularFile};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::CStr16;

#[repr(C)]
#[derive(Debug)]
#[unsafe_protocol("8826fb7e-438f-11ee-879a-2cf05d73e0d3")]
pub struct LoopProtocol {
    pub set_file: unsafe extern "efiapi" fn(
        this: *mut Self,
        read_only: bool,
        is_partition: bool,
        fs_device: RawHandle,
        path: *const FfiDevicePath,
    ) -> Status,
    /// device-mapper like linear concatting
    pub set_mapping_table: unsafe extern "efiapi" fn(
        this: *mut Self,
        read_only: bool,
        is_partition: bool,
        num_table_items: usize,
        table: *const LoopMappingItem,
    ) -> Status,
    pub clear: unsafe extern "efiapi" fn(this: *mut Self) -> Status,
    pub get_info: unsafe extern "efiapi" fn(this: *mut Self, info: *mut LoopInfo) -> Status,
    /// Allocate a device owned 8-bytes aligned memory to be used in mapping table.
    /// The memory pointer became invalid after passing to mapping table,
    /// and must not be passed, accessed or called with [LoopProtocol::free_pool].
    pub alloc_pool:
        unsafe extern "efiapi" fn(this: *mut Self, size: usize, buffer: *mut *mut c_void) -> Status,
    pub free_pool: unsafe extern "efiapi" fn(this: *mut Self, buffer: *mut c_void) -> Status,
}

#[repr(C)]
#[derive(Default)]
pub struct LoopInfo {
    pub unit_number: u32,
}

#[allow(unused)]
#[repr(C, u32)]
#[derive(Clone, Copy)]
pub enum LoopTarget {
    Zero = 0,
    /// The ownership of `buffer` would transfer back to loop
    LoopPool {
        buffer: *mut c_void,
    } = 1,
    /// `path` is a media device path with
    /// [file path](https://uefi.org/specs/UEFI/2.10/10_Protocols_Device_Path_Protocol.html#file-path-media-device-path)
    /// appended before end node if `fs_device` is null, otherwise only the file path portion.
    File {
        fs_device: RawHandle,
        path: *const FfiDevicePath,
    } = 2,
}

pub const SECTOR_SIZE: usize = 512;

/// A sector is 512-bytes
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LoopMappingItem {
    pub start_sector: u64,
    pub num_sectors: u64,
    pub target: LoopTarget,
    pub target_start_sector: u64,
}
impl LoopMappingItem {
    #[inline]
    pub fn end_sector(&self) -> u64 {
        self.start_sector + self.num_sectors
    }
}

impl PrivMappingItem {
    unsafe fn from_loop_mapping_item(
        bt: &BootServices,
        item: &loopback::LoopMappingItem,
    ) -> Result<Self> {
        let validate_target_size =
            |size: u64| (size / SECTOR_SIZE as u64 - item.target_start_sector) >= item.num_sectors;
        let invalid_err = || uefi::Error::new(Status::INVALID_PARAMETER, ());
        let target = match item.target {
            LoopTarget::Zero => PrivTarget::Zero,
            LoopTarget::LoopPool { buffer } => {
                // the pool now owns buffer memory
                let pool = Pool::boxed_from_data_ptr(buffer as _).ok_or_else(invalid_err)?;

                if !validate_target_size(pool.data.len() as _) {
                    log::error!(
                        "pool too small {} {} {}",
                        pool.data.len() / SECTOR_SIZE,
                        item.target_start_sector,
                        item.num_sectors
                    );
                    return Err(invalid_err());
                }
                PrivTarget::LoopPool { pool }
            }
            LoopTarget::File { fs_device, path } => {
                let GetFileInfo {
                    fs_device,
                    fs_interface,
                    path,
                    file,
                    info,
                } = get_file_info(bt, fs_device, path)?;

                if !validate_target_size(info.file_size()) {
                    log::error!("file too small");
                    return Err(invalid_err());
                }
                PrivTarget::File {
                    fs_device,
                    path: path.to_boxed(),
                    fs_interface,
                    file,
                    info,
                }
            }
        };
        Ok(PrivMappingItem {
            start_sector: item.start_sector,
            num_sectors: item.num_sectors,
            target,
            target_start_sector: item.target_start_sector,
        })
    }
}

struct GetFileInfo<'a> {
    fs_device: Handle,
    fs_interface: *mut SimpleFileSystem,
    path: &'a DevicePath,
    file: RegularFile,
    info: Box<FileInfo>,
}

unsafe fn get_file_info<'a, 'b: 'a>(
    bt: &'b BootServices,
    fs_device: RawHandle,
    path: *const FfiDevicePath,
) -> Result<GetFileInfo<'a>> {
    let mut path = DevicePath::from_ffi_ptr(path);
    let fs_device = if let Some(h) = Handle::from_ptr(fs_device) {
        h
    } else {
        bt.locate_device_path::<SimpleFileSystem>(&mut path)?
    };
    let invalid_err = || uefi::Error::new(Status::INVALID_PARAMETER, ());

    let fs_interface =
        &mut *get_protocol_mut::<SimpleFileSystem>(bt, fs_device)?.ok_or_else(invalid_err)?;
    let mut root = fs_interface.open_volume()?;

    let path_node = path.node_iter().next().ok_or_else(invalid_err)?;
    if path_node.full_type() != (DeviceType::MEDIA, DeviceSubType::MEDIA_FILE_PATH) {
        log::error!("path is not a media file device path");
        return Err(invalid_err());
    }
    let file_path = CStr16::from_ptr(path_node.data().as_ptr() as _);

    let mut file = root
        .open(file_path, FileMode::Read, FileAttribute::empty())
        .map_err(|e| {
            log::error!("failed to open {}, {}", file_path, e.status());
            e
        })?
        .into_regular_file()
        .ok_or_else(|| {
            log::error!("{} is not a file", file_path);
            invalid_err()
        })?;
    let info = file.get_boxed_info::<FileInfo>()?;

    // log::debug!("info {:?}", info);

    Ok(GetFileInfo {
        fs_device,
        fs_interface,
        path,
        file,
        info,
    })
}

unsafe extern "efiapi" fn set_file(
    this: *mut LoopProtocol,
    read_only: bool,
    is_partition: bool,
    fs_device: RawHandle,
    path: *const FfiDevicePath,
) -> Status {
    if this.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let bt = system_table().as_ref().boot_services();
    let ctx = LoopContext::from_loop_pt_ptr(this);

    let res = PrivMappingItem::from_loop_mapping_item(
        bt,
        &LoopMappingItem {
            start_sector: 0,
            num_sectors: 0,
            target: LoopTarget::File { fs_device, path },
            target_start_sector: 0,
        },
    );
    let mut item = match res {
        Err(e) => return e.status(),
        Ok(v) => v,
    };

    let PrivTarget::File { info, .. } = &item.target else {
        unreachable!()
    };

    let num_sectors = info.file_size() / SECTOR_SIZE as u64;
    item.num_sectors = num_sectors;
    set_media(ctx, read_only, is_partition, vec![item]);

    let res = bt.connect_controller(ctx.device_handle, None, None, true);
    res.status()
}

fn set_media(
    ctx: &mut LoopContext,
    read_only: bool,
    is_partition: bool,
    table: Vec<PrivMappingItem>,
) -> bool {
    let Some(last) = table.last() else {
        return false;
    };
    let total_sectors = last.start_sector + last.num_sectors;
    ctx.table = table;
    ctx.media.read_only = read_only;
    ctx.media.logical_partition = is_partition;
    ctx.media.block_size = SECTOR_SIZE as _;
    ctx.media.last_block = total_sectors;
    ctx.media.media_id = ctx.media.media_id.wrapping_add(1);
    ctx.media.media_present = true;
    true
}

unsafe extern "efiapi" fn set_mapping_table(
    this: *mut LoopProtocol,
    read_only: bool,
    is_partition: bool,
    num_table_items: usize,
    table: *const LoopMappingItem,
) -> Status {
    if this.is_null() || (num_table_items > 0 && table.is_null()) {
        return Status::INVALID_PARAMETER;
    }
    let bt = system_table().as_ref().boot_services();
    let ctx = LoopContext::from_loop_pt_ptr(this);

    let mut table = core::slice::from_raw_parts(table, num_table_items).to_vec();
    table.sort_by_key(|i| i.start_sector);

    let mut priv_table = vec![];
    priv_table.reserve(num_table_items);

    let mut res = Status::SUCCESS;
    let mut prev_end = 0;
    for item in &table {
        if res != Status::SUCCESS {
            if let LoopTarget::LoopPool { buffer } = item.target {
                let _ = Pool::boxed_from_data_ptr(buffer as _);
            }
            continue;
        }
        let item = PrivMappingItem::from_loop_mapping_item(bt, item);
        if res != Status::SUCCESS {
            continue;
        }
        let item = match item {
            Err(e) => {
                res = e.status();
                continue;
            }
            Ok(v) => v,
        };
        if item.num_sectors == 0 {
            continue;
        }
        if item.start_sector != prev_end {
            log::error!("mapping table not continuous");
            return Status::INVALID_PARAMETER;
        }
        prev_end = item.start_sector + item.num_sectors;
        priv_table.push(item);
    }
    if prev_end == 0 {
        log::error!("empty mapping table");
        return Status::INVALID_PARAMETER;
    }

    if res != Status::SUCCESS {
        return res;
    }

    set_media(ctx, read_only, is_partition, priv_table);

    let res = bt.connect_controller(ctx.device_handle, None, None, true);
    res.status()
}

unsafe extern "efiapi" fn clear(this: *mut LoopProtocol) -> Status {
    if this.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let bt = system_table().as_ref().boot_services();
    let ctx = LoopContext::from_loop_pt_ptr(this);
    ctx.media.media_present = false;
    ctx.media.last_block = 0;
    ctx.table = vec![];

    let res = bt.disconnect_controller(ctx.device_handle, None, None);
    res.status()
}

unsafe extern "efiapi" fn get_info(this: *mut LoopProtocol, info: *mut LoopInfo) -> Status {
    if this.is_null() || info.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let ctx = LoopContext::from_loop_pt_ptr(this);
    let info = &mut *info;
    info.unit_number = ctx.unit_number;
    Status::SUCCESS
}

unsafe extern "efiapi" fn alloc_pool(
    this: *mut LoopProtocol,
    size: usize,
    buffer: *mut *mut c_void,
) -> Status {
    if this.is_null() || buffer.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let ctx = LoopContext::from_loop_pt_ptr(this);

    let header_size = mem::size_of::<PoolHeader>();
    let layout = match Layout::from_size_align(header_size + size, POOL_ALIGN) {
        Err(e) => {
            log::error!("{}", e);
            return Status::INVALID_PARAMETER;
        }
        Ok(l) => l,
    };
    let ptr = alloc(layout);

    let meta = &mut *ptr.cast::<PoolHeader>();
    meta.ctx = ctx;
    meta.pool_size = size;

    *buffer = ptr.add(header_size) as _;
    Status::SUCCESS
}

unsafe extern "efiapi" fn free_pool(this: *mut LoopProtocol, buffer: *mut c_void) -> Status {
    if this.is_null() || buffer.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let Some(pool) = Pool::boxed_from_data_ptr(buffer as _) else {
        return Status::INVALID_PARAMETER;
    };
    let ctx = LoopContext::from_loop_pt_ptr(this);

    if pool.header.ctx != ctx {
        log::error!("pool {:?} is invalid or not managed by this loop", buffer);
        return Status::INVALID_PARAMETER;
    }

    Status::SUCCESS
}

pub fn create_loopback() -> LoopProtocol {
    LoopProtocol {
        set_file,
        set_mapping_table,
        clear,
        get_info,
        alloc_pool,
        free_pool,
    }
}
