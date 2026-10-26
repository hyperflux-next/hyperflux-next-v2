// SPDX-License-Identifier: GPL-2.0-only

//! The commands end to end against the mock bridge and a fixture tree.

mod common;

use common::{FakeRunner, MockBridge, Script, ScriptedPrompt, Tree, healthy, run, system};
use hfx_proto::messages::{Inventory, Presence, Support};
use hyperfluxctl::system::System;
use std::os::unix::fs::PermissionsExt;

struct Setup {
    bridge: MockBridge,
    tree: Tree,
    system: System,
}

impl Setup {
    fn new(script: Script) -> Self {
        Self::with(script, false)
    }

    /// With `full`, Full is installed and this login isn't in OpenRazer's group.
    fn with(script: Script, full: bool) -> Self {
        let (tree, runner) = (Tree::new(), FakeRunner::default());
        healthy(&tree, &runner);
        if full {
            common::full(&tree, &runner);
        }
        let system = system(&tree, &runner, &[]);
        Self {
            bridge: MockBridge::start(script),
            tree,
            system,
        }
    }

    fn run(&self, line: &[&str]) -> common::Run {
        let socket = self.bridge.socket_arg();
        let mut line = line.to_vec();
        line.extend(["--socket", &socket]);
        run(&self.system, &line)
    }

    fn line(&self, line: &str) -> common::Run {
        self.run(&line.split_whitespace().collect::<Vec<_>>())
    }
}

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// The fixtures' receiver id, login and serials, none of which a bundle,
/// a capture or a report may hold.
fn assert_private(text: &str) {
    for private in ["6b1f0e9c", "robin", "SER1AL", "PM2143H0001"] {
        assert!(!text.contains(private), "{private} is in the file");
    }
}

/// Doctor in a bundle or a report: HFX-206 names the receiver by its alias
/// and the group fix says `$USER`.
fn assert_shared_doctor(doctor: &serde_json::Value) {
    let findings = doctor["findings"].as_array().unwrap();
    let inventory = findings.iter().find(|f| f["code"] == "HFX-206").unwrap();
    assert_eq!(inventory["details"]["receiver"], "receiver-1");
    let fixes = doctor["fixes"].as_array().unwrap();
    let group = r#"sudo gpasswd -a "$USER" plugdev"#;
    assert!(fixes.iter().any(|f| f["command"] == group), "{fixes:?}");
}

const STATUS: &str = "\
Driver: active
Bridge: running
Keeper: running, connected to the bridge
Receivers: 1
Razer Basilisk V3 Pro 35K: online, battery 79%, charging
Razer DeathStalker V2 Pro TKL: asleep, battery 64%, no update for 42 minutes
Next: no action required
";

#[test]
fn status_text_and_json() {
    let setup = Setup::new(Script::default());
    let result = setup.run(&["status"]);
    assert_eq!((result.ok(), result.out.as_str()), (0, STATUS));
    // Status reads the snapshot and asks for no feature.
    let log = setup.bridge.log();
    assert!(log[0].ends_with(" []") && log[1] == "snapshot", "{log:?}");
    let json = setup.run(&["--json", "status"]).json();
    let state = |key: &str| json[key].as_str().map(str::to_owned);
    let states = ["driver", "bridge", "keeper"].map(state);
    assert_eq!(
        states,
        ["active", "running", "connected"].map(|s| Some(s.into()))
    );
    assert_eq!(json["devices"][0]["battery_charge"], "charging");
    assert_eq!(json["devices"][1]["battery_stale"], true);
    assert_eq!(json["devices"][1]["controlled_by"], "OpenRazer");
}

#[test]
fn status_without_a_bridge() {
    let (tree, runner) = (Tree::new(), FakeRunner::default());
    healthy(&tree, &runner);
    runner.answer(common::SERVICE, "inactive");
    runner.answer(
        common::KEEPER,
        "ActiveState=active\nStatusText=waiting for the bridge",
    );
    let missing = tree.path("/run/nothing.sock").display().to_string();
    let result = run(
        &system(&tree, &runner, &[]),
        &["status", "--socket", &missing],
    );
    assert_eq!(result.ok(), 0);
    let next = "Next: Start it with: sudo systemctl enable --now hyperflux-next-bridge.service";
    assert_eq!(
        result.out,
        format!(
            "Driver: active\nBridge: stopped\nKeeper: running, not connected to the bridge\n\
             Receivers: 1\n{next}\n"
        )
    );
}

