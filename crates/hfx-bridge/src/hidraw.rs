// SPDX-License-Identifier: GPL-2.0-only

//! The transport: the receiver's own HID interfaces through hidraw, with
//! the kernel's ordinary HID drivers bound. Interface 0 carries the
//! exchanges (a 90-byte feature report without a report id, so hidraw's
//! buffer starts with a 0) and the mouse's movement, interface 1 the
//! passive vendor reports and composite input, interface 4 the pairing
//! inventory. The bridge opens interface 0 for the exchanges, which it
//! runs itself (`wire::run_exchange`), and reads interface 4. The device
//! keeper reads the input of interfaces 0 and 1.
//!
//! Receivers are found in sysfs and followed through udev's netlink
//! messages. The hidraw operations sit behind [`HidHost`] so the simulator
//! can play the receiver. [`ioctl`] is the crate's only unsafe code.

use crate::transport::{
    Control, Exchanged, Gone, Info, NodeChange, OpenError, Opened, Timing, Transport,
};
use crate::wire::{self, FeaturePipe, Rate};
use hfx_proto::devices::RECEIVER;
use hfx_proto::messages::Lane;
use hfx_proto::report::{REPORT_LEN, Report};
use rustix::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

/// One receiver as sysfs shows it: its USB device and the hidraw node of
/// each interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub usb_path: String,
    pub nodes: BTreeMap<u8, String>,
}

pub trait HidHost {
    /// # Errors
    /// Fails when sysfs can't be read.
    fn scan(&mut self) -> io::Result<Vec<Found>>;
    fn watch_fd(&self) -> Option<BorrowedFd<'_>>;
    /// Reads what the watch has. True when receivers may have come or gone.
    fn drain(&mut self) -> bool;
    /// # Errors
    /// The open's errno.
    fn open_input(&mut self, node: &str) -> Result<Box<dyn HidInput>, Errno>;
    /// # Errors
    /// The open's errno.
    fn open_feature(&mut self, node: &str) -> Result<Box<dyn HidFeature>, Errno>;
}

pub trait HidInput {
    fn fd(&self) -> BorrowedFd<'_>;
    /// One input report per call.
    ///
    /// # Errors
    /// EAGAIN when none is queued; anything else means the device is gone.
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Errno>;
}

pub trait HidFeature: Send {
    /// # Errors
    /// The transfer's errno.
    fn set(&mut self, report: &[u8]) -> Result<(), Errno>;
    /// `buffer[0]` names the report, and the answer replaces the buffer.
    ///
    /// # Errors
    /// The transfer's errno.
    fn get(&mut self, buffer: &mut [u8]) -> Result<usize, Errno>;
}

pub struct HidrawTransport {
    host: Box<dyn HidHost>,
    /// name (the USB path) -> interface nodes and generation.
    known: BTreeMap<String, (Found, u64)>,
    next_generation: u64,
}

fn opened(e: Errno) -> OpenError {
    match e {
        Errno::ACCESS | Errno::PERM => OpenError::Access,
        Errno::NODEV | Errno::NOENT | Errno::NXIO => OpenError::Gone,
        e => OpenError::Failed(e.to_string()),
    }
}

impl HidrawTransport {
    #[must_use]
    pub fn new(host: Box<dyn HidHost>) -> Self {
        Self {
            host,
            known: BTreeMap::new(),
            next_generation: 1,
        }
    }

    fn node(&self, name: &str, interface: u8) -> Result<String, OpenError> {
        let found = self.known.get(name).ok_or(OpenError::Gone)?;
        found
            .0
            .nodes
            .get(&interface)
            .cloned()
            .ok_or(OpenError::Gone)
    }

    /// The input of interfaces 1 and 0, which the device keeper reads.
    ///
    /// # Errors
    /// See [`OpenError`].
    pub fn inputs(&mut self, name: &str) -> Result<[Box<dyn HidInput>; 2], OpenError> {
        let composite = self.node(name, wire::IF_COMPOSITE)?;
        let pointer = self.node(name, wire::IF_POINTER)?;
        let composite = self.host.open_input(&composite).map_err(opened)?;
        Ok([composite, self.host.open_input(&pointer).map_err(opened)?])
    }

