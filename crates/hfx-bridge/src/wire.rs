// SPDX-License-Identifier: GPL-2.0-only

//! The receiver's own rules, applied by the bridge: which byte picks the
//! lane, which reply answers a request, how long to wait for it, and what
//! the passive reports on the receiver's input interface look like. These
//! were proven on the receiver. Don't loosen them.

use crate::transport::{Control, Event, Exchanged, Failure, Timing, Uncertain};
use hfx_proto::devices::{self, RECEIVER};
use hfx_proto::messages::{self, Inventory, Lane};
use hfx_proto::report::{self, Report};
use rustix::io::Errno;
use std::time::{Duration, Instant};

/// bInterfaceNumber of the receiver's HID interfaces. The exchanges and
/// the mouse's movement.
pub const IF_POINTER: u8 = 0;
/// Keys, media and the vendor reports.
pub const IF_COMPOSITE: u8 = 1;
/// Holds the pairing inventory.
pub const IF_INVENTORY: u8 = 4;

pub const INVENTORY_REPORT_ID: u8 = 0x01;
pub const INVENTORY_LEN: usize = 512;
/// At most one activity event per lane this often.
pub const ACTIVITY_INTERVAL_MS: u64 = 250;
/// Interface 0 is watched for one report, then left alone this long: at
/// about 900 reports a second while the mouse moves, that makes ten
/// wake-ups a second instead of 900.
pub const POINTER_REARM_MS: u64 = 100;
pub const RAW_WINDOW_MS: u64 = 1_000;
pub const RAW_PER_WINDOW: u32 = 4;

/// The 2.x module's timings: 8 ms before the first read (still the wait for
/// an unlisted product), then five reads 10 ms apart. The reads still end
/// 40 ms after the first, and a failed one is still read again 10 ms later.
pub const FIRST_GET_DELAY_US: u16 = devices::UNLISTED_FIRST_GET_DELAY_US;
pub const GET_INTERVAL_US: u16 = 10_000;
pub const MAX_GETS: u32 = 5;
/// Read again this soon after a reply that doesn't match. In
/// `hyperfluxctl bench --reread-busy --quick` early reads got the
/// previous answer, never busy, and a query took 4.2 ms instead of 9.4 on
/// the Basilisk V3 Pro 35K and 5.2 instead of 9.3 on the DeathStalker
/// (ARCHITECTURE.md, section 10).
pub const REREAD_INTERVAL_US: u16 = 1_000;

/// The longest an exchange may take, request and reply.
pub const EXCHANGE_TIMEOUT_MAX_MS: u32 = 1_000;

#[must_use]
pub const fn selector(lane: Lane) -> u8 {
    match lane {
        Lane::Pointer => RECEIVER.pointer_lane_selector,
        Lane::Keyboard => RECEIVER.keyboard_lane_selector,
    }
}

/// The receiver routes by byte 1, so the caller's transaction id gives way
/// to the lane selector. Byte 1 is outside the checksum.
#[must_use]
pub fn route(request: &Report, lane: Lane) -> Report {
    let mut routed = *request;
    routed[report::TRANSACTION_ID] = selector(lane);
    routed
}

/// Whether `response` answers `sent` (as routed): a valid response with
/// the full selector byte, the same remaining-packets bytes, class and
/// command. We once matched class and command alone, and the other lane's
/// late battery reply got through.
#[must_use]
pub fn matches(sent: &Report, response: &Report) -> bool {
    (report::STATUS_BUSY..=report::STATUS_NOT_SUPPORTED).contains(&response[report::STATUS])
        && response[report::PROTOCOL_TYPE] == 0
        && response[report::DATA_SIZE] <= report::MAX_DATA_SIZE
        && response[report::CHECKSUM] == report::checksum(response)
        && response[report::RESERVED] == 0
        && response[report::TRANSACTION_ID] == sent[report::TRANSACTION_ID]
        && response[2..4] == sent[2..4]
        && response[report::CLASS..=report::COMMAND] == sent[report::CLASS..=report::COMMAND]
}

/// What `matches` compares an answer with: one answers any request with this key.
#[must_use]
pub fn answer_key(lane: Lane, r: &Report) -> (Lane, [u8; 4]) {
    (lane, [r[2], r[3], r[report::CLASS], r[report::COMMAND]])
}

/// The response as the caller should see it: with its own transaction id.
#[must_use]
pub fn restore(response: &Report, transaction_id: u8) -> Report {
    let mut restored = *response;
    restored[report::TRANSACTION_ID] = transaction_id;
    restored
}

