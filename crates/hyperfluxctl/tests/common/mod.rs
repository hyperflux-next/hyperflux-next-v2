// SPDX-License-Identifier: GPL-2.0-only

//! Test helpers: a small bridge that speaks the real protocol from
//! hfx-proto, a command runner with canned answers, and a fixture tree
//! standing in for /sys, /dev and friends.

#![allow(dead_code)]

use hfx_proto::errors::{ErrorCode, rpc_error, rpc_error_with};
use hfx_proto::framing::{read_frame, write_frame};
use hfx_proto::ids::MonotonicMs;
use hfx_proto::messages::{CaptureResult, DeviceKind, RouteList, Snapshot};
use hfx_proto::negotiation::negotiate;
use hfx_proto::rpc::{Call, Request, decode_request, encode_error, encode_result};
use hyperfluxctl::system::{Output, Runner, System};
use hyperfluxctl::{Context, Failure, Prompt};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

/// The `result` of one of hfx-proto's golden replies.
pub fn fixture<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let path = format!(
        "{}/../hfx-proto/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let reply: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    serde_json::from_value(reply["result"].clone()).unwrap()
}

/// The fixture snapshot 42 minutes after the keyboard's last battery
/// reading. The text then matches the documented example.
pub fn snapshot() -> Snapshot {
    let mut snapshot: Snapshot = fixture("snapshot-reply.json");
    snapshot.now_ms = MonotonicMs::new(1_180_000 + 42 * 60_000).unwrap();
    snapshot
}

pub struct Script {
    /// Served in order; the last one repeats.
    pub snapshots: Vec<Snapshot>,
    pub routes: RouteList,
    pub captures: VecDeque<CaptureResult>,
    pub busy_holder: Option<&'static str>,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            snapshots: vec![snapshot()],
            routes: fixture("list-routes-reply.json"),
            captures: VecDeque::from([fixture("capture-reply.json")]),
            busy_holder: None,
        }
    }
}

struct State {
    script: Script,
    served: usize,
    /// `hello <client id> [<required features>]`, then one method name per request.
    log: Vec<String>,
}

pub struct MockBridge {
    _dir: TempDir,
    pub socket: PathBuf,
    state: Arc<Mutex<State>>,
}

impl MockBridge {
    pub fn start(script: Script) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("bridge.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let state = Arc::new(Mutex::new(State {
            script,
            served: 0,
            log: Vec::new(),
        }));
        let shared = Arc::clone(&state);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = Arc::clone(&shared);
                thread::spawn(move || serve(stream, &state));
            }
        });
        Self {
            _dir: dir,
            socket,
            state,
        }
    }

    pub fn log(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }

    pub fn socket_arg(&self) -> String {
        self.socket.display().to_string()
    }
}

fn serve(mut stream: UnixStream, state: &Mutex<State>) {
    let mut features = Vec::new();
    while let Ok(Some(payload)) = read_frame(&mut stream) {
        let request = decode_request(&payload).expect("hyperfluxctl sends valid requests");
        if write_frame(&mut stream, &answer(&request, state, &mut features)).is_err() {
            return;
        }
    }
}