    fn rescan(&mut self) -> Vec<NodeChange> {
        let Ok(found) = self.host.scan() else {
            return Vec::new();
        };
        let mut changes = Vec::new();
        let names: Vec<String> = found.iter().map(|f| f.usb_path.clone()).collect();
        self.known.retain(|name, _| {
            let keep = names.contains(name);
            if !keep {
                changes.push(NodeChange::Gone(name.clone()));
            }
            keep
        });
        for f in found {
            match self.known.get_mut(&f.usb_path) {
                // New nodes under the same port mean the receiver was replugged.
                Some((old, _)) if *old == f => {}
                Some(entry) => {
                    changes.push(NodeChange::Gone(f.usb_path.clone()));
                    changes.push(NodeChange::Appeared(f.usb_path.clone()));
                    *entry = (f, self.next_generation);
                    self.next_generation += 1;
                }
                None => {
                    changes.push(NodeChange::Appeared(f.usb_path.clone()));
                    self.known
                        .insert(f.usb_path.clone(), (f, self.next_generation));
                    self.next_generation += 1;
                }
            }
        }
        changes
    }
}

impl Transport for HidrawTransport {
    fn kind(&self) -> hfx_proto::messages::Transport {
        hfx_proto::messages::Transport::Hidraw
    }

    fn scan(&mut self) -> io::Result<Vec<String>> {
        self.rescan();
        Ok(self.known.keys().cloned().collect())
    }

    fn watch_fd(&self) -> Option<BorrowedFd<'_>> {
        self.host.watch_fd()
    }

    fn changes(&mut self) -> Vec<NodeChange> {
        if !self.host.drain() {
            return Vec::new();
        }
        let mut changes = self.rescan();
        // udev reuses hidraw names and broadcasts its event once it has set
        // the node's group and mode, so a receiver opened a moment too early
        // looks unchanged here. We report a change anyway: the bridge and the
        // keeper retry the ones that refused them and ignore the rest.
        for name in self.known.keys() {
            let appeared = NodeChange::Appeared(name.clone());
            if !changes.contains(&appeared) {
                changes.push(NodeChange::Changed(name.clone()));
            }
        }
        changes
    }

    fn open(&mut self, name: &str, write: bool) -> Result<Opened, OpenError> {
        let generation = self.known.get(name).ok_or(OpenError::Gone)?.1;
        let control: Option<Box<dyn Control>> = if write {
            let node = self.node(name, wire::IF_POINTER)?;
            let feature = self.host.open_feature(&node).map_err(opened)?;
            Some(Box::new(HidrawControl(feature)))
        } else {
            None
        };
        let read = self.node(name, wire::IF_INVENTORY).ok().and_then(|n| {
            let mut buffer = vec![0_u8; wire::INVENTORY_LEN];
            buffer[0] = wire::INVENTORY_REPORT_ID;
            let len = self.host.open_feature(&n).ok()?.get(&mut buffer).ok()?;
            buffer.truncate(len);
            Some(buffer)
        });
        // The bridge falls back on these ids when the receiver doesn't
        // answer its paired-slots query.
        let inventory = wire::read_inventory(read.as_deref());
        let info = Info {
            generation,
            vendor_id: RECEIVER.vendor_id,
            product_id: RECEIVER.product_id,
            suspended: false,
            inventory,
        };
        Ok(Opened { info, control })
    }

    fn usb_path(&self, name: &str) -> Option<String> {
        self.known
            .get(name)
            .map(|(found, _)| found.usb_path.clone())
    }
}

/// Interface 0's input: a boot mouse's reports without report ids, about
/// 900 a second while the mouse moves. All that matters is that the
/// pointer moved. The keeper reads them in batches. What hidraw drops
/// from this reader's buffer meanwhile costs nothing: every reader has its
/// own, the desktop's input comes through the kernel's input path, and no
/// vendor reports come out of interface 0.
pub struct PointerInput {
    pub input: Box<dyn HidInput>,
    rate: Rate,
}

impl PointerInput {
    #[must_use]
    pub fn new(input: Box<dyn HidInput>) -> Self {
        Self {
            input,
            rate: Rate::default(),
        }
    }

