// SPDX-License-Identifier: GPL-2.0-only

//! A simulated HyperFlux V2 receiver with a Basilisk V3 Pro 35K on the
//! pointer lane and a DeathStalker V2 Pro TKL on the keyboard lane.
//!
//! It behaves like the raw receiver behind hidraw: a feature SET is routed
//! by its byte 1 and answered a few milliseconds later, a feature GET
//! returns the newest answer (an old one, for someone else, until then, or
//! busy if asked to), the mouse's movement comes out of interface 0, vendor
//! reports and key presses out of interface 1 and the pairing inventory out
//! of interface 4.
//!
//! A connection to the simulator's socket starts with one JSON frame
//! naming what it is (see `Hello`). After that each kind has a small fixed
//! binary exchange, private to this file. A bridge's watch connection also
//! brings it what the rest of a system would: where OpenRazer's files are,
//! uevents for them, and system suspends.

use crate::hidraw::{Found, HidFeature, HidHost, HidInput};
use crate::platform::{OpenRazerEnv, UhidOpener};
use crate::uhid;
use crate::wire;
use hfx_proto::framing::{read_frame, write_frame};
use hfx_proto::messages::Lane;
use hfx_proto::report::{self, REPORT_LEN, Report};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

pub const MOUSE_PID: u16 = 0x00cd;
pub const KEYBOARD_PID: u16 = 0x0296;
/// How long the receiver takes to have an answer ready: about 3 ms for the
/// mouse and 4 ms for the keyboard, going by the bench runs
/// (ARCHITECTURE.md, section 10). Each one answers by its first-GET wait.
const ANSWER_AFTER: [Duration; 2] = [Duration::from_millis(3), Duration::from_millis(4)];

#[derive(Clone, Debug)]
struct SimDevice {
    pid: u16,
    asleep: bool,
    level: u8,
    power: u8,
    charge: u8,
    /// Just put on the mat: 07/84 reads 0 until then.
    charging_from: Option<Instant>,
    serial: String,
    busy: u32,
    /// A read before the answer is ready gets busy, not the receiver's
    /// previous answer.
    busy_until_ready: bool,
    silent: u32,
    misroute: u32,
    /// Answer times in microseconds to take in turn, instead of the lane's.
    answer_after: Vec<u64>,
    /// Class and command pairs answered with "not supported".
    unsupported: Vec<(u8, u8)>,
    /// The answer to 00:81.
    firmware: [u8; 2],
    requests: Vec<Report>,
    /// All of them, while `requests` keeps the recent ones.
    asked: u64,
}

impl SimDevice {
    fn new(pid: u16, port: usize) -> Self {
        let mouse = pid == MOUSE_PID;
        Self {
            pid,
            asleep: false,
            level: if mouse { 202 } else { 163 },
            power: u8::from(mouse),
            charge: u8::from(mouse),
            charging_from: None,
            serial: format!("{}{port:08}", if mouse { "PM2342H1" } else { "IO2318K2" }),
            busy: 0,
            busy_until_ready: false,
            silent: 0,
            misroute: 0,
            answer_after: Vec::new(),
            unsupported: Vec::new(),
            firmware: [2, 0x33],
            requests: Vec::new(),
            asked: 0,
        }
    }

    /// The device's answer to a request, or None for no answer at all.
    fn answer(&mut self, wire: &Report) -> Option<Report> {
        if self.requests.len() == 4096 {
            self.requests.drain(..1024);
        }
        self.requests.push(*wire);
        self.asked += 1;
        if self.silent > 0 {
            self.silent -= 1;
            return None;
        }
        let mut r = *wire;
        if self.asleep {
            r[report::STATUS] = report::STATUS_TIMEOUT;
        } else if self.busy > 0 {
            self.busy -= 1;
            r[report::STATUS] = report::STATUS_BUSY;
        } else if self
            .unsupported
            .contains(&(wire[report::CLASS], wire[report::COMMAND]))
        {
            r[report::STATUS] = report::STATUS_NOT_SUPPORTED;
        } else {
            r[report::STATUS] = report::STATUS_SUCCESS;
            let args = report::ARGS;
            match (wire[report::CLASS], wire[report::COMMAND]) {
                (0x07, 0x80) => r[args + 1] = self.level,
                (0x07, 0x84) => {
                    let lagging = self.charging_from.is_some_and(|t| Instant::now() < t);
                    r[args + 1] = if lagging { 0 } else { self.charge };
                }
                (0x00, 0x81) => r[args..args + 2].copy_from_slice(&self.firmware),
                (0x00, 0x82) => {
                    r[args..args + 22].fill(0);
                    let serial = self.serial.as_bytes();
                    r[args..args + serial.len().min(22)]
                        .copy_from_slice(&serial[..serial.len().min(22)]);
                }
                _ => {}
            }
        }
        if self.misroute > 0 {
            // A late answer for the other lane.
            self.misroute -= 1;
            r[report::TRANSACTION_ID] ^= 0x80;
        }
        r[report::CHECKSUM] = report::checksum(&r);
        Some(r)
    }
}

struct SimReceiver {
    port: usize,
    generation: Option<u64>,
    lanes: [Option<SimDevice>; 2],
    // The control endpoint.
    last_answer: Report,
    /// The next answer, when it is ready, and whether it's busy until then.
    pending: Option<(Instant, Report, bool)>,
    fail_set: Option<Errno>,
    /// hidraw readers of interfaces 0 and 1.
    inputs: Vec<(u8, UnixStream)>,
    /// Counts movements, so a stopped one's thread knows to end, and the
    /// longest gap between two of its reports.
    motion: (u64, Duration),
    /// The hidraw nodes exist, but udev hasn't given them to the bridge and
    /// the keeper yet: opening them fails with EACCES.
    locked: bool,
    /// Where the pairing inventory's marker is, if it has one.
    marker: Option<usize>,
    /// Requests whose transaction id names no lane. Of these the paired-slots
    /// query alone gets an answer.
    unrouted: Vec<Report>,
    /// Whether it answers that query.
    slots: bool,
}