#[test]
fn devices_text() {
    let result = Setup::new(Script::default()).run(&["devices"]);
    assert_eq!(result.ok(), 0);
    assert_eq!(
        result.out,
        "Razer Basilisk V3 Pro 35K
  id: pointer-00cd (1532:00cd)
  receiver: receiver-6b1f0e9c2a7d4e83b5c1f2a0, generation 3, devices named by the receiver's paired slots
  lane: pointer
  support: tested on hardware
  presence: online, last seen less than a minute ago
  contact: on the mat
  battery: 79%, charging, read less than a minute ago
  control: available, not in use
  slot: connected through the receiver
  openrazer: live
  timing: wait 3.0 ms (learned from 3.0)
  exchanges: 210 since the bridge started, 203 answered at the first read, 0 not at all; read again 4 times after another reply, 5 after busy

Razer DeathStalker V2 Pro TKL
  id: keyboard-0296 (1532:0296)
  receiver: receiver-6b1f0e9c2a7d4e83b5c1f2a0, generation 3, devices named by the receiver's paired slots
  lane: keyboard
  support: tested on hardware
  presence: asleep, last seen 41 minutes ago
  contact: not reported
  battery: 64%, read 42 minutes ago (stale)
  control: held by OpenRazer
  slot: not connected through the receiver (off, or on its cable)
  openrazer: retrying (serial read not answered)
  timing: wait 4.5 ms (learned from 4.0, resumed at 5.0)
  exchanges: 96 since the bridge started, 88 answered at the first read, 1 not at all; read again 3 times after another reply, 9 after busy
"
    );
}

#[test]
fn unidentified_device() {
    let mut snapshot = common::snapshot();
    snapshot.receivers[0].inventory = Some(Inventory::NotRecognised);
    let device = &mut snapshot.receivers[0].devices[0];
    (device.product_id, device.name, device.support) = (0, None, Support::Unknown);
    let setup = Setup::new(Script {
        snapshots: vec![snapshot],
        ..Script::default()
    });
    let line = "Unidentified pointer device: online, battery 79%, charging; \
                move or wake the device so the receiver names it\n";
    assert!(setup.run(&["status"]).out.contains(line));
    let json = setup.run(&["--json", "devices"]).json();
    assert_eq!(json["receivers"][0]["inventory"], "not-recognised");
    assert_eq!(json["devices"][1]["refused"][0]["count"], 3);
}

