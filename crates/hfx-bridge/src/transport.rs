// SPDX-License-Identifier: GPL-2.0-only

//! How the bridge reaches receivers. A transport finds receivers and opens
//! them for exchanges. The one transport is hidraw (`hidraw.rs`), on the
//! real system or on the simulator. These traits keep the event loop apart
//! from it, so tests can stand in for a receiver. The receiver's input
//! comes from the device keeper (`keeper.rs`) as the same records.
//!
//! The receiver's rules (lane selectors, reply matching, the read loop,
//! passive report layout) live in `wire.rs` and are applied by the bridge.
//! A transport moves bytes and no more: the bridge already puts the lane
//! selector in byte 1 and checks the replies it gets back.

use hfx_proto::messages::Lane;
use hfx_proto::report::Report;
use rustix::fd::BorrowedFd;
use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The receiver's state as a whole.
    State {
        suspended: bool,
        leaving: bool,
        ready: bool,
    },
    /// The child on a lane: `arg0` is the pointer's contact byte or the
    /// keyboard's route byte, `arg1` the keyboard's byte after it.
    Identity {
        lane: Lane,
        product_id: u16,
        arg0: u8,
        arg1: u8,
    },
    /// A passive status report's four bytes, undecoded.
    Status {
        lane: Lane,
        level: u8,
        power: u8,
        charge: u8,
        extra: u8,
    },
    /// Input happened on a lane.
    Activity { lane: Lane },
    /// A vendor report as it came, for `capture`: its first bytes and
    /// length, and its lane where the report id says.
    Raw {
        lane: Option<Lane>,
        bytes: [u8; 16],
        len: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Receiver,
    Passive,
    Inventory,
    /// The receiver's answer to the bridge's paired-slots query.
    Slots,
    Input,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    /// CLOCK_BOOTTIME of the observation.
    pub time_ms: u64,
    /// A restatement of something learnt while opening, not a new
    /// observation.
    pub replayed: bool,
    pub source: Source,
    pub event: Event,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    /// Counts receiver connections: a replug is a new generation.
    pub generation: u64,
    pub vendor_id: u16,
    pub product_id: u16,
    pub suspended: bool,
    /// The pairing inventory as read when the receiver was opened.
    pub inventory: crate::wire::InventoryRead,
}

/// The receiver went away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gone;

#[derive(Debug)]
pub enum OpenError {
    /// Not allowed yet: udev sets permissions a moment after a node appears.
    Access,
    /// The receiver is going away.
    Gone,
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotSent {
    /// The deadline passed before the request could go out.
    Deadline,
    /// The receiver is gone or can't be written to.
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uncertain {
    /// Sent, but no matching reply came.
    NoReply,
    /// Sent, then a transfer failed.
    Transfer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    NotSent(NotSent),
    MayHaveBeenSent(Uncertain),
}

#[derive(Clone, Copy, Debug)]
pub struct Exchanged {
    pub result: Result<Report, Failure>,
    /// Read attempts made, 0 when nothing was sent.
    pub gets: u8,
    /// Of those, reads after a mismatched reply, and after a busy one.
    pub rereads: [u8; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// Budget for getting the request out and its reply back, 1..=1000.
    pub timeout_ms: u32,
    /// Wait before the first read: the device's learned one, or the one
    /// bench measures. 0 falls back to the 2.x module's 8 ms.
    pub first_get_delay_us: u16,
    /// Wait before reading again after a reply that doesn't match: 0 for
    /// the usual 1 ms. Bench is the one caller that asks for another.
    pub get_interval_us: u16,
    /// Read on after a busy answer, until another answer or the time the
    /// 2.x module's five took (ARCHITECTURE.md, section 10).
    pub reread_busy: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeChange {
    Appeared(String),
    /// Permissions changed. An open that was refused is worth retrying.
    Changed(String),
    Gone(String),
}

/// Finds receivers.
pub trait Transport {
    /// Which one this is, for the snapshot.
    fn kind(&self) -> hfx_proto::messages::Transport;
    /// Names of the receivers present now.
    ///
    /// # Errors
    /// Fails when the transport can't look.
    fn scan(&mut self) -> io::Result<Vec<String>>;
    /// Readable when [`Transport::changes`] has something.
    fn watch_fd(&self) -> Option<BorrowedFd<'_>>;
    fn changes(&mut self) -> Vec<NodeChange>;
    /// Opens a receiver, with `write` asking for the right to send.
    ///
    /// # Errors
    /// See [`OpenError`].
    fn open(&mut self, name: &str, write: bool) -> Result<Opened, OpenError>;
    /// The receiver's USB device in sysfs: what makes its id stable.
    fn usb_path(&self, name: &str) -> Option<String>;
}

/// An open receiver: what opening it found, and for one opened to write the
/// handle for its worker thread.
pub struct Opened {
    pub info: Info,
    pub control: Option<Box<dyn Control>>,
}

/// Exchanges, run on the receiver's worker thread because they block.
pub trait Control: Send {
    /// One request, one reply. Byte 1 of `request` is already the lane
    /// selector, and the reply comes back as the receiver sent it.
    fn exchange(&mut self, lane: Lane, request: &Report, timing: Timing) -> Exchanged;
}