impl SimReceiver {
    fn usb_path(&self) -> String {
        format!("/sys/devices/pci0000:00/0000:00:14.0/usb1/1-{}", self.port)
    }

    /// Like hidrawN, the name comes back when the receiver is replugged.
    fn hid_node(&self, interface: u8) -> Option<String> {
        self.generation
            .map(|_| format!("sim:{}:if{interface}", self.port))
    }

    /// An input report out of interface 1, or 0. A reader of interface 0
    /// that falls behind loses reports, as one whose hidraw buffer is full.
    fn input_on(&mut self, interface: u8, bytes: &[u8]) {
        let mut frame = u16::try_from(bytes.len())
            .unwrap_or(0)
            .to_le_bytes()
            .to_vec();
        frame.extend_from_slice(bytes);
        self.inputs.retain_mut(|(i, s)| {
            *i != interface
                || match s.write(&frame) {
                    Ok(n) => n == frame.len(),
                    Err(e) => e.kind() == io::ErrorKind::WouldBlock,
                }
        });
    }

    fn input(&mut self, bytes: &[u8]) {
        self.input_on(wire::IF_COMPOSITE, bytes);
    }

    fn set(&mut self, wire: &Report) -> Result<(), Errno> {
        if self.generation.is_none() {
            return Err(Errno::NODEV);
        }
        if let Some(errno) = self.fail_set.take() {
            return Err(errno);
        }
        let lane = match wire[report::TRANSACTION_ID] {
            0x1f => 0,
            0x9f => 1,
            tx => {
                self.unrouted.push(*wire);
                let query = (wire[report::CLASS], wire[report::COMMAND]) == (0x00, 0xbf);
                if self.slots && query && [0x08, 0x88, 0x3f].contains(&tx) {
                    self.pending =
                        Some((Instant::now() + ANSWER_AFTER[0], self.slots(wire), false));
                }
                return Ok(());
            }
        };
        let Some(d) = self.lanes[lane].as_mut() else {
            return Ok(());
        };
        let turn = d
            .answer_after
            .get(d.asked as usize % d.answer_after.len().max(1));
        let after = turn
            .copied()
            .map_or(ANSWER_AFTER[lane], Duration::from_micros);
        if let Some(answer) = d.answer(wire) {
            self.pending = Some((Instant::now() + after, answer, d.busy_until_ready));
        }
        Ok(())
    }

    fn get(&mut self) -> Result<Report, Errno> {
        if self.generation.is_none() {
            return Err(Errno::NODEV);
        }
        match self.pending {
            Some((ready, answer, _)) if ready <= Instant::now() => {
                self.last_answer = answer;
                self.pending = None;
            }
            Some((_, mut early, true)) => {
                early[report::STATUS] = report::STATUS_BUSY;
                return Ok(early);
            }
            _ => {}
        }
        Ok(self.last_answer)
    }

    /// As the tested receiver answers 00:bf: two slots, each with
    /// whether its device is connected (here: awake) and its product id.
    fn slots(&self, wire: &Report) -> Report {
        let mut r = *wire;
        r[report::STATUS] = report::STATUS_SUCCESS;
        r[report::ARGS] = 2;
        for (i, d) in self.lanes.iter().enumerate() {
            let (on, pid) = d.as_ref().map_or((false, 0), |d| (!d.asleep, d.pid));
            let at = report::ARGS + 1 + 3 * i;
            r[at] = u8::from(on);
            r[at + 1..at + 3].copy_from_slice(&pid.to_be_bytes());
        }
        r[report::CHECKSUM] = report::checksum(&r);
        r
    }

    /// Filler unlike any receiver's: the bridge has to go by the marker
    /// and the product ids after it.
    fn inventory(&self) -> Vec<u8> {
        let pid = |i: usize| self.lanes[i].as_ref().map_or(0, |d| d.pid);
        let mut report = vec![0x5a; wire::INVENTORY_LEN];
        report[0] = wire::INVENTORY_REPORT_ID;
        if let Some(m) = self.marker {
            report[m..m + 2].fill(0xff);
            report[m + 18..m + 20].copy_from_slice(&pid(0).to_be_bytes());
            report[m + 20..m + 22].copy_from_slice(&pid(1).to_be_bytes());
        }
        report
    }

