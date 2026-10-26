// SPDX-License-Identifier: GPL-2.0-only

//! What the bridge, the CLI and the OpenRazer route code know about the
//! receiver and its children.
//!
//! devices.toml at the repository root is the readable catalog. This table
//! must say the same thing. tests/test_device_tables.py compares them.
//! Keep each `Device { ... }` literal in the layout used below (one field
//! per line, hex ids as `0x....`, commands as `cmd(0x.., 0x.., &[..])` or
//! the name of a shared `&[Command]` list).

use crate::messages::{DeviceKind, Lane, Support};
use crate::report::{self, Report};

pub struct Receiver {
    pub vendor_id: u16,
    pub product_id: u16,
    pub name: &'static str,
    pub pointer_lane_selector: u8,
    pub keyboard_lane_selector: u8,
}

pub const RECEIVER: Receiver = Receiver {
    vendor_id: 0x1532,
    product_id: 0x00cf,
    name: "Razer HyperFlux V2",
    pointer_lane_selector: 0x1f,
    keyboard_lane_selector: 0x9f,
};

/// The bridge's read-only queries to the receiver itself, for `hyperfluxctl
/// probe receiver`: transaction id, class, command, data size. Other Razer
/// receivers answer 00:bf with their slots on 0x08 and 0x88 (OpenRazer
/// #1892) or 0x3f (#2817), and 00:c5 on 0x1f with a lane's product (#1892).
/// The HyperFlux V2 answers 00:bf on all three, and the bridge asks it on
/// 0x08 and 0x3f to name the devices.
pub const RECEIVER_PROBES: [(u8, u8, u8, u8); 4] = [
    (0x08, 0x00, 0xbf, 49),
    (0x88, 0x00, 0xbf, 49),
    (0x3f, 0x00, 0xbf, 31),
    (0x1f, 0x00, 0xc5, 3),
];

#[must_use]
pub fn receiver_probe(i: usize) -> Option<Report> {
    let (tx, class, command, size) = *RECEIVER_PROBES.get(i)?;
    let args = &[0; 49][..usize::from(size)];
    Some(report::build(tx, class, command, args))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mouse,
    Keyboard,
}

impl Kind {
    #[must_use]
    pub const fn lane(self) -> Lane {
        match self {
            Self::Mouse => Lane::Pointer,
            Self::Keyboard => Lane::Keyboard,
        }
    }

