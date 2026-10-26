// SPDX-License-Identifier: GPL-2.0-only

//! docs/protocol.md from the client's side: each method against a bridge
//! on a real socket.

mod common;

use common::{Harness, device, level, until};
use hfx_proto::client::{BenchReads, Client, ClientError};
use hfx_proto::devices;
use hfx_proto::framing::{read_frame, write_frame};
use hfx_proto::ids::{
    BenchIterations, EventLimit, ExchangeTimeoutMs, FirstGetDelayUs, GetIntervalUs,
    LeaseDurationMs, LeaseId, RouteToken, WaitTimeoutMs,
};
use hfx_proto::messages::BenchOutcome::DeviceStatus;
use hfx_proto::messages::{
    BenchOutcome, BenchQuery, BenchResult, BenchSample, Capture, CaptureEvent, CaptureKind,
    CaptureSource, FirstGet, Lane, LeaseOutcome,
};
use hfx_proto::report::{self, Report};
use serde_json::{Value, json};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// A client that writes the JSON itself, for what the typed client won't
/// send.
struct Raw(UnixStream);

impl Raw {
    fn connect(h: &Harness) -> Self {
        let stream = UnixStream::connect(&h.socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self(stream)
    }

    fn send(&mut self, message: &Value) {
        write_frame(&mut self.0, message.to_string().as_bytes()).unwrap();
    }

    /// None once the bridge has closed the connection.
    fn recv(&mut self) -> Option<Value> {
        read_frame(&mut self.0)
            .ok()
            .flatten()
            .map(|f| serde_json::from_slice(&f).unwrap())
    }

    fn call(&mut self, id: &str, method: &str, params: Value) -> Value {
        self.send(&json!({"id": id, "method": method, "params": params}));
        let reply = self.recv().expect("a reply");
        assert_eq!(reply["id"], id);
        reply
    }

    fn hello(&mut self, id: &str, client: &str, required: &[&str], optional: &[&str]) -> Value {
        self.call(
            id,
            "hello",
            json!({
                "client_id": client, "client_name": "Raw", "min_version": 1, "max_version": 1,
                "required_features": required, "optional_features": optional,
            }),
        )
    }
}

fn code(reply: &Value) -> &str {
    reply["error"]["code"].as_str().unwrap_or("none")
}

#[test]
fn hello_negotiation() {
    let h = Harness::start();
    let mut raw = Raw::connect(&h);
    assert_eq!(code(&raw.call("r1", "snapshot", json!({}))), "HFX-103");
    let hello = |min: u16, max: u16, required: &[&str]| {
        json!({"client_id": "raw-1", "client_name": "Raw", "min_version": min, "max_version": max,
               "required_features": required, "optional_features": ["wait", "future-thing"]})
    };
    let reply = raw.call("r2", "hello", hello(2, 9, &[]));
    assert_eq!(code(&reply), "HFX-101");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .ends_with("Supported protocol versions: 1.")
    );
    assert_eq!(code(&raw.call("r3", "hello", hello(3, 2, &[]))), "HFX-103");
    let reply = raw.call("r4", "hello", hello(1, 1, &["zeta", "native-routes"]));
    assert_eq!(code(&reply), "HFX-102");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .ends_with("Missing features: zeta.")
    );
    let welcome = raw.call("r5", "hello", hello(1, 3, &["native-routes"]));
    let result = &welcome["result"];
    assert_eq!(result["version"], 1);
    assert_eq!(result["features"], json!(["native-routes", "wait"]));
    assert_eq!(result["bridge_version"], "1.0.0");
    assert!(
        result["server_instance_id"]
            .as_str()
            .unwrap()
            .starts_with("bridge-")
    );
    // The same hello again is fine; a different one isn't.
    assert_eq!(
        raw.call("r6", "hello", hello(1, 3, &["native-routes"]))["result"],
        *result
    );
    assert_eq!(
        code(&raw.call("r7", "hello", hello(1, 1, &["native-routes"]))),
        "HFX-103"
    );
    // Unknown methods and features that weren't asked for: HFX-102, and the
    // connection stays.
    let teleport = raw.call("r8", "teleport", json!({"to": "mars"}));
    assert_eq!(code(&teleport), "HFX-102");
    assert_eq!(
        code(&raw.call(
            "r9",
            "capture",
            json!({"after": null, "max_events": 1, "timeout_ms": 0})
        )),
        "HFX-102"
    );
    assert!(raw.call("r10", "snapshot", json!({}))["result"]["revision"].is_string());
    // Another connection cannot take the same client id.
    let mut other = Raw::connect(&h);
    assert_eq!(code(&other.hello("r1", "raw-1", &[], &[])), "HFX-104");
}