    fn drop_readers(&mut self) {
        for (_, s) in self.inputs.drain(..) {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

fn lane_index(lane: Lane) -> usize {
    match lane {
        Lane::Pointer => 0,
        Lane::Keyboard => 1,
    }
}

/// What a watch connection gets: the receivers whose hidraw nodes exist,
/// by USB path, with the node of each interface, after every change, and
/// the rest when there is news.
#[derive(Serialize, Deserialize)]
struct Note {
    receivers: Vec<(String, BTreeMap<u8, String>)>,
    /// A sysfs tree with OpenRazer's files in it, for the bridge to use.
    sysfs: Option<PathBuf>,
    /// A kernel uevent for that tree.
    uevent: Option<String>,
    /// The system slept this long.
    slept_ms: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Hello {
    /// The receiver list now and after every change.
    Watch,
    /// A hidraw interface, opened to stream its input reports, or for
    /// feature SETs and GETs.
    Hidraw { node: String, input: bool },
}

struct World {
    receivers: Vec<SimReceiver>,
    next_generation: u64,
    watchers: Vec<UnixStream>,
    sysfs: Option<PathBuf>,
}

impl World {
    /// What udev says about hidraw nodes, after a change and once it has
    /// set their permissions: the nodes there are now.
    fn announce(&mut self) {
        self.tell(None, 0);
    }

    /// The receivers, and any uevent or suspend.
    fn tell(&mut self, uevent: Option<String>, slept_ms: u64) {
        let note = serde_json::to_vec(&self.note(uevent, slept_ms)).unwrap_or_default();
        self.watchers.retain_mut(|s| write_frame(s, &note).is_ok());
    }

    fn note(&self, uevent: Option<String>, slept_ms: u64) -> Note {
        let plugged = self.receivers.iter().filter(|r| r.generation.is_some());
        let receivers = plugged.map(|r| {
            let nodes = [wire::IF_POINTER, wire::IF_COMPOSITE, wire::IF_INVENTORY]
                .into_iter()
                .filter_map(|i| Some((i, r.hid_node(i)?)))
                .collect();
            (r.usb_path(), nodes)
        });
        let (receivers, sysfs) = (receivers.collect(), self.sysfs.clone());
        Note {
            receivers,
            sysfs,
            uevent,
            slept_ms,
        }
    }

    fn plug(&mut self, i: usize) {
        if self.receivers[i].generation.is_some() {
            return;
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        let r = &mut self.receivers[i];
        r.generation = Some(generation);
        r.last_answer = [0; REPORT_LEN];
        r.pending = None;
        self.announce();
    }

    fn unplug(&mut self, i: usize) {
        let r = &mut self.receivers[i];
        if r.generation.take().is_none() {
            return;
        }
        r.drop_readers();
        self.announce();
    }

    /// The receiver and interface a hidraw node names, if it still exists.
    fn hid_target(&self, node: &str) -> Option<(usize, u8)> {
        let i = self.receivers.iter().position(|r| {
            [wire::IF_POINTER, wire::IF_COMPOSITE, wire::IF_INVENTORY]
                .iter()
                .any(|n| r.hid_node(*n).as_deref() == Some(node))
        })?;
        let interface = node.rsplit_once(":if")?.1.parse().ok()?;
        Some((i, interface))
    }
}

/// Handle to the simulated receivers. Clones share them.
#[derive(Clone)]
pub struct Sim {
    world: Arc<Mutex<World>>,
}

const OP_SET: u8 = 1;
const OP_GET: u8 = 2;
/// A boot mouse's report: no buttons, one step right.
const MOVED: [u8; 4] = [0, 1, 0, 0];

impl Sim {
    /// `receivers` receivers, each plugged in with both devices paired.
    #[must_use]
    pub fn new(receivers: usize) -> Self {
        let mut world = World {
            receivers: Vec::new(),
            next_generation: 1,
            watchers: Vec::new(),
            sysfs: None,
        };
        for i in 0..receivers {
            let port = i + 1;
            world.receivers.push(SimReceiver {
                port,
                generation: None,
                lanes: [
                    Some(SimDevice::new(MOUSE_PID, port)),
                    Some(SimDevice::new(KEYBOARD_PID, port)),
                ],
                last_answer: [0; REPORT_LEN],
                pending: None,
                fail_set: None,
                inputs: Vec::new(),
                motion: (0, Duration::ZERO),
                locked: false,
                marker: Some(wire::INVENTORY_MARKER),
                unrouted: Vec::new(),
                slots: true,
            });
            world.plug(i);
        }
        Self {
            world: Arc::new(Mutex::new(world)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, World> {
        self.world.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn with<T>(&self, receiver: usize, f: impl FnOnce(&mut SimReceiver) -> T) -> Option<T> {
        self.lock().receivers.get_mut(receiver).map(f)
    }

    fn with_device<T>(
        &self,
        receiver: usize,
        lane: Lane,
        f: impl FnOnce(&mut SimDevice) -> T,
    ) -> Option<T> {
        self.with(receiver, |r| r.lanes[lane_index(lane)].as_mut().map(f))
            .flatten()
    }

    #[must_use]
    pub fn receivers(&self) -> usize {
        self.lock().receivers.len()
    }

    /// The receiver's USB path, which its receiver id is made from.
    #[must_use]
    pub fn usb_path(&self, receiver: usize) -> String {
        self.with(receiver, |r| r.usb_path()).unwrap_or_default()
    }

    /// A passive status report: level 0..=255, power and charge bytes as
    /// the device sends them.
    pub fn status(&self, receiver: usize, lane: Lane, level: u8, power: u8, charge: u8) {
        self.with(receiver, |r| {
            if let Some(d) = r.lanes[lane_index(lane)].as_mut() {
                (d.level, d.power, d.charge) = (level, power, charge);
                d.charging_from = None;
            }
            let id = if lane == Lane::Pointer { 0x05 } else { 0x09 };
            r.input(&[id, 0x31, level, power, charge, 0x00]);
        });
    }

    /// Input on a lane: a key on interface 1, or one report of movement, a
    /// boot mouse's without a report id, on interface 0.
    pub fn activity(&self, receiver: usize, lane: Lane) {
        match lane {
            Lane::Pointer => self.with(receiver, |r| r.input_on(wire::IF_POINTER, &MOVED)),
            Lane::Keyboard => self.with(receiver, |r| r.input(&[0x01, 0, 0, 0x04, 0, 0, 0, 0, 0])),
        };
    }

    /// The mouse moves, `per_second` reports a second out of interface 0
    /// (the tested receiver sent about 900), until it is set to 0.
    /// Returns the longest gap between two reports of the movement that
    /// ends, which shows when the whole process stood still.
    pub fn set_motion(&self, receiver: usize, per_second: u32) -> Duration {
        let Some((epoch, gap)) = self.with(receiver, |r| {
            r.motion.0 += 1;
            (r.motion.0, std::mem::take(&mut r.motion.1))
        }) else {
            return Duration::ZERO;
        };
        let sim = self.clone();
        let period = Duration::from_secs(1) / per_second.max(1);
        let moving = move || {
            let (mut next, mut last) = (Instant::now(), Instant::now());
            while sim.with(receiver, |r| {
                let on = r.motion.0 == epoch;
                if on {
                    r.motion.1 = r.motion.1.max(last.elapsed());
                    r.input_on(wire::IF_POINTER, &MOVED);
                }
                on
            }) == Some(true)
            {
                (last, next) = (Instant::now(), next + period);
                thread::sleep(next.saturating_duration_since(last));
            }
        };
        if per_second > 0 {
            let _ = thread::Builder::new()
                .name("sim-motion".into())
                .spawn(moving);
        }
        gap
    }

    /// A passive identity report, which pairs `pid` on the lane if it is new.
    pub fn identity(&self, receiver: usize, lane: Lane, pid: u16, arg0: u8) {
        self.with(receiver, |r| {
            let slot = &mut r.lanes[lane_index(lane)];
            if slot.as_ref().map(|d| d.pid) != Some(pid) {
                *slot = Some(SimDevice::new(pid, r.port));
            }
            let [high, low] = pid.to_be_bytes();
            match lane {
                Lane::Pointer => r.input(&[0x05, 0x3b, arg0, high, low]),
                Lane::Keyboard => r.input(&[0x09, 0x35, arg0, 0x00, high, low]),
            }
        });
    }

    /// Any input report out of interface 1.
    pub fn report(&self, receiver: usize, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.with(receiver, |r| r.input(bytes));
        }
    }

    /// The mouse goes on or off its mat, and as on hardware the receiver
    /// says nothing. A 07/84 query tells. Put on the mat, it reads not
    /// charging for `lag` first.
    pub fn mat(&self, receiver: usize, on: bool, lag: Duration) {
        self.with_device(receiver, Lane::Pointer, |d| {
            (d.power, d.charge) = (u8::from(on), u8::from(on));
            d.charging_from = on.then(|| Instant::now() + lag);
        });
    }

    /// Asleep devices answer every exchange with status 0x04.
    pub fn set_asleep(&self, receiver: usize, lane: Lane, asleep: bool) {
        self.with_device(receiver, lane, |d| d.asleep = asleep);
    }

    pub fn set_busy(&self, receiver: usize, lane: Lane, replies: u32) {
        self.with_device(receiver, lane, |d| d.busy = replies);
    }

    /// Reads before the answer is ready get busy for the request (as the
    /// keyboard's sometimes do on hardware), or the previous answer.
    pub fn set_busy_until_ready(&self, receiver: usize, lane: Lane, on: bool) {
        self.with_device(receiver, lane, |d| d.busy_until_ready = on);
    }

    /// What the device answers its serial query with.
    pub fn set_serial(&self, receiver: usize, lane: Lane, serial: &str) {
        self.with_device(receiver, lane, |d| d.serial = serial.to_owned());
    }

    pub fn set_firmware(&self, receiver: usize, lane: Lane, firmware: [u8; 2]) {
        self.with_device(receiver, lane, |d| d.firmware = firmware);
    }

    /// The device answers these class and command pairs with 0x05.
    pub fn set_unsupported(&self, receiver: usize, lane: Lane, commands: &[(u8, u8)]) {
        self.with_device(receiver, lane, |d| d.unsupported = commands.to_vec());
    }

    /// The device's answers are ready after these microseconds in turn.
    pub fn set_answer_after(&self, receiver: usize, lane: Lane, us: &[u64]) {
        self.with_device(receiver, lane, |d| d.answer_after = us.to_vec());
    }

    /// The device doesn't answer the next `requests` requests.
    pub fn set_silent(&self, receiver: usize, lane: Lane, requests: u32) {
        self.with_device(receiver, lane, |d| d.silent = requests);
    }

    /// The next `requests` answers carry the other lane's selector.
    pub fn misroute(&self, receiver: usize, lane: Lane, requests: u32) {
        self.with_device(receiver, lane, |d| d.misroute = requests);
    }

    /// The next SET to the receiver fails with `errno`.
    pub fn fail_next(&self, receiver: usize, errno: Errno) {
        self.with(receiver, |r| r.fail_set = Some(errno));
    }

    /// The requests the lane has received, as they arrived (routed).
    #[must_use]
    pub fn requests(&self, receiver: usize, lane: Lane) -> Vec<Report> {
        self.with_device(receiver, lane, |d| d.requests.clone())
            .unwrap_or_default()
    }

    /// The requests that named no lane, as they arrived.
    #[must_use]
    pub fn unrouted(&self, receiver: usize) -> Vec<Report> {
        self.with(receiver, |r| r.unrouted.clone())
            .unwrap_or_default()
    }

    /// Whether the receiver answers the paired-slots query.
    pub fn set_slots(&self, receiver: usize, answers: bool) {
        self.with(receiver, |r| r.slots = answers);
    }

    /// Moves the inventory's marker, or with None leaves it out.
    pub fn set_inventory_marker(&self, receiver: usize, marker: Option<usize>) {
        self.with(receiver, |r| r.marker = marker);
    }

    pub fn unplug(&self, receiver: usize) {
        self.lock().unplug(receiver);
    }

    pub fn plug(&self, receiver: usize) {
        self.lock().plug(receiver);
    }

    /// Plugs the receiver in with hidraw nodes that refuse to open until
    /// [`Sim::grant_access`], as on hardware before udev has applied its rules.
    pub fn plug_without_access(&self, receiver: usize) {
        let mut world = self.lock();
        world.receivers[receiver].locked = true;
        world.plug(receiver);
    }

    /// A sysfs tree with OpenRazer's files, for a bridge started with
    /// `--sim` to make its OpenRazer devices and look for cable twins in.
    /// Set it before the bridge connects.
    pub fn set_sysfs(&self, dir: &Path) {
        self.lock().sysfs = Some(dir.to_owned());
    }

    /// A kernel uevent for that tree, such as a wired device's `bind`.
    pub fn uevent(&self, message: &str) {
        self.lock().tell(Some(message.to_owned()), 0);
    }

    /// The system sleeps for `ms` and wakes, as far as the bridge can tell.
    pub fn suspend(&self, ms: u64) {
        self.lock().tell(None, ms);
    }

    /// udev applies its rules: the nodes open, and with `announce` its
    /// event goes out too.
    pub fn grant_access(&self, receiver: usize, announce: bool) {
        let mut world = self.lock();
        world.receivers[receiver].locked = false;
        if announce {
            world.announce();
        }
    }

    // ---------------------------------------------------- the socket

    /// Accepts connections on `path` in background threads.
    ///
    /// # Errors
    /// Fails when the socket cannot be bound.
    pub fn serve(&self, path: &Path) -> io::Result<()> {
        let listener = UnixListener::bind(path)?;
        let sim = self.clone();
        thread::Builder::new()
            .name("sim-accept".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let sim = sim.clone();
                    let _ = thread::Builder::new()
                        .name("sim-conn".into())
                        .spawn(move || sim.connection(stream));
                }
            })?;
        Ok(())
    }

    fn connection(&self, mut stream: UnixStream) {
        let Ok(Some(first)) = read_frame(&mut stream) else {
            return;
        };
        match serde_json::from_slice::<Hello>(&first) {
            Ok(Hello::Watch) => self.watch(stream),
            Ok(Hello::Hidraw { node, input }) => self.hidraw(stream, &node, input),
            Err(_) => {}
        }
    }

    fn watch(&self, mut stream: UnixStream) {
        let mut world = self.lock();
        let note = serde_json::to_vec(&world.note(None, 0)).unwrap_or_default();
        if write_frame(&mut stream, &note).is_ok() {
            world.watchers.push(stream);
        }
    }

    fn hidraw(&self, mut stream: UnixStream, node: &str, input: bool) {
        let target = {
            let world = self.lock();
            world.hid_target(node).map(|(i, interface)| {
                let r = &world.receivers[i];
                (i, interface, r.generation, r.locked)
            })
        };
        let refuse = |stream: &mut UnixStream, errno: Errno| {
            let _ = stream.write_all(&errno.raw_os_error().to_ne_bytes());
        };
        let Some((i, interface, generation, locked)) = target else {
            return refuse(&mut stream, Errno::NODEV);
        };
        if locked {
            return refuse(&mut stream, Errno::ACCESS);
        }
        if stream.write_all(&0_i32.to_ne_bytes()).is_err() {
            return;
        }
        // An open file belongs to one plug-in, even when the name comes back.
        let same_plug = || self.with(i, |r| r.generation) == Some(generation);
        if input {
            // Interface 0's readers can't hold the simulator up.
            let pointer = interface == wire::IF_POINTER;
            if let (Ok(reader), Some(r)) = (stream.try_clone(), self.lock().receivers.get_mut(i))
                && (!pointer || reader.set_nonblocking(true).is_ok())
            {
                r.inputs.push((interface, reader));
            }
            return;
        }
        // Feature transfers: [op][u16 length][buffer] in, [i32 errno][u16 length][buffer] out.
        let mut head = [0_u8; 3];
        while stream.read_exact(&mut head).is_ok() {
            let mut buffer = vec![0_u8; usize::from(u16::from_le_bytes([head[1], head[2]]))];
            if stream.read_exact(&mut buffer).is_err() || !same_plug() {
                return;
            }
            let result = self.with(i, |r| match (head[0], interface, buffer.len()) {
                (OP_SET, wire::IF_POINTER, 91) if buffer[0] == 0 => r
                    .set(buffer[1..].try_into().expect("90 bytes"))
                    .map(|()| Vec::new()),
                (OP_GET, wire::IF_POINTER, 91) if buffer[0] == 0 => r.get().map(|answer| {
                    let mut out = vec![0];
                    out.extend_from_slice(&answer);
                    out
                }),
                (OP_GET, wire::IF_INVENTORY, wire::INVENTORY_LEN)
                    if buffer[0] == wire::INVENTORY_REPORT_ID =>
                {
                    Ok(r.inventory())
                }
                _ => Err(Errno::PIPE),
            });
            let result = result.unwrap_or(Err(Errno::NODEV));
            let (errno, body) = match result {
                Ok(body) => (0, body),
                Err(e) => (e.raw_os_error(), Vec::new()),
            };
            let mut reply = errno.to_ne_bytes().to_vec();
            reply.extend_from_slice(&u16::try_from(body.len()).unwrap_or(0).to_le_bytes());
            reply.extend_from_slice(&body);
            if stream.write_all(&reply).is_err() {
                return;
            }
        }
    }
}

// ------------------------------------------------------------ bridge side

fn connect(path: &Path, hello: &Hello) -> io::Result<UnixStream> {
    let mut stream = UnixStream::connect(path)?;
    let hello = serde_json::to_vec(hello).map_err(io::Error::other)?;
    write_frame(&mut stream, &hello).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(stream)
}

/// Reads whole length-prefixed records off a non-blocking stream and keeps
/// a split record for the next call.
struct Buffered {
    stream: UnixStream,
    partial: Vec<u8>,
    closed: bool,
}

impl Buffered {
    fn fill(&mut self) {
        let mut chunk = [0_u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    self.closed = true;
                    return;
                }
                Ok(n) => self.partial.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(_) => {
                    self.closed = true;
                    return;
                }
            }
        }
    }
}

/// Takes the complete length-prefixed JSON frames out of a buffer.
fn frames(buffered: &mut Buffered) -> Vec<Note> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(len) = buffered.partial.get(at..at + 4) {
        let len = u32::from_be_bytes(len.try_into().expect("4 bytes")) as usize;
        let Some(payload) = buffered.partial.get(at + 4..at + 4 + len) else {
            break;
        };
        out.extend(serde_json::from_slice(payload).ok());
        at += 4 + len;
    }
    buffered.partial.drain(..at);
    out
}

/// The simulated receivers' hidraw interfaces, for the bridge.
pub struct SimHidraw {
    path: PathBuf,
    /// Dropped when the simulator hangs up, which stops the loop polling it.
    watch: Option<Buffered>,
    found: Vec<Found>,
    sysfs: Option<PathBuf>,
    /// Passes the simulator's uevents on to [`SimHidraw::openrazer`]'s.
    uevents: Option<UnixDatagram>,
    /// Adds the simulator's suspends to what the bridge sees.
    slept: Arc<AtomicU64>,
}

fn found(receivers: Vec<(String, BTreeMap<u8, String>)>) -> Vec<Found> {
    receivers
        .into_iter()
        .map(|(usb_path, nodes)| Found { usb_path, nodes })
        .collect()
}

impl SimHidraw {
    /// # Errors
    /// Fails when the simulator is not listening.
    pub fn connect(path: &Path, slept: Arc<AtomicU64>) -> io::Result<Self> {
        let mut stream = connect(path, &Hello::Watch)?;
        let frame = read_frame(&mut stream)
            .map_err(|e| io::Error::other(e.to_string()))?
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        let note: Note = serde_json::from_slice(&frame).map_err(io::Error::other)?;
        stream.set_nonblocking(true)?;
        Ok(Self {
            path: path.to_owned(),
            watch: Some(Buffered {
                stream,
                partial: Vec::new(),
                closed: false,
            }),
            found: found(note.receivers),
            sysfs: note.sysfs,
            uevents: None,
            slept,
        })
    }

    /// OpenRazer as the simulator offers it, if it does.
    pub fn openrazer(&mut self) -> Option<OpenRazerEnv> {
        let sysfs = self.sysfs.as_ref()?;
        let (ours, theirs) = UnixDatagram::pair().ok()?;
        theirs.set_nonblocking(true).ok()?;
        self.uevents = Some(ours);
        Some(OpenRazerEnv {
            sys_module: sysfs.join("module"),
            hid_drivers: sysfs.join("drivers"),
            uevents: Some(OwnedFd::from(theirs)),
        })
    }

    /// The sysfs tree with OpenRazer's files, if the simulator offers one.
    #[must_use]
    pub fn sysfs(&self) -> Option<&Path> {
        self.sysfs.as_deref()
    }

    fn open(&self, node: &str, input: bool) -> Result<UnixStream, Errno> {
        let node = node.to_owned();
        let mut stream =
            connect(&self.path, &Hello::Hidraw { node, input }).map_err(|_| Errno::NODEV)?;
        let mut errno = [0_u8; 4];
        stream.read_exact(&mut errno).map_err(|_| Errno::NODEV)?;
        match i32::from_ne_bytes(errno) {
            0 => Ok(stream),
            e => Err(Errno::from_raw_os_error(e)),
        }
    }
}

impl HidHost for SimHidraw {
    fn scan(&mut self) -> io::Result<Vec<Found>> {
        Ok(self.found.clone())
    }

    fn watch_fd(&self) -> Option<BorrowedFd<'_>> {
        self.watch.as_ref().map(|w| w.stream.as_fd())
    }

    fn drain(&mut self) -> bool {
        let Some(watch) = self.watch.as_mut() else {
            return false;
        };
        watch.fill();
        let mut changed = false;
        for note in frames(watch) {
            self.found = found(note.receivers);
            changed = true;
            if let (Some(uevent), Some(to)) = (note.uevent, &self.uevents) {
                let _ = to.send(uevent.as_bytes());
            }
            self.slept.fetch_add(note.slept_ms, Ordering::Relaxed);
        }
        if watch.closed {
            eprintln!("simulator connection closed");
            self.watch = None;
            changed |= !self.found.is_empty();
            self.found.clear();
        }
        changed
    }

    fn open_input(&mut self, node: &str) -> Result<Box<dyn HidInput>, Errno> {
        let stream = self.open(node, true)?;
        stream.set_nonblocking(true).map_err(|_| Errno::NODEV)?;
        Ok(Box::new(Buffered {
            stream,
            partial: Vec::new(),
            closed: false,
        }))
    }

    fn open_feature(&mut self, node: &str) -> Result<Box<dyn HidFeature>, Errno> {
        Ok(Box::new(SimFeature(self.open(node, false)?)))
    }
}

impl HidInput for Buffered {
    fn fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Errno> {
        self.fill();
        let len = self
            .partial
            .get(..2)
            .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])));
        let Some(len) = len.filter(|len| self.partial.len() >= 2 + len) else {
            return Err([Errno::AGAIN, Errno::NODEV][usize::from(self.closed)]);
        };
        let n = len.min(buffer.len());
        buffer[..n].copy_from_slice(&self.partial[2..2 + n]);
        self.partial.drain(..2 + len);
        Ok(n)
    }
}

