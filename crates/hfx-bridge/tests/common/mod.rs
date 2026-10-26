// SPDX-License-Identifier: GPL-2.0-only

//! A simulator, a bridge and a device keeper on temporary sockets, plus the
//! kernel side of the virtual HID devices.

#![allow(dead_code)]

use hfx_bridge::platform::{OpenRazerEnv, UhidOpener};
use hfx_bridge::sim::{Batteries, Sim};
use hfx_bridge::{Config, Source};
use hfx_proto::client::{Client, ClientConfig};
use hfx_proto::ids::{LeaseDurationMs, LeaseId, RouteToken, Sequence, WaitTimeoutMs};
use hfx_proto::messages::{Charge, DeviceInfo, Lane, LeaseOutcome, Presence, Route, Snapshot};
use rustix::fd::OwnedFd;
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// What OpenRazer tests get: the virtual devices the bridge creates (with
/// their CREATE2 event), a uevent socket and the temporary sysfs tree.
pub struct OpenRazer {
    pub devices: mpsc::Receiver<(Vec<u8>, OwnedFd)>,
    pub uevents: UnixDatagram,
    pub sys: tempfile::TempDir,
}

/// A daemon running on a thread of the test, as its process would.
pub struct Running {
    stop: UnixStream,
    thread: Option<JoinHandle<Result<(), String>>>,
    /// The thread's name, unique in the process.
    name: String,
}

impl Running {
    pub fn start(
        what: &str,
        run: impl FnOnce(OwnedFd) -> Result<(), String> + Send + 'static,
    ) -> Self {
        static STARTED: AtomicUsize = AtomicUsize::new(0);
        let name = format!("{what}-{}", STARTED.fetch_add(1, Ordering::Relaxed));
        let (stop_read, stop) = UnixStream::pair().unwrap();
        stop_read.set_nonblocking(true).unwrap();
        let thread = thread::Builder::new()
            .name(name.clone())
            .spawn(move || run(OwnedFd::from(stop_read)))
            .unwrap();
        Self {
            stop,
            thread: Some(thread),
            name,
        }
    }

    pub fn stop(&mut self) -> Result<(), String> {
        let _ = std::io::Write::write_all(&mut self.stop, b"x");
        self.thread.take().map_or(Ok(()), |t| t.join().unwrap())
    }

    /// How often the thread has slept and woken up so far.
    fn wakeups(&self) -> u64 {
        let tasks = std::fs::read_dir("/proc/self/task").unwrap().flatten();
        let ours = tasks.map(|t| t.path()).find(|t| {
            std::fs::read_to_string(t.join("comm")).is_ok_and(|c| c.trim_end() == self.name)
        });
        let status = std::fs::read_to_string(ours.unwrap().join("status")).unwrap();
        let line = status
            .lines()
            .find_map(|l| l.strip_prefix("voluntary_ctxt_switches:"));
        line.unwrap().trim().parse().unwrap()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub struct Harness {
    pub sim: Sim,
    pub dir: tempfile::TempDir,
    pub socket: PathBuf,
    pub batteries: Batteries,
    pretend: Arc<AtomicU64>,
    /// What a bridge or keeper started again needs.
    sim_socket: PathBuf,
    uhid: UhidOpener,
    drivers: Option<PathBuf>,
    read_only: bool,
    state_dir: Option<PathBuf>,
    /// Hellos the keepers got from a bridge.
    pub connected: Arc<AtomicU64>,
    keeper: Option<Running>,
    bridge: Option<Running>,
}

/// Something to do to the simulator before the bridge starts.
pub type Before = Box<dyn FnOnce(&Sim)>;

pub struct Options {
    pub receivers: usize,
    pub read_only: bool,
    pub openrazer: bool,
    /// Opening /dev/uhid fails this many times first, as it can at boot.
    pub refuse_uhid: u32,
    /// Runs on the simulator before the bridge starts.
    pub before: Option<Before>,
    /// Runs on the temporary sysfs tree before the bridge starts.
    pub sysfs: Option<fn(&Path)>,
    /// Where the bridge keeps its learned waits.
    pub state_dir: Option<PathBuf>,
    /// The uid the bridge lets the keeper run as. None is the test's own.
    pub keeper_uid: Option<u32>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            receivers: 1,
            read_only: false,
            openrazer: false,
            refuse_uhid: 0,
            before: None,
            sysfs: None,
            state_dir: None,
            keeper_uid: None,
        }
    }
}

impl Options {
    /// The defaults, with `before` run on the simulator first.
    pub fn before(before: impl FnOnce(&Sim) + 'static) -> Self {
        let before: Before = Box::new(before);
        Self {
            before: Some(before),
            ..Self::default()
        }
    }
}

