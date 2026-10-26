// SPDX-License-Identifier: GPL-2.0-only

//! `struct uhid_event` from linux/uhid.h, the subset a feature-only device
//! needs. Events are the full 4376-byte packed structure in native byte
//! order. The kernel accepts shorter writes, but one size keeps it simple.

use hfx_proto::openrazer::{BUS_VIRTUAL, DESCRIPTOR, PHYS_PREFIX, UHID_VERSION};
use hfx_proto::report::{REPORT_LEN, Report};

pub const EVENT_BYTES: usize = 4376;
const DATA_MAX: usize = 4096;

const DESTROY: u32 = 1;
const START: u32 = 2;
const STOP: u32 = 3;
const OPEN: u32 = 4;
const CLOSE: u32 = 5;
const OUTPUT: u32 = 6;
const GET_REPORT: u32 = 9;
const GET_REPORT_REPLY: u32 = 10;
const CREATE2: u32 = 11;
const INPUT2: u32 = 12;
const SET_REPORT: u32 = 13;
const SET_REPORT_REPLY: u32 = 14;

pub const FEATURE_REPORT: u8 = 0;
/// `UHID_DEV_NUMBERED_FEATURE_REPORTS`: our descriptor has no report ids,
/// so a start with this flag means the kernel parsed something else.
pub const NUMBERED_FEATURE_REPORTS: u64 = 1 << 0;
pub const EIO: u16 = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Start {
        flags: u64,
    },
    Stop,
    Open,
    Close,
    Output,
    GetReport {
        id: u32,
        number: u8,
        kind: u8,
    },
    SetReport {
        id: u32,
        number: u8,
        kind: u8,
        data: Vec<u8>,
    },
    Other(u32),
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Decodes what one read() of /dev/uhid returned.
#[must_use]
pub fn decode(bytes: &[u8]) -> Option<Event> {
    let kind = u32_at(bytes, 0)?;
    Some(match kind {
        START => Event::Start {
            flags: u64::from_ne_bytes(bytes.get(4..12)?.try_into().ok()?),
        },
        STOP => Event::Stop,
        OPEN => Event::Open,
        CLOSE => Event::Close,
        OUTPUT => Event::Output,
        GET_REPORT => Event::GetReport {
            id: u32_at(bytes, 4)?,
            number: *bytes.get(8)?,
            kind: *bytes.get(9)?,
        },
        SET_REPORT => {
            let size = usize::from(u16::from_ne_bytes(bytes.get(10..12)?.try_into().ok()?));
            if size > DATA_MAX {
                return None;
            }
            Event::SetReport {
                id: u32_at(bytes, 4)?,
                number: *bytes.get(8)?,
                kind: *bytes.get(9)?,
                data: bytes.get(12..12 + size)?.to_vec(),
            }
        }
        other => Event::Other(other),
    })
}

fn event(kind: u32) -> Vec<u8> {
    let mut out = vec![0_u8; EVENT_BYTES];
    out[..4].copy_from_slice(&kind.to_ne_bytes());
    out
}

fn put_str(out: &mut [u8], text: &str) {
    out[..text.len()].copy_from_slice(text.as_bytes());
}

pub const NAME_MAX: usize = 127;

/// UHID_CREATE2 on the virtual bus, with `ids` as vendor and product.
///
/// # Panics
/// If a string doesn't fit its field, which the callers rule out.
#[must_use]
pub fn create_device(
    name: &str,
    phys: &str,
    uniq: &str,
    ids: (u16, u16),
    descriptor: &[u8],
) -> Vec<u8> {
    let name = &name[..name.len().min(NAME_MAX)];
    assert!(phys.len() < 64 && uniq.len() < 64 && descriptor.len() <= DATA_MAX);
    let mut out = event(CREATE2);
    put_str(&mut out[4..132], name);
    put_str(&mut out[132..196], phys);
    put_str(&mut out[196..260], uniq);
    let size = u16::try_from(descriptor.len()).unwrap_or(0);
    out[260..262].copy_from_slice(&size.to_ne_bytes());
    out[262..264].copy_from_slice(&BUS_VIRTUAL.to_ne_bytes());
    out[264..268].copy_from_slice(&u32::from(ids.0).to_ne_bytes());
    out[268..272].copy_from_slice(&u32::from(ids.1).to_ne_bytes());
    out[272..276].copy_from_slice(&UHID_VERSION.to_ne_bytes());
    out[280..280 + descriptor.len()].copy_from_slice(descriptor);
    out
}