struct SimFeature(UnixStream);

impl SimFeature {
    fn call(&mut self, op: u8, buffer: &[u8]) -> Result<Vec<u8>, Errno> {
        let mut request = vec![op];
        request.extend_from_slice(&u16::try_from(buffer.len()).unwrap_or(0).to_le_bytes());
        request.extend_from_slice(buffer);
        self.0.write_all(&request).map_err(|_| Errno::NODEV)?;
        let mut head = [0_u8; 6];
        self.0.read_exact(&mut head).map_err(|_| Errno::NODEV)?;
        let mut body = vec![0_u8; usize::from(u16::from_le_bytes([head[4], head[5]]))];
        self.0.read_exact(&mut body).map_err(|_| Errno::NODEV)?;
        match i32::from_ne_bytes(head[..4].try_into().expect("4 bytes")) {
            0 => Ok(body),
            e => Err(Errno::from_raw_os_error(e)),
        }
    }
}

impl HidFeature for SimFeature {
    fn set(&mut self, report: &[u8]) -> Result<(), Errno> {
        self.call(OP_SET, report).map(drop)
    }

    fn get(&mut self, buffer: &mut [u8]) -> Result<usize, Errno> {
        let body = self.call(OP_GET, buffer)?;
        let n = body.len().min(buffer.len());
        buffer[..n].copy_from_slice(&body[..n]);
        Ok(n)
    }
}

