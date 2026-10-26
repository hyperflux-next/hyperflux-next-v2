// SPDX-License-Identifier: GPL-2.0-only

//! The event loop and everything it owns. One thread: epoll wakes it for
//! client requests, the device keeper's messages, uevents and finished
//! exchanges, and deadlines wake it for timers. After every wake-up `service`
//! brings derived state (route tokens, probes, publications, virtual
//! devices) in line with what changed, then works out the next deadline.
//! Receivers are reached through the traits in `transport.rs`, their input
//! and the virtual devices through the keeper (`ipc.rs`), never directly.

use crate::capture::{Entry, Ring};
use crate::decode;
use crate::exchange::{Completions, Done, Outcome, Queued, Work, Worker};
use crate::hidraw::Waiting;
use crate::ipc::{self, Channel, FromKeeper, Template, ToKeeper};
use crate::leases::{Holder, Leases};
use crate::notify::{self, Notifier};
use crate::platform::OpenRazerEnv;
use crate::poll::Poller;
use crate::server::{Conn, MAX_CONNECTIONS};
use crate::state::{
    self, Change, Device, LANES, PROBE_RETRY_MS, PUBLISH_RETRY_MS, Publish, Receiver, lane_name,
};
use crate::transport::{
    Event, Failure, NodeChange, NotSent, OpenError, Record, Source, Timing, Transport,
};
use crate::vdev::{self, Live, Phase, Teardown, VirtualDevice};
use crate::{battery, uhid, wire};
use hfx_proto::devices::{self, ChargeDecoder, Tier};
use hfx_proto::errors::ErrorCode;
use hfx_proto::ids::{ReceiverId, ReportHex, RequestId, lower_hex};
use hfx_proto::messages::{
    Bench, BenchSample, CaptureKind, CaptureLane, CaptureSource, Exchanges, Lane, Presence,
    ReceiverInfo, Route, RouteAccess, Transact,
};
use hfx_proto::report::{self, Report};
use rustix::fd::{AsFd, OwnedFd};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io;
use std::os::unix::net::UnixListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const TOKEN_LISTENER: u64 = 1;
const TOKEN_STOP: u64 = 2;
const TOKEN_DONE: u64 = 3;
const TOKEN_WATCH: u64 = 4;
const TOKEN_UEVENTS: u64 = 5;
const TOKEN_KEEPER_LISTENER: u64 = 6;
const TOKEN_KEEPER: u64 = 7;
const FIRST_ID: u64 = 16;

pub const MAX_RECEIVERS: usize = hfx_proto::messages::MAX_RECEIVERS;
const REVOKED_MEMORY: usize = 256;
/// The keeper stamps what it read. We take older stamps than this as this
/// old. A stuck or odd clock can't move a device's history far.
const MAX_EVENT_AGE_MS: u64 = 60_000;
/// We drop a teardown the keeper has not finished by then.
const TEARDOWN_GIVE_UP_MS: u64 = vdev::TEARDOWN_DEADLINE_MS + 1_000;
/// Exchanges the bridge starts itself get the full 1 s budget.
const OWN_TIMEOUT_MS: u64 = 1_000;
/// How long shutdown waits for virtual devices to come down.
const SHUTDOWN_GRACE_MS: u64 = 6_000;
/// More time between CLOCK_MONOTONIC and CLOCK_BOOTTIME than this means
/// the system was suspended. The bridge is frozen meanwhile and gets no
/// other sign of it.
const RESUME_GAP_MS: u64 = 1_000;
/// When to ask the paired slots again after a cable went in or out.
const SLOTS_AFTER_CABLE_MS: u64 = 3_000;
/// The soonest to ask them again after the last time, when a device shows
/// life that its slot called disconnected.
const SLOTS_AGAIN_MS: u64 = 3_000;

pub(crate) struct OpenRazer {
    pub env: OpenRazerEnv,
    pub uhid_ok: bool,
    pub capable: BTreeMap<&'static str, bool>,
}

pub(crate) struct BenchRun {
    pub params: Bench,
    pub request: Report,
    /// A different query, sent with the device's own wait before each
    /// measured one: the receiver answers an early read with its previous
    /// answer, and the previous identical query's answer would match and
    /// look fast.
    pub spoiler: Option<Report>,
    /// The job in flight is the spoiler, not a sample.
    pub spoiling: bool,
    pub samples: Vec<BenchSample>,
}

pub(crate) enum Origin {
    Transact {
        conn: u64,
        id: RequestId,
        params: Box<Transact>,
    },
    Bench {
        conn: u64,
        id: RequestId,
        run: Box<BenchRun>,
    },
    Probe {
        charge: bool,
    },
    Serial,
    /// The connection and request, and the answers so far.
    ReceiverProbe(u64, RequestId, Vec<Option<ReportHex>>),
    /// The bridge's own paired-slots query: which of `wire::SLOT_QUERIES`.
    Slots(usize),
    Uhid {
        live: u64,
    },
}

pub(crate) struct Pending {
    pub rx: u64,
    pub lane: Lane,
    pub pid: u16,
    pub origin: Origin,
    /// The learned first-GET wait it was sent with, or None for bench's.
    pub learning: Option<u16>,
}

/// How a failed exchange reads for a client: whether it may have been
/// sent, and its code.
#[must_use]
pub fn failure_code(failure: Failure) -> (bool, ErrorCode) {
    match failure {
        Failure::MayHaveBeenSent(_) => (true, ErrorCode::MayHaveBeenSent),
        Failure::NotSent(NotSent::Deadline) => (false, ErrorCode::DeadlineBeforeSend),
        Failure::NotSent(NotSent::Unavailable) => (false, ErrorCode::NothingSent),
    }
}

fn monotonic_ms() -> u64 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    u64::try_from(now.tv_sec).unwrap_or(0) * 1000
        + u64::try_from(now.tv_nsec).unwrap_or(0) / 1_000_000
}

#[must_use]
pub fn receiver_id(usb_path: &str, vendor_id: u16, product_id: u16) -> String {
    let mut hash = Sha256::new();
    hash.update(b"hyperflux-next receiver\0");
    hash.update(usb_path.as_bytes());
    hash.update(vendor_id.to_be_bytes());
    hash.update(product_id.to_be_bytes());
    format!("receiver-{}", lower_hex(&hash.finalize()[..12]))
}

fn random_hex(bytes: usize) -> String {
    let mut random = vec![0_u8; bytes];
    // Without entropy the ids would collide across restarts. The kernel
    // can always provide it. Failing loudly here is wrong in theory, never
    // in practice.
    getrandom::fill(&mut random).expect("getrandom works on Linux");
    lower_hex(&random)
}

/// The bridge's end of the device keeper's connection.
struct KeeperEnd {
    listener: UnixListener,
    /// The uid the keeper has to run as.
    uid: u32,
    channel: Option<Channel>,
    /// The keeper said hello: its devices can be asked for.
    ready: bool,
}

impl KeeperEnd {
    fn send(&mut self, message: &ToKeeper) {
        if let Some(channel) = self.channel.as_mut().filter(|_| self.ready) {
            channel.send(message);
        }
    }
}

pub struct Bridge {
    pub(crate) poll: Poller,
    pub(crate) now: u64,
    pub(crate) instance: String,
    pub(crate) transport: Box<dyn Transport>,
    listener: Option<UnixListener>,
    stop: OwnedFd,
    done: Completions,
    pub(crate) openrazer: Option<OpenRazer>,
    notifier: Option<Notifier>,
    watchdog: Option<(u64, u64)>,
    read_only: bool,
    next_id: u64,
    pub(crate) receivers: BTreeMap<u64, Receiver>,
    waiting: Waiting,
    keeper: KeeperEnd,
    pub(crate) conns: BTreeMap<u64, Conn>,
    pub(crate) jobs: HashMap<u64, Pending>,
    pub(crate) leases: Leases,
    pub(crate) revoked: VecDeque<(String, ErrorCode)>,
    pub(crate) capture: Ring,
    teardowns: Vec<Teardown>,
    pub(crate) state_rev: u64,
    pub(crate) routes_rev: u64,
    state_view: Vec<ReceiverInfo>,
    routes_view: Vec<Route>,
    logged: BTreeSet<String>,
    stopping: Option<u64>,
    suspended_ms: u64,
    pretend_suspend: Option<Arc<AtomicU64>>,
    /// By receiver id, lane and product id, so they outlast replugs.
    exchanges: BTreeMap<(String, Lane, u16), Exchanges>,
    waits: crate::waits::Waits,
}

/// What the bridge runs with, besides its socket.
pub struct Setup {
    pub transport: Box<dyn Transport>,
    pub openrazer: Option<OpenRazerEnv>,
    pub read_only: bool,
    /// Tests add pretend suspend time here to make the bridge see a resume.
    pub pretend_suspend: Option<Arc<AtomicU64>>,
    pub state_dir: Option<std::path::PathBuf>,
    /// Where the device keeper connects, and the uid it has to run as.
    pub keeper: (UnixListener, u32),
}

