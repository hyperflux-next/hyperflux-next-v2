// SPDX-License-Identifier: GPL-2.0-only

//! Virtual OpenRazer devices: one feature-only UHID device per qualified,
//! available route of a device OpenRazer knows. The patched razermouse and
//! razerkbd bind it and send their usual SET and GET feature reports. Each
//! SET becomes one exchange (or a few, for a busy query) and the GET gets
//! the device's answer or an HFXNERR1 envelope. The device keeper holds
//! the UHID device and passes the kernel's requests and these answers on.
//!
//! Before a device is created, two serial reads 250 ms apart must agree, so
//! OpenRazer never sees a device that is answering for someone else. While
//! a wired device of the model is bound, OpenRazer has that one, and this
//! one waits. Failures back off per route; nothing is blocked for good.

use crate::decode;
use crate::uhid;
use hfx_proto::devices::{self, CommandRefusal, Device};
use hfx_proto::messages::OpenRazerState;
use hfx_proto::openrazer::{Uncertainty, envelope};
use hfx_proto::report::{self, REPORT_LEN, Report};
use sha2::{Digest, Sha256};

pub const SERIAL_GAP_MS: u64 = 250;
pub const START_TIMEOUT_MS: u64 = 10_000;
pub const BLOCKED_RETRY_MS: u64 = 300_000;
/// After a device answered 0x04, re-probe at least this often...
pub const DEPARTURE_CAP_MS: u64 = 2_000;
/// ...for this long.
pub const DEPARTURE_WINDOW_MS: u64 = 60_000;
pub const MAX_RECOVERIES: u8 = 3;
pub const QUERY_ATTEMPTS: u8 = 5;
pub const TEARDOWN_DEADLINE_MS: u64 = 5_000;
pub const QUIET_BEFORE_DESTROY_MS: u64 = 200;
/// How long a 0x04 answer waits for OpenRazer's GET before recycling anyway.
pub const RECYCLE_WAIT_MS: u64 = 2_000;
/// Only the tested devices have shown that 0x04 means asleep. Another
/// device is recycled once the bridge's own probe gets 0x04 too, or once
/// the lane has shown no life for this long.
pub const DOUBT_MS: u64 = 10_000;

/// Delay after the n-th consecutive failure.
#[must_use]
pub fn backoff_ms(failures: u32) -> u64 {
    match failures {
        0..=5 => 100,
        6 => 1_000,
        7 => 2_000,
        8 => 4_000,
        9 => 8_000,
        10 => 16_000,
        _ => 30_000,
    }
}

/// `native-` and 16 hex digits, unique per receiver generation and device.
#[must_use]
pub fn phys_label(receiver_id: &str, generation: u64, device_id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"hyperflux-next uhid\0");
    hash.update(receiver_id.as_bytes());
    hash.update(generation.to_le_bytes());
    hash.update(device_id.as_bytes());
    let digest = hash.finalize();
    format!("native-{}", hfx_proto::ids::lower_hex(&digest[..8]))
}

pub enum Phase {
    /// Next serial read at `at`, with the first read's result in `first`.
    Idle {
        at: u64,
        first: Option<[u8; 32]>,
    },
    Reading {
        first: Option<[u8; 32]>,
    },
    /// A wired device of the model is bound: this one on its cable.
    Cable,
    Live(Box<Live>),
    Blocked {
        until: u64,
    },
}

pub struct VirtualDevice {
    /// For log lines: `receiver-…/pointer-00cd`.
    pub who: String,
    pub entry: &'static Device,
    pub label: String,
    pub phase: Phase,
    pub failures: u32,
    pub fast_until: Option<u64>,
    pub recoveries: u8,
    /// The last failure was the device not answering. The backoff ends
    /// as soon as the lane shows life.
    pub waiting_for_device: bool,
    /// The lane showed life while a serial read was out; its answer may
    /// predate that.
    pub life_during_read: bool,
    /// Stands in when the device gives no serial: made from the receiver,
    /// lane and product. It never matches another's.
    pub no_serial: [u8; 32],
    /// The fingerprint both serial reads agreed on last.
    pub print: Option<[u8; 32]>,
    /// The last failure logged, and its delay.
    pub told: Option<(String, u64)>,
    /// The wired device of the model is another unit, by its serial.
    pub beside: bool,
}

