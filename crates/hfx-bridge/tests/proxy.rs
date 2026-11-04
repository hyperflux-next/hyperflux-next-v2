// SPDX-License-Identifier: GPL-2.0-only

//! The per-user socket's proxy between a client and a stand-in bridge that
//! echoes what it reads, in-process and as the program systemd starts.

use hfx_bridge::proxy;
use rustix::fd::OwnedFd;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// Sends back everything it reads, then closes. It only answers once the
/// client has closed its half, so an answer proves the half-close passed.
fn echo_bridge(path: &Path) {
    let listener = UnixListener::bind(path).unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            thread::spawn(move || {
                let mut data = Vec::new();
                stream.read_to_end(&mut data).unwrap();
                stream.write_all(&data).unwrap();
            });
        }
    });
}

fn ask(path: &Path, request: &[u8]) -> Vec<u8> {
    try_ask(path, request).unwrap()
}

fn try_ask(path: &Path, request: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut client = UnixStream::connect(path)?;
    client.set_read_timeout(Some(Duration::from_secs(5)))?;
    client.write_all(request)?;
    client.shutdown(std::net::Shutdown::Write)?;
    let mut answer = Vec::new();
    client.read_to_end(&mut answer)?;
    Ok(answer)
}

/// Closed without an answer. Closing a socket with unread input makes the
/// kernel reset the connection, so that counts as closed too.
fn closed(path: &Path, request: &[u8]) -> bool {
    match try_ask(path, request) {
        Ok(answer) => answer.is_empty(),
        Err(e) => matches!(
            e.kind(),
            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
        ),
    }
}

struct Paths {
    _dir: tempfile::TempDir,
    user: PathBuf,
    bridge: PathBuf,
}

fn paths() -> Paths {
    let dir = tempfile::tempdir().unwrap();
    let (user, bridge) = (dir.path().join("user.sock"), dir.path().join("bridge.sock"));
    Paths {
        _dir: dir,
        user,
        bridge,
    }
}

fn start(paths: &Paths, max: usize) {
    let listener = UnixListener::bind(&paths.user).unwrap();
    let bridge = paths.bridge.clone();
    thread::spawn(move || proxy::serve(&listener, &bridge, max));
}

#[test]
fn both_ways() {
    let p = paths();
    echo_bridge(&p.bridge);
    start(&p, proxy::MAX_CONNECTIONS);
    // Larger than a socket buffer, so the copies have to take turns.
    let request: Vec<u8> = (0..1_000_000u32).map(|i| i as u8).collect();
    assert_eq!(ask(&p.user, &request), request);
    assert_eq!(ask(&p.user, b"again"), b"again");
}

#[test]
fn no_bridge() {
    let p = paths();
    start(&p, proxy::MAX_CONNECTIONS);
    assert!(closed(&p.user, b"anyone?"));
}

#[test]
fn bridge_returns() {
    let p = paths();
    start(&p, proxy::MAX_CONNECTIONS);
    assert!(closed(&p.user, b"anyone?"));
    echo_bridge(&p.bridge);
    assert_eq!(ask(&p.user, b"back?"), b"back?");
}

#[test]
fn too_many() {
    let p = paths();
    echo_bridge(&p.bridge);
    start(&p, 1);
    let mut first = UnixStream::connect(&p.user).unwrap();
    first.write_all(b"first").unwrap();
    // The second is closed unanswered while the first stays open.
    assert!(closed(&p.user, b"second"));
    first.shutdown(std::net::Shutdown::Write).unwrap();
    let mut answer = Vec::new();
    first.read_to_end(&mut answer).unwrap();
    assert_eq!(answer, b"first");
    // With the first gone there's room again. It ends on its own thread,
    // just after the answer.
    for _ in 0..50 {
        if try_ask(&p.user, b"third").is_ok_and(|answer| answer == b"third") {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("no room after the first connection closed");
}

const PROGRAM: &str = env!("CARGO_BIN_EXE_hyperflux-next-proxy");

#[test]
fn program_takes_its_socket_on_stdin() {
    let p = paths();
    echo_bridge(&p.bridge);
    let listener = UnixListener::bind(&p.user).unwrap();
    let mut child = Command::new(PROGRAM)
        .args(["--socket".as_ref(), p.bridge.as_os_str()])
        .stdin(Stdio::from(OwnedFd::from(listener)))
        .spawn()
        .unwrap();
    let answer = ask(&p.user, b"hello");
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(answer, b"hello");
}

#[test]
fn program_listens_where_told() {
    let p = paths();
    echo_bridge(&p.bridge);
    let mut child = Command::new(PROGRAM)
        .args([
            "--socket".as_ref(),
            p.bridge.as_os_str(),
            "--listen".as_ref(),
            p.user.as_os_str(),
        ])
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let mut answer = None;
    for _ in 0..250 {
        if let Ok(got) = try_ask(&p.user, b"hello") {
            answer = Some(got);
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(answer.as_deref(), Some(&b"hello"[..]));
}

#[test]
fn program_refuses_other_stdin() {
    let output = Command::new(PROGRAM).stdin(Stdio::null()).output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("isn't a listening Unix socket"), "{stderr}");
}
