// SPDX-License-Identifier: GPL-2.0-only

//! Receivers, their devices, presence and battery, and how they look in a
//! snapshot and a route list.

use crate::charge;
use crate::decode;
use crate::exchange::Worker;
use crate::transport::{Event, Record as EventRecord, Source};
use crate::vdev::VirtualDevice;
use hfx_proto::devices::{self, ChargeDecoder, Tier};
use hfx_proto::ids::{
    BatteryPercent, ClientName, DeviceId, DeviceName, GenerationId, HumanMessage, MonotonicMs,
    ReceiverId, RouteToken,
};
use hfx_proto::messages::{
    Battery, Charge, Contact, DeviceInfo, DeviceKind, FirstGet, Inventory, Lane, OpenRazerDevice,
    OpenRazerState, Presence, ROUTE_REPORT_BYTES, ReceiverInfo, ReceiverState, Refused, Route,
    RouteAccess, RouteState, Slot, Support, Unreported,
};
use hfx_proto::present::BATTERY_STALE_AFTER_MS;
use std::collections::BTreeMap;

/// Evidence after this much silence on a lane means the device came back.
pub const RETURN_AFTER_MS: u64 = 60_000;

pub const LANES: [Lane; 2] = [Lane::Pointer, Lane::Keyboard];

#[must_use]
pub const fn lane_index(lane: Lane) -> usize {
    match lane {
        Lane::Pointer => 0,
        Lane::Keyboard => 1,
    }
}

#[must_use]
pub const fn lane_name(lane: Lane) -> &'static str {
    match lane {
        Lane::Pointer => "pointer",
        Lane::Keyboard => "keyboard",
    }
}

