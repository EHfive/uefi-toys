use super::*;

use loop_pt::SECTOR_SIZE;

pub use uefi_raw::protocol::block::{BlockIoMedia, BlockIoProtocol, Lba};

const REVISION_1: u64 = 0x00010000u64;

unsafe extern "efiapi" fn reset(
    this: *mut BlockIoProtocol,
    _extended_verification: bool,
) -> Status {
    if this.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let _ctx = LoopContext::from_block_io_ptr(this);
    Status::SUCCESS
}

fn access_blocks<F>(ctx: &mut LoopContext, lba: Lba, buffer: &mut [u8], mut target_cb: F) -> Result
where
    F: FnMut(
        &mut LoopContext,
        /* buffer */ &mut [u8],
        &mut PrivTarget,
        /* start_sector */ u64,
        /* num_sectors */ u64,
    ) -> Result,
{
    let end_sector = if let Some(last) = ctx.table.last() {
        last.start_sector + last.num_sectors
    } else {
        0
    };

    let start_sector = lba * ctx.media.block_size as u64 / SECTOR_SIZE as u64;
    let total_sectors = (buffer.len() / SECTOR_SIZE) as u64;
    // log::debug!("read {}+{} {}", start_sector, total_sectors, end_sector);
    if start_sector + total_sectors > end_sector {
        log::error!("buffer region overflows device region");
        return Status::INVALID_PARAMETER.to_result();
    }

    let upper_bound = ctx
        .table
        .partition_point(|x| x.start_sector <= start_sector);
    // hit if mapping table is empty, unsorted or `start_sector` of first item is not 0
    assert_ne!(0, upper_bound);

    let mut total_advance: u64 = 0;

    // preserve table structure
    let mut table = mem::take(&mut ctx.table);
    for item in &mut table[upper_bound - 1..] {
        let remaining = total_sectors - total_advance;
        if remaining == 0 {
            break;
        }
        let curr_sector = start_sector + total_advance;
        let item_end_sector = item.start_sector + item.num_sectors;
        let advance = remaining.min(item_end_sector - curr_sector);
        let offset = curr_sector - item.start_sector;
        let target_sector = item.target_start_sector + offset;
        let item_buffer = &mut buffer[total_advance as usize * SECTOR_SIZE
            ..(total_advance + advance) as usize * SECTOR_SIZE];

        target_cb(ctx, item_buffer, &mut item.target, target_sector, advance)?;

        total_advance += advance;
    }
    ctx.table = table;

    assert_eq!(total_advance, total_sectors);
    Ok(())
}

unsafe fn validate_blocks_params(
    this: *const BlockIoProtocol,
    media_id: u32,
    _lba: Lba,
    buffer_size: usize,
    buffer: *const c_void,
) -> Status {
    if this.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let ctx = LoopContext::from_block_io_ptr(this.cast_mut());
    if !ctx.media.media_present {
        return Status::NO_MEDIA;
    }
    if media_id != ctx.media.media_id {
        return Status::MEDIA_CHANGED;
    }
    if buffer_size > 0 && buffer.is_null() {
        return Status::INVALID_PARAMETER;
    }
    if buffer_size % ctx.media.block_size as usize != 0 {
        return Status::BAD_BUFFER_SIZE;
    }
    Status::SUCCESS
}

unsafe extern "efiapi" fn read_blocks(
    this: *const BlockIoProtocol,
    media_id: u32,
    lba: Lba,
    buffer_size: usize,
    buffer: *mut c_void,
) -> Status {
    match validate_blocks_params(this, media_id, lba, buffer_size, buffer) {
        Status::SUCCESS => {}
        e => {
            log::error!("failed to read block: {}", e);
            return e;
        }
    }
    let bt = system_table().as_ref().boot_services();
    let ctx = LoopContext::from_block_io_ptr(this.cast_mut());
    let buffer = core::slice::from_raw_parts_mut(buffer as *mut u8, buffer_size);

    let res = access_blocks(ctx, lba, buffer, |_ctx, buffer, target, sector, num| {
        match target {
            PrivTarget::Zero => {
                buffer.fill(0);
            }
            PrivTarget::LoopPool { pool } => {
                buffer.copy_from_slice(
                    &pool.data
                        [sector as usize * SECTOR_SIZE..(sector + num) as usize * SECTOR_SIZE],
                );
            }
            PrivTarget::File {
                file,
                fs_device,
                fs_interface,
                ..
            } => {
                if !validate_handle_protocol(
                    bt,
                    fs_device.as_ptr(),
                    &SimpleFileSystem::GUID,
                    *fs_interface as _,
                ) {
                    log::error!("file device or FS protocol interface changed");
                    // XXX: notify error?
                    return Status::DEVICE_ERROR.to_result();
                }
                file.set_position(sector * SECTOR_SIZE as u64).unwrap();
                if file.read(buffer)? != buffer.len() {
                    log::error!("read underflow");
                    return Status::DEVICE_ERROR.to_result();
                }
            }
        }
        Ok(())
    });
    if let Err(e) = res {
        log::error!("failed to read blocks: {}", e);
        return e.status();
    }

    Status::SUCCESS
}

