use super::*;

use core::mem;
use core::ops::ControlFlow;

use r_efi::protocols::shell;
use regex::{Regex, RegexSetBuilder};
use uefi::proto::device_path::text::{AllowShortcuts, DevicePathFromText, DisplayOnly};
use uefi::proto::media::file::{File, FileInfo, RegularFile};
use uefi::CString16;

use uefi_loopdrv::{LoopMappingItem, LoopTarget, SECTOR_SIZE};

#[derive(Debug)]
pub enum PatchAction<'a> {
    MetaCpio,
    Append(&'a str),
    Replace(&'a str),
}

mod helper {
    use super::*;
    use bytemuck::{Pod, Zeroable};
    use core::ops::{Deref, DerefMut};

    pub struct LoopPool<'a> {
        loop_pt: &'a mut LoopProtocol,
        buffer: &'a mut [u8],
    }
    impl<'a> LoopPool<'a> {
        pub unsafe fn from_raw_parts(
            loop_pt: &'a mut LoopProtocol,
            buffer: *mut u8,
            size: usize,
        ) -> Self {
            Self {
                loop_pt,
                buffer: core::slice::from_raw_parts_mut(buffer, size),
            }
        }
        pub fn into_raw(self) -> *mut u8 {
            core::mem::ManuallyDrop::new(self).buffer.as_mut_ptr()
        }
    }
    impl Deref for LoopPool<'_> {
        type Target = [u8];
        fn deref(&self) -> &Self::Target {
            self.buffer
        }
    }
    impl DerefMut for LoopPool<'_> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            self.buffer
        }
    }
    impl Drop for LoopPool<'_> {
        fn drop(&mut self) {
            unsafe {
                let _ = (self.loop_pt.free_pool)(self.loop_pt, self.buffer.as_mut_ptr() as _);
            }
        }
    }

    pub trait ChunkRead {
        fn size(&self) -> usize;
        fn read_to_end(&mut self, buffer: &mut [u8]) -> Result;
    }

    pub struct VecChunk(pub Vec<u8>);
    impl ChunkRead for VecChunk {
        fn size(&self) -> usize {
            self.0.len()
        }
        fn read_to_end(&mut self, buffer: &mut [u8]) -> Result {
            if buffer.len() != self.0.len() {
                return Status::BAD_BUFFER_SIZE.to_result();
            }
            buffer.copy_from_slice(&self.0);
            Ok(())
        }
    }

    pub struct FileChunk {
        file: RegularFile,
        offset: u64,
        size: usize,
    }
    impl FileChunk {
        pub fn new(mut file: RegularFile, offset: u64, size: usize) -> Result<Self> {
            let info = file.get_boxed_info::<FileInfo>()?;
            if (offset + size as u64) > info.file_size() {
                return Err(uefi::Error::new(Status::ABORTED, ()));
            }
            Ok(Self { file, offset, size })
        }
    }
    impl ChunkRead for FileChunk {
        fn size(&self) -> usize {
            self.size
        }
        fn read_to_end(&mut self, buffer: &mut [u8]) -> Result {
            if buffer.len() != self.size {
                return Status::BAD_BUFFER_SIZE.to_result();
            }

            self.file.set_position(self.offset as _)?;
            if self.file.read(buffer)? != self.size {
                return Status::ABORTED.to_result();
            }
            Ok(())
        }
    }

    #[derive(Clone, Copy, Pod, Zeroable)]
    #[repr(C)]
    struct CpioNewcHeader {
        magic: [u8; 6],
        ino: [u8; 8],
        mode: [u8; 8],
        uid: [u8; 8],
        gid: [u8; 8],
        n_link: [u8; 8],
        mtime: [u8; 8],
        file_size: [u8; 8],
        dev_major: [u8; 8],
        dev_minor: [u8; 8],
        rdev_major: [u8; 8],
        rdev_minor: [u8; 8],
        name_size: [u8; 8],
        check: [u8; 8],
    }

    fn four_bytes_padded_size(size: usize) -> usize {
        (size + 3) / 4 * 4
    }

    fn calc_cpio_entry_size(name_size: usize, file_size: usize) -> usize {
        let header_name_size = four_bytes_padded_size(mem::size_of::<CpioNewcHeader>() + name_size);
        let file_size = four_bytes_padded_size(file_size);
        header_name_size + file_size
    }

    fn write_hex(buf: &mut [u8; 8], mut value: u32) {
        for i in buf.iter_mut().rev() {
            let m = (value % 16) as u8;
            *i = if m < 10 { m + b'0' } else { m - 10 + b'a' };
            value >>= 4;
        }
    }

    const META_FILE_NAME: &[u8] = b".uefi-lopatch-metadata";
    const TRAILER: &[u8] = b"TRAILER!!!";

    /// Produce cpio in newc format, see <https://man.archlinux.org/man/cpio.5#New_ASCII_Format>
    pub struct MetaCpioChunk {
        metadata: String,
    }
    impl MetaCpioChunk {
        pub fn new(metadata: String) -> Self {
            Self { metadata }
        }
    }
    impl ChunkRead for MetaCpioChunk {
        #[inline]
        fn size(&self) -> usize {
            let entries =
                calc_cpio_entry_size(META_FILE_NAME.len() + 1, self.metadata.as_bytes().len())
                    + calc_cpio_entry_size(TRAILER.len() + 1, 0);
            (entries + SECTOR_SIZE - 1) / SECTOR_SIZE * SECTOR_SIZE
        }

        fn read_to_end(&mut self, buffer: &mut [u8]) -> Result {
            if buffer.len() != self.size() {
                return Status::BAD_BUFFER_SIZE.to_result();
            }

            let metadata_header = {
                let mut header = CpioNewcHeader::zeroed();
                bytemuck::bytes_of_mut(&mut header).fill(b'0');
                header.magic = *b"070701";
                write_hex(&mut header.ino, 0xdeadbeef);
                write_hex(&mut header.mode, 0o0100644);
                header
            };
            let trailer_header = {
                let mut header = CpioNewcHeader::zeroed();
                bytemuck::bytes_of_mut(&mut header).fill(b'0');
                header.magic = *b"070701";
                header
            };

            let files = [
                (metadata_header, META_FILE_NAME, self.metadata.as_bytes()),
                (trailer_header, TRAILER, &[]),
            ];

            let header_size = mem::size_of::<CpioNewcHeader>();
            let mut pos = 0;
            for (mut header, name, data) in files {
                write_hex(&mut header.n_link, 1);
                write_hex(&mut header.file_size, data.len() as _);
                write_hex(&mut header.name_size, (name.len() + 1) as _);
                let header_buf = bytemuck::bytes_of(&header);
                buffer[pos..][..header_size].copy_from_slice(header_buf);
                pos += header_size;
                // name
                let name_with_pad_size =
                    four_bytes_padded_size(header_size + name.len() + 1) - header_size;
                buffer[pos..][..name.len()].copy_from_slice(name);
                buffer[pos..][name.len()..name_with_pad_size].fill(0);
                pos += name_with_pad_size;
                // data
                let data_with_pad_size = four_bytes_padded_size(data.len());
                buffer[pos..][..data.len()].copy_from_slice(data);
                buffer[pos..][data.len()..data_with_pad_size].fill(0);
                pos += data_with_pad_size;
            }
            buffer[pos..].fill(0);
            Ok(())
        }
    }
}
use helper::*;