impl Bridge {
    /// # Errors
    /// Fails when epoll or the completion eventfd cannot be created or the
    /// first scan fails.
    pub fn new(setup: Setup, listener: UnixListener, stop: OwnedFd) -> io::Result<Self> {
        let Setup {
            transport,
            openrazer,
            read_only,
            pretend_suspend,
            state_dir,
            keeper: (keeper_listener, keeper_uid),
        } = setup;
        let poll = Poller::new()?;
        let done = Completions::new()?;
        listener.set_nonblocking(true)?;
        poll.add(&listener, TOKEN_LISTENER)?;
        keeper_listener.set_nonblocking(true)?;
        poll.add(&keeper_listener, TOKEN_KEEPER_LISTENER)?;
        poll.add(&stop, TOKEN_STOP)?;
        poll.add(done.fd(), TOKEN_DONE)?;
        if let Some(fd) = transport.watch_fd() {
            poll.add(fd, TOKEN_WATCH)?;
        }
        let openrazer = openrazer.map(|env| {
            if let Some(fd) = &env.uevents {
                let _ = poll.add(fd, TOKEN_UEVENTS);
            }
            OpenRazer {
                env,
                uhid_ok: false,
                capable: BTreeMap::new(),
            }
        });
        let mut bridge = Self {
            poll,
            now: hfx_proto::clock::boottime_ms(),
            instance: format!("bridge-{}", random_hex(12)),
            transport,
            listener: Some(listener),
            stop,
            done,
            openrazer,
            notifier: Notifier::from_env(),
            watchdog: None,
            read_only,
            next_id: FIRST_ID,
            receivers: BTreeMap::new(),
            waiting: Waiting::default(),
            keeper: KeeperEnd {
                listener: keeper_listener,
                uid: keeper_uid,
                channel: None,
                ready: false,
            },
            conns: BTreeMap::new(),
            jobs: HashMap::new(),
            leases: Leases::default(),
            revoked: VecDeque::new(),
            capture: Ring::default(),
            teardowns: Vec::new(),
            state_rev: 0,
            routes_rev: 0,
            state_view: Vec::new(),
            routes_view: Vec::new(),
            logged: BTreeSet::new(),
            stopping: None,
            suspended_ms: 0,
            pretend_suspend,
            exchanges: BTreeMap::new(),
            waits: crate::waits::Waits::new(state_dir.as_deref()),
        };
        bridge.suspended_ms = bridge.suspend_gap();
        bridge.watchdog = notify::watchdog_interval_ms().map(|i| (i, bridge.now + i));
        bridge.check_drivers();
        for node in bridge.transport.scan()? {
            bridge.admit(&node);
        }
        bridge.service();
        Ok(bridge)
    }

    /// Runs until the stop fd becomes readable and the virtual devices are
    /// down.
    ///
    /// # Errors
    /// Fails when epoll itself fails.
    pub fn run(mut self) -> io::Result<()> {
        if let Some(n) = &self.notifier {
            n.send("READY=1");
        }
        loop {
            let ready = self.poll.wait(self.now)?;
            self.now = hfx_proto::clock::boottime_ms();
            for r in ready {
                match r.token {
                    TOKEN_LISTENER => self.accept(),
                    TOKEN_STOP => self.begin_stop(),
                    TOKEN_DONE => {
                        for done in self.done.drain() {
                            self.on_done(done, true);
                        }
                    }
                    TOKEN_WATCH => self.node_changes(),
                    TOKEN_UEVENTS => self.uevents(),
                    TOKEN_KEEPER_LISTENER => self.accept_keeper(),
                    TOKEN_KEEPER => self.keeper_messages(),
                    token if self.conns.contains_key(&token) => {
                        if let Some(conn) = self.conns.get_mut(&token) {
                            if r.readable || r.hangup {
                                conn.read(self.now);
                            }
                            if r.writable {
                                conn.flush();
                            }
                        }
                    }
                    _ => {}
                }
            }
            self.service();
            if let Some(since) = self.stopping
                && (self.teardowns.is_empty() || self.now >= since + SHUTDOWN_GRACE_MS)
                && !self.any_live()
            {
                break;
            }
        }
        for p in self.receivers.values().flat_map(|r| &r.publish) {
            if let Some((device, ..)) = p.device {
                self.keeper.send(&ToKeeper::Destroy { device });
            }
        }
        if let Some(channel) = self.keeper.channel.as_mut() {
            channel.flush();
        }
        Ok(())
    }

    pub(crate) fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn log_once(&mut self, key: String, line: &str) {
        if self.logged.insert(key) {
            eprintln!("{line}");
        }
    }

    fn begin_stop(&mut self) {
        let mut byte = [0_u8; 64];
        let _ = rustix::io::read(&self.stop, &mut byte);
        if self.stopping.is_some() {
            return;
        }
        if let Some(n) = &self.notifier {
            n.send("STOPPING=1");
        }
        eprintln!("stopping");
        self.stopping = Some(self.now);
        self.listener = None;
        self.conns.clear();
    }

    fn any_live(&self) -> bool {
        self.receivers
            .values()
            .flat_map(|r| r.lanes.iter().flatten())
            .any(|d| matches!(d.vdev.as_ref().map(|v| &v.phase), Some(Phase::Live(_))))
    }

    // ------------------------------------------------------------ clients