unsafe extern "efiapi" fn write_blocks(
    this: *mut BlockIoProtocol,
    media_id: u32,
    lba: Lba,
    buffer_size: usize,
    buffer: *const c_void,
) -> Status {
    match validate_blocks_params(this, media_id, lba, buffer_size, buffer) {
        Status::SUCCESS => {}
        e => return e,
    }
    let bt = system_table().as_ref().boot_services();
    let ctx = LoopContext::from_block_io_ptr(this);
    if ctx.media.read_only {
        return Status::WRITE_PROTECTED;
    }
    let buffer = core::slice::from_raw_parts_mut(buffer as *mut u8, buffer_size);

    let res = access_blocks(ctx, lba, buffer, |_ctx, buffer, target, sector, num| {
        match target {
            PrivTarget::Zero => log::warn!("writing to virtual zero block, discard"),
            PrivTarget::LoopPool { pool } => {
                let data_slice = &mut pool.data
                    [sector as usize * SECTOR_SIZE..(sector + num) as usize * SECTOR_SIZE];
                data_slice.copy_from_slice(buffer);
            }
            PrivTarget::File {
                file,
                fs_device,
                fs_interface,
                ..
            } => {
                if !validate_handle_protocol(
                    bt,
                    fs_device.as_ptr(),
                    &SimpleFileSystem::GUID,
                    *fs_interface as _,
                ) {
                    log::error!("file device or FS protocol interface changed");
                    // XXX: notify error?
                    return Status::DEVICE_ERROR.to_result();
                }
                file.set_position(sector * SECTOR_SIZE as u64).unwrap();
                if let Err(e) = file.write(buffer) {
                    log::error!("written {} of {} bytes", e.data(), buffer_size);
                    return Err(e.to_err_without_payload());
                }
            }
        }
        Ok(())
    });
    if let Err(e) = res {
        return e.status();
    }

    Status::SUCCESS
}

unsafe extern "efiapi" fn flush_blocks(this: *mut BlockIoProtocol) -> Status {
    if this.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let bt = system_table().as_ref().boot_services();
    let ctx = LoopContext::from_block_io_ptr(this);
    if !ctx.media.media_present {
        return Status::NO_MEDIA;
    }
    if ctx.media.read_only {
        return Status::SUCCESS;
    }

    for item in &mut ctx.table {
        if let PrivTarget::File {
            fs_device,
            fs_interface,
            file,
            ..
        } = &mut item.target
        {
            if !validate_handle_protocol(
                bt,
                fs_device.as_ptr(),
                &SimpleFileSystem::GUID,
                *fs_interface as _,
            ) {
                log::error!("file device or FS protocol interface changed");
                // XXX: notify error?
                return Status::DEVICE_ERROR;
            }
            if let Err(e) = file.flush() {
                return e.status();
            }
        }
    }

    Status::SUCCESS
}

pub fn create_default_media() -> BlockIoMedia {
    BlockIoMedia {
        media_id: 0,
        removable_media: true,
        media_present: false,
        logical_partition: false,
        read_only: true,
        write_caching: false,
        block_size: SECTOR_SIZE as _,
        io_align: 0,
        last_block: 0,
        // Added in revision 2.
        lowest_aligned_lba: 0,
        logical_blocks_per_physical_block: 0,
        // Added in revision 3.
        optimal_transfer_length_granularity: 0,
    }
}

pub fn create_block_io(media: *const BlockIoMedia) -> BlockIoProtocol {
    BlockIoProtocol {
        revision: REVISION_1,
        media,
        reset,
        read_blocks,
        write_blocks,
        flush_blocks,
    }
}