impl VirtualDevice {
    #[must_use]
    pub fn new(who: String, entry: &'static Device, label: String, now: u64) -> Self {
        let no_serial = Sha256::new()
            .chain_update(b"hyperflux-next no serial\0")
            .chain_update(who.as_bytes())
            .finalize()
            .into();
        Self {
            who,
            entry,
            label,
            phase: Phase::Idle {
                at: now,
                first: None,
            },
            failures: 0,
            fast_until: None,
            recoveries: 0,
            waiting_for_device: false,
            life_during_read: false,
            no_serial,
            print: None,
            told: None,
            beside: false,
        }
    }

    #[must_use]
    pub fn phys(&self) -> String {
        format!("{}{}", hfx_proto::openrazer::PHYS_PREFIX, self.label)
    }

    #[must_use]
    pub fn driver(&self) -> &'static str {
        self.entry.openrazer.as_ref().map_or("", |o| o.driver)
    }

    #[must_use]
    pub fn serial_request(&self) -> Report {
        decode::serial_request(
            self.entry
                .openrazer
                .as_ref()
                .map_or(0x1f, |o| o.serial_transaction_id),
        )
    }

    /// Remembers a failure. True when it is news: another reason or delay.
    fn tell(&mut self, why: &str, delay: u64) -> bool {
        let old = self.told.replace((why.to_owned(), delay));
        old.is_none_or(|(w, d)| w != why || d != delay)
    }

    /// A retryable failure: try again after the backoff. Logged when the
    /// reason or the delay changes, so a device that keeps failing the same
    /// way between signs of life doesn't fill the log. True when logged.
    pub fn fail(&mut self, now: u64, why: &str) -> bool {
        self.waiting_for_device = false;
        self.failures = self.failures.saturating_add(1);
        let mut delay = backoff_ms(self.failures);
        if self.fast_until.is_some_and(|until| now < until) {
            delay = delay.min(DEPARTURE_CAP_MS);
        }
        let news = self.tell(why, delay);
        if news {
            eprintln!("openrazer {}: {why}; retrying in {delay} ms", self.who);
        }
        self.phase = Phase::Idle {
            at: now + delay,
            first: None,
        };
        news
    }

    /// Something that won't fix itself quickly.
    pub fn block(&mut self, now: u64, why: &str) {
        if self.tell(why, BLOCKED_RETRY_MS) {
            eprintln!("openrazer {}: {why}; retrying in 5 minutes", self.who);
        }
        self.phase = Phase::Blocked {
            until: now + BLOCKED_RETRY_MS,
        };
    }

    /// A wired device of the model, with the serials that could be read,
    /// or None. It counts as this unit on its cable unless both serials are
    /// known and differ. Then the receiver copy stays. Returns the copy to tear
    /// down.
    pub fn cable(&mut self, now: u64, wired: Option<&[[u8; 32]]>) -> Option<Box<Live>> {
        let (who, ours) = (&self.who, self.print.filter(|p| *p != self.no_serial));
        let other = wired
            .zip(ours)
            .is_some_and(|(w, p)| !w.is_empty() && !w.contains(&p));
        if other && !self.beside {
            eprintln!("openrazer {who}: another unit is on the cable; keeping it");
        }
        self.beside = other;
        let cabled = matches!(self.phase, Phase::Cable);
        if wired.is_some() && !other && !cabled {
            let live = self.take_live(Phase::Cable);
            let what = if live.is_some() {
                "removing"
            } else {
                "not creating"
            };
            eprintln!("openrazer {}: plugged in by cable; {what} it", self.who);
            (self.failures, self.told) = (0, None);
            return live;
        }
        if cabled && (wired.is_none() || other) {
            if !other {
                eprintln!("openrazer {who}: the cable is gone; probing again");
            }
            self.phase = Phase::Idle {
                at: now,
                first: None,
            };
        }
        None
    }

    /// The state the snapshot shows, and why.
    #[must_use]
    pub fn state(&self) -> (OpenRazerState, Option<&str>) {
        let why = self.told.as_ref().map(|(why, _)| why.as_str());
        match &self.phase {
            Phase::Live(_) => (OpenRazerState::Live, None),
            Phase::Cable => (OpenRazerState::Cable, None),
            Phase::Blocked { .. } => (OpenRazerState::Blocked, why),
            _ if why.is_none() => (OpenRazerState::Starting, None),
            _ => (OpenRazerState::Retrying, why),
        }
    }

    /// The device answered 0x04: recreate it once it answers again, and
    /// look often for a minute.
    pub fn departed(&mut self, now: u64) {
        self.fast_until = Some(now + DEPARTURE_WINDOW_MS);
        self.fail(now, "device stopped answering (0x04)");
        self.waiting_for_device = true;
    }

    /// Evidence after a long silence: don't sit out the backoff.
    pub fn returned(&mut self, now: u64) {
        if let Phase::Idle { at, .. } = &mut self.phase {
            *at = now;
            self.failures = 0;
        }
    }

    /// Any evidence from the device. A route that backs off because the
    /// device didn't answer tries again now. One between its two serial
    /// reads keeps the spacing, and other failures keep their backoff.
    pub fn life(&mut self, now: u64) {
        match &mut self.phase {
            Phase::Idle { at, first: None } if self.waiting_for_device => {
                *at = (*at).min(now);
                self.failures = 0;
                self.waiting_for_device = false;
            }
            Phase::Reading { .. } => self.life_during_read = true,
            _ => {}
        }
    }

    pub fn published(&mut self, print: [u8; 32]) {
        (self.failures, self.fast_until, self.told) = (0, None, None);
        self.print = Some(print);
    }

    /// The serial read finished. True when both reads agree.
    pub fn serial_read(
        &mut self,
        now: u64,
        result: Result<[u8; 32], decode::SerialError>,
    ) -> Option<[u8; 32]> {
        let Phase::Reading { first } = self.phase else {
            return None;
        };
        let lively = std::mem::take(&mut self.life_during_read);
        // Two well-formed answers without a usable serial agree on that.
        let result = match result {
            Err(decode::SerialError::Unusable) => Ok(self.no_serial),
            other => other,
        };
        match (result, first) {
            (Ok(print), None) => {
                self.phase = Phase::Idle {
                    at: now + SERIAL_GAP_MS,
                    first: Some(print),
                };
                None
            }
            (Ok(print), Some(earlier)) if print == earlier => Some(print),
            (Ok(_), Some(_)) => {
                self.fail(now, "serial numbers of two reads differ");
                None
            }
            (Err(decode::SerialError::NoAnswer), _) => {
                if lively {
                    self.failures = 0;
                }
                self.fail(now, "serial read not answered");
                self.waiting_for_device = true;
                None
            }
            (Err(decode::SerialError::Retry), _) => {
                self.fail(now, "serial read failed");
                None
            }
            (Err(decode::SerialError::Invalid | decode::SerialError::Unusable), _) => {
                self.block(now, "serial read returned no usable answer");
                None
            }
        }
    }

    pub fn take_live(&mut self, next: Phase) -> Option<Box<Live>> {
        match std::mem::replace(&mut self.phase, next) {
            Phase::Live(live) => Some(live),
            _ => None,
        }
    }

    /// When the loop should look at this device again.
    #[must_use]
    pub fn due(&self) -> Option<u64> {
        match &self.phase {
            Phase::Idle { at, .. } => Some(*at),
            Phase::Blocked { until } => Some(*until),
            Phase::Live(live) if !live.started => Some(live.created + START_TIMEOUT_MS),
            Phase::Live(live) => live.recycle_at.or(live.doubt.map(|t| t + DOUBT_MS)),
            Phase::Reading { .. } | Phase::Cable => None,
        }
    }
}