/// A device's name on the desktop and in the log. Product id 0 is the
/// stand-in for a device the receiver hasn't named.
#[must_use]
pub fn device_name(pid: u16, lane: Lane) -> String {
    match devices::device(pid) {
        Some(e) => e.name.to_owned(),
        None if pid == 0 => format!("Unidentified {} device", lane_name(lane)),
        None => format!("Unknown device 1532:{pid:04x}"),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BatteryState {
    pub level: Option<u8>,
    pub charge: Option<Charge>,
    pub updated: Option<u64>,
    /// When a reply last said whether it charges.
    pub charge_at: Option<u64>,
    /// The level went up at its last change.
    pub rising: bool,
}

/// What the desktop should show for a lane's battery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub capacity: u8,
    pub status: decode::BatteryStatus,
    /// The device's full name, as OpenRazer names it.
    pub name: String,
    pub product_id: u16,
}

/// Publication of one lane's battery.
#[derive(Debug, Default)]
pub struct Publish {
    /// What the desktop has now. None is "no power supply".
    pub published: Option<Record>,
    pub failures: u8,
    pub retry_at: Option<u64>,
    /// The keeper's battery device for it: the bridge's id for the device,
    /// the product it was made for, and the report it was sent last.
    pub device: Option<(u64, u16, Option<[u8; 3]>)>,
}

/// Retry delays for a refused publication. After the last one the bridge
/// waits for the next reading.
pub const PUBLISH_RETRY_MS: [u64; 3] = [1_000, 5_000, 30_000];

impl Publish {
    /// A new reading retries even a publication that ran out of retries.
    pub fn reading(&mut self) {
        self.failures = 0;
        self.retry_at = None;
    }

    #[must_use]
    pub fn due(&self, now: u64) -> bool {
        match self.retry_at {
            Some(at) => at <= now,
            None => self.failures == 0,
        }
    }
}

#[derive(Debug, Default)]
pub struct Probe {
    pub due: Option<u64>,
    pub retries: usize,
    /// The device answered 0x04. Ask again once it shows signs of life.
    pub after_evidence: bool,
    pub running: bool,
    pub started: Option<u64>,
    /// The last time a probe was due and tried, started or not.
    pub tried: Option<u64>,
    /// The receiver said this keyboard is gone; the probe decides.
    pub confirm_gone: bool,
}

pub const PROBE_RETRY_MS: [u64; 3] = [2_000, 10_000, 60_000];
/// An online device is asked once it goes this long without telling the
/// bridge its level (or, when it has to be asked, its charge).
pub const REFRESH_AFTER_MS: u64 = 600_000;
/// Never two probes of one lane closer than this, whatever asks for them.
/// A fidgety mouse can't make the bridge ask more often.
pub const PROBE_GAP_MS: u64 = 2_000;
/// Probes on the pointer lane wait until the pointer has been still this
/// long. A query travels to the mouse over the same radio link as its
/// movement, and might stall the cursor for a moment. stall-check found no
/// clear link on one receiver. We keep the wait as a precaution.
pub const POINTER_PAUSE_MS: u64 = 300;
/// The keeper reports at most one activity event per `ACTIVITY_INTERVAL_MS`
/// and looks at the pointer every `POINTER_REARM_MS` while it moves, so the
/// last movement can trail the last event by both.
const POINTER_STILL_MS: u64 =
    crate::wire::ACTIVITY_INTERVAL_MS + crate::wire::POINTER_REARM_MS + POINTER_PAUSE_MS;
/// Refused commands are told apart up to this many per device, and each is
/// logged at most once a minute.
pub const MAX_REFUSED: usize = 16;
pub const REFUSED_LOG_MS: u64 = 60_000;

pub struct Device {
    pub pid: u16,
    pub lane: Lane,
    /// The catalog entry, if the device is in the table, on either lane.
    pub entry: Option<&'static devices::Device>,
    pub presence: Presence,
    pub last_seen: Option<u64>,
    /// Time of the last live evidence, or of pairing.
    pub quiet_since: u64,
    pub contact: Option<Contact>,
    pub battery: BatteryState,
    pub token: Option<String>,
    pub probe: Probe,
    /// Some for a mouse whose charge the bridge learns by asking.
    pub watch: Option<charge::Watch>,
    /// It answered 07/80 or 07/84 with "not supported", or 07/84 with an
    /// answer too short to hold a value. The bridge stops asking it.
    pub battery_unsupported: bool,
    pub charge_unsupported: bool,
    pub last_activity: Option<u64>,
    pub vdev: Option<VirtualDevice>,
    /// Why the bridge doesn't offer it to OpenRazer, if it doesn't.
    pub not_offered: Option<&'static str>,
    pub learned: crate::wire::Learned,
    /// What it answered 00:81 with last, when the bridge saw it.
    pub firmware: Option<[u8; 2]>,
    /// Commands refused for not being in the list (class, id and size):
    /// how often, and when that was last logged.
    pub refused: BTreeMap<[u8; 3], (u32, u64)>,
}

impl Device {
    #[must_use]
    pub fn new(lane: Lane, pid: u16, now: u64) -> Self {
        let mut d = Self {
            pid,
            lane,
            entry: devices::device(pid),
            presence: Presence::Waiting,
            last_seen: None,
            quiet_since: now,
            contact: None,
            battery: BatteryState::default(),
            token: None,
            probe: Probe {
                due: Some(now),
                ..Probe::default()
            },
            watch: None,
            battery_unsupported: false,
            charge_unsupported: false,
            last_activity: None,
            vdev: None,
            not_offered: None,
            learned: crate::wire::Learned::new(devices::first_get_delay_us(pid)),
            firmware: None,
            refused: BTreeMap::new(),
        };
        // The pointer's movement says when a mouse may have changed mats.
        let mouse = d.own_lane() && lane == Lane::Pointer;
        d.watch = (mouse && d.asks_charge()).then(|| charge::Watch::new(now));
        d
    }

    /// On the lane of its kind. Elsewhere it works, but we don't know what
    /// its status bytes mean there.
    #[must_use]
    pub fn own_lane(&self) -> bool {
        self.entry.is_some_and(|e| e.kind.lane() == self.lane)
    }

    #[must_use]
    pub fn decoder(&self) -> ChargeDecoder {
        let entry = self.entry.filter(|_| self.own_lane());
        entry.map_or(ChargeDecoder::None, |e| e.charge)
    }

    /// Every tested or checked device is asked for its level, whatever its
    /// list says: the query changes nothing, and a device without it says
    /// "not supported".
    #[must_use]
    pub fn asks_battery(&self) -> bool {
        self.entry.is_some_and(devices::Device::qualified) && !self.battery_unsupported
    }

    /// Asked whether it charges too, the question OpenRazer asks, whatever
    /// its list says, unless its status reports decide (the tested
    /// keyboard's) or it failed to answer.
    #[must_use]
    pub fn asks_charge(&self) -> bool {
        let status = self.decoder() == ChargeDecoder::KeyboardStatus;
        !status && self.asks_battery() && !self.charge_unsupported
    }

    #[must_use]
    pub fn unreported(&self) -> Option<Unreported> {
        if self.battery.level.is_none() && self.battery_unsupported {
            return Some(Unreported::Battery);
        }
        let none = self.decoder() == ChargeDecoder::None && !self.asks_charge();
        none.then_some(Unreported::Charge)
    }

    /// When an online device is asked for want of news: ten minutes after
    /// its last reading and after the last probe that was due.
    #[must_use]
    pub fn refresh_at(&self) -> Option<u64> {
        if self.presence != Presence::Online || !self.asks_battery() {
            return None;
        }
        let b = &self.battery;
        let mut news = b.updated.unwrap_or(0);
        if self.decoder() == ChargeDecoder::None && self.asks_charge() {
            news = news.min(b.charge_at.unwrap_or(0));
        }
        Some(news.max(self.probe.tried.unwrap_or(0)) + REFRESH_AFTER_MS)
    }

    /// Takes the probe that may start now. True when there is a query to
    /// send for it.
    pub fn take_probe(&mut self, now: u64) -> bool {
        if self.probe.running || self.probe_at().is_none_or(|at| at > now) {
            return false;
        }
        (self.probe.due, self.probe.tried) = (None, Some(now));
        self.asks_battery()
    }

    /// The receiver says the keyboard is gone (route byte 2). The tested
    /// keyboard has shown that it means that. Any other is asked
    /// first, when the bridge can ask.
    fn gone(&mut self, at: u64, usable: bool) {
        let tested = self.entry.is_some_and(|e| e.tier == Tier::Tested);
        if tested || !usable || !self.asks_battery() {
            self.presence = Presence::Unreachable;
        } else if !self.probe.confirm_gone {
            self.probe.confirm_gone = true;
            self.probe.due = Some(self.probe.due.map_or(at, |due| due.min(at)));
            self.probe.retries = 0;
        }
    }

    #[must_use]
    pub fn id(&self) -> String {
        format!("{}-{:04x}", lane_name(self.lane), self.pid)
    }

    #[must_use]
    pub fn kind(&self) -> DeviceKind {
        self.entry.map_or(DeviceKind::Unknown, |e| e.kind.wire())
    }

    #[must_use]
    pub fn access(&self) -> RouteAccess {
        match self.entry {
            Some(e) if e.qualified() => RouteAccess::Qualified,
            Some(_) => RouteAccess::Unsupported,
            None => RouteAccess::Unknown,
        }
    }

    #[must_use]
    pub fn display_name(&self) -> String {
        device_name(self.pid, self.lane)
    }

    /// Counts a refused command. Returns the count when a log line is due.
    pub fn refuse(&mut self, command: [u8; 3], now: u64) -> Option<u32> {
        if self.refused.len() >= MAX_REFUSED && !self.refused.contains_key(&command) {
            return None;
        }
        let (count, logged) = self.refused.entry(command).or_insert((0, now));
        *count += 1;
        let due = *count == 1 || now >= *logged + REFUSED_LOG_MS;
        *logged = if due { now } else { *logged };
        due.then_some(*count)
    }

    /// Records live evidence and returns true when it ends a long silence.
    pub fn evidence(&mut self, at: u64) -> bool {
        let returned = at.saturating_sub(self.quiet_since) >= RETURN_AFTER_MS;
        self.quiet_since = self.quiet_since.max(at);
        self.seen(at);
        self.presence = Presence::Online;
        if self.probe.after_evidence {
            self.probe.after_evidence = false;
            self.probe.due = Some(at);
            self.probe.retries = 0;
        }
        if let Some(v) = self.vdev.as_mut() {
            v.life(at);
        }
        returned
    }

    pub fn seen(&mut self, at: u64) {
        self.last_seen = Some(self.last_seen.map_or(at, |s| s.max(at)));
    }

    /// Input on the device's lane.
    pub fn activity(&mut self, at: u64) {
        self.last_activity = Some(self.last_activity.map_or(at, |a| a.max(at)));
        if let Some(w) = self.watch.as_mut() {
            w.activity(at);
        }
    }

    /// Applies a reading taken at `at`. Older readings never replace newer.
    pub fn reading(&mut self, at: u64, reading: decode::Reading) -> bool {
        if self.battery.updated.is_some_and(|u| at < u) {
            return false;
        }
        match reading {
            decode::Reading::Level(level) => {
                if let Some(old) = self.battery.level.filter(|old| *old != level) {
                    self.battery.rising = level > old;
                }
                self.battery.level = Some(level);
                self.battery.updated = Some(at);
                return true;
            }
            decode::Reading::Charge(charge) => self.battery.charge = Some(charge),
            // Off the mat or just put on it, or full on a cable: not charging
            // now, but not known to be discharging.
            decode::Reading::NotCharging => {
                if self.battery.charge == Some(Charge::Charging) {
                    self.battery.charge = None;
                }
            }
        }
        self.battery.charge_at = Some(at);
        if let Some(w) = self.watch.as_mut() {
            w.refreshed(at);
        }
        true
    }

    /// When the due probe may start: not within `PROBE_GAP_MS` of
    /// the last one, and on the pointer lane not while the pointer moves.
    #[must_use]
    pub fn probe_at(&self) -> Option<u64> {
        let due = self.probe.due.or_else(|| self.refresh_at())?;
        let gap = self.probe.started.map_or(0, |at| at + PROBE_GAP_MS);
        let still = match (self.lane, self.last_activity) {
            (Lane::Pointer, Some(at)) => at + POINTER_STILL_MS,
            _ => 0,
        };
        Some(due.max(gap).max(still))
    }

    /// Hands a charge check the watch wants on to the probe. A device that
    /// isn't online is left alone: asleep, it would answer 0x04.
    pub fn watch_charge(&mut self, now: u64, usable: bool) {
        let Some(w) = self.watch.as_mut() else {
            return;
        };
        if w.due() > now {
            return;
        }
        if usable && self.presence == Presence::Online && !self.probe.running {
            self.probe.due = Some(self.probe.due.map_or(now, |due| due.min(now)));
        }
        w.served(now);
    }

    /// When `watch_charge` has something to do.
    #[must_use]
    pub fn watch_due(&self, usable: bool) -> Option<u64> {
        let ready = usable && self.presence == Presence::Online && !self.probe.running;
        self.watch
            .as_ref()
            .filter(|_| ready)
            .map(charge::Watch::due)
    }

    #[must_use]
    pub fn stale(&self, now: u64) -> bool {
        self.battery
            .updated
            .is_some_and(|u| now.saturating_sub(u) > BATTERY_STALE_AFTER_MS)
    }

    /// When the reading turns stale, unless it already has.
    #[must_use]
    pub fn stale_at(&self) -> Option<u64> {
        self.battery.updated.map(|u| u + BATTERY_STALE_AFTER_MS + 1)
    }

    /// What the desktop should show right now.
    #[must_use]
    pub fn desired_record(&self) -> Option<Record> {
        let level = self.battery.level?;
        Some(Record {
            capacity: level,
            status: decode::battery_status(level, self.battery.charge),
            name: self.display_name(),
            product_id: self.pid,
        })
    }

    fn openrazer(&self) -> OpenRazerDevice {
        let (state, why) = match (&self.vdev, self.not_offered) {
            (_, Some(why)) => (OpenRazerState::NotOffered, Some(why)),
            (Some(v), None) => v.state(),
            (None, None) => (OpenRazerState::Starting, None),
        };
        let reason = why.and_then(|why| HumanMessage::new(why).ok());
        OpenRazerDevice { state, reason }
    }

    fn battery_view(&self, now: u64) -> Battery {
        let level = self.battery.level.and_then(|l| BatteryPercent::new(l).ok());
        let stale = level.is_some() && self.stale(now);
        Battery {
            charge: if stale || level.is_none() {
                None
            } else {
                self.battery.charge
            },
            updated_ms: level.and(self.battery.updated).map(ms),
            rising: self.battery.rising && level.is_some() && !stale,
            unreported: self.unreported(),
            level,
            stale,
        }
    }
}

fn ms(value: u64) -> MonotonicMs {
    MonotonicMs::new(value).unwrap_or(MonotonicMs::new(0).expect("0 is a valid Ms"))
}

/// What applying a receiver event changed, for the bridge to act on.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    /// A different product id took the lane; the old device is gone.
    Replaced(Lane),
    Paired(Lane),
    Returned(Lane),
    Reading(Lane),
}