#[test]
fn battery_text_and_waybar() {
    let setup = Setup::new(Script::default());
    let lines = "Razer Basilisk V3 Pro 35K: 79% charging\nRazer DeathStalker V2 Pro TKL: 64% (42 minutes old)";
    assert_eq!(setup.run(&["battery"]).out, format!("{lines}\n"));
    let bar = setup.run(&["battery", "--waybar"]);
    assert_eq!(bar.ok(), 0);
    let tooltip = lines.replace('\n', "\\n");
    let expected =
        format!(r#"{{"text":"79% 64%","tooltip":"{tooltip}","class":"stale","percentage":64}}"#);
    assert_eq!(bar.out, expected + "\n");
}

#[test]
fn battery_without_a_bridge() {
    let tree = Tree::new();
    let system = system(&tree, &FakeRunner::default(), &[]);
    let missing = tree.path("/run/nothing.sock").display().to_string();
    let error = run(&system, &["battery", "--socket", &missing]).error();
    assert!(
        error.starts_with("can't reach the HyperFlux Next V2 bridge: "),
        "{error}"
    );
    // A bar gets an empty module rather than an error.
    let bar = run(&system, &["battery", "--waybar", "--socket", &missing]);
    assert_eq!(bar.ok(), 0);
    assert_eq!(
        (bar.json()["text"].as_str(), bar.json()["class"].as_str()),
        (Some(""), Some("unavailable"))
    );
}

#[test]
fn watch_prints_changed_lines() {
    let first = common::snapshot();
    let mut second = first.clone();
    second.revision = hfx_proto::ids::Sequence::new(42).unwrap();
    second.receivers[0].devices[1].presence = Presence::Online;
    let setup = Setup::new(Script {
        snapshots: vec![first, second],
        ..Script::default()
    });
    let (mut out, mut prompt) = (Vec::new(), ScriptedPrompt::new(false, &[]));
    let mut ctx = hyperfluxctl::Context {
        out: &mut out,
        system: &setup.system,
        prompt: &mut prompt,
    };
    let socket = Some(setup.bridge.socket.as_path());
    assert_eq!(
        hyperfluxctl::commands::watch(&mut ctx, socket, false, Some(3)).unwrap(),
        0
    );
    let mut expected: Vec<&str> = STATUS.lines().skip(4).take(2).collect();
    expected.push("Razer DeathStalker V2 Pro TKL: online, battery 64%, no update for 42 minutes");
    assert_eq!(String::from_utf8(out).unwrap(), expected.join("\n") + "\n");
    let log = setup.bridge.log();
    assert!(log[0].ends_with(" [wait]"), "{log:?}");
    assert_eq!(log[1..], ["snapshot", "wait", "snapshot"]);
}

/// The fixture snapshot with the keyboard awake as well.
fn both_online() -> Script {
    let mut snapshot = common::snapshot();
    snapshot.receivers[0].devices[1].presence = Presence::Online;
    Script {
        snapshots: vec![snapshot],
        ..Script::default()
    }
}

/// Both awake, and a pairing inventory the bridge couldn't read. Doctor
/// reports HFX-206 with the receiver's id.
fn inventory_unread() -> Script {
    let mut script = both_online();
    script.snapshots[0].receivers[0].inventory = Some(Inventory::Absent);
    script
}

#[test]
fn bench_releases_lease() {
    let setup = Setup::new(Script::default());
    let result = setup.line("bench --delay-us 8000 --delay-us 5000 --iterations 3");
    assert_eq!(result.ok(), 0);
    assert_eq!(
        result.out,
        "Measuring every online device with the firmware query, 3 times per first-GET delay.
Only read-only queries are sent. Nothing on the devices or in HyperFlux Next V2 changes, and each device keeps its default wait.
Skipped Razer DeathStalker V2 Pro TKL (keyboard-0296): asleep.

Razer Basilisk V3 Pro 35K (pointer-00cd), default first GET 3000 us
first GET  retry     ok          median       p90       max   GETs
8000 us    1000 us   3/3         9.0 ms    9.0 ms    9.0 ms    3.0
5000 us    1000 us   3/3         6.0 ms    6.0 ms    6.0 ms    3.0
"
    );
    let log = setup.bridge.log();
    assert!(log[0].ends_with(" [native-routes,bench]"), "{log:?}");
    let calls =
        "snapshot list-routes acquire-lease renew-lease bench renew-lease bench release-lease";
    assert_eq!(log[1..].join(" "), calls);
}

#[test]
fn bench_all_online_devices() {
    let setup = Setup::new(both_online());
    let line = "bench --delay-us 6000 --delay-us 4000 --iterations 3";
    let result = setup.line(line);
    assert_eq!(result.ok(), 0);
    // The keyboard's learned wait, from the snapshot.
    let keyboard = "
Razer DeathStalker V2 Pro TKL (keyboard-0296), default first GET 4500 us
first GET  retry     ok          median       p90       max   GETs
6000 us    1000 us   3/3         7.0 ms    7.0 ms    7.0 ms    3.0
4000 us    1000 us   2/3         5.0 ms    5.0 ms    5.0 ms    3.0
4000 us, retry 1000 us: 1 busy
";
    let out = &result.out;
    assert!(
        out.contains("default first GET 3000 us\n") && out.ends_with(keyboard),
        "{out}"
    );
    let lease = "acquire-lease renew-lease bench renew-lease bench release-lease";
    assert_eq!(
        setup.bridge.log()[3..].join(" "),
        format!("{lease} {lease}")
    );

    let json = setup.line(&format!("--json {line}")).json();
    let devices = json["devices"].as_array().unwrap();
    let ids: Vec<&str> = devices
        .iter()
        .map(|d| d["device_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["pointer-00cd", "keyboard-0296"]);
    assert_eq!(devices[0]["default_first_get_delay_us"], 3000);
    assert_eq!(devices[1]["default_first_get_delay_us"], 4500);
    let short = &devices[1]["results"][1];
    assert_eq!(short["first_get_delay_us"], 4000);
    assert_eq!(
        (&short["counts"]["ok"], &short["counts"]["busy"]),
        (&2.into(), &1.into())
    );
    assert_eq!(json["skipped"], serde_json::json!([]));

    // --device still measures just the one.
    let one = setup.line("--json bench --device keyboard-0296 --iterations 1");
    let devices = one.json()["devices"].as_array().unwrap().clone();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["results"].as_array().unwrap().len(), 5);
}

#[test]
fn bench_retry_grid() {
    let setup = Setup::new(Script::default());
    let line = "bench --device pointer-00cd --delay-us 4000 --delay-us 2000 --retry-us 1000 \
                --retry-us 10000 --iterations 2";
    let result = setup.line(line);
    assert_eq!(result.ok(), 0);
    let rows: Vec<&str> = result.out.lines().skip(5).collect();
    assert_eq!(
        rows,
        [
            "4000 us    1000 us   2/2         5.0 ms    5.0 ms    5.0 ms    3.0",
            "4000 us    10000 us  2/2         8.0 ms    8.0 ms    8.0 ms    1.0",
            "2000 us    1000 us   2/2         3.0 ms    3.0 ms    3.0 ms    3.0",
            "2000 us    10000 us  2/2         6.0 ms    6.0 ms    6.0 ms    1.0",
        ]
    );
    assert!(
        result
            .out
            .contains("2 times per first-GET delay and retry interval.")
    );
    let log = setup.bridge.log();
    assert!(
        log[0].ends_with(" [native-routes,bench,bench-interval]"),
        "{log:?}"
    );
}

#[test]
fn bench_reread_busy() {
    let setup = Setup::new(both_online());
    let line = "bench --device keyboard-0296 --reread-busy --quick --iterations 2";
    let result = setup.line(line);
    assert_eq!(result.ok(), 0);
    assert!(
        result
            .out
            .contains("retry interval, reading again after busy answers.\n")
    );
    let table: Vec<&str> = result.out.lines().skip(4).collect();
    assert_eq!(
        table,
        [
            "first GET  retry     ok          median       p90       max   GETs  busy",
            "2000 us    1000 us   2/2         3.0 ms    3.0 ms    3.0 ms    3.0     0",
            "2000 us    2000 us   2/2         3.0 ms    3.0 ms    3.0 ms    3.0     0",
            "3000 us    1000 us   2/2         4.0 ms    4.0 ms    4.0 ms    3.0     0",
            "3000 us    2000 us   2/2         4.0 ms    4.0 ms    4.0 ms    3.0     0",
            "4000 us    1000 us   2/2         5.0 ms    5.0 ms    5.0 ms    3.0     0",
            "4000 us    2000 us   2/2         5.0 ms    5.0 ms    5.0 ms    3.0     0",
        ]
    );
    // The whole grid starts at 1 ms, where the keyboard can stay busy.
    let json = setup.run(&["--json", "bench", "--reread-busy", "--iterations", "2"]);
    let json = json.json();
    assert_eq!(json["reread_busy"], true);
    let results = json["devices"][1]["results"].as_array().unwrap();
    assert_eq!(results.len(), 20);
    assert_eq!(results[0]["counts"]["busy"], 1);
    let log = setup.bridge.log();
    assert!(log[0].ends_with(" [native-routes,bench,bench-interval]"));
}

#[test]
fn bench_defaults() {
    let setup = Setup::new(both_online());
    let result = setup.line("bench --defaults --iterations 120");
    assert_eq!(result.ok(), 0);
    // At the waits the bridge has learned, 4.5 ms for the keyboard, not 4,
    // and reading on after busy answers as the bridge does.
    let table = "
device         first GET  retry     ok          median       p90       max   GETs
pointer-00cd   3000 us    1000 us   120/120     4.4 ms    4.9 ms    5.0 ms    3.0
keyboard-0296  4500 us    1000 us   120/120     5.9 ms    6.4 ms    6.5 ms    3.0
";
    let out = &result.out;
    let per = "120 times per device, at its default timings.";
    assert!(out.ends_with(table) && out.contains(per), "{out}");
    // 120 runs take two requests, with no interval sent.
    let log = setup.bridge.log();
    assert!(
        log[0].ends_with(" [native-routes,bench,bench-interval]"),
        "{log:?}"
    );
    let lease = "acquire-lease renew-lease bench renew-lease bench";
    assert_eq!(log[3..8].join(" "), lease);
    let json = setup.line("--json bench --defaults --iterations 3").json();
    let results = json["devices"][1]["results"].as_array().unwrap();
    assert_eq!((json["defaults"].as_bool(), results.len()), (Some(true), 1));
    assert_eq!(results[0]["first_get_delay_us"], 4500);
}

#[test]
fn bench_busy_or_missing_device() {
    let setup = Setup::new(Script {
        busy_holder: Some("OpenRGB"),
        ..Script::default()
    });
    let busy = "Razer Basilisk V3 Pro 35K is in use by OpenRGB; try again when it's idle";
    assert_eq!(
        setup.run(&["bench", "--device", "pointer-00cd"]).error(),
        busy
    );
    // Without --device a busy device is skipped, and with nothing left
    // to measure, that is the error.
    assert_eq!(setup.run(&["bench"]).error(), busy);
    let missing = "there's no device pointer-0000; hyperfluxctl devices lists the ids";
    assert_eq!(
        setup.run(&["bench", "--device", "pointer-0000"]).error(),
        missing
    );
}

#[test]
fn stall_check_needs_readable_input() {
    let setup = Setup::new(Script::default());
    let failure = |setup: &Setup| match setup.run(&["stall-check"]).code {
        Err(failure) => (failure.exit, failure.message),
        Ok(code) => panic!("exit {code}"),
    };
    let (exit, why) = failure(&setup);
    assert!(exit == 1 && why.starts_with("no pointer input from a HyperFlux V2 receiver"));
    // Interface 0's mouse node, and the keyboard on interface 1, which has
    // no REL_X or REL_Y.
    for (event, rel, phys) in [("event8", "1943", "input0"), ("event9", "1040", "input1")] {
        let device = format!("/sys/class/input/{event}/device");
        setup
            .tree
            .file(&format!("{device}/id/vendor"), "1532\n")
            .file(&format!("{device}/id/product"), "00cf\n")
            .file(&format!("{device}/capabilities/rel"), format!("{rel}\n"))
            .file(
                &format!("{device}/phys"),
                format!("usb-0000:00:14.0-3/{phys}\n"),
            )
            .file(&format!("/dev/input/{event}"), "");
    }
    // A plain file opens but takes no clock setting.
    let (exit, why) = failure(&setup);
    assert!(exit == 1 && why.starts_with("/dev/input/event8 doesn't take a clock setting"));
    if !rustix::process::geteuid().is_root() {
        setup.tree.mode("/dev/input/event8", 0);
        let (exit, why) = failure(&setup);
        assert!(exit == 3 && why.starts_with("can't read /dev/input/event8: permission denied."));
        assert!(why.contains("usermod -aG input") && why.contains("sudo hyperfluxctl stall-check"));
    }
}

#[test]
fn capture_file() {
    let setup = Setup::new(Script::default());
    let output = setup.tree.path("/capture.jsonl");
    let path = output.display().to_string();
    let line = ["capture", "--seconds", "1", "--output", &path];
    let result = setup.run(&line);
    assert_eq!(result.ok(), 0);
    assert!(
        result
            .out
            .ends_with(&format!("Capture written: {path} (2 events)\n")),
        "{}",
        result.out
    );
    assert_eq!(mode(&output), 0o600);
    let text = std::fs::read_to_string(&output).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(
        (lines[0]["capture"].as_str(), lines[2]["kind"].as_str()),
        (Some("hyperflux-next-capture"), Some("raw"))
    );
    assert_eq!(lines[0]["snapshot"]["revision"], "41");
    assert_eq!(
        lines[0]["snapshot"]["receivers"][0]["receiver_id"],
        "receiver-1"
    );
    assert_eq!(lines[2]["receiver_id"], "receiver-1");
    assert_private(&text);
    assert_eq!(
        setup.run(&line).error(),
        format!("{path} already exists; nothing was overwritten")
    );
    assert_eq!(std::fs::read_to_string(&output).unwrap(), text);
}

#[test]
fn probe_receiver_answers() {
    let setup = Setup::new(Script::default());
    let result = setup.run(&["probe", "receiver"]);
    let zeros = ["00"; 16].join(" ");
    let inventory = format!("01{}", " 5a".repeat(21));
    let expected = format!(
        "receiver-6b1f0e9c2a7d4e83b5c1f2a0\n  08 00:bf  no answer\n  88 00:bf  no answer\n  \
         3f 00:bf  no answer\n  1f 00:c5  status 02, size 3: {zeros}\n  inventory  {inventory}\n"
    );
    assert_eq!((result.ok(), result.out), (0, expected));
    assert!(setup.bridge.log()[0].ends_with(" [probe-receiver]"));
}

#[test]
fn report_file() {
    let setup = Setup::with(inventory_unread(), true);
    let output = setup.tree.path("/report.json");
    let path = output.display().to_string();
    // A short run: the guided capture would otherwise take its default 180 s.
    let result = setup.run(&["report", "--seconds", "1", "--output", &path]);
    assert_eq!(result.ok(), 0);
    assert!(
        result.out.contains("[1/5] Leave the device alone."),
        "{}",
        result.out
    );
    assert!(
        result.out.ends_with(&format!("Report written: {path}\n")),
        "{}",
        result.out
    );
    assert_eq!(mode(&output), 0o600);
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&output).unwrap()).unwrap();
    // Every section is there.
    for key in [
        "devices",
        "capture",
        "bench",
        "doctor",
        "probe_receiver",
        "journal",
    ] {
        assert!(json.get(key).is_some(), "missing {key}");
    }
    assert_eq!(json["capture"]["guide"]["seconds"], 1);
    assert_eq!(json["bench"]["iterations"], 200);
    // The fake runner has no journalctl. The report says the journal
    // couldn't be read rather than leaving it out.
    assert_eq!(json["journal"]["readable"], false);
    // Receiver ids are masked to a stable alias everywhere they appear.
    assert_private(&std::fs::read_to_string(&output).unwrap());
    assert_eq!(
        json["probe_receiver"]["receivers"][0]["receiver_id"],
        "receiver-1"
    );
    assert_shared_doctor(&json["doctor"]);
    // It never overwrites.
    assert!(
        setup
            .run(&["report", "--seconds", "1", "--output", &path])
            .error()
            .contains("already exists")
    );
}