/// The feature-only device the patched OpenRazer drivers trust: phys under
/// the HyperFlux prefix, our descriptor, no uniq.
///
/// # Panics
/// If `label` is not a valid label, which the callers rule out.
#[must_use]
pub fn create(name: &str, label: &str, product_id: u16) -> Vec<u8> {
    assert!(hfx_proto::openrazer::label_ok(label));
    create_device(
        name,
        &format!("{PHYS_PREFIX}{label}"),
        "",
        (hfx_proto::devices::RECEIVER.vendor_id, product_id),
        &DESCRIPTOR,
    )
}

/// UHID_INPUT2: an input report from the device.
#[must_use]
pub fn input(report: &[u8]) -> Vec<u8> {
    let mut out = event(INPUT2);
    let size = report.len().min(DATA_MAX);
    out[4..6].copy_from_slice(&u16::try_from(size).unwrap_or(0).to_ne_bytes());
    out[6..6 + size].copy_from_slice(&report[..size]);
    out
}

#[must_use]
pub fn destroy() -> Vec<u8> {
    event(DESTROY)
}

/// The answer to a GET: the report, or an errno without data.
#[must_use]
pub fn get_reply(id: u32, data: Result<&Report, u16>) -> Vec<u8> {
    let mut out = event(GET_REPORT_REPLY);
    out[4..8].copy_from_slice(&id.to_ne_bytes());
    match data {
        Ok(report) => {
            out[10..12].copy_from_slice(&(REPORT_LEN as u16).to_ne_bytes());
            out[12..12 + REPORT_LEN].copy_from_slice(report);
        }
        Err(errno) => out[8..10].copy_from_slice(&errno.to_ne_bytes()),
    }
    out
}

#[must_use]
pub fn set_reply(id: u32, errno: u16) -> Vec<u8> {
    let mut out = event(SET_REPORT_REPLY);
    out[4..8].copy_from_slice(&id.to_ne_bytes());
    out[8..10].copy_from_slice(&errno.to_ne_bytes());
    out
}

/// What the kernel side of a test reads back from the bridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Written {
    Create {
        name: String,
        phys: String,
        uniq: String,
        bus: u16,
        vendor: u32,
        product: u32,
        version: u32,
        descriptor: Vec<u8>,
    },
    Destroy,
    GetReply {
        id: u32,
        errno: u16,
        data: Vec<u8>,
    },
    SetReply {
        id: u32,
        errno: u16,
    },
    Input(Vec<u8>),
    Other(u32),
}

fn c_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// The inverse of the encoders above, used by tests playing the kernel.
#[must_use]
pub fn decode_written(bytes: &[u8]) -> Option<Written> {
    if bytes.len() != EVENT_BYTES {
        return None;
    }
    let u16_at = |at: usize| u16::from_ne_bytes([bytes[at], bytes[at + 1]]);
    Some(match u32_at(bytes, 0)? {
        CREATE2 => {
            let size = usize::from(u16_at(260)).min(DATA_MAX);
            Written::Create {
                name: c_string(&bytes[4..132]),
                phys: c_string(&bytes[132..196]),
                uniq: c_string(&bytes[196..260]),
                bus: u16_at(262),
                vendor: u32_at(bytes, 264)?,
                product: u32_at(bytes, 268)?,
                version: u32_at(bytes, 272)?,
                descriptor: bytes[280..280 + size].to_vec(),
            }
        }
        DESTROY => Written::Destroy,
        GET_REPORT_REPLY => {
            let size = usize::from(u16_at(10)).min(DATA_MAX);
            Written::GetReply {
                id: u32_at(bytes, 4)?,
                errno: u16_at(8),
                data: bytes[12..12 + size].to_vec(),
            }
        }
        SET_REPORT_REPLY => Written::SetReply {
            id: u32_at(bytes, 4)?,
            errno: u16_at(8),
        },
        INPUT2 => Written::Input(bytes[6..6 + usize::from(u16_at(4)).min(DATA_MAX)].to_vec()),
        other => Written::Other(other),
    })
}