// ------------------------------------------------------------ uhid

/// A battery device as the stand-in for /dev/uhid sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimBattery {
    /// The order the keeper opened it in, which tells it from the device
    /// that replaces it on its lane, with the same phys.
    pub id: u64,
    pub name: String,
    pub uniq: String,
    pub vendor: u32,
    pub keyboard: bool,
    pub level: Option<u8>,
    pub charging: bool,
    /// Battery reports taken, and dropped for arriving too early.
    pub reports: u32,
    pub dropped: u32,
}

/// The kernel sends UHID_START from hid_hw_start(), before hid-input has
/// registered the power supply, and drops input reports until then. On a
/// real machine that takes a few milliseconds.
const CONNECTED_AFTER_START: Duration = Duration::from_millis(20);

/// Battery devices by phys.
pub type Batteries = Arc<Mutex<BTreeMap<String, SimBattery>>>;
/// Other devices, with the CREATE2 event already read from them.
pub type Forward = std::sync::mpsc::Sender<(Vec<u8>, OwnedFd)>;

/// A stand-in for /dev/uhid: battery devices are started and followed in
/// `Batteries` (and logged when `log` is set). Any other device goes to
/// `forward`, for a test to play the kernel, or without one is started and
/// left alone.
#[must_use]
pub fn local_uhid(forward: Option<Forward>, log: bool) -> (UhidOpener, Batteries) {
    let batteries: Batteries = Arc::default();
    let kept = Arc::clone(&batteries);
    let opened = AtomicU64::new(0);
    let opener: UhidOpener = Arc::new(move || {
        let (ours, kernel) = rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::CLOEXEC,
            None,
        )?;
        rustix::io::ioctl_fionbio(&ours, true)?;
        let batteries = Arc::clone(&kept);
        let forward = forward.clone();
        let id = opened.fetch_add(1, Ordering::Relaxed);
        thread::Builder::new()
            .name("sim-uhid".into())
            .spawn(move || play_uhid(&kernel, &batteries, forward, log, id))?;
        Ok(ours)
    });
    (opener, batteries)
}