pub fn attach_loop_device(
    bt: &BootServices,
    id: Option<u32>,
    read_only: bool,
    is_partition: bool,
    patch: &[(Regex, Vec<PatchAction>)],
    image_file: &str,
) -> Result {
    let handle = bt.get_handle_for_protocol::<LoopControlProtocol>()?;
    let loop_ctl = bt.open_protocol_exclusive::<LoopControlProtocol>(handle)?;

    let handle = unsafe {
        let mut handle: RawHandle = ptr::null_mut();
        if let Some(id) = id {
            (loop_ctl.find)(loop_ctl.get_mut().unwrap(), id, &mut handle).to_result()?;
        } else {
            (loop_ctl.get_free)(loop_ctl.get_mut().unwrap(), &mut handle).to_result()?;
        }
        Handle::from_ptr(handle).unwrap()
    };

    let loop_pt = bt.open_protocol_exclusive::<LoopProtocol>(handle)?;
    if id.is_some() {
        unsafe {
            (loop_pt.clear)(loop_pt.get_mut().unwrap()).to_result()?;
        }
    }

    let image_dp = device_path_from_shell_text(bt, image_file)?;
    let GetFileInfo {
        fs_device,
        path: image_path,
        file: mut image_file,
        info: image_file_info,
        ..
    } = unsafe { get_file_info(bt, ptr::null_mut(), image_dp.as_ffi_ptr())? };
    let total_sectors = image_file_info.file_size() / SECTOR_SIZE as u64;

    let iso9660 = ISO9660::new(&mut image_file);

    let read_only = if iso9660.is_ok() && !read_only {
        log::warn!("Detected ISO9660, force read-only");
        true
    } else {
        read_only
    };

    // no patching
    if patch.is_empty() {
        unsafe {
            return (loop_pt.set_file)(
                loop_pt.get_mut().unwrap(),
                iso9660.is_ok() || read_only,
                is_partition,
                ptr::null_mut(),
                image_dp.as_ffi_ptr(),
            )
            .to_result();
        };
    }

    //
    // ISO9660 patching
    //
    let re_set = RegexSetBuilder::new(patch.iter().map(|f| f.0.as_str()))
        .case_insensitive(true)
        .build()
        .unwrap();

    let mut iso9660 = iso9660.map_err(|e| {
        log::error!("not a ISO9660");
        e
    })?;
    let (record_pos, record_size) = iso9660.find_root_record()?;
    let mut buffer = [0u8; 255];

    let mut append_item_start = total_sectors;
    let mut append_item_list = Vec::new();

    let mut append_item = |target, target_start_sector, num_sectors| {
        let start_sector = append_item_start;
        append_item_list.push(LoopMappingItem {
            start_sector,
            num_sectors,
            target,
            target_start_sector,
        });
        append_item_start += num_sectors;
        start_sector
    };

    #[derive(Debug)]
    struct PatchRecord {
        record_position: u64,
        new_extent_lba: u64,
        new_extent_size: usize,
    }
    let mut patch_record_list = Vec::<PatchRecord>::new();
    let mut pool_dp_list = Vec::<PoolDevicePath>::new();

    iso9660.walk_record::<(), _>(&mut buffer, record_pos, record_size, "", &mut |info| {
        if info.is_dir {
            return Ok(ControlFlow::Continue(()));
        }
        let matches = re_set.matches(info.path);
        if !matches.matched_any() {
            return Ok(ControlFlow::Continue(()));
        }

        let (replace, appends) = {
            let mut res = Vec::new();
            let mut replace = None;
            for patch in matches.into_iter().flat_map(|idx| &patch[idx].1) {
                if let PatchAction::Replace(_) = patch {
                    replace = Some(patch);
                    res.clear();
                } else {
                    res.push(patch);
                }
            }
            (replace, res)
        };
        log::debug!("matched {} {:?} {:?}", info.path, replace, appends);

        let mut reader_list: Vec<Box<dyn ChunkRead>> = Vec::new();

        let (file_start_sector, file_item_size) = if let Some(&PatchAction::Replace(path)) = replace
        {
            let replace_dp = device_path_from_shell_text(bt, path)?;
            let GetFileInfo {
                fs_device,
                path,
                mut file,
                info: file_info,
                ..
            } = unsafe { get_file_info(bt, ptr::null_mut(), replace_dp.as_ffi_ptr())? };
            let start = append_item(
                LoopTarget::File {
                    fs_device: fs_device.as_ptr(),
                    path: path.as_ffi_ptr(),
                },
                0,
                file_info.file_size() / SECTOR_SIZE as u64,
            );
            pool_dp_list.push(replace_dp);

            let file_item_size = file_info.file_size() / SECTOR_SIZE as u64 * SECTOR_SIZE as u64;
            let file_rest = (file_info.file_size() % SECTOR_SIZE as u64) as usize;
            if file_rest > 0 {
                let mut buffer = Vec::<u8>::new();
                buffer.resize(file_rest, 0);

                read_exact(&mut file, file_item_size, &mut buffer)?;

                reader_list.push(Box::new(VecChunk(buffer)))
            }
            (start, file_item_size as usize)
        } else {
            let start = append_item(
                LoopTarget::File {
                    fs_device: fs_device.as_ptr(),
                    path: image_path.as_ffi_ptr(),
                },
                info.extent_position / SECTOR_SIZE as u64,
                (info.extent_size / SECTOR_SIZE) as _,
            );

            let file_item_size = info.extent_size / SECTOR_SIZE * SECTOR_SIZE;
            let file_rest = info.extent_size % SECTOR_SIZE;
            if file_rest > 0 {
                let mut buffer = Vec::<u8>::new();
                buffer.resize(file_rest, 0);

                info.file
                    .read(info.extent_position + file_item_size as u64, &mut buffer)?;

                reader_list.push(Box::new(VecChunk(buffer)))
            }
            (start, file_item_size)
        };

        for append in appends {
            match append {
                &PatchAction::Append(file) => {
                    let dp = device_path_from_shell_text(bt, file)?;
                    let GetFileInfo {
                        file,
                        info: file_info,
                        ..
                    } = unsafe { get_file_info(bt, ptr::null_mut(), dp.as_ffi_ptr())? };
                    reader_list.push(Box::new(FileChunk::new(
                        file,
                        0,
                        file_info.file_size() as _,
                    )?));
                }
                PatchAction::MetaCpio => reader_list.push(Box::new(MetaCpioChunk::new(format!(
                    "LOPATCH_DEVICE_PATH='{}'\n",
                    image_dp
                        .to_string(bt, DisplayOnly(false), AllowShortcuts(false))
                        .ok()
                        .unwrap_or_default()
                        .unwrap_or_default(),
                )))),
                PatchAction::Replace(_) => unreachable!(),
            }
        }

        let pool_size = reader_list.iter().fold(0, |acc, c| acc + c.size());
        let pool_size = (pool_size + SECTOR_SIZE - 1) / SECTOR_SIZE * SECTOR_SIZE;
        let mut loop_pool = {
            let mut loop_pool = ptr::null_mut();
            unsafe {
                (loop_pt.alloc_pool)(loop_pt.get_mut().unwrap(), pool_size, &mut loop_pool)
                    .to_result()
                    .unwrap();
                LoopPool::from_raw_parts(loop_pt.get_mut().unwrap(), loop_pool as _, pool_size)
            }
        };

        let mut pool_pos = 0;
        for mut reader in reader_list {
            let end = pool_pos + reader.size();
            reader.read_to_end(&mut loop_pool[pool_pos..end])?;
            pool_pos = end;
        }

        patch_record_list.push(PatchRecord {
            record_position: info.record_position,
            new_extent_lba: file_start_sector / (ISO_BLOCK_SIZE / SECTOR_SIZE) as u64,
            new_extent_size: file_item_size + pool_pos,
        });

        let pool_sectors = (loop_pool.len() / SECTOR_SIZE) as _;
        append_item(
            LoopTarget::LoopPool {
                buffer: loop_pool.into_raw() as _,
            },
            0,
            pool_sectors,
        );

        Ok(ControlFlow::Continue(()))
    })?;

    fn alter_record(record_block: &mut [u8], offset: usize, extent_lba: u32, extent_size: u32) {
        let record = &mut record_block[offset..offset + 34];
        record[2..10].copy_from_slice(&get_u32_lsb_msb_bytes(extent_lba));
        record[10..18].copy_from_slice(&get_u32_lsb_msb_bytes(extent_size));
    }

    patch_record_list.sort_by_key(|i| i.record_position);
    let mut record_block_list = Vec::<(u64, LoopPool)>::new();
    for PatchRecord {
        record_position,
        new_extent_lba,
        new_extent_size,
    } in patch_record_list
    {
        let record_lba = record_position / ISO_BLOCK_SIZE as u64;
        let record_lba_pos = record_lba * ISO_BLOCK_SIZE as u64;
        let record_offset = (record_position % ISO_BLOCK_SIZE as u64) as usize;
        if let Some(last) = record_block_list.last_mut() {
            if last.0 == record_lba {
                alter_record(
                    &mut last.1,
                    record_offset,
                    new_extent_lba as _,
                    new_extent_size as _,
                );
                continue;
            }
        }

        let mut record_block = unsafe {
            let mut record_block = ptr::null_mut();
            (loop_pt.alloc_pool)(
                loop_pt.get_mut().unwrap(),
                ISO_BLOCK_SIZE,
                &mut record_block,
            )
            .to_result()?;
            LoopPool::from_raw_parts(
                loop_pt.get_mut().unwrap(),
                record_block as _,
                ISO_BLOCK_SIZE,
            )
        };

        iso9660.read(record_lba_pos, &mut record_block)?;

        alter_record(
            &mut record_block,
            record_offset,
            new_extent_lba as _,
            new_extent_size as _,
        );
        record_block_list.push((record_lba, record_block));
    }

    let mut table = Vec::<LoopMappingItem>::new();
    for (record_lba, record_block) in record_block_list {
        let record_sector = record_lba * (ISO_BLOCK_SIZE / SECTOR_SIZE) as u64;
        let prev_end_sector = if let Some(last) = table.last() {
            last.end_sector()
        } else {
            0
        };

        if prev_end_sector < record_sector {
            table.push(LoopMappingItem {
                start_sector: prev_end_sector,
                num_sectors: record_sector - prev_end_sector,
                target: LoopTarget::File {
                    fs_device: fs_device.as_ptr(),
                    path: image_path.as_ffi_ptr(),
                },
                target_start_sector: prev_end_sector,
            })
        }

        table.push(LoopMappingItem {
            start_sector: record_sector,
            num_sectors: (ISO_BLOCK_SIZE / SECTOR_SIZE) as _,
            target: LoopTarget::LoopPool {
                buffer: record_block.into_raw() as _,
            },
            target_start_sector: 0,
        })
    }
    let prev_end_sector = if let Some(last) = table.last() {
        last.end_sector()
    } else {
        0
    };
    if prev_end_sector < total_sectors {
        table.push(LoopMappingItem {
            start_sector: prev_end_sector,
            num_sectors: total_sectors - prev_end_sector,
            target: LoopTarget::File {
                fs_device: fs_device.as_ptr(),
                path: image_path.as_ffi_ptr(),
            },
            target_start_sector: prev_end_sector,
        })
    }

    table.extend(append_item_list);

    unsafe {
        (loop_pt.set_mapping_table)(
            loop_pt.get_mut().unwrap(),
            read_only,
            is_partition,
            table.len(),
            table.as_ptr(),
        )
        .to_result()
    }
}

