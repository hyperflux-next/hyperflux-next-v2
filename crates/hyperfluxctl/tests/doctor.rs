// SPDX-License-Identifier: GPL-2.0-only

//! Doctor's probes against a fixture tree and a fake command runner, and
//! `--fix` never running anything without a yes.

mod common;

use common::{
    BUSCTL, FakeRunner, HAS_OWNER, MockBridge, OPENRAZER_GID, SERVICE, Script, ScriptedPrompt, Tree,
};
use hfx_proto::client::PeerCheck;
use hfx_proto::messages::Inventory;
use hyperfluxctl::bridge::{Candidate, Reach, reach};
use hyperfluxctl::system::System;

const EFIVAR: &str = "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-e2b9b7dc6b0b";
const COMPONENT: &str = "/usr/share/hyperflux-next/components/full.json";

struct Machine {
    bridge: MockBridge,
    tree: Tree,
    runner: FakeRunner,
    groups: Vec<u32>,
}

impl Machine {
    fn core() -> Self {
        Self::with_bridge(Script::default())
    }

    fn with_bridge(script: Script) -> Self {
        let (tree, runner) = (Tree::new(), FakeRunner::default());
        common::healthy(&tree, &runner);
        Self {
            bridge: MockBridge::start(script),
            tree,
            runner,
            groups: Vec::new(),
        }
    }

    fn with_full() -> Self {
        let machine = Self::core();
        common::full(&machine.tree, &machine.runner);
        Self {
            groups: vec![OPENRAZER_GID],
            ..machine
        }
    }

    fn doctor(&self, extra: &[&str]) -> common::Run {
        self.doctor_with(&mut ScriptedPrompt::new(false, &[]), extra)
    }

    fn doctor_with(&self, prompt: &mut ScriptedPrompt, extra: &[&str]) -> common::Run {
        let socket = self.bridge.socket_arg();
        let line = [&["doctor", "--socket", &socket][..], extra].concat();
        common::run_with(
            &common::system(&self.tree, &self.runner, &self.groups),
            prompt,
            &line,
        )
    }

