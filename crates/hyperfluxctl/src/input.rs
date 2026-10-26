// SPDX-License-Identifier: GPL-2.0-only

//! The receiver's pointer input, read the way any program that watches
//! /dev/input does: read-only, non-blocking and never with EVIOCGRAB (the
//! desktop keeps getting every event). The one setting this reader
//! changes is its own: EVIOCSCLOCKID stamps its copy of the events with
//! CLOCK_BOOTTIME, the clock the bridge stamps its capture events with.

use crate::system::System;
use crate::{EXIT_NO_INPUT, Failure};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;
use std::ffi::c_int;
use std::sync::atomic::{AtomicBool, Ordering};

const EV_SYN: u16 = 0;
const EV_REL: u16 = 2;
const SYN_REPORT: u16 = 0;
const SYN_DROPPED: u16 = 3;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const LONG: usize = size_of::<usize>();
/// struct input_event: a timeval of two longs, then type, code and value.
const EVENT_LEN: usize = 2 * LONG + 8;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Node {
    pub path: String,
    /// The receiver's USB interface, from the device's `phys`.
    pub interface: Option<u8>,
}

/// What the device sent between two SYN_REPORTs, when it moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Motion {
    pub time_us: u64,
    pub dx: i32,
    pub dy: i32,
}

#[derive(Debug, Default)]
pub struct Recording {
    /// In time order, from all nodes.
    pub motion: Vec<Motion>,
    /// When the kernel dropped events because this reader fell behind.
    pub dropped: Vec<u64>,
    /// A node went away during the run.
    pub lost_node: bool,
}

/// The input devices of any `1532:00cf` receiver that report REL_X and
/// REL_Y, from sysfs.
#[must_use]
pub fn pointer_nodes(system: &System) -> Vec<Node> {
    let mut nodes = Vec::new();
    for name in system.list("/sys/class/input", 1024).unwrap_or_default() {
        let attr = |a: &str| system.read(&format!("/sys/class/input/{name}/device/{a}"));
        let receiver = attr("id/vendor").as_deref() == Some("1532")
            && attr("id/product").as_deref() == Some("00cf");
        // The bitmap is hex words, most significant first. REL_X and REL_Y
        // are its two lowest bits.
        let rel = attr("capabilities/rel").and_then(|r| {
            let last = r.split_whitespace().last()?;
            u64::from_str_radix(last, 16).ok()
        });
        if name.starts_with("event") && receiver && rel.is_some_and(|bits| bits & 0b11 == 0b11) {
            let phys = attr("phys").unwrap_or_default();
            let interface = phys.rsplit_once("/input").and_then(|(_, n)| n.parse().ok());
            let path = format!("/dev/input/{name}");
            nodes.push(Node { path, interface });
        }
    }
    nodes.sort_by_key(|n| (n.interface, n.path.clone()));
    nodes
}

/// EVIOCSCLOCKID, `_IOW('E', 0xa0, int)`: this reader's timestamps on
/// CLOCK_BOOTTIME. Other readers keep their own clocks.
#[allow(unsafe_code)]
fn boottime_stamps(fd: BorrowedFd<'_>) -> rustix::io::Result<()> {
    use rustix::ioctl::{Opcode, Setter, ioctl, opcode};
    const EVIOCSCLOCKID: Opcode = opcode::write::<c_int>(b'E', 0xa0);
    const CLOCK_BOOTTIME: c_int = 7;
    // SAFETY: EVIOCSCLOCKID reads one int through the pointer Setter passes.
    unsafe { ioctl(fd, Setter::<EVIOCSCLOCKID, c_int>::new(CLOCK_BOOTTIME)) }
}

/// Opens a node to read, never to grab.
///
/// # Errors
/// Exit code 3 and the fix when this user may not read it, otherwise 1.
pub fn open(system: &System, node: &Node) -> Result<OwnedFd, Failure> {
    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = rustix::fs::open(system.path(&node.path), flags, Mode::empty());
    let fd = fd.map_err(|e| match e {
        Errno::ACCESS | Errno::PERM => Failure {
            exit: EXIT_NO_INPUT,
            ..Failure::new(format!(
                "can't read {}: permission denied. Reading the pointer needs the input group: \
                 run sudo usermod -aG input \"$USER\", then log out and back in. Members of that \
                 group can read every keyboard and mouse, so for a one-off check \
                 sudo hyperfluxctl stall-check may suit you better",
                node.path
            ))
        },
        e => Failure::new(format!("can't open {}: {e}", node.path)),
    })?;
    boottime_stamps(fd.as_fd())
        .map_err(|e| Failure::new(format!("{} doesn't take a clock setting: {e}", node.path)))?;
    Ok(fd)
}

/// Movement since the last SYN_REPORT, or None after a SYN_DROPPED: the
/// rest of that packet is incomplete and gets thrown away.
pub type Pending = Option<(i32, i32)>;

