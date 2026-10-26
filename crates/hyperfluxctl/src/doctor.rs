// SPDX-License-Identifier: GPL-2.0-only

//! Doctor: what is installed, loaded and running, and the first thing to
//! fix. Probing ([`probe`]) and judging ([`assess`]) are kept apart, which
//! lets the finding order be tested on plain facts.
//!
//! The bridge and the device keeper reach the receiver through hidraw
//! (interfaces 0, 1 and 4), and the keeper publishes batteries through
//! /dev/uhid. Those checks come first. Order and suppression:
//!
//! - An old driver that can still load (HFX-303) is always reported first.
//! - A receiver they cannot open hides the bridge and battery findings.
//! - A keeper that isn't working (HFX-208) hides the battery findings.
//! - The Full checks run on their own.
//! - DKMS records that cannot load anything (HFX-305) come last.
//!
//! The first finding is the primary issue. Notes, such as a device
//! OpenRazer waits for, are not findings.

use crate::bridge::Reach;
use crate::lines;
use crate::system::{PROBE_TIMEOUT, System, identity};
use hfx_proto::devices::{self, OLD_MODULE, RETIRED_DRIVERS};
use hfx_proto::errors::{ErrorCode, Severity, render};
use hfx_proto::messages::{
    DeviceInfo, DeviceKind, Inventory, OpenRazerState, Presence, ReceiverInfo, Snapshot,
};
use hfx_proto::openrazer::{CAPABILITY_NAME, CAPABILITY_VALUE, MODULES, PATCHED_MODULES};
use hfx_proto::{PRODUCT_VERSION, paths};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The group OpenRazer's files belong to on this distribution. Packages
/// build hyperfluxctl with the same value the OpenRazer driver package uses.
pub const OPENRAZER_GROUP: &str = match option_env!("HFX_OPENRAZER_GROUP") {
    Some(group) => group,
    None => "plugdev",
};

/// The receiver interfaces the bridge and the keeper open through hidraw.
pub const HIDRAW_INTERFACES: [u8; 3] = [0, 1, 4];
/// The receiver's HID LampArray interfaces, which no person should be able
/// to open (ARCHITECTURE.md, section 10).
pub const LAMPARRAY_INTERFACES: [u8; 2] = [3, 4];
/// The 2.x driver package, which HyperFlux Next V2 does not need.
pub const OLD_DKMS_PACKAGE: &str = "hyperflux-next-dkms";

pub const SECURE_BOOT_VAR: &str =
    "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-e2b9b7dc6b0b";
/// Where DKMS keeps the key it signs modules with: upstream DKMS, then
/// Ubuntu's shim-signed setup.
const DKMS_KEYS: &[&str] = &["/var/lib/dkms/mok.pub", "/var/lib/shim-signed/mok/MOK.der"];
const KEY_RULES_OVERRIDE: &str = "/etc/udev/rules.d/70-hyperflux-next-openrazer-keys.rules";
const BY_ID: &str = "/dev/input/by-id";
const BUSCTL: &str = "busctl --user --no-pager --no-legend";

pub enum Bridge {
    Up {
        snapshot: Box<Snapshot>,
        version: String,
    },
    NoAccess {
        socket: String,
    },
    Down {
        why: String,
    },
}

/// Owner, group and mode of a device node.
pub type NodeAccess = (u32, u32, u32);

/// One HID interface of a plugged-in receiver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    pub number: Option<u8>,
    pub driver: Option<String>,
    /// `/dev/hidrawN`, with its owner, group and mode, when the driver
    /// made one.
    pub hidraw: Option<(String, Option<NodeAccess>)>,
}

/// What an old receiver driver left behind: the 2.x package, or any
/// hid-hyperflux-next, since HyperFlux Next V2 has no module of its own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Leftovers {
    /// `version` of the loaded hid_hyperflux_next, or "unknown".
    pub loaded: Option<String>,
    /// The package manager that still has the 2.x DKMS package.
    pub package: Option<&'static str>,
    /// DKMS registrations that can still put the module in place,
    /// `hid-hyperflux-next/2.2.0`.
    pub dkms: Vec<String>,
    /// Old module files for an installed kernel that no DKMS registration
    /// accounts for.
    pub files: Vec<String>,
    /// Registrations of the 2.x and pre-release drivers that can no longer
    /// put a module in place, and the paths to delete for them.
    pub inert: Vec<String>,
    pub cleanup: Vec<String>,
}