#[test]
fn support_bundle_preview() {
    let setup = Setup::new(Script::default());
    let json = setup.run(&["support-bundle", "--preview"]).json();
    assert_eq!(
        (json["file_written"].as_bool(), json["mode"].as_str()),
        (Some(false), Some("0600"))
    );
    let id = json["bundle_id"].as_str().unwrap();
    assert!(
        id.starts_with("hfx-2") && id.len() == "hfx-20261021T101500Z-1a2b3c4d".len(),
        "{id}"
    );
    assert!(!std::path::Path::new(json["output"].as_str().unwrap()).exists());
}

#[test]
fn support_bundle_file() {
    let setup = Setup::with(inventory_unread(), true);
    setup
        .tree
        .file("/proc/sys/kernel/osrelease", "6.17.1-arch1-1\n")
        .file("/etc/os-release", "NAME=\"Arch Linux\"\nID=arch\n")
        .link(
            "/dev/input/by-id/usb-Razer_Razer_BlackWidow_V3_Pro_SER1AL-if01-event-kbd",
            "../event99",
        );
    let output = setup.tree.path("/bundle.json");
    let path = output.display().to_string();
    let result = setup.run(&["support-bundle", "--output", &path]);
    assert_eq!(
        (result.ok(), result.out),
        (0, format!("Support bundle written: {path}\n"))
    );
    assert_eq!(mode(&output), 0o600);
    let text = std::fs::read_to_string(&output).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        (
            json["host"]["kernel"].as_str(),
            json["host"]["distribution"].as_str()
        ),
        (Some("6.17.1-arch1-1"), Some("arch"))
    );
    assert_eq!(
        json["receivers"][0]["devices"][0]["product_id"],
        "1532:00cd"
    );
    assert!(
        json["doctor"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["code"] == "HFX-407")
    );
    assert_private(&text);
    assert_shared_doctor(&json["doctor"]);
    let again = setup.run(&["support-bundle", "--output", &path]).error();
    assert!(again.starts_with("support bundle failed: "), "{again}");
    assert_eq!(std::fs::read_to_string(&output).unwrap(), text);
}

#[test]
fn client_id_per_run() {
    let setup = Setup::new(Script::default());
    setup.run(&["battery"]).ok();
    setup.run(&["battery"]).ok();
    let hellos: Vec<String> = setup
        .bridge
        .log()
        .into_iter()
        .filter(|l| l.starts_with("hello "))
        .collect();
    assert_eq!(hellos.len(), 2);
    assert_ne!(hellos[0], hellos[1]);
    assert!(hellos[0].starts_with("hello hyperfluxctl-"));
}

#[test]
fn usage_help_and_version() {
    let tree = Tree::new();
    let system = system(&tree, &FakeRunner::default(), &[]);
    let help = run(&system, &["--help"]);
    assert_eq!(
        (help.ok(), help.out.as_str()),
        (0, hyperfluxctl::args::USAGE)
    );
    assert_eq!(
        run(&system, &["--version"]).out,
        "hyperfluxctl (HyperFlux Next V2) 1.0.0\n"
    );
    let failure = run(&system, &["status", "--fix"]).code.unwrap_err();
    assert!(failure.usage);
    assert_eq!(failure.message, "unexpected \"--fix\"");
}
