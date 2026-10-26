// SPDX-License-Identifier: GPL-2.0-only

//! The device keeper on its own, with the test as its bridge, and with a
//! real bridge when either side goes away and comes back.

mod common;

use common::{Harness, Options, Running, own_uid, until, wait_for};
use hfx_bridge::ipc::{FromKeeper, ToKeeper};
use hfx_bridge::sim::{Batteries, Sim};
use hfx_bridge::uhid::{self, Written};
use hfx_proto::framing::{read_frame, write_frame};
use hfx_proto::messages::{Lane, OpenRazerState};
use hfx_proto::report;
use rustix::fd::OwnedFd;
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A keeper whose bridge is the test.
struct Alone {
    sim: Sim,
    _dir: tempfile::TempDir,
    listener: UnixListener,
    bridge: UnixStream,
    devices: mpsc::Receiver<(Vec<u8>, OwnedFd)>,
    batteries: Batteries,
    _keeper: Running,
}

const LABEL: &str = "native-0123456789abcdef";
const RECEIVER: &str = "receiver-6b1f0e9c2a7d4e83b5c1f2a0";

impl Alone {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let sim = Sim::new(1);
        sim.serve(&dir.path().join("sim.sock")).unwrap();
        let listener = UnixListener::bind(dir.path().join("keeper.sock")).unwrap();
        let (forward, devices) = mpsc::channel();
        let (uhid, batteries) = hfx_bridge::sim::local_uhid(Some(forward), false);
        let config = hfx_bridge::keeper::Config {
            source: hfx_bridge::Source::Sim(dir.path().join("sim.sock")),
            socket: dir.path().join("keeper.sock"),
            bridge_uid: own_uid(),
            uhid,
            hid_drivers: None,
            connected: None,
        };
        let keeper = Running::start("keeper", move |stop| hfx_bridge::keeper::run(config, stop));
        let bridge = accept(&listener);
        let mut alone = Self {
            sim,
            _dir: dir,
            listener,
            bridge,
            devices,
            batteries,
            _keeper: keeper,
        };
        alone.hello();
        alone
    }

    fn hello(&mut self) {
        assert!(matches!(
            self.recv(2000),
            Some(FromKeeper::Hello { version: 1, .. })
        ));
        self.send(&ToKeeper::Hello { version: 1 });
    }

    /// The keeper's next connection, after it dropped this one.
    fn reconnected(&mut self) {
        assert_eq!(self.recv(2000), None, "the keeper should have hung up");
        self.bridge = accept(&self.listener);
        self.hello();
    }

    fn send(&mut self, message: &ToKeeper) {
        self.send_text(&serde_json::to_string(message).unwrap());
    }

    fn send_text(&mut self, text: &str) {
        write_frame(&mut self.bridge, text.as_bytes()).unwrap();
    }

    /// The next message, or None when nothing comes in time or the keeper
    /// hung up.
    fn recv(&mut self, ms: u64) -> Option<FromKeeper> {
        let frame = self.frame(ms)?;
        Some(serde_json::from_slice(&frame).unwrap())
    }

    fn frame(&mut self, ms: u64) -> Option<Vec<u8>> {
        let timeout = Some(Duration::from_millis(ms));
        self.bridge.set_read_timeout(timeout).unwrap();
        read_frame(&mut self.bridge).ok().flatten()
    }

    /// An OpenRazer device the keeper made, started by the test's kernel.
    fn openrazer(&mut self, device: u64) -> UnixDatagram {
        self.send_text(&create(device, 0x00cd, LABEL));
        let (_, fd) = self.devices.recv_timeout(Duration::from_secs(2)).unwrap();
        let kernel = UnixDatagram::from(fd);
        kernel.send(&uhid::kernel_start(0)).unwrap();
        let started = FromKeeper::Started { device, flags: 0 };
        assert_eq!(self.recv(2000), Some(started));
        kernel
    }

    /// No device reached the kernel, and a refusal came back for `device`.
    fn refused(&mut self, device: u64) {
        let Some(FromKeeper::Ended { device: d, why }) = self.recv(2000) else {
            panic!("expected a refusal for device {device}")
        };
        assert!(
            d == device && why.starts_with("the keeper refused"),
            "{why}"
        );
        assert!(self.devices.try_recv().is_err());
    }
}