pub struct Receiver {
    pub key: u64,
    pub node: String,
    pub id: String,
    pub generation: u64,
    pub vendor_id: u16,
    pub product_id: u16,
    /// The receiver's own state, from the link's `State` events. It shows
    /// as `state` in the snapshot.
    pub suspended: bool,
    pub leaving: bool,
    pub writable: bool,
    pub worker: Worker,
    pub lanes: [Option<Device>; 2],
    /// Vendor reports recorded for `capture`, per report id.
    pub raw: [crate::wire::Rate; 4],
    /// Kept per lane: a device that leaves can still be withdrawn.
    pub publish: [Publish; 2],
    pub inventory: crate::wire::InventoryRead,
    pub slots: Slots,
}

/// What the receiver's paired slots said, and when to ask them.
#[derive(Debug, Default)]
pub struct Slots {
    /// Per lane, from the last answer that could be read.
    pub lanes: [Option<crate::wire::Slot>; 2],
    /// An answer named the devices in this generation.
    pub named: bool,
    /// The first query of the generation has finished, one way or the other.
    pub settled: bool,
    pub due: Option<u64>,
    pub running: bool,
    /// When the last query went out.
    pub asked: u64,
}

/// Replaced devices, handed back so the bridge can clean up after them.
pub struct Applied {
    pub changes: Vec<Change>,
    pub removed: Vec<Device>,
}

