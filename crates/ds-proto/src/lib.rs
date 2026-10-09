//! DualSense and DualSense Edge wire protocol.
//!
//! Everything in here is pure: byte layouts, CRCs, and encoders. No I/O.
//! USB indexes are the base; Bluetooth adds one because of the extra header byte after the
//! report id.

pub mod crc;
pub mod input;
pub mod output;
pub mod stream;
pub mod trigger;
pub mod virtual_pad;

pub use input::{BatteryStatus, Buttons, InputState, TouchPoint};
pub use output::OutputState;
pub use trigger::{TriggerEffect, TriggerPreset};

pub const SONY_VID: u16 = 0x054C;
pub const DUALSENSE_PID: u16 = 0x0CE6;
pub const DUALSENSE_EDGE_PID: u16 = 0x0DF2;

/// Bluetooth input report `0x31` length, including the report id.
pub const BT_INPUT_LEN: usize = 78;
/// Bluetooth classic output report `0x31` length, including the report id.
pub const BT_OUTPUT_LEN: usize = 78;
/// USB output report `0x02` length: the id and the 47-byte common block.
/// The writer pads it to the controller's own output report length.
pub const USB_OUTPUT_LEN: usize = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Connection {
    Usb,
    Bluetooth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Model {
    DualSense,
    DualSenseEdge,
}

impl Model {
    pub fn from_pid(pid: u16) -> Option<Model> {
        match pid {
            DUALSENSE_PID => Some(Model::DualSense),
            DUALSENSE_EDGE_PID => Some(Model::DualSenseEdge),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Model::DualSense => "DualSense",
            Model::DualSenseEdge => "DualSense Edge",
        }
    }

    pub fn is_edge(self) -> bool {
        self == Model::DualSenseEdge
    }
}

/// Connection type from the HID max input report length: 78 bytes is Bluetooth, anything else is USB.
pub fn connection_from_input_len(len: usize) -> Connection {
    if len == BT_INPUT_LEN {
        Connection::Bluetooth
    } else {
        Connection::Usb
    }
}