fn play_uhid(kernel: &OwnedFd, shared: &Batteries, forward: Option<Forward>, log: bool, id: u64) {
    let mut buffer = vec![0_u8; uhid::EVENT_BYTES + 1];
    let Ok(n) = rustix::io::read(kernel, &mut buffer) else {
        return;
    };
    let Some(uhid::Written::Create {
        name,
        phys,
        uniq,
        vendor,
        descriptor,
        ..
    }) = uhid::decode_written(&buffer[..n])
    else {
        return;
    };
    if !phys.starts_with(crate::battery::PHYS_PREFIX) {
        if let (Some(forward), Ok(fd)) = (forward, kernel.try_clone()) {
            let _ = forward.send((buffer[..n].to_vec(), fd));
            return;
        }
        // Without a test to play the kernel: a driver binds it and waits.
        let _ = rustix::io::write(kernel, &uhid::kernel_start(0));
        while let Ok(n) = rustix::io::read(kernel, &mut buffer) {
            if matches!(
                uhid::decode_written(&buffer[..n]),
                Some(uhid::Written::Destroy) | None
            ) {
                break;
            }
        }
        return;
    }
    let battery = SimBattery {
        id,
        name,
        uniq,
        vendor,
        keyboard: descriptor.get(2..4) == Some(&[0x09, 0x06]),
        level: None,
        charging: false,
        reports: 0,
        dropped: 0,
    };
    let lock = || shared.lock().unwrap_or_else(PoisonError::into_inner);
    let mut all = lock();
    // The keeper destroyed this device before it opened the successor, but
    // the successor's thread may have got here first.
    if all.get(&phys).is_none_or(|b| b.id < id) {
        all.insert(phys.clone(), battery);
    }
    drop(all);
    // Timed from before START goes out: this thread waking up late must not
    // make the keeper's resends count as early too.
    let started = Instant::now();
    let _ = rustix::io::write(kernel, &uhid::kernel_start(0));
    while let Ok(n) = rustix::io::read(kernel, &mut buffer) {
        let mut all = lock();
        let Some(b) = all.get_mut(&phys).filter(|b| b.id == id) else {
            break;
        };
        match uhid::decode_written(&buffer[..n]) {
            Some(uhid::Written::Input(report)) if report.len() == 3 && report[0] == 2 => {
                if started.elapsed() < CONNECTED_AFTER_START {
                    b.dropped += 1;
                    continue;
                }
                b.reports += 1;
                (b.level, b.charging) = (Some(report[1]), report[2] & 1 != 0);
                if log {
                    let charging = if b.charging { " charging" } else { "" };
                    eprintln!("simulated battery {}: {}%{charging}", b.name, report[1]);
                }
            }
            Some(uhid::Written::Destroy) | None => break,
            _ => {}
        }
    }
    // Its successor may be there already, under the same phys.
    lock().retain(|p, b| *p != phys || b.id != id);
}