pub struct SetState {
    pub request: Report,
    pub attempts: u8,
}

/// A UHID device the keeper made.
pub struct Live {
    /// The device's id with the keeper.
    pub id: u64,
    pub created: u64,
    pub started: bool,
    pub stopped: bool,
    pub bad_start: bool,
    /// The keeper says the device is gone, and why.
    pub ended: Option<String>,
    pub set: Option<SetState>,
    pub response: Option<Report>,
    pub get: Option<u32>,
    pub recycle_at: Option<u64>,
    /// When a device outside the tested tier answered 0x04, until that is
    /// confirmed or it shows life.
    pub doubt: Option<u64>,
    /// Last GET activity.
    pub last_get: u64,
    pub mode: [u8; 2],
    /// The last SET refused for not being in the device's list.
    pub refused: Option<Report>,
    /// Answers for the kernel, which the bridge hands to the keeper: the
    /// request's id, an errno, and a GET's report.
    pub replies: Vec<(u32, u16, Option<Report>)>,
}

pub enum SetResult {
    Matched(Report),
    NotSent,
    MayHaveBeenSent,
}

/// OpenRazer reads and writes the device mode around every effect change.
/// The receiver's children don't need it, and driver mode would hand the
/// keyboard's keys to a daemon that can't see them. It stays local.
fn device_mode(mode: &mut [u8; 2], request: &Report) -> Option<Report> {
    if request[report::CLASS] != 0x00 || request[report::DATA_SIZE] != 2 {
        return None;
    }
    match request[report::COMMAND] {
        0x04 => mode.copy_from_slice(&request[report::ARGS..report::ARGS + 2]),
        0x84 => {}
        _ => return None,
    }
    let mut response = *request;
    response[report::STATUS] = report::STATUS_SUCCESS;
    response[report::ARGS..report::ARGS + 2].copy_from_slice(mode);
    response[report::CHECKSUM] = report::checksum(&response);
    Some(response)
}