    fn findings(&self) -> Vec<serde_json::Value> {
        self.doctor(&["--json"]).json()["findings"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn codes(&self) -> Vec<String> {
        self.findings()
            .iter()
            .map(|f| f["code"].as_str().unwrap().to_owned())
            .collect()
    }
}

#[test]
fn ready_with_and_without_full() {
    let machine = Machine::with_full();
    let result = machine.doctor(&[]);
    assert_eq!(result.ok(), 0, "{}", result.out);
    let ready = "HyperFlux Next V2 is ready.\nDriver active, bridge running, 1 receiver.\n";
    assert_eq!(result.out, format!("{ready}No action required.\n"));
    let core = Machine::core().doctor(&[]);
    assert_eq!(
        core.out,
        format!("{ready}Optional Full integration is not installed.\nNo action required.\n")
    );
    // Ubuntu, Fedora and openSUSE: Full without the separate plugin package.
    machine.tree.remove(hfx_proto::paths::OPENRGB_PLUGIN);
    let without = machine.doctor(&[]);
    assert_eq!(without.ok(), 0, "{}", without.out);
    assert_eq!(
        without.out,
        format!("{ready}Optional OpenRGB plugin is not installed.\nNo action required.\n")
    );
}

#[test]
fn old_2x_driver() {
    let machine = Machine::core();
    machine
        .tree
        .file("/sys/module/hid_hyperflux_next/version", "2.2.0\n");
    machine.runner.answer(
        "pacman -Q hyperflux-next-dkms",
        "hyperflux-next-dkms 2.2.0-1",
    );
    let json = machine.doctor(&["--json"]).json();
    assert_eq!(json["findings"][0]["code"], "HFX-303");
    assert_eq!(
        json["fixes"][0]["command"],
        "sudo pacman -R hyperflux-next-dkms"
    );
    // Only a DKMS registration with its source left, which DKMS builds for
    // the next kernel: remove that.
    machine.runner.forget("pacman -Q hyperflux-next-dkms");
    machine
        .tree
        .file("/usr/src/hid-hyperflux-next-2.2.0/dkms.conf", "")
        .link(
            "/var/lib/dkms/hid-hyperflux-next/2.2.0/source",
            "/usr/src/hid-hyperflux-next-2.2.0",
        );
    let json = machine.doctor(&["--json"]).json();
    assert_eq!(
        json["fixes"][0]["command"],
        "sudo dkms remove hid-hyperflux-next/2.2.0 --all"
    );
    // 1.0 has no module of its own. A 1.0.0 build counts as well.
    machine
        .tree
        .remove("/var/lib/dkms/hid-hyperflux-next/2.2.0");
    machine.tree.remove("/usr/src/hid-hyperflux-next-2.2.0");
    machine
        .tree
        .file("/sys/module/hid_hyperflux_next/version", "1.0.0\n");
    assert_eq!(machine.codes(), ["HFX-303"]);
    machine.tree.remove("/sys/module/hid_hyperflux_next");
    assert!(machine.codes().is_empty());
}

/// Doctor's HFX-303 finding and the fix it offers for the 2.x package.
fn old_package(machine: &Machine) -> (String, String) {
    let json = machine.doctor(&["--json"]).json();
    let code = json["findings"][0]["code"].as_str().unwrap_or("none");
    let fix = json["fixes"][0]["command"].as_str().unwrap_or("none");
    (code.to_owned(), fix.to_owned())
}

fn nothing() -> (String, String) {
    ("none".into(), "none".into())
}

#[test]
fn pacman_provider() {
    let machine = Machine::core();
    let query = "pacman -Q hyperflux-next-dkms";
    // What pacman prints with hyperflux-next-v2-linux installed.
    machine
        .runner
        .answer(query, "hyperflux-next-v2-linux 1.0.0-1");
    assert_eq!(old_package(&machine), nothing());
    machine.runner.answer(query, "hyperflux-next-dkms 2.2.0-1");
    let fix = "sudo pacman -R hyperflux-next-dkms";
    assert_eq!(old_package(&machine), ("HFX-303".into(), fix.into()));
}

#[test]
fn dpkg_provided_name() {
    let machine = Machine::core();
    let query = "dpkg-query -W -f=${Package}:${Status} hyperflux-next-dkms";
    // Provided by hyperflux-next-v2-linux, and the 2.x package removed
    // with its configuration kept.
    for status in ["unknown ok not-installed", "deinstall ok config-files"] {
        machine
            .runner
            .answer(query, &format!("hyperflux-next-dkms:{status}"));
        assert_eq!(old_package(&machine), nothing(), "{status}");
    }
    machine
        .runner
        .answer(query, "hyperflux-next-dkms:install ok installed");
    let fix = "sudo apt remove hyperflux-next-dkms";
    assert_eq!(old_package(&machine), ("HFX-303".into(), fix.into()));
}

#[test]
fn rpm_installed_names() {
    let machine = Machine::core();
    let query = "rpm -q --qf %{NAME} hyperflux-next-dkms";
    // rpm -q doesn't look at what hyperflux-next-v2-linux provides.
    let missing = "package hyperflux-next-dkms is not installed";
    machine.runner.answer_status(query, false, missing);
    assert_eq!(old_package(&machine), nothing());
    machine.runner.answer(query, "hyperflux-next-dkms");
    let fix = "sudo rpm -e hyperflux-next-dkms";
    assert_eq!(old_package(&machine), ("HFX-303".into(), fix.into()));
}

/// What the test machine had: a July build of the 2.x module for
/// two kernels that are long gone, without source, alongside the leftover
/// module directory of a removed kernel.
fn inert_dkms(machine: &Machine) {
    for kernel in ["6.18.37-1-lts", "7.0.14-arch1-1"] {
        machine.tree.file(
            &format!(
                "/var/lib/dkms/hid-hyperflux-next/0.0.0-dev.1/{kernel}/x86_64/module/hid-hyperflux-next.ko"
            ),
            "",
        );
    }
    machine.tree.file(
        "/usr/lib/modules/6.18.22-1-lts/updates/dkms/hid-hyperflux-next.ko.zst",
        "",
    );
}

#[test]
fn inert_dkms_records() {
    let machine = Machine::core();
    inert_dkms(&machine);
    let result = machine.doctor(&[]);
    assert!(
        result
            .out
            .starts_with("HyperFlux Next V2 is ready, with 1 warning.\n")
            && result.out.contains("\nWarning: [HFX-305]")
            && result
                .out
                .contains("sudo rm -r /var/lib/dkms/hid-hyperflux-next.")
            && !result.out.contains("package"),
        "{}",
        result.out
    );
    let json = machine.doctor(&["--json"]).json();
    assert_eq!(json["findings"][0]["severity"], "warning");
    assert_eq!(
        json["fixes"][0]["command"],
        "sudo rm -r /var/lib/dkms/hid-hyperflux-next"
    );
    // --fix never deletes without a yes.
    let mut prompt = ScriptedPrompt::new(true, &[false]);
    machine.doctor_with(&mut prompt, &["--fix", "--yes"]);
    assert_eq!(
        prompt.questions,
        ["Run `sudo rm -r /var/lib/dkms/hid-hyperflux-next` as root? [y/N] "]
    );
    assert!(machine.runner.visible().is_empty());
    // A source tree without dkms.conf can't build either; it goes too.
    machine.tree.dir("/usr/src/hid-hyperflux-next-0.0.0-dev.1");
    assert_eq!(
        machine.findings()[0]["details"]["paths"],
        "/var/lib/dkms/hid-hyperflux-next /usr/src/hid-hyperflux-next-0.0.0-dev.1"
    );
    // Something real comes first.
    machine
        .tree
        .remove("/sys/class/power_supply/hid-hfx1-battery");
    assert_eq!(machine.codes(), ["HFX-205", "HFX-305"]);
}

#[test]
fn loadable_dkms_records() {
    let machine = Machine::core();
    inert_dkms(&machine);
    // A module file for the running kernel.
    let file = "/usr/lib/modules/7.2.11-arch1-1/updates/dkms/hid-hyperflux-next.ko.zst";
    machine.tree.file(file, "");
    let finding = &machine.findings()[0];
    assert_eq!(finding["code"], "HFX-303");
    assert_eq!(finding["details"]["registration"], file);
    assert!(!finding["action"].as_str().unwrap().contains("package"));
    machine.tree.remove(file);
    assert_eq!(machine.codes(), ["HFX-305"]);
    // DKMS installed it for a kernel that is still installed.
    machine.tree.link(
        "/var/lib/dkms/hid-hyperflux-next/kernel-6.18.37-1-lts-x86_64",
        "0.0.0-dev.1/6.18.37-1-lts",
    );
    assert_eq!(machine.codes(), ["HFX-305"], "that kernel is gone");
    // Installed the way Debian does it: the image in /boot.
    machine
        .tree
        .dir("/usr/lib/modules/6.18.37-1-lts/kernel")
        .file("/boot/vmlinuz-6.18.37-1-lts", "");
    let json = machine.doctor(&["--json"]).json();
    assert_eq!(json["findings"][0]["code"], "HFX-303");
    assert_eq!(
        json["fixes"][0]["command"],
        "sudo dkms remove hid-hyperflux-next/0.0.0-dev.1 --all"
    );
}

#[test]
fn receiver_without_hidraw() {
    let machine = Machine::core();
    // The old driver holds interface 1 and gives it no hidraw node.
    let hid = format!("{}/1-3:1.1/0003:1532:00CF.0002", common::RECEIVER_USB);
    machine.tree.remove(&format!("{hid}/hidraw"));
    machine.tree.remove(&format!("{hid}/driver"));
    machine.tree.link(
        &format!("{hid}/driver"),
        "../../../../../../../bus/hid/drivers/hid-hyperflux-next",
    );
    machine
        .tree
        .file("/sys/module/hid_hyperflux_next/version", "2.2.0\n");
    // The bridge doesn't matter while it can't open the receiver.
    machine.runner.answer(SERVICE, "failed");
    assert_eq!(machine.codes(), ["HFX-303", "HFX-202"]);
    let system = common::system(&machine.tree, &machine.runner, &[]);
    let status = common::run(
        &system,
        &["status", "--socket", &machine.bridge.socket_arg()],
    );
    assert!(
        status
            .out
            .starts_with("Driver: owned by hid-hyperflux-next\n"),
        "{}",
        status.out
    );
}

#[test]
fn hidraw_and_uhid_group() {
    let machine = Machine::core();
    machine.tree.mode("/dev/hidraw4", 0o666);
    let findings = machine.findings();
    assert_eq!(findings[0]["code"], "HFX-202");
    let receiver = findings[0]["details"]["receiver"].as_str().unwrap();
    assert!(
        receiver.starts_with("/dev/hidraw4 is group ") && receiver.ends_with(", mode 0666"),
        "{receiver}"
    );
    machine.tree.mode("/dev/hidraw4", 0o660);
    machine.tree.mode("/dev/uhid", 0o600);
    assert_eq!(machine.codes(), ["HFX-408"]);
}

#[test]
fn keeper_unreachable() {
    let machine = Machine::core();
    machine
        .runner
        .answer(common::KEEPER, "ActiveState=failed\nStatusText=");
    // Missing batteries follow from it and are not reported on their own.
    machine
        .tree
        .remove("/sys/class/power_supply/hid-hfx1-battery");
    let json = machine.doctor(&["--json"]).json();
    assert_eq!(json["findings"][0]["code"], "HFX-208");
    assert_eq!(json["findings"].as_array().unwrap().len(), 1);
    assert_eq!(json["findings"][0]["details"]["problem"], "is failed");
    assert_eq!(
        json["fixes"][0]["command"],
        "sudo systemctl restart hyperflux-next-keeper.service"
    );
    assert_eq!(json["keeper"], "stopped");
    let waiting = "ActiveState=active\nStatusText=waiting for the bridge";
    machine.runner.answer(common::KEEPER, waiting);
    let json = machine.doctor(&["--json"]).json();
    let problem = &json["findings"][0]["details"]["problem"];
    assert_eq!(problem, "is running, but the bridge doesn't have it");
}

#[test]
fn interface_0_owner() {
    let machine = Machine::core();
    // Group-writable, as the old rule made it: the keeper could write it.
    machine.tree.mode("/dev/hidraw0", 0o660);
    let findings = machine.findings();
    assert_eq!(findings[0]["code"], "HFX-202");
    let receiver = findings[0]["details"]["receiver"].as_str().unwrap();
    assert!(receiver.starts_with("/dev/hidraw0 is user "), "{receiver}");
    // Nodes of another group than the keeper's aren't the keeper's.
    machine.tree.mode("/dev/hidraw0", 0o640);
    machine
        .runner
        .answer("getent group hyperflux-keeper", "hyperflux-keeper:x:4242:");
    assert_eq!(machine.codes(), ["HFX-202", "HFX-408"]);
}

#[test]
fn missing_battery_entry() {
    let machine = Machine::core();
    machine
        .tree
        .remove("/sys/class/power_supply/hid-hfx1-battery");
    let result = machine.doctor(&[]);
    // Only a warning: ready, but still a finding for the exit status.
    assert_eq!(result.ok(), 2);
    assert!(
        result
            .out
            .starts_with("HyperFlux Next V2 is ready, with 1 warning.\n"),
        "{}",
        result.out
    );
    assert!(
        result.out.contains("\nWarning: [HFX-205]"),
        "{}",
        result.out
    );
    let finding = &machine.findings()[0];
    assert_eq!(
        finding["details"]["device"],
        "Razer DeathStalker V2 Pro TKL"
    );
    assert_eq!(finding["severity"], "warning");
    assert_eq!(
        machine.doctor(&["--json"]).json()["state"],
        "ready-with-warnings"
    );
    let socket = machine.bridge.socket_arg();
    let status = common::run_with(
        &common::system(&machine.tree, &machine.runner, &machine.groups),
        &mut ScriptedPrompt::new(false, &[]),
        &["status", "--socket", &socket],
    );
    assert_eq!(status.ok(), 0);
    assert!(
        status
            .out
            .ends_with("\nNext: no action required (1 warning: run hyperfluxctl doctor)\n"),
        "{}",
        status.out
    );
}

#[test]
fn inventory_not_recognised() {
    let mut snapshot = common::snapshot();
    snapshot.receivers[0].inventory = Some(Inventory::Absent);
    let script = Script {
        snapshots: vec![snapshot],
        ..Script::default()
    };
    let finding = &Machine::with_bridge(script).findings()[0];
    let what = (&finding["code"], &finding["details"]["problem"]);
    assert_eq!(what, (&"HFX-206".into(), &"couldn't be read".into()));
}

#[test]
fn battery_entry_without_level() {
    let machine = Machine::core();
    // hid-input answers ENODATA until it has had a battery report.
    machine
        .tree
        .remove("/sys/class/power_supply/hid-hfx1-battery/capacity");
    let finding = &machine.findings()[0];
    assert_eq!(finding["code"], "HFX-205");
    assert_eq!(
        finding["details"]["problem"],
        "its power supply entry has no level to read"
    );
}

/// Gives `path` an ACL like the one logind applies for a uaccess tag:
/// owner, uid 1000 read-write, group, mask and other. False when the
/// temporary directory's file system has no ACLs.
fn uaccess_acl(machine: &Machine, path: &str) -> bool {
    let mut acl = 2_u32.to_le_bytes().to_vec();
    for (tag, perm, id) in [
        (1_u16, 6_u16, u32::MAX),
        (2, 6, 1000),
        (4, 6, u32::MAX),
        (0x10, 6, u32::MAX),
        (0x20, 0, u32::MAX),
    ] {
        acl.extend(
            tag.to_le_bytes()
                .iter()
                .chain(&perm.to_le_bytes())
                .chain(&id.to_le_bytes()),
        );
    }
    let flags = rustix::fs::XattrFlags::empty();
    let path = machine.tree.path(path);
    rustix::fs::setxattr(path, "system.posix_acl_access", &acl, flags).is_ok()
}

#[test]
fn uhid_acl_for_a_user() {
    let machine = Machine::with_full();
    if !uaccess_acl(&machine, "/dev/uhid") {
        return;
    }
    let json = machine.doctor(&["--json"]).json();
    let findings = json["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(
        (
            findings[0]["code"].as_str(),
            findings[0]["details"]["uid"].as_str()
        ),
        (Some("HFX-409"), Some("1000"))
    );
    // Changing udev rules is the user's call.
    assert!(json["fixes"].as_array().unwrap().is_empty());
}

#[test]
fn lamparray_group_or_mode() {
    let mut machine = Machine::core();
    assert!(machine.codes().is_empty());
    // The fixture's own group stands in for hyperflux-next.
    machine.groups.push(rustix::process::getegid().as_raw());
    let finding = &machine.findings()[0];
    assert_eq!(
        (finding["code"].as_str(), finding["severity"].as_str()),
        (Some("HFX-410"), Some("warning"))
    );
    let why =
        "/dev/hidraw4 (interface 4) belongs to the hyperflux-next group, which this login is in";
    assert_eq!(finding["details"]["why"], why);
    machine.groups.clear();
    machine.tree.mode("/dev/hidraw3", 0o666);
    let why = "/dev/hidraw3 (interface 3) has mode 0666, open to everyone";
    assert_eq!(machine.findings()[0]["details"]["why"], why);
    // root can open anything. A person's login is what counts.
    let runner = Box::new(machine.runner.clone());
    let root = System::new(machine.tree.dir.path(), runner, 0, Vec::new());
    let socket = machine.bridge.socket_arg();
    let json = common::run(&root, &["doctor", "--json", "--socket", &socket]).json();
    assert_eq!(json["findings"], serde_json::json!([]));
    // Nothing to say about a node that isn't there.
    machine.tree.remove("/dev/hidraw3");
    assert!(machine.codes().is_empty());
}

#[test]
fn lamparray_open_by_acl() {
    let machine = Machine::core();
    if !uaccess_acl(&machine, "/dev/hidraw3") {
        return;
    }
    let result = machine.doctor(&[]);
    let warning = "\nWarning: [HFX-410] Programs you run can open the receiver's lighting \
                   interfaces, so OpenRGB's LampArray driver would show your devices twice.\n\
                   Why: /dev/hidraw3 (interface 3) has an ACL entry for uid 1000. Interfaces 3 \
                   and 4 are the receiver's HID LampArrays,";
    assert!(result.out.contains(warning), "{}", result.out);
    let leave = "\nDo: If this login is in the hyperflux-next group, leave it: sudo gpasswd -d";
    assert!(result.out.contains(leave), "{}", result.out);
}

#[test]
fn secure_boot_unsigned_module() {
    let machine = Machine::with_full();
    machine.tree.remove("/sys/module/razerkbd");
    machine.tree.file(EFIVAR, [6, 0, 0, 0, 1]);
    machine.runner.answer("modinfo -F signer razerkbd", "");
    let finding = &machine.findings()[0];
    assert_eq!(
        (
            finding["code"].as_str(),
            finding["details"]["module"].as_str()
        ),
        (Some("HFX-304"), Some("razerkbd"))
    );
    // Signed, but with a DKMS key the firmware does not know.
    machine
        .runner
        .answer("modinfo -F signer razerkbd", "DKMS module signing key");
    machine.tree.file("/var/lib/dkms/mok.pub", "key");
    let test_key = "mokutil --test-key /var/lib/dkms/mok.pub";
    machine
        .runner
        .answer_status(test_key, false, "/var/lib/dkms/mok.pub is not enrolled");
    assert_eq!(machine.codes(), ["HFX-304"]);
    machine
        .runner
        .answer(test_key, "/var/lib/dkms/mok.pub is already enrolled");
    assert!(machine.codes().is_empty());
    // With Secure Boot off an unloaded module is fine too.
    machine.tree.file(EFIVAR, [6, 0, 0, 0, 0]);
    machine.runner.answer("modinfo -F signer razerkbd", "");
    assert!(machine.codes().is_empty());
}

#[test]
fn receiver_not_registered() {
    let mut snapshot = common::snapshot();
    snapshot.receivers.clear();
    let machine = Machine::with_bridge(Script {
        snapshots: vec![snapshot],
        ..Script::default()
    });
    assert_eq!(machine.codes(), ["HFX-202"]);
}

#[test]
fn login_without_socket_access() {
    let dir = tempfile::tempdir().unwrap();
    let system_socket = dir.path().join("bridge.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&system_socket).unwrap();
    let no_access = std::os::unix::fs::PermissionsExt::from_mode(0o000);
    std::fs::set_permissions(&system_socket, no_access).unwrap();
    let user_socket = dir.path().join("user.sock");
    let candidates = [
        Candidate {
            path: user_socket.clone(),
            peer: PeerCheck::Uid(0),
            per_user: true,
        },
        Candidate {
            path: system_socket,
            peer: PeerCheck::SocketOwner,
            per_user: false,
        },
    ];
    match reach(&candidates, &[]) {
        Reach::NoAccess {
            user_socket: missing,
        } => assert_eq!(missing, user_socket),
        Reach::Up(_) => panic!("connected through a socket with mode 0000"),
        // root ignores file modes. The system socket is open to it.
        Reach::Down(why) => assert_eq!(rustix::process::geteuid().as_raw(), 0, "{why}"),
    }
}

#[test]
fn per_user_socket_first() {
    let bridge = MockBridge::start(Script::default());
    let dir = tempfile::tempdir().unwrap();
    let candidates = [
        Candidate {
            path: dir.path().join("missing.sock"),
            peer: PeerCheck::Any,
            per_user: true,
        },
        Candidate {
            path: bridge.socket.clone(),
            peer: PeerCheck::Any,
            per_user: false,
        },
    ];
    assert!(matches!(reach(&candidates, &[]), Reach::Up(_)));
}

#[test]
fn openrazer_modules() {
    let machine = Machine::with_full();
    machine
        .runner
        .answer("modinfo -F hyperflux_native_transport razermouse", "");
    assert_eq!(machine.codes(), ["HFX-402"]);
    machine.runner.answer(
        "modinfo -F hyperflux_native_transport razermouse",
        "razer-feature-report-v1",
    );
    machine
        .tree
        .remove("/sys/module/razerkbd/parameters/hyperflux_native_transport");
    assert_eq!(machine.codes(), ["HFX-401"]);
    machine.runner.forget("modinfo -F srcversion razerkraken");
    assert_eq!(machine.findings()[0]["details"]["module"], "razerkraken");
}

#[test]
fn full_version_mismatch() {
    let machine = Machine::with_full();
    machine
        .tree
        .file(COMPONENT, r#"{"component":"full","version":"0.9.0"}"#);
    assert_eq!(machine.codes(), ["HFX-405"]);
}

#[test]
fn broken_link_is_named() {
    let machine = Machine::with_full();
    let link = "/dev/input/by-id/usb-Razer_Razer_BlackWidow_V3_Pro-if01-event-kbd";
    machine.tree.link(link, "../event42");
    machine.tree.link(
        "/dev/input/by-id/usb-Other_Keyboard-event-kbd",
        "../event43",
    );
    let result = machine.doctor(&[]);
    assert_eq!(result.ok(), 2);
    let advice =
        format!("Do: Run: sudo rm {link} and then: systemctl --user restart openrazer-daemon.");
    assert!(result.out.contains(&advice), "{}", result.out);
    assert_eq!(machine.codes(), ["HFX-407"]);
}

#[test]
fn openrazer_access() {
    let mut machine = Machine::with_full();
    machine.runner.forget("getent group plugdev");
    assert_eq!(machine.codes(), ["HFX-403"]);
    machine.runner.answer(
        "getent group plugdev",
        &format!("plugdev:x:{OPENRAZER_GID}:"),
    );
    machine.groups.clear();
    assert_eq!(machine.codes(), ["HFX-404"]);
    machine.groups.push(OPENRAZER_GID);
    machine.runner.answer(HAS_OWNER, "b false");
    assert_eq!(
        machine.findings()[0]["details"]["access"],
        "daemon not running"
    );
    // openrazer-daemon runs but doesn't list the mouse.
    machine.runner.answer(HAS_OWNER, "b true");
    let devices =
        format!("{BUSCTL} --auto-start=no call org.razer /org/razer razer.devices getDevices");
    machine.runner.answer(&devices, r#"as 1 "IO2219F0002""#);
    let finding = &machine.findings()[0];
    assert_eq!(
        (
            finding["code"].as_str(),
            finding["details"]["missing"].as_str()
        ),
        (Some("HFX-404"), Some("1532:00cd"))
    );
}

#[test]
fn keyboard_not_awake_yet() {
    let machine = Machine::with_full();
    // The fixture's keyboard is asleep, and openrazer-daemon lists only the
    // mouse: the bridge cannot read the keyboard's serial until it wakes.
    let devices =
        format!("{BUSCTL} --auto-start=no call org.razer /org/razer razer.devices getDevices");
    machine.runner.answer(&devices, r#"as 1 "PM2143H0001""#);
    let result = machine.doctor(&[]);
    assert_eq!(result.ok(), 0, "{}", result.out);
    assert!(
        result.out.contains(
            "Waiting for Razer DeathStalker V2 Pro TKL to wake; OpenRazer lists it once it answers."
        ),
        "{}",
        result.out
    );
    let json = machine.doctor(&["--json"]).json();
    assert_eq!(json["notes"].as_array().unwrap().len(), 1);
}

#[test]
fn key_link_missing() {
    let machine = Machine::with_full();
    machine.tree.remove("/dev/input/event7");
    // The dangling link is also one OpenRazer would trip over.
    let findings = machine.findings();
    assert_eq!(
        (findings[0]["code"].as_str(), findings[1]["code"].as_str()),
        (Some("HFX-407"), Some("HFX-406"))
    );
    let pattern = "/dev/input/by-id/hyperflux-next-*-DSV2Pro_TKL_000000000000-if01-event-mouse";
    assert_eq!(findings[1]["details"]["link"], pattern);
    // An admin's own rules file means the link is theirs to manage.
    machine.tree.file(
        "/etc/udev/rules.d/70-hyperflux-next-openrazer-keys.rules",
        "",
    );
    assert_eq!(machine.codes(), ["HFX-407"]);
}

#[test]
fn explain_lists_all() {
    let machine = Machine::core();
    for version in ["0.1.0", "0.1.1"] {
        machine
            .tree
            .file(
                &format!("/usr/src/hid-razer-hyperflux-v2-{version}/dkms.conf"),
                "",
            )
            .dir(&format!("/var/lib/dkms/hid-razer-hyperflux-v2/{version}"));
    }
    let result = machine.doctor(&["--explain"]);
    assert_eq!(result.ok(), 2);
    assert_eq!(result.out.matches("[HFX-303]").count(), 2);
    assert!(!result.out.contains("More:"));
    let json = machine.doctor(&["--json"]).json();
    assert_eq!(
        (
            json["state"].as_str(),
            json["findings"][0]["severity"].as_str()
        ),
        (Some("ready-with-warnings"), Some("warning"))
    );
    assert_eq!(
        json["findings"][0]["documentation"],
        "docs/user/troubleshooting.md#hfx-303"
    );
    assert_eq!(
        json["fixes"][0]["command"],
        "sudo dkms remove hid-razer-hyperflux-v2/0.1.0 --all"
    );
}

/// Two findings with fixes that need root: a retired DKMS registration
/// and a user outside the OpenRazer group.
fn needs_two_root_fixes() -> Machine {
    let machine = Machine::with_full();
    machine
        .tree
        .file("/usr/src/hid-razer-hyperflux-v2-0.1.2/dkms.conf", "")
        .dir("/var/lib/dkms/hid-razer-hyperflux-v2/0.1.2");
    Machine {
        groups: Vec::new(),
        ..machine
    }
}

const DKMS_FIX: &str = "sudo dkms remove hid-razer-hyperflux-v2/0.1.2 --all";
const GROUP_FIX: &str = "sudo gpasswd -a robin plugdev";

#[test]
fn fixes_without_terminal() {
    let machine = needs_two_root_fixes();
    let mut prompt = ScriptedPrompt::new(false, &[true, true]);
    let result = machine.doctor_with(&mut prompt, &["--fix", "--yes"]);
    assert_eq!(result.ok(), 2);
    let listed = format!(
        "\nFixes:\n  [HFX-303] {DKMS_FIX}\n  [HFX-404] {GROUP_FIX}\n\
         Not running on a terminal, so nothing was changed. Run the commands above yourself.\n"
    );
    assert!(result.out.ends_with(&listed), "{}", result.out);
    assert!(prompt.questions.is_empty() && machine.runner.visible().is_empty());
}

#[test]
fn fixes_need_yes() {
    let machine = needs_two_root_fixes();
    let mut prompt = ScriptedPrompt::new(true, &[false, true]);
    let result = machine.doctor_with(&mut prompt, &["--fix"]);
    assert_eq!(result.ok(), 2);
    let questions = [DKMS_FIX, GROUP_FIX].map(|c| format!("Run `{c}` as root? [y/N] "));
    assert_eq!(prompt.questions, questions);
    assert_eq!(machine.runner.visible(), [GROUP_FIX]);
    let ran = format!("Skipped.\nRunning: {GROUP_FIX}\nDone. Log out and back in afterwards.\n");
    assert!(result.out.contains(&ran), "{}", result.out);
}

#[test]
fn yes_only_for_user_fixes() {
    let machine = needs_two_root_fixes();
    let mut prompt = ScriptedPrompt::new(true, &[]);
    machine.doctor_with(&mut prompt, &["--fix", "--yes"]).ok();
    assert_eq!(prompt.questions.len(), 2);
    assert!(machine.runner.visible().is_empty());

    let machine = Machine::with_full();
    machine.runner.answer(HAS_OWNER, "b false");
    let mut prompt = ScriptedPrompt::new(true, &[]);
    machine.doctor_with(&mut prompt, &["--fix", "--yes"]).ok();
    assert!(prompt.questions.is_empty());
    assert_eq!(
        machine.runner.visible(),
        ["systemctl --user enable --now openrazer-daemon.service"]
    );
}