/// The bridge's side of an exchange: route the request to its lane,
/// accept nothing but a matching reply, and give the caller back its own
/// transaction id. With `unrouted`, one of `devices::RECEIVER_PROBES`
/// (and nothing else) goes out as it is.
pub fn exchange(
    control: &mut dyn Control,
    lane: Lane,
    request: &Report,
    timing: Timing,
    unrouted: bool,
) -> Exchanged {
    let probes = 0..devices::RECEIVER_PROBES.len();
    let probe = unrouted
        && probes
            .into_iter()
            .any(|i| devices::receiver_probe(i) == Some(*request));
    let routed = if probe {
        *request
    } else {
        route(request, lane)
    };
    let out = control.exchange(lane, &routed, timing);
    let result = match out.result {
        Ok(reply) if matches(&routed, &reply) => {
            Ok(restore(&reply, request[report::TRANSACTION_ID]))
        }
        // Sent, and what came back answers something else.
        Ok(_) => Err(Failure::MayHaveBeenSent(Uncertain::NoReply)),
        Err(failure) => Err(failure),
    };
    Exchanged { result, ..out }
}

/// A vendor report from the receiver's input interface (report id 0x05 for
/// the pointer lane, 0x09 for the keyboard lane), byte 0 being the id:
///
/// ```text
/// 05 3b CC PH PL        pointer identity: contact byte, product id (BE)
/// 09 35 RR SS PH PL     keyboard identity: route byte, status, product id
/// 05 31 BB SS CC XX     pointer status: level, power, charge, extra
/// 09 31 BB SR CH XX     keyboard status: level, source, charge, extra
/// ```
///
/// None for anything else, and for an identity naming product 0x0000 or
/// 0xffff (the receiver says that while a mouse is on its cable).
/// [`raw_report`] keeps those for `capture`.
#[must_use]
pub fn passive_report(bytes: &[u8]) -> Option<Event> {
    let lane = match bytes.first()? {
        0x05 => Lane::Pointer,
        0x09 => Lane::Keyboard,
        _ => return None,
    };
    let (product_id, arg0, arg1) = match (lane, bytes.get(1)) {
        (Lane::Pointer, Some(0x3b)) if bytes.len() >= 5 => {
            (u16::from_be_bytes([bytes[3], bytes[4]]), bytes[2], 0)
        }
        (Lane::Keyboard, Some(0x35)) if bytes.len() >= 6 => {
            (u16::from_be_bytes([bytes[4], bytes[5]]), bytes[2], bytes[3])
        }
        (_, Some(0x31)) if bytes.len() >= 6 => {
            return Some(Event::Status {
                lane,
                level: bytes[2],
                power: bytes[3],
                charge: bytes[4],
                extra: bytes[5],
            });
        }
        _ => return None,
    };
    (product_id != 0x0000 && product_id != 0xffff).then_some(Event::Identity {
        lane,
        product_id,
        arg0,
        arg1,
    })
}

/// Vendor report ids. In OpenRazer PR #2800 0x04 carried a mouse's DPI
/// buttons and 0x08 a keyboard's dial, on lanes we don't know.
pub const VENDOR_REPORTS: [u8; 4] = [0x04, 0x05, 0x08, 0x09];

/// A vendor report's first 16 bytes for `capture`, parsed or not, from
/// those bytes and the report's length.
#[must_use]
pub fn raw_report(bytes: &[u8], len: u16) -> Option<Event> {
    let lane = match *bytes.first().filter(|id| VENDOR_REPORTS.contains(id))? {
        0x05 => Some(Lane::Pointer),
        0x09 => Some(Lane::Keyboard),
        _ => None,
    };
    let mut head = [0_u8; 16];
    let n = bytes.len().min(16);
    head[..n].copy_from_slice(&bytes[..n]);
    Some(Event::Raw {
        lane,
        bytes: head,
        len,
    })
}

/// Whether an ordinary input report on interface 1 is the keyboard's. The
/// pointer's movement comes out of interface 0, and none came here as report
/// 0x0a on the tested receiver (ARCHITECTURE.md, section 10).
#[must_use]
pub fn keyboard_input(report_id: u8) -> bool {
    matches!(report_id, 0x01..=0x03)
}

/// At most `limit` events per window, counted in fixed windows.
#[derive(Clone, Copy, Debug, Default)]
pub struct Rate {
    start: u64,
    count: u32,
}