impl Live {
    #[must_use]
    pub fn new(id: u64, now: u64) -> Self {
        Self {
            id,
            created: now,
            started: false,
            stopped: false,
            bad_start: false,
            ended: None,
            set: None,
            response: None,
            get: None,
            recycle_at: None,
            doubt: None,
            last_get: now,
            mode: [0, 0],
            refused: None,
            replies: Vec::new(),
        }
    }

    pub fn start(&mut self, flags: u64) {
        if flags & uhid::NUMBERED_FEATURE_REPORTS != 0 {
            self.bad_start = true;
        } else {
            self.started = true;
        }
    }

    pub fn stop(&mut self) {
        (self.stopped, self.started) = (true, false);
    }

    pub fn get(&mut self, id: u32, now: u64) {
        self.last_get = now;
        if !self.started {
            self.replies.push((id, uhid::EIO, None));
        } else if let Some(response) = self.response.take() {
            self.replies.push((id, 0, Some(response)));
            self.answered(now);
        } else if self.set.is_some() {
            if let Some(older) = self.get.replace(id) {
                self.replies.push((older, uhid::EIO, None));
            }
        } else {
            self.replies.push((id, uhid::EIO, None));
        }
    }

    /// A SET, admitted against `entry`'s command list when there is one.
    /// Returns the report to exchange.
    pub fn set(&mut self, id: u32, request: Report, entry: Option<&Device>) -> Option<Report> {
        let check = entry.map(|entry| devices::check_command(entry, &request));
        if check == Some(Err(CommandRefusal::NotListed)) {
            self.refused = Some(request);
        }
        let admitted = check.is_some_and(|c| c.is_ok())
            && self.started
            && self.set.is_none()
            && self.recycle_at.is_none();
        if !admitted {
            self.replies.push((id, uhid::EIO, None));
            return None;
        }
        // Acknowledge first: OpenRazer's GET then waits in the kernel while
        // the exchange runs.
        self.replies.push((id, 0, None));
        self.response = device_mode(&mut self.mode, &request);
        if self.response.is_some() {
            return None;
        }
        self.set = Some(SetState {
            request,
            attempts: 1,
        });
        Some(request)
    }

    fn answered(&mut self, now: u64) {
        if self.recycle_at.is_some() {
            self.recycle_at = Some(now);
        }
    }