/// A temporary sysfs tree with both patched drivers loaded.
pub fn sysfs() -> tempfile::TempDir {
    let sys = tempfile::tempdir().unwrap();
    for driver in ["razermouse", "razerkbd"] {
        let params = sys.path().join("module").join(driver).join("parameters");
        std::fs::create_dir_all(&params).unwrap();
        std::fs::write(
            params.join("hyperflux_native_transport"),
            "razer-feature-report-v1\n",
        )
        .unwrap();
        let dir = sys.path().join("drivers").join(driver);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("unbind"), "").unwrap();
    }
    sys
}

/// A battery as the desktop would show it.
#[derive(Debug, PartialEq, Eq)]
pub struct Shown {
    pub capacity: u8,
    pub charging: bool,
    pub name: String,
}

impl Harness {
    pub fn start() -> Self {
        Self::with(Options::default()).0
    }

    pub fn with(options: Options) -> (Self, Option<OpenRazer>) {
        let dir = tempfile::tempdir().unwrap();
        let sim_socket = dir.path().join("sim.sock");
        let socket = dir.path().join("bridge.sock");
        let sim = Sim::new(options.receivers);
        if let Some(before) = options.before {
            before(&sim);
        }
        sim.serve(&sim_socket).unwrap();
        let (forward, devices) = mpsc::channel();
        let (uhid, batteries) = hfx_bridge::sim::local_uhid(Some(forward), false);
        let uhid = refusing(uhid, options.refuse_uhid);
        let (env, openrazer) = if options.openrazer {
            let sys = sysfs();
            if let Some(prepare) = options.sysfs {
                prepare(sys.path());
            }
            let (ours, theirs) = UnixDatagram::pair().unwrap();
            ours.set_nonblocking(true).unwrap();
            let env = OpenRazerEnv {
                sys_module: sys.path().join("module"),
                hid_drivers: sys.path().join("drivers"),
                uevents: Some(OwnedFd::from(ours)),
            };
            (
                Some(env),
                Some(OpenRazer {
                    devices,
                    uevents: theirs,
                    sys,
                }),
            )
        } else {
            (None, None)
        };
        let mut h = Self {
            sim,
            dir,
            socket,
            batteries,
            pretend: Arc::new(AtomicU64::new(0)),
            sim_socket,
            uhid,
            drivers: openrazer.as_ref().map(|o| o.sys.path().join("drivers")),
            read_only: options.read_only,
            state_dir: options.state_dir,
            connected: Arc::default(),
            keeper: None,
            bridge: None,
        };
        let keeper_uid = options.keeper_uid.unwrap_or_else(own_uid);
        h.start_bridge(env, keeper_uid);
        h.start_keeper();
        // The keeper reads the receivers before it reaches the bridge, so
        // after this nothing the simulator sends is lost.
        if options.keeper_uid.is_none() {
            wait_for(5, || h.connected.load(Ordering::Relaxed) > 0);
        }
        (h, openrazer)
    }

