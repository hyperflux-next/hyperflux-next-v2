// SPDX-License-Identifier: GPL-2.0-only

//! The device keeper, `hyperflux-next-keeper`: a small daemon under a user
//! of its own that holds what the bridge shouldn't, the receiver's input on
//! interfaces 0 and 1 and `/dev/uhid`. It passes the vendor reports on to
//! the bridge and turns key reports and the mouse's movement into activity
//! events, so what was typed never leaves it. It makes virtual devices from
//! two fixed templates, the feature-only device the patched OpenRazer
//! drivers bind and the battery devices, and sends no input report but the
//! battery's. For an OpenRazer device it hands the kernel's GETs and SETs
//! to the bridge, passes back the answers to the ones still waiting, and
//! unbinds the driver before destroying the device. When the bridge goes
//! away, its devices go too.

use crate::battery::{self, Resend};
use crate::exchange::{Completions, Done, Outcome};
use crate::hidraw::{HidHost, HidInput, HidrawTransport, PointerInput, SysHidraw, Waiting};
use crate::ipc::{self, Channel, FromKeeper, Template, ToKeeper};
use crate::notify::{self, Notifier};
use crate::platform::UhidOpener;
use crate::poll::Poller;
use crate::transport::{NodeChange, OpenError, Transport};
use crate::uhid::{self, Event};
use crate::vdev::{QUIET_BEFORE_DESTROY_MS, TEARDOWN_DEADLINE_MS};
use crate::wire::{self, Rate};
use hfx_proto::devices::{self, RECEIVER};
use hfx_proto::ids::{HexBytes, ReceiverId, ReportHex};
use hfx_proto::messages::Lane;
use hfx_proto::paths;
use hfx_proto::report::Report;
use rustix::fd::OwnedFd;
use rustix::io::Errno;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct Config {
    pub source: crate::Source,
    /// The bridge's socket for the keeper.
    pub socket: PathBuf,
    /// The uid the bridge has to run as.
    pub bridge_uid: u32,
    pub uhid: UhidOpener,
    /// OpenRazer's drivers in sysfs, where teardowns unbind them. None
    /// takes the simulator's tree, if it offers one.
    pub hid_drivers: Option<PathBuf>,
    /// Counts the bridges that answered the keeper's hello, for tests.
    pub connected: Option<Arc<AtomicU64>>,
}

const TOKEN_STOP: u64 = 1;
const TOKEN_WATCH: u64 = 2;
const TOKEN_BRIDGE: u64 = 3;
const TOKEN_DONE: u64 = 4;
const FIRST_ID: u64 = 16;

const MAX_RECEIVERS: usize = hfx_proto::messages::MAX_RECEIVERS;
/// Two devices on each lane of every receiver.
const MAX_DEVICES: usize = 4 * MAX_RECEIVERS;
/// Requests of one device waiting for the bridge. The kernel sends one at
/// a time. More is a driver gone wrong.
const MAX_ASKED: usize = 4;
/// Between tries to reach the bridge.
const RECONNECT_MS: [u64; 5] = [100, 200, 400, 1_000, 2_000];
const SHUTDOWN_GRACE_MS: u64 = 6_000;

/// Runs the keeper until `stop` becomes readable and its devices are gone.
///
/// # Errors
/// A one-line description of a fatal start-up error.
pub fn run(config: Config, stop: OwnedFd) -> Result<(), String> {
    let mut drivers = config.hid_drivers.clone();
    let host: Box<dyn HidHost> = match &config.source {
        crate::Source::System => {
            Box::new(SysHidraw::new().map_err(|e| format!("can't watch udev: {e}"))?)
        }
        crate::Source::Sim(path) => {
            let sim = crate::sim::SimHidraw::connect(path, Arc::default())
                .map_err(|e| format!("can't reach the simulator at {}: {e}", path.display()))?;
            drivers = drivers.or_else(|| sim.sysfs().map(|s| s.join("drivers")));
            Box::new(sim)
        }
    };
    let keeper =
        Keeper::new(config, host, drivers, stop).map_err(|e| format!("start-up failed: {e}"))?;
    keeper.run().map_err(|e| format!("event loop failed: {e}"))
}

struct Receiver {
    id: ReceiverId,
    name: String,
    composite: Box<dyn HidInput>,
    pointer: PointerInput,
    /// The pointer input's token, and when to watch it again.
    rearm: (u64, Option<u64>),
    keys: Rate,
}