    /// Takes an exchange result for the pending SET and returns the request
    /// to send again when a query came back busy or for someone else. A 0x04
    /// answer recycles the device, or with `doubt` waits for confirmation.
    pub fn exchanged(&mut self, result: SetResult, now: u64, doubt: bool) -> Option<Report> {
        let set = self.set.as_mut()?;
        let request = set.request;
        let response = match result {
            SetResult::Matched(response) => {
                let asleep = response[report::STATUS] == report::STATUS_TIMEOUT;
                if asleep && doubt {
                    self.doubt = self.doubt.or(Some(now));
                } else if asleep {
                    self.recycle_at = Some(now + RECYCLE_WAIT_MS);
                }
                if !report::is_query(&request) || decode::query_answered(&request, &response) {
                    response
                } else if decode::query_retryable(&request, &response)
                    && set.attempts < QUERY_ATTEMPTS
                {
                    set.attempts += 1;
                    return Some(request);
                } else {
                    envelope(Uncertainty::MayHaveBeenSent)
                }
            }
            SetResult::NotSent => envelope(Uncertainty::NotSent),
            SetResult::MayHaveBeenSent => envelope(Uncertainty::MayHaveBeenSent),
        };
        self.set = None;
        match self.get.take() {
            Some(id) => {
                self.replies.push((id, 0, Some(response)));
                self.answered(now);
            }
            None => self.response = Some(response),
        }
        None
    }

    /// Life on the lane after a doubted 0x04 ends the doubt, and `DOUBT_MS`
    /// without any recycles the device. Returns the open doubt.
    pub fn settle_doubt(&mut self, last_life: u64, now: u64) -> Option<u64> {
        let since = self.doubt?;
        if last_life > since {
            self.doubt = None;
        } else if now >= since + DOUBT_MS {
            (self.doubt, self.recycle_at) = (None, Some(now));
        }
        self.doubt
    }
}

const _: () = assert!(REPORT_LEN == 90);

/// A device on its way out: still answering the kernel's requests while
/// the keeper unbinds the driver, until the keeper says it is gone.
pub struct Teardown {
    pub live: Box<Live>,
    pub started: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_schedule() {
        let delays: Vec<u64> = (1..=12).map(backoff_ms).collect();
        assert_eq!(
            delays,
            [
                100, 100, 100, 100, 100, 1000, 2000, 4000, 8000, 16000, 30000, 30000
            ]
        );
    }

    fn device() -> VirtualDevice {
        let entry = devices::device(0x00cd).unwrap();
        VirtualDevice::new(
            "test".into(),
            entry,
            phys_label("receiver-1", 1, "pointer-00cd"),
            0,
        )
    }

    #[test]
    fn labels_fit_the_patch() {
        let label = phys_label("receiver-6b1f0e9c2a7d4e83b5c1f2a0", 3, "pointer-00cd");
        assert!(hfx_proto::openrazer::label_ok(&label));
        assert_eq!(label.len(), 23);
        assert_ne!(
            label,
            phys_label("receiver-6b1f0e9c2a7d4e83b5c1f2a0", 4, "pointer-00cd")
        );
    }

    #[test]
    fn departure_retry_cap() {
        let mut v = device();
        let mut now = 1_000;
        v.departed(now);
        let mut delays = Vec::new();
        for _ in 0..10 {
            let Phase::Idle { at, .. } = v.phase else {
                panic!()
            };
            delays.push(at - now);
            now = at;
            v.phase = Phase::Reading { first: None };
            v.fail(now, "busy");
        }
        assert_eq!(
            delays,
            [100, 100, 100, 100, 100, 1000, 2000, 2000, 2000, 2000]
        );
        // Past the window the normal schedule resumes.
        now = 1_000 + DEPARTURE_WINDOW_MS;
        v.phase = Phase::Reading { first: None };
        v.fail(now, "busy");
        let Phase::Idle { at, .. } = v.phase else {
            panic!()
        };
        assert_eq!(at - now, 30_000);
        v.published([1; 32]);
        assert_eq!((v.failures, v.fast_until), (0, None));
    }

    #[test]
    fn same_failure_logged_once() {
        let mut v = device();
        let mut logged = Vec::new();
        for now in [0, 2_000, 4_000] {
            // The lane showed life during each read, which resets the count.
            v.failures = 0;
            logged.push(v.fail(now, "serial read not answered"));
        }
        logged.push(v.fail(5_000, "serial read failed"));
        v.failures = 5;
        logged.push(v.fail(5_100, "serial read failed"));
        assert_eq!(logged, [true, false, false, true, true]);
        let state = (OpenRazerState::Retrying, Some("serial read failed"));
        assert_eq!(v.state(), state);
    }