    fn start_bridge(&mut self, openrazer: Option<OpenRazerEnv>, keeper_uid: u32) {
        let config = Config {
            socket: self.socket.clone(),
            system: false,
            source: Source::Sim(self.sim_socket.clone()),
            read_only: self.read_only,
            keeper_uid,
            openrazer,
            sim_openrazer: false,
            pretend_suspend: Some(Arc::clone(&self.pretend)),
            state_dir: self.state_dir.clone(),
        };
        self.bridge = Some(Running::start("bridge", move |stop| {
            hfx_bridge::run(config, stop)
        }));
        let deadline = Instant::now() + Duration::from_secs(5);
        while UnixStream::connect(&self.socket).is_err() {
            assert!(Instant::now() < deadline, "the bridge didn't start");
            thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn start_keeper(&mut self) {
        let config = hfx_bridge::keeper::Config {
            source: Source::Sim(self.sim_socket.clone()),
            socket: hfx_bridge::keeper_socket(&self.socket),
            bridge_uid: own_uid(),
            uhid: Arc::clone(&self.uhid),
            hid_drivers: self.drivers.clone(),
            connected: Some(Arc::clone(&self.connected)),
        };
        self.keeper = Some(Running::start("keeper", move |stop| {
            hfx_bridge::keeper::run(config, stop)
        }));
    }

    /// Stops the keeper, which takes its devices with it.
    pub fn stop_keeper(&mut self) -> Result<(), String> {
        self.keeper.take().map_or(Ok(()), |mut k| k.stop())
    }

    /// Stops the bridge and starts another, without OpenRazer's uevents.
    pub fn restart_bridge(&mut self) {
        self.bridge.take().unwrap().stop().unwrap();
        let openrazer = self.drivers.as_ref().map(|drivers| OpenRazerEnv {
            sys_module: drivers.with_file_name("module"),
            hid_drivers: drivers.clone(),
            uevents: None,
        });
        self.start_bridge(openrazer, own_uid());
    }

    /// A client with these features, and `wait` for [`until`].
    pub fn client(&self, features: &[&str]) -> Client {
        let optional: &[&str] = if features.contains(&"wait") {
            &[]
        } else {
            &["wait"]
        };
        let mut config = ClientConfig::new("test", "Test", features, optional);
        config.timeout = Duration::from_secs(5);
        Client::connect(&self.socket, &config).unwrap()
    }

    /// How often the event loops of the bridge and the keeper have slept
    /// and woken up so far.
    pub fn wakeups(&self) -> u64 {
        let running = self.bridge.iter().chain(&self.keeper);
        running.map(Running::wakeups).sum()
    }

    /// Makes the bridge believe the system slept for five seconds.
    pub fn sleep_and_wake(&self) {
        self.pretend.fetch_add(5_000, Ordering::Relaxed);
    }

    /// The battery the desktop shows for a lane: the level its virtual
    /// battery device last reported.
    pub fn shown(&self, receiver: usize, lane: Lane) -> Option<Shown> {
        let id = hfx_bridge::bridge::receiver_id(&self.sim.usb_path(receiver), 0x1532, 0x00cf);
        let lane = match lane {
            Lane::Pointer => "pointer",
            Lane::Keyboard => "keyboard",
        };
        let phys = format!(
            "hyperflux-next/battery/{}/{lane}",
            id.trim_start_matches("receiver-")
        );
        let batteries = self.batteries.lock().unwrap();
        let b = batteries.get(&phys)?;
        Some(Shown {
            capacity: b.level?,
            charging: b.charging,
            name: b.name.clone(),
        })
    }

    /// Stops the bridge. The keeper stays and drops the bridge's devices.
    pub fn stop(mut self) -> Result<(), String> {
        self.bridge.take().unwrap().stop()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // The bridge first: the keeper drops its devices on the hang-up.
        self.bridge.take();
        self.keeper.take();
    }
}

pub fn own_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// `open`, failing with EHOSTDOWN the first `times` times.
fn refusing(open: UhidOpener, times: u32) -> UhidOpener {
    let left = Mutex::new(times);
    Arc::new(move || {
        let mut left = left.lock().unwrap();
        if *left > 0 {
            *left -= 1;
            return Err(rustix::io::Errno::HOSTDOWN.into());
        }
        drop(left);
        open()
    })
}

/// Waits (through the protocol's `wait`) until `check` holds for a snapshot.
pub fn until(client: &mut Client, seconds: u64, check: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        let snapshot = client.snapshot().unwrap();
        if check(&snapshot) {
            return snapshot;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "condition not reached; last snapshot: {snapshot:#?}"
        );
        let ms = u32::try_from(left.as_millis().min(1000)).unwrap();
        client
            .wait(
                Some(snapshot.revision),
                None,
                WaitTimeoutMs::new(ms).unwrap(),
            )
            .unwrap();
    }
}

pub fn device(snapshot: &Snapshot, receiver: usize, lane: Lane) -> Option<&DeviceInfo> {
    snapshot
        .receivers
        .get(receiver)?
        .devices
        .iter()
        .find(|d| d.lane == lane)
}

pub fn presence_of(snapshot: &Snapshot, receiver: usize, lane: Lane) -> Option<Presence> {
    Some(device(snapshot, receiver, lane)?.presence)
}

pub fn charge(snapshot: &Snapshot, receiver: usize, lane: Lane) -> Option<Charge> {
    device(snapshot, receiver, lane)?.battery.charge
}

pub fn level(snapshot: &Snapshot, receiver: usize, lane: Lane) -> Option<u8> {
    device(snapshot, receiver, lane)?
        .battery
        .level
        .map(|l| l.get())
}

pub fn route(client: &mut Client, lane: Lane) -> Route {
    client
        .list_routes()
        .unwrap()
        .routes
        .into_iter()
        .find(|r| r.lane == lane)
        .expect("a route for the lane")
}

/// Waits for the lane's route token, once the start-up battery probe is
/// done with the device.
pub fn token(client: &mut Client, lane: Lane) -> RouteToken {
    until(client, 5, |s| {
        device(s, 0, lane).is_some_and(|d| {
            d.controllable && d.controlled_by.is_none() && d.battery.level.is_some()
        })
    });
    route(client, lane)
        .route_token
        .expect("a token on a controllable route")
}

pub fn lease(client: &mut Client, token: &RouteToken) -> LeaseId {
    let result = client
        .acquire_lease(token, LeaseDurationMs::new(30_000).unwrap())
        .unwrap();
    assert_eq!(result.outcome, LeaseOutcome::Granted, "{result:?}");
    result.lease_id.unwrap()
}

/// A client with `features`, holding a lease on the lane's route.
pub fn leased(h: &Harness, features: &[&str], lane: Lane) -> (Client, RouteToken, LeaseId) {
    let mut client = h.client(features);
    let token = token(&mut client, lane);
    let lease = lease(&mut client, &token);
    (client, token, lease)
}

pub fn seq(n: u64) -> Sequence {
    Sequence::new(n).unwrap()
}

pub fn wait_for(seconds: u64, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !check() {
        assert!(Instant::now() < deadline, "condition not reached");
        thread::sleep(Duration::from_millis(5));
    }
}