#[test]
fn violations_close_connection() {
    let h = Harness::start();
    for bad in [
        json!({"id": "r1", "method": "snapshot", "params": {}, "extra": 1}),
        json!({"id": "r1", "method": "snapshot"}),
        json!({"id": "r1", "method": "hello", "params": {"client_id": "x"}}),
        json!({"id": "", "method": "snapshot", "params": {}}),
    ] {
        let mut raw = Raw::connect(&h);
        raw.send(&bad);
        assert_eq!(raw.recv(), None, "{bad}");
    }
    let mut raw = Raw::connect(&h);
    raw.hello("r1", "raw-2", &[], &[]);
    std::io::Write::write_all(&mut raw.0, &[0, 0, 0, 0]).unwrap();
    assert_eq!(raw.recv(), None);
}

#[test]
fn connection_limit() {
    let h = Harness::start();
    // Let the bridge see the harness's own start-up connection go.
    h.client(&[]).snapshot().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let mut open: Vec<Raw> = (0..32).map(|_| Raw::connect(&h)).collect();
    let mut extra = Raw::connect(&h);
    assert_eq!(extra.recv(), None, "the 33rd is closed at once");
    let reply = open[31].hello("r1", "last", &[], &[]);
    assert!(reply["result"].is_object());
    open.truncate(0);
}

#[test]
fn wait() {
    let h = Harness::start();
    let mut c = h.client(&["wait", "native-routes"]);
    until(&mut c, 5, |s| {
        level(s, 0, Lane::Keyboard).is_some() && level(s, 0, Lane::Pointer).is_some()
    });
    let routes = c.list_routes().unwrap().revision;
    std::thread::sleep(Duration::from_millis(100));
    let s = c.snapshot().unwrap();
    let t = Instant::now();
    let quiet = c
        .wait(
            Some(s.revision),
            Some(routes),
            WaitTimeoutMs::new(150).unwrap(),
        )
        .unwrap();
    assert!(quiet.timed_out && t.elapsed() >= Duration::from_millis(140));
    let now = c
        .wait(None, None, WaitTimeoutMs::new(10_000).unwrap())
        .unwrap();
    assert!(!now.timed_out);
    let sim = h.sim.clone();
    let poke = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        sim.status(0, Lane::Keyboard, 100, 0, 0);
    });
    let t = Instant::now();
    let woke = c
        .wait(Some(s.revision), None, WaitTimeoutMs::new(10_000).unwrap())
        .unwrap();
    assert!(!woke.timed_out && woke.state > s.revision && t.elapsed() < Duration::from_secs(5));
    poke.join().unwrap();
    // last_seen_ms alone doesn't move the revision.
    let s = c.snapshot().unwrap();
    h.sim.activity(0, Lane::Keyboard);
    let quiet = c
        .wait(Some(s.revision), None, WaitTimeoutMs::new(200).unwrap())
        .unwrap();
    assert!(quiet.timed_out);
}