    #[test]
    fn cable_hides_the_receiver_copy() {
        let mut v = device();
        let (ours, other) = ([1; 32], [2; 32]);
        // Not live yet: no serial read while the cable is in.
        assert!(v.cable(0, Some(&[])).is_none());
        assert_eq!(v.state(), (OpenRazerState::Cable, None));
        v.cable(10, None);
        assert!(matches!(v.phase, Phase::Idle { at: 10, .. }));
        // Live, the copy goes unless both serials are known and differ.
        v.published(ours);
        for (wired, kept) in [(&[][..], false), (&[ours], false), (&[other], true)] {
            v.phase = Phase::Live(Box::new(live()));
            let gone = v.cable(20, Some(wired));
            assert_eq!((gone.is_none(), v.beside), (kept, kept), "{wired:?}");
        }
        // A device without a serial can't be told from another unit.
        v.phase = Phase::Live(Box::new(live()));
        v.print = Some(v.no_serial);
        assert!(v.cable(30, Some(&[other])).is_some());
        // Once the other unit's serial can be read, the copy comes back.
        v.print = Some(ours);
        assert!(v.cable(40, Some(&[other])).is_none());
        assert!(matches!(v.phase, Phase::Idle { at: 40, .. }));
    }

    #[test]
    fn life_cuts_backoff() {
        let mut v = device();
        for _ in 0..8 {
            v.phase = Phase::Reading { first: None };
            v.serial_read(1_000, Err(decode::SerialError::NoAnswer));
        }
        assert!(matches!(v.phase, Phase::Idle { at: 5_000, .. }));
        v.life(1_200);
        assert!(matches!(
            v.phase,
            Phase::Idle {
                at: 1_200,
                first: None
            }
        ));
        assert_eq!(v.failures, 0);
        // The first read agreed: the second still waits its 250 ms.
        v.phase = Phase::Reading { first: None };
        v.serial_read(1_300, Ok([1; 32]));
        v.life(1_310);
        assert!(matches!(
            v.phase,
            Phase::Idle {
                at: 1_550,
                first: Some(_)
            }
        ));
        // A busy device answered. Its own reply is the life: no reset.
        v.phase = Phase::Reading { first: None };
        v.serial_read(2_000, Err(decode::SerialError::Retry));
        v.life(2_010);
        assert!(matches!(v.phase, Phase::Idle { at: 2_100, .. }));
        // Life while a read is out: its 0x04 doesn't start a long wait.
        v.failures = 9;
        v.phase = Phase::Reading { first: None };
        v.life(3_000);
        v.serial_read(3_010, Err(decode::SerialError::NoAnswer));
        assert!(matches!(v.phase, Phase::Idle { at: 3_110, .. }));
    }

    #[test]
    fn two_reads_must_agree() {
        let mut v = device();
        v.phase = Phase::Reading { first: None };
        assert_eq!(v.serial_read(10, Ok([1; 32])), None);
        assert!(matches!(
            v.phase,
            Phase::Idle {
                at: 260,
                first: Some(_)
            }
        ));
        v.phase = Phase::Reading {
            first: Some([1; 32]),
        };
        assert_eq!(v.serial_read(300, Ok([1; 32])), Some([1; 32]));
        v.phase = Phase::Reading {
            first: Some([1; 32]),
        };
        assert_eq!(v.serial_read(300, Ok([2; 32])), None);
        assert!(matches!(v.phase, Phase::Idle { first: None, .. }));
        // Two answers without a usable serial agree on the stand-in.
        v.phase = Phase::Reading { first: None };
        v.serial_read(400, Err(decode::SerialError::Unusable));
        let first = Some(v.no_serial);
        assert!(matches!(v.phase, Phase::Idle { first: f, .. } if f == first));
        v.phase = Phase::Reading { first };
        let agreed = v.serial_read(700, Err(decode::SerialError::Unusable));
        assert_eq!(agreed, first);
        v.phase = Phase::Reading { first: None };
        v.serial_read(800, Err(decode::SerialError::Invalid));
        assert!(matches!(v.phase, Phase::Blocked { until } if until == 800 + BLOCKED_RETRY_MS));
    }

    fn live() -> Live {
        Live::new(1, 0)
    }