// ------------------------------------------------------------ commands

pub const HELP: &str = "\
Receivers are numbered from 1; LANE is mouse or keyboard.

  list                          show receivers and devices
  status R LANE PERCENT [charging|discharging|cable]
                                send a passive status report
  activity R LANE               input on a lane
  move R N                      the mouse moves: N reports a second, 0 stops
  sleep R LANE | wake R LANE    make a device answer 0x04, or stop doing so
  pair R LANE PID               announce another product id (hex) on a lane
  busy R LANE N                 answer the next N requests with busy
  early R LANE busy|old         what a read gets before the answer is ready
  silent R LANE N               don't answer the next N requests
  unplug R | plug R             remove the receiver, or plug it in again
  suspend                       the system sleeps for 5 s
  report R HEX                  any input report out of interface 1
  help                          this text
  quit                          exit";

fn parse_lane(word: &str) -> Option<Lane> {
    match word {
        "mouse" | "pointer" => Some(Lane::Pointer),
        "keyboard" => Some(Lane::Keyboard),
        _ => None,
    }
}

/// The level byte for a percentage, as the device would report it.
#[must_use]
pub fn raw_level(percent: u8) -> u8 {
    u8::try_from((u16::from(percent.min(100)) * 255 + 50) / 100).unwrap_or(255)
}