#[test]
fn lease_ownership() {
    let h = Harness::start();
    let mut a = h.client(&["native-routes"]);
    let mut b = h.client(&["native-routes"]);
    let token = common::token(&mut a, Lane::Pointer);
    let long = LeaseDurationMs::new(30_000).unwrap();
    let lease = common::lease(&mut a, &token);
    let busy = b.acquire_lease(&token, long).unwrap();
    assert_eq!(
        (busy.outcome, busy.holder.unwrap().as_str()),
        (LeaseOutcome::Busy, "Test")
    );
    assert_eq!(
        a.acquire_lease(&token, long).unwrap().outcome,
        LeaseOutcome::Busy
    );
    let s = c_snapshot(&mut a);
    assert_eq!(
        device(&s, 0, Lane::Pointer)
            .unwrap()
            .controlled_by
            .as_ref()
            .unwrap()
            .as_str(),
        "Test"
    );
    assert_eq!(
        b.renew_lease(&lease, long).unwrap_err().code(),
        Some("HFX-105")
    );
    assert_eq!(b.release_lease(&lease).unwrap_err().code(), Some("HFX-105"));
    let renewed = a.renew_lease(&lease, long).unwrap();
    assert_eq!(renewed.lease_id, lease);
    a.release_lease(&lease).unwrap();
    a.release_lease(&lease).unwrap();
    assert_eq!(
        a.renew_lease(&lease, long).unwrap_err().code(),
        Some("HFX-105")
    );
    // A short lease expires, and closing a connection ends its leases.
    let short = b
        .acquire_lease(&token, LeaseDurationMs::new(1000).unwrap())
        .unwrap();
    assert_eq!(short.outcome, LeaseOutcome::Granted);
    std::thread::sleep(Duration::from_millis(1200));
    common::lease(&mut a, &token);
    drop(a);
    let deadline = Instant::now() + Duration::from_secs(5);
    while b.acquire_lease(&token, long).unwrap().outcome != LeaseOutcome::Granted {
        assert!(
            Instant::now() < deadline,
            "the lease outlived its connection"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let error = b
        .acquire_lease(
            &hfx_proto::ids::RouteToken::new("0".repeat(64)).unwrap(),
            long,
        )
        .unwrap_err();
    assert_eq!(error.code(), Some("HFX-108"));
}

fn c_snapshot(c: &mut hfx_proto::client::Client) -> hfx_proto::messages::Snapshot {
    c.snapshot().unwrap()
}

#[test]
fn transact_checks_and_replays() {
    let h = Harness::start();
    let mut typed = h.client(&["native-routes"]);
    let mouse = common::token(&mut typed, Lane::Pointer);
    let keyboard = common::token(&mut typed, Lane::Keyboard);
    let mut raw = Raw::connect(&h);
    raw.hello("h", "raw-transact", &["native-routes"], &[]);
    let lease = raw.call(
        "l",
        "acquire-lease",
        json!({"route_token": mouse.as_str(), "duration_ms": 30000}),
    );
    let lease = lease["result"]["lease_id"].as_str().unwrap().to_owned();
    let hex = |r: &report::Report| hfx_proto::ids::lower_hex(r);
    let battery = report::build(0x1f, 0x07, 0x80, &[0, 0]);
    let transact = |token: &str, report: &report::Report| json!({"lease_id": lease, "route_token": token, "report": hex(report), "timeout_ms": 1000});
    let outcome = |reply: &Value| {
        (
            reply["result"]["outcome"].as_str().unwrap().to_owned(),
            reply["result"]["code"].clone(),
        )
    };
    let not_sent = |code: &str| ("not-sent".to_owned(), json!(code));
    assert_eq!(
        outcome(&raw.call("t1", "transact", transact(&"0".repeat(64), &battery))),
        not_sent("HFX-108")
    );
    assert_eq!(
        outcome(&raw.call("t2", "transact", transact(keyboard.as_str(), &battery))),
        not_sent("HFX-105")
    );
    let wrong_size = report::build(0x1f, 0x07, 0x80, &[0]);
    assert_eq!(
        outcome(&raw.call("t3", "transact", transact(mouse.as_str(), &wrong_size))),
        not_sent("HFX-103")
    );
    let s = typed.snapshot().unwrap();
    let r = device(&s, 0, Lane::Pointer).unwrap().refused[0];
    assert_eq!((r.class, r.id, r.size, r.count), (7, 0x80, 1, 1));
    let sent_before = h.sim.requests(0, Lane::Pointer).len();
    let first = raw.call("t4", "transact", transact(mouse.as_str(), &battery));
    assert_eq!(first["result"]["outcome"], "matched");
    let response =
        hfx_proto::ids::decode_lower_hex(first["result"]["response"].as_str().unwrap()).unwrap();
    assert_eq!((response[0], response[1], response[9]), (2, 0x1f, 202));
    // A remembered id with the same params: same answer, nothing sent.
    assert_eq!(
        raw.call("t4", "transact", transact(mouse.as_str(), &battery)),
        first
    );
    assert_eq!(h.sim.requests(0, Lane::Pointer).len(), sent_before + 1);
    assert_eq!(
        code(&raw.call("t4", "transact", transact(mouse.as_str(), &wrong_size))),
        "HFX-103"
    );
    assert_eq!(code(&raw.call("t4", "snapshot", json!({}))), "HFX-103");
    // The not-sent results are remembered too.
    assert_eq!(
        outcome(&raw.call("t1", "transact", transact(&"0".repeat(64), &battery))),
        not_sent("HFX-108")
    );
    // A device that never answers: maybe delivered, never repeated.
    h.sim.set_silent(0, Lane::Pointer, 1);
    let lost = raw.call(
        "t5",
        "transact",
        transact(
            mouse.as_str(),
            &report::build(0x1f, 0x0f, 0x04, &[1, 0, 200]),
        ),
    );
    assert_eq!(
        outcome(&lost),
        ("may-have-been-sent".to_owned(), json!("HFX-110"))
    );
    assert_eq!(h.sim.requests(0, Lane::Pointer).len(), sent_before + 2);
    // A transfer that fails at once is just as uncertain.
    h.sim.fail_next(0, rustix::io::Errno::PIPE);
    let failed = raw.call("t6", "transact", transact(mouse.as_str(), &battery));
    assert_eq!(outcome(&failed).1, json!("HFX-110"));
}

#[test]
fn bench_sends_queries_only() {
    let h = Harness::start();
    let (mut c, token, lease) = common::leased(&h, &["native-routes", "bench"], Lane::Pointer);
    let (before, on) = (h.sim.requests(0, Lane::Pointer).len(), (&token, &lease));
    let normal = bench(&mut c, on, BenchQuery::Battery, (8000, 0, 3), false).unwrap();
    let timings = (normal.first_get_delay_us, normal.get_interval_us);
    assert_eq!((timings, normal.samples.len()), ((8000, 1000), 3));
    let once = |s: &BenchSample| (s.outcome, s.status, s.gets) == (BenchOutcome::Ok, Some(2), 1);
    assert!(normal.samples.iter().all(once), "{normal:?}");
    // Each sample comes after a device mode query, and every request sent is
    // a query.
    let sent = &h.sim.requests(0, Lane::Pointer)[before..];
    assert_eq!(sent.len(), 6);
    assert!(sent.iter().all(report::is_query));
    let device_mode = |r: &report::Report| (r[report::CLASS], r[report::COMMAND]) == (0x00, 0x84);
    assert!(sent.iter().step_by(2).all(device_mode));
    c.release_lease(&lease).unwrap();
    let error = bench(&mut c, on, BenchQuery::Battery, (8000, 0, 1), false);
    assert_eq!(error.unwrap_err().code(), Some("HFX-105"));
}

/// `n` bench samples on a leased route after a first wait of `first` us,
/// reading again every `interval` us (0: the usual), and on after busy.
fn bench(
    c: &mut Client,
    (token, lease): (&RouteToken, &LeaseId),
    query: BenchQuery,
    (first, interval, n): (u16, u16, u16),
    reread_busy: bool,
) -> Result<BenchResult, ClientError> {
    let interval = (interval > 0).then(|| GetIntervalUs::new(interval).unwrap());
    let reads = BenchReads {
        interval,
        reread_busy,
    };
    let (first, n) = (
        FirstGetDelayUs::new(first).unwrap(),
        BenchIterations::new(n).unwrap(),
    );
    c.bench(lease, token, query, first, n, reads)
}

fn median_us(result: &BenchResult) -> u32 {
    let mut times: Vec<u32> = result.samples.iter().map(|s| s.elapsed_us).collect();
    times.sort_unstable();
    times[times.len() / 2]
}

#[test]
fn stale_answer_reread() {
    let h = Harness::start();
    let (mut c, token, lease) = common::leased(&h, &["native-routes", "bench"], Lane::Pointer);
    // The simulated mouse answers 3 ms after the SET. A read at 2 ms gets
    // the receiver's previous answer, the device mode query's, which
    // doesn't match. The next read, 1 ms later, has the battery answer.
    let on = (&token, &lease);
    let stale = bench(&mut c, on, BenchQuery::Battery, (2000, 0, 9), false).unwrap();
    assert_eq!(stale.get_interval_us, 1000);
    let second = |s: &BenchSample| (s.outcome, s.status, s.gets) == (BenchOutcome::Ok, Some(2), 2);
    assert!(stale.samples.iter().all(second), "{stale:?}");
    // With 10 ms between reads it would be 12 ms.
    assert!(median_us(&stale) < 8000, "{stale:?}");
}

#[test]
fn bench_interval_override() {
    let h = Harness::start();
    let (mut c, token, lease) = common::leased(
        &h,
        &["native-routes", "bench", "bench-interval"],
        Lane::Keyboard,
    );
    let on = (&token, &lease);
    let run =
        |c: &mut Client, interval, n| bench(c, on, BenchQuery::Battery, (1000, interval, n), false);
    // The simulated keyboard answers after 4 ms. With the 2.x module's
    // 10 ms the second read finds the answer at 11 ms; with the usual 1 ms
    // a read soon after 4 does.
    let old = run(&mut c, 10_000, 9).unwrap();
    let usual = run(&mut c, 0, 9).unwrap();
    assert_eq!((old.get_interval_us, usual.get_interval_us), (10_000, 1000));
    let matched = |s: &BenchSample| (s.outcome, s.status) == (BenchOutcome::Ok, Some(2));
    assert!(old.samples.iter().all(matched) && usual.samples.iter().all(matched));
    assert!(old.samples.iter().all(|s| s.gets == 2));
    assert!(usual.samples.iter().all(|s| s.gets > 2));
    assert!(median_us(&old) > 11_000, "{old:?}");
    assert!(median_us(&usual) < 10_000, "{usual:?}");
    // Replies for the other lane still never match, and the reads stop
    // when the five would have: 1 ms + 4 x 10 ms after the SET.
    h.sim.misroute(0, Lane::Keyboard, 2);
    let lost = run(&mut c, 0, 1).unwrap().samples[0].clone();
    assert_eq!(lost.outcome, BenchOutcome::NoResponse, "{lost:?}");
    assert!(lost.gets > 5 && lost.gets <= 41, "{lost:?}");
    assert!(lost.elapsed_us < 41_000 + 5_000, "{lost:?}");
    // Without the feature in hello, the field is refused.
    let plain = run(&mut h.client(&["native-routes", "bench"]), 1000, 1);
    assert_eq!(plain.unwrap_err().code(), Some("HFX-102"));
}

#[test]
fn bench_reads_on_after_busy() {
    let h = Harness::start();
    let mut c = h.client(&["native-routes", "bench", "bench-interval"]);
    // Here the simulated devices answer busy until their answer is ready.
    for (lane, ready) in [(Lane::Pointer, 3000), (Lane::Keyboard, 4000)] {
        h.sim.set_busy_until_ready(0, lane, true);
        let token = common::token(&mut c, lane);
        let lease = common::lease(&mut c, &token);
        let (on, query) = ((&token, &lease), BenchQuery::Firmware);
        let run = |c: &mut Client, interval, reread_busy| {
            bench(c, on, query, (1000, interval, 9), reread_busy)
        };
        // As before, the busy answer to the first read ends the exchange.
        let ended = run(&mut c, 1000, false).unwrap();
        let busy = |s: &BenchSample| (s.outcome, s.status, s.gets) == (DeviceStatus, Some(1), 1);
        assert!(ended.samples.iter().all(busy), "{ended:?}");
        // Read on, and the answer comes at most one interval after it is ready.
        for interval in [1000, 5000] {
            let reread = run(&mut c, interval, true).unwrap();
            let ok = |s: &BenchSample| (s.outcome, s.status) == (BenchOutcome::Ok, Some(2));
            assert!(reread.samples.iter().all(ok), "{reread:?}");
            let median = median_us(&reread);
            let within = ready..ready + u32::from(interval) + 1500;
            assert!(within.contains(&median), "{reread:?}");
        }
        let plain = run(&mut h.client(&["native-routes", "bench"]), 1000, true);
        assert_eq!(plain.unwrap_err().code(), Some("HFX-102"));
        c.release_lease(&lease).unwrap();
    }
}

#[test]
fn exchanges_counted_but_not_bench() {
    let h = Harness::start();
    let (mut c, token, lease) = common::leased(&h, &["native-routes", "bench"], Lane::Pointer);
    let counts = |c: &mut Client| {
        let s = c.snapshot().unwrap();
        device(&s, 0, Lane::Pointer).unwrap().exchanges.unwrap()
    };
    // The start-up probes, 07/80 and 07/84, answered at once.
    let start = counts(&mut c);
    assert!(start.sent >= 2 && start.first_read == start.sent);
    let on = (&token, &lease);
    bench(&mut c, on, BenchQuery::Battery, (1000, 0, 5), false).unwrap();
    assert_eq!(counts(&mut c), start);
    let timeout = ExchangeTimeoutMs::new(1000).unwrap();
    let mut send = |query| {
        let request = report::build(0x1f, 0x00, query, &[0, 0]);
        c.transact(&lease, &token, &request, timeout).unwrap();
        counts(&mut c)
    };
    let plain = send(0x81);
    assert_eq!([plain.sent, plain.first_read], [start.sent + 1; 2]);
    // Busy until the answer is ready, 1.5 ms after the first read.
    h.sim.set_answer_after(0, Lane::Pointer, &[4500]);
    h.sim.set_busy_until_ready(0, Lane::Pointer, true);
    let busy = send(0x84);
    assert_eq!([busy.first_read, busy.reread_other], [plain.sent, 0]);
    assert!(busy.reread_busy > 0 && busy.no_answer == 0, "{busy:?}");
    // No answer at all: every read gets the previous one.
    h.sim.set_silent(0, Lane::Pointer, 1);
    let silent = send(0x81);
    assert_eq!([silent.sent, silent.no_answer], [busy.sent + 1, 1]);
    assert!(silent.reread_other > 5, "{silent:?}");
}

/// Runs `n` windows of 50 transacts that take `queries` in turn, with the
/// device answering after `answers` us in turn, and gives the learned wait
/// after each window. The lease ends with the connection.
fn learn(h: &Harness, lane: Lane, answers: &[u64], queries: &[Report], n: usize) -> Vec<u16> {
    h.sim.set_answer_after(0, lane, answers);
    let (mut c, token, lease) = common::leased(h, &["native-routes"], lane);
    let timeout = ExchangeTimeoutMs::new(1000).unwrap();
    let mut window = |_| {
        for query in queries.iter().cycle().take(50) {
            c.transact(&lease, &token, query, timeout).unwrap();
        }
        learned(&mut c, lane).delay_us
    };
    (0..n).map(&mut window).collect()
}

fn learned(c: &mut Client, lane: Lane) -> FirstGet {
    let snapshot = c.snapshot().unwrap();
    device(&snapshot, 0, lane).unwrap().first_get.unwrap()
}

/// Two different queries, so a first read can't take the previous answer
/// for its own.
fn two() -> [Report; 2] {
    [0x81, 0x84].map(|command| report::build(0x1f, 0x00, command, &[0, 0]))
}

#[test]
fn first_get_wait_is_learned() {
    // A checked mouse, 6 ms in the catalog, that answers after 2.5 ms:
    // down a step a window, then within a step of 2.5 ms.
    let options = common::Options::before(|sim| sim.identity(0, Lane::Pointer, 0x00ab, 1));
    let (h, _) = Harness::with(options);
    let waits = learn(&h, Lane::Pointer, &[2500], &two(), 9);
    let (down, rest) = waits.split_at(7);
    assert_eq!(down, [5500, 5000, 4500, 4000, 3500, 3000, 2500]);
    assert!(rest.iter().all(|w| [2000, 2500].contains(w)), "{rest:?}");
    // The keyboard, 4 ms in the catalog, answering after 7 ms: up a step a
    // window, then within a step of 7 ms.
    let up = learn(&h, Lane::Keyboard, &[7000], &two(), 8);
    assert_eq!(up[..6], [4500, 5000, 5500, 6000, 6500, 7000], "{up:?}");
    assert!(up[6..].iter().all(|w| [6500, 7000].contains(w)), "{up:?}");
    // Answering after 9 ms, it stops at 8 ms.
    let slow = learn(&h, Lane::Keyboard, &[9000], &two(), 4);
    assert_eq!(slow[2..], [8000, 8000], "{slow:?}");
}

#[test]
fn learning_keeps_its_limits() {
    let h = Harness::start();
    // Bench's exchanges teach nothing: these would have made it longer.
    let (mut c, token, lease) = common::leased(&h, &["native-routes", "bench"], Lane::Keyboard);
    let on = (&token, &lease);
    bench(&mut c, on, BenchQuery::Battery, (1000, 0, 100), false).unwrap();
    assert_eq!(learned(&mut c, Lane::Keyboard).delay_us, 4000);
    drop(c);
    // The same query again and again: an early read gets the previous
    // answer, which matches, so the first reads tell nothing.
    let battery = [report::build(0x9f, 0x07, 0x80, &[0, 0])];
    assert_eq!(learn(&h, Lane::Keyboard, &[4500], &battery, 2), [4000; 2]);
    // Noisy answers, a quarter of them late: it settles on the two waits
    // around 3.5 ms and goes no further.
    let noisy = learn(&h, Lane::Keyboard, &[2000, 2000, 2000, 3500], &two(), 6);
    assert!(noisy.iter().all(|w| [3000, 3500].contains(w)), "{noisy:?}");
    // A mouse that answers at once: down to 2 ms and no further.
    let fast = learn(&h, Lane::Pointer, &[1000], &two(), 3);
    assert_eq!(fast, [2500, 2000, 2000]);
    // A query like the one before still waits the catalog's 3 ms, since an
    // early read would get the previous answer and take it for its own.
    let (mut c, token, lease) = common::leased(&h, &["native-routes"], Lane::Pointer);
    let battery = report::build(0x1f, 0x07, 0x80, &[0, 0]);
    let timeout = ExchangeTimeoutMs::new(1000).unwrap();
    let mut send = || c.transact(&lease, &token, &battery, timeout).unwrap();
    send();
    let start = Instant::now();
    send();
    assert!(start.elapsed() >= Duration::from_millis(3));
}

#[test]
fn learned_waits_kept_across_restarts() {
    let state = tempfile::tempdir().unwrap();
    let start = |firmware| {
        let before =
            move |sim: &hfx_bridge::sim::Sim| sim.set_firmware(0, Lane::Keyboard, firmware);
        let state_dir = Some(state.path().to_owned());
        let options = common::Options {
            state_dir,
            ..common::Options::before(before)
        };
        Harness::with(options).0
    };
    let kept = |h: &Harness| {
        let mut c = h.client(&[]);
        common::until(&mut c, 5, |s| device(s, 0, Lane::Keyboard).is_some());
        learned(&mut c, Lane::Keyboard)
    };
    // Answering after 5.5 ms, the keyboard's wait goes up from the
    // catalog's 4 ms. The file is written at once, the first time.
    let h = start([2, 0x33]);
    assert_eq!(learn(&h, Lane::Keyboard, &[5500], &two(), 1), [4500]);
    common::wait_for(5, || state.path().join("learned-waits.json").exists());
    h.stop().unwrap();
    let resumed = FirstGet {
        delay_us: 4500,
        learned_from_us: 4000,
        saved_us: Some(4500),
    };
    assert_eq!(kept(&start([2, 0x33])), resumed);
    // On other firmware it is dropped, once the bridge sees an answer to 00:81.
    let h = start([2, 0x40]);
    assert_eq!(kept(&h), resumed);
    assert_eq!(learn(&h, Lane::Keyboard, &[3000], &two()[..1], 1), [4000]);
    assert_eq!(kept(&h).saved_us, None);
}

#[test]
fn capture() {
    let h = Harness::start();
    let mut c = h.client(&["capture", "wait", "native-routes"]);
    common::token(&mut c, Lane::Pointer);
    common::token(&mut c, Lane::Keyboard);
    let sim = h.sim.clone();
    let poke = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        sim.activity(0, Lane::Keyboard);
        sim.status(0, Lane::Pointer, 202, 1, 1);
        sim.report(0, &[0x05, 0x77, 1, 2]);
    });
    let params = |after, timeout| Capture {
        after,
        max_events: EventLimit::new(256).unwrap(),
        timeout_ms: WaitTimeoutMs::new(timeout).unwrap(),
    };
    // A capture starts with the inventory: 64 bytes, then its length, the
    // marker's offset and the two ids.
    let new = c.capture(params(None, 5000)).unwrap();
    let first = &new.events[0];
    assert_eq!(
        (first.kind, first.source),
        (CaptureKind::Raw, CaptureSource::Inventory)
    );
    assert_eq!(first.data.as_slice()[64..], [0, 2, 30, 1, 0, 0xcd, 2, 0x96]);
    poke.join().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let all = c.capture(params(Some(common::seq(0)), 0)).unwrap();
    assert!(!all.gap);
    // A key press only as activity, vendor reports as they came and parsed.
    let seen = |e: &CaptureEvent| (e.kind, e.source, e.data.as_slice().to_vec());
    let seen: Vec<_> = all.events.iter().map(seen).collect();
    let passive = |(kind, data)| (kind, CaptureSource::Passive, data);
    let (raw, status) = (CaptureKind::Raw, CaptureKind::Status);
    let activity = (CaptureKind::Activity, CaptureSource::Input, vec![0; 8]);
    let reports = [
        (raw, vec![5, 0x31, 0xca, 1, 1, 0]),
        (status, vec![0xca, 1, 1, 0, 0, 0, 0, 0]),
        (raw, vec![5, 0x77, 1, 2]),
    ];
    let wanted: Vec<_> = [activity].into_iter().chain(reports.map(passive)).collect();
    let quiet: Vec<_> = seen
        .into_iter()
        .filter(|e| e.1 != CaptureSource::Bridge)
        .collect();
    assert!(quiet.ends_with(&wanted), "{quiet:?}");
    let replies: Vec<_> = all
        .events
        .iter()
        .filter(|e| e.kind == CaptureKind::Reply)
        .collect();
    assert!(
        !replies.is_empty()
            && replies
                .iter()
                .all(|e| e.source == CaptureSource::Bridge && e.data.as_slice().len() == 90)
    );
    assert_eq!(all.next, all.events.last().unwrap().sequence);
    // Nothing new: a short wait returns empty, with the same cursor.
    let none = c.capture(params(Some(all.next), 50)).unwrap();
    assert!(none.events.is_empty() && none.next == all.next);
}

