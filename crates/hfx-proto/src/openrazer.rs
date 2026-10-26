// SPDX-License-Identifier: GPL-2.0-only

//! The contract between the bridge's virtual OpenRazer devices and the
//! patched OpenRazer kernel drivers (integrations/openrazer/).
//!
//! The patch trusts a HID device as a HyperFlux route only if all of this
//! holds: bus BUS_VIRTUAL, vendor 0x1532, phys starting with [`PHYS_PREFIX`]
//! followed by a non-empty `[a-z0-9-]` label, a report descriptor equal to
//! [`DESCRIPTOR`], and a product id already in the driver's own USB table.
//! Only root and the hyperflux-keeper group can open /dev/uhid, and the
//! keeper is the group's sole member, so the keeper alone can create such
//! a device. A uaccess ACL on /dev/uhid gets around that (ARCHITECTURE.md,
//! section 7), which is why Doctor reports one.
//!
//! tests/test_device_tables.py checks the C copies of these values against
//! this file.

use crate::report::{REPORT_LEN, Report};

pub const BUS_VIRTUAL: u16 = 0x06;
pub const UHID_VERSION: u32 = 0x0100;
pub const PHYS_PREFIX: &str = "hyperflux-next/native/v1/";
/// Longest label after the prefix (phys is a 64-byte field with a NUL).
pub const MAX_LABEL_LEN: usize = 24;

/// Vendor page 0xff00, one 90-byte feature report without a report id, no
/// input or output items, so hid-input creates no input node for it.
pub const DESCRIPTOR: [u8; 19] = [
    0x06, 0x00, 0xff, // Usage Page (0xff00)
    0x09, 0x01, // Usage (1)
    0xa1, 0x01, // Collection (Application)
    0x15, 0x00, // Logical Minimum (0)
    0x26, 0xff, 0x00, // Logical Maximum (255)
    0x75, 0x08, // Report Size (8)
    0x95, 0x5a, // Report Count (90)
    0xb1, 0x02, // Feature (Data, Var, Abs)
    0xc0, // End Collection
];

/// Read-only module parameter the patched razerkbd and razermouse expose,
/// at /sys/module/<module>/parameters/<name>. The bridge creates virtual
/// devices only when a loaded driver reports this value. The same pair is
/// also a MODULE_INFO tag, for `modinfo -F` to check the installed file.
pub const CAPABILITY_NAME: &str = "hyperflux_native_transport";
pub const CAPABILITY_VALUE: &str = "razer-feature-report-v1";

/// OpenRazer's kernel modules, loaded at boot by the Full package so they
/// are bound before the first virtual device appears.
pub const MODULES: &[&str] = &["razeraccessory", "razerkbd", "razerkraken", "razermouse"];
/// The modules that carry the capability parameter.
pub const PATCHED_MODULES: &[&str] = &["razerkbd", "razermouse"];

/// What the bridge answers a GET with when it has no device response to
/// give. The patched driver turns it into an errno for openrazer-daemon:
/// not-sent becomes EAGAIN, may-have-been-sent becomes ENOTRECOVERABLE.
/// Delivered as a successful 90-byte GET reply, because uhid maps every
/// non-zero reply error to EIO and that would lose the distinction.
pub const ENVELOPE_MAGIC: &[u8; 8] = b"HFXNERR1";
pub const ENVELOPE_VERSION: u8 = 1;
pub const ENVELOPE_NOT_SENT: u8 = 1;
pub const ENVELOPE_MAY_HAVE_BEEN_SENT: u8 = 2;
pub const ENVELOPE_TRAILER: [u8; 2] = [0x5a, 0xa5];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uncertainty {
    NotSent,
    MayHaveBeenSent,
}

#[must_use]
pub fn envelope(kind: Uncertainty) -> Report {
    let mut report = [0_u8; REPORT_LEN];
    report[..8].copy_from_slice(ENVELOPE_MAGIC);
    report[8] = ENVELOPE_VERSION;
    report[9] = match kind {
        Uncertainty::NotSent => ENVELOPE_NOT_SENT,
        Uncertainty::MayHaveBeenSent => ENVELOPE_MAY_HAVE_BEEN_SENT,
    };
    report[88..].copy_from_slice(&ENVELOPE_TRAILER);
    report
}

/// The inverse of [`envelope`]. `None` for an ordinary device response.
#[must_use]
pub fn parse_envelope(report: &Report) -> Option<Uncertainty> {
    if &report[..8] != ENVELOPE_MAGIC
        || report[8] != ENVELOPE_VERSION
        || report[88..] != ENVELOPE_TRAILER
        || report[10..88].iter().any(|b| *b != 0)
    {
        return None;
    }
    match report[9] {
        ENVELOPE_NOT_SENT => Some(Uncertainty::NotSent),
        ENVELOPE_MAY_HAVE_BEEN_SENT => Some(Uncertainty::MayHaveBeenSent),
        _ => None,
    }
}

/// Phys label check used before UHID_CREATE2.
#[must_use]
pub fn label_ok(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= MAX_LABEL_LEN
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trip() {
        for kind in [Uncertainty::NotSent, Uncertainty::MayHaveBeenSent] {
            assert_eq!(parse_envelope(&envelope(kind)), Some(kind));
        }
        let mut damaged = envelope(Uncertainty::NotSent);
        damaged[40] = 1;
        assert_eq!(parse_envelope(&damaged), None);
        assert_eq!(parse_envelope(&[0; REPORT_LEN]), None);
    }

    #[test]
    fn labels() {
        assert!(label_ok("native-0123456789abcdef"));
        assert!(!label_ok(""));
        assert!(!label_ok("Native-1"));
        assert!(!label_ok(&"a".repeat(25)));
        assert!(PHYS_PREFIX.len() + MAX_LABEL_LEN < 64);
    }
}
