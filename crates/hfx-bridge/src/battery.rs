// SPDX-License-Identifier: GPL-2.0-only

//! Batteries on the desktop: one virtual HID device per paired device,
//! whose battery hid-input turns into a power supply. UPower then lists it
//! on its own, typed by the input collection next to it (a mouse for the
//! pointer lane, a keyboard for the keyboard lane), with the model from the
//! device name and the serial from `uniq`.
//!
//! The mouse and keyboard collections exist for that typing. Nothing is
//! ever sent on them. The phys label is outside the prefix the
//! patched OpenRazer drivers trust, and the vendor id isn't Razer's: those
//! drivers list every virtual Razer device, and hid-generic leaves alone
//! any device another driver lists, so a Razer id would leave the battery
//! device without a driver.
//!
//! The device keeper makes these devices from this template when the bridge
//! asks, and sends the one input report they have, the battery's.

use crate::decode::BatteryStatus;
use hfx_proto::messages::Lane;

pub const PHYS_PREFIX: &str = "hyperflux-next/battery/";
/// No vendor: no special HID driver claims it, leaving it to hid-generic.
pub const VENDOR_ID: u16 = 0x0000;
pub const BATTERY_REPORT: u8 = 0x02;
/// When to send the report again after UHID_START or UHID_OPEN. The kernel
/// sends UHID_START from hid_hw_start(), before hid-input has registered the
/// power supply, and drops input reports until it has. Nothing else would
/// send the level again until it changes.
pub const RESEND_AFTER_MS: [u64; 3] = [50, 500, 2_000];

/// Report 2: Battery Strength 0..100 and Battery System Charging, padded
/// to a byte, then the end of the application collection. hid-input starts
/// every battery as discharging, and nothing but the charging usage changes
/// that (`hidinput_setup_battery` in 7.2), so leaving the usage out for a
/// device that cannot say would not make the desktop show unknown.
const BATTERY_ITEMS: [u8; 37] = [
    0x85, 0x02, 0x05, 0x06, 0x09, 0x20, 0x15, 0x00, 0x25, 0x64, 0x75, 0x08, 0x95, 0x01, 0x81,
    0x02, //
    0x05, 0x85, 0x09, 0x44, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02, //
    0x75, 0x07, 0x95, 0x01, 0x81, 0x01, 0xc0,
];

/// A mouse: report 1 with three buttons and X/Y, never sent.
pub const MOUSE: [u8; 51] = [
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x85, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19,
    0x01, //
    0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75,
    0x05, //
    0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95,
    0x02, //
    0x81, 0x06, 0xc0,
];

/// A keyboard: report 1 laid out like a boot keyboard (eight modifier bits,
/// a reserved byte, six key slots), never sent. The modifiers aren't
/// decoration: udev's input_id won't set ID_INPUT_KEYBOARD, which UPower
/// types the battery by, unless every key from KEY_ESC up to KEY_D is there,
/// and KEY_LEFTCTRL among them comes from usage 0xE0.
pub const KEYBOARD: [u8; 44] = [
    0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, 0x01, //
    0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81,
    0x02, //
    0x95, 0x01, 0x75, 0x08, 0x81, 0x01, //
    0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00,
];

#[must_use]
pub fn descriptor(lane: Lane) -> Vec<u8> {
    let mut out = match lane {
        Lane::Pointer => MOUSE.to_vec(),
        Lane::Keyboard => KEYBOARD.to_vec(),
    };
    out.extend_from_slice(&BATTERY_ITEMS);
    out
}

/// The 3-byte input report: id, level, charging.
#[must_use]
pub fn report(capacity: u8, status: BatteryStatus) -> [u8; 3] {
    let charging = matches!(status, BatteryStatus::Charging | BatteryStatus::Full);
    [BATTERY_REPORT, capacity.min(100), u8::from(charging)]
}

/// A battery device's report as the keeper keeps it: what to say, and when
/// to say it again. Each call returns the report to write now, if any.
#[derive(Debug, Default)]
pub struct Resend {
    started: bool,
    want: Option<[u8; 3]>,
    sent: Option<[u8; 3]>,
    /// The START or OPEN the resends count from, and the next one to send.
    resend: Option<(u64, usize)>,
}

impl Resend {
    fn send(&mut self) -> Option<[u8; 3]> {
        let want = self.want.filter(|_| self.started)?;
        self.sent = Some(want);
        Some(want)
    }