fn answer(request: &Request, state: &Mutex<State>, features: &mut Vec<String>) -> Vec<u8> {
    let id = &request.id;
    if let Call::Hello(hello) = &request.call {
        let required: Vec<&str> = hello.required_features.iter().map(|f| f.as_str()).collect();
        let entry = format!("hello {} [{}]", hello.client_id, required.join(","));
        state.lock().unwrap().log.push(entry);
        return match negotiate(hello) {
            Ok(negotiated) => {
                features.extend(negotiated.features.iter().map(ToString::to_string));
                let mut welcome: serde_json::Value = fixture("hello-reply.json");
                welcome["features"] = json!(negotiated.features);
                encode_result(id, &welcome).unwrap()
            }
            Err(e) => encode_error(id, &rpc_error_with(e.code(), e.detail().as_deref())).unwrap(),
        };
    }
    let method = request.call.method().expect("known methods only");
    if method
        .feature()
        .is_some_and(|f| !features.iter().any(|g| g == f))
    {
        return encode_error(id, &rpc_error(ErrorCode::FeatureUnavailable)).unwrap();
    }
    let mut guard = state.lock().unwrap();
    let st = &mut *guard;
    st.log.push(method.name().to_owned());
    let script = &mut st.script;
    let result = match &request.call {
        Call::Snapshot => {
            let snapshot = &script.snapshots[st.served.min(script.snapshots.len() - 1)];
            st.served += 1;
            json!(snapshot)
        }
        Call::Wait(p) => {
            let after = p.state_after.map_or(0, |s| s.get());
            let newer = script
                .snapshots
                .get(st.served)
                .map(|s| s.revision.get())
                .filter(|r| *r > after);
            drop(guard);
            if newer.is_none() {
                thread::sleep(Duration::from_millis(20));
            }
            let state = newer.unwrap_or(after).to_string();
            json!({"state": state, "routes": "7", "timed_out": newer.is_none()})
        }
        Call::ListRoutes => json!(script.routes),
        Call::AcquireLease(_) => match script.busy_holder {
            Some(holder) => {
                json!({"outcome": "busy", "lease_id": null, "expires_ms": null, "holder": holder})
            }
            None => {
                json!({"outcome": "granted", "lease_id": "lease-test-1", "expires_ms": "60000", "holder": null})
            }
        },
        Call::RenewLease(p) => json!({"lease_id": p.lease_id, "expires_ms": "120000"}),
        Call::ReleaseLease(_) => json!({}),
        Call::Bench(p) => {
            let delay = u32::from(p.first_get_delay_us.get());
            let keyboard = script.routes.routes.iter().any(|r| {
                r.route_token.as_ref() == Some(&p.route_token) && r.kind == DeviceKind::Keyboard
            });
            let interval = p.get_interval_us.map_or(1000, |i| u32::from(i.get()));
            // A shorter interval answers 3 ms sooner after three reads.
            let (sooner, gets) = if interval < 10_000 { (3000, 3) } else { (0, 1) };
            // As on hardware, the keyboard is sometimes still busy below 5 ms.
            // Reading on after busy leaves that only at 1 ms.
            let busy_below = if p.reread_busy { 2000 } else { 5000 };
            let sample = |i: u16| {
                let elapsed = delay + 4000 + u32::from(i) * 10 - sooner;
                if keyboard && delay < busy_below && i == 1 {
                    json!({"outcome": "device-status", "status": 1, "elapsed_us": elapsed, "gets": 1})
                } else {
                    json!({"outcome": "ok", "status": 2, "elapsed_us": elapsed, "gets": gets})
                }
            };
            let samples: Vec<_> = (0..p.iterations.get()).map(sample).collect();
            json!({"first_get_delay_us": delay, "get_interval_us": interval, "samples": samples})
        }
        Call::Capture(p) => {
            let next = script.captures.pop_front();
            drop(guard);
            next.map_or_else(
                || {
                    thread::sleep(Duration::from_millis(u64::from(p.timeout_ms.get()).min(20)));
                    let next = p.after.map_or(118, |s| s.get()).to_string();
                    json!({"next": next, "gap": false, "events": []})
                },
                |c| json!(c),
            )
        }
        Call::ProbeReceiver(_) => fixture("probe-receiver-reply.json"),
        _ => return encode_error(id, &rpc_error(ErrorCode::FeatureUnavailable)).unwrap(),
    };
    encode_result(id, &result).unwrap()
}

type Effect = Box<dyn Fn() + Send>;

/// Canned command output keyed by the whole command line. Anything else
/// behaves like a program that isn't installed.
#[derive(Clone, Default)]
pub struct FakeRunner {
    answers: Arc<Mutex<HashMap<String, Output>>>,
    effects: Arc<Mutex<HashMap<String, Effect>>>,
    calls: Arc<Mutex<Vec<String>>>,
    visible: Arc<Mutex<Vec<String>>>,
}

impl FakeRunner {
    pub fn answer(&self, command: &str, stdout: &str) -> &Self {
        self.answer_status(command, true, stdout)
    }

    pub fn answer_status(&self, command: &str, success: bool, stdout: &str) -> &Self {
        let output = Output {
            success,
            stdout: stdout.to_owned(),
        };
        self.answers
            .lock()
            .unwrap()
            .insert(command.to_owned(), output);
        self
    }

    pub fn forget(&self, command: &str) {
        self.answers.lock().unwrap().remove(command);
    }

    /// Runs `effect` whenever `command` runs, to play its side effects.
    pub fn on(&self, command: &str, effect: impl Fn() + Send + 'static) {
        self.effects
            .lock()
            .unwrap()
            .insert(command.to_owned(), Box::new(effect));
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    pub fn visible(&self) -> Vec<String> {
        self.visible.lock().unwrap().clone()
    }
}

impl Runner for FakeRunner {
    fn output(&self, argv: &[&str], _timeout: Duration) -> Option<Output> {
        let line = argv.join(" ");
        self.calls.lock().unwrap().push(line.clone());
        if let Some(effect) = self.effects.lock().unwrap().get(&line) {
            effect();
        }
        self.answers.lock().unwrap().get(&line).cloned()
    }

    fn visible(&self, argv: &[String]) -> bool {
        self.visible.lock().unwrap().push(argv.join(" "));
        true
    }
}

/// A fake root directory.
pub struct Tree {
    pub dir: TempDir,
}

impl Tree {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    pub fn path(&self, absolute: &str) -> PathBuf {
        self.dir.path().join(absolute.trim_start_matches('/'))
    }