/// Kernel-side encoders, for tests.
#[must_use]
pub fn kernel_start(flags: u64) -> Vec<u8> {
    let mut out = event(START);
    out[4..12].copy_from_slice(&flags.to_ne_bytes());
    out
}

#[must_use]
pub fn kernel_stop() -> Vec<u8> {
    event(STOP)
}

#[must_use]
pub fn kernel_open() -> Vec<u8> {
    event(OPEN)
}

#[must_use]
pub fn kernel_get(id: u32) -> Vec<u8> {
    let mut out = event(GET_REPORT);
    out[4..8].copy_from_slice(&id.to_ne_bytes());
    out[8] = 0;
    out[9] = FEATURE_REPORT;
    out
}

#[must_use]
pub fn kernel_set(id: u32, data: &[u8]) -> Vec<u8> {
    let mut out = event(SET_REPORT);
    out[4..8].copy_from_slice(&id.to_ne_bytes());
    out[9] = FEATURE_REPORT;
    let size = data.len().min(DATA_MAX);
    out[10..12].copy_from_slice(&u16::try_from(size).unwrap_or(0).to_ne_bytes());
    out[12..12 + size].copy_from_slice(&data[..size]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_keeps_the_trusted_identity() {
        let bytes = create(
            "Razer Basilisk V3 Pro 35K",
            "native-0123456789abcdef",
            0x00cd,
        );
        assert_eq!(bytes.len(), EVENT_BYTES);
        assert_eq!(
            decode_written(&bytes),
            Some(Written::Create {
                name: "Razer Basilisk V3 Pro 35K".into(),
                phys: "hyperflux-next/native/v1/native-0123456789abcdef".into(),
                uniq: String::new(),
                bus: 0x06,
                vendor: 0x1532,
                product: 0x00cd,
                version: 0x0100,
                descriptor: DESCRIPTOR.to_vec(),
            })
        );
        // uniq and country stay zero.
        assert!(bytes[196..260].iter().all(|b| *b == 0));
        assert!(bytes[276..280].iter().all(|b| *b == 0));
    }

    #[test]
    fn kernel_events_round_trip() {
        let mut data = [0_u8; 90];
        data[7] = 0x80;
        assert_eq!(
            decode(&kernel_set(7, &data)),
            Some(Event::SetReport {
                id: 7,
                number: 0,
                kind: FEATURE_REPORT,
                data: data.to_vec()
            })
        );
        assert_eq!(
            decode(&kernel_get(8)),
            Some(Event::GetReport {
                id: 8,
                number: 0,
                kind: FEATURE_REPORT
            })
        );
        assert_eq!(decode(&kernel_start(0)), Some(Event::Start { flags: 0 }));
        assert_eq!(decode(&kernel_stop()), Some(Event::Stop));
        assert_eq!(decode(&[1, 2]), None);
    }

    #[test]
    fn input_reports() {
        assert_eq!(
            decode_written(&input(&[2, 79, 1])),
            Some(Written::Input(vec![2, 79, 1]))
        );
    }

    #[test]
    fn replies() {
        let report = [9_u8; 90];
        assert_eq!(
            decode_written(&get_reply(3, Ok(&report))),
            Some(Written::GetReply {
                id: 3,
                errno: 0,
                data: report.to_vec()
            })
        );
        assert_eq!(
            decode_written(&get_reply(4, Err(EIO))),
            Some(Written::GetReply {
                id: 4,
                errno: 5,
                data: vec![]
            })
        );
        assert_eq!(
            decode_written(&set_reply(5, 0)),
            Some(Written::SetReply { id: 5, errno: 0 })
        );
    }
}