impl Sim {
    /// Runs one command line. `Ok(false)` means quit.
    ///
    /// # Errors
    /// A message for a command that doesn't parse.
    pub fn command(&self, line: &str) -> Result<bool, String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some((&verb, args)) = words.split_first() else {
            return Ok(true);
        };
        let receiver = || -> Result<usize, String> {
            let n: usize = args
                .first()
                .and_then(|a| a.parse().ok())
                .ok_or("expected a receiver number")?;
            (1..=self.receivers())
                .contains(&n)
                .then(|| n - 1)
                .ok_or_else(|| format!("no receiver {n}"))
        };
        let lane = || {
            args.get(1)
                .and_then(|w| parse_lane(w))
                .ok_or("expected mouse or keyboard")
        };
        let number = |i: usize| -> Result<u32, String> {
            args.get(i)
                .and_then(|a| a.parse().ok())
                .ok_or_else(|| "expected a number".into())
        };
        match verb {
            "help" => println!("{HELP}"),
            "quit" | "exit" => return Ok(false),
            "list" => print!("{}", self.describe()),
            "status" => {
                let (r, lane) = (receiver()?, lane()?);
                let percent = u8::try_from(number(2)?.min(100)).unwrap_or(100);
                let (power, charge) = match (args.get(3).copied(), lane) {
                    (Some("charging"), Lane::Pointer) => (1, 1),
                    (Some("cable"), Lane::Pointer) => (6, 1),
                    (Some("charging" | "cable"), Lane::Keyboard) => (1, 1),
                    (Some("discharging") | None, _) => (0, 0),
                    _ => return Err("expected charging, discharging or cable".into()),
                };
                self.status(r, lane, raw_level(percent), power, charge);
            }
            "activity" => self.activity(receiver()?, lane()?),
            "move" => {
                self.set_motion(receiver()?, number(1)?);
            }
            "sleep" => self.set_asleep(receiver()?, lane()?, true),
            "wake" => {
                let (r, lane) = (receiver()?, lane()?);
                self.set_asleep(r, lane, false);
                self.activity(r, lane);
            }
            "pair" => {
                let (r, lane) = (receiver()?, lane()?);
                let pid = args
                    .get(2)
                    .and_then(|p| u16::from_str_radix(p.trim_start_matches("0x"), 16).ok())
                    .ok_or("expected a hex product id")?;
                self.identity(r, lane, pid, 1);
            }
            "busy" => self.set_busy(receiver()?, lane()?, number(2)?),
            "early" => match args.get(2).copied() {
                Some(how @ ("busy" | "old")) => {
                    self.set_busy_until_ready(receiver()?, lane()?, how == "busy");
                }
                _ => return Err("expected busy or old".into()),
            },
            "silent" => self.set_silent(receiver()?, lane()?, number(2)?),
            "unplug" => self.unplug(receiver()?),
            "suspend" => self.suspend(5_000),
            "plug" => self.plug(receiver()?),
            "report" => {
                let r = receiver()?;
                let bytes = args
                    .get(1)
                    .and_then(|h| hfx_proto::ids::decode_lower_hex(h))
                    .ok_or("expected lowercase hex")?;
                self.report(r, &bytes);
            }
            other => return Err(format!("unknown command {other}; try help")),
        }
        Ok(true)
    }

    fn describe(&self) -> String {
        let world = self.lock();
        let mut out = String::new();
        for r in &world.receivers {
            let state = if r.generation.is_some() {
                "plugged in"
            } else {
                "unplugged"
            };
            out.push_str(&format!("receiver {}: {state}\n", r.port));
            for (i, device) in r.lanes.iter().enumerate() {
                let Some(d) = device else { continue };
                out.push_str(&format!(
                    "  {}: 1532:{:04x}{}, level byte {}, {} requests\n",
                    ["mouse", "keyboard"][i],
                    d.pid,
                    if d.asleep { ", asleep" } else { "" },
                    d.level,
                    d.asked
                ));
            }
        }
        out
    }
}