impl Rate {
    pub fn allow(&mut self, now: u64, window: u64, limit: u32) -> bool {
        if self.count == 0 || now.saturating_sub(self.start) >= window {
            self.start = now;
            self.count = 0;
        }
        if self.count >= limit {
            return false;
        }
        self.count += 1;
        true
    }
}

/// The two transfers an exchange is made of: a feature SET of the routed
/// request and a feature GET of whatever the receiver has to say.
pub trait FeaturePipe {
    /// # Errors
    /// The transfer's errno.
    fn set(&mut self, wire: &Report) -> Result<(), Errno>;
    /// # Errors
    /// The transfer's errno.
    fn get(&mut self, response: &mut Report) -> Result<(), Errno>;
}

/// One SET, then GETs until one matches. A first wait of 0 means the 2.x
/// module's 8 ms. A reply that doesn't match (the receiver's previous
/// answer, or the other lane's) is skipped, never returned, and the next
/// read comes `REREAD_INTERVAL_US` later, or at bench's interval. A failed
/// GET is read again 10 ms later, unless the device is gone. The last read
/// never comes later than the fifth of the 2.x module's would. With
/// `reread_busy` (all but some bench queries) a busy answer is kept and the
/// reads go on. It comes back unless something else matches in that time.
/// Returns the reads made, and how many of them followed a reply that
/// didn't match and how many a busy one.
pub fn run_exchange(
    pipe: &mut dyn FeaturePipe,
    wire: &Report,
    timing: Timing,
) -> (Result<Report, Failure>, u8, [u8; 2]) {
    let deadline = Instant::now() + Duration::from_millis(u64::from(timing.timeout_ms));
    let first = match timing.first_get_delay_us {
        0 => FIRST_GET_DELAY_US,
        d => d,
    };
    let interval = match timing.get_interval_us {
        0 => REREAD_INTERVAL_US,
        i => i.min(GET_INTERVAL_US),
    };
    let reads = 1 + (MAX_GETS - 1) * u32::from(GET_INTERVAL_US) / u32::from(interval);
    if pipe.set(wire).is_err() {
        return (
            Err(Failure::MayHaveBeenSent(Uncertain::Transfer)),
            0,
            [0; 2],
        );
    }
    let span = u64::from(first) + u64::from((MAX_GETS - 1) * u32::from(GET_INTERVAL_US));
    let last_get =
        (interval < GET_INTERVAL_US).then(|| Instant::now() + Duration::from_micros(span));
    let (mut gets, mut wait) = (0, first);
    let mut transfer_failed = false;
    let (mut busy, mut rereads, mut after) = (None, [0_u8; 2], None);
    for _ in 0..reads {
        let delay = Duration::from_micros(u64::from(wait));
        // A GET that would start past the deadline is not worth the sleep.
        let start = Instant::now() + delay;
        if start >= deadline || last_get.is_some_and(|last| start > last) {
            break;
        }
        std::thread::sleep(delay);
        let mut response = [0_u8; report::REPORT_LEN];
        (gets, wait) = (gets + 1, interval);
        if let Some(i) = after.take() {
            rereads[i] += 1;
        }
        match pipe.get(&mut response) {
            Err(Errno::NODEV | Errno::SHUTDOWN) => {
                return (
                    Err(Failure::MayHaveBeenSent(Uncertain::Transfer)),
                    gets,
                    rereads,
                );
            }
            Err(_) => (transfer_failed, wait) = (true, GET_INTERVAL_US),
            Ok(()) if !matches(wire, &response) => after = Some(0),
            Ok(()) if timing.reread_busy && response[report::STATUS] == report::STATUS_BUSY => {
                (busy, after) = (Some(response), Some(1));
            }
            Ok(()) => return (Ok(response), gets, rereads),
        }
    }
    if let Some(busy) = busy {
        return (Ok(busy), gets, rereads);
    }
    let why = if transfer_failed {
        Uncertain::Transfer
    } else {
        Uncertain::NoReply
    };
    (Err(Failure::MayHaveBeenSent(why)), gets, rereads)
}

/// Each device's first-GET wait is learned from the catalog's, or from the
/// one kept from an earlier run (`waits.rs`), since bench measured two
/// devices and units and firmware differ: every `LEARN_WINDOW` exchanges it
/// gets `LEARN_STEP_US` shorter if `LEARN_SHORTEN_AT` had the answer at the
/// first read, longer if more than `LEARN_LENGTHEN_OVER` needed another. The
/// match rules stay as they are.
pub const LEARN_WINDOW: u8 = 50;
pub const LEARN_SHORTEN_AT: u8 = 49;
pub const LEARN_LENGTHEN_OVER: u8 = 5;
pub const LEARN_STEP_US: u16 = 500;
/// At 2 ms both tested devices often needed another read. 8 ms is 2.x's wait.
pub const LEARN_MIN_US: u16 = 2_000;
pub const LEARN_MAX_US: u16 = 8_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Learned {
    /// The catalog's wait, where learning started.
    pub from_us: u16,
    pub wait_us: u16,
    /// The kept wait it went on from, if there was one.
    pub saved_us: Option<u16>,
    /// This window's exchanges, first-read answers and second reads.
    counts: [u8; 3],
}

