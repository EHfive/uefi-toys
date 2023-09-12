pub mod attach;
pub mod detach;
pub mod list;

use crate::utils::*;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ptr;

use uefi::prelude::*;
use uefi::table::boot::SearchType;
use uefi::Identify;
use uefi::Result;
use uefi_raw::Handle as RawHandle;
use uefi_services::println;

use uefi_loopdrv::{LoopControlProtocol, LoopProtocol};