impl Receiver {
    /// Exchanges can go out: opened to write, not leaving.
    #[must_use]
    pub fn usable(&self) -> bool {
        self.writable && !self.leaving
    }

    #[must_use]
    pub fn device(&self, lane: Lane) -> Option<&Device> {
        self.lanes[lane_index(lane)].as_ref()
    }

    pub fn device_mut(&mut self, lane: Lane) -> Option<&mut Device> {
        self.lanes[lane_index(lane)].as_mut()
    }

    /// A device has shown life since the slots were last read, and they
    /// said it was not connected: it was off, asleep or still connecting
    /// then, and the answer is old news.
    #[must_use]
    pub fn slot_outdated(&self) -> bool {
        self.lanes.iter().flatten().any(|d| {
            let slot = self.slots.lanes[lane_index(d.lane)];
            let off = slot.is_some_and(|s| s.product_id == d.pid && s.state == Slot::NotConnected);
            off && d.presence == Presence::Online && d.quiet_since > self.slots.asked
        })
    }

    /// The device on `lane`, if it is still the one with `pid`.
    pub fn same_device(&mut self, lane: Lane, pid: u16) -> Option<&mut Device> {
        self.device_mut(lane).filter(|d| d.pid == pid)
    }

    #[must_use]
    pub fn route_available(&self, device: &Device) -> bool {
        self.usable() && matches!(device.presence, Presence::Online | Presence::Asleep)
    }