#[test]
fn receiver_probe() {
    let h = Harness::start();
    let mut c = h.client(&["probe-receiver", "native-routes"]);
    common::token(&mut c, Lane::Pointer);
    let id = c.snapshot().unwrap().receivers[0].receiver_id.clone();
    let before = h.sim.requests(0, Lane::Pointer).len();
    // The bridge's own question at admission was the first of them.
    let asked = h.sim.unrouted(0).len();
    h.sim.set_slots(0, false);
    let probe = c.probe_receiver(id.clone()).unwrap();
    // Exactly the fixed queries, in turn, and 00:c5 is the one answered now.
    let fixed: Vec<_> = (0..4).filter_map(devices::receiver_probe).collect();
    assert_eq!((asked, &h.sim.unrouted(0)[asked..]), (1, &fixed[..3]));
    assert_eq!(h.sim.requests(0, Lane::Pointer)[before..], fixed[3..]);
    let answered: Vec<_> = probe.answers.iter().map(Option::is_some).collect();
    assert_eq!(answered, [false, false, false, true]);
    let inventory = probe.inventory.as_slice();
    assert_eq!((inventory.len(), inventory[0]), (22, 1));
    let error = h.client(&[]).probe_receiver(id).unwrap_err();
    assert_eq!(error.code(), Some("HFX-102"));
}

#[test]
fn snapshot_and_routes() {
    let h = Harness::start();
    let mut c = h.client(&["native-routes"]);
    let s = until(&mut c, 5, |s| {
        s.receivers
            .first()
            .is_some_and(|r| r.devices.len() == 2 && r.devices.iter().all(|d| d.controllable))
    });
    let r = &s.receivers[0];
    assert_eq!(
        (r.vendor_id, r.product_id, r.generation_id.get()),
        (5426, 207, 1)
    );
    assert!(r.receiver_id.as_str().starts_with("receiver-") && r.receiver_id.as_str().len() == 33);
    let ids: Vec<_> = r.devices.iter().map(|d| d.device_id.as_str()).collect();
    assert_eq!(ids, ["pointer-00cd", "keyboard-0296"]);
    let routes = c.list_routes().unwrap().routes;
    let ids: Vec<_> = routes.iter().map(|r| r.device_id.as_str()).collect();
    assert_eq!(ids, ["keyboard-0296", "pointer-00cd"]);
    assert!(routes.iter().all(|r| {
        r.report_bytes == 90
            && r.route_token
                .as_ref()
                .is_some_and(|t| t.as_str().len() == 64)
    }));
}