/// Adds the motion reports in a run of raw `input_event`s. A packet may
/// start in an earlier run.
pub fn parse(bytes: &[u8], pending: &mut Pending, recording: &mut Recording) {
    for event in bytes.chunks_exact(EVENT_LEN) {
        let long = |at: usize| {
            let word: [u8; LONG] = event[at..at + LONG].try_into().expect("one long");
            usize::from_ne_bytes(word) as u64
        };
        let short = |at: usize| u16::from_ne_bytes([event[at], event[at + 1]]);
        let time_us = long(0) * 1_000_000 + long(LONG);
        let value = i32::from_ne_bytes(event[2 * LONG + 4..].try_into().expect("an int"));
        match (short(2 * LONG), short(2 * LONG + 2), pending.as_mut()) {
            (EV_REL, REL_X, Some(moved)) => moved.0 += value,
            (EV_REL, REL_Y, Some(moved)) => moved.1 += value,
            (EV_SYN, SYN_REPORT, _) => {
                if let Some((dx, dy)) = pending.replace((0, 0))
                    && (dx, dy) != (0, 0)
                {
                    recording.motion.push(Motion { time_us, dx, dy });
                }
            }
            (EV_SYN, SYN_DROPPED, _) => {
                recording.dropped.push(time_us);
                *pending = None;
            }
            _ => {}
        }
    }
}

/// Reads the nodes until `stop` is set or all of them are gone.
#[must_use]
pub fn record(fds: Vec<OwnedFd>, stop: &AtomicBool) -> Recording {
    let mut recording = Recording::default();
    let mut nodes: Vec<(OwnedFd, Pending)> = fds.into_iter().map(|fd| (fd, Some((0, 0)))).collect();
    let mut buffer = [0_u8; EVENT_LEN * 64];
    let tick = Timespec {
        tv_sec: 0,
        tv_nsec: 100_000_000,
    };
    while !stop.load(Ordering::Relaxed) && !nodes.is_empty() {
        let mut polled: Vec<PollFd<'_>> = nodes
            .iter()
            .map(|(fd, _)| PollFd::new(fd, PollFlags::IN))
            .collect();
        let _ = poll(&mut polled, Some(&tick));
        drop(polled);
        nodes.retain_mut(|(fd, pending)| {
            loop {
                match rustix::io::read(&*fd, &mut buffer) {
                    Ok(0) | Err(Errno::AGAIN) => break true,
                    Ok(n) => parse(&buffer[..n], pending, &mut recording),
                    Err(Errno::INTR) => {}
                    Err(_) => {
                        recording.lost_node = true;
                        break false;
                    }
                }
            }
        });
    }
    recording.motion.sort_by_key(|m| m.time_us);
    recording
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(time_us: u64, kind: u16, code: u16, value: i32) -> Vec<u8> {
        let long = |v: u64| usize::try_from(v).unwrap().to_ne_bytes();
        let mut bytes = long(time_us / 1_000_000).to_vec();
        bytes.extend(long(time_us % 1_000_000));
        bytes.extend(kind.to_ne_bytes());
        bytes.extend(code.to_ne_bytes());
        bytes.extend(value.to_ne_bytes());
        bytes
    }

    #[test]
    fn motion_reports_from_events() {
        let mut stream = Vec::new();
        for (t, kind, code, value) in [
            (1_000_000, EV_REL, REL_X, 3),
            (1_000_000, EV_REL, REL_Y, -2),
            (1_000_000, EV_SYN, SYN_REPORT, 0),
            // A wheel turn alone isn't movement.
            (1_001_000, EV_REL, 8, 1),
            (1_001_000, EV_SYN, SYN_REPORT, 0),
            // What follows a drop up to the next report is incomplete.
            (1_002_000, EV_REL, REL_X, 5),
            (1_002_500, EV_SYN, SYN_DROPPED, 0),
            (1_003_000, EV_REL, REL_Y, 4),
            (1_003_000, EV_SYN, SYN_REPORT, 0),
            (1_004_000, EV_REL, REL_X, 2),
        ] {
            stream.extend(event(t, kind, code, value));
        }
        let (mut recording, mut pending) = (Recording::default(), Some((0, 0)));
        // Split mid-packet, as two reads might.
        let (first, rest) = stream.split_at(EVENT_LEN * 7);
        parse(first, &mut pending, &mut recording);
        parse(rest, &mut pending, &mut recording);
        let end = event(1_004_000, EV_SYN, SYN_REPORT, 0);
        parse(&end, &mut pending, &mut recording);
        let motion = |time_us, dx, dy| Motion { time_us, dx, dy };
        let reports = [motion(1_000_000, 3, -2), motion(1_004_000, 2, 0)];
        assert_eq!(
            (recording.motion, recording.dropped),
            (reports.to_vec(), vec![1_002_500])
        );
    }
}
