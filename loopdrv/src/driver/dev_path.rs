use super::*;
use uefi::proto::device_path::{DeviceSubType, DeviceType};
use uefi_raw::protocol::device_path::DevicePathProtocol;
use uefi_raw::{guid, Guid};

#[inline]
fn create_header<N>(t: DeviceType, sub_t: DeviceSubType) -> DevicePathProtocol {
    DevicePathProtocol {
        major_type: t.0,
        sub_type: sub_t.0,
        length: (mem::size_of::<N>() as u16).to_le_bytes(),
    }
}

#[repr(C, packed)]
struct EndNode {
    header: DevicePathProtocol,
}
impl Default for EndNode {
    fn default() -> Self {
        Self {
            header: create_header::<Self>(DeviceType::END, DeviceSubType::END_ENTIRE),
        }
    }
}

#[repr(C, packed)]
struct LoopControlNode {
    header: DevicePathProtocol,
    vendor_guid: Guid,
}
impl LoopControlNode {
    const VENDOR_GUID: Guid = guid!("6470a202-4597-11ee-ae06-2cf05d73e0d3");
}
impl Default for LoopControlNode {
    fn default() -> Self {
        Self {
            header: create_header::<Self>(DeviceType::HARDWARE, DeviceSubType::HARDWARE_VENDOR),
            vendor_guid: Self::VENDOR_GUID,
        }
    }
}

#[repr(C, packed)]
#[derive(Default)]
pub struct LoopControlPath {
    hardware: LoopControlNode,
    end: EndNode,
}
impl LoopControlPath {
    pub fn new() -> Self {
        Self::default()
    }
}

#[repr(C, packed)]
struct LoopbackNode {
    header: DevicePathProtocol,
    vendor_guid: Guid,
    unit_number: [u8; 4],
}
impl LoopbackNode {
    #[allow(non_upper_case_globals)]
    const VENDOR_GUID: Guid = guid!("2711f120-45b7-11ee-8e7b-2cf05d73e0d3");

    fn new(unit_number: u32) -> Self {
        Self {
            header: create_header::<Self>(DeviceType::MESSAGING, DeviceSubType::MESSAGING_VENDOR),
            vendor_guid: Self::VENDOR_GUID,
            unit_number: unit_number.to_le_bytes(),
        }
    }
}

#[repr(C, packed)]
pub struct LoopbackPath {
    hardware: LoopControlNode,
    messaging: LoopbackNode,
    end: EndNode,
}
impl LoopbackPath {
    pub fn new(unit_number: u32) -> Self {
        Self {
            hardware: Default::default(),
            messaging: LoopbackNode::new(unit_number),
            end: Default::default(),
        }
    }
}