    pub fn apply(&mut self, record: &EventRecord, now: u64) -> Applied {
        let mut out = Applied {
            changes: Vec::new(),
            removed: Vec::new(),
        };
        let at = record.time_ms.min(now);
        let live = !record.replayed;
        let usable = self.usable();
        match record.event {
            Event::State {
                suspended, leaving, ..
            } => (self.suspended, self.leaving) = (suspended, leaving),
            Event::Identity {
                lane,
                product_id,
                arg0,
                ..
            } => {
                let slot = &mut self.lanes[lane_index(lane)];
                if slot.as_ref().is_none_or(|d| d.pid != product_id) {
                    let mut named = Device::new(lane, product_id, now);
                    match slot.take() {
                        // The stand-in's reading came from this device.
                        Some(old) if old.pid == 0 => named.battery = old.battery,
                        Some(old) => {
                            out.removed.push(old);
                            out.changes.push(Change::Replaced(lane));
                        }
                        None => {}
                    }
                    *slot = Some(named);
                    out.changes.push(Change::Paired(lane));
                }
                let d = slot.as_mut().expect("the slot was just filled");
                if record.source == Source::Passive {
                    d.seen(at);
                    if d.decoder() == ChargeDecoder::MouseStatus {
                        d.contact = decode::contact(arg0);
                    }
                    if live {
                        if lane == Lane::Keyboard && arg0 == 2 {
                            d.gone(at, usable);
                        } else if d.evidence(at) {
                            out.changes.push(Change::Returned(lane));
                        }
                    }
                }
            }
            Event::Status {
                lane,
                level,
                power,
                charge,
                ..
            } => {
                // A device without a name from the receiver gets a stand-in,
                // product id 0, for its level. Input alone doesn't make one:
                // a mouse's macro keys come in as keyboard input.
                let slot = &mut self.lanes[lane_index(lane)];
                if slot.is_none() {
                    out.changes.push(Change::Paired(lane));
                }
                let d = slot.get_or_insert_with(|| Device::new(lane, 0, now));
                d.seen(at);
                if d.reading(at, decode::Reading::Level(decode::level_percent(level))) {
                    // Without a decoder the bytes say nothing, and the last
                    // 07/84 answer stands.
                    let decoder = d.decoder();
                    if decoder != ChargeDecoder::None {
                        d.battery.charge = decode::status_charge(decoder, power, charge);
                    }
                    if decoder == ChargeDecoder::MouseStatus {
                        d.contact = decode::contact(power);
                    }
                    if let Some(w) = d.watch.as_mut() {
                        w.refreshed(at);
                    }
                    out.changes.push(Change::Reading(lane));
                }
                if live && d.evidence(at) {
                    out.changes.push(Change::Returned(lane));
                }
            }
            Event::Activity { lane } => {
                if let Some(d) = self.device_mut(lane)
                    && live
                {
                    d.activity(at);
                    if d.evidence(at) {
                        out.changes.push(Change::Returned(lane));
                    }
                }
            }
            Event::Raw { .. } => {}
        }
        out
    }