    /// A SET sent once and waiting for its answer.
    fn pending(request: Report) -> Option<SetState> {
        Some(SetState {
            request,
            attempts: 1,
        })
    }

    #[test]
    fn device_mode_stays_local() {
        let entry = devices::device(0x0296).unwrap();
        let mut live = live();
        live.start(0);
        let set = report::build(0x9f, 0x00, 0x04, &[0x03, 0x00]);
        assert_eq!(live.set(1, set, Some(entry)), None);
        live.get(2, 0);
        let [(1, 0, None), (2, 0, Some(data))] = live.replies[..] else {
            panic!("{:?}", live.replies)
        };
        assert_eq!(data[0], 2);
        live.replies.clear();
        let get = report::build(0x9f, 0x00, 0x84, &[0, 0]);
        assert_eq!(live.set(3, get, Some(entry)), None);
        live.get(4, 0);
        let Some((4, 0, Some(data))) = live.replies.pop() else {
            panic!()
        };
        assert_eq!(&data[8..10], &[0x03, 0x00]);
        assert_eq!(data[88], report::checksum(&data));
        // Mode 1 is refused and never reaches the store.
        let bad = report::build(0x9f, 0x00, 0x04, &[0x01, 0x00]);
        live.set(5, bad, Some(entry));
        assert_eq!(live.replies.pop(), Some((5, uhid::EIO, None)));
        assert_eq!(live.mode, [3, 0]);
        // Nor does anything while the device is on its way out.
        assert_eq!(live.set(6, get, None), None);
        assert_eq!(live.replies.pop(), Some((6, uhid::EIO, None)));
    }

    #[test]
    fn busy_query_retries() {
        let mut live = live();
        let request = report::build(0x1f, 0x07, 0x80, &[0, 0]);
        live.started = true;
        live.set = pending(request);
        let mut busy = request;
        busy[0] = report::STATUS_BUSY;
        for _ in 1..QUERY_ATTEMPTS {
            assert_eq!(
                live.exchanged(SetResult::Matched(busy), 0, false),
                Some(request)
            );
        }
        assert_eq!(live.exchanged(SetResult::Matched(busy), 0, false), None);
        assert_eq!(live.response, Some(envelope(Uncertainty::MayHaveBeenSent)));

        // A write passes through byte for byte, whatever its status.
        let write = report::build(0x1f, 0x0f, 0x04, &[1, 0, 200]);
        live.set = pending(write);
        live.get = Some(9);
        let mut failed = write;
        failed[0] = report::STATUS_FAILURE;
        assert_eq!(live.exchanged(SetResult::Matched(failed), 0, false), None);
        assert_eq!(live.replies, [(9, 0, Some(failed))]);
        live.set = pending(request);
        live.exchanged(SetResult::NotSent, 0, false);
        assert_eq!(live.response, Some(envelope(Uncertainty::NotSent)));
    }

    #[test]
    fn timeout_reply_arms_recycle() {
        let mut live = live();
        let request = report::build(0x1f, 0x07, 0x80, &[0, 0]);
        live.started = true;
        live.set = pending(request);
        let mut asleep = request;
        asleep[0] = report::STATUS_TIMEOUT;
        // Outside the tested tier, 0x04 starts a doubt instead of a recycle.
        live.exchanged(SetResult::Matched(asleep), 50, true);
        assert_eq!((live.recycle_at, live.doubt), (None, Some(50)));
        assert_eq!(live.settle_doubt(40, 5_000), Some(50));
        assert_eq!(live.settle_doubt(60, 5_000), None);
        live.doubt = Some(50);
        assert_eq!(live.settle_doubt(40, 50 + DOUBT_MS), None);
        assert_eq!(live.recycle_at, Some(50 + DOUBT_MS));
        live.recycle_at = None;
        live.set = pending(request);
        live.exchanged(SetResult::Matched(asleep), 100, false);
        assert_eq!(live.recycle_at, Some(100 + RECYCLE_WAIT_MS));
        live.get(1, 150);
        let envelope = envelope(Uncertainty::MayHaveBeenSent);
        assert_eq!(live.replies, [(1, 0, Some(envelope))]);
        assert_eq!(live.recycle_at, Some(150));
    }
}