impl Learned {
    #[must_use]
    pub const fn new(from_us: u16) -> Self {
        Self {
            from_us,
            wait_us: from_us,
            saved_us: None,
            counts: [0; 3],
        }
    }

    /// Goes on from a wait kept from an earlier run, within the same limits.
    #[must_use]
    pub fn resume(mut self, saved_us: u16) -> Self {
        self.wait_us = saved_us.clamp(LEARN_MIN_US, LEARN_MAX_US);
        self.saved_us = Some(self.wait_us);
        self
    }

    /// An exchange sent after `wait_us`, with the status of the reply that
    /// matched, None when none did. Busy at the first read means the read
    /// came too early, like a second read. An asleep answer there fills
    /// the window but counts neither way.
    pub fn record(&mut self, wait_us: u16, gets: u8, status: Option<u8>) {
        if wait_us != self.wait_us || gets == 0 {
            return;
        }
        let busy = status == Some(report::STATUS_BUSY);
        let answered = status.is_some_and(|s| s != report::STATUS_TIMEOUT) && !busy;
        let [seen, first, later] = &mut self.counts;
        *seen += 1;
        *first += u8::from(gets == 1 && answered);
        *later += u8::from(busy || (gets > 1 && status.is_some()));
        if *seen < LEARN_WINDOW {
            return;
        }
        if *first >= LEARN_SHORTEN_AT {
            self.wait_us = self.wait_us.saturating_sub(LEARN_STEP_US).max(LEARN_MIN_US);
        } else if *later > LEARN_LENGTHEN_OVER {
            self.wait_us = (self.wait_us + LEARN_STEP_US).min(LEARN_MAX_US);
        }
        self.counts = [0; 3];
    }
}

/// Where the tested receiver has its pairing inventory's `ff ff` marker.
/// The pointer's and keyboard's product ids follow 18 and 20 bytes after
/// it, big-endian.
pub const INVENTORY_MARKER: usize = 286;
/// What `capture` keeps of an inventory, since the rest may tell one unit
/// from another: the first 64 bytes, then its length and the marker's
/// offset (little-endian, 0 for none) and the two ids.
pub const INVENTORY_SHOWN: usize = 64;
pub const INVENTORY_MASKED: usize = INVENTORY_SHOWN + 8;

/// The ids after a marker (two 0xff bytes, no more) at `marker`, if each is
/// empty (0x0000, 0xffff) or a catalog product.
fn ids_at(report: &[u8], marker: usize) -> Option<(u16, u16)> {
    let b = report.get(marker - 1..marker + 22)?;
    let id = |i: usize| u16::from_be_bytes([b[i], b[i + 1]]);
    let fits = |pid: u16| matches!(pid, 0x0000 | 0xffff) || devices::device(pid).is_some();
    let marked = b[1..3] == [0xff, 0xff] && b[0] != 0xff && b[3] != 0xff;
    (marked && fits(id(19)) && fits(id(21))).then(|| (id(19), id(21)))
}

/// The marker's offset and the two product ids in the pairing inventory
/// (feature report 1 of interface 4). The receiver's descriptor declares
/// that report as 22 bytes of LampArray. The marker and the ids are all
/// that count, and a unit without the marker is not recognised. We try the
/// known offset first. Elsewhere the marker must come with a catalog
/// product, as filler is full of 0x00 and 0xff, and fit in exactly one place.
#[must_use]
pub fn inventory(report: &[u8]) -> Option<(usize, u16, u16)> {
    if report.first() != Some(&INVENTORY_REPORT_ID) {
        return None;
    }
    if let Some((pointer, keyboard)) = ids_at(report, INVENTORY_MARKER) {
        return Some((INVENTORY_MARKER, pointer, keyboard));
    }
    let named = |(p, k): &(u16, u16)| devices::device(*p).or(devices::device(*k)).is_some();
    let mut found = (1..report.len()).filter_map(|m| Some((m, ids_at(report, m).filter(named)?)));
    match (found.next(), found.next()) {
        (Some((marker, (pointer, keyboard))), None) => Some((marker, pointer, keyboard)),
        _ => None,
    }
}