    fn accept(&mut self) {
        loop {
            let Some(listener) = &self.listener else {
                return;
            };
            let stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            };
            if self.conns.len() >= MAX_CONNECTIONS || stream.set_nonblocking(true).is_err() {
                // Accepted and closed straight away, without a reply.
                continue;
            }
            let id = self.id();
            if self.poll.add(&stream, id).is_ok() {
                self.conns.insert(id, Conn::new(id, stream, self.now));
            }
        }
    }

    fn close_conn(&mut self, id: u64) {
        if self.conns.remove(&id).is_some() {
            self.leases.end_connection(id);
        }
    }

    // ------------------------------------------------------------ receivers

    fn node_changes(&mut self) {
        for change in self.transport.changes() {
            match change {
                NodeChange::Appeared(node) => self.admit(&node),
                NodeChange::Changed(node) => {
                    if self.waiting.contains(&node) {
                        self.admit(&node);
                    }
                }
                NodeChange::Gone(node) => {
                    self.waiting.remove(&node);
                    let key = self
                        .receivers
                        .iter()
                        .find(|(_, r)| r.node == node)
                        .map(|(k, _)| *k);
                    if let Some(key) = key {
                        self.retire(key);
                    }
                }
            }
        }
    }

    fn admit(&mut self, node: &str) {
        if self.stopping.is_some() || self.receivers.values().any(|r| r.node == node) {
            return;
        }
        if self.receivers.len() >= MAX_RECEIVERS {
            self.log_once(
                "too many".into(),
                "more than 16 receivers; ignoring the rest",
            );
            return;
        }
        let opened = match self.transport.open(node, !self.read_only) {
            Ok(opened) => opened,
            Err(OpenError::Access) => {
                // udev sets permissions a moment after a node appears. We
                // try again on its next event, or on a timer.
                if self.waiting.refused(node, self.now) {
                    eprintln!("{node}: no access yet; trying again once udev has set it up");
                }
                return;
            }
            Err(OpenError::Gone) => return,
            Err(OpenError::Failed(why)) => {
                let line = format!("can't open {node}: {why}");
                return self.log_once(format!("open {node}"), &line);
            }
        };
        self.waiting.remove(node);
        self.waits.load();
        let info = opened.info;
        let usb_path = self.transport.usb_path(node).unwrap_or_else(|| {
            eprintln!("{node}: no USB path in sysfs; its receiver id won't survive a replug");
            node.to_owned()
        });
        let id = receiver_id(&usb_path, info.vendor_id, info.product_id);
        let writable = opened.control.is_some();
        let worker = match opened.control.map(|c| Worker::start(c, self.done.notify())) {
            Some(Ok(worker)) => worker,
            Some(Err(e)) => return eprintln!("{node}: can't start the exchange worker: {e}"),
            None => Worker::default(),
        };
        let key = self.id();
        let rx = Receiver {
            key,
            node: node.to_owned(),
            id,
            generation: info.generation,
            vendor_id: info.vendor_id,
            product_id: info.product_id,
            suspended: info.suspended,
            leaving: false,
            writable,
            worker,
            lanes: [None, None],
            raw: Default::default(),
            publish: Default::default(),
            inventory: info.inventory,
            // The slots name the devices, and the inventory does if they can't.
            slots: state::Slots {
                due: writable.then_some(self.now),
                ..state::Slots::default()
            },
        };
        eprintln!(
            "receiver {} generation {} admitted ({}), {}",
            rx.id,
            rx.generation,
            if writable { "writable" } else { "read-only" },
            hfx_proto::present::inventory_phrase(rx.inventory.state)
        );
        self.receivers.insert(key, rx);
        let event = Event::State {
            suspended: false,
            leaving: false,
            ready: true,
        };
        let (time_ms, replayed, source) = (self.now, true, Source::Receiver);
        self.receiver_event(
            key,
            &Record {
                time_ms,
                replayed,
                source,
                event,
            },
        );
        if !writable {
            self.inventory_names(key);
        }
    }

    /// The receiver names its devices: identities that aren't evidence of
    /// life, like the ones learnt while opening.
    fn name_lanes(&mut self, key: u64, ids: [Option<u16>; 2], source: Source) {
        for (lane, product_id) in LANES.into_iter().zip(ids) {
            let Some(product_id) = product_id.filter(|p| !matches!(p, 0 | 0xffff)) else {
                continue;
            };
            let event = Event::Identity {
                lane,
                product_id,
                arg0: 0,
                arg1: 0,
            };
            let (time_ms, replayed) = (self.now, true);
            self.receiver_event(
                key,
                &Record {
                    time_ms,
                    replayed,
                    source,
                    event,
                },
            );
        }
    }

    fn inventory_names(&mut self, key: u64) {
        let ids = self.receivers.get(&key).and_then(|r| r.inventory.ids);
        let ids = ids.map_or([None, None], |(p, k)| [Some(p), Some(k)]);
        self.name_lanes(key, ids, Source::Inventory);
    }

    fn ask_slots(&mut self) {
        let now = self.now;
        for r in self.receivers.values_mut() {
            if r.slots.due.is_none() && r.slot_outdated() {
                r.slots.due = Some(now.max(r.slots.asked + SLOTS_AGAIN_MS));
            }
        }
        let due = self.receivers.iter().filter(|(_, r)| {
            r.usable() && !r.slots.running && r.slots.due.is_some_and(|at| at <= now)
        });
        for key in due.map(|(k, _)| *k).collect::<Vec<_>>() {
            self.slot_query(key, 0);
        }
    }

    /// Sends one of the paired-slots queries, the fixed table's own, as
    /// `probe receiver` does, with the 2.x module's 8 ms wait.
    fn slot_query(&mut self, key: u64, step: usize) {
        let now = self.now;
        let i = wire::SLOT_QUERIES.get(step);
        let request = i.and_then(|i| devices::receiver_probe(*i));
        let wait = wire::FIRST_GET_DELAY_US;
        let sent = request.is_some_and(|request| {
            let origin = Origin::Slots(step);
            (self.submit(key, Lane::Pointer, request, OWN_TIMEOUT_MS, wait, origin)).is_ok()
        });
        if let Some(r) = self.receivers.get_mut(&key) {
            (r.slots.due, r.slots.running, r.slots.asked) = (None, sent, now);
        }
        if !sent {
            self.slots_done(key, step, None);
        }
    }

    fn slots_done(&mut self, key: u64, step: usize, reply: Option<Report>) {
        let Some(r) = self.receivers.get_mut(&key) else {
            return;
        };
        r.slots.running = false;
        let Some(lanes) = reply.as_ref().and_then(wire::slots) else {
            if step + 1 < wire::SLOT_QUERIES.len() {
                return self.slot_query(key, step + 1);
            }
            if !std::mem::replace(&mut r.slots.settled, true) {
                eprintln!(
                    "receiver {}: its paired slots can't be read; going by the pairing inventory",
                    r.id
                );
                self.inventory_names(key);
            }
            return;
        };
        if !r.slots.named {
            eprintln!("receiver {}: devices named by its paired slots", r.id);
        }
        (r.slots.lanes, r.slots.named, r.slots.settled) = (lanes, true, true);
        self.name_lanes(key, lanes.map(|s| s.map(|s| s.product_id)), Source::Slots);
    }

    fn retire(&mut self, key: u64) {
        let Some(mut rx) = self.receivers.remove(&key) else {
            return;
        };
        eprintln!("receiver {} generation {} retired", rx.id, rx.generation);
        for done in rx.worker.close() {
            self.on_done(done, false);
        }
        self.leases.end_receiver(key);
        for p in &rx.publish {
            if let Some((device, ..)) = p.device {
                self.keeper.send(&ToKeeper::Destroy { device });
            }
        }
        for d in rx.lanes.iter_mut().flatten() {
            if let Some(token) = d.token.take() {
                self.revoke(token, ErrorCode::StaleGeneration);
            }
            self.drop_vdev(d.vdev.take());
        }
    }

    pub(crate) fn revoke(&mut self, token: String, reason: ErrorCode) {
        if self.revoked.len() == REVOKED_MEMORY {
            self.revoked.pop_front();
        }
        self.revoked.push_back((token, reason));
    }

    fn receiver_event(&mut self, key: u64, record: &Record) {
        let now = self.now;
        let Some(rx) = self.receivers.get_mut(&key) else {
            return;
        };
        let (kind, lane) = match record.event {
            Event::State { .. } => (CaptureKind::Receiver, None),
            Event::Identity { lane, .. } => (CaptureKind::Identity, Some(lane)),
            Event::Status { lane, .. } => (CaptureKind::Status, Some(lane)),
            Event::Activity { lane } => (CaptureKind::Activity, Some(lane)),
            Event::Raw { lane, .. } => (CaptureKind::Raw, lane),
        };
        let payload = crate::wire::capture_payload(&record.event);
        self.capture.push(&Entry {
            receiver_id: &rx.id,
            generation: rx.generation,
            time_ms: record.time_ms,
            lane: match lane {
                Some(Lane::Pointer) => CaptureLane::Pointer,
                Some(Lane::Keyboard) => CaptureLane::Keyboard,
                None => CaptureLane::Receiver,
            },
            kind,
            source: match record.source {
                Source::Passive => CaptureSource::Passive,
                Source::Inventory => CaptureSource::Inventory,
                Source::Slots => CaptureSource::Bridge,
                Source::Input => CaptureSource::Input,
                Source::Receiver => CaptureSource::Kernel,
            },
            data: &payload,
        });
        let applied = rx.apply(record, now);
        let rx_id = rx.id.clone();
        for old in applied.removed {
            self.device_gone(key, old, &rx_id);
        }
        for change in applied.changes {
            match change {
                Change::Paired(lane) => {
                    let r = self.receivers.get_mut(&key);
                    if let Some(d) = r.and_then(|r| r.device_mut(lane)).filter(|d| d.pid != 0)
                        && let Some(saved) = self.waits.start((&rx_id, lane, d.pid))
                    {
                        d.learned = d.learned.resume(saved);
                    }
                    if let Some(d) = self.receivers.get(&key).and_then(|r| r.device(lane)) {
                        let usual = d.entry.map(|e| e.kind.lane()).filter(|l| *l != lane);
                        let usual = usual
                            .map(|l| format!(", not the {} lane it usually has", lane_name(l)));
                        eprintln!(
                            "receiver {rx_id}: {} (1532:{:04x}) on the {} lane{}",
                            d.display_name(),
                            d.pid,
                            lane_name(lane),
                            usual.unwrap_or_default()
                        );
                    }
                }
                Change::Returned(lane) => self.returned(key, lane),
                Change::Reading(lane) => {
                    if let Some(rx) = self.receivers.get_mut(&key) {
                        rx.publish[state::lane_index(lane)].reading();
                    }
                }
                Change::Replaced(_) => {}
            }
        }
    }

    /// Everything the receivers said before the system slept is old news:
    /// ask again, and let routes come back with the answers.
    fn system_resumed(&mut self) {
        eprintln!("system resumed");
        let now = self.now;
        for r in self.receivers.values_mut() {
            r.slots.due = Some(now);
        }
        for d in self
            .receivers
            .values_mut()
            .flat_map(|r| r.lanes.iter_mut().flatten())
        {
            d.presence = hfx_proto::messages::Presence::Waiting;
            d.probe = state::Probe {
                due: Some(now),
                ..state::Probe::default()
            };
            if let Some(w) = d.watch.as_mut() {
                *w = crate::charge::Watch::new(now);
            }
        }
    }

    fn suspend_gap(&self) -> u64 {
        let pretend = self
            .pretend_suspend
            .as_ref()
            .map_or(0, |p| p.load(Ordering::Relaxed));
        hfx_proto::clock::boottime_ms().saturating_sub(monotonic_ms()) + pretend
    }

    fn device_gone(&mut self, key: u64, mut old: Device, rx_id: &str) {
        eprintln!(
            "receiver {rx_id}: {} left the {} lane",
            old.display_name(),
            lane_name(old.lane)
        );
        self.leases.end_device(key, old.lane);
        if let Some(token) = old.token.take() {
            self.revoke(token, ErrorCode::NothingSent);
        }
        self.drop_vdev(old.vdev.take());
    }

    fn returned(&mut self, key: u64, lane: Lane) {
        let now = self.now;
        if let Some(d) = self.lane_device(key, lane, None) {
            // A probe's own answer can be the evidence. That probe is
            // already reading the battery.
            if !d.probe.running {
                d.probe.due = Some(now);
                d.probe.retries = 0;
            }
            if let Some(v) = d.vdev.as_mut() {
                v.returned(now);
            }
        }
    }

    // ------------------------------------------------------------ exchanges

    /// Queues an exchange, or gives the origin back when the receiver can't
    /// take it. A `delay_us` of 0 asks for the device's learned first-GET
    /// wait. Bench passes the one it measures.
    pub(crate) fn submit(
        &mut self,
        rx: u64,
        lane: Lane,
        request: Report,
        timeout_ms: u64,
        delay_us: u16,
        origin: Origin,
    ) -> Result<(), Origin> {
        let job = self.id();
        let now = self.now;
        let Some(r) = self.receivers.get_mut(&rx) else {
            return Err(origin);
        };
        // The receiver probes go out with or without a device on the lane.
        let unrouted = matches!(origin, Origin::ReceiverProbe(..) | Origin::Slots(_));
        let fallback = unrouted.then_some((0, crate::wire::Learned::new(delay_us)));
        let Some((pid, learned)) = r.device(lane).map(|d| (d.pid, d.learned)).or(fallback) else {
            return Err(origin);
        };
        if !r.usable() {
            return Err(origin);
        }
        let first_get_delay_us = match delay_us {
            0 => learned.wait_us,
            d => d,
        };
        // Bench's exchanges, measured or not, don't teach the wait.
        let learning =
            (delay_us == 0 && !matches!(origin, Origin::Bench { .. })).then_some(learned.wait_us);
        // A busy device is still working on it and we keep reading, except
        // in a measured bench query that does not ask: that shows how often a
        // device is busy.
        let (get_interval_us, reread_busy) = match &origin {
            Origin::Bench { run, .. } if !run.spoiling => {
                (run.params.get_interval_us, run.params.reread_busy)
            }
            _ => (None, true),
        };
        r.worker.push(Queued {
            job,
            work: Work {
                lane,
                request,
                timing: Timing {
                    timeout_ms: 1,
                    first_get_delay_us,
                    get_interval_us: get_interval_us.map_or(0, |i| i.get()),
                    reread_busy,
                },
                repeat_wait_us: learning.map_or(0, |_| learned.from_us),
                unrouted,
            },
            deadline: now + timeout_ms,
        });
        self.jobs.insert(
            job,
            Pending {
                rx,
                lane,
                pid,
                origin,
                learning,
            },
        );
        Ok(())
    }

    fn on_done(&mut self, done: Done, from_worker: bool) {
        let Some(pending) = self.jobs.remove(&done.job) else {
            return;
        };
        if from_worker && let Some(rx) = self.receivers.get_mut(&pending.rx) {
            rx.worker.finished();
        }
        match done.outcome {
            Outcome::Exchange {
                request,
                result,
                gets,
                rereads,
                elapsed_us,
                fresh,
            } => {
                self.count(&pending, &result, gets, rereads);
                self.exchanged(pending, &request, result, gets, elapsed_us, fresh);
            }
            // The keeper's teardowns unbind; the bridge never asks for one.
            Outcome::Unbind(_) => {}
        }
    }

    fn exchanged(
        &mut self,
        p: Pending,
        request: &Report,
        result: Result<Report, Failure>,
        gets: u8,
        elapsed_us: u32,
        fresh: bool,
    ) {
        // Only an exchange that waited the learned time teaches it.
        if let (Some(wait), true) = (p.learning, fresh)
            && let Some(r) = self.receivers.get_mut(&p.rx)
            && let Some(d) = r.same_device(p.lane, p.pid)
        {
            let status = result.as_ref().ok().map(|reply| reply[report::STATUS]);
            d.learned.record(wait, gets, status);
            let (now_us, firmware) = (d.learned.wait_us, d.firmware);
            if now_us != wait {
                self.waits.learned((&r.id, p.lane, p.pid), firmware, now_us);
            }
        }
        if let Ok(response) = &result
            && !matches!(p.origin, Origin::ReceiverProbe(..) | Origin::Slots(_))
        {
            let own = matches!(
                p.origin,
                Origin::Probe { .. } | Origin::Serial | Origin::Bench { .. }
            );
            self.device_answered(p.rx, p.lane, p.pid, request, response, own);
        }
        match p.origin {
            Origin::Transact { conn, id, params } => self.transact_done(conn, id, *params, result),
            Origin::Bench { conn, id, run } => self.bench_step(
                conn,
                id,
                run,
                p.rx,
                p.lane,
                Some((result, gets, elapsed_us)),
            ),
            Origin::Probe { charge } => self.probe_done(&p, charge, result.ok()),
            Origin::Serial => self.serial_done(&p, request, result.ok()),
            Origin::ReceiverProbe(conn, id, mut answers) => {
                answers.push(result.ok().map(ReportHex::from_bytes));
                self.probe_next(p.rx, Origin::ReceiverProbe(conn, id, answers));
            }
            Origin::Slots(step) => self.slots_done(p.rx, step, result.ok()),
            Origin::Uhid { live } => self.uhid_done(&p, live, result),
        }
    }

    /// Counts our own exchanges with a device and the apps' that went out.
    /// Bench's run at other timings, and the receiver probes go to no device.
    fn count(&mut self, p: &Pending, result: &Result<Report, Failure>, gets: u8, rereads: [u8; 2]) {
        let counted = matches!(
            p.origin,
            Origin::Transact { .. } | Origin::Probe { .. } | Origin::Serial | Origin::Uhid { .. }
        ) && !matches!(result, Err(Failure::NotSent(_)));
        let Some(r) = self.receivers.get(&p.rx).filter(|_| counted) else {
            return;
        };
        let e = self
            .exchanges
            .entry((r.id.clone(), p.lane, p.pid))
            .or_default();
        let add = |n: &mut u32, by: u8| *n = n.saturating_add(u32::from(by));
        add(&mut e.sent, 1);
        add(&mut e.first_read, u8::from(gets == 1 && result.is_ok()));
        add(&mut e.reread_other, rereads[0]);
        add(&mut e.reread_busy, rereads[1]);
        add(&mut e.no_answer, u8::from(result.is_err()));
    }

    /// Bookkeeping for a matched response, whoever asked: presence,
    /// battery harvest and the capture log.
    fn device_answered(
        &mut self,
        rx: u64,
        lane: Lane,
        pid: u16,
        request: &Report,
        response: &Report,
        own: bool,
    ) {
        let now = self.now;
        let Some(r) = self.receivers.get_mut(&rx) else {
            return;
        };
        let (receiver_id, generation) = (r.id.clone(), r.generation);
        let Some(d) = r.same_device(lane, pid) else {
            return;
        };
        let mut returned = false;
        if response[report::STATUS] == report::STATUS_TIMEOUT {
            d.presence = Presence::Asleep;
        } else {
            returned = d.evidence(now);
        }
        let harvested = decode::harvest(d.decoder(), request, response)
            .is_some_and(|reading| d.reading(now, reading));
        // A kept wait is tied to the firmware it was learned on.
        if let Some(firmware) = decode::firmware(request, response) {
            d.firmware = Some(firmware);
            if self.waits.firmware((&receiver_id, lane, pid), firmware) {
                d.learned = crate::wire::Learned::new(d.learned.from_us);
                let name = d.display_name();
                eprintln!(
                    "receiver {receiver_id}: {name} runs other firmware than its kept wait was learned on; starting again from the catalog's"
                );
            }
        }
        if harvested {
            r.publish[state::lane_index(lane)].reading();
        }
        if returned {
            self.returned(rx, lane);
        }
        let serial = request[report::CLASS] == 0x00 && request[report::COMMAND] == 0x82;
        if report::is_query(request) && !serial {
            self.capture.push(&Entry {
                receiver_id: &receiver_id,
                generation,
                time_ms: now,
                lane: match lane {
                    Lane::Pointer => CaptureLane::Pointer,
                    Lane::Keyboard => CaptureLane::Keyboard,
                },
                kind: CaptureKind::Reply,
                source: if own {
                    CaptureSource::Bridge
                } else {
                    CaptureSource::App
                },
                data: response,
            });
        }
    }

    // ------------------------------------------------------------ probes

    fn run_probes(&mut self) {
        let now = self.now;
        let mut start = Vec::new();
        for (key, rx) in &mut self.receivers {
            let usable = rx.usable();
            for d in rx.lanes.iter_mut().flatten() {
                d.watch_charge(now, usable);
                if usable && d.take_probe(now) {
                    start.push((*key, d.lane));
                }
            }
        }
        for (key, lane) in start {
            let started = self.leases.take_internal(key, lane, &Holder::HyperFlux)
                && self
                    .submit(
                        key,
                        lane,
                        probe_request(lane, 0x80),
                        OWN_TIMEOUT_MS,
                        0,
                        Origin::Probe { charge: false },
                    )
                    .is_ok();
            if let Some(d) = self.lane_device(key, lane, None) {
                if started {
                    d.probe.running = true;
                    d.probe.started = Some(now);
                } else {
                    retry_probe(d, now);
                }
            }
            if !started {
                self.release_internal(key, lane, &Holder::HyperFlux);
            }
        }
    }

    /// A command the device's list lacks: counted, and logged at most once
    /// a minute per device and command.
    pub(crate) fn refused(&mut self, rx: u64, lane: Lane, request: &Report) {
        let [class, id, size] = [request[6], request[7], request[report::DATA_SIZE]];
        let r = self.receivers.get_mut(&rx);
        if let Some((r, Some(d))) = r.map(|r| (&r.id, r.lanes[state::lane_index(lane)].as_mut()))
            && let Some(count) = d.refuse([class, id, size], self.now)
        {
            let name = d.display_name();
            eprintln!(
                "receiver {r}: refused {class:02x}/{id:02x} size {size} for {name}: not in its list ({count} so far)"
            );
        }
    }

    fn release_internal(&mut self, rx: u64, lane: Lane, holder: &Holder) {
        self.leases
            .after_internal(rx, lane, holder, false, self.now);
    }

    fn probe_done(&mut self, p: &Pending, charge: bool, response: Option<Report>) {
        let now = self.now;
        let status = response.map(|r| r[report::STATUS]);
        let r = self.receivers.get_mut(&p.rx);
        let rx_id = r.as_ref().map(|r| r.id.clone()).unwrap_or_default();
        let Some(d) = r.and_then(|r| r.same_device(p.lane, p.pid)) else {
            return self.release_internal(p.rx, p.lane, &Holder::HyperFlux);
        };
        let name = d.display_name();
        if !charge && std::mem::take(&mut d.probe.confirm_gone) {
            match status {
                None => d.presence = Presence::Unreachable,
                Some(_) => eprintln!(
                    "receiver {rx_id}: {name} answered after the receiver said it was gone"
                ),
            }
        }
        // An answer too short to hold the value tells nothing either.
        let unusable = charge
            && status == Some(report::STATUS_SUCCESS)
            && response.is_some_and(|r| r[report::DATA_SIZE] < 2);
        if status == Some(report::STATUS_NOT_SUPPORTED) || unusable {
            let what = if charge { "charging" } else { "battery" };
            let how = ["doesn't support", "gave no usable answer to"][usize::from(unusable)];
            eprintln!("receiver {rx_id}: {name} {how} the {what} query; not asking again");
            d.charge_unsupported |= charge;
            d.battery_unsupported |= !charge;
            d.watch = None;
        }
        // Our own query got 0x04 too: the device is asleep.
        if let Some(Phase::Live(live)) = d.vdev.as_mut().map(|v| &mut v.phase)
            && live.doubt.is_some()
            && status == Some(report::STATUS_TIMEOUT)
        {
            live.recycle_at = Some(now);
        }
        let follow_up = !charge && status == Some(report::STATUS_SUCCESS) && d.asks_charge();
        if follow_up
            && self
                .submit(
                    p.rx,
                    p.lane,
                    probe_request(p.lane, 0x84),
                    OWN_TIMEOUT_MS,
                    0,
                    Origin::Probe { charge: true },
                )
                .is_ok()
        {
            return;
        }
        self.release_internal(p.rx, p.lane, &Holder::HyperFlux);
        let Some(d) = self.lane_device(p.rx, p.lane, Some(p.pid)) else {
            return;
        };
        d.probe.running = false;
        match status {
            Some(report::STATUS_SUCCESS) => d.probe.retries = 0,
            _ if charge => {}
            Some(report::STATUS_TIMEOUT) => d.probe.after_evidence = true,
            Some(report::STATUS_NOT_SUPPORTED) => {}
            _ => retry_probe(d, now),
        }
    }

    // ------------------------------------------------------------ batteries

    /// Shows, changes and withdraws each lane's battery device, through the
    /// keeper.
    fn publish_batteries(&mut self) {
        if !self.keeper.ready {
            return;
        }
        let now = self.now;
        for rx in self.receivers.values_mut().filter(|r| r.writable) {
            for lane in LANES {
                let desired = rx.lanes[state::lane_index(lane)]
                    .as_ref()
                    .and_then(Device::desired_record);
                let p = &mut rx.publish[state::lane_index(lane)];
                if p.published == desired || !p.due(now) {
                    continue;
                }
                let pid = desired.as_ref().map(|r| r.product_id);
                // Another device on the lane gets a battery device of its own.
                if let Some((device, ..)) =
                    p.device.filter(|(_, made_for, _)| Some(*made_for) != pid)
                {
                    self.keeper.send(&ToKeeper::Destroy { device });
                    p.device = None;
                }
                let who = format!("{}/{}", rx.id, lane_name(lane));
                match &desired {
                    Some(r) => {
                        if p.device.is_none() {
                            self.next_id += 1;
                            let device = self.next_id;
                            let receiver =
                                ReceiverId::new(rx.id.clone()).expect("receiver ids are short");
                            let product_id = r.product_id;
                            let template = Template::Battery {
                                receiver,
                                lane,
                                product_id,
                            };
                            self.keeper.send(&ToKeeper::Create { device, template });
                            p.device = Some((device, product_id, None));
                        }
                        let report = battery::report(r.capacity, r.status);
                        if let Some((device, _, sent)) = p.device.as_mut()
                            && *sent != Some(report)
                        {
                            *sent = Some(report);
                            let (device, level, charging) = (*device, report[1], report[2] != 0);
                            self.keeper.send(&ToKeeper::Battery {
                                device,
                                level,
                                charging,
                            });
                        }
                        let status = match r.status {
                            decode::BatteryStatus::Charging => ", charging",
                            decode::BatteryStatus::Full => ", full",
                            decode::BatteryStatus::Discharging => ", discharging",
                            decode::BatteryStatus::Unknown => "",
                        };
                        eprintln!("battery {who}: {}%{status}", r.capacity);
                    }
                    None => eprintln!("battery {who}: withdrawn"),
                }
                p.published = desired;
                p.failures = 0;
                p.retry_at = None;
            }
        }
    }

    // ------------------------------------------------------------ OpenRazer

    fn check_drivers(&mut self) {
        let Some(o) = self.openrazer.as_mut() else {
            return;
        };
        for driver in hfx_proto::openrazer::PATCHED_MODULES {
            let capable = o.env.capable(driver);
            if o.capable.insert(driver, capable) != Some(capable) {
                eprintln!(
                    "{driver}: {}",
                    if capable {
                        "HyperFlux Next V2 build loaded"
                    } else {
                        "not loaded or not the HyperFlux Next V2 build"
                    }
                );
            }
        }
    }

    fn uevents(&mut self) {
        let Some(fd) = self.openrazer.as_ref().and_then(|o| o.env.uevents.as_ref()) else {
            return;
        };
        let events = crate::platform::read_uevents(fd.as_fd());
        if events
            .iter()
            .any(|e| e.subsystem == "drivers" || e.subsystem == "module")
        {
            self.check_drivers();
        }
        if events.iter().any(|e| e.subsystem == "hid") {
            self.rescan_twins();
        }
    }

    /// The device on a receiver's lane, if it is still the one with `pid`.
    fn lane_device(&mut self, rx: u64, lane: Lane, pid: Option<u16>) -> Option<&mut Device> {
        let d = self.receivers.get_mut(&rx)?.device_mut(lane)?;
        pid.is_none_or(|pid| d.pid == pid).then_some(d)
    }

    /// Every paired device, by receiver and lane.
    fn device_keys(&self) -> Vec<(u64, Lane)> {
        let all = self.receivers.iter();
        all.flat_map(|(k, r)| r.lanes.iter().flatten().map(move |d| (*k, d.lane)))
            .collect()
    }

    fn drive_vdevs(&mut self) {
        let drivers = self.openrazer.as_ref();
        let drivers = drivers.map(|o| (o.uhid_ok, o.capable.clone()));
        let (stopping, keeper) = (self.stopping.is_some(), self.keeper.ready);
        for (key, lane) in self.device_keys() {
            let Some(rx) = self.receivers.get_mut(&key) else {
                continue;
            };
            let (rx_id, generation) = (rx.id.clone(), rx.generation);
            let Some(d) = rx.device_mut(lane) else {
                continue;
            };
            let entry = d.entry.filter(|e| e.openrazer.is_some());
            let driver = entry
                .and_then(|e| e.openrazer.as_ref())
                .map_or("", |o| o.driver);
            let wanted = d.token.is_some() && entry.is_some();
            let mut vdev = d.vdev.take();
            // We believe a doubted 0x04 once our own probe gets one too
            // (`probe_done`). One probe per doubt.
            if let Some(Phase::Live(live)) = vdev.as_mut().map(|v| &mut v.phase)
                && let Some(since) = live.settle_doubt(d.quiet_since, self.now)
                && !d.probe.running
                && d.probe.due.is_none()
                && d.probe.tried.is_none_or(|t| t < since)
            {
                d.probe.due = Some(self.now);
            }
            let device_id = d.id();
            let pid = d.pid;
            d.not_offered = if entry.is_none() {
                Some("OpenRazer doesn't drive this device")
            } else if drivers.is_none() {
                Some("the bridge runs without OpenRazer devices")
            } else if !wanted || stopping {
                Some("no route to the device now")
            } else if !keeper {
                Some("the device keeper isn't connected")
            } else if drivers
                .as_ref()
                .is_some_and(|(uhid, c)| !uhid || c.get(driver) != Some(&true))
            {
                Some("the patched OpenRazer driver or /dev/uhid isn't there")
            } else {
                None
            };
            if d.not_offered.is_some() {
                self.drop_vdev(vdev);
                continue;
            }
            let v = vdev.get_or_insert_with(|| {
                VirtualDevice::new(
                    format!("{rx_id}/{device_id}"),
                    entry.expect("wanted implies an entry"),
                    vdev::phys_label(&rx_id, generation, &device_id),
                    self.now,
                )
            });
            self.step_vdev(key, lane, v);
            if let Some(d) = self.lane_device(key, lane, Some(pid)) {
                d.vdev = vdev;
            } else {
                self.drop_vdev(vdev);
            }
        }
    }

    /// The wired device of `v`'s model, if one is there, with the serials
    /// that could be read.
    fn wired(&self, v: &VirtualDevice) -> Option<Vec<[u8; 32]>> {
        let o = self.openrazer.as_ref()?;
        let pid = v.entry.wired_product_id;
        crate::sysfs::wired_twin(&o.env.hid_drivers, v.driver(), pid).ok()?
    }

    fn step_vdev(&mut self, key: u64, lane: Lane, v: &mut VirtualDevice) {
        let now = self.now;
        // On its cable the device doesn't answer through the receiver.
        if matches!(v.phase, Phase::Idle { at, .. } if at <= now) {
            v.cable(now, self.wired(v).as_deref());
        }
        match &mut v.phase {
            Phase::Idle { at, first } if *at <= now => {
                let first = *first;
                let request = v.serial_request();
                if !self.leases.take_internal(key, lane, &Holder::OpenRazer) {
                    v.fail(now, "another client holds the device");
                } else if self
                    .submit(key, lane, request, OWN_TIMEOUT_MS, 0, Origin::Serial)
                    .is_ok()
                {
                    v.phase = Phase::Reading { first };
                } else {
                    self.release_internal(key, lane, &Holder::OpenRazer);
                    v.fail(now, "the receiver can't take requests");
                }
            }
            Phase::Blocked { until } if *until <= now => {
                v.phase = Phase::Idle {
                    at: now,
                    first: None,
                };
            }
            Phase::Live(live) => {
                let ended = live.ended.clone();
                let why = if let Some(why) = ended.as_deref() {
                    Some(why)
                } else if live.bad_start {
                    Some("the kernel started the device with numbered reports")
                } else if live.stopped && v.recoveries >= vdev::MAX_RECOVERIES {
                    Some("drivers keep letting go of the device")
                } else if !live.started && now >= live.created + vdev::START_TIMEOUT_MS {
                    Some("no driver bound the device within 10 s")
                } else {
                    None
                };
                if let Some(why) = why {
                    let live = v.take_live(Phase::Blocked { until: 0 }).expect("live");
                    v.block(now, why);
                    self.start_teardown(live);
                } else if live.stopped {
                    v.recoveries += 1;
                    let live = v.take_live(Phase::Blocked { until: 0 }).expect("live");
                    v.fail(now, "the driver let go of the device (UHID_STOP)");
                    self.start_teardown(live);
                } else if live.recycle_at.is_some_and(|t| t <= now) && live.set.is_none() {
                    let live = v.take_live(Phase::Blocked { until: 0 }).expect("live");
                    v.departed(now);
                    self.start_teardown(live);
                }
            }
            _ => {}
        }
    }

    fn serial_done(&mut self, p: &Pending, request: &Report, response: Option<Report>) {
        let now = self.now;
        self.leases
            .after_internal(p.rx, p.lane, &Holder::OpenRazer, response.is_some(), now);
        let d = self.lane_device(p.rx, p.lane, Some(p.pid));
        let Some(mut v) = d.and_then(|d| d.vdev.take()) else {
            return;
        };
        let result = response.map_or(Err(decode::SerialError::Retry), |r| {
            decode::serial_fingerprint(request, &r)
        });
        if let Some(print) = v.serial_read(now, result) {
            v.print = Some(print);
            v.cable(now, self.wired(&v).as_deref());
            if matches!(v.phase, Phase::Reading { .. }) {
                self.publish_vdev(p.pid, &mut v, print);
            }
        }
        if let Some(d) = self.lane_device(p.rx, p.lane, Some(p.pid)) {
            d.vdev = Some(v);
        }
    }

    /// Asks the keeper for the device. When that fails the keeper says why,
    /// and the device is blocked for that reason.
    fn publish_vdev(&mut self, pid: u16, v: &mut VirtualDevice, print: [u8; 32]) {
        let now = self.now;
        let id = self.id();
        let label = v.label.clone();
        let template = Template::OpenRazer {
            product_id: pid,
            label,
        };
        self.keeper.send(&ToKeeper::Create {
            device: id,
            template,
        });
        if print == v.no_serial {
            let line = format!("openrazer {}: no usable serial", v.who);
            self.log_once(format!("no serial {}", v.who), &line);
        }
        eprintln!("openrazer {}: created {}", v.who, v.phys());
        v.published(print);
        v.phase = Phase::Live(Box::new(Live::new(id, now)));
    }

    /// A HID device came or went. A wired device of a paired model takes
    /// the receiver copy's place as soon as it appears, before its driver
    /// binds it and well before OpenRazer's daemon looks at it, and gives
    /// it back when it goes.
    fn rescan_twins(&mut self) {
        let now = self.now;
        for (key, lane) in self.device_keys() {
            let d = self.lane_device(key, lane, None);
            let Some(mut v) = d.and_then(|d| d.vdev.take()) else {
                continue;
            };
            let was = matches!(v.phase, Phase::Cable);
            if let Some(live) = v.cable(now, self.wired(&v).as_deref()) {
                self.start_teardown(live);
            }
            let moved = was != matches!(v.phase, Phase::Cable);
            let Some(r) = self.receivers.get_mut(&key) else {
                continue;
            };
            if moved {
                // The receiver's slot shows the change a moment later.
                let at = now + SLOTS_AFTER_CABLE_MS;
                r.slots.due = Some(r.slots.due.map_or(at, |due| due.min(at)));
            }
            if let Some(d) = r.device_mut(lane) {
                // Without a status decoder we have to ask whether a cable
                // that came or went charges the device.
                let ask = moved && d.decoder() == ChargeDecoder::None && d.asks_charge();
                d.probe.due = d.probe.due.or(ask.then_some(now));
                d.vdev = Some(v);
            }
        }
    }

    /// A virtual device that goes with its device or route.
    fn drop_vdev(&mut self, v: Option<VirtualDevice>) {
        if let Some(mut v) = v
            && let Some(live) = v.take_live(Phase::Blocked { until: 0 })
        {
            self.start_teardown(live);
        }
    }

    /// The keeper unbinds the driver from the device before it destroys it,
    /// and a GET the driver is still waiting on gets its answer instead of
    /// EIO. Until the keeper reports it gone, we answer its requests here.
    fn start_teardown(&mut self, live: Box<Live>) {
        // Without a keeper, or once the keeper has ended it, the device has
        // gone already, and nothing would tell us when its teardown is over.
        if self.keeper.ready && live.ended.is_none() {
            self.keeper.send(&ToKeeper::Destroy { device: live.id });
            let started = self.now;
            self.teardowns.push(Teardown { live, started });
        }
    }

    /// Calls `f` with the Live with this id and the device it belongs to, if
    /// it still has one (a device being torn down doesn't), and hands the
    /// keeper the answers the call left for the kernel.
    fn with_live<T>(
        &mut self,
        id: u64,
        f: impl FnOnce(&mut Live, Option<&'static hfx_proto::devices::Device>) -> T,
    ) -> Option<(T, Option<(u64, Lane)>)> {
        let mut found = None;
        'search: for (key, rx) in &mut self.receivers {
            for d in rx.lanes.iter_mut().flatten() {
                if let Some(v) = d.vdev.as_mut()
                    && let Phase::Live(live) = &mut v.phase
                    && live.id == id
                {
                    found = Some((live.as_mut(), Some(v.entry), Some((*key, d.lane))));
                    break 'search;
                }
            }
        }
        let (live, entry, owner) = match found {
            Some(found) => found,
            None => {
                let t = self.teardowns.iter_mut().find(|t| t.live.id == id)?;
                (t.live.as_mut(), None, None)
            }
        };
        let result = f(live, entry);
        for (id, errno, report) in live.replies.drain(..) {
            let data = report.map(ReportHex::from_bytes);
            let device = live.id;
            self.keeper.send(&ToKeeper::Reply {
                device,
                id,
                errno,
                data,
            });
        }
        Some((result, owner))
    }

    fn uhid_send(&mut self, rx: u64, lane: Lane, live: u64, request: Report) {
        let sent = self.leases.take_internal(rx, lane, &Holder::OpenRazer)
            && self
                .submit(rx, lane, request, OWN_TIMEOUT_MS, 0, Origin::Uhid { live })
                .is_ok();
        if !sent {
            self.release_internal(rx, lane, &Holder::OpenRazer);
            let now = self.now;
            self.with_live(live, |l, _| {
                l.exchanged(vdev::SetResult::NotSent, now, false)
            });
        }
    }

    fn uhid_done(&mut self, p: &Pending, live: u64, result: Result<Report, Failure>) {
        let now = self.now;
        self.leases
            .after_internal(p.rx, p.lane, &Holder::OpenRazer, result.is_ok(), now);
        let set_result = match result {
            Ok(response) => vdev::SetResult::Matched(response),
            Err(Failure::MayHaveBeenSent(_)) => vdev::SetResult::MayHaveBeenSent,
            Err(_) => vdev::SetResult::NotSent,
        };
        let doubt =
            |e: Option<&hfx_proto::devices::Device>| e.is_some_and(|e| e.tier != Tier::Tested);
        if let Some((Some(again), Some((rx, lane)))) =
            self.with_live(live, |l, e| l.exchanged(set_result, now, doubt(e)))
        {
            self.uhid_send(rx, lane, live, again);
        }
    }

    // ------------------------------------------------------------ the keeper

    fn accept_keeper(&mut self) {
        loop {
            let stream = match self.keeper.listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            };
            if self.keeper.channel.is_some() {
                let line = "a second device keeper tried to connect; refused";
                self.log_once("second keeper".into(), line);
                continue;
            }
            match Channel::new(stream, self.keeper.uid) {
                Ok(channel) if self.poll.add(channel.fd(), TOKEN_KEEPER).is_ok() => {
                    self.keeper.channel = Some(channel);
                }
                Ok(_) => {}
                Err(why) => {
                    let line = format!("refused a device keeper: {why}");
                    self.log_once(format!("keeper {why}"), &line);
                }
            }
        }
    }

    fn keeper_messages(&mut self) {
        let Some(channel) = self.keeper.channel.as_mut() else {
            return;
        };
        channel.flush();
        for frame in channel.read() {
            match serde_json::from_slice::<FromKeeper>(&frame) {
                Ok(message) => self.keeper_message(message),
                Err(e) => return self.keeper_gone(&format!("it sent something unreadable: {e}")),
            }
            if self.keeper.channel.is_none() {
                return;
            }
        }
        if self.keeper.channel.as_ref().is_some_and(|c| c.dead) {
            self.keeper_gone("the connection closed");
        }
    }

    fn refuse_keeper(&mut self, what: &str) {
        let line = format!("refused {what} from the device keeper");
        self.log_once(format!("keeper sent {what}"), &line);
    }

    fn keeper_message(&mut self, message: FromKeeper) {
        if !self.keeper.ready {
            return match message {
                FromKeeper::Hello { version, uhid } if version == ipc::VERSION => {
                    self.keeper_hello(uhid);
                }
                _ => self.keeper_gone("it didn't say hello in this version"),
            };
        }
        let now = self.now;
        match message {
            FromKeeper::Hello { .. } => self.refuse_keeper("a second hello"),
            FromKeeper::Activity {
                receiver,
                lane,
                time_ms,
            } => {
                let event = Event::Activity { lane };
                self.keeper_event(&receiver, time_ms, Source::Input, event);
            }
            FromKeeper::Vendor {
                receiver,
                time_ms,
                report,
                len,
            } => self.vendor(&receiver, time_ms, report.as_slice(), len),
            FromKeeper::Request { device, id, set } => {
                let served = self.with_live(device, |live, entry| {
                    let request = match set {
                        Some(report) => live.set(id, *report.bytes(), entry),
                        None => {
                            live.get(id, now);
                            None
                        }
                    };
                    (request, live.refused.take())
                });
                match served {
                    Some(((request, refused), Some((rx, lane)))) => {
                        if let Some(refused) = refused {
                            self.refused(rx, lane, &refused);
                        }
                        if let Some(request) = request {
                            self.uhid_send(rx, lane, device, request);
                        }
                    }
                    Some(_) => {}
                    // One the bridge has let go of: the kernel still gets its answer.
                    None => self.keeper.send(&ToKeeper::Reply {
                        device,
                        id,
                        errno: uhid::EIO,
                        data: None,
                    }),
                }
            }
            FromKeeper::Started { device, flags } => {
                self.with_live(device, |live, _| live.start(flags));
            }
            FromKeeper::Stopped { device } => {
                self.with_live(device, |live, _| live.stop());
            }
            FromKeeper::Ended { device, why } => self.ended(device, &why),
        }
    }

    fn keeper_hello(&mut self, uhid: bool) {
        self.keeper.ready = true;
        self.keeper.send(&ToKeeper::Hello {
            version: ipc::VERSION,
        });
        eprintln!("device keeper connected");
        if let Some(o) = self.openrazer.as_mut() {
            o.uhid_ok = uhid;
            match uhid {
                true => eprintln!("OpenRazer routes on"),
                false => eprintln!("OpenRazer routes off: the device keeper can't open /dev/uhid"),
            }
        }
        for p in self.receivers.values_mut().flat_map(|r| &mut r.publish) {
            p.reading();
        }
    }

    /// The keeper went, and its devices with it. When it comes back, we make
    /// the batteries and the OpenRazer devices again.
    fn keeper_gone(&mut self, why: &str) {
        let Some(channel) = self.keeper.channel.take() else {
            return;
        };
        self.poll.remove(channel.fd());
        if !std::mem::take(&mut self.keeper.ready) {
            let line = format!("dropped a device keeper before its hello ({why})");
            return self.log_once(format!("keeper {why}"), &line);
        }
        eprintln!("device keeper went away ({why})");
        if let Some(o) = self.openrazer.as_mut().filter(|o| o.uhid_ok) {
            o.uhid_ok = false;
            eprintln!("OpenRazer routes off: no device keeper");
        }
        self.teardowns.clear();
        for p in self.receivers.values_mut().flat_map(|r| &mut r.publish) {
            (p.device, p.published) = (None, None);
            p.reading();
        }
    }

    /// Something the keeper read on a receiver.
    fn keeper_event(&mut self, receiver: &ReceiverId, time_ms: u64, source: Source, event: Event) {
        let key = self
            .receivers
            .iter()
            .find(|(_, r)| r.id == receiver.as_str());
        // A receiver not admitted yet, or retired: like an unread report.
        let Some(key) = key.map(|(k, _)| *k) else {
            return;
        };
        let time_ms = time_ms.clamp(self.now.saturating_sub(MAX_EVENT_AGE_MS), self.now);
        let replayed = false;
        let record = Record {
            time_ms,
            replayed,
            source,
            event,
        };
        self.receiver_event(key, &record);
    }

    /// A vendor report as the keeper passed it on: kept for `capture` as it
    /// came, at most four a second per id, and read for what it says.
    fn vendor(&mut self, receiver: &ReceiverId, time_ms: u64, head: &[u8], len: u16) {
        let id = head
            .first()
            .and_then(|id| wire::VENDOR_REPORTS.iter().position(|v| v == id));
        let whole = head.len() == ipc::VENDOR_HEAD || usize::from(len) == head.len();
        let Some(i) = id.filter(|_| whole && usize::from(len) >= head.len()) else {
            return self.refuse_keeper("a vendor report that isn't one");
        };
        let now = self.now;
        let r = self
            .receivers
            .values_mut()
            .find(|r| r.id == receiver.as_str());
        if r.is_some_and(|r| r.raw[i].allow(now, wire::RAW_WINDOW_MS, wire::RAW_PER_WINDOW))
            && let Some(raw) = wire::raw_report(head, len)
        {
            self.keeper_event(receiver, time_ms, Source::Passive, raw);
        }
        if let Some(event) = wire::passive_report(head) {
            self.keeper_event(receiver, time_ms, Source::Passive, event);
        }
    }

    fn ended(&mut self, device: u64, why: &str) {
        if !ipc::plain(why) {
            return self.refuse_keeper("an end without a plain reason");
        }
        let tearing = self.teardowns.len();
        self.teardowns.retain(|t| t.live.id != device);
        if self.teardowns.len() < tearing {
            return;
        }
        if self
            .with_live(device, |live, _| live.ended = Some(why.to_owned()))
            .is_some()
        {
            return;
        }
        let now = self.now;
        for rx in self.receivers.values_mut() {
            for (lane, p) in LANES.into_iter().zip(&mut rx.publish) {
                if p.device.is_some_and(|(d, ..)| d == device) {
                    (p.device, p.published) = (None, None);
                    let who = format!("{}/{}", rx.id, lane_name(lane));
                    refused_battery(p, &who, why, now);
                }
            }
        }
    }

    // ------------------------------------------------------------ service

    /// Brings derived state in line, answers what can be answered and sets
    /// the next deadline.
    pub(crate) fn service(&mut self) {
        self.now = hfx_proto::clock::boottime_ms();
        let now = self.now;
        let gap = self.suspend_gap();
        if gap > self.suspended_ms + RESUME_GAP_MS {
            self.system_resumed();
        }
        self.suspended_ms = gap;
        self.leases.expire(now);
        for node in self.waiting.due(now) {
            self.admit(&node);
        }
        self.teardowns
            .retain(|t| now < t.started + TEARDOWN_GIVE_UP_MS);
        for _ in 0..8 {
            self.settle();
            self.ask_slots();
            self.run_probes();
            self.publish_batteries();
            self.drive_vdevs();
            let mut expired = Vec::new();
            for rx in self.receivers.values_mut() {
                expired.extend(rx.worker.pump(now));
            }
            let quiet = expired.is_empty();
            for done in expired {
                self.on_done(done, false);
            }
            let handled = self.process_requests();
            if quiet && !handled {
                break;
            }
        }
        self.settle();
        self.expire_parked();
        self.waits.save(now);
        let dead: Vec<u64> = self
            .conns
            .values_mut()
            .filter_map(|c| {
                c.flush();
                (c.dead || c.deadline().is_some_and(|d| d <= now)).then_some(c.id)
            })
            .collect();
        for id in dead {
            self.close_conn(id);
        }
        for c in self.conns.values_mut() {
            if let Some(on) = c.write_interest_changed() {
                let _ = self.poll.want_write(&c.stream, c.id, on);
            }
        }
        if let Some(channel) = self.keeper.channel.as_mut() {
            channel.flush();
            if let Some(on) = channel.write_interest_changed() {
                let _ = self.poll.want_write(channel.fd(), TOKEN_KEEPER, on);
            }
        }
        if self.keeper.channel.as_ref().is_some_and(|c| c.dead) {
            self.keeper_gone("the connection failed");
        }
        if let Some((interval, next)) = self.watchdog
            && next <= now
        {
            if let Some(n) = &self.notifier {
                n.send("WATCHDOG=1");
            }
            self.watchdog = Some((interval, now + interval));
        }
        if let Some(at) = self.next_deadline() {
            self.poll.wake_at(at.max(now + 1));
        }
    }

    /// Mints and revokes route tokens, moves the revisions and wakes
    /// parked requests whose condition now holds.
    pub(crate) fn settle(&mut self) {
        let mut revoke = Vec::new();
        for rx in self.receivers.values_mut() {
            let ready: Vec<bool> = rx
                .lanes
                .iter()
                .flatten()
                .map(|d| rx.route_available(d) && d.access() == RouteAccess::Qualified)
                .collect();
            for (d, ready) in rx.lanes.iter_mut().flatten().zip(ready) {
                match (ready, d.token.is_some()) {
                    (true, false) => {
                        d.token = Some(random_hex(32));
                        eprintln!("route {}/{}: available", rx.id, d.id());
                    }
                    (false, true) => {
                        revoke.extend(d.token.take());
                        eprintln!("route {}/{}: unavailable", rx.id, d.id());
                    }
                    _ => {}
                }
            }
        }
        for token in revoke {
            self.revoke(token, ErrorCode::NothingSent);
        }
        let state = self.receiver_infos(false);
        if state != self.state_view {
            self.state_rev += 1;
            self.state_view = state;
        }
        let routes = self.route_list();
        if routes != self.routes_view {
            self.routes_rev += 1;
            self.routes_view = routes;
        }
        self.wake_parked();
    }

    pub(crate) fn receiver_infos(&self, with_last_seen: bool) -> Vec<ReceiverInfo> {
        let mut infos: Vec<ReceiverInfo> = self
            .receivers
            .iter()
            .map(|(key, rx)| {
                let mut info = rx.info(self.now, |lane| {
                    self.leases.on_device(*key, lane).map(|l| l.holder.name())
                });
                // None of these is worth a wake-up.
                for d in &mut info.devices {
                    if with_last_seen {
                        let id = (rx.id.clone(), d.lane, d.product_id);
                        d.exchanges = self.exchanges.get(&id).copied();
                    } else {
                        d.last_seen_ms = None;
                        d.refused.clear();
                    }
                }
                info
            })
            .collect();
        infos.sort_by(|a, b| {
            (&a.receiver_id, a.generation_id).cmp(&(&b.receiver_id, b.generation_id))
        });
        infos
    }

    pub(crate) fn route_list(&self) -> Vec<Route> {
        let mut routes: Vec<Route> = self.receivers.values().flat_map(Receiver::routes).collect();
        routes.sort_by(|a, b| {
            (&a.receiver_id, a.generation_id, &a.device_id).cmp(&(
                &b.receiver_id,
                b.generation_id,
                &b.device_id,
            ))
        });
        routes
    }

    fn next_deadline(&self) -> Option<u64> {
        let now = self.now;
        let mut times: Vec<u64> = Vec::new();
        times.extend(self.watchdog.map(|(_, next)| next));
        times.extend(self.waiting.next());
        times.extend(self.leases.next_expiry());
        times.extend(self.waits.due());
        for rx in self.receivers.values() {
            times.extend(rx.slots.due.filter(|_| rx.usable() && !rx.slots.running));
            times.extend(rx.worker.next_deadline());
            for (lane, p) in LANES.iter().zip(&rx.publish) {
                if rx.writable
                    && self.keeper.ready
                    && p.published != rx.device(*lane).and_then(Device::desired_record)
                {
                    times.extend(p.retry_at);
                }
            }
            for d in rx.lanes.iter().flatten() {
                if rx.usable() && !d.probe.running {
                    times.extend(d.probe_at());
                }
                times.extend(d.watch_due(rx.usable()));
                if !d.stale(now) {
                    times.extend(d.stale_at());
                }
                times.extend(d.vdev.as_ref().and_then(VirtualDevice::due));
            }
        }
        times.extend(
            self.teardowns
                .iter()
                .map(|t| t.started + TEARDOWN_GIVE_UP_MS),
        );
        for c in self.conns.values() {
            times.extend(c.deadline());
            times.extend(c.parked.as_ref().and_then(crate::server::Parked::deadline));
        }
        if let Some(since) = self.stopping {
            times.push(since + SHUTDOWN_GRACE_MS);
        }
        times.into_iter().min()
    }
}

fn probe_request(lane: Lane, command: u8) -> Report {
    let tx = match lane {
        Lane::Pointer => hfx_proto::devices::RECEIVER.pointer_lane_selector,
        Lane::Keyboard => hfx_proto::devices::RECEIVER.keyboard_lane_selector,
    };
    report::build(tx, 0x07, command, &[0, 0])
}

/// A battery device the keeper failed to make or keep: tried again after
/// 1 s, 5 s and 30 s, then with the next reading.
fn refused_battery(p: &mut Publish, who: &str, why: &str, now: u64) {
    let next = PUBLISH_RETRY_MS.get(usize::from(p.failures)).copied();
    p.failures = p.failures.saturating_add(1);
    p.retry_at = next.map(|d| now + d);
    match next {
        Some(d) => eprintln!("battery {who}: refused ({why}); retrying in {} s", d / 1000),
        None => eprintln!("battery {who}: refused ({why}); retrying with the next reading"),
    }
}

fn retry_probe(d: &mut Device, now: u64) {
    if let Some(delay) = PROBE_RETRY_MS.get(d.probe.retries) {
        d.probe.due = Some(now + delay);
        d.probe.retries += 1;
    }
}