/// A power supply as Doctor sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerSupply {
    pub model: String,
    /// Reading `capacity` works. Doctor tries it on HyperFlux devices and no
    /// others, because for some batteries a read goes out to the device.
    pub level: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub name: &'static str,
    pub installed: Option<String>,
    /// The loaded srcversion, or `None` when not loaded.
    pub loaded: Option<Option<String>>,
    /// Patched modules: whether the installed file has the capability tag.
    pub capability: Option<bool>,
    /// Loaded patched modules: whether the running copy has the parameter.
    pub capability_loaded: Option<bool>,
    /// Installed, not loaded, and Secure Boot won't load it as signed.
    pub secure_boot_blocked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// openrazer-daemon is not installed, or Doctor runs as root.
    NotApplicable,
    GroupMissing,
    NotMember,
    DaemonNotRunning,
    Unverified,
    Ready,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FullFacts {
    /// The plugin package is installed. It is separate from Full, which
    /// only depends on it where the distribution's OpenRGB loads it.
    pub openrgb_plugin: bool,
    pub modules: Vec<Module>,
    pub broken_links: Vec<String>,
    pub access: Access,
    /// Devices the bridge offers OpenRazer that it does not list, `1532:00cd`.
    pub unpublished: Vec<String>,
    /// Devices OpenRazer leaves out because they have not answered yet.
    pub waiting: Vec<(&'static str, Presence)>,
    pub missing_key_links: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Full {
    NotInstalled,
    Invalid(String),
    Ready(Box<FullFacts>),
}

/// The keeper's unit as systemd sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keeper {
    pub state: String,
    /// Its status says the bridge has it.
    pub connected: bool,
}

pub struct Facts {
    /// `ActiveState` of the bridge unit, if systemctl answered.
    pub service: Option<String>,
    pub bridge: Bridge,
    /// The keeper's unit, if systemctl answered.
    pub keeper: Option<Keeper>,
    pub receivers_attached: usize,
    pub interfaces: Vec<Interface>,
    /// The bridge's user, which owns interface 0, and its group, which
    /// owns interface 4.
    pub bridge_uid: Option<u32>,
    pub bridge_gid: Option<u32>,
    /// The keeper's group, which owns interfaces 0 and 1 and /dev/uhid.
    pub keeper_gid: Option<u32>,
    /// Owner, group and mode of /dev/uhid, if it exists.
    pub uhid: Option<NodeAccess>,
    /// Users an ACL lets write /dev/uhid.
    pub uhid_acl_users: Vec<u32>,
    /// The LampArray nodes this login can open for reading and writing,
    /// each with how. Empty for root.
    pub lamparray_open: Vec<String>,
    pub leftovers: Leftovers,
    /// Pre-release driver registrations that can still put a module in place.
    pub retired: Vec<String>,
    /// The power supplies that have a model name.
    pub batteries: Vec<PowerSupply>,
    pub full: Full,
}

impl Facts {
    #[must_use]
    pub fn snapshot(&self) -> Option<&Snapshot> {
        match &self.bridge {
            Bridge::Up { snapshot, .. } => Some(snapshot),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------ probing

/// Reads the machine. `reach` is the bridge connection attempt, made by
/// the caller so it can honour `--socket`.
#[must_use]
pub fn probe(system: &System, reach: Reach) -> Facts {
    let bridge = match reach {
        Reach::Up(mut client) => match client.snapshot() {
            Ok(snapshot) => {
                let version = client.welcome().bridge_version.to_string();
                Bridge::Up {
                    snapshot: Box::new(snapshot),
                    version,
                }
            }
            Err(error) => Bridge::Down {
                why: format!("snapshot failed: {error}"),
            },
        },
        Reach::NoAccess { user_socket } => Bridge::NoAccess {
            socket: user_socket.display().to_string(),
        },
        Reach::Down(why) => Bridge::Down { why },
    };
    let service = format!(
        "systemctl show --property=ActiveState --value {}",
        paths::BRIDGE_UNIT
    );
    let (leftovers, retired) = leftovers(system);
    let batteries = system
        .list("/sys/class/power_supply", 256)
        .unwrap_or_default();
    let ours = |model: &str| {
        devices::DEVICES
            .iter()
            .any(|d| model.eq_ignore_ascii_case(d.name))
    };
    let supply = |name: &String| {
        let dir = format!("/sys/class/power_supply/{name}");
        let model = system.read(&format!("{dir}/model_name"))?;
        // hid-input answers ENODATA until it has had a battery report.
        let level = ours(&model) && system.read(&format!("{dir}/capacity")).is_some();
        Some(PowerSupply { model, level })
    };
    let snapshot = match &bridge {
        Bridge::Up { snapshot, .. } => Some(snapshot.as_ref()),
        _ => None,
    };
    let full = probe_full(system, snapshot);
    let mut facts = Facts {
        service: system.run(&service).filter(|v| !v.is_empty()),
        bridge,
        keeper: keeper(system),
        receivers_attached: receivers_strict(system).unwrap_or(0),
        interfaces: interfaces(system),
        bridge_uid: system.user_id(paths::BRIDGE_USER),
        bridge_gid: system.group_id(paths::BRIDGE_USER),
        keeper_gid: system.group_id(paths::KEEPER_USER),
        uhid: system.access(paths::UHID_DEVICE),
        uhid_acl_users: system.acl_users(paths::UHID_DEVICE, 0o2),
        lamparray_open: Vec::new(),
        leftovers,
        retired,
        batteries: batteries.iter().filter_map(supply).collect(),
        full,
    };
    facts.lamparray_open = lamparray_open(system, &facts);
    facts
}

/// The keeper's `ActiveState`, and whether its `StatusText` says the bridge
/// has it.
fn keeper(system: &System) -> Option<Keeper> {
    let line = format!(
        "systemctl show --property=ActiveState,StatusText {}",
        paths::KEEPER_UNIT
    );
    let shown = system.run(&line)?;
    let value = |key: &str| shown.lines().find_map(|l| l.strip_prefix(key));
    let state = value("ActiveState=").filter(|v| !v.is_empty())?.to_owned();
    let connected = value("StatusText=") == Some(paths::KEEPER_CONNECTED);
    Some(Keeper { state, connected })
}

/// USB devices with the receiver's ids. `Err` when sysfs cannot be read
/// well enough to be sure.
///
/// # Errors
/// When the device list is unreadable, too long, or an entry is half there.
pub fn receivers_strict(system: &System) -> Result<usize, &'static str> {
    const DIR: &str = "/sys/bus/usb/devices";
    let names = system
        .list(DIR, 257)
        .ok_or("the USB device list can't be read")?;
    if names.len() > 256 {
        return Err("the USB device list is too long to check");
    }
    let receiver = (
        format!("{:04x}", devices::RECEIVER.vendor_id),
        format!("{:04x}", devices::RECEIVER.product_id),
    );
    let mut count = 0;
    for name in names {
        let vendor = system.read(&format!("{DIR}/{name}/idVendor"));
        let product = system.read(&format!("{DIR}/{name}/idProduct"));
        match (vendor, product) {
            (None, None) => {}
            (Some(v), Some(p)) => {
                count += usize::from((v.to_lowercase(), p.to_lowercase()) == receiver);
            }
            _ => return Err("a USB device is only half described in sysfs"),
        }
    }
    Ok(count)
}

/// The receiver's HID interfaces, sorted by HID device name. The USB
/// interface number sits in the parent directory of each HID device.
fn interfaces(system: &System) -> Vec<Interface> {
    const DIR: &str = "/sys/bus/hid/devices";
    let receiver = &devices::RECEIVER;
    let prefix = format!(
        "0003:{:04X}:{:04X}.",
        receiver.vendor_id, receiver.product_id
    );
    let names = system.list(DIR, 256).unwrap_or_default();
    let ours = names
        .iter()
        .filter(|name| name.to_ascii_uppercase().starts_with(&prefix));
    let interface = |name: &String| {
        let dir = format!("{DIR}/{name}");
        let number = system.read(&format!("{dir}/../bInterfaceNumber"));
        let node = system
            .list(&format!("{dir}/hidraw"), 4)
            .and_then(|n| n.into_iter().next());
        let hidraw = node.map(|n| {
            let dev = format!("/dev/{n}");
            let access = system.access(&dev);
            (dev, access)
        });
        Interface {
            number: number.and_then(|n| u8::from_str_radix(&n, 16).ok()),
            driver: system.link_name(&format!("{dir}/driver")),
            hidraw,
        }
    };
    ours.map(interface).collect()
}

/// How this login can open the LampArray nodes for reading and writing: an
/// ACL entry for it, one of its groups, or the mode. Owners are not looked
/// at, because Core's rule leaves both nodes root's and udev opens hidraw
/// nodes to people with tags, groups and modes. root opens anything.
fn lamparray_open(system: &System, facts: &Facts) -> Vec<String> {
    if system.uid == 0 {
        return Vec::new();
    }
    let rw = |bits: u32| bits & 0o6 == 0o6;
    let mut open = Vec::new();
    for i in &facts.interfaces {
        let n = i.number.filter(|n| LAMPARRAY_INTERFACES.contains(n));
        let (Some(n), Some((node, Some((_, gid, mode))))) = (n, &i.hidraw) else {
            continue;
        };
        let mut how = Vec::new();
        if system.acl_users(node, 0o6).contains(&system.uid) {
            how.push(format!("has an ACL entry for uid {}", system.uid));
        }
        if system.groups.contains(gid) && rw(mode >> 3) {
            let group = match facts.bridge_gid {
                Some(ours) if ours == *gid => format!("the {} group", paths::BRIDGE_USER),
                _ => format!("group {gid}"),
            };
            how.push(format!("belongs to {group}, which this login is in"));
        }
        if rw(*mode) {
            how.push(format!("has mode {mode:04o}, open to everyone"));
        }
        if !how.is_empty() {
            open.push(format!("{node} (interface {n}) {}", how.join(" and ")));
        }
    }
    open
}

/// Where DKMS installs modules below /usr/lib/modules/KERNEL: Arch, Debian
/// and Ubuntu use updates/dkms, openSUSE updates, Fedora extra.
const MODULE_DIRS: [&str; 3] = ["updates/dkms", "updates", "extra"];

/// Versions, kernel names and file names that go into paths and commands.
fn plain(word: &str) -> bool {
    let ok = |b: u8| b.is_ascii_alphanumeric() || b"._+~-".contains(&b);
    !word.is_empty() && word.len() <= 64 && !word.starts_with('.') && word.bytes().all(ok)
}

/// Kernels that can boot: the running one, and any other whose image is
/// still there. A removed kernel can leave its module directory behind.
fn installed_kernels(system: &System) -> Vec<String> {
    let running = system.read("/proc/sys/kernel/osrelease");
    let kernels = system.list("/usr/lib/modules", 64).unwrap_or_default();
    kernels
        .into_iter()
        .filter(|k| plain(k))
        .filter(|k| {
            running.as_deref() == Some(k.as_str())
                || system.is_file(&format!("/usr/lib/modules/{k}/vmlinuz"))
                || system.is_file(&format!("/boot/vmlinuz-{k}"))
        })
        .collect()
}

/// Files of `module` for these kernels: kernel and path.
fn module_files(system: &System, module: &str, kernels: &[String]) -> Vec<(String, String)> {
    let stems = [module.to_owned(), module.replace('-', "_")];
    let is_module = |name: &str| {
        stems.iter().any(|stem| {
            let rest = name
                .strip_prefix(stem.as_str())
                .and_then(|r| r.strip_prefix(".ko"));
            rest.is_some_and(|r| matches!(r, "" | ".zst" | ".xz" | ".gz"))
        })
    };
    let mut found = Vec::new();
    for kernel in kernels {
        for dir in MODULE_DIRS {
            let dir = format!("/usr/lib/modules/{kernel}/{dir}");
            for name in system.list(&dir, 512).unwrap_or_default() {
                if plain(&name) && is_module(&name) {
                    found.push((kernel.clone(), format!("{dir}/{name}")));
                }
            }
        }
    }
    found
}

#[derive(Default)]
struct Dkms {
    active: Vec<String>,
    files: Vec<String>,
    inert: Vec<String>,
    cleanup: Vec<String>,
}

/// The DKMS records of `name` whose version `old` picks. A registration can
/// put a module in place while it has a source tree, because DKMS builds it
/// for every new kernel, or while it is installed for a kernel that is
/// still installed. Without either it's inert.
fn dkms(system: &System, name: &str, old: impl Fn(&str) -> bool, kernels: &[String]) -> Dkms {
    let dir = format!("/var/lib/dkms/{name}");
    let entries = system.list(&dir, 64).unwrap_or_default();
    // kernel-KERNEL-ARCH links to VERSION/KERNEL: DKMS installed it there.
    let links: Vec<(&String, String, String)> = entries
        .iter()
        .filter(|e| e.starts_with("kernel-"))
        .filter_map(|e| {
            let target = system.link_target(&format!("{dir}/{e}"))?;
            let (version, kernel) = target.split_once('/')?;
            Some((e, version.to_owned(), kernel.to_owned()))
        })
        .collect();
    let installed: Vec<(String, String)> = links
        .iter()
        .map(|(_, v, k)| (v.clone(), k.clone()))
        .collect();
    let mut found = Dkms::default();
    let versions = entries
        .iter()
        .filter(|v| v.starts_with(|c: char| c.is_ascii_digit()) && plain(v) && old(v));
    for version in versions {
        let registration = format!("{name}/{version}");
        let source = format!("/usr/src/{name}-{version}");
        let buildable = system.is_file(&format!("{dir}/{version}/source/dkms.conf"))
            || system.is_file(&format!("{source}/dkms.conf"));
        let in_place = installed
            .iter()
            .any(|(v, k)| v == version && kernels.contains(k));
        if buildable || in_place {
            found.active.push(registration);
        } else {
            found.inert.push(registration);
            found.cleanup.push(format!("{dir}/{version}"));
            if system.exists(&source) {
                found.cleanup.push(source);
            }
        }
    }
    // Nothing else in the directory, apart from links to those versions for
    // kernels that are gone: it can go as a whole.
    let inert = |version: &str| found.inert.contains(&format!("{name}/{version}"));
    let removable =
        |entry: &String| inert(entry) || links.iter().any(|(e, v, _)| *e == entry && inert(v));
    if !found.inert.is_empty() && entries.iter().all(removable) {
        found
            .cleanup
            .retain(|path| !path.starts_with(&format!("{dir}/")));
        found.cleanup.insert(0, dir.clone());
    }
    for (kernel, path) in module_files(system, name, kernels) {
        // DKMS put it there. The registration it came from answers for it.
        if !installed.iter().any(|(_, k)| *k == kernel) {
            found.files.push(path);
        }
    }
    found
}

/// The package manager that has the 2.x package itself. pacman answers for
/// a name another package provides, as hyperflux-next-v2-linux does, so
/// every answer has to name the package. dpkg-query and rpm -q only know
/// installed packages by their own names.
fn old_package(system: &System) -> Option<&'static str> {
    let package = OLD_DKMS_PACKAGE;
    let pacman = system.run(&format!("pacman -Q {package}"));
    if pacman.is_some_and(|out| out.split_whitespace().next() == Some(package)) {
        return Some("pacman");
    }
    let dpkg = system.run(&format!(
        "dpkg-query -W -f=${{Package}}:${{Status}} {package}"
    ));
    let status = dpkg
        .as_deref()
        .and_then(|out| out.strip_prefix(package)?.strip_prefix(':'));
    if status.is_some_and(|s| s.ends_with(" installed")) {
        return Some("apt");
    }
    let rpm = system.run(&format!("rpm -q --qf %{{NAME}} {package}"));
    (rpm.as_deref() == Some(package)).then_some("rpm")
}

/// What an old receiver driver left, and the pre-release registrations that
/// can still put a module in place.
fn leftovers(system: &System) -> (Leftovers, Vec<String>) {
    let module = format!("/sys/module/{}", OLD_MODULE.replace('-', "_"));
    let version = system.read(&format!("{module}/version"));
    let loaded = system
        .is_dir(&module)
        .then(|| version.unwrap_or_else(|| "unknown".into()));
    let package = old_package(system);
    let kernels = installed_kernels(system);
    let mut ours = dkms(system, OLD_MODULE, |_| true, &kernels);
    if package.is_some() {
        // Removing the package takes its records along.
        ours.inert.clear();
        ours.cleanup.clear();
    }
    let mut retired = Vec::new();
    let mut names: Vec<&str> = RETIRED_DRIVERS.iter().map(|(name, _)| *name).collect();
    names.dedup();
    for name in names {
        let listed = |v: &str| RETIRED_DRIVERS.contains(&(name, v));
        let found = dkms(system, name, listed, &kernels);
        retired.extend(found.active);
        ours.files.extend(found.files);
        ours.inert.extend(found.inert);
        ours.cleanup.extend(found.cleanup);
    }
    let leftovers = Leftovers {
        loaded,
        package,
        dkms: ours.active,
        files: ours.files,
        inert: ours.inert,
        cleanup: ours.cleanup,
    };
    (leftovers, retired)
}

fn secure_boot_on(system: &System) -> bool {
    // The efivar is four attribute bytes and one value byte.
    match system.read_bytes(SECURE_BOOT_VAR) {
        Some(bytes) => bytes.get(4) == Some(&1),
        None => system
            .run_any("mokutil --sb-state", PROBE_TIMEOUT)
            .is_some_and(|out| out.stdout.contains("SecureBoot enabled")),
    }
}

/// Whether the DKMS signing key is known to be missing from the firmware.
fn dkms_key_not_enrolled(system: &System) -> bool {
    let not_enrolled: Vec<bool> = DKMS_KEYS
        .iter()
        .filter(|key| system.exists(key))
        .filter_map(|key| system.run_any(&format!("mokutil --test-key {key}"), PROBE_TIMEOUT))
        .map(|out| out.stdout.contains("is not enrolled"))
        .collect();
    !not_enrolled.is_empty() && not_enrolled.iter().all(|n| *n)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Component {
    component: String,
    version: String,
}

fn probe_full(system: &System, snapshot: Option<&Snapshot>) -> Full {
    let Some(text) = system.read(paths::FULL_COMPONENT) else {
        if system.exists(paths::FULL_COMPONENT) {
            return Full::Invalid("unreadable".into());
        }
        return Full::NotInstalled;
    };
    let component = match serde_json::from_str::<Component>(&text) {
        Ok(component) if component.component == "full" => component,
        _ => return Full::Invalid("malformed".into()),
    };
    if component.version != PRODUCT_VERSION {
        return Full::Invalid(format!("version {}", component.version));
    }
    let daemon = system.is_file("/usr/bin/openrazer-daemon");
    let mut access = probe_access(system, daemon);
    let (mut unpublished, mut waiting) = (Vec::new(), Vec::new());
    if let (Access::Ready, Some(snapshot)) = (access, snapshot) {
        match published(system) {
            Some(mut published) => {
                // OpenRazer lists a device a moment after the bridge makes it.
                // One made just now gets a few seconds.
                let missing =
                    |p: &[(u16, u16)]| missing_from(expected_published(snapshot), p.to_vec());
                let started = std::time::Instant::now();
                while !missing(&published).is_empty() && started.elapsed() < system.settle {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    published = self::published(system).unwrap_or(published);
                }
                waiting = waiting_for(snapshot, &published);
                unpublished = missing(&published);
            }
            None => access = Access::Unverified,
        }
    }
    let mut modules: Vec<Module> = MODULES.iter().map(|m| module(system, m)).collect();
    let unloaded = |m: &Module| m.installed.is_some() && m.loaded.is_none();
    if modules.iter().any(unloaded) && secure_boot_on(system) {
        let key_missing = dkms_key_not_enrolled(system);
        for m in modules.iter_mut().filter(|m| unloaded(m)) {
            let signer = system.run(&format!("modinfo -F signer {}", m.name));
            m.secure_boot_blocked = key_missing || signer.is_none_or(|s| s.is_empty());
        }
    }
    Full::Ready(Box::new(FullFacts {
        openrgb_plugin: system.exists(paths::OPENRGB_PLUGIN),
        modules,
        broken_links: if daemon {
            broken_links(system)
        } else {
            Vec::new()
        },
        access,
        unpublished,
        waiting,
        missing_key_links: snapshot
            .map(|s| missing_key_links(system, s))
            .unwrap_or_default(),
    }))
}

fn module(system: &System, name: &'static str) -> Module {
    let patched = PATCHED_MODULES.contains(&name);
    let dir = format!("/sys/module/{name}");
    let loaded = system
        .is_dir(&dir)
        .then(|| identity(system.read(&format!("{dir}/srcversion"))));
    let tag = system.run(&format!("modinfo -F {CAPABILITY_NAME} {name}"));
    let parameter = system.read(&format!("{dir}/parameters/{CAPABILITY_NAME}"));
    Module {
        name,
        installed: identity(system.run(&format!("modinfo -F srcversion {name}"))),
        capability: patched.then(|| tag.as_deref() == Some(CAPABILITY_VALUE)),
        capability_loaded: (patched && loaded.is_some())
            .then(|| parameter.as_deref() == Some(CAPABILITY_VALUE)),
        loaded,
        secure_boot_blocked: false,
    }
}

/// OpenRazer's EVENT_FILE_REGEX values are simple: `.*`, a literal stem,
/// maybe a group or two, and an `-event-kbd`/`-event-mouse` ending. A name
/// that contains the stem and has the ending may match. That is enough to
/// decide which dangling links are worth reporting.
fn may_match(name: &str, pattern: &str) -> bool {
    let body = pattern.strip_prefix(".*").unwrap_or(pattern);
    let stem_end = body
        .find(|c| "\\.^$|?*+()[]{}".contains(c))
        .unwrap_or(body.len());
    let stem = &body[..stem_end];
    let ending = ["-event-kbd", "-event-mouse"]
        .into_iter()
        .find(|e| body.ends_with(e));
    !stem.is_empty() && ending.is_some_and(|e| name.ends_with(e)) && name.contains(stem)
}

fn broken_links(system: &System) -> Vec<String> {
    let openrazer = devices::DEVICES.iter().filter_map(|d| d.openrazer.as_ref());
    let patterns: Vec<&str> = openrazer.filter_map(|o| o.event_file_regex).collect();
    let names = system.list(BY_ID, 512).unwrap_or_default();
    names
        .iter()
        .filter(|name| patterns.iter().any(|p| may_match(name, p)))
        .map(|name| format!("{BY_ID}/{name}"))
        .filter(|link| system.dangling(link))
        .take(16)
        .collect()
}

fn probe_access(system: &System, daemon: bool) -> Access {
    if !daemon {
        return Access::NotApplicable;
    }
    let Some(gid) = system.group_id(OPENRAZER_GROUP) else {
        return Access::GroupMissing;
    };
    // root has no desktop session bus to check.
    if system.uid == 0 {
        return Access::NotApplicable;
    }
    if !system.groups.contains(&gid) {
        return Access::NotMember;
    }
    let dbus = "org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus";
    match system
        .run(&format!("{BUSCTL} call {dbus} NameHasOwner s org.razer"))
        .as_deref()
    {
        Some("b true") => Access::Ready,
        Some("b false") => Access::DaemonNotRunning,
        _ => Access::Unverified,
    }
}

fn razer_call(system: &System, object: &str, method: &str) -> Option<String> {
    system.run(&format!(
        "{BUSCTL} --auto-start=no call org.razer {object} {method}"
    ))
}

/// `(vendor, product)` of every device openrazer-daemon lists. Serials go
/// into object paths and are never kept.
fn published(system: &System) -> Option<Vec<(u16, u16)>> {
    let list = razer_call(system, "/org/razer", "razer.devices getDevices")?;
    let mut words = list.split_whitespace();
    if words.next()? != "as" {
        return None;
    }
    let count: usize = words.next()?.parse().ok()?;
    let serials: Vec<&str> = words.map(|w| w.trim_matches('"')).collect();
    let plain = |s: &&str| {
        let ok = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        !s.is_empty() && s.len() <= 128 && s.bytes().all(ok)
    };
    if count > 64 || serials.len() != count || !serials.iter().all(plain) {
        return None;
    }
    let ids = |serial: &&str| {
        let object = format!("/org/razer/device/{serial}");
        let answer = razer_call(system, &object, "razer.device.misc getVidPid")?;
        match answer.split_whitespace().collect::<Vec<_>>()[..] {
            ["ai", "2", vendor, product] => Some((vendor.parse().ok()?, product.parse().ok()?)),
            _ => None,
        }
    };
    serials.iter().map(ids).collect()
}

/// Paired devices the bridge can make an OpenRazer device for.
fn openrazer_devices(
    snapshot: &Snapshot,
) -> impl Iterator<Item = (&hfx_proto::messages::DeviceInfo, &'static devices::Device)> {
    lines::devices(snapshot)
        .filter(|(_, d)| d.kind != DeviceKind::Unknown)
        .filter_map(|(_, d)| Some((d, devices::device(d.product_id)?)))
        .filter(|(_, catalog)| catalog.openrazer.is_some())
}

/// The bridge's OpenRazer devices that are there for OpenRazer to list.
/// From a bridge that does not say, the online devices with a route: it
/// makes one once it reads the same serial twice, which needs the device
/// to answer.
fn expected_published(snapshot: &Snapshot) -> Vec<(u16, u16)> {
    let live = |d: &DeviceInfo| match &d.openrazer {
        Some(o) => o.state == OpenRazerState::Live,
        None => d.controllable && d.presence == Presence::Online,
    };
    openrazer_devices(snapshot)
        .filter(|(d, _)| live(d))
        .map(|(d, _)| (d.vendor_id, d.product_id))
        .collect()
}

/// Devices OpenRazer doesn't list because they haven't answered: asleep,
/// out of range, or not heard from since the bridge started or resumed.
/// One on its cable is listed as the wired device.
fn waiting_for(snapshot: &Snapshot, published: &[(u16, u16)]) -> Vec<(&'static str, Presence)> {
    openrazer_devices(snapshot)
        .filter(|(d, _)| d.presence != Presence::Online)
        .filter(|(d, _)| (d.openrazer.as_ref()).is_none_or(|o| o.state != OpenRazerState::Cable))
        .filter(|(d, _)| !published.contains(&(d.vendor_id, d.product_id)))
        .map(|(d, catalog)| (catalog.name, d.presence))
        .collect()
}

fn missing_from(expected: Vec<(u16, u16)>, mut published: Vec<(u16, u16)>) -> Vec<String> {
    let mut missing = Vec::new();
    for ids in expected {
        match published.iter().position(|p| *p == ids) {
            Some(at) => drop(published.swap_remove(at)),
            None => missing.push(format!("{:04x}:{:04x}", ids.0, ids.1)),
        }
    }
    missing
}

/// The Full udev rule gives each receiver one link per OpenRazer keyboard
/// pattern, named after the receiver's port. Without udev's help Doctor
/// cannot name the port. It counts instead: each paired keyboard needs one
/// live link with its suffix.
fn missing_key_links(system: &System, snapshot: &Snapshot) -> Vec<String> {
    if system.exists(KEY_RULES_OVERRIDE) {
        return Vec::new();
    }
    let names = system.list(BY_ID, 512).unwrap_or_default();
    let mut needed: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, device) in lines::devices(snapshot) {
        let catalog =
            devices::device(device.product_id).filter(|_| device.kind == DeviceKind::Keyboard);
        if let Some(suffix) = catalog.and_then(|d| d.openrazer.as_ref()?.key_event_link) {
            *needed.entry(suffix).or_default() += 1;
        }
    }
    let present = |suffix: &str| {
        let ending = format!("-{suffix}");
        let live = |n: &&String| system.path(&format!("{BY_ID}/{n}")).exists();
        let ours = |n: &&String| n.starts_with("hyperflux-next-") && n.ends_with(&ending);
        names.iter().filter(ours).filter(live).count()
    };
    needed
        .into_iter()
        .filter(|(suffix, count)| present(suffix) < *count)
        .map(|(suffix, _)| format!("{BY_ID}/hyperflux-next-*-{suffix}"))
        .collect()
}

// ------------------------------------------------------------------ judging

/// How the receiver is reached: its HID interfaces and their hidraw nodes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Driver {
    Active,
    NoReceiver,
    /// A needed interface has no hidraw node; the driver that has it.
    Owned(String),
    NoHidraw,
    /// The hidraw nodes exist, but not for the bridge and the keeper.
    NoAccess,
}

impl Driver {
    /// The JSON token and the words `status` prints.
    #[must_use]
    pub fn words(&self) -> (&'static str, String) {
        match self {
            Self::Active => ("active", "active".into()),
            Self::NoReceiver => ("no-receiver", "no receiver".into()),
            Self::Owned(driver) => ("owned-by-other-driver", format!("owned by {driver}")),
            Self::NoHidraw => ("no-hidraw", "no hidraw node".into()),
            Self::NoAccess => (
                "no-access",
                "hidraw not open to the bridge and keeper".into(),
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeState {
    Running,
    Stopped,
    NoAccess,
}

impl BridgeState {
    #[must_use]
    pub const fn words(self) -> (&'static str, &'static str) {
        match self {
            Self::Running => ("running", "running"),
            Self::Stopped => ("stopped", "stopped"),
            Self::NoAccess => ("not-reachable", "not reachable from this login"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub code: ErrorCode,
    /// Fill the catalog placeholders. Extra keys show up in JSON, not in text.
    pub details: Vec<(&'static str, String)>,
}

impl Finding {
    fn new(code: ErrorCode, details: &[(&'static str, &str)]) -> Self {
        let details = details.iter().map(|(k, v)| (*k, (*v).to_owned())).collect();
        Self { code, details }
    }

    #[must_use]
    pub fn detail(&self, key: &str) -> Option<&str> {
        self.details
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }

    /// One catalog text with the placeholders filled.
    fn fill(&self, pick: fn(&hfx_proto::errors::CatalogEntry) -> &'static str) -> String {
        let values: Vec<(&str, &str)> =
            self.details.iter().map(|(k, v)| (*k, v.as_str())).collect();
        render(pick(self.code.entry()), &values)
    }

    #[must_use]
    pub fn summary(&self) -> String {
        self.fill(|e| e.summary)
    }
    #[must_use]
    pub fn cause(&self) -> String {
        self.fill(|e| e.cause)
    }
    #[must_use]
    pub fn action(&self) -> String {
        self.fill(|e| e.action)
    }
    #[must_use]
    pub fn verify(&self) -> String {
        self.fill(|e| e.verify)
    }

    #[must_use]
    pub fn json(&self) -> Value {
        let details: BTreeMap<_, _> = self.details.iter().cloned().collect();
        json!({
            "code": self.code.id(),
            "severity": self.code.entry().severity.as_str(),
            "summary": self.summary(),
            "cause": self.cause(),
            "action": self.action(),
            "verify": self.verify(),
            "documentation": self.code.documentation(),
            "details": details,
        })
    }
}

/// The keeper as `status` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeeperState {
    Connected,
    /// Running, but the bridge doesn't have it.
    Waiting,
    Stopped,
    /// systemd didn't say.
    Unknown,
}

impl KeeperState {
    #[must_use]
    pub const fn words(self) -> (&'static str, &'static str) {
        match self {
            Self::Connected => ("connected", "running, connected to the bridge"),
            Self::Waiting => ("waiting", "running, not connected to the bridge"),
            Self::Stopped => ("stopped", "not running"),
            Self::Unknown => ("unknown", "unknown"),
        }
    }
}

pub struct Assessment {
    pub driver: Driver,
    pub bridge: BridgeState,
    pub keeper: KeeperState,
    pub receivers: usize,
    pub findings: Vec<Finding>,
    /// Worth knowing, nothing to fix.
    pub notes: Vec<String>,
}

impl Assessment {
    /// Everything works: nothing found, or only warnings.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.findings
            .iter()
            .all(|f| f.code.entry().severity == Severity::Warning)
    }

    #[must_use]
    pub fn next(&self) -> String {
        match self.findings.first() {
            None => "no action required".into(),
            Some(_) if self.ready() => format!(
                "no action required ({}: run hyperfluxctl doctor)",
                count(self.findings.len(), "warning")
            ),
            Some(first) => first.action(),
        }
    }
}

fn count(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

fn needed(interface: &Interface) -> bool {
    interface
        .number
        .is_some_and(|n| HIDRAW_INTERFACES.contains(&n))
}

/// The node is as the udev rule makes it: interface 0 the bridge's to read
/// and write and the keeper's to read, 1 the keeper's to read, 4 the
/// bridge's, and nobody else's.
fn hidraw_open(interface: &Interface, facts: &Facts) -> bool {
    let Some((_, Some((uid, gid, mode)))) = interface.hidraw else {
        return false;
    };
    let (keeper, bridge) = (Some(gid) == facts.keeper_gid, Some(gid) == facts.bridge_gid);
    match (interface.number, mode & 0o777) {
        (Some(0), 0o640) => keeper && Some(uid) == facts.bridge_uid,
        (Some(1), 0o640) => keeper,
        (Some(4), 0o660) => bridge,
        _ => false,
    }
}

fn driver(facts: &Facts) -> Driver {
    if facts.receivers_attached == 0 {
        return Driver::NoReceiver;
    }
    for number in HIDRAW_INTERFACES {
        let interface = facts.interfaces.iter().find(|i| i.number == Some(number));
        if interface.is_none_or(|i| i.hidraw.is_none()) {
            let driver = interface.and_then(|i| i.driver.clone());
            return driver
                .filter(|d| d != "hid-generic")
                .map_or(Driver::NoHidraw, Driver::Owned);
        }
    }
    let mut needed = facts.interfaces.iter().filter(|i| needed(i));
    if needed.all(|i| hidraw_open(i, facts)) {
        Driver::Active
    } else {
        Driver::NoAccess
    }
}

fn receiver_finding(facts: &Facts, driver: &Driver) -> Option<Finding> {
    let problem = match driver {
        Driver::Active | Driver::NoReceiver => return None,
        Driver::Owned(other) => format!("its interfaces are bound to {other}"),
        Driver::NoHidraw => "it has no hidraw nodes".into(),
        Driver::NoAccess => {
            let closed = |i: &&Interface| needed(i) && !hidraw_open(i, facts);
            let interface = facts.interfaces.iter().find(closed);
            let owner = interface.is_some_and(|i| i.number == Some(0));
            match interface.and_then(|i| i.hidraw.clone()) {
                Some((node, Some((uid, gid, mode)))) if owner => {
                    format!("{node} is user {uid}, group {gid}, mode {mode:04o}")
                }
                Some((node, Some((_, gid, mode)))) => {
                    format!("{node} is group {gid}, mode {mode:04o}")
                }
                Some((node, None)) => format!("{node} is missing"),
                None => "its hidraw nodes aren't open to the bridge".into(),
            }
        }
    };
    Some(Finding::new(
        ErrorCode::ReceiverNotReady,
        &[("receiver", &problem)],
    ))
}

fn uhid_finding(facts: &Facts) -> Option<Finding> {
    let problem = match (facts.uhid, facts.keeper_gid) {
        (None, _) => "it doesn't exist; the uhid module isn't loaded".to_owned(),
        (_, None) => format!("there is no {} group", paths::KEEPER_USER),
        (Some((_, gid, mode)), Some(group)) if gid != group || mode & 0o777 != 0o660 => {
            format!("it is group {gid}, mode {mode:04o}")
        }
        _ => return None,
    };
    Some(Finding::new(
        ErrorCode::UhidNotWritable,
        &[("uhid", &problem)],
    ))
}

fn dkms_remedy(registration: &str) -> String {
    format!(
        "Remove its DKMS registration with: sudo dkms remove {registration} --all, then reboot. \
         hyperfluxctl doctor --fix offers to run that."
    )
}

/// An old driver that can still load: from the package, a DKMS
/// registration, a module file, or the copy loaded now.
fn leftover_findings(facts: &Facts) -> Vec<Finding> {
    let old = &facts.leftovers;
    let what = old
        .dkms
        .first()
        .cloned()
        .or_else(|| old.package.map(|_| OLD_DKMS_PACKAGE.to_owned()))
        .or_else(|| old.files.first().cloned());
    let what = what.or_else(|| {
        old.loaded
            .as_ref()
            .map(|v| format!("{OLD_MODULE} {v} (still loaded)"))
    });
    let remedy = if old.package.is_some() {
        format!(
            "Remove the old 2.x package, {OLD_DKMS_PACKAGE}, with your package manager, then \
             reboot. hyperfluxctl doctor --fix shows the exact command."
        )
    } else if let Some(registration) = old.dkms.first() {
        dkms_remedy(registration)
    } else if let Some(file) = old.files.first() {
        format!("Delete {file}, run sudo depmod -a, then reboot.")
    } else {
        "None of it is installed any more. Reboot, and the receiver goes back to the \
         kernel's own HID drivers."
            .to_owned()
    };
    let mut findings = Vec::new();
    if let Some(registration) = what {
        let package = old.package.unwrap_or("none");
        let loaded = old.loaded.as_deref().unwrap_or("no");
        let mut details = vec![
            ("registration", registration.as_str()),
            ("remedy", remedy.as_str()),
            ("package", package),
            ("loaded", loaded),
        ];
        if let Some(dkms) = old.dkms.first().filter(|_| old.package.is_none()) {
            details.push(("dkms", dkms));
        }
        findings.push(Finding::new(ErrorCode::RetiredDriverInstalled, &details));
    }
    for registration in &facts.retired {
        let remedy = dkms_remedy(registration);
        let details = [
            ("registration", registration.as_str()),
            ("remedy", remedy.as_str()),
            ("dkms", registration.as_str()),
        ];
        findings.push(Finding::new(ErrorCode::RetiredDriverInstalled, &details));
    }
    findings
}

/// DKMS records that cannot load anything. Low on the list: they're
/// clutter, and the cleanup is one command.
fn inert_findings(facts: &Facts) -> Option<Finding> {
    let old = &facts.leftovers;
    if old.inert.is_empty() || old.cleanup.is_empty() {
        return None;
    }
    let details = [
        ("records", old.inert.join(", ")),
        ("paths", old.cleanup.join(" ")),
    ];
    let details: Vec<(&str, &str)> = details.iter().map(|(k, v)| (*k, v.as_str())).collect();
    Some(Finding::new(ErrorCode::InertDriverRecords, &details))
}

#[must_use]
pub fn assess(facts: &Facts) -> Assessment {
    let driver = driver(facts);
    let mut findings = leftover_findings(facts);
    let receiver = receiver_finding(facts, &driver);
    let receiver_ok = receiver.is_none();
    findings.extend(receiver);
    findings.extend(uhid_finding(facts));
    if receiver_ok {
        findings.extend(readiness(facts));
        let keeper = keeper_finding(facts);
        // A keeper that isn't working explains every missing battery.
        if keeper.is_none() {
            findings.extend(battery_findings(facts));
        }
        findings.extend(keeper);
        findings.extend(inventory_findings(facts));
        findings.extend(openrazer_device_findings(facts));
    }
    findings.extend(full_findings(&facts.full));
    findings.extend(uhid_acl_findings(facts));
    findings.extend(lamparray_finding(facts));
    findings.extend(inert_findings(facts));
    let bridge = match facts.bridge {
        Bridge::Up { .. } => BridgeState::Running,
        Bridge::NoAccess { .. } => BridgeState::NoAccess,
        Bridge::Down { .. } => BridgeState::Stopped,
    };
    let receivers = facts
        .snapshot()
        .map_or(facts.receivers_attached, |s| s.receivers.len());
    let keeper = match &facts.keeper {
        None => KeeperState::Unknown,
        Some(k) if k.state != "active" => KeeperState::Stopped,
        Some(k) if k.connected => KeeperState::Connected,
        Some(_) => KeeperState::Waiting,
    };
    Assessment {
        driver,
        bridge,
        keeper,
        receivers,
        findings,
        notes: notes(&facts.full),
    }
}

fn notes(full: &Full) -> Vec<String> {
    let Full::Ready(facts) = full else {
        return Vec::new();
    };
    let note = |(name, presence): &(&str, Presence)| {
        let until = match presence {
            Presence::Unreachable => "come back in range",
            _ => "wake",
        };
        format!("Waiting for {name} to {until}; OpenRazer lists it once it answers.")
    };
    facts.waiting.iter().map(note).collect()
}

fn readiness(facts: &Facts) -> Option<Finding> {
    let snapshot = match &facts.bridge {
        Bridge::NoAccess { socket } => {
            return Some(Finding::new(
                ErrorCode::NoSocketAccess,
                &[("socket", socket)],
            ));
        }
        Bridge::Down { why } => {
            let state = facts.service.as_deref().unwrap_or("in an unknown state");
            let details = [("state", state), ("why", why.as_str())];
            return Some(Finding::new(ErrorCode::ServiceDown, &details));
        }
        Bridge::Up { snapshot, .. } => snapshot,
    };
    let not_ready = |state: &str| Finding::new(ErrorCode::ReceiverNotReady, &[("receiver", state)]);
    if snapshot.receivers.is_empty() {
        return (facts.receivers_attached > 0).then(|| not_ready("it isn't registered"));
    }
    if snapshot.receivers.iter().any(|r| !r.writable) {
        return Some(not_ready("it's open read-only"));
    }
    let paired = snapshot.receivers.iter().any(|r| !r.devices.is_empty());
    let devices = if paired { "none online" } else { "none paired" };
    let usable = lines::devices(snapshot).any(|(_, d)| d.controllable);
    (!usable).then(|| Finding::new(ErrorCode::NoDeviceOnline, &[("devices", devices)]))
}

/// The keeper's unit is not active, or it is and the bridge is up but does
/// not have it. Without systemd to ask, we say nothing.
fn keeper_finding(facts: &Facts) -> Option<Finding> {
    let keeper = facts.keeper.as_ref()?;
    let problem = if keeper.state != "active" {
        format!("is {}", keeper.state)
    } else if !keeper.connected && facts.snapshot().is_some() {
        "is running, but the bridge doesn't have it".to_owned()
    } else {
        return None;
    };
    Some(Finding::new(
        ErrorCode::KeeperDown,
        &[("problem", &problem)],
    ))
}

/// A reading the desktop does not show: the bridge has a level for the
/// device, but no power supply has its name, or none with its name has a
/// level to read. hid-input names a uhid battery after the device.
fn battery_findings(facts: &Facts) -> Vec<Finding> {
    let Some(snapshot) = facts.snapshot() else {
        return Vec::new();
    };
    let problem = |catalog: &devices::Device| {
        let named = |b: &&PowerSupply| b.model.eq_ignore_ascii_case(catalog.name);
        let entries: Vec<&PowerSupply> = facts.batteries.iter().filter(named).collect();
        if entries.is_empty() {
            Some("there's no power supply entry for it")
        } else if !entries.iter().any(|b| b.level) {
            Some("its power supply entry has no level to read")
        } else {
            None
        }
    };
    lines::devices(snapshot)
        .filter(|(_, d)| d.battery.level.is_some() && d.kind != DeviceKind::Unknown)
        .filter_map(|(_, d)| devices::device(d.product_id))
        .filter_map(|catalog| {
            let details = [("device", catalog.name), ("problem", problem(catalog)?)];
            Some(Finding::new(ErrorCode::NoBatteryEntry, &details))
        })
        .collect()
}

/// A receiver whose devices neither its paired slots nor its pairing
/// inventory could name.
fn inventory_findings(facts: &Facts) -> Vec<Finding> {
    let receivers = facts.snapshot().map_or(&[][..], |s| &s.receivers);
    let finding = |r: &ReceiverInfo| {
        let problem = match r.inventory? {
            Inventory::NotRecognised => "isn't laid out the way the bridge knows",
            Inventory::Absent => "couldn't be read",
            Inventory::Slots | Inventory::KnownOffset | Inventory::Marker => return None,
        };
        let details = [("receiver", r.receiver_id.as_str()), ("problem", problem)];
        Some(Finding::new(ErrorCode::InventoryNotRecognised, &details))
    };
    receivers.iter().filter_map(finding).collect()
}

/// An online device the bridge offers OpenRazer whose OpenRazer device it
/// could not create. One that is asleep is a note (`waiting_for`), and one
/// on its cable has its wired device in OpenRazer.
fn openrazer_device_findings(facts: &Facts) -> Vec<Finding> {
    let devices = facts.snapshot().into_iter().flat_map(openrazer_devices);
    devices
        .filter(|(d, _)| d.presence == Presence::Online)
        .filter_map(|(d, catalog)| {
            let o = d.openrazer.as_ref()?;
            let failed = matches!(o.state, OpenRazerState::Retrying | OpenRazerState::Blocked);
            let reason = o.reason.as_ref().map_or("no reason given", |r| r.as_str());
            let details = [("device", catalog.name), ("reason", reason)];
            failed.then(|| Finding::new(ErrorCode::OpenRazerDeviceFailed, &details))
        })
        .collect()
}

/// A uaccess ACL on /dev/uhid only matters to the patched OpenRazer
/// drivers, which trust what can be created there.
fn uhid_acl_findings(facts: &Facts) -> Vec<Finding> {
    if facts.full == Full::NotInstalled {
        return Vec::new();
    }
    let finding =
        |uid: &u32| Finding::new(ErrorCode::UhidSharedWithUser, &[("uid", &uid.to_string())]);
    facts.uhid_acl_users.iter().map(finding).collect()
}

/// OpenRGB's generic LampArray driver takes any LampArray it can open. One
/// that is open to a person puts each paired device in OpenRGB twice.
fn lamparray_finding(facts: &Facts) -> Option<Finding> {
    let why = facts.lamparray_open.join("; ");
    let finding = Finding::new(ErrorCode::LampArrayOpenToUser, &[("why", &why)]);
    (!why.is_empty()).then_some(finding)
}

fn blocking_module(modules: &[Module]) -> Option<(ErrorCode, &'static str, &'static str)> {
    use ErrorCode::{
        OpenRazerDriverNotActive as NotActive, OpenRazerDriverNotPatched as NotPatched,
    };
    let stale = |m: &&Module| {
        m.loaded.as_ref().is_some_and(|id| *id != m.installed) || m.capability_loaded == Some(false)
    };
    if let Some(m) = modules.iter().find(|m| m.installed.is_none()) {
        return Some((NotActive, m.name, "not installed"));
    }
    if let Some(m) = modules.iter().find(|m| m.secure_boot_blocked) {
        return Some((
            ErrorCode::SecureBootBlocked,
            m.name,
            "unsigned for Secure Boot",
        ));
    }
    if let Some(m) = modules.iter().find(|m| m.capability == Some(false)) {
        return Some((NotPatched, m.name, "no capability"));
    }
    modules
        .iter()
        .find(stale)
        .map(|m| (NotActive, m.name, "older copy loaded"))
}

fn full_findings(full: &Full) -> Vec<Finding> {
    let facts = match full {
        Full::NotInstalled => return Vec::new(),
        Full::Invalid(reason) => {
            return vec![Finding::new(ErrorCode::FullMismatch, &[("reason", reason)])];
        }
        Full::Ready(facts) => facts,
    };
    let mut findings = Vec::new();
    if let Some((code, module, state)) = blocking_module(&facts.modules) {
        findings.push(Finding::new(code, &[("module", module), ("state", state)]));
    }
    for link in &facts.broken_links {
        findings.push(Finding::new(ErrorCode::BrokenInputLink, &[("link", link)]));
    }
    let group = ("group", OPENRAZER_GROUP);
    let access = match facts.access {
        Access::NotApplicable | Access::Ready => None,
        Access::GroupMissing => Some((ErrorCode::OpenRazerGroupMissing, "group missing")),
        Access::NotMember => Some((ErrorCode::OpenRazerNotReady, "not a member")),
        Access::DaemonNotRunning => Some((ErrorCode::OpenRazerNotReady, "daemon not running")),
        Access::Unverified => Some((ErrorCode::OpenRazerNotReady, "unverified")),
    };
    if let Some((code, state)) = access {
        findings.push(Finding::new(code, &[group, ("access", state)]));
    } else if !facts.unpublished.is_empty() {
        let missing = facts.unpublished.join(",");
        let details = [group, ("access", "devices missing"), ("missing", &missing)];
        findings.push(Finding::new(ErrorCode::OpenRazerNotReady, &details));
    }
    for link in &facts.missing_key_links {
        findings.push(Finding::new(ErrorCode::KeyLinkMissing, &[("link", link)]));
    }
    findings
}

// ------------------------------------------------------------------ output

#[must_use]
pub fn text(assessment: &Assessment, full: &Full, explain: bool) -> String {
    let (_, driver) = assessment.driver.words();
    let (_, bridge) = assessment.bridge.words();
    let receivers = count(assessment.receivers, "receiver");
    let working = format!("Driver {driver}, bridge {bridge}, {receivers}.\n");
    let ready = assessment.ready();
    let block = |f: &Finding| {
        format!(
            "[{}] {}\nWhy: {}\nDo: {}\nVerify: {}\n",
            f.code.id(),
            f.summary(),
            f.cause(),
            f.action(),
            f.verify()
        )
    };
    let findings = &assessment.findings;
    let mut out = match findings.first() {
        None => format!("HyperFlux Next V2 is ready.\n{working}"),
        Some(first) if ready => format!(
            "HyperFlux Next V2 is ready, with {}.\n{working}Warning: {}",
            count(findings.len(), "warning"),
            block(first)
        ),
        Some(first) => format!(
            "HyperFlux Next V2 needs attention.\nPrimary issue: {}",
            block(first)
        ),
    };
    let rest = findings.get(1..).unwrap_or_default();
    if explain {
        for f in rest {
            out.push_str(&format!("\n{}", block(f)));
        }
    } else if !rest.is_empty() {
        let kind = if ready {
            "more warning"
        } else {
            "more finding"
        };
        let more = count(rest.len(), kind);
        out.push_str(&format!(
            "More: {more}; run hyperfluxctl doctor --explain.\n"
        ));
    }
    for note in &assessment.notes {
        out.push_str(&format!("{note}\n"));
    }
    if !ready {
        return out + "Support: hyperfluxctl support-bundle --preview if the problem remains.\n";
    }
    match full {
        Full::NotInstalled => out.push_str("Optional Full integration is not installed.\n"),
        Full::Ready(facts) if !facts.openrgb_plugin => {
            out.push_str("Optional OpenRGB plugin is not installed.\n");
        }
        _ => {}
    }
    if findings.is_empty() {
        out.push_str("No action required.\n");
    }
    out
}

#[must_use]
pub fn json(facts: &Facts, assessment: &Assessment, fixes: &[crate::fix::Fix]) -> Value {
    let (driver, driver_text) = assessment.driver.words();
    let ready = matches!(facts.full, Full::Ready(_));
    let plugin = match &facts.full {
        Full::Ready(f) => Some(f.openrgb_plugin),
        _ => None,
    };
    let bridge_version = match &facts.bridge {
        Bridge::Up { version, .. } => Some(version.as_str()),
        _ => None,
    };
    let findings: Vec<Value> = assessment.findings.iter().map(Finding::json).collect();
    let fixes: Vec<Value> = fixes.iter().map(crate::fix::Fix::json).collect();
    json!({
        "state": match (findings.is_empty(), assessment.ready()) {
            (true, _) => "ready",
            (false, true) => "ready-with-warnings",
            (false, false) => "needs-attention",
        },
        "version": PRODUCT_VERSION,
        "driver": driver,
        "driver_text": driver_text,
        "bridge": assessment.bridge.words().0,
        "bridge_version": bridge_version,
        "keeper": assessment.keeper.words().0,
        "transport": facts.snapshot().map(|s| s.transport),
        "receivers": assessment.receivers,
        "full": {"installed": facts.full != Full::NotInstalled, "ready": ready, "openrgb_plugin": plugin},
        "findings": findings,
        "notes": assessment.notes,
        "fixes": fixes,
        "next": assessment.next(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GID: u32 = 961;
    const UID: u32 = 961;

    fn healthy() -> Facts {
        let snapshot = Box::new(lines::tests::snapshot());
        let interface = |n: u8| Interface {
            number: Some(n),
            driver: Some("hid-generic".into()),
            hidraw: Some((
                format!("/dev/hidraw{n}"),
                Some((UID, GID, if n < 2 { 0o640 } else { 0o660 })),
            )),
        };
        Facts {
            service: Some("active".into()),
            bridge: Bridge::Up {
                snapshot,
                version: PRODUCT_VERSION.into(),
            },
            keeper: Some(Keeper {
                state: "active".into(),
                connected: true,
            }),
            receivers_attached: 1,
            interfaces: (0..5).map(interface).collect(),
            bridge_uid: Some(UID),
            bridge_gid: Some(GID),
            keeper_gid: Some(GID),
            uhid: Some((0, GID, 0o660)),
            uhid_acl_users: Vec::new(),
            lamparray_open: Vec::new(),
            leftovers: Leftovers::default(),
            retired: Vec::new(),
            batteries: ["Razer Basilisk V3 Pro 35K", "Razer DeathStalker V2 Pro TKL"]
                .map(|model| PowerSupply {
                    model: model.into(),
                    level: true,
                })
                .to_vec(),
            full: Full::NotInstalled,
        }
    }

    fn full_ready() -> FullFacts {
        let module = |name| {
            let patched = PATCHED_MODULES.contains(&name).then_some(true);
            let id = Some("AB12".to_owned());
            Module {
                name,
                installed: id.clone(),
                loaded: Some(id),
                capability: patched,
                capability_loaded: patched,
                secure_boot_blocked: false,
            }
        };
        FullFacts {
            openrgb_plugin: true,
            modules: MODULES.iter().map(|m| module(*m)).collect(),
            broken_links: Vec::new(),
            access: Access::Ready,
            unpublished: Vec::new(),
            waiting: Vec::new(),
            missing_key_links: Vec::new(),
        }
    }

    fn codes(facts: &Facts) -> Vec<&'static str> {
        assess(facts).findings.iter().map(|f| f.code.id()).collect()
    }

    #[test]
    fn healthy_is_ready() {
        let assessment = assess(&healthy());
        assert!(assessment.findings.is_empty());
        assert_eq!(assessment.driver, Driver::Active);
    }

    #[test]
    fn receiver_access_hides_bridge_findings() {
        let mut facts = healthy();
        facts.bridge = Bridge::Down { why: "x".into() };
        facts.interfaces[1].hidraw = None;
        facts.interfaces[1].driver = Some(OLD_MODULE.into());
        facts.leftovers.loaded = Some("2.2.0".into());
        assert_eq!(codes(&facts), ["HFX-303", "HFX-202"]);
        let assessment = assess(&facts);
        assert_eq!(assessment.driver.words().1, "owned by hid-hyperflux-next");
        let cause = assessment.findings[0].cause();
        assert_eq!(
            cause,
            "Left over from an earlier release: hid-hyperflux-next 2.2.0 (still loaded). \
             HyperFlux Next V2 reaches the receiver without a kernel module of its own."
        );
        let mut facts = healthy();
        facts.interfaces[4].hidraw = Some(("/dev/hidraw4".into(), Some((0, 0, 0o600))));
        assert_eq!(
            assess(&facts).findings[0].detail("receiver"),
            Some("/dev/hidraw4 is group 0, mode 0600")
        );
        // Interface 2 isn't one the bridge opens.
        let mut facts = healthy();
        facts.interfaces[2].hidraw = None;
        assert!(codes(&facts).is_empty());
    }

    #[test]
    fn uhid_and_readiness() {
        let mut facts = healthy();
        facts.uhid = Some((0, 0, 0o600));
        let finding = &assess(&facts).findings[0];
        assert_eq!(finding.code.id(), "HFX-408");
        assert_eq!(
            finding.cause(),
            "The keeper needs /dev/uhid, with group hyperflux-keeper and mode 0660, \
             but it is group 0, mode 0600."
        );
        let mut facts = healthy();
        if let Bridge::Up { snapshot, .. } = &mut facts.bridge {
            snapshot.receivers[0].writable = false;
        }
        assert_eq!(
            assess(&facts).findings[0].cause(),
            "The bridge can't use the receiver: it's open read-only. Its hidraw interface 0 \
             has to belong to the hyperflux-next user and the hyperflux-keeper group with mode \
             0640, interface 1 to the hyperflux-keeper group with mode 0640, and interface 4 \
             to the hyperflux-next group with mode 0660."
        );
        let mut facts = healthy();
        facts.bridge = Bridge::NoAccess {
            socket: "/run/user/1000/x.sock".into(),
        };
        assert_eq!(codes(&facts), ["HFX-204"]);
        let mut facts = healthy();
        if let Bridge::Up { snapshot, .. } = &mut facts.bridge {
            snapshot.receivers[0]
                .devices
                .iter_mut()
                .for_each(|d| d.controllable = false);
        }
        assert_eq!(codes(&facts), ["HFX-203"]);
    }

    #[test]
    fn battery_entries_and_uhid_acls() {
        let mut facts = healthy();
        facts.batteries.truncate(1);
        facts.uhid_acl_users = vec![1000];
        let findings = assess(&facts).findings;
        assert_eq!(
            findings.len(),
            1,
            "the ACL only matters with the patched OpenRazer drivers"
        );
        assert_eq!(findings[0].code, ErrorCode::NoBatteryEntry);
        assert_eq!(
            findings[0].cause(),
            "The bridge has a battery reading for Razer DeathStalker V2 Pro TKL, but there's no \
             power supply entry for it, so UPower doesn't list it."
        );
        // Nothing to show before the first reading.
        if let Bridge::Up { snapshot, .. } = &mut facts.bridge {
            snapshot.receivers[0].devices[1].battery.level = None;
        }
        assert!(codes(&facts).is_empty());
        facts.full = Full::Ready(Box::new(full_ready()));
        let findings = assess(&facts).findings;
        assert_eq!(findings[0].code, ErrorCode::UhidSharedWithUser);
        assert!(
            findings[0]
                .cause()
                .starts_with("An ACL lets uid 1000 write /dev/uhid")
        );
    }

    #[test]
    fn battery_entry_without_level() {
        let mut facts = healthy();
        facts.batteries[0].level = false;
        let findings = assess(&facts).findings;
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].cause(),
            "The bridge has a battery reading for Razer Basilisk V3 Pro 35K, but its power supply \
             entry has no level to read, so UPower doesn't list it."
        );
        // Another entry of the same name with a level is enough.
        facts.batteries.push(PowerSupply {
            model: "Razer Basilisk V3 Pro 35K".into(),
            level: true,
        });
        assert!(codes(&facts).is_empty());
    }

    #[test]
    fn old_driver_advice() {
        let mut facts = healthy();
        facts.leftovers.loaded = Some("2.2.0".into());
        let finding = &assess(&facts).findings[0];
        assert_eq!(
            finding.action(),
            "None of it is installed any more. Reboot, and the receiver goes back to the \
             kernel's own HID drivers."
        );
        let file = "/usr/lib/modules/7.2.11-arch1-1/updates/dkms/hid-hyperflux-next.ko.zst";
        facts.leftovers.files = vec![file.into()];
        let finding = &assess(&facts).findings[0];
        assert_eq!(finding.detail("registration"), Some(file));
        assert_eq!(
            finding.action(),
            format!("Delete {file}, run sudo depmod -a, then reboot.")
        );
        facts.leftovers.dkms = vec!["hid-hyperflux-next/2.2.0".into()];
        assert!(assess(&facts).findings[0].action().starts_with(
            "Remove its DKMS registration with: sudo dkms remove hid-hyperflux-next/2.2.0 --all"
        ));
        facts.leftovers.package = Some("apt");
        assert!(
            assess(&facts).findings[0]
                .action()
                .starts_with("Remove the old 2.x package, hyperflux-next-dkms,")
        );
    }

    #[test]
    fn inert_dkms_records_last() {
        let mut facts = healthy();
        facts.leftovers.inert = vec!["hid-hyperflux-next/0.0.0-dev.1".into()];
        facts.leftovers.cleanup = vec!["/var/lib/dkms/hid-hyperflux-next".into()];
        let assessment = assess(&facts);
        assert_eq!(assessment.findings.len(), 1);
        let finding = &assessment.findings[0];
        assert_eq!(finding.code.id(), "HFX-305");
        assert_eq!(finding.code.entry().severity.as_str(), "warning");
        assert_eq!(
            finding.action(),
            "Delete them when it suits you: sudo rm -r /var/lib/dkms/hid-hyperflux-next. \
             hyperfluxctl doctor --fix asks before it runs that."
        );
        let text = text(&assessment, &facts.full, true);
        assert!(!text.contains("package"), "{text}");
        facts.batteries.truncate(1);
        assert_eq!(codes(&facts), ["HFX-205", "HFX-305"]);
    }

    #[test]
    fn waiting_device_is_a_note() {
        let mut facts = healthy();
        let mut full = full_ready();
        full.waiting = vec![
            ("Razer DeathStalker V2 Pro TKL", Presence::Asleep),
            ("Razer Basilisk V3 Pro 35K", Presence::Unreachable),
        ];
        facts.full = Full::Ready(Box::new(full));
        let assessment = assess(&facts);
        assert!(assessment.findings.is_empty());
        assert_eq!(
            text(&assessment, &facts.full, false),
            "HyperFlux Next V2 is ready.
Driver active, bridge running, 1 receiver.
Waiting for Razer DeathStalker V2 Pro TKL to wake; OpenRazer lists it once it answers.
Waiting for Razer Basilisk V3 Pro 35K to come back in range; OpenRazer lists it once it answers.
No action required.
"
        );
    }

    #[test]
    fn bridge_side_openrazer_failure() {
        let mut facts = healthy();
        let Bridge::Up { snapshot, .. } = &mut facts.bridge else {
            unreachable!()
        };
        let mouse = snapshot.receivers[0].devices[0].openrazer.as_mut().unwrap();
        mouse.state = OpenRazerState::Retrying;
        mouse.reason = hfx_proto::ids::HumanMessage::new("serial read failed").ok();
        let mut cabled = (**snapshot).clone();
        assert_eq!(
            assess(&facts).findings[0].cause(),
            "The bridge couldn't create the OpenRazer device for Razer Basilisk V3 Pro 35K: \
             serial read failed. Until it does, OpenRazer and Polychromatic don't list the device."
        );
        // Asleep, it's waited for. On its cable, OpenRazer has the wired one.
        cabled.receivers[0].devices[0].presence = Presence::Asleep;
        assert_eq!(waiting_for(&cabled, &[]).len(), 2);
        let mouse = cabled.receivers[0].devices[0].openrazer.as_mut();
        mouse.unwrap().state = OpenRazerState::Cable;
        assert_eq!(waiting_for(&cabled, &[]).len(), 1);
        assert!(expected_published(&cabled).is_empty());
    }

    #[test]
    fn full_section_order() {
        let mut facts = healthy();
        facts.bridge = Bridge::Down { why: "x".into() };
        let mut full = full_ready();
        full.modules[1].capability = Some(false);
        full.modules[3].installed = None;
        full.broken_links = vec!["/dev/input/by-id/a-event-kbd".into()];
        full.access = Access::NotMember;
        full.missing_key_links = vec!["/dev/input/by-id/hyperflux-next-*-x-event-kbd".into()];
        facts.full = Full::Ready(Box::new(full));
        assert_eq!(
            codes(&facts),
            ["HFX-201", "HFX-401", "HFX-407", "HFX-404", "HFX-406"]
        );
        assert_eq!(
            assess(&facts).findings[1].detail("module"),
            Some("razermouse")
        );
    }

    #[test]
    fn openrazer_module_precedence() {
        let mut full = full_ready();
        full.modules[1].loaded = Some(Some("OLD".into()));
        full.modules[3].capability = Some(false);
        let found = blocking_module(&full.modules).unwrap();
        assert_eq!(
            (found.0, found.1),
            (ErrorCode::OpenRazerDriverNotPatched, "razermouse")
        );
        full.modules[0].secure_boot_blocked = true;
        let found = blocking_module(&full.modules).unwrap();
        assert_eq!(
            (found.0, found.1),
            (ErrorCode::SecureBootBlocked, "razeraccessory")
        );
        full.modules[0].secure_boot_blocked = false;
        full.modules[3].capability = Some(true);
        let found = blocking_module(&full.modules).unwrap();
        assert_eq!(
            (found.0, found.1),
            (ErrorCode::OpenRazerDriverNotActive, "razerkbd")
        );
    }

    #[test]
    fn needs_attention_text() {
        let mut facts = healthy();
        let registrations = [
            "hid-razer-hyperflux-v2/0.1.1",
            "hid-razer-hyperflux-v2/0.1.2",
        ];
        facts.retired = registrations.map(String::from).to_vec();
        facts.uhid = Some((0, 0, 0o600));
        assert_eq!(
            text(&assess(&facts), &facts.full, false),
            "HyperFlux Next V2 needs attention.
Primary issue: [HFX-303] An old receiver driver is still on this system and can take the receiver away from the bridge.
Why: Left over from an earlier release: hid-razer-hyperflux-v2/0.1.1. HyperFlux Next V2 reaches the receiver without a kernel module of its own.
Do: Remove its DKMS registration with: sudo dkms remove hid-razer-hyperflux-v2/0.1.1 --all, then reboot. hyperfluxctl doctor --fix offers to run that.
Verify: Run hyperfluxctl doctor again and check that no old driver is listed.
More: 2 more findings; run hyperfluxctl doctor --explain.
Support: hyperfluxctl support-bundle --preview if the problem remains.
"
        );
    }

    #[test]
    fn ready_with_warnings_text() {
        let mut facts = healthy();
        facts.full = Full::Ready(Box::new(full_ready()));
        facts.uhid_acl_users = vec![1000];
        let assessment = assess(&facts);
        assert!(assessment.ready());
        let out = text(&assessment, &facts.full, false);
        assert!(
            out.starts_with(
                "HyperFlux Next V2 is ready, with 1 warning.
Driver active, bridge running, 1 receiver.
Warning: [HFX-409] Programs other than the device keeper can create devices that OpenRazer would trust.
Why: An ACL lets uid 1000 write /dev/uhid,"
            ),
            "{out}"
        );
        assert!(
            out.ends_with(
                "Verify: Run hyperfluxctl doctor again and check that the warning is gone.\n"
            ),
            "{out}"
        );
        assert_eq!(
            assessment.next(),
            "no action required (1 warning: run hyperfluxctl doctor)"
        );
        facts.batteries.truncate(1);
        let assessment = assess(&facts);
        let out = text(&assessment, &facts.full, false);
        assert!(
            out.starts_with("HyperFlux Next V2 is ready, with 2 warnings.\n"),
            "{out}"
        );
        assert!(out.contains("\nMore: 1 more warning; run hyperfluxctl doctor --explain.\n"));
        assert_eq!(
            assessment.next(),
            "no action required (2 warnings: run hyperfluxctl doctor)"
        );
        // One error among them and it isn't ready.
        facts.uhid = Some((0, 0, 0o600));
        let assessment = assess(&facts);
        assert!(!assessment.ready());
        let out = text(&assessment, &facts.full, false);
        assert!(
            out.starts_with("HyperFlux Next V2 needs attention.\n"),
            "{out}"
        );
        assert_eq!(assessment.next(), assessment.findings[0].action());
    }

    #[test]
    fn link_patterns() {
        let tkl = ".*DSV2Pro_TKL_000000000000-if01-event-mouse";
        let ours =
            "hyperflux-next-pci-0000_00_14_0-usb-0_3_1_1-DSV2Pro_TKL_000000000000-if01-event-mouse";
        assert!(may_match(ours, tkl));
        assert!(!may_match(
            "usb-Logitech_USB_Receiver-if01-event-mouse",
            tkl
        ));
        let bw = ".*Razer_Razer_BlackWidow_V3_Pro(_\\d+)?(-if01)?-event-kbd";
        assert!(may_match(
            "usb-Razer_Razer_BlackWidow_V3_Pro_1234-if01-event-kbd",
            bw
        ));
        assert!(!may_match(
            "usb-Razer_Razer_BlackWidow_V3_Pro-if01-event-mouse",
            bw
        ));
    }

    #[test]
    fn unpublished_counts_duplicates() {
        let expected = vec![(0x1532, 0xcd), (0x1532, 0xcd), (0x1532, 0x296)];
        let missing = missing_from(expected, vec![(0x1532, 0xcd)]);
        assert_eq!(missing, ["1532:00cd", "1532:0296"]);
    }
}