/// An inventory read as the snapshot, the lanes and `capture` see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InventoryRead {
    pub state: Inventory,
    /// The pointer's and the keyboard's product ids.
    pub ids: Option<(u16, u16)>,
    pub masked: [u8; INVENTORY_MASKED],
}

/// An inventory read, None if it failed.
#[must_use]
pub fn read_inventory(report: Option<&[u8]>) -> InventoryRead {
    let mut masked = [0_u8; INVENTORY_MASKED];
    let Some(report) = report else {
        let (state, ids) = (Inventory::Absent, None);
        return InventoryRead { state, ids, masked };
    };
    let found = inventory(report);
    let (marker, pointer, keyboard) = found.unwrap_or_default();
    let n = report.len().min(INVENTORY_SHOWN);
    masked[..n].copy_from_slice(&report[..n]);
    let [len, at] = [report.len(), marker].map(|n| u16::try_from(n).unwrap_or(0).to_le_bytes());
    let tail = [len, at, pointer.to_be_bytes(), keyboard.to_be_bytes()];
    masked[INVENTORY_SHOWN..].copy_from_slice(tail.as_flattened());
    let state = match found {
        None => Inventory::NotRecognised,
        Some(_) if marker == INVENTORY_MARKER => Inventory::KnownOffset,
        Some(_) => Inventory::Marker,
    };
    let ids = found.map(|(_, p, k)| (p, k));
    InventoryRead { state, ids, masked }
}

/// The receiver probes that ask for the paired slots, in the order the
/// bridge tries them: 00:bf on 0x08, then on 0x3f.
pub const SLOT_QUERIES: [usize; 2] = [0, 2];

/// A device the receiver keeps a slot for, and whether it is connected
/// through the receiver now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub product_id: u16,
    pub state: messages::Slot,
}

/// A slot's availability byte, 0 or 1.
const AVAILABILITY: [messages::Slot; 2] = [messages::Slot::NotConnected, messages::Slot::Connected];

/// The paired slots in the receiver's answer to 00:bf, per lane: a count
/// from 1 to 4, then per slot an availability byte (1 connected through
/// the receiver now, 0 not) and the big-endian product id, which the slot
/// keeps while the device is off or on its cable. Each id has to be empty
/// (0x0000, 0xffff) or a catalog product, whose kind gives the lane. None
/// unless it all fits and no lane gets two.
#[must_use]
pub fn slots(reply: &Report) -> Option<[Option<Slot>; 2]> {
    let size = usize::from(reply[report::DATA_SIZE]);
    let args = reply.get(report::ARGS..report::ARGS + size)?;
    let (&count, rest) = args.split_first()?;
    let ok = reply[report::STATUS] == report::STATUS_SUCCESS && (1..=4).contains(&count);
    if !ok || rest.len() < 3 * usize::from(count) {
        return None;
    }
    let mut lanes = [None, None];
    for entry in rest.chunks_exact(3).take(usize::from(count)) {
        let state = *AVAILABILITY.get(usize::from(entry[0]))?;
        let product_id = u16::from_be_bytes([entry[1], entry[2]]);
        if matches!(product_id, 0x0000 | 0xffff) {
            continue;
        }
        let lane = devices::device(product_id)?.kind.lane();
        let slot = &mut lanes[crate::state::lane_index(lane)];
        if slot.replace(Slot { product_id, state }).is_some() {
            return None;
        }
    }
    Some(lanes)
}