    #[must_use]
    pub fn info(&self, now: u64, holder: impl Fn(Lane) -> Option<ClientName>) -> ReceiverInfo {
        ReceiverInfo {
            receiver_id: ReceiverId::new(self.id.clone()).expect("receiver ids are short"),
            generation_id: GenerationId::new(self.generation).expect("generations start at 1"),
            vendor_id: self.vendor_id,
            product_id: self.product_id,
            state: if self.suspended {
                ReceiverState::Suspended
            } else {
                ReceiverState::Active
            },
            writable: self.writable,
            devices: self
                .lanes
                .iter()
                .flatten()
                .map(|d| DeviceInfo {
                    device_id: DeviceId::new(d.id()).expect("device ids are short"),
                    lane: d.lane,
                    kind: d.kind(),
                    vendor_id: devices::RECEIVER.vendor_id,
                    product_id: d.pid,
                    name: d
                        .entry
                        .map(|e| DeviceName::new(e.name).expect("catalog names are short")),
                    support: d.entry.map_or(Support::Unknown, devices::Device::support),
                    presence: d.presence,
                    last_seen_ms: d.last_seen.map(ms),
                    contact: d.contact,
                    controllable: d.token.is_some(),
                    controlled_by: holder(d.lane),
                    battery: d.battery_view(now),
                    first_get: Some(FirstGet {
                        delay_us: d.learned.wait_us,
                        learned_from_us: d.learned.from_us,
                        saved_us: d.learned.saved_us,
                    }),
                    refused: (d.refused.iter())
                        .map(|(&[class, id, size], &(count, _))| Refused {
                            class,
                            id,
                            size,
                            count,
                        })
                        .collect(),
                    slot: (self.slots.lanes[lane_index(d.lane)])
                        .filter(|s| s.product_id == d.pid)
                        .map(|s| s.state),
                    openrazer: Some(d.openrazer()),
                    exchanges: None,
                })
                .collect(),
            inventory: Some(if self.slots.named {
                Inventory::Slots
            } else {
                self.inventory.state
            }),
        }
    }

    #[must_use]
    pub fn routes(&self) -> Vec<Route> {
        self.lanes
            .iter()
            .flatten()
            .map(|d| Route {
                route_token: d
                    .token
                    .as_ref()
                    .map(|t| RouteToken::new(t.clone()).expect("tokens are 64 hex digits")),
                receiver_id: ReceiverId::new(self.id.clone()).expect("receiver ids are short"),
                generation_id: GenerationId::new(self.generation).expect("generations start at 1"),
                device_id: DeviceId::new(d.id()).expect("device ids are short"),
                lane: d.lane,
                kind: d.kind(),
                vendor_id: devices::RECEIVER.vendor_id,
                product_id: d.pid,
                state: if self.route_available(d) {
                    RouteState::Available
                } else {
                    RouteState::Unavailable
                },
                access: d.access(),
                report_bytes: ROUTE_REPORT_BYTES,
            })
            .collect()
    }
}