    /// A new reading from the bridge.
    pub fn set(&mut self, report: [u8; 3]) -> Option<[u8; 3]> {
        self.want = Some(report);
        if self.sent == self.want {
            return None;
        }
        self.send()
    }

    /// UHID_START, or with `start` false UHID_OPEN: someone opened the input
    /// device, which means hid-input is surely connected by now. Sends now and
    /// schedules the resends.
    pub fn kick(&mut self, now: u64, start: bool) -> Option<[u8; 3]> {
        self.started |= start;
        self.resend = self.started.then_some((now, 0));
        self.send()
    }

    pub fn stop(&mut self) {
        (self.started, self.sent, self.resend) = (false, None, None);
    }

    #[must_use]
    pub fn due(&self) -> Option<u64> {
        self.resend
            .and_then(|(from, next)| RESEND_AFTER_MS.get(next).map(|ms| from + ms))
    }

    pub fn tick(&mut self, now: u64) -> Option<[u8; 3]> {
        let (from, _) = self.resend?;
        if self.due().is_none_or(|at| at > now) {
            return None;
        }
        // A late wake-up sends once and skips what it overslept.
        let next = RESEND_AFTER_MS.iter().position(|ms| from + ms > now);
        self.resend = next.map(|next| (from, next));
        self.send()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_descriptor() {
        let tried: [u8; 88] = [
            0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x85, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09,
            0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02,
            0x95, 0x01, 0x75, 0x05, 0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81,
            0x25, 0x7f, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06, 0xc0, 0x85, 0x02, 0x05, 0x06, 0x09,
            0x20, 0x15, 0x00, 0x25, 0x64, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0x05, 0x85, 0x09,
            0x44, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02, 0x75, 0x07, 0x95,
            0x01, 0x81, 0x01, 0xc0,
        ];
        assert_eq!(descriptor(Lane::Pointer), tried);
    }

    #[test]
    fn keyboard_descriptor() {
        let keyboard = descriptor(Lane::Keyboard);
        let boot = [
            // Generic Desktop, Keyboard, application collection, report 1.
            0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, 0x01, //
            // Keyboard page, usages 0xE0-0xE7 as eight one-bit variables.
            0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08,
            0x81, 0x02, //
            // One reserved byte.
            0x95, 0x01, 0x75, 0x08, 0x81, 0x01, //
            // Six key slots, usages 0x00-0x65.
            0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00,
        ];
        assert_eq!(&keyboard[..boot.len()], &boot);
        assert_eq!(&keyboard[boot.len()..], &BATTERY_ITEMS);
        assert_eq!(&descriptor(Lane::Pointer)[MOUSE.len()..], &BATTERY_ITEMS);
    }

    #[test]
    fn resends_after_start_and_open() {
        let mut r = Resend::default();
        let level = [2, 76, 0];
        // Nothing goes out before the kernel starts the device.
        assert_eq!((r.set(level), r.due()), (None, None));
        assert_eq!(r.kick(1_000, true), Some(level));
        let mut resent = Vec::new();
        while let Some(at) = r.due() {
            assert_eq!(r.tick(at), Some(level));
            resent.push(at);
        }
        assert_eq!(resent, [1_050, 1_500, 3_000]);
        assert_eq!(r.tick(5_000), None, "settled: nothing more");

        // An open starts over; a late wake-up sends once and skips ahead.
        assert_eq!(r.kick(10_000, false), Some(level));
        assert_eq!((r.tick(10_600), r.tick(10_600)), (Some(level), None));
        assert_eq!(r.due(), Some(12_000));

        // A new level goes out at once and the schedule carries it.
        assert_eq!(r.set([2, 75, 0]), Some([2, 75, 0]));
        assert_eq!(r.set([2, 75, 0]), None);
        assert_eq!(r.tick(12_000), Some([2, 75, 0]));
        r.stop();
        assert_eq!((r.due(), r.set([2, 74, 0])), (None, None));
    }

    #[test]
    fn reports() {
        assert_eq!(report(79, BatteryStatus::Charging), [2, 79, 1]);
        assert_eq!(report(100, BatteryStatus::Full), [2, 100, 1]);
        assert_eq!(report(40, BatteryStatus::Unknown), [2, 40, 0]);
    }

    #[test]
    fn phys_fits_and_isnt_trusted() {
        let longest = format!("{PHYS_PREFIX}{}/keyboard", "6b1f0e9c2a7d4e83b5c1f2a0");
        assert!(longest.len() < 64);
        assert!(!longest.starts_with(hfx_proto::openrazer::PHYS_PREFIX));
    }
}
