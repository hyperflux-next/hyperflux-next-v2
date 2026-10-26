// SPDX-License-Identifier: GPL-2.0-only

//! What the bridge and the keeper need from the host besides receivers:
//! `/dev/uhid`, the OpenRazer drivers in sysfs, uevents and signals.

use rustix::fd::{BorrowedFd, OwnedFd};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Opens one virtual HID device file per call.
pub type UhidOpener = Arc<dyn Fn() -> io::Result<OwnedFd> + Send + Sync>;

#[must_use]
pub fn system_uhid() -> UhidOpener {
    Arc::new(|| {
        use rustix::fs::{Mode, OFlags};
        Ok(rustix::fs::open(
            hfx_proto::paths::UHID_DEVICE,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )?)
    })
}

/// The kernel's own uevents, and udev's after it has applied its rules.
pub const KERNEL_GROUP: u32 = 1;
pub const UDEV_GROUP: u32 = 2;

/// A non-blocking uevent socket for one multicast group.
///
/// # Errors
/// Fails when the socket can't be created or bound.
pub fn netlink_socket(group: u32) -> io::Result<OwnedFd> {
    use rustix::net::{AddressFamily, SocketFlags, SocketType, netlink};
    let socket = rustix::net::socket_with(
        AddressFamily::NETLINK,
        SocketType::DGRAM,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        Some(netlink::KOBJECT_UEVENT),
    )?;
    rustix::net::bind(&socket, &netlink::SocketAddrNetlink::new(0, group))?;
    Ok(socket)
}

/// One uevent, reduced to what the bridge reacts to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uevent {
    pub action: String,
    pub subsystem: String,
    pub driver: Option<String>,
}

fn property<'a>(message: &'a [u8], key: &str) -> Option<&'a str> {
    message
        .split(|b| *b == 0)
        .filter_map(|part| std::str::from_utf8(part).ok())
        .find_map(|text| text.strip_prefix(key)?.strip_prefix('='))
}

/// `SUBSYSTEM=` of a kernel or udev message (udev's binary header holds no
/// such text, so a plain search over the NUL-separated parts finds it).
#[must_use]
pub fn udev_subsystem(message: &[u8]) -> Option<&str> {
    property(message, "SUBSYSTEM")
}

/// Parses `ACTION@DEVPATH\0KEY=VALUE\0...` as the kernel multicasts it.
#[must_use]
pub fn parse_uevent(message: &[u8]) -> Option<Uevent> {
    let header = std::str::from_utf8(message.split(|b| *b == 0).next()?).ok()?;
    let (action, _) = header.split_once('@')?;
    Some(Uevent {
        action: action.to_owned(),
        subsystem: property(message, "SUBSYSTEM")
            .unwrap_or_default()
            .to_owned(),
        driver: property(message, "DRIVER").map(str::to_owned),
    })
}

/// Reads the queued uevents from `fd`.
pub fn read_uevents(fd: BorrowedFd<'_>) -> Vec<Uevent> {
    let mut events = Vec::new();
    let mut buffer = vec![0_u8; 8192];
    while let Ok(n) = rustix::io::read(fd, &mut buffer) {
        if n == 0 {
            break;
        }
        events.extend(parse_uevent(&buffer[..n]));
    }
    events
}

/// What the bridge needs from the system for the virtual OpenRazer devices.
pub struct OpenRazerEnv {
    pub sys_module: PathBuf,
    pub hid_drivers: PathBuf,
    /// Readable when uevents are queued: the netlink socket, or a datagram
    /// socket in tests.
    pub uevents: Option<OwnedFd>,
}

impl OpenRazerEnv {
    /// The real system: `/sys` and the kernel's uevents.
    #[must_use]
    pub fn system() -> Self {
        Self {
            sys_module: PathBuf::from(hfx_proto::paths::SYS_MODULE_DIR),
            hid_drivers: PathBuf::from(hfx_proto::paths::HID_DRIVERS_DIR),
            uevents: netlink_socket(KERNEL_GROUP)
                .inspect_err(|e| {
                    eprintln!("no uevent socket ({e}); wired devices and driver loads go unnoticed")
                })
                .ok(),
        }
    }

    /// Whether the loaded `driver` is the HyperFlux build of OpenRazer.
    #[must_use]
    pub fn capable(&self, driver: &str) -> bool {
        capability(&self.sys_module, driver)
    }
}

/// Becomes readable on SIGTERM, SIGINT or SIGQUIT.
///
/// # Errors
/// Fails when the socket pair cannot be made or a handler cannot be set.
pub fn stop_on_signals() -> io::Result<OwnedFd> {
    let (read, write) = std::os::unix::net::UnixStream::pair()?;
    read.set_nonblocking(true)?;
    for signal in signal_hook::consts::TERM_SIGNALS {
        signal_hook::low_level::pipe::register(*signal, write.try_clone()?)?;
    }
    Ok(OwnedFd::from(read))
}

/// The parameter reads back with a trailing newline.
#[must_use]
pub fn capability(sys_module: &Path, driver: &str) -> bool {
    let path = sys_module
        .join(driver)
        .join("parameters")
        .join(hfx_proto::openrazer::CAPABILITY_NAME);
    std::fs::read_to_string(path).is_ok_and(|v| v.trim() == hfx_proto::openrazer::CAPABILITY_VALUE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uevents() {
        let message = b"bind@/devices/pci0000:00/usb1/1-3/1-3:1.0/0003:1532:00CC.0007\0ACTION=bind\0SUBSYSTEM=hid\0DRIVER=razermouse\0SEQNUM=4410\0";
        assert_eq!(
            parse_uevent(message),
            Some(Uevent {
                action: "bind".into(),
                subsystem: "hid".into(),
                driver: Some("razermouse".into()),
            })
        );
        assert_eq!(parse_uevent(b""), None);
        let udev = b"libudev\0\xfe\xed\xca\xfe\x28\0\0\0ACTION=add\0SUBSYSTEM=hidraw\0DEVNAME=/dev/hidraw3\0";
        assert_eq!(udev_subsystem(udev), Some("hidraw"));
        assert_eq!(udev_subsystem(b"SUBSYSTEMS=usb\0"), None);
    }

    #[test]
    fn capability_parameter() {
        let dir = tempfile::tempdir().unwrap();
        let params = dir.path().join("razermouse/parameters");
        std::fs::create_dir_all(&params).unwrap();
        assert!(!capability(dir.path(), "razermouse"));
        std::fs::write(
            params.join("hyperflux_native_transport"),
            "razer-feature-report-v1\n",
        )
        .unwrap();
        assert!(capability(dir.path(), "razermouse"));
        assert!(!capability(dir.path(), "razerkbd"));
    }
}