    /// Empties the queue without blocking. True when there was something
    /// in it and an activity event is due, at most one per
    /// `ACTIVITY_INTERVAL_MS`.
    ///
    /// # Errors
    /// The receiver is gone.
    pub fn drain(&mut self, now: u64) -> Result<bool, Gone> {
        let mut buffer = [0_u8; 64];
        let mut moved = false;
        loop {
            match self.input.read(&mut buffer) {
                Ok(0) => return Err(Gone),
                Ok(_) => moved = true,
                Err(Errno::AGAIN | Errno::INTR) => break,
                Err(_) => return Err(Gone),
            }
        }
        Ok(moved && self.rate.allow(now, wire::ACTIVITY_INTERVAL_MS, 1))
    }
}

/// Receivers whose nodes refused to open. udev sets the permissions a
/// moment after a node appears, and we try each again on udev's next
/// event, then 100 ms, 500 ms and 2 s later in case that event was missed.
#[derive(Default)]
pub struct Waiting(BTreeMap<String, (Option<u64>, usize)>);

const ACCESS_RETRY_MS: [u64; 3] = [100, 500, 2_000];

impl Waiting {
    /// Notes a refusal. True for the first, which is worth a log line.
    pub fn refused(&mut self, name: &str, now: u64) -> bool {
        let tries = self.0.get(name).map_or(0, |(_, t)| *t);
        let at = ACCESS_RETRY_MS.get(tries).map(|ms| now + ms);
        self.0
            .insert(name.to_owned(), (at, tries + usize::from(at.is_some())));
        tries == 0
    }

    pub fn remove(&mut self, name: &str) {
        self.0.remove(name);
    }

    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }

    /// The names whose retry is due.
    pub fn due(&mut self, now: u64) -> Vec<String> {
        let due = self
            .0
            .iter_mut()
            .filter(|(_, (at, _))| at.is_some_and(|at| at <= now));
        due.map(|(name, (at, _))| {
            *at = None;
            name.clone()
        })
        .collect()
    }

    #[must_use]
    pub fn next(&self) -> Option<u64> {
        self.0.values().filter_map(|(at, _)| *at).min()
    }
}

struct HidrawControl(Box<dyn HidFeature>);

impl FeaturePipe for HidrawControl {
    fn set(&mut self, wire: &Report) -> Result<(), Errno> {
        let mut buffer = [0_u8; REPORT_LEN + 1];
        buffer[1..].copy_from_slice(wire);
        self.0.set(&buffer)
    }

    fn get(&mut self, response: &mut Report) -> Result<(), Errno> {
        let mut buffer = [0_u8; REPORT_LEN + 1];
        match self.0.get(&mut buffer)? {
            n if n == buffer.len() => {
                response.copy_from_slice(&buffer[1..]);
                Ok(())
            }
            _ => Err(Errno::PROTO),
        }
    }
}

impl Control for HidrawControl {
    fn exchange(&mut self, _lane: Lane, request: &Report, timing: Timing) -> Exchanged {
        let (result, gets, rereads) = wire::run_exchange(self, request, timing);
        Exchanged {
            result,
            gets,
            rereads,
        }
    }
}

// ------------------------------------------------------------ the system

/// hidraw nodes in sysfs and /dev, udev's netlink messages for hotplug.
pub struct SysHidraw {
    udev: OwnedFd,
}

// HIDIOCSFEATURE and HIDIOCGFEATURE: _IOC(READ|WRITE, 'H', nr, len).
const fn hidioc(nr: u32, len: usize) -> u32 {
    (3 << 30) | ((len as u32) << 16) | ((b'H' as u32) << 8) | nr
}

/// ioctl(2) with a buffer argument. Returns the call's non-negative result.
///
/// # Errors
/// The errno.
#[allow(unsafe_code)]
fn ioctl(fd: BorrowedFd<'_>, request: u32, buffer: &mut [u8]) -> Result<i32, Errno> {
    // SAFETY: `request` comes from `hidioc` with this buffer's length as its
    // size, so the kernel never reads or writes outside the buffer, which the
    // caller owns for the duration of the call.
    let result =
        unsafe { libc::ioctl(fd.as_raw_fd(), request as libc::Ioctl, buffer.as_mut_ptr()) };
    if result < 0 {
        Err(Errno::from_io_error(&io::Error::last_os_error()).unwrap_or(Errno::IO))
    } else {
        Ok(result)
    }
}

