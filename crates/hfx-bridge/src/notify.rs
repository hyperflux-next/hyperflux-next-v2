// SPDX-License-Identifier: GPL-2.0-only

//! sd_notify without libsystemd: one datagram per state change to
//! `$NOTIFY_SOCKET`, plus the watchdog interval from `$WATCHDOG_USEC`.

use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};

pub struct Notifier {
    socket: UnixDatagram,
    address: SocketAddr,
}

impl Notifier {
    /// None when the bridge doesn't run under systemd (or the socket is
    /// unusable, which systemd would report as a start timeout anyway).
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Self::to(&std::env::var_os("NOTIFY_SOCKET")?)
    }

    /// A path, or an abstract name after `@`.
    #[must_use]
    pub fn to(path: &std::ffi::OsStr) -> Option<Self> {
        let address = match path.as_encoded_bytes().strip_prefix(b"@") {
            Some(name) => SocketAddr::from_abstract_name(name).ok()?,
            None => SocketAddr::from_pathname(path).ok()?,
        };
        let socket = UnixDatagram::unbound().ok()?;
        Some(Self { socket, address })
    }

    pub fn send(&self, state: &str) {
        let _ = self.socket.send_to_addr(state.as_bytes(), &self.address);
    }
}

/// How often to send `WATCHDOG=1`: half of `$WATCHDOG_USEC`, when the
/// watchdog is meant for this process.
#[must_use]
pub fn watchdog_interval_ms() -> Option<u64> {
    if let Ok(pid) = std::env::var("WATCHDOG_PID")
        && pid.trim().parse::<u32>().ok() != Some(std::process::id())
    {
        return None;
    }
    let usec: u64 = std::env::var("WATCHDOG_USEC").ok()?.trim().parse().ok()?;
    (usec >= 2000).then_some(usec / 2000)
}

#[cfg(test)]
mod tests {
    #[test]
    fn states_reach_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notify");
        let listener = std::os::unix::net::UnixDatagram::bind(&path).unwrap();
        super::Notifier::to(path.as_os_str())
            .unwrap()
            .send("READY=1");
        let mut buffer = [0_u8; 32];
        let n = listener.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..n], b"READY=1");
    }
}
