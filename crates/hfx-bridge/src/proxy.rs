// SPDX-License-Identifier: GPL-2.0-only

//! The proxy behind each user's socket, `hyperflux-next-proxy`. systemd
//! listens on `/run/user/UID/hyperflux-next-bridge.sock` and starts the
//! proxy as that user on the first connection, with the listening socket
//! as its standard input. The proxy copies every connection to and from
//! the bridge's system socket, byte for byte, and reads none of it.
//!
//! systemd-socket-proxyd did this up to 1.0.2. SELinux confines it
//! differently on each distribution, and on Tumbleweed its domain may not
//! write to the bridge's socket. Our own program in the bridge's directory
//! gets the bridge's label, and with it the bridge's domain
//! (ARCHITECTURE.md, section 3.4).

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

/// Connections at once. The socket is the user's own (mode 0600), so these
/// are their OpenRGB and hyperfluxctl. Past this a new one is closed.
pub const MAX_CONNECTIONS: usize = 64;

/// Takes connections until accepting fails, and returns that error.
/// Each connection gets its own threads, one per direction. While the
/// bridge is stopped each connection is closed, and the proxy keeps
/// listening for when it's back.
pub fn serve(listener: &UnixListener, bridge: &Path, max: usize) -> io::Error {
    let open = Arc::new(AtomicUsize::new(0));
    let reachable = Arc::new(AtomicBool::new(true));
    loop {
        let client = match listener.accept() {
            Ok((client, _)) => client,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted
                ) =>
            {
                continue;
            }
            Err(e) => return e,
        };
        if open.fetch_add(1, Ordering::SeqCst) >= max {
            open.fetch_sub(1, Ordering::SeqCst);
            eprintln!("{max} connections are open already; closing a new one");
            continue;
        }
        let (open, reachable, bridge) =
            (Arc::clone(&open), Arc::clone(&reachable), bridge.to_owned());
        let spawned = thread::Builder::new().name("relay".into()).spawn(move || {
            relay(client, &bridge, &reachable);
            open.fetch_sub(1, Ordering::SeqCst);
        });
        if let Err(e) = spawned {
            eprintln!("can't start a thread for a connection ({e}); closing it");
        }
    }
}

/// One connection, until both sides are done. A side that stops sending
/// gets its half closed on the other side, so a request followed by
/// shutdown(SHUT_WR) still gets its answer. A bridge that can't be reached
/// is logged once, not for every client that polls it meanwhile.
fn relay(client: UnixStream, path: &Path, reachable: &AtomicBool) {
    let bridge = match UnixStream::connect(path) {
        Ok(bridge) => {
            if !reachable.swap(true, Ordering::SeqCst) {
                eprintln!("the bridge answers again");
            }
            bridge
        }
        Err(e) => {
            if reachable.swap(false, Ordering::SeqCst) {
                eprintln!(
                    "can't reach the bridge at {} ({e}); closing connections until it's back",
                    path.display()
                );
            }
            return;
        }
    };
    let (Ok(client_in), Ok(bridge_out)) = (client.try_clone(), bridge.try_clone()) else {
        eprintln!("can't duplicate a connection's socket; closing it");
        return;
    };
    let upstream = thread::Builder::new()
        .name("relay-up".into())
        .spawn(move || copy(client_in, bridge_out));
    copy(bridge, client);
    if let Ok(upstream) = upstream {
        let _ = upstream.join();
    }
}

/// Copies until `from` ends, then closes `to` for writing. On an error both
/// sockets close in both directions, which also ends the other copy.
fn copy(mut from: UnixStream, mut to: UnixStream) {
    match io::copy(&mut from, &mut to) {
        Ok(_) => {
            let _ = to.shutdown(std::net::Shutdown::Write);
        }
        Err(_) => {
            let _ = from.shutdown(std::net::Shutdown::Both);
            let _ = to.shutdown(std::net::Shutdown::Both);
        }
    }
}