    #[must_use]
    pub const fn wire(self) -> DeviceKind {
        match self {
            Self::Mouse => DeviceKind::Mouse,
            Self::Keyboard => DeviceKind::Keyboard,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Verified on real hardware through the receiver.
    Tested,
    /// Razer lists it for the receiver, OpenRazer 3.12.4 or OpenRGB 1.1
    /// drives it, and the entry matches theirs. Not tried through the
    /// receiver.
    Checked,
    /// Razer lists it, but neither OpenRazer 3.12.4 nor OpenRGB 1.1 can
    /// drive it.
    Listed,
}

/// How the status bytes of a passive report map to a charging state. Only
/// decoders checked on hardware exist. A tested or checked device without
/// one is asked 07/84 instead, and its answer is read as OpenRazer reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChargeDecoder {
    /// Pointer status: charge byte 1 is charging, charge 0 with power byte 0
    /// (off the mat) is discharging, and anything else is unknown. A queried
    /// 07/84 answer other than 0 also means charging.
    MouseStatus,
    /// Keyboard status: word power<<8|charge 0x0101 or 0x0001 = charging,
    /// 0x0000 = discharging, anything else unknown. Queried 07/84 answers
    /// are ignored (the keyboard says 0 while full on its cable).
    KeyboardStatus,
    /// No passive decoder: a queried 07/84 answer other than 0 means
    /// charging, and 0 only ends a charging reading.
    None,
}

pub struct OpenRazer {
    /// Kernel driver that binds the virtual device.
    pub driver: &'static str,
    /// Daemon class instantiated for the wireless product id.
    pub class: &'static str,
    /// The daemon's EVENT_FILE_REGEX for this class, if it has one.
    pub event_file_regex: Option<&'static str>,
    /// Transaction id OpenRazer uses for get_serial on this device.
    pub serial_transaction_id: u8,
    /// Suffix of the /dev/input/by-id link our udev rule adds on receiver
    /// interface 1 so OpenRazer finds key presses (keyboards only). The full
    /// name is `hyperflux-next-<ID_PATH_TAG>-<suffix>`.
    pub key_event_link: Option<&'static str>,
}

/// One allowed command: class, id, and the data sizes apps send with it.
pub struct Command {
    pub class: u8,
    pub id: u8,
    pub sizes: &'static [u8],
}

const fn cmd(class: u8, id: u8, sizes: &'static [u8]) -> Command {
    Command { class, id, sizes }
}

pub struct Device {
    /// Product id through the receiver (the device's wireless id).
    pub product_id: u16,
    /// Product id when plugged in by cable.
    pub wired_product_id: u16,
    pub kind: Kind,
    /// Name OpenRazer gives the wireless device, without "(Wireless)", or
    /// OpenRGB's name when OpenRazer doesn't know it.
    pub name: &'static str,
    pub tier: Tier,
    pub charge: ChargeDecoder,
    /// The wait before the first read the bridge starts learning from.
    /// Shorter than 6 ms only with bench results from the device.
    pub first_get_delay_us: u16,
    pub openrazer: Option<OpenRazer>,
    /// The commands OpenRazer or OpenRGB send to this device. Anything
    /// else is refused before it reaches the receiver.
    pub commands: &'static [Command],
}

// The Basilisk V3 Pro, its 35K model and the 35K's Phantom Green Edition
// share their OpenRazer attributes and OpenRGB's 1x13 matrix. For 0f/03,
// OpenRGB sends whole rows (5 bytes of header and 3 per LED) and OpenRazer
// pads each row to 71.
const BASILISK_V3_PRO: &[Command] = &[
    cmd(0x00, 0x04, &[2]),
    cmd(0x00, 0x05, &[1]),
    cmd(0x00, 0x81, &[2]),
    cmd(0x00, 0x82, &[22]),
    cmd(0x00, 0x84, &[2]),
    cmd(0x00, 0x85, &[1]),
    cmd(0x00, 0x86, &[2]),
    cmd(0x02, 0x14, &[2]),
    cmd(0x02, 0x16, &[2]),
    cmd(0x02, 0x17, &[2]),
    cmd(0x02, 0x94, &[2]),
    cmd(0x02, 0x96, &[2]),
    cmd(0x02, 0x97, &[2]),
    cmd(0x04, 0x05, &[7]),
    cmd(0x04, 0x06, &[38]),
    cmd(0x04, 0x85, &[7]),
    cmd(0x04, 0x86, &[38]),
    cmd(0x07, 0x01, &[1]),
    cmd(0x07, 0x03, &[2]),
    cmd(0x07, 0x80, &[2]),
    cmd(0x07, 0x81, &[1]),
    cmd(0x07, 0x83, &[2]),
    cmd(0x07, 0x84, &[2]),
    cmd(0x0f, 0x02, &[6, 9, 12]),
    cmd(0x0f, 0x03, &[44, 71]),
    cmd(0x0f, 0x04, &[3]),
    cmd(0x0f, 0x80, &[80]),
    cmd(0x0f, 0x81, &[80]),
    cmd(0x0f, 0x84, &[3]),
];

pub const DEVICES: &[Device] = &[
    Device {
        product_id: 0x00a8,
        wired_product_id: 0x00a7,
        kind: Kind::Mouse,
        name: "Razer Naga V2 Pro",
        tier: Tier::Checked,
        charge: ChargeDecoder::None,
        first_get_delay_us: 6_000,
        openrazer: Some(OpenRazer {
            driver: "razermouse",
            class: "RazerNagaV2ProWireless",
            event_file_regex: None,
            serial_transaction_id: 0x1f,
            key_event_link: None,
        }),
        commands: &[
            cmd(0x00, 0x04, &[2]),
            cmd(0x00, 0x05, &[1]),
            cmd(0x00, 0x81, &[2]),
            cmd(0x00, 0x82, &[22]),
            cmd(0x00, 0x84, &[2]),
            cmd(0x00, 0x85, &[1]),
            cmd(0x00, 0x86, &[2]),
            cmd(0x03, 0x01, &[5]),
            cmd(0x03, 0x0a, &[1]),
            cmd(0x03, 0x0d, &[3, 7, 10]),
            cmd(0x03, 0x10, &[1]),
            cmd(0x04, 0x05, &[7]),
            cmd(0x04, 0x06, &[38]),
            cmd(0x04, 0x85, &[7]),
            cmd(0x04, 0x86, &[38]),
            cmd(0x07, 0x01, &[1]),
            cmd(0x07, 0x03, &[2]),
            cmd(0x07, 0x80, &[2]),
            cmd(0x07, 0x81, &[1]),
            cmd(0x07, 0x83, &[2]),
            cmd(0x07, 0x84, &[2]),
            cmd(0x0f, 0x02, &[6, 9, 12]),
            cmd(0x0f, 0x03, &[11, 14]),
            cmd(0x0f, 0x04, &[3]),
            cmd(0x0f, 0x80, &[80]),
            cmd(0x0f, 0x81, &[80]),
            cmd(0x0f, 0x84, &[3]),
        ],
    },
    Device {
        product_id: 0x00ab,
        wired_product_id: 0x00aa,
        kind: Kind::Mouse,
        name: "Razer Basilisk V3 Pro",
        tier: Tier::Checked,
        charge: ChargeDecoder::None,
        first_get_delay_us: 6_000,
        openrazer: Some(OpenRazer {
            driver: "razermouse",
            class: "RazerBasiliskV3ProWireless",
            event_file_regex: Some(".*Razer_Basilisk_V3_Pro_000000000000-if0(1|2)-event-kbd"),
            serial_transaction_id: 0x1f,
            key_event_link: None,
        }),
        commands: BASILISK_V3_PRO,
    },
    Device {
        product_id: 0x00b0,
        wired_product_id: 0x00af,
        kind: Kind::Mouse,
        name: "Razer Cobra Pro",
        tier: Tier::Checked,
        charge: ChargeDecoder::None,
        first_get_delay_us: 6_000,
        openrazer: Some(OpenRazer {
            driver: "razermouse",
            class: "RazerCobraProWireless",
            event_file_regex: Some(".*usb-Razer_Razer_Cobra_Pro-if0(1|2)-event-kbd"),
            serial_transaction_id: 0x1f,
            key_event_link: None,
        }),
        commands: &[
            cmd(0x00, 0x04, &[2]),
            cmd(0x00, 0x05, &[1]),
            cmd(0x00, 0x81, &[2]),
            cmd(0x00, 0x82, &[22]),
            cmd(0x00, 0x84, &[2]),
            cmd(0x00, 0x85, &[1]),
            cmd(0x00, 0x86, &[2]),
            cmd(0x04, 0x05, &[7]),
            cmd(0x04, 0x06, &[38]),
            cmd(0x04, 0x85, &[7]),
            cmd(0x04, 0x86, &[38]),
            cmd(0x07, 0x01, &[1]),
            cmd(0x07, 0x03, &[2]),
            cmd(0x07, 0x80, &[2]),
            cmd(0x07, 0x81, &[1]),
            cmd(0x07, 0x83, &[2]),
            cmd(0x07, 0x84, &[2]),
            cmd(0x0f, 0x02, &[6, 9, 12]),
            cmd(0x0f, 0x03, &[38]),
            cmd(0x0f, 0x04, &[3]),
            cmd(0x0f, 0x80, &[80]),
            cmd(0x0f, 0x81, &[80]),
            cmd(0x0f, 0x84, &[3]),
        ],
    },
    Device {
        product_id: 0x00cd,
        wired_product_id: 0x00cc,
        kind: Kind::Mouse,
        name: "Razer Basilisk V3 Pro 35K",
        tier: Tier::Tested,
        charge: ChargeDecoder::MouseStatus,
        first_get_delay_us: 3_000,
        openrazer: Some(OpenRazer {
            driver: "razermouse",
            class: "RazerBasiliskV3Pro35KWireless",
            event_file_regex: Some(".*Razer_Basilisk_V3_Pro_35K_000000000000-if0(1|2)-event-kbd"),
            serial_transaction_id: 0x1f,
            key_event_link: None,
        }),
        commands: BASILISK_V3_PRO,
    },
    Device {
        product_id: 0x00d7,
        wired_product_id: 0x00d6,
        kind: Kind::Mouse,
        name: "Razer Basilisk V3 Pro 35K Phantom Green Edition",
        tier: Tier::Checked,
        charge: ChargeDecoder::None,
        first_get_delay_us: 6_000,
        openrazer: Some(OpenRazer {
            driver: "razermouse",
            class: "RazerBasiliskV3Pro35KPhantomGreenEditionWireless",
            event_file_regex: None,
            serial_transaction_id: 0x1f,
            key_event_link: None,
        }),
        commands: BASILISK_V3_PRO,
    },
    Device {
        product_id: 0x00db,
        wired_product_id: 0x00da,
        kind: Kind::Mouse,
        name: "Razer Cobra HyperSpeed",
        tier: Tier::Listed,
        charge: ChargeDecoder::None,
        first_get_delay_us: 6_000,
        openrazer: None,
        commands: &[],
    },
    Device {
        product_id: 0x00e3,
        wired_product_id: 0x00e2,
        kind: Kind::Mouse,
        name: "Razer Basilisk V4 Pro",
        tier: Tier::Listed,
        charge: ChargeDecoder::None,
        first_get_delay_us: 6_000,
        openrazer: None,
        commands: &[],
    },
    Device {
        product_id: 0x00e8,
        wired_product_id: 0x00e7,
        kind: Kind::Mouse,
        name: "Razer Naga Pro V3",
        tier: Tier::Checked,
        charge: ChargeDecoder::None,
        first_get_delay_us: 6_000,
        openrazer: None,
        commands: &[
            cmd(0x00, 0x81, &[2]),
            cmd(0x00, 0x82, &[22]),
            cmd(0x00, 0x84, &[2]),
            cmd(0x00, 0x86, &[2]),
            cmd(0x0f, 0x02, &[6, 9, 12]),
            cmd(0x0f, 0x03, &[14]),
            cmd(0x0f, 0x04, &[3]),
            cmd(0x0f, 0x80, &[80]),
            cmd(0x0f, 0x81, &[80]),
        ],
    },
    Device {
        product_id: 0x00ea,
        wired_product_id: 0x00eb,
        kind: Kind::Mouse,
        name: "Razer Basilisk V4 HyperSpeed",
        tier: Tier::Listed,
        charge: ChargeDecoder::None,
        first_get_delay_us: 6_000,
        openrazer: None,
        commands: &[],
    },
    Device {
        product_id: 0x025c,
        wired_product_id: 0x025a,
        kind: Kind::Keyboard,
        name: "Razer BlackWidow V3 Pro",
        tier: Tier::Checked,
        charge: ChargeDecoder::None,
        first_get_delay_us: 6_000,
        openrazer: Some(OpenRazer {
            driver: "razerkbd",
            class: "RazerBlackWidowV3ProWireless",
            event_file_regex: Some(".*Razer_Razer_BlackWidow_V3_Pro(_\\d+)?(-if01)?-event-kbd"),
            serial_transaction_id: 0xff,
            key_event_link: Some("Razer_Razer_BlackWidow_V3_Pro-if01-event-kbd"),
        }),
        commands: &[
            cmd(0x00, 0x04, &[2]),
            cmd(0x00, 0x81, &[2]),
            cmd(0x00, 0x82, &[22]),
            cmd(0x00, 0x84, &[2]),
            cmd(0x00, 0x86, &[2]),
            cmd(0x03, 0x00, &[3]),
            cmd(0x03, 0x01, &[5]),
            cmd(0x03, 0x02, &[3]),
            cmd(0x03, 0x10, &[1]),
            cmd(0x03, 0x80, &[3]),
            cmd(0x03, 0x82, &[3]),
            cmd(0x07, 0x01, &[1]),
            cmd(0x07, 0x80, &[2]),
            cmd(0x07, 0x81, &[1]),
            cmd(0x07, 0x84, &[2]),
            cmd(0x0f, 0x02, &[6, 9, 12]),
            cmd(0x0f, 0x03, &[71]),
            cmd(0x0f, 0x04, &[3]),
            cmd(0x0f, 0x81, &[80]),
            cmd(0x0f, 0x84, &[3]),
        ],
    },
    Device {
        product_id: 0x0290,
        wired_product_id: 0x0292,
        kind: Kind::Keyboard,
        name: "Razer DeathStalker V2 Pro",
        tier: Tier::Checked,
        charge: ChargeDecoder::None,
        first_get_delay_us: 6_000,
        openrazer: Some(OpenRazer {
            driver: "razerkbd",
            class: "RazerDeathStalkerV2ProWireless",
            event_file_regex: Some(".*DSV2_Pro_000000000000-event-kbd"),
            serial_transaction_id: 0xff,
            key_event_link: Some("DSV2_Pro_000000000000-event-kbd"),
        }),
        commands: &[
            cmd(0x00, 0x04, &[2]),
            cmd(0x00, 0x81, &[2]),
            cmd(0x00, 0x82, &[22]),
            cmd(0x00, 0x84, &[2]),
            cmd(0x00, 0x86, &[2]),
            cmd(0x03, 0x00, &[3]),
            cmd(0x03, 0x02, &[3]),
            cmd(0x03, 0x80, &[3]),
            cmd(0x03, 0x82, &[3]),
            cmd(0x07, 0x80, &[2]),
            cmd(0x07, 0x84, &[2]),
            cmd(0x0f, 0x02, &[6, 9, 12]),
            cmd(0x0f, 0x03, &[71]),
            cmd(0x0f, 0x04, &[3]),
            cmd(0x0f, 0x80, &[80]),
            cmd(0x0f, 0x81, &[80]),
            cmd(0x0f, 0x84, &[3]),
        ],
    },
    Device {
        product_id: 0x0296,
        wired_product_id: 0x0298,
        kind: Kind::Keyboard,
        name: "Razer DeathStalker V2 Pro TKL",
        tier: Tier::Tested,
        charge: ChargeDecoder::KeyboardStatus,
        first_get_delay_us: 4_000,
        openrazer: Some(OpenRazer {
            driver: "razerkbd",
            class: "RazerDeathStalkerV2ProTKLWireless",
            event_file_regex: Some(".*DSV2Pro_TKL_000000000000-if01-event-mouse"),
            serial_transaction_id: 0xff,
            key_event_link: Some("DSV2Pro_TKL_000000000000-if01-event-mouse"),
        }),
        commands: &[
            cmd(0x00, 0x04, &[2]),
            cmd(0x00, 0x81, &[2]),
            cmd(0x00, 0x82, &[22]),
            cmd(0x00, 0x84, &[2]),
            cmd(0x00, 0x86, &[2]),
            cmd(0x03, 0x00, &[3]),
            cmd(0x03, 0x02, &[3]),
            cmd(0x03, 0x80, &[3]),
            cmd(0x03, 0x82, &[3]),
            cmd(0x07, 0x80, &[2]),
            cmd(0x07, 0x84, &[2]),
            cmd(0x0f, 0x02, &[6, 9, 12]),
            cmd(0x0f, 0x03, &[56, 71]),
            cmd(0x0f, 0x04, &[3]),
            cmd(0x0f, 0x80, &[80]),
            cmd(0x0f, 0x81, &[80]),
            cmd(0x0f, 0x84, &[3]),
        ],
    },
    Device {
        product_id: 0x02ba,
        wired_product_id: 0x02b9,
        kind: Kind::Keyboard,
        name: "Razer BlackWidow V4 Mini Hyperspeed",
        tier: Tier::Checked,
        charge: ChargeDecoder::None,
        first_get_delay_us: 6_000,
        openrazer: Some(OpenRazer {
            driver: "razerkbd",
            class: "RazerBlackWidowV4MiniHyperSpeedWireless",
            event_file_regex: Some(".*Razer_BlackWidow_V4_Mini_(-if01)?-event-kbd"),
            serial_transaction_id: 0xff,
            key_event_link: Some("Razer_BlackWidow_V4_Mini_-if01-event-kbd"),
        }),
        commands: &[
            cmd(0x00, 0x04, &[2]),
            cmd(0x00, 0x81, &[2]),
            cmd(0x00, 0x82, &[22]),
            cmd(0x00, 0x84, &[2]),
            cmd(0x00, 0x86, &[2]),
            cmd(0x03, 0x00, &[3]),
            cmd(0x03, 0x02, &[3]),
            cmd(0x03, 0x80, &[3]),
            cmd(0x03, 0x82, &[3]),
            cmd(0x07, 0x80, &[2]),
            cmd(0x07, 0x84, &[2]),
            cmd(0x0f, 0x02, &[6, 9, 12]),
            cmd(0x0f, 0x03, &[71]),
            cmd(0x0f, 0x04, &[3]),
            cmd(0x0f, 0x84, &[3]),
        ],
    },
    Device {
        product_id: 0x02c9,
        wired_product_id: 0x02cc,
        kind: Kind::Keyboard,
        name: "Razer BlackWidow V4 Low-profile HyperSpeed",
        tier: Tier::Listed,
        charge: ChargeDecoder::None,
        first_get_delay_us: 6_000,
        openrazer: None,
        commands: &[],
    },
    Device {
        product_id: 0x02d2,
        wired_product_id: 0x02d4,
        kind: Kind::Keyboard,
        name: "Razer Blackwidow V4 Low Profile TKL",
        tier: Tier::Checked,
        charge: ChargeDecoder::None,
        first_get_delay_us: 6_000,
        openrazer: None,
        commands: &[
            cmd(0x00, 0x81, &[2]),
            cmd(0x00, 0x82, &[22]),
            cmd(0x00, 0x84, &[2]),
            cmd(0x00, 0x86, &[2]),
            cmd(0x0f, 0x02, &[6, 9, 12]),
            cmd(0x0f, 0x03, &[59]),
            cmd(0x0f, 0x04, &[3]),
            cmd(0x0f, 0x81, &[80]),
        ],
    },
    Device {
        product_id: 0x02d5,
        wired_product_id: 0x02d7,
        kind: Kind::Keyboard,
        name: "Razer BlackWidow V4 Tenkeyless HyperSpeed",
        tier: Tier::Checked,
        charge: ChargeDecoder::None,
        first_get_delay_us: 6_000,
        openrazer: Some(OpenRazer {
            driver: "razerkbd",
            class: "RazerBlackWidowV4TenkeylessHyperSpeedWireless",
            event_file_regex: Some(
                ".*Razer_Razer_BlackWidow_V4_Tenkeyless_HyperSpeed(_\\d+)?(-if01)?-event-mouse",
            ),
            serial_transaction_id: 0xff,
            key_event_link: Some(
                "Razer_Razer_BlackWidow_V4_Tenkeyless_HyperSpeed-if01-event-mouse",
            ),
        }),
        commands: &[
            cmd(0x00, 0x04, &[2]),
            cmd(0x00, 0x40, &[2]),
            cmd(0x00, 0x81, &[2]),
            cmd(0x00, 0x82, &[22]),
            cmd(0x00, 0x84, &[2]),
            cmd(0x00, 0x86, &[2]),
            cmd(0x00, 0xc0, &[1]),
            cmd(0x03, 0x00, &[3]),
            cmd(0x03, 0x80, &[3]),
            cmd(0x07, 0x80, &[2]),
            cmd(0x07, 0x84, &[2]),
            cmd(0x0f, 0x02, &[6, 9, 12]),
            cmd(0x0f, 0x03, &[59, 71]),
            cmd(0x0f, 0x04, &[3]),
            cmd(0x0f, 0x81, &[80]),
            cmd(0x0f, 0x84, &[3]),
        ],
    },
];

#[must_use]
pub fn device(product_id: u16) -> Option<&'static Device> {
    DEVICES.iter().find(|d| d.product_id == product_id)
}