impl SysHidraw {
    /// # Errors
    /// Fails when the udev monitor socket cannot be bound.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            udev: crate::platform::netlink_socket(crate::platform::UDEV_GROUP)?,
        })
    }
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_owned())
}

impl HidHost for SysHidraw {
    fn scan(&mut self) -> io::Result<Vec<Found>> {
        let mut receivers: BTreeMap<String, Found> = BTreeMap::new();
        for entry in std::fs::read_dir("/sys/class/hidraw")? {
            let entry = entry?;
            let Ok(node_dir) = std::fs::canonicalize(entry.path()) else {
                continue;
            };
            // .../<usb device>/<interface>/<hid device>/hidraw/hidrawN
            let Some(hid) = node_dir.parent().and_then(Path::parent) else {
                continue;
            };
            let wanted = format!(":{:04X}:{:04X}.", RECEIVER.vendor_id, RECEIVER.product_id);
            if !hid
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(&wanted))
            {
                continue;
            }
            let Some(interface) = hid.parent() else {
                continue;
            };
            let Some(number) = read_trimmed(&interface.join("bInterfaceNumber"))
                .and_then(|n| u8::from_str_radix(&n, 16).ok())
            else {
                continue;
            };
            let Some(usb) = interface.parent().and_then(Path::to_str) else {
                continue;
            };
            let node = format!("/dev/{}", entry.file_name().to_string_lossy());
            receivers
                .entry(usb.to_owned())
                .or_insert_with(|| Found {
                    usb_path: usb.to_owned(),
                    nodes: BTreeMap::new(),
                })
                .nodes
                .insert(number, node);
        }
        Ok(receivers
            .into_values()
            .filter(|f| {
                f.nodes.contains_key(&wire::IF_POINTER) && f.nodes.contains_key(&wire::IF_COMPOSITE)
            })
            .collect())
    }

    fn watch_fd(&self) -> Option<BorrowedFd<'_>> {
        Some(self.udev.as_fd())
    }

    fn drain(&mut self) -> bool {
        let mut relevant = false;
        let mut buffer = vec![0_u8; 8192];
        while let Ok(n) = rustix::io::read(&self.udev, &mut buffer) {
            if n == 0 {
                break;
            }
            relevant |=
                crate::platform::udev_subsystem(&buffer[..n]).is_some_and(|s| s == "hidraw");
        }
        relevant
    }

    fn open_input(&mut self, node: &str) -> Result<Box<dyn HidInput>, Errno> {
        let fd = rustix::fs::open(
            node,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(Box::new(SysNode(fd)))
    }

    fn open_feature(&mut self, node: &str) -> Result<Box<dyn HidFeature>, Errno> {
        let fd = rustix::fs::open(node, OFlags::RDWR | OFlags::CLOEXEC, Mode::empty())?;
        Ok(Box::new(SysNode(fd)))
    }
}

struct SysNode(OwnedFd);

impl HidInput for SysNode {
    fn fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Errno> {
        rustix::io::read(&self.0, buffer)
    }
}

impl HidFeature for SysNode {
    fn set(&mut self, report: &[u8]) -> Result<(), Errno> {
        let mut copy = report.to_vec();
        ioctl(self.0.as_fd(), hidioc(0x06, copy.len()), &mut copy).map(drop)
    }

    fn get(&mut self, buffer: &mut [u8]) -> Result<usize, Errno> {
        let n = ioctl(self.0.as_fd(), hidioc(0x07, buffer.len()), buffer)?;
        Ok(usize::try_from(n).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn feature_ioctl_numbers() {
        // As the kernel defines them for a 91-byte buffer (report id + 90).
        assert_eq!(super::hidioc(0x06, 91), 0xc05b_4806);
        assert_eq!(super::hidioc(0x07, 91), 0xc05b_4807);
    }
}