/// Whether the device's command list has this query with two zero bytes.
#[must_use]
pub fn supports_query(entry: &devices::Device, class: u8, id: u8) -> bool {
    entry
        .commands
        .iter()
        .any(|c| c.class == class && c.id == id && c.sizes.contains(&2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_after_30_minutes() {
        let mut d = Device::new(Lane::Keyboard, 0x0296, 0);
        assert!(d.reading(1_000, decode::Reading::Level(50)));
        d.battery.charge = Some(Charge::Charging);
        let last_fresh = 1_000 + BATTERY_STALE_AFTER_MS;
        assert!(!d.stale(last_fresh));
        assert_eq!(d.stale_at(), Some(last_fresh + 1));
        let view = d.battery_view(last_fresh + 1);
        assert!(view.stale);
        // The level stays, the charge state doesn't.
        assert_eq!(view.level.map(BatteryPercent::get), Some(50));
        assert_eq!(view.charge, None);
        assert_eq!(d.desired_record().map(|r| r.capacity), Some(50));
        // An older reading never replaces it.
        assert!(!d.reading(500, decode::Reading::Level(90)));
    }

    #[test]
    fn queried_zero_ends_charging_only() {
        let mut d = Device::new(Lane::Pointer, 0x00cd, 0);
        d.reading(10, decode::Reading::Charge(Charge::Charging));
        d.reading(20, decode::Reading::NotCharging);
        assert_eq!(d.battery.charge, None);
        d.battery.charge = Some(Charge::Discharging);
        d.reading(30, decode::Reading::NotCharging);
        assert_eq!(d.battery.charge, Some(Charge::Discharging));
    }

    #[test]
    fn refusals_logged_once_a_minute() {
        let mut d = Device::new(Lane::Pointer, 0x00cd, 0);
        let poll_rate = [0x00, 0x85, 1];
        assert_eq!(d.refuse(poll_rate, 1_000), Some(1));
        assert_eq!(d.refuse(poll_rate, 60_999), None);
        assert_eq!(d.refuse(poll_rate, 61_000), Some(3));
        assert_eq!(d.refuse([0x00, 0x85, 2], 61_000), Some(1));
    }

    /// An online mouse with its start-up probe done.
    fn mouse() -> Device {
        let mut d = Device::new(Lane::Pointer, 0x00cd, 0);
        d.presence = Presence::Online;
        d.probe.due = None;
        d
    }

    #[test]
    fn probe_pause_and_gap() {
        let mut d = mouse();
        d.probe.due = Some(1_000);
        assert_eq!(d.probe_at(), Some(1_000));
        d.activity(900);
        assert_eq!(d.probe_at(), Some(900 + POINTER_STILL_MS));
        d.probe.started = Some(800);
        assert_eq!(d.probe_at(), Some(800 + PROBE_GAP_MS));
        // Typing doesn't hold back a keyboard probe.
        let mut k = Device::new(Lane::Keyboard, 0x0296, 0);
        k.activity(900);
        assert_eq!(k.probe_at(), Some(0));
        assert!(k.watch.is_none());
    }

    #[test]
    fn asleep_mouse_isnt_asked() {
        let mut d = mouse();
        d.activity(10_000);
        d.presence = Presence::Asleep;
        assert_eq!(d.watch_due(true), None);
        for now in [11_000, 13_000, 30_000, 100_000] {
            d.watch_charge(now, true);
        }
        assert_eq!(d.probe.due, None);
        // Back online, it gets asked again within the minute.
        d.presence = Presence::Online;
        assert!(d.watch_due(true).is_some_and(|at| at <= 160_000));
    }

    /// Plays pointer activity through a mouse the way the bridge's loop
    /// does, waking at the deadlines it reports, with each probe taking
    /// 10 ms. Returns when probes started.
    fn probe_starts(activity: &[u64], until: u64) -> Vec<u64> {
        let mut d = mouse();
        let mut events = activity.iter().copied().peekable();
        let mut starts: Vec<u64> = Vec::new();
        let mut now = 0;
        loop {
            let done = starts.last().map(|s| s + 10).filter(|_| d.probe.running);
            let next = [
                events.peek().copied(),
                d.watch_due(true),
                d.probe_at().filter(|_| !d.probe.running),
                done,
            ]
            .into_iter()
            .flatten()
            .min();
            let Some(at) = next.filter(|at| *at <= until) else {
                return starts;
            };
            now = now.max(at);
            while let Some(a) = events.next_if(|a| *a <= now) {
                d.activity(a);
            }
            if done.is_some_and(|t| t <= now) {
                d.probe.running = false;
                d.reading(now, decode::Reading::NotCharging);
            }
            d.watch_charge(now, true);
            if d.take_probe(now) {
                d.probe.running = true;
                d.probe.started = Some(now);
                starts.push(now);
            }
        }
    }

    #[test]
    fn asked_after_ten_quiet_minutes() {
        use decode::Reading::{Level, NotCharging};
        // A checked keyboard without a status decoder: its 07/84 answers
        // alone say whether it charges, and they have to be recent too.
        let mut k = Device::new(Lane::Keyboard, 0x0290, 0);
        k.presence = Presence::Online;
        assert!(k.take_probe(0) && k.watch.is_none());
        k.reading(1_000, Level(50));
        k.reading(1_000, NotCharging);
        assert_eq!(k.probe_at(), Some(1_000 + REFRESH_AFTER_MS));
        k.reading(300_000, Level(49));
        assert_eq!(k.probe_at(), Some(1_000 + REFRESH_AFTER_MS));
        // Due but held up (a lease, say): the next one is ten minutes on.
        assert!(k.take_probe(601_000));
        assert_eq!(k.probe_at(), Some(601_000 + REFRESH_AFTER_MS));
        // The tested keyboard's status reports are enough.
        let mut tkl = Device::new(Lane::Keyboard, 0x0296, 0);
        tkl.presence = Presence::Online;
        tkl.take_probe(0);
        tkl.reading(400_000, Level(50));
        assert_eq!(tkl.probe_at(), Some(400_000 + REFRESH_AFTER_MS));
        tkl.presence = Presence::Asleep;
        assert_eq!(tkl.probe_at(), None);
        // Once it said "not supported", it is never asked again.
        k.battery_unsupported = true;
        assert!(!k.take_probe(2_000_000) && k.probe_at().is_none());
    }

    #[test]
    fn rising_and_unreported() {
        // The Low Profile TKL has no status decoder and no 07/84 in its
        // list. Asking is the one way to tell, until it can't answer.
        let mut d = Device::new(Lane::Keyboard, 0x02d2, 0);
        for (at, level) in [(1, 15), (2, 16), (3, 16)] {
            d.reading(at, decode::Reading::Level(level));
        }
        assert!(d.battery_view(3).rising);
        d.reading(4, decode::Reading::Level(15));
        let view = d.battery_view(4);
        assert!(!view.rising && view.unreported.is_none() && d.asks_charge());
        d.charge_unsupported = true;
        assert_eq!(d.unreported(), Some(Unreported::Charge));
        assert_eq!(mouse().unreported(), None);
        // A checked mouse is asked and watched like the 35K, until it says
        // it can't tell.
        let mut naga = Device::new(Lane::Pointer, 0x00a8, 0);
        assert!(naga.watch.is_some() && naga.unreported().is_none());
        naga.battery_unsupported = true;
        assert_eq!(naga.unreported(), Some(Unreported::Battery));
        // A keyboard on the pointer lane keeps its entry, not its decoder.
        let odd = Device::new(Lane::Pointer, 0x0296, 0);
        let what = (odd.kind(), odd.access(), odd.decoder(), odd.watch.is_some());
        let none = ChargeDecoder::None;
        assert_eq!(
            what,
            (DeviceKind::Keyboard, RouteAccess::Qualified, none, false)
        );
    }

    #[test]
    fn gone_confirmed_unless_tested() {
        let gone = |pid, usable| {
            let mut k = Device::new(Lane::Keyboard, pid, 0);
            k.presence = Presence::Online;
            k.gone(5_000, usable);
            (k.presence, k.probe.confirm_gone)
        };
        assert_eq!(gone(0x0290, true), (Presence::Online, true));
        assert_eq!(gone(0x0290, false), (Presence::Unreachable, false));
        assert_eq!(gone(0x0296, true), (Presence::Unreachable, false));
    }

    #[test]
    fn rest_lift_and_put_down() {
        // Still for a minute, then picked up and moved for 2 s.
        let moved: Vec<u64> = (70_000..=72_000).step_by(250).collect();
        let starts = probe_starts(&moved, 100_000);
        assert_eq!(starts, [60_000, 72_000 + POINTER_STILL_MS, 75_000, 90_000]);
    }

    proptest::proptest! {
        #[test]
        fn fidgeting_mouse_is_rate_limited(
            start in 0_u64..10_000,
            gaps in proptest::collection::vec(250_u64..8_000, 0..80),
        ) {
            let activity: Vec<u64> = gaps
                .iter()
                .scan(start, |at, gap| {
                    *at += gap;
                    Some(*at)
                })
                .collect();
            let last = activity.last().copied().unwrap_or(0);
            let starts = probe_starts(&activity, last + 30_000);
            for pair in starts.windows(2) {
                proptest::prop_assert!(pair[1] - pair[0] >= PROBE_GAP_MS, "{starts:?}");
            }
            for s in &starts {
                let moving = activity.iter().any(|a| a <= s && s - a < POINTER_STILL_MS);
                proptest::prop_assert!(!moving, "probe at {s} while moving: {activity:?}");
            }
            if !activity.is_empty() {
                let window = last..=last + charge::PUT_DOWN_AFTER_MS + PROBE_GAP_MS;
                proptest::prop_assert!(starts.iter().any(|s| window.contains(s)), "{starts:?}");
            }
        }
    }
}
