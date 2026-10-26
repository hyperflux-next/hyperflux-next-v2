// SPDX-License-Identifier: GPL-2.0-only

//! A long run under random events, ignored by default and driven by the
//! weekly workflow. The bridge and keeper binaries run on their own, with
//! `--sim`, against a simulator in this process, so what gets measured is the
//! two of them. The storm sleeps and wakes devices and the system, replugs
//! the receiver, brings cable twins in and out, moves the mouse, sends
//! status reports, has an app transact under a lease, benches and probes,
//! all at random, for a set time (HFX_SOAK_SECS, default 1800). Every 30 s
//! it samples each process's memory, threads and open files, and checks
//! that the bridge still answers and that its snapshot matches the
//! simulator. At the end neither's memory may have grown the way a leak
//! makes it grow, and the bridge has to go quiet once the storm stops.
//!
//! Run it with `cargo test -p hfx-bridge --test soak -- --ignored --nocapture`.

mod common;

use hfx_bridge::sim::Sim;
use hfx_proto::client::{BenchReads, Client, ClientConfig};
use hfx_proto::ids::{
    BenchIterations, ExchangeTimeoutMs, FirstGetDelayUs, LeaseDurationMs, ReceiverId,
};
use hfx_proto::messages::{BenchQuery, Lane, OpenRazerState};
use hfx_proto::report;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// SplitMix64: deterministic, and no dependency needed.
struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z ^ (z >> 31)) % bound
    }
}