/// The payload bytes `capture` reports for an event, as docs/protocol.md
/// lays them out. Multi-byte numbers are little-endian.
#[must_use]
pub fn capture_payload(event: &Event) -> Vec<u8> {
    let mut out = vec![0_u8; 8];
    match *event {
        Event::State {
            suspended,
            leaving,
            ready,
        } => {
            let flags = u32::from(suspended) | u32::from(leaving) << 1 | u32::from(ready) << 3;
            out[..4].copy_from_slice(&flags.to_le_bytes());
        }
        Event::Identity {
            product_id,
            arg0,
            arg1,
            ..
        } => {
            out[..2].copy_from_slice(&product_id.to_le_bytes());
            out[2] = arg0;
            out[3] = arg1;
        }
        Event::Status {
            level,
            power,
            charge,
            extra,
            ..
        } => out[..4].copy_from_slice(&[level, power, charge, extra]),
        Event::Raw { bytes, len, .. } => return bytes[..usize::from(len).min(16)].to_vec(),
        Event::Activity { .. } => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_restores_caller_id() {
        let request = report::build(0xff, 0x00, 0x82, &[0; 22]);
        let routed = route(&request, Lane::Keyboard);
        assert_eq!(routed[1], 0x9f);
        assert!(report::request_shape_ok(&routed));
        let mut reply = routed;
        reply[0] = report::STATUS_SUCCESS;
        assert!(matches(&routed, &reply));
        assert_eq!(restore(&reply, request[1])[1], 0xff);
    }

    #[test]
    fn foreign_replies_dont_match() {
        let sent = route(&report::build(0x1f, 0x07, 0x80, &[0, 0]), Lane::Pointer);
        let mut good = sent;
        good[0] = report::STATUS_SUCCESS;
        good[9] = 202;
        good[report::CHECKSUM] = report::checksum(&good);
        assert!(matches(&sent, &good));
        let mut other_lane = good;
        other_lane[1] = 0x9f;
        let mut other_command = good;
        other_command[7] = 0x84;
        other_command[report::CHECKSUM] = report::checksum(&other_command);
        let mut bad_sum = good;
        bad_sum[report::CHECKSUM] ^= 1;
        let mut new = good;
        new[0] = 0;
        let mut packets = good;
        packets[3] = 1;
        packets[report::CHECKSUM] = report::checksum(&packets);
        for reply in [other_lane, other_command, bad_sum, new, packets] {
            assert!(!matches(&sent, &reply));
        }
    }

    #[test]
    fn passive_reports() {
        assert_eq!(
            passive_report(&[0x05, 0x3b, 0x01, 0x00, 0xcd]),
            Some(Event::Identity {
                lane: Lane::Pointer,
                product_id: 0x00cd,
                arg0: 1,
                arg1: 0
            })
        );
        assert_eq!(
            passive_report(&[0x09, 0x35, 0x00, 0x56, 0x02, 0x96]),
            Some(Event::Identity {
                lane: Lane::Keyboard,
                product_id: 0x0296,
                arg0: 0,
                arg1: 0x56
            })
        );
        assert_eq!(
            passive_report(&[0x05, 0x31, 0xca, 0x01, 0x01, 0x00]),
            Some(Event::Status {
                lane: Lane::Pointer,
                level: 202,
                power: 1,
                charge: 1,
                extra: 0
            })
        );
        assert_eq!(
            passive_report(&[0x09, 0x31, 0xa3, 0x00, 0x01, 0x07, 0x99]),
            Some(Event::Status {
                lane: Lane::Keyboard,
                level: 163,
                power: 0,
                charge: 1,
                extra: 7
            })
        );
        // The mouse on its cable: the receiver names product 0.
        let cabled = [0x05, 0x3b, 0x01, 0x00, 0x00];
        for other in [&cabled[..], &[5, 0x31, 16], &[9, 0x77], &[1, 4], &[]] {
            assert_eq!(passive_report(other), None);
        }
        // Capture keeps every vendor report, parsed or not, and nothing else.
        let raw = |b: &[u8]| raw_report(b, u16::try_from(b.len()).unwrap());
        let payload = |b: &[u8]| raw(b).map(|e| capture_payload(&e));
        assert_eq!(payload(&cabled), Some(cabled.to_vec()));
        assert_eq!(payload(&[0x08; 20]), Some(vec![0x08; 16]));
        assert_eq!(payload(&[0x01, 0x00, 0x04]), None);
        let lane = |b: &[u8]| matches!(raw(b), Some(Event::Raw { lane: Some(_), .. }));
        assert!(lane(&cabled) && !lane(&[0x08]));
    }

    struct Script {
        sets: usize,
        replies: Vec<Result<Report, Errno>>,
    }

    impl FeaturePipe for Script {
        fn set(&mut self, _: &Report) -> Result<(), Errno> {
            self.sets += 1;
            Ok(())
        }
        fn get(&mut self, response: &mut Report) -> Result<(), Errno> {
            let next = if self.replies.is_empty() {
                Ok([0; 90])
            } else {
                self.replies.remove(0)
            };
            *response = next?;
            Ok(())
        }
    }

    fn timing(first_get_delay_us: u16, get_interval_us: u16) -> Timing {
        Timing {
            timeout_ms: 1000,
            first_get_delay_us,
            get_interval_us,
            reread_busy: false,
        }
    }

    #[test]
    fn read_loop_skips_foreign_replies() {
        let wire = route(&report::build(0x1f, 0x07, 0x80, &[0, 0]), Lane::Pointer);
        let mut ours = wire;
        ours[0] = report::STATUS_SUCCESS;
        let mut theirs = ours;
        theirs[1] = 0x9f;
        let run = |replies: Vec<Result<Report, Errno>>, timing| {
            let mut pipe = Script { sets: 0, replies };
            (run_exchange(&mut pipe, &wire, timing), pipe.sets)
        };
        let start = Instant::now();
        let replies = vec![Ok(theirs), Err(Errno::PIPE), Ok(ours)];
        assert_eq!(run(replies, timing(1000, 0)), ((Ok(ours), 3, [1, 0]), 1));
        // 1 ms, 1 ms after their reply, 10 ms after the failed read.
        assert!(start.elapsed() >= Duration::from_millis(12));
        // Nothing matches: reads 1 ms apart until 40 ms after the first,
        // or five 10 ms apart when bench asks for that.
        let ((result, gets, rereads), _) = run(vec![], timing(1000, 0));
        assert_eq!(result, Err(Failure::MayHaveBeenSent(Uncertain::NoReply)));
        assert!(gets > 5 && gets <= 41 && rereads == [gets - 1, 0], "{gets}");
        assert_eq!(run(vec![], timing(1000, 10_000)).0.1, 5);
        let transfer = Err(Failure::MayHaveBeenSent(Uncertain::Transfer));
        assert_eq!(run(vec![Err(Errno::PIPE)], timing(1000, 0)).0.0, transfer);
        let gone = run(vec![Err(Errno::NODEV), Ok(ours)], timing(1000, 0));
        assert_eq!(gone.0, (transfer, 1, [0, 0]));
        // Out of time before the first read: sent, so maybe delivered.
        let late = Timing {
            timeout_ms: 5,
            ..timing(0, 0)
        };
        let no_reply = Err(Failure::MayHaveBeenSent(Uncertain::NoReply));
        assert_eq!(run(vec![Ok(ours)], late).0, (no_reply, 0, [0, 0]));
    }

    #[test]
    fn bench_rereads_busy() {
        let wire = route(&report::build(0x1f, 0x00, 0x81, &[0, 0]), Lane::Pointer);
        let (mut ours, mut busy) = (wire, wire);
        (ours[0], busy[0]) = (report::STATUS_SUCCESS, report::STATUS_BUSY);
        let run = |replies: &[Report], timing| {
            let replies = replies.iter().copied().map(Ok).collect();
            run_exchange(&mut Script { sets: 0, replies }, &wire, timing)
        };
        let reread = |interval| Timing {
            reread_busy: true,
            ..timing(1000, interval)
        };
        // Busy ends the exchange, unless bench asks to read on.
        let once = (Ok(busy), 1, [0, 0]);
        assert_eq!(run(&[busy, busy, ours], timing(1000, 1000)), once);
        assert_eq!(
            run(&[busy, busy, ours], reread(1000)),
            (Ok(ours), 3, [0, 2])
        );
        // Still busy when the time is up: that's the answer.
        assert_eq!(run(&[busy], reread(10_000)), (Ok(busy), 5, [3, 1]));
    }

    #[test]
    fn early_busy_lengthens_wait() {
        let window = |status| {
            let mut learned = Learned::new(6_000);
            for i in 0..LEARN_WINDOW {
                let early = if i < 6 {
                    status
                } else {
                    report::STATUS_SUCCESS
                };
                learned.record(6_000, 1, Some(early));
            }
            learned.wait_us
        };
        assert_eq!(window(report::STATUS_BUSY), 6_500);
        assert_eq!(window(report::STATUS_TIMEOUT), 6_000);
    }

    /// A report of `filler` with a marker and ids planted at each offset.
    fn planted(filler: u8, at: &[(usize, [u16; 2])]) -> Vec<u8> {
        let mut report = vec![filler; INVENTORY_LEN];
        report[0] = INVENTORY_REPORT_ID;
        for &(m, [pointer, keyboard]) in at {
            report[m..m + 2].fill(0xff);
            report[m + 18..m + 20].copy_from_slice(&pointer.to_be_bytes());
            report[m + 20..m + 22].copy_from_slice(&keyboard.to_be_bytes());
        }
        report
    }

    #[test]
    fn inventory_by_structure() {
        // Filler around the marker doesn't matter. The marker and the ids do.
        for filler in [0x00, 0x5a, 0xc3] {
            let report = planted(filler, &[(286, [0x00cd, 0x0296])]);
            assert_eq!(inventory(&report), Some((286, 0x00cd, 0x0296)));
            assert_eq!(read_inventory(Some(&report)).state, Inventory::KnownOffset);
        }
        // Elsewhere it needs a product and must sit in exactly one place.
        let moved = planted(0x5a, &[(100, [0xffff, 0x0296])]);
        let read = read_inventory(Some(&moved));
        assert_eq!(
            (read.state, read.ids),
            (Inventory::Marker, Some((0xffff, 0x0296)))
        );
        assert_eq!(read.masked[64..], [0, 2, 100, 0, 0xff, 0xff, 2, 0x96]);
        for refused in [
            planted(0x5a, &[(100, [0xffff, 0x0296]), (400, [0x00cd, 0])]),
            planted(0x5a, &[(100, [0xffff, 0])]),
            planted(0, &[(286, [0x1234, 0x0296])]),
            planted(0xff, &[]),
        ] {
            assert_eq!(inventory(&refused), None);
        }
        assert_eq!(
            read_inventory(Some(&[1; 22])).state,
            Inventory::NotRecognised
        );
        assert_eq!(read_inventory(None).state, Inventory::Absent);
    }

    #[test]
    fn paired_slots() {
        let answer = |tx: u8, data: &[u8]| {
            let mut reply = report::build(tx, 0x00, 0xbf, &[0; 49]);
            reply[report::STATUS] = report::STATUS_SUCCESS;
            reply[report::ARGS..report::ARGS + data.len()].copy_from_slice(data);
            reply
        };
        let slot = |product_id, on: bool| {
            let state = AVAILABILITY[usize::from(on)];
            Some(Slot { product_id, state })
        };
        // As the tested receiver answered: both on, the keyboard off,
        // the mouse on its cable.
        for (data, mouse, keyboard) in [
            ([2, 1, 0x00, 0xcd, 1, 0x02, 0x96], true, true),
            ([2, 1, 0x00, 0xcd, 0, 0x02, 0x96], true, false),
            ([2, 0, 0x00, 0xcd, 1, 0x02, 0x96], false, true),
        ] {
            let lanes = [slot(0x00cd, mouse), slot(0x0296, keyboard)];
            assert_eq!(slots(&answer(0x08, &data)), Some(lanes));
        }
        // The kind picks the lane, and an empty slot names nothing.
        let swapped = answer(0x3f, &[3, 1, 0x02, 0x90, 0, 0xff, 0xff, 1, 0x00, 0xab]);
        let lanes = [slot(0x00ab, true), slot(0x0290, true)];
        assert_eq!(slots(&swapped), Some(lanes));
        for bad in [
            &[0][..],
            &[5, 1, 0x00, 0xcd],
            &[2, 2, 0x00, 0xcd, 1, 0x02, 0x96],
            &[1, 1, 0x12, 0x34],
            &[2, 1, 0x00, 0xcd, 1, 0x00, 0xab],
        ] {
            assert_eq!(slots(&answer(0x08, bad)), None, "{bad:02x?}");
        }
        let mut short = answer(0x08, &[2, 1, 0x00, 0xcd, 1, 0x02, 0x96]);
        short[report::DATA_SIZE] = 4;
        let mut busy = answer(0x08, &[1, 1, 0x00, 0xcd]);
        busy[report::STATUS] = report::STATUS_BUSY;
        assert_eq!((slots(&short), slots(&busy)), (None, None));
    }

    #[test]
    fn activity_lanes_and_rates() {
        assert!(keyboard_input(0x01) && !keyboard_input(0x0a) && !keyboard_input(0x05));
        let mut rate = Rate::default();
        assert!(rate.allow(0, 250, 1));
        assert!(!rate.allow(249, 250, 1));
        assert!(rate.allow(250, 250, 1));
    }

    #[test]
    fn capture_payload_layout() {
        let status = Event::Status {
            lane: Lane::Pointer,
            level: 0xca,
            power: 1,
            charge: 1,
            extra: 0,
        };
        let identity = Event::Identity {
            lane: Lane::Keyboard,
            product_id: 0x0296,
            arg0: 0,
            arg1: 0x56,
        };
        // What opening a receiver records: ready to send, nothing else.
        let state = Event::State {
            suspended: false,
            leaving: false,
            ready: true,
        };
        let hex = |event: &Event| hfx_proto::ids::lower_hex(&capture_payload(event));
        assert_eq!(hex(&status), "ca01010000000000");
        assert_eq!(hex(&identity), "9602005600000000");
        assert_eq!(hex(&state), "0800000000000000");
    }
}