    fn parent(&self, absolute: &str) -> PathBuf {
        let path = self.path(absolute);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    }

    pub fn file(&self, absolute: &str, content: impl AsRef<[u8]>) -> &Self {
        std::fs::write(self.parent(absolute), content).unwrap();
        self
    }

    pub fn dir(&self, absolute: &str) -> &Self {
        std::fs::create_dir_all(self.path(absolute)).unwrap();
        self
    }

    pub fn link(&self, absolute: &str, target: &str) -> &Self {
        std::os::unix::fs::symlink(target, self.parent(absolute)).unwrap();
        self
    }

    pub fn remove(&self, absolute: &str) {
        let path = self.path(absolute);
        if path.is_dir() && !path.is_symlink() {
            std::fs::remove_dir_all(path).unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
    }

    pub fn mode(&self, absolute: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(self.path(absolute), std::fs::Permissions::from_mode(mode))
            .unwrap();
    }
}

pub const OPENRAZER_GID: u32 = 985;
pub const BUSCTL: &str = "busctl --user --no-pager --no-legend";
pub const HAS_OWNER: &str = "busctl --user --no-pager --no-legend call org.freedesktop.DBus \
                             /org/freedesktop/DBus org.freedesktop.DBus NameHasOwner s org.razer";
pub const SERVICE: &str =
    "systemctl show --property=ActiveState --value hyperflux-next-bridge.service";
pub const KEEPER: &str =
    "systemctl show --property=ActiveState,StatusText hyperflux-next-keeper.service";
/// The running kernel in the fixture.
pub const KERNEL: &str = "7.2.11-arch1-1";
/// Where the receiver's USB interfaces live in the fixture's sysfs.
pub const RECEIVER_USB: &str = "/sys/devices/pci0000:00/0000:00:14.0/usb1/1-3";

/// A machine where everything in Core works: one receiver whose five HID
/// interfaces hid-generic binds, hidraw nodes and /dev/uhid set up as
/// Core's rule does, both services active, a battery entry with a level
/// for both paired devices, and the running kernel installed.
pub fn healthy(tree: &Tree, runner: &FakeRunner) {
    tree.file("/sys/bus/usb/devices/usb1/idVendor", "1d6b\n")
        .file("/sys/bus/usb/devices/usb1/idProduct", "0002\n")
        .file("/sys/bus/usb/devices/1-3/idVendor", "1532\n")
        .file("/sys/bus/usb/devices/1-3/idProduct", "00cf\n")
        .file("/dev/uhid", "");
    tree.mode("/dev/uhid", 0o660);
    for n in 0..5 {
        let hid = format!("{RECEIVER_USB}/1-3:1.{n}/0003:1532:00CF.000{}", n + 1);
        tree.file(
            &format!("{RECEIVER_USB}/1-3:1.{n}/bInterfaceNumber"),
            format!("0{n}\n"),
        )
        .dir(&format!("{hid}/hidraw/hidraw{n}"))
        .link(
            &format!("{hid}/driver"),
            "../../../../../../../bus/hid/drivers/hid-generic",
        )
        .link(
            &format!("/sys/bus/hid/devices/0003:1532:00CF.000{}", n + 1),
            &format!("../../..{}", &hid[4..]),
        )
        .file(&format!("/dev/hidraw{n}"), "");
        // As Core's rule has it: interface 0 read-write for the bridge, its
        // owner, and read-only for the keeper's group, 1 read-only for that
        // group, 2 and 3 root's, and 4 read-write for the bridge's group.
        let mode = [0o640, 0o640, 0o600, 0o600, 0o660][n];
        tree.mode(&format!("/dev/hidraw{n}"), mode);
    }
    for (n, model) in [
        (0, "Razer Basilisk V3 Pro 35K"),
        (1, "Razer DeathStalker V2 Pro TKL"),
    ] {
        let supply = format!("/sys/class/power_supply/hid-hfx{n}-battery");
        tree.file(&format!("{supply}/model_name"), model)
            .file(&format!("{supply}/capacity"), "76\n");
    }
    tree.file("/proc/sys/kernel/osrelease", format!("{KERNEL}\n"))
        .file(&format!("/usr/lib/modules/{KERNEL}/vmlinuz"), "");
    let gid = rustix::process::getegid().as_raw();
    let uid = rustix::process::geteuid().as_raw();
    runner.answer(SERVICE, "active");
    runner.answer(
        KEEPER,
        "ActiveState=active\nStatusText=connected to the bridge",
    );
    // The fixture's files all belong to the test. Its ids stand in for
    // the bridge's and the keeper's.
    runner.answer(
        "getent group hyperflux-next",
        &format!("hyperflux-next:x:{gid}:"),
    );
    runner.answer(
        "getent group hyperflux-keeper",
        &format!("hyperflux-keeper:x:{gid}:"),
    );
    runner.answer(
        "getent passwd hyperflux-next",
        &format!("hyperflux-next:x:{uid}:{gid}::/:/usr/bin/nologin"),
    );
    runner.answer("id -un", "robin");
}

/// Full installed and working on top of [`healthy`]: patched OpenRazer
/// modules loaded, openrazer-daemon listing both devices, the keyboard's
/// key-press link in place and the OpenRGB plugin installed.
pub fn full(tree: &Tree, runner: &FakeRunner) {
    let component = format!(
        r#"{{"component":"full","version":"{}"}}"#,
        hfx_proto::PRODUCT_VERSION
    );
    let key_link = "/dev/input/by-id/hyperflux-next-pci-0000_00_14_0-usb-0_3_1_1-\
                    DSV2Pro_TKL_000000000000-if01-event-mouse";
    tree.file("/usr/share/hyperflux-next/components/full.json", component)
        .file(hfx_proto::paths::OPENRGB_PLUGIN, "")
        .file("/usr/bin/openrazer-daemon", "")
        .file("/dev/input/event7", "")
        .link(key_link, "../event7");
    for module in hfx_proto::openrazer::MODULES {
        tree.file(&format!("/sys/module/{module}/srcversion"), "A1B2C3\n");
        runner.answer(&format!("modinfo -F srcversion {module}"), "A1B2C3");
    }
    let capability = "razer-feature-report-v1";
    for module in hfx_proto::openrazer::PATCHED_MODULES {
        tree.file(
            &format!("/sys/module/{module}/parameters/hyperflux_native_transport"),
            capability,
        );
        runner.answer(
            &format!("modinfo -F hyperflux_native_transport {module}"),
            capability,
        );
    }
    runner.answer(
        "getent group plugdev",
        &format!("plugdev:x:{OPENRAZER_GID}:robin"),
    );
    runner.answer(HAS_OWNER, "b true");
    let razer = format!("{BUSCTL} --auto-start=no call org.razer");
    let devices = format!("{razer} /org/razer razer.devices getDevices");
    runner.answer(&devices, r#"as 2 "PM2143H0001" "IO2219F0002""#);
    for (serial, product) in [("PM2143H0001", 205), ("IO2219F0002", 662)] {
        let call = format!("{razer} /org/razer/device/{serial} razer.device.misc getVidPid");
        runner.answer(&call, &format!("ai 2 5426 {product}"));
    }
}

pub struct ScriptedPrompt {
    pub terminal: bool,
    pub answers: VecDeque<bool>,
    pub questions: Vec<String>,
}

impl ScriptedPrompt {
    pub fn new(terminal: bool, answers: &[bool]) -> Self {
        Self {
            terminal,
            answers: answers.iter().copied().collect(),
            questions: Vec::new(),
        }
    }
}

impl Prompt for ScriptedPrompt {
    fn interactive(&self) -> bool {
        self.terminal
    }

    fn confirm(&mut self, question: &str) -> bool {
        self.questions.push(question.to_owned());
        self.answers.pop_front().unwrap_or(false)
    }
}

pub struct Run {
    pub code: Result<i32, Failure>,
    pub out: String,
}

impl Run {
    pub fn ok(&self) -> i32 {
        match &self.code {
            Ok(code) => *code,
            Err(failure) => panic!("failed: {}\noutput:\n{}", failure.message, self.out),
        }
    }

    pub fn error(&self) -> String {
        match &self.code {
            Ok(code) => panic!("expected a failure, got exit {code}\n{}", self.out),
            Err(failure) => failure.message.clone(),
        }
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.out).unwrap()
    }
}

/// A normal user (uid 1000) on the fixture tree.
pub fn system(tree: &Tree, runner: &FakeRunner, groups: &[u32]) -> System {
    System::new(
        tree.dir.path(),
        Box::new(runner.clone()),
        1000,
        groups.to_vec(),
    )
}

/// Runs one command line against `system`, collecting stdout.
pub fn run_with(system: &System, prompt: &mut dyn Prompt, line: &[&str]) -> Run {
    let args: Vec<String> = line.iter().map(|s| (*s).to_owned()).collect();
    let mut out = Vec::new();
    let code = hyperfluxctl::run(
        &args,
        &mut Context {
            out: &mut out,
            system,
            prompt,
        },
    );
    Run {
        code,
        out: String::from_utf8(out).unwrap(),
    }
}

pub fn run(system: &System, line: &[&str]) -> Run {
    run_with(system, &mut ScriptedPrompt::new(false, &[]), line)
}
