use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use core::ops::{ControlFlow, Deref};

use uefi::prelude::*;
use uefi::proto::device_path::FfiDevicePath;
use uefi::proto::device_path::{DevicePath, DeviceSubType, DeviceType};
use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode, RegularFile};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::{CStr16, Result, Status};
use uefi_raw::Handle as RawHandle;

use uefi_loopdrv::get_protocol_mut;

pub struct PoolDevicePath<'a> {
    bt: &'a BootServices,
    dp: *const FfiDevicePath,
}
impl<'a> PoolDevicePath<'a> {
    pub fn new(bt: &'a BootServices, dp: *const FfiDevicePath) -> Self {
        Self { bt, dp }
    }
}
impl Deref for PoolDevicePath<'_> {
    type Target = DevicePath;
    fn deref(&self) -> &Self::Target {
        unsafe { DevicePath::from_ffi_ptr(self.dp) }
    }
}
impl Drop for PoolDevicePath<'_> {
    fn drop(&mut self) {
        let bt_raw = uefi_loopdrv::get_boot_service_raw(self.bt);
        let _ = unsafe { (bt_raw.free_pool)(self.dp as _) };
    }
}

#[allow(unused)]
pub struct GetFileInfo<'a> {
    pub fs_device: Handle,
    pub fs_interface: *mut SimpleFileSystem,
    pub path: &'a DevicePath,
    pub file: RegularFile,
    pub info: Box<FileInfo>,
}

pub unsafe fn get_file_info<'a, 'b: 'a>(
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

    Ok(GetFileInfo {
        fs_device,
        fs_interface,
        path,
        file,
        info,
    })
}

pub const ISO_BLOCK_SIZE: usize = 2048;

pub struct ISO9660<'a> {
    file: &'a mut RegularFile,
}

pub struct WalkRecordInfo<'a, 'b, 'c, 'd> {
    pub file: &'a mut ISO9660<'b>,
    pub record: &'c [u8],
    pub record_position: u64,
    pub record_size: usize,
    pub extent_position: u64,
    pub extent_size: usize,
    pub path: &'d str,
    pub is_dir: bool,
    pub file_version: u16,
}

impl<'a> ISO9660<'a> {
    pub fn new(file: &'a mut RegularFile) -> Result<Self> {
        let mut iso9660 = Self { file };
        let mut buffer = [0u8; 7];
        iso9660.read(16 * ISO_BLOCK_SIZE as u64, &mut buffer)?;
        let vd_id = &buffer[1..6];
        let vd_ver = buffer[6];
        if vd_id != b"CD001" && vd_ver != 1 {
            return Err(uefi::Error::new(Status::ABORTED, ()));
        }
        Ok(iso9660)
    }

    #[inline]
    pub fn read(&mut self, position: u64, buffer: &mut [u8]) -> Result {
        self.file.set_position(position)?;
        if self.file.read(buffer)? != buffer.len() {
            log::error!("read underflow");
            return Status::DEVICE_ERROR.to_result();
        }
        Ok(())
    }

    pub fn find_pvd_position(&mut self) -> Result<u64> {
        let mut buffer = [0u8; ISO_BLOCK_SIZE];

        let mut start = 16;
        loop {
            self.read(start * ISO_BLOCK_SIZE as u64, &mut buffer)?;
            let vd_type = buffer[0];
            let vd_id = &buffer[1..6];
            let vd_ver = buffer[6];
            if vd_id != b"CD001" && vd_ver != 1 {
                return Err(uefi::Error::new(Status::ABORTED, ()));
            }

            match vd_type {
                255 => return Err(uefi::Error::new(Status::NOT_FOUND, ())),
                1 => break,
                _ => {}
            }
            start += 1;
        }
        Ok(start * ISO_BLOCK_SIZE as u64)
    }

    #[inline]
    pub fn find_root_record(&mut self) -> Result<(u64, usize)> {
        let pvd_pos = self.find_pvd_position()?;
        Ok((pvd_pos + 156, 34))
    }

    pub fn walk_record<T, F>(
        &mut self,
        buffer: &mut [u8],
        record_position: u64,
        record_size: usize,
        parent_path: &str,
        f: &mut F,
    ) -> Result<ControlFlow<T>>
    where
        F: FnMut(WalkRecordInfo) -> Result<ControlFlow<T>>,
    {
        if buffer.len() < u8::MAX as _ {
            return Err(uefi::Error::new(Status::BUFFER_TOO_SMALL, ()));
        }
        let record = &mut buffer[..record_size];
        self.read(record_position, record)?;

        let file_flags = record[25];
        let is_dir = (file_flags & 0b00000010) != 0;
        let not_final_record = (file_flags & 0b01000000) != 0;
        if not_final_record {
            log::warn!("handling of multi-records file not implemented")
        }
        let id_len = record[32] as usize;

        let id_slice = &record[33..33 + id_len];
        let id = match memchr::memchr(0, id_slice) {
            None => String::from_utf8_lossy(id_slice),
            Some(nul_pos) => String::from_utf8_lossy(&id_slice[..nul_pos]),
        };

        let mut path = if id.is_empty() && parent_path.is_empty() {
            String::new()
        } else {
            let parent_path = parent_path.trim_end_matches('/');
            let id = id.trim_start_matches('/');
            format!("{}/{}", parent_path, id)
        };

        let extent_lba = u32::from_le_bytes(record[2..6].try_into().unwrap()) as u64;
        let extent_size = u32::from_le_bytes(record[10..14].try_into().unwrap()) as usize;
        let mut position = extent_lba * ISO_BLOCK_SIZE as u64;

        let file_version = if !is_dir {
            match path.rfind(';') {
                Some(idx) => {
                    let version: u16 = path[idx + 1..].parse().unwrap();
                    path.truncate(idx);
                    version
                }
                None => 1,
            }
        } else {
            0
        };

        let flow = f(WalkRecordInfo {
            file: self,
            record,
            record_position,
            record_size,
            extent_position: position,
            extent_size,
            path: &path,
            is_dir,
            file_version,
        })?;
        if !is_dir {
            return Ok(flow);
        }
        if let ControlFlow::Break(b) = flow {
            return Ok(ControlFlow::Break(b));
        }

        let mut block_num = 0;
        let num_blocks = (extent_size + ISO_BLOCK_SIZE - 1) / ISO_BLOCK_SIZE;
        let mut count = 0;
        while block_num < num_blocks {
            count += 1;

            let mut size = [0u8; 1];
            self.read(position, &mut size).map_err(|e| {
                log::error!("failed to read record size {}", position);
                e
            })?;
            let size = size[0] as usize;

            if size == 0 || (position % ISO_BLOCK_SIZE as u64) + 34 > ISO_BLOCK_SIZE as u64 {
                block_num += 1;
                position = (block_num as u64 + extent_lba) * ISO_BLOCK_SIZE as u64;
                continue;
            }

            if count > 2 {
                if let ControlFlow::Break(v) = self.walk_record(buffer, position, size, &path, f)? {
                    return Ok(ControlFlow::Break(v));
                }
            }

            position += size as u64;
            block_num = ((position / ISO_BLOCK_SIZE as u64) - extent_lba) as usize;
        }

        Ok(ControlFlow::Continue(()))
    }
}

pub fn read_exact(file: &mut RegularFile, position: u64, buffer: &mut [u8]) -> Result {
    file.set_position(position)?;
    if file.read(buffer)? != buffer.len() {
        return Status::DEVICE_ERROR.to_result();
    }
    Ok(())
}
