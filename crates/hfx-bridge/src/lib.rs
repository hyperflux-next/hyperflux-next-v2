// SPDX-License-Identifier: GPL-2.0-only

//! hyperflux-next-bridge: the only writer to HyperFlux V2 receivers. It
//! serves the client protocol and decides the batteries the desktop shows
//! and the virtual HID devices OpenRazer binds, which the device keeper
//! (`keeper.rs`) holds for it. The library form exists so the integration
//! tests can run both against the simulator.

pub mod battery;
pub mod bridge;
pub mod capture;
pub mod charge;
pub mod decode;
pub mod exchange;
pub mod hidraw;
pub mod ipc;
pub mod keeper;
pub mod leases;
mod methods;
pub mod notify;
pub mod platform;
pub mod poll;
pub mod server;
pub mod sim;
pub mod state;
pub mod sysfs;
pub mod transport;
pub mod uhid;
pub mod vdev;
pub mod waits;
pub mod wire;

use platform::OpenRazerEnv;
use rustix::fd::OwnedFd;
use rustix::fs::{FileType, FlockOperation, Mode, OFlags};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use transport::Transport;

pub enum Source {
    System,
    /// A simulator listening at this path.
    Sim(PathBuf),
}

pub struct Config {
    pub socket: PathBuf,
    /// The default socket: check the runtime directory and use the
    /// system lock file.
    pub system: bool,
    pub source: Source,
    pub read_only: bool,
    /// The uid the device keeper has to run as.
    pub keeper_uid: u32,
    pub openrazer: Option<OpenRazerEnv>,
    /// Without `openrazer`, take OpenRazer from the simulator if it offers it.
    pub sim_openrazer: bool,
    /// See [`bridge::Setup`]. A simulator adds its suspends here.
    pub pretend_suspend: Option<Arc<AtomicU64>>,
    /// Where the learned waits are kept (`waits.rs`). None keeps nothing.
    pub state_dir: Option<PathBuf>,
}

fn open_transport(config: &mut Config) -> Result<Box<dyn Transport>, String> {
    let host: Box<dyn hidraw::HidHost> = match &config.source {
        Source::System => {
            Box::new(hidraw::SysHidraw::new().map_err(|e| format!("can't watch udev: {e}"))?)
        }
        Source::Sim(path) => {
            let slept = Arc::clone(config.pretend_suspend.get_or_insert_default());
            let mut sim = sim::SimHidraw::connect(path, slept)
                .map_err(|e| format!("can't reach the simulator at {}: {e}", path.display()))?;
            if config.sim_openrazer && config.openrazer.is_none() {
                config.openrazer = sim.openrazer();
            }
            Box::new(sim)
        }
    };
    Ok(Box::new(hidraw::HidrawTransport::new(host)))
}

/// The bound sockets and the lock that makes them ours. Dropping it removes
/// each socket, unless something else has replaced it meanwhile.
pub struct SocketGuard {
    sockets: Vec<Bound>,
    _lock: OwnedFd,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        for (path, identity) in &self.sockets {
            if let Ok(st) = rustix::fs::lstat(path)
                && (st.st_dev, st.st_ino) == *identity
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// A socket's path, and its device and inode, by which we know it as ours.
type Bound = (PathBuf, (u64, u64));

/// Binds a socket at `path`, replacing a stale one, open to its group.
fn listen(path: &Path) -> Result<(UnixListener, Bound), String> {
    let failed = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    match rustix::fs::lstat(path) {
        Ok(st) if FileType::from_raw_mode(st.st_mode) == FileType::Socket => {
            std::fs::remove_file(path).map_err(|e| failed(&e))?;
        }
        Ok(_) => {
            return Err(format!(
                "{} exists and isn't a socket; not replacing it",
                path.display()
            ));
        }
        Err(_) => {}
    }
    let listener = UnixListener::bind(path).map_err(|e| failed(&e))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))
        .map_err(|e| failed(&e))?;
    let st = rustix::fs::lstat(path).map_err(|e| failed(&e))?;
    Ok((listener, (path.to_owned(), (st.st_dev, st.st_ino))))
}

/// The device keeper's socket, beside the bridge's.
#[must_use]
pub fn keeper_socket(socket: &Path) -> PathBuf {
    socket.with_file_name(ipc::SOCKET_NAME)
}

fn check_runtime_dir(dir: &Path) -> Result<(), String> {
    let st = rustix::fs::stat(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mode = st.st_mode;
    if FileType::from_raw_mode(mode) != FileType::Directory
        || st.st_uid != rustix::process::geteuid().as_raw()
        || mode & 0o022 != 0
    {
        return Err(format!(
            "{} must be a directory owned by this user and writable by nobody else",
            dir.display()
        ));
    }
    Ok(())
}

/// Binds the protocol socket and the keeper's.
///
/// # Errors
/// One line saying what went wrong: another bridge holds the lock, a path is
/// something other than a socket, or the runtime directory is unsafe.
pub fn bind(
    socket: &Path,
    system: bool,
) -> Result<(UnixListener, UnixListener, SocketGuard), String> {
    let lock_path = if system {
        check_runtime_dir(Path::new(hfx_proto::paths::RUNTIME_DIR))?;
        PathBuf::from(hfx_proto::paths::BRIDGE_LOCK)
    } else {
        let mut path = socket.as_os_str().to_owned();
        path.push(".lock");
        PathBuf::from(path)
    };
    let lock = rustix::fs::open(
        &lock_path,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|e| format!("{}: {e}", lock_path.display()))?;
    rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|_| {
        format!(
            "another bridge is running (it holds {})",
            lock_path.display()
        )
    })?;
    let (listener, bound) = listen(socket)?;
    let (keeper, keeper_bound) = listen(&keeper_socket(socket))?;
    let sockets = vec![bound, keeper_bound];
    Ok((
        listener,
        keeper,
        SocketGuard {
            sockets,
            _lock: lock,
        },
    ))
}

/// Runs the bridge until `stop` becomes readable.
///
/// # Errors
/// A one-line description of a fatal start-up error.
pub fn run(mut config: Config, stop: OwnedFd) -> Result<(), String> {
    let (listener, keeper, guard) = bind(&config.socket, config.system)?;
    let setup = bridge::Setup {
        transport: open_transport(&mut config)?,
        openrazer: config.openrazer,
        read_only: config.read_only,
        pretend_suspend: config.pretend_suspend,
        state_dir: config.state_dir,
        keeper: (keeper, config.keeper_uid),
    };
    let bridge =
        bridge::Bridge::new(setup, listener, stop).map_err(|e| format!("start-up failed: {e}"))?;
    let result = bridge.run().map_err(|e| format!("event loop failed: {e}"));
    drop(guard);
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn bind_replaces_stale_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bridge.sock");
        std::fs::write(&path, "not a socket").unwrap();
        assert!(
            super::bind(&path, false)
                .err()
                .unwrap()
                .contains("isn't a socket")
        );
        std::fs::remove_file(&path).unwrap();
        let stale = std::os::unix::net::UnixListener::bind(&path).unwrap();
        drop(stale);
        let (_listener, _keeper, guard) = super::bind(&path, false).unwrap();
        assert!(
            super::bind(&path, false)
                .err()
                .unwrap()
                .contains("another bridge")
        );
        assert!(dir.path().join("keeper.sock").exists());
        drop(guard);
        assert!(!path.exists(), "the socket goes with its bridge");
        assert!(!dir.path().join("keeper.sock").exists());
    }
}