#[inline]
fn get_u32_lsb_msb_bytes(num: u32) -> [u8; 8] {
    let mut res = [0; 8];
    res[0..4].copy_from_slice(&num.to_le_bytes());
    res[4..8].copy_from_slice(&num.to_be_bytes());
    res
}

fn get_shell_pt(bt: &BootServices) -> Option<&shell::Protocol> {
    let bt = uefi_loopdrv::get_boot_service_raw(bt);
    unsafe {
        let mut sh_ptr = ptr::null_mut();
        let res = (bt.locate_protocol)(
            &shell::PROTOCOL_GUID as *const _ as _,
            ptr::null_mut(),
            &mut sh_ptr,
        );
        if sh_ptr.is_null() || res.is_error() {
            return None;
        }
        let sh_ptr = sh_ptr as *mut shell::Protocol;
        Some(&*sh_ptr)
    }
}

fn device_path_from_shell_text<'a>(bt: &'a BootServices, path: &str) -> Result<PoolDevicePath<'a>> {
    if let Some(shell_pt) = get_shell_pt(bt) {
        let path = path.replace('/', r"\");
        let path = CString16::try_from(path.as_str()).unwrap();
        let dp = (shell_pt.get_device_path_from_file_path)(path.as_ptr() as _);
        if !dp.is_null() {
            return Ok(PoolDevicePath::new(bt, dp as _));
        }
    }
    let handle = bt.get_handle_for_protocol::<DevicePathFromText>()?;
    let text2dp = bt.open_protocol_exclusive::<DevicePathFromText>(handle)?;
    let path = CString16::try_from(path).unwrap();
    // FIXME: uefi-rs leaks memory of this device path
    let dp = text2dp.convert_text_to_device_path(&path)?;
    Ok(PoolDevicePath::new(bt, dp.as_ffi_ptr()))
}