enum Unbind {
    /// A worker is writing to the driver's unbind file.
    Running,
    Done(&'static str),
    /// The unbind file isn't writable: destroy once things are quiet.
    Quiet,
}

struct OpenRazer {
    driver: &'static str,
    /// The kernel's requests the bridge has not answered, true for a GET.
    asked: BTreeMap<u32, bool>,
    last_request: u64,
    teardown: Option<(u64, Unbind)>,
}

enum Kind {
    OpenRazer(OpenRazer),
    Battery(Resend),
}

struct Device {
    fd: OwnedFd,
    token: u64,
    /// The name, for log lines.
    who: String,
    phys: String,
    kind: Kind,
}

/// The CREATE2 a template makes, the device's phys and name, and the
/// driver to unbind, or why the keeper won't make it. Names, descriptors
/// and ids come from the catalog and the templates. The bridge can pick
/// among them but not add to them.
type Made = (Vec<u8>, String, String, Option<&'static str>);

fn made(template: &Template) -> Result<Made, &'static str> {
    match template {
        Template::OpenRazer { product_id, label } => {
            let entry = devices::device(*product_id).ok_or("a product the catalog lacks")?;
            let openrazer = entry.openrazer.as_ref();
            let driver = openrazer.ok_or("a product OpenRazer doesn't drive")?.driver;
            let hex = label.strip_prefix("native-").unwrap_or_default();
            let hex_ok =
                hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
            if !hex_ok || !hfx_proto::openrazer::label_ok(label) {
                return Err("a phys label the patched drivers don't take");
            }
            let phys = format!("{}{label}", hfx_proto::openrazer::PHYS_PREFIX);
            let create = uhid::create(entry.name, label, *product_id);
            Ok((create, phys, entry.name.to_owned(), Some(driver)))
        }
        Template::Battery {
            receiver,
            lane,
            product_id,
        } => {
            let hex = ipc::receiver_hex(receiver).ok_or("a malformed receiver id")?;
            let lane_name = crate::state::lane_name(*lane);
            let phys = format!("{}{hex}/{lane_name}", battery::PHYS_PREFIX);
            let uniq = format!("hyperflux-{hex}-{lane_name}");
            let name = crate::state::device_name(*product_id, *lane);
            let ids = (battery::VENDOR_ID, *product_id);
            let create = uhid::create_device(&name, &phys, &uniq, ids, &battery::descriptor(*lane));
            Ok((create, phys, name, None))
        }
    }
}

struct Keeper {
    poll: Poller,
    now: u64,
    transport: HidrawTransport,
    stop: OwnedFd,
    done: Completions,
    uhid: UhidOpener,
    hid_drivers: Option<PathBuf>,
    socket: PathBuf,
    bridge_uid: u32,
    bridge: Option<Channel>,
    /// The bridge answered the hello: messages go out, devices may be made.
    ready: bool,
    /// When to try the bridge again, and the tries since it last answered.
    retry: (Option<u64>, usize),
    connected: Option<Arc<AtomicU64>>,
    next_id: u64,
    /// By the composite input's token.
    receivers: BTreeMap<u64, Receiver>,
    pointers: HashMap<u64, u64>,
    waiting: Waiting,
    /// By the bridge's id for the device.
    devices: BTreeMap<u64, Device>,
    tokens: HashMap<u64, u64>,
    /// Unbind jobs: the device, and the HID device being unbound.
    jobs: HashMap<u64, (u64, String)>,
    notifier: Option<Notifier>,
    watchdog: Option<(u64, u64)>,
    stopping: Option<u64>,
    logged: BTreeSet<String>,
}

impl Keeper {
    fn new(
        config: Config,
        host: Box<dyn HidHost>,
        hid_drivers: Option<PathBuf>,
        stop: OwnedFd,
    ) -> io::Result<Self> {
        let poll = Poller::new()?;
        let done = Completions::new()?;
        poll.add(&stop, TOKEN_STOP)?;
        poll.add(done.fd(), TOKEN_DONE)?;
        let transport = HidrawTransport::new(host);
        if let Some(fd) = transport.watch_fd() {
            poll.add(fd, TOKEN_WATCH)?;
        }
        let now = hfx_proto::clock::boottime_ms();
        let mut keeper = Self {
            poll,
            now,
            transport,
            stop,
            done,
            uhid: config.uhid,
            hid_drivers,
            socket: config.socket,
            bridge_uid: config.bridge_uid,
            bridge: None,
            ready: false,
            retry: (None, 0),
            connected: config.connected,
            next_id: FIRST_ID,
            receivers: BTreeMap::new(),
            pointers: HashMap::new(),
            waiting: Waiting::default(),
            devices: BTreeMap::new(),
            tokens: HashMap::new(),
            jobs: HashMap::new(),
            notifier: Notifier::from_env(),
            watchdog: notify::watchdog_interval_ms().map(|i| (i, now + i)),
            stopping: None,
            logged: BTreeSet::new(),
        };
        // The receivers first, so nothing they say after the bridge has
        // the keeper is lost.
        for name in keeper.transport.scan()? {
            keeper.watch(&name);
        }
        keeper.connect();
        keeper.service();
        Ok(keeper)
    }