fn soak_seconds() -> u64 {
    std::env::var("HFX_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1800)
}

/// What a process holds at one moment.
#[derive(Clone, Copy, Debug)]
struct Sample {
    secs: u64,
    rss_kb: u64,
    /// The heap and stacks, without the binary's pages, which come in as
    /// the storm reaches code it hadn't run yet.
    anon_kb: u64,
    threads: u64,
    fds: u64,
}

impl Sample {
    fn take(what: &str, pid: u32, since: Instant) -> Self {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        let field = |name: &str| -> u64 {
            let line = status.lines().find_map(|l| l.strip_prefix(name)).unwrap();
            line.split_whitespace().next().unwrap().parse().unwrap()
        };
        let fds = std::fs::read_dir(format!("/proc/{pid}/fd"))
            .unwrap()
            .count();
        let sample = Self {
            secs: since.elapsed().as_secs(),
            rss_kb: field("VmRSS:"),
            anon_kb: field("RssAnon:"),
            threads: field("Threads:"),
            fds: fds as u64,
        };
        let (m, s) = (sample.secs / 60, sample.secs % 60);
        eprintln!(
            "soak {m:3}:{s:02}  {what:6}  rss {} kB  anon {} kB  threads {}  fds {}",
            sample.rss_kb, sample.anon_kb, sample.threads, sample.fds
        );
        sample
    }
}

/// How fast `kb` grew over these samples, in kB an hour: the slope of a
/// least-squares line, which one high or low sample doesn't swing.
fn growth_per_hour(samples: &[Sample], kb: fn(&Sample) -> u64) -> f64 {
    let n = samples.len() as f64;
    let (t, m) = samples.iter().fold((0.0, 0.0), |(t, m), s| {
        (t + s.secs as f64 / n, m + kb(s) as f64 / n)
    });
    let (cov, var) = samples.iter().fold((0.0, 0.0), |(c, v), s| {
        let dt = s.secs as f64 - t;
        (c + dt * (kb(s) as f64 - m), v + dt * dt)
    });
    cov / var * 3600.0
}

/// The twin dir OpenRazer would make for the mouse on its cable.
const TWIN: &str = "razermouse/0003:1532:00CC.0042";

/// A daemon's process and its log. It goes with the test whatever happens,
/// and a failed test shows the end of the log.
struct Daemon(Child, PathBuf, &'static str);

impl Daemon {
    fn start(dir: &Path, what: &'static str, binary: &str, socket: &Path) -> Self {
        let log = dir.join(format!("{what}.log"));
        Command::new(binary)
            .arg("--sim")
            .arg(dir.join("sim.sock"))
            .arg("--socket")
            .arg(socket)
            .env("STATE_DIRECTORY", dir)
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .map(|child| Self(child, log, what))
            .unwrap()
    }

    /// SIGTERM, and a clean exit after it.
    fn stop(&mut self) {
        let pid = rustix::process::Pid::from_child(&self.0);
        rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
        let stopped = self.0.wait().unwrap();
        assert!(stopped.success(), "the {} didn't stop cleanly", self.2);
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        if std::thread::panicking() {
            let log = std::fs::read_to_string(&self.1).unwrap_or_default();
            let start = log.lines().count().saturating_sub(40);
            log.lines()
                .skip(start)
                .for_each(|l| eprintln!("{}: {l}", self.2));
        }
    }
}

struct Soak {
    sim: Sim,
    daemons: [Daemon; 2],
    drivers: PathBuf,
    c: Client,
    rng: Rng,
    timeout: ExchangeTimeoutMs,
    lease_ms: LeaseDurationMs,
    plugged: bool,
    twin: bool,
    moving: bool,
}

impl Soak {
    fn uevent(&self, action: &str) {
        let message =
            format!("{action}@/devices/x\0ACTION={action}\0SUBSYSTEM=hid\0DRIVER=razermouse\0");
        self.sim.uevent(&message);
    }

    /// One random event. Client calls that fail because a route is not ready
    /// just mean "try later". The point is that none of this panics or hangs.
    fn storm(&mut self) {
        let lane = [Lane::Pointer, Lane::Keyboard][self.rng.below(2) as usize];
        let coin = self.rng.below(2) == 0;
        match self.rng.below(10) {
            0 => self.sim.set_asleep(0, lane, true),
            1 => {
                self.sim.set_asleep(0, lane, false);
                self.sim.activity(0, lane);
            }
            2 => {
                let (power, charge) = (self.rng.below(2) as u8, self.rng.below(2) as u8);
                let level = self.rng.below(256) as u8;
                self.sim.status(0, lane, level, power, charge);
            }
            3 if lane == Lane::Pointer => {
                self.moving = !self.moving;
                self.sim.set_motion(0, if self.moving { 1000 } else { 0 });
            }
            3 => self.sim.activity(0, lane),
            4 => {
                self.plugged = !self.plugged;
                if self.plugged {
                    self.sim.plug(0);
                } else {
                    self.sim.unplug(0);
                }
            }
            5 => self.flip_twin(),
            6 if self.plugged => self.sim.suspend(5_000),
            7 | 8 if self.plugged => self.app_traffic(lane, coin),
            _ if self.plugged => {
                let id = hfx_bridge::bridge::receiver_id(&self.sim.usb_path(0), 0x1532, 0x00cf);
                let _ = self.c.probe_receiver(ReceiverId::new(id).unwrap());
            }
            _ => {}
        }
    }

    fn flip_twin(&mut self) {
        let twin = self.drivers.join(TWIN);
        if self.twin {
            let _ = std::fs::remove_dir_all(&twin);
            self.uevent("remove");
        } else {
            std::fs::create_dir_all(&twin).unwrap();
            // Another unit's serial hash, so the paired one's copy stays.
            let hash = format!("{}\n", "5a".repeat(32));
            std::fs::write(twin.join("serial_sha256"), hash).unwrap();
            self.uevent("bind");
        }
        self.twin = !self.twin;
    }

    /// An app leases `lane` and either transacts a query or benches, then
    /// lets the lease go.
    fn app_traffic(&mut self, lane: Lane, transact: bool) {
        let Ok(routes) = self.c.list_routes() else {
            return;
        };
        let route = routes.routes.into_iter().find(|r| r.lane == lane);
        let Some(token) = route.and_then(|r| r.route_token) else {
            return;
        };
        let lease = self.c.acquire_lease(&token, self.lease_ms);
        let Ok(Some(lease)) = lease.map(|r| r.lease_id) else {
            return;
        };
        if transact {
            let tx = if lane == Lane::Pointer { 0x1f } else { 0x9f };
            let query = report::build(tx, 0x00, 0x81, &[0, 0]);
            let _ = self.c.transact(&lease, &token, &query, self.timeout);
        } else {
            let reads = BenchReads {
                interval: None,
                reread_busy: true,
            };
            let delay = FirstGetDelayUs::new(4000).unwrap();
            let iterations = BenchIterations::new(5).unwrap();
            let query = BenchQuery::Battery;
            let _ = self
                .c
                .bench(&lease, &token, query, delay, iterations, reads);
        }
        let _ = self.c.release_lease(&lease);
    }

    /// After a phase: the bridge still answers, the snapshot matches whether
    /// the receiver is plugged in, and it has no more than its two OpenRazer
    /// devices.
    fn check(&mut self) {
        let snapshot = self.c.snapshot().expect("the bridge still answers");
        assert_eq!(snapshot.receivers.len(), usize::from(self.plugged));
        let devices = snapshot.receivers.iter().flat_map(|r| &r.devices);
        let mut live = 0;
        for d in devices {
            assert!(
                d.product_id == 0 || hfx_proto::devices::device(d.product_id).is_some(),
                "a device with an unknown id appeared"
            );
            let state = d.openrazer.as_ref().map(|o| o.state);
            live += usize::from(state == Some(OpenRazerState::Live));
        }
        assert!(live <= 2, "{live} OpenRazer devices live");
    }

    fn bring_to_rest(&mut self) {
        if !self.plugged {
            self.sim.plug(0);
            self.plugged = true;
        }
        if self.twin {
            self.flip_twin();
        }
        self.sim.set_motion(0, 0);
        for lane in [Lane::Pointer, Lane::Keyboard] {
            self.sim.set_asleep(0, lane, false);
            self.sim.activity(0, lane);
        }
    }
}

/// The bridge, then the keeper, which reaches it at once.
fn start(dir: &Path) -> [Daemon; 2] {
    let socket = dir.join("bridge.sock");
    let bridge = Daemon::start(
        dir,
        "bridge",
        env!("CARGO_BIN_EXE_hyperflux-next-bridge"),
        &socket,
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(&socket).is_err() {
        assert!(Instant::now() < deadline, "the bridge didn't start");
        std::thread::sleep(Duration::from_millis(20));
    }
    let keeper_socket = dir.join("keeper.sock");
    let keeper = Daemon::start(
        dir,
        "keeper",
        env!("CARGO_BIN_EXE_hyperflux-next-keeper"),
        &keeper_socket,
    );
    [bridge, keeper]
}

/// Grows a leak would show: after the first third the capture ring and the
/// maps are full, and a leak shows as a steady climb.
fn settled(what: &str, samples: &[Sample], end: &Sample) {
    let first = &samples[0];
    assert!(end.threads <= first.threads + 2, "{what}: {end:?}");
    assert!(end.fds <= first.fds + 4, "{what}: {end:?}");
    let settled = &samples[samples.len() / 3..];
    let growth = growth_per_hour(settled, |s| s.anon_kb);
    let rss = growth_per_hour(settled, |s| s.rss_kb);
    eprintln!(
        "soak done, {what}: {} samples, RSS {} -> {} kB, after the first third {rss:.0} kB an hour, {growth:.0} of it anonymous",
        samples.len(),
        first.rss_kb,
        end.rss_kb
    );
    assert!(
        growth < 1024.0,
        "{what}: memory grew by {growth:.0} kB an hour"
    );
}

#[test]
#[ignore = "long-running; the weekly workflow runs it"]
fn long_run_under_random_events() {
    let dir = tempfile::tempdir().unwrap();
    let sys = common::sysfs();
    let sim = Sim::new(1);
    sim.set_sysfs(sys.path());
    sim.serve(&dir.path().join("sim.sock")).unwrap();
    let daemons = start(dir.path());
    let features = ["native-routes", "bench", "bench-interval", "probe-receiver"];
    let mut config = ClientConfig::new("soak", "Soak", &features, &["wait"]);
    config.timeout = Duration::from_secs(5);
    let mut soak = Soak {
        c: Client::connect(&dir.path().join("bridge.sock"), &config).unwrap(),
        sim,
        daemons,
        drivers: sys.path().join("drivers"),
        rng: Rng(0xf00d_cafe),
        timeout: ExchangeTimeoutMs::new(500).unwrap(),
        lease_ms: LeaseDurationMs::new(5_000).unwrap(),
        plugged: true,
        twin: false,
        moving: false,
    };
    let pids = soak.daemons.each_ref().map(|d| (d.2, d.0.id()));
    let take = |start| pids.map(|(what, pid)| Sample::take(what, pid, start));

    // Let the start-up probes and the OpenRazer devices settle first.
    std::thread::sleep(Duration::from_secs(3));
    let start = Instant::now();
    let mut samples = vec![take(start)];
    let run_for = Duration::from_secs(soak_seconds());
    let phase = Duration::from_secs(30);
    while start.elapsed() < run_for {
        let phase_end = (Instant::now() + phase).min(start + run_for);
        while Instant::now() < phase_end {
            soak.storm();
            let pause = soak.rng.below(30) + 5;
            std::thread::sleep(Duration::from_millis(pause));
        }
        soak.check();
        let sample = take(start);
        // A thread or a file per replug or per device would pass these
        // within minutes.
        for s in &sample {
            assert!(s.threads <= 16 && s.fds <= 64, "{s:?}");
        }
        samples.push(sample);
    }

    // The storm is over: everything plugged and awake. The bridge should go
    // quiet, apart from the odd battery timer.
    soak.bring_to_rest();
    std::thread::sleep(Duration::from_secs(3));
    let before = soak.c.snapshot().unwrap().revision.get();
    std::thread::sleep(Duration::from_secs(2));
    let churn = soak.c.snapshot().unwrap().revision.get() - before;
    assert!(churn <= 8, "{churn} revisions in 2 s while idle");
    soak.check();
    let end = take(start);
    for (i, (what, _)) in pids.iter().enumerate() {
        let each: Vec<Sample> = samples.iter().map(|s| s[i]).collect();
        settled(what, &each, &end[i]);
    }
    // The keeper first, as at shutdown, while the bridge still answers.
    let [bridge, keeper] = &mut soak.daemons;
    keeper.stop();
    bridge.stop();
}