/// The 2.x module's first-GET wait, kept for a product the table doesn't
/// list.
pub const UNLISTED_FIRST_GET_DELAY_US: u16 = 8_000;

/// The first-GET wait the bridge starts from for this product, before it
/// learns the device's own.
#[must_use]
pub fn first_get_delay_us(product_id: u16) -> u16 {
    device(product_id).map_or(UNLISTED_FIRST_GET_DELAY_US, |d| d.first_get_delay_us)
}

impl Device {
    #[must_use]
    pub const fn support(&self) -> Support {
        match self.tier {
            Tier::Tested => Support::Tested,
            Tier::Checked => Support::Checked,
            Tier::Listed => Support::Listed,
        }
    }

    /// Whether the bridge offers a route token for this device.
    #[must_use]
    pub fn qualified(&self) -> bool {
        self.tier != Tier::Listed && !self.commands.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandRefusal {
    /// Status, protocol type, checksum or reserved byte wrong.
    Shape,
    /// Transaction id 0 or non-zero remaining-packets bytes.
    Header,
    /// Data size 0 or bytes after the arguments not zero.
    Arguments,
    /// Class, id and size aren't in the device's list.
    NotListed,
    /// Device mode other than 0x00 (normal) or 0x03 (driver).
    DeviceMode,
}

/// The command check a report goes through before it can reach the
/// receiver, whichever client sent it. It keeps the command surface to what
/// OpenRazer and OpenRGB actually send. Argument values are left to the
/// device firmware, except the device mode, which must stay 0 or 3.
///
/// # Errors
/// Returns why the report was refused.
pub fn check_command(device: &Device, report: &Report) -> Result<(), CommandRefusal> {
    if !report::request_shape_ok(report) {
        return Err(CommandRefusal::Shape);
    }
    if report[report::TRANSACTION_ID] == 0 || report[2] != 0 || report[3] != 0 {
        return Err(CommandRefusal::Header);
    }
    let size = report[report::DATA_SIZE];
    let args_end = report::ARGS + usize::from(size);
    if size == 0 || report[args_end..report::CHECKSUM].iter().any(|b| *b != 0) {
        return Err(CommandRefusal::Arguments);
    }
    let class = report[report::CLASS];
    let id = report[report::COMMAND];
    let listed = device
        .commands
        .iter()
        .any(|c| c.class == class && c.id == id && c.sizes.contains(&size));
    if !listed {
        return Err(CommandRefusal::NotListed);
    }
    if class == 0x00 && id == 0x04 {
        let mode = report[report::ARGS];
        if !(mode == 0x00 || mode == 0x03) || report[report::ARGS + 1] != 0 {
            return Err(CommandRefusal::DeviceMode);
        }
    }
    Ok(())
}

/// The receiver module of the 2.x line. 1.0 has none, and Doctor looks for
/// what is left of it.
pub const OLD_MODULE: &str = "hid-hyperflux-next";

/// Pre-release drivers that claim the same receiver. Doctor warns when DKMS
/// still has one registered, because a kernel update rebuilds it.
pub const RETIRED_DRIVERS: &[(&str, &str)] = &[
    ("hid-razer-hyperflux-v2", "0.1.0"),
    ("hid-razer-hyperflux-v2", "0.1.1"),
    ("hid-razer-hyperflux-v2", "0.1.2"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_consistent() {
        for (i, d) in DEVICES.iter().enumerate() {
            assert!(
                DEVICES[i + 1..]
                    .iter()
                    .all(|o| o.product_id != d.product_id)
            );
            assert!(d.name.starts_with("Razer "), "{}", d.name);
            assert_eq!(d.qualified(), d.tier != Tier::Listed, "{}", d.name);
            // The range bench accepts. Anything shorter than 6 ms was benched.
            assert!((1_000..=8_000).contains(&d.first_get_delay_us));
            assert!(d.first_get_delay_us >= 6_000 || d.tier == Tier::Tested);
            for c in d.commands {
                assert!(!c.sizes.is_empty() && c.sizes.windows(2).all(|p| p[0] < p[1]));
                assert!(c.sizes.iter().all(|s| (1..=80).contains(s)));
            }
            if let Some(o) = &d.openrazer {
                // The serial query is how routes are checked before publishing.
                let serial = d.commands.iter().any(|c| (c.class, c.id) == (0, 0x82));
                assert!(serial, "{}", d.name);
                assert_eq!(
                    o.key_event_link.is_some(),
                    d.kind == Kind::Keyboard,
                    "{}",
                    d.name
                );
            }
        }
    }

    #[test]
    fn first_get_waits() {
        assert_eq!(first_get_delay_us(0x00cd), 3_000);
        assert_eq!(first_get_delay_us(0x0296), 4_000);
        assert_eq!(first_get_delay_us(0x02d5), 6_000);
        assert_eq!(first_get_delay_us(0x1234), UNLISTED_FIRST_GET_DELAY_US);
    }

    #[test]
    fn command_check() {
        let mouse = device(0x00cd).unwrap();
        assert_eq!(
            check_command(mouse, &report::build(0x1f, 0x07, 0x80, &[0, 0])),
            Ok(())
        );
        assert_eq!(
            check_command(mouse, &report::build(0x1f, 0x07, 0x80, &[0])),
            Err(CommandRefusal::NotListed)
        );
        assert_eq!(
            check_command(mouse, &report::build(0x00, 0x07, 0x80, &[0, 0])),
            Err(CommandRefusal::Header)
        );
        assert_eq!(
            check_command(mouse, &report::build(0x1f, 0x00, 0x04, &[0x03, 0])),
            Ok(())
        );
        assert_eq!(
            check_command(mouse, &report::build(0x1f, 0x00, 0x04, &[0x01, 0])),
            Err(CommandRefusal::DeviceMode)
        );
        let mut trailing = report::build(0x1f, 0x07, 0x80, &[0, 0]);
        trailing[20] = 1;
        trailing[report::CHECKSUM] = report::checksum(&trailing);
        assert_eq!(
            check_command(mouse, &trailing),
            Err(CommandRefusal::Arguments)
        );
        let mut bad_sum = report::build(0x1f, 0x07, 0x80, &[0, 0]);
        bad_sum[report::CHECKSUM] ^= 1;
        assert_eq!(check_command(mouse, &bad_sum), Err(CommandRefusal::Shape));
    }

    #[test]
    fn openrgb_setup_queries() {
        // OpenRGB 1.1 asks for the lighting regions and their effects while
        // it builds a controller, and the keyboard layout of every device.
        let mut regions = [0_u8; 80];
        regions[0] = 0x05;
        let mouse = device(0x00cd).unwrap();
        for (class, id, args) in [(0x0f, 0x80, &[0; 80]), (0x0f, 0x81, &regions)] {
            let query = report::build(0x1f, class, id, args);
            assert_eq!(check_command(mouse, &query), Ok(()));
        }
        let layout = report::build(0x1f, 0x00, 0x86, &[0, 0]);
        assert_eq!(check_command(mouse, &layout), Ok(()));
        let short = report::build(0x9f, 0x0f, 0x81, &[0x05]);
        assert_eq!(
            check_command(device(0x0296).unwrap(), &short),
            Err(CommandRefusal::NotListed)
        );
    }
}