    fn run(mut self) -> io::Result<()> {
        if let Some(n) = &self.notifier {
            n.send("READY=1");
        }
        self.status(paths::KEEPER_WAITING);
        loop {
            let ready = self.poll.wait(self.now)?;
            self.now = hfx_proto::clock::boottime_ms();
            for r in ready {
                match r.token {
                    TOKEN_STOP => self.begin_stop(),
                    TOKEN_WATCH => self.node_changes(),
                    TOKEN_BRIDGE => self.bridge_messages(),
                    TOKEN_DONE => {
                        for done in self.done.drain() {
                            self.unbound(done);
                        }
                    }
                    token if self.receivers.contains_key(&token) => self.composite(token),
                    token if self.pointers.contains_key(&token) => self.pointer(token),
                    token => {
                        if let Some(&device) = self.tokens.get(&token) {
                            self.kernel(device);
                        }
                    }
                }
            }
            self.service();
            if self.stopping.is_some_and(|since| {
                self.devices.is_empty() || self.now >= since + SHUTDOWN_GRACE_MS
            }) {
                break;
            }
        }
        for device in self.devices.keys().copied().collect::<Vec<_>>() {
            self.destroy(device, None);
        }
        Ok(())
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn log_once(&mut self, key: String, line: &str) {
        if self.logged.insert(key) {
            eprintln!("{line}");
        }
    }

    fn status(&self, text: &str) {
        if let Some(n) = &self.notifier {
            n.send(&format!("STATUS={text}"));
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
        for device in self.devices.keys().copied().collect::<Vec<_>>() {
            self.tear_down(device);
        }
    }

    // ------------------------------------------------------------ the bridge

    fn send(&mut self, message: FromKeeper) {
        if let Some(channel) = self.bridge.as_mut().filter(|_| self.ready) {
            channel.send(&message);
        }
    }

    fn refuse(&mut self, what: &str) {
        self.log_once(
            format!("refused {what}"),
            &format!("refused {what} from the bridge"),
        );
    }

    fn connect(&mut self) {
        self.retry.0 = None;
        let stream = UnixStream::connect(&self.socket).map_err(|e| e.to_string());
        match stream.and_then(|s| Channel::new(s, self.bridge_uid)) {
            Ok(mut channel) if self.poll.add(channel.fd(), TOKEN_BRIDGE).is_ok() => {
                let uhid = (self.uhid)().is_ok();
                channel.send(&FromKeeper::Hello {
                    version: ipc::VERSION,
                    uhid,
                });
                self.bridge = Some(channel);
            }
            attempt => {
                let tries = self.retry.1;
                if let (0, Err(why)) = (tries, attempt) {
                    let path = self.socket.display();
                    eprintln!("can't reach the bridge at {path} ({why}); trying again");
                }
                let delay = RECONNECT_MS[tries.min(RECONNECT_MS.len() - 1)];
                self.retry = (Some(self.now + delay), tries + 1);
            }
        }
    }

    /// The bridge is gone, or broke the protocol: so are its devices.
    fn lost_bridge(&mut self, why: &str) {
        let Some(channel) = self.bridge.take() else {
            return;
        };
        self.poll.remove(channel.fd());
        // A bridge that went away is soon back. One that turns us away we
        // try less and less often.
        let tries = if std::mem::take(&mut self.ready) {
            eprintln!("the bridge went away ({why}); removing its devices");
            0
        } else {
            let line = format!("the bridge closed the connection ({why})");
            self.log_once(format!("bridge {why}"), &line);
            self.retry.1
        };
        self.status(paths::KEEPER_WAITING);
        let delay = RECONNECT_MS[tries.min(RECONNECT_MS.len() - 1)];
        self.retry = (Some(self.now + delay), tries + 1);
        for device in self.devices.keys().copied().collect::<Vec<_>>() {
            // Nobody is left to answer what the kernel asked.
            if let Some(Device {
                fd,
                kind: Kind::OpenRazer(o),
                ..
            }) = self.devices.get_mut(&device)
            {
                for (id, get) in std::mem::take(&mut o.asked) {
                    let _ = rustix::io::write(&*fd, &eio(id, get));
                }
            }
            self.tear_down(device);
        }
    }

    fn bridge_messages(&mut self) {
        let Some(channel) = self.bridge.as_mut() else {
            return;
        };
        channel.flush();
        for frame in channel.read() {
            match serde_json::from_slice::<ToKeeper>(&frame) {
                Ok(message) => self.message(message),
                Err(e) => return self.lost_bridge(&format!("it sent something unreadable: {e}")),
            }
            if self.bridge.is_none() {
                return;
            }
        }
        if self.bridge.as_ref().is_some_and(|c| c.dead) {
            self.lost_bridge("the connection closed");
        }
    }

    fn message(&mut self, message: ToKeeper) {
        if !self.ready {
            return match message {
                ToKeeper::Hello { version } if version == ipc::VERSION => {
                    eprintln!("connected to the bridge");
                    (self.ready, self.retry) = (true, (None, 0));
                    self.status(paths::KEEPER_CONNECTED);
                    if let Some(c) = &self.connected {
                        c.fetch_add(1, Ordering::Relaxed);
                    }
                }
                _ => self.lost_bridge("it didn't say hello in this version"),
            };
        }
        match message {
            ToKeeper::Hello { .. } => self.refuse("a second hello"),
            ToKeeper::Create { device, template } => self.create(device, &template),
            ToKeeper::Destroy { device } => self.tear_down(device),
            ToKeeper::Reply {
                device,
                id,
                errno,
                data,
            } => self.reply(device, id, errno, data),
            ToKeeper::Battery {
                device,
                level,
                charging,
            } => self.battery(device, level, charging),
        }
    }

    // ------------------------------------------------------------ devices

    fn create(&mut self, device: u64, template: &Template) {
        if let Err(why) = self.make(device, template) {
            self.send(FromKeeper::Ended { device, why });
        }
    }

    fn make(&mut self, device: u64, template: &Template) -> Result<(), String> {
        let full = self.devices.len() >= MAX_DEVICES || self.devices.contains_key(&device);
        let made = match template {
            _ if self.stopping.is_some() => Err("a device while stopping"),
            _ if full => Err("a device over the limit or with an id in use"),
            template => made(template),
        };
        let taken = |m: &Made| self.devices.values().any(|d| d.phys == m.1);
        let made = made.and_then(|m| {
            if taken(&m) {
                Err("a device with a phys in use")
            } else {
                Ok(m)
            }
        });
        let (create, phys, who, driver) = made.map_err(|what| {
            self.refuse(what);
            format!("the keeper refused {what}")
        })?;
        let fd = (self.uhid)().map_err(|e| format!("can't open /dev/uhid ({e})"))?;
        rustix::io::write(&fd, &create).map_err(|e| format!("UHID_CREATE2 failed ({e})"))?;
        let token = self.id();
        let watched = self.poll.add(&fd, token);
        watched.map_err(|_| "can't watch the UHID device".to_owned())?;
        let (asked, last_request, teardown) = (BTreeMap::new(), self.now, None);
        let kind = match driver {
            Some(driver) => Kind::OpenRazer(OpenRazer {
                driver,
                asked,
                last_request,
                teardown,
            }),
            None => Kind::Battery(Resend::default()),
        };
        self.tokens.insert(token, device);
        let made = Device {
            fd,
            token,
            who,
            phys,
            kind,
        };
        self.devices.insert(device, made);
        Ok(())
    }

    /// What the kernel queued for a device.
    fn kernel(&mut self, device: u64) {
        let (now, ready) = (self.now, self.ready);
        let Some(d) = self.devices.get_mut(&device) else {
            return;
        };
        let mut out = Vec::new();
        let mut failed = false;
        let mut buffer = vec![0_u8; uhid::EVENT_BYTES];
        loop {
            let n = match rustix::io::read(&d.fd, &mut buffer) {
                Ok(n) if n > 0 => n,
                Err(Errno::AGAIN | Errno::INTR) => break,
                _ => {
                    failed = true;
                    break;
                }
            };
            let Some(event) = uhid::decode(&buffer[..n]) else {
                continue;
            };
            let (write, message) = match (&mut d.kind, event) {
                (Kind::Battery(r), event) => (battery_event(r, event, now), None),
                (Kind::OpenRazer(_), Event::Start { flags }) => {
                    (None, Some(FromKeeper::Started { device, flags }))
                }
                (Kind::OpenRazer(o), Event::Stop) => {
                    let stopped = FromKeeper::Stopped { device };
                    (None, o.teardown.is_none().then_some(stopped))
                }
                (Kind::OpenRazer(o), event) => o.request(device, event, ready, now),
            };
            if let Some(bytes) = write {
                failed |= rustix::io::write(&d.fd, &bytes).is_err();
            }
            out.extend(message);
        }
        for message in out {
            self.send(message);
        }
        if failed {
            self.failed(device);
        }
    }

    fn reply(&mut self, device: u64, id: u32, errno: u16, data: Option<ReportHex>) {
        let Some(Device {
            fd,
            kind: Kind::OpenRazer(o),
            ..
        }) = self.devices.get_mut(&device)
        else {
            return self.refuse("a reply for a device without requests");
        };
        let Some(get) = o.asked.remove(&id) else {
            return self.refuse("a reply to nothing the kernel asked");
        };
        let event = match (get, errno, data) {
            (true, 0, Some(report)) => Some(uhid::get_reply(id, Ok(report.bytes()))),
            (true, uhid::EIO, None) => Some(uhid::get_reply(id, Err(uhid::EIO))),
            (false, 0 | uhid::EIO, None) => Some(uhid::set_reply(id, errno)),
            _ => None,
        };
        // Nothing passes that doesn't fit the request, but the kernel still
        // gets an answer.
        let fits = event.is_some();
        let failed = rustix::io::write(&*fd, &event.unwrap_or_else(|| eio(id, get))).is_err();
        if !fits {
            self.refuse("a reply that doesn't fit its request");
        }
        if failed {
            self.failed(device);
        }
    }

    fn battery(&mut self, device: u64, level: u8, charging: bool) {
        let report = [battery::BATTERY_REPORT, level, u8::from(charging)];
        let d = self.devices.get_mut(&device).filter(|_| level <= 100);
        let Some(Device {
            fd,
            kind: Kind::Battery(r),
            ..
        }) = d
        else {
            return self.refuse("a battery report for something other than a battery");
        };
        let write = r.set(report).map(|r| uhid::input(&r));
        if write.is_some_and(|w| rustix::io::write(&*fd, &w).is_err()) {
            self.failed(device);
        }
    }

    /// Takes a device down: a battery at once, an OpenRazer device once
    /// its driver is unbound and the kernel has its answers.
    fn tear_down(&mut self, device: u64) {
        let Some(d) = self.devices.get(&device) else {
            return;
        };
        let Kind::OpenRazer(o) = &d.kind else {
            return self.destroy(device, None);
        };
        if o.teardown.is_some() {
            return;
        }
        let (driver, phys) = (o.driver, d.phys.clone());
        let drivers = self.hid_drivers.clone();
        let bound = drivers.as_ref().and_then(|dir| {
            crate::sysfs::bound_device(dir, driver, &phys)
                .ok()
                .flatten()
        });
        let (Some(dir), Some(name)) = (drivers, bound) else {
            // No driver has it. Nothing can be waiting on it.
            return self.destroy(device, Some("not bound"));
        };
        let unbind = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join(driver).join("unbind"));
        let state = match unbind {
            Ok(mut file) => {
                let (job, notify, unbound) = (self.id(), self.done.notify(), name.clone());
                let spawned = std::thread::Builder::new()
                    .name("hfx-unbind".into())
                    .stack_size(64 * 1024)
                    .spawn(move || {
                        let result = file
                            .write_all(unbound.as_bytes())
                            .map_err(|e| Errno::from_io_error(&e).unwrap_or(Errno::IO));
                        notify.send(Done {
                            job,
                            outcome: Outcome::Unbind(result),
                        });
                    });
                if spawned.is_ok() {
                    self.jobs.insert(job, (device, name));
                    Unbind::Running
                } else {
                    Unbind::Quiet
                }
            }
            Err(e) => {
                let line = format!(
                    "{driver}: unbind isn't writable ({e}); destroying once the device is idle"
                );
                self.log_once(format!("unbind {driver}"), &line);
                Unbind::Quiet
            }
        };
        let now = self.now;
        if let Some(Device {
            kind: Kind::OpenRazer(o),
            ..
        }) = self.devices.get_mut(&device)
        {
            o.teardown = Some((now, state));
        }
    }

    fn unbound(&mut self, done: Done) {
        let (Some((device, name)), Outcome::Unbind(result)) =
            (self.jobs.remove(&done.job), done.outcome)
        else {
            return;
        };
        let drivers = self.hid_drivers.as_ref();
        if let Some(Device {
            kind: Kind::OpenRazer(o),
            ..
        }) = self.devices.get_mut(&device)
            && let Some((_, state)) = &mut o.teardown
        {
            let still = |dir: &PathBuf| crate::sysfs::still_bound(dir, o.driver, &name);
            *state = Unbind::Done(match result {
                Ok(()) if drivers.is_some_and(still) => "still bound",
                Ok(()) => "released",
                Err(_) => "unbind failed",
            });
        }
    }

    fn drive_teardowns(&mut self) {
        let now = self.now;
        let due: Vec<(u64, &'static str)> = (self.devices.iter())
            .filter_map(|(id, d)| {
                let Kind::OpenRazer(o) = &d.kind else {
                    return None;
                };
                let (since, state) = o.teardown.as_ref()?;
                let quiet = o.asked.is_empty();
                let outcome = match state {
                    _ if now >= since + TEARDOWN_DEADLINE_MS => "deadline exceeded",
                    Unbind::Done(outcome) if quiet => outcome,
                    Unbind::Quiet if quiet && now >= o.last_request + QUIET_BEFORE_DESTROY_MS => {
                        "destroyed without unbinding"
                    }
                    _ => return None,
                };
                Some((*id, outcome))
            })
            .collect();
        for (device, outcome) in due {
            self.destroy(device, Some(outcome));
        }
    }

    /// The device's file failed.
    fn failed(&mut self, device: u64) {
        let kind = self.devices.get(&device).map(|d| &d.kind);
        let tearing = matches!(kind, Some(Kind::OpenRazer(o)) if o.teardown.is_some());
        let why = if tearing {
            "ended by a UHID error"
        } else {
            "the UHID device failed"
        };
        self.destroy(device, Some(why));
    }

    /// Destroys a device, and with `why` tells the bridge it is gone.
    fn destroy(&mut self, device: u64, why: Option<&str>) {
        let Some(d) = self.devices.remove(&device) else {
            return;
        };
        self.tokens.remove(&d.token);
        self.jobs.retain(|_, (id, _)| *id != device);
        let _ = rustix::io::write(&d.fd, &uhid::destroy());
        if let (Kind::OpenRazer(o), Some(why)) = (&d.kind, why)
            && let Some((since, _)) = o.teardown
        {
            let ms = self.now.saturating_sub(since);
            eprintln!("openrazer {}: teardown {why} after {ms} ms", d.who);
        }
        if let Some(why) = why {
            self.send(FromKeeper::Ended {
                device,
                why: why.to_owned(),
            });
        }
    }

    // ------------------------------------------------------------ receivers

    fn node_changes(&mut self) {
        for change in self.transport.changes() {
            match change {
                NodeChange::Appeared(name) => self.watch(&name),
                NodeChange::Changed(name) if self.waiting.contains(&name) => self.watch(&name),
                NodeChange::Changed(_) => {}
                NodeChange::Gone(name) => {
                    self.waiting.remove(&name);
                    let key = self.receivers.iter().find(|(_, r)| r.name == name);
                    if let Some(key) = key.map(|(k, _)| *k) {
                        self.unwatch(key);
                    }
                }
            }
        }
    }

    fn watch(&mut self, name: &str) {
        if self.stopping.is_some() || self.receivers.values().any(|r| r.name == name) {
            return;
        }
        if self.receivers.len() >= MAX_RECEIVERS {
            return self.log_once(
                "too many".into(),
                "more than 16 receivers; ignoring the rest",
            );
        }
        let [composite, pointer] = match self.transport.inputs(name) {
            Ok(inputs) => inputs,
            Err(OpenError::Access) => {
                if self.waiting.refused(name, self.now) {
                    eprintln!("{name}: no access yet; trying again once udev has set it up");
                }
                return;
            }
            Err(OpenError::Gone) => return,
            Err(OpenError::Failed(why)) => {
                let line = format!("can't open {name}: {why}");
                return self.log_once(format!("open {name}"), &line);
            }
        };
        self.waiting.remove(name);
        let usb_path = self
            .transport
            .usb_path(name)
            .unwrap_or_else(|| name.to_owned());
        let id = crate::bridge::receiver_id(&usb_path, RECEIVER.vendor_id, RECEIVER.product_id);
        let (key, token) = (self.id(), self.id());
        let watched = self.poll.add(composite.fd(), key);
        if let Err(e) = watched.and_then(|()| self.poll.add_once(pointer.fd(), token)) {
            return eprintln!("{name}: can't watch its input: {e}");
        }
        eprintln!("receiver {id}: reading its input");
        let receiver = Receiver {
            id: ReceiverId::new(id).expect("receiver ids are short"),
            name: name.to_owned(),
            composite,
            pointer: PointerInput::new(pointer),
            rearm: (token, None),
            keys: Rate::default(),
        };
        self.receivers.insert(key, receiver);
        self.pointers.insert(token, key);
    }

    fn unwatch(&mut self, key: u64) {
        if let Some(r) = self.receivers.remove(&key) {
            self.poll.remove(r.composite.fd());
            self.poll.remove(r.pointer.input.fd());
            self.pointers.remove(&r.rearm.0);
            eprintln!("receiver {}: gone", r.id);
        }
    }

    /// Interface 1: vendor reports go to the bridge as they came, key
    /// reports as nothing more than the fact that a key was used.
    fn composite(&mut self, key: u64) {
        let Some(r) = self.receivers.get_mut(&key) else {
            return;
        };
        let mut out = Vec::new();
        let mut buffer = [0_u8; 256];
        let gone = loop {
            let n = match r.composite.read(&mut buffer) {
                Ok(0) => break true,
                Ok(n) => n,
                Err(Errno::AGAIN | Errno::INTR) => break false,
                Err(_) => break true,
            };
            let (time_ms, receiver) = (hfx_proto::clock::boottime_ms(), r.id.clone());
            if wire::VENDOR_REPORTS.contains(&buffer[0]) {
                let head = buffer[..n.min(ipc::VENDOR_HEAD)].to_vec();
                let report = HexBytes::new(head).expect("a vendor report's head fits");
                let len = u16::try_from(n).unwrap_or(u16::MAX);
                out.push(FromKeeper::Vendor {
                    receiver,
                    time_ms,
                    report,
                    len,
                });
            } else if wire::keyboard_input(buffer[0])
                && r.keys.allow(time_ms, wire::ACTIVITY_INTERVAL_MS, 1)
            {
                let lane = Lane::Keyboard;
                out.push(FromKeeper::Activity {
                    receiver,
                    lane,
                    time_ms,
                });
            }
        };
        for message in out {
            self.send(message);
        }
        if gone {
            self.unwatch(key);
        }
    }

    /// Interface 0 moved: we empty it and leave it alone for a while, since
    /// all the bridge needs is when the pointer moved.
    fn pointer(&mut self, token: u64) {
        let (now, key) = (self.now, self.pointers[&token]);
        let Some(r) = self.receivers.get_mut(&key) else {
            return;
        };
        let moved = r.pointer.drain(now);
        r.rearm.1 = moved.is_ok().then_some(now + wire::POINTER_REARM_MS);
        // Gone: interface 1 hangs up too, and that ends the receiver.
        if moved == Ok(true) {
            let receiver = r.id.clone();
            let lane = Lane::Pointer;
            let time_ms = now;
            self.send(FromKeeper::Activity {
                receiver,
                lane,
                time_ms,
            });
        }
    }

    // ------------------------------------------------------------ service

    fn service(&mut self) {
        self.now = hfx_proto::clock::boottime_ms();
        let now = self.now;
        let away = self.bridge.is_none() && self.stopping.is_none();
        if away && self.retry.0.is_some_and(|at| at <= now) {
            self.connect();
        }
        for name in self.waiting.due(now) {
            self.watch(&name);
        }
        for r in self.receivers.values_mut() {
            if r.rearm.1.is_some_and(|at| at <= now) {
                r.rearm.1 = None;
                let _ = self.poll.rearm(r.pointer.input.fd(), r.rearm.0);
            }
        }
        let mut failed = Vec::new();
        for (device, d) in &mut self.devices {
            if let Kind::Battery(r) = &mut d.kind
                && let Some(report) = r.tick(now)
                && rustix::io::write(&d.fd, &uhid::input(&report)).is_err()
            {
                failed.push(*device);
            }
        }
        for device in failed {
            self.failed(device);
        }
        self.drive_teardowns();
        if let Some(channel) = self.bridge.as_mut() {
            channel.flush();
            if let Some(on) = channel.write_interest_changed() {
                let _ = self.poll.want_write(channel.fd(), TOKEN_BRIDGE, on);
            }
        }
        if self.bridge.as_ref().is_some_and(|c| c.dead) {
            self.lost_bridge("the connection failed");
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

    fn next_deadline(&self) -> Option<u64> {
        let mut times: Vec<u64> = Vec::new();
        times.extend(self.watchdog.map(|(_, next)| next));
        times.extend(self.retry.0.filter(|_| self.bridge.is_none()));
        times.extend(self.waiting.next());
        times.extend(self.receivers.values().filter_map(|r| r.rearm.1));
        for d in self.devices.values() {
            match &d.kind {
                Kind::Battery(r) => times.extend(r.due()),
                Kind::OpenRazer(o) => {
                    if let Some((since, state)) = &o.teardown {
                        times.push(since + TEARDOWN_DEADLINE_MS);
                        if matches!(state, Unbind::Quiet) {
                            times.push(o.last_request + QUIET_BEFORE_DESTROY_MS);
                        }
                    }
                }
            }
        }
        times.extend(self.stopping.map(|since| since + SHUTDOWN_GRACE_MS));
        times.into_iter().min()
    }
}

impl OpenRazer {
    /// A GET, or a SET of 90 bytes, of feature report 0 goes to the bridge.
    /// For anything else, too many at once, or any while no bridge is
    /// connected, we answer the kernel with EIO here. Returns what to write
    /// and what to send.
    fn request(
        &mut self,
        device: u64,
        event: Event,
        ready: bool,
        now: u64,
    ) -> (Option<Vec<u8>>, Option<FromKeeper>) {
        let (id, get, report, set) = match event {
            Event::GetReport { id, number, kind } => (id, true, (number, kind), None),
            Event::SetReport {
                id,
                number,
                kind,
                data,
            } => (id, false, (number, kind), Report::try_from(&data[..]).ok()),
            _ => return (None, None),
        };
        // A SET has to carry exactly one report.
        let fits = report == (0, uhid::FEATURE_REPORT) && (get || set.is_some());
        if !fits || !ready || self.asked.len() >= MAX_ASKED {
            return (Some(eio(id, get)), None);
        }
        self.asked.insert(id, get);
        self.last_request = now;
        let set = set.map(ReportHex::from_bytes);
        (None, Some(FromKeeper::Request { device, id, set }))
    }
}

/// What a battery device's event calls for. It offers its input report
/// alone, though hid-input may ask for a feature report anyway.
fn battery_event(r: &mut Resend, event: Event, now: u64) -> Option<Vec<u8>> {
    let input = |report: Option<[u8; 3]>| report.map(|r| uhid::input(&r));
    match event {
        Event::Start { .. } => input(r.kick(now, true)),
        Event::Open => input(r.kick(now, false)),
        Event::Stop => {
            r.stop();
            None
        }
        Event::GetReport { id, .. } => Some(eio(id, true)),
        Event::SetReport { id, .. } => Some(eio(id, false)),
        _ => None,
    }
}

/// The EIO answer to a GET or a SET.
fn eio(id: u32, get: bool) -> Vec<u8> {
    if get {
        uhid::get_reply(id, Err(uhid::EIO))
    } else {
        uhid::set_reply(id, uhid::EIO)
    }
}