fn accept(listener: &UnixListener) -> UnixStream {
    listener.set_nonblocking(false).unwrap();
    listener.accept().unwrap().0
}

fn create(device: u64, product_id: u16, label: &str) -> String {
    format!(
        r#"{{"kind":"create","device":{device},"template":{{"openrazer":{{"product_id":{product_id},"label":"{label}"}}}}}}"#
    )
}

fn battery(device: u64, receiver: &str, product_id: u16) -> String {
    format!(
        r#"{{"kind":"create","device":{device},"template":{{"battery":{{"receiver":"{receiver}","lane":"pointer","product_id":{product_id}}}}}}}"#
    )
}

/// What the test's kernel end got next, if anything.
fn written(kernel: &UnixDatagram, ms: u64) -> Option<Written> {
    kernel
        .set_read_timeout(Some(Duration::from_millis(ms)))
        .unwrap();
    let mut buffer = vec![0_u8; uhid::EVENT_BYTES + 1];
    let n = kernel.recv(&mut buffer).ok()?;
    uhid::decode_written(&buffer[..n])
}

#[test]
fn bad_templates_refused() {
    let mut k = Alone::start();
    // An unknown template, or one with a field too many, doesn't
    // even parse, and costs the bridge its connection.
    for text in [
        create(1, 0x00cd, LABEL).replace("openrazer", "keyboard"),
        create(1, 0x00cd, LABEL).replace(r#""label""#, r#""name":"Razer","label""#),
    ] {
        k.send_text(&text);
        k.reconnected();
    }
    // So does a frame over 1 KiB.
    let padded = create(1, 0x00cd, LABEL).replace(LABEL, &"a".repeat(1100));
    k.send_text(&padded);
    k.reconnected();
    // The rest parse, and are refused one by one.
    let long = format!("{LABEL}0");
    for (device, text) in [
        (2, create(2, 0x1234, LABEL)),
        (3, create(3, 0x00e8, LABEL)),
        (4, create(4, 0x00cd, "Native-0123456789abcdef")),
        (5, create(5, 0x00cd, &long)),
        (6, create(6, 0x00cd, "native-0123/../../uhid")),
        (7, battery(7, "receiver-6B1F0E9C2A7D4E83B5C1F2A0", 0x00cd)),
        (8, battery(8, "receiver-0", 0x00cd)),
    ] {
        k.send_text(&text);
        k.refused(device);
    }
    assert!(k.batteries.lock().unwrap().is_empty());
    // One device per id and per phys.
    let _kernel = k.openrazer(9);
    k.send_text(&create(9, 0x0296, "native-fedcba9876543210"));
    k.refused(9);
    k.send_text(&create(10, 0x0296, LABEL));
    k.refused(10);
}

#[test]
fn stray_messages_refused() {
    let mut k = Alone::start();
    let kernel = k.openrazer(1);
    let answer = report::build(0x1f, 0x07, 0x80, &[0, 0]);
    let data = Some(hfx_proto::ids::ReportHex::from_bytes(answer));
    // A reply nobody asked for never reaches the kernel.
    k.send(&ToKeeper::Reply {
        device: 1,
        id: 99,
        errno: 0,
        data,
    });
    assert_eq!(written(&kernel, 300), None);
    // An asked one does, once; one of the wrong shape becomes EIO.
    kernel.send(&uhid::kernel_get(7)).unwrap();
    let asked = FromKeeper::Request {
        device: 1,
        id: 7,
        set: None,
    };
    assert_eq!(k.recv(2000), Some(asked));
    let reply = |errno, data| ToKeeper::Reply {
        device: 1,
        id: 7,
        errno,
        data,
    };
    k.send(&reply(0, None));
    let eio = Written::GetReply {
        id: 7,
        errno: uhid::EIO,
        data: vec![],
    };
    assert_eq!(written(&kernel, 2000), Some(eio));
    k.send(&reply(0, data));
    assert_eq!(written(&kernel, 300), None);
    // Battery levels go to batteries alone, and no higher than 100.
    k.send(&ToKeeper::Battery {
        device: 1,
        level: 50,
        charging: false,
    });
    assert_eq!(written(&kernel, 300), None);
    k.send_text(&battery(2, RECEIVER, 0x00cd));
    let phys = format!("hyperflux-next/battery/{}/pointer", &RECEIVER[9..]);
    wait_for(2, || k.batteries.lock().unwrap().contains_key(&phys));
    for level in [101, 255, 64] {
        k.send_text(&format!(
            r#"{{"kind":"battery","device":2,"level":{level},"charging":true}}"#
        ));
    }
    let level = || k.batteries.lock().unwrap()[&phys].level;
    wait_for(2, || level().is_some());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(level(), Some(64));
    // There is no message for any other input report: asking for one ends
    // the connection, and the bridge's devices go with it.
    let input = r#"{"kind":"input","device":1,"report":"0100001d00000000"}"#;
    k.send_text(input);
    let mut seen = Vec::new();
    while let Some(w) = written(&kernel, 2000) {
        seen.push(w);
    }
    assert_eq!(seen, [Written::Destroy], "only the destroy");
    wait_for(2, || k.batteries.lock().unwrap().is_empty());
}

#[test]
fn keys_stay_in_keeper() {
    let mut k = Alone::start();
    let keys = [0x01, 0x00, 0x00, 0x1d, 0x1b, 0x06, 0x00, 0x00, 0x00];
    let started = Instant::now();
    for _ in 0..20 {
        k.sim.report(0, &keys);
        k.sim.report(0, &[0x02, 0xe9, 0x00]);
        k.sim.report(0, &[0x0a, 0x1d, 0x1b]);
    }
    let typing = started.elapsed();
    k.sim.status(0, Lane::Keyboard, 163, 0, 1);
    let mut frames = Vec::new();
    while let Some(frame) = k.frame(500) {
        frames.push(frame);
    }
    let messages: Vec<FromKeeper> = frames
        .iter()
        .map(|f| serde_json::from_slice(f).unwrap())
        .collect();
    let mut activity = 0;
    for m in &messages {
        match m {
            FromKeeper::Activity { lane, .. } => activity += usize::from(*lane == Lane::Keyboard),
            FromKeeper::Vendor { report, .. } => {
                assert!(hfx_bridge::wire::VENDOR_REPORTS.contains(&report.as_slice()[0]));
            }
            other => panic!("{other:?}"),
        }
    }
    // One activity event per 250 ms, whatever was typed, and the status.
    let most = 1 + typing.as_millis() / 250;
    assert!(activity >= 1 && activity as u128 <= most, "{activity}");
    let text = |f: &Vec<u8>| String::from_utf8_lossy(f).into_owned();
    let status = hfx_proto::ids::lower_hex(&[0x09, 0x31, 163, 0, 1, 0]);
    assert!(frames.iter().any(|f| text(f).contains(&status)));
    let typed = hfx_proto::ids::lower_hex(&keys);
    assert!(
        frames.iter().all(|f| !text(f).contains(&typed[6..])),
        "a key got out"
    );
}

#[test]
fn bridge_checks_keeper_uid() {
    let (h, _) = Harness::with(Options {
        keeper_uid: Some(own_uid() + 1),
        openrazer: true,
        ..Options::default()
    });
    let mut c = h.client(&[]);
    until(&mut c, 5, |s| common::level(s, 0, Lane::Pointer).is_some());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(h.connected.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert!(h.batteries.lock().unwrap().is_empty());
    let s = c.snapshot().unwrap();
    let d = common::device(&s, 0, Lane::Pointer).unwrap();
    let reason = d.openrazer.as_ref().and_then(|o| o.reason.clone());
    assert_eq!(
        reason.unwrap().as_str(),
        "the device keeper isn't connected"
    );
}

#[test]
fn keeper_checks_bridge_uid() {
    let dir = tempfile::tempdir().unwrap();
    let sim = Sim::new(1);
    sim.serve(&dir.path().join("sim.sock")).unwrap();
    let listener = UnixListener::bind(dir.path().join("keeper.sock")).unwrap();
    let config = hfx_bridge::keeper::Config {
        source: hfx_bridge::Source::Sim(dir.path().join("sim.sock")),
        socket: dir.path().join("keeper.sock"),
        bridge_uid: own_uid() + 1,
        uhid: hfx_bridge::sim::local_uhid(None, false).0,
        hid_drivers: None,
        connected: None,
    };
    // Timed from before the keeper starts: its 100 ms wait begins after
    // that, while noticing the hang-up here can lag on a busy machine. The
    // keeper counts whole milliseconds, so we allow it a little less.
    let started = Instant::now();
    let _keeper = Running::start("keeper", move |stop| hfx_bridge::keeper::run(config, stop));
    // It hangs up without a word, and doesn't come back at once.
    let mut first = accept(&listener);
    first
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    assert_eq!(read_frame(&mut first).ok().flatten(), None);
    drop(accept(&listener));
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(90),
        "came back after {took:?}"
    );
}

/// The next OpenRazer device the keeper makes for the bridge, started.
fn device(or: &common::OpenRazer) -> UnixDatagram {
    let (_, fd) = or.devices.recv_timeout(Duration::from_secs(10)).unwrap();
    let kernel = UnixDatagram::from(fd);
    kernel.send(&uhid::kernel_start(0)).unwrap();
    kernel
}

fn both_shown(h: &Harness) -> bool {
    h.shown(0, Lane::Pointer).is_some() && h.shown(0, Lane::Keyboard).is_some()
}

fn openrazer_state(h: &Harness, lane: Lane) -> Option<OpenRazerState> {
    let s = h.client(&[]).snapshot().unwrap();
    Some(common::device(&s, 0, lane)?.openrazer.as_ref()?.state)
}

#[test]
fn keeper_restart_recovers() {
    let (mut h, or) = Harness::with(Options {
        openrazer: true,
        ..Options::default()
    });
    let or = or.unwrap();
    let first = [device(&or), device(&or)];
    wait_for(5, || both_shown(&h));
    h.stop_keeper().unwrap();
    // Its devices went with it, and the bridge knows.
    for kernel in &first {
        assert_eq!(written(kernel, 3000), Some(Written::Destroy));
    }
    wait_for(2, || h.batteries.lock().unwrap().is_empty());
    wait_for(5, || {
        openrazer_state(&h, Lane::Pointer) == Some(OpenRazerState::NotOffered)
    });
    let connected = h.connected.load(std::sync::atomic::Ordering::Relaxed);
    h.start_keeper();
    wait_for(5, || {
        h.connected.load(std::sync::atomic::Ordering::Relaxed) > connected
    });
    wait_for(5, || both_shown(&h));
    let again = [device(&or), device(&or)];
    for kernel in &again {
        assert_eq!(written(kernel, 200), None);
    }
    wait_for(5, || {
        openrazer_state(&h, Lane::Keyboard) == Some(OpenRazerState::Live)
    });
}

#[test]
fn bridge_restart_recovers() {
    let (mut h, or) = Harness::with(Options {
        openrazer: true,
        ..Options::default()
    });
    let or = or.unwrap();
    let first = [device(&or), device(&or)];
    wait_for(5, || both_shown(&h));
    let before: Vec<u64> = h.batteries.lock().unwrap().values().map(|b| b.id).collect();
    h.restart_bridge();
    for kernel in &first {
        assert_eq!(written(kernel, 7000), Some(Written::Destroy));
    }
    // The new bridge's batteries are new devices.
    wait_for(5, || {
        let batteries = h.batteries.lock().unwrap();
        batteries.len() == 2 && batteries.values().all(|b| !before.contains(&b.id))
    });
    wait_for(5, || both_shown(&h));
    let again = device(&or);
    let query = report::build(0x1f, 0x07, 0x80, &[0, 0]);
    again.send(&uhid::kernel_set(1, &query)).unwrap();
    assert_eq!(
        written(&again, 2000),
        Some(Written::SetReply { id: 1, errno: 0 })
    );
}
