// SPDX-License-Identifier: GPL-2.0-only

//! Client connections: non-blocking framing in both directions, the hello
//! and partial-frame timeouts, and the per-connection replay memory. What
//! each method does lives in `methods.rs`.

use hfx_proto::framing::{FrameReader, encode_frame};
use hfx_proto::ids::RequestId;
use hfx_proto::messages::{Hello, Transact, TransactResult, Welcome};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;

pub const MAX_CONNECTIONS: usize = 32;
pub const HELLO_TIMEOUT_MS: u64 = 10_000;
pub const FRAME_TIMEOUT_MS: u64 = 10_000;
pub const REPLAY_MEMORY: usize = 64;
/// A client that stops reading its replies gets cut off here.
const MAX_OUTPUT: usize = 4 << 20;

pub struct Session {
    pub hello: Hello,
    pub welcome: Welcome,
}

pub enum Parked {
    Wait {
        id: RequestId,
        state_after: Option<u64>,
        routes_after: Option<u64>,
        deadline: u64,
    },
    Capture {
        id: RequestId,
        after: u64,
        max: usize,
        deadline: u64,
    },
    /// A transact or bench waiting for the receiver.
    Job,
}

impl Parked {
    #[must_use]
    pub fn deadline(&self) -> Option<u64> {
        match self {
            Self::Wait { deadline, .. } | Self::Capture { deadline, .. } => Some(*deadline),
            Self::Job => None,
        }
    }
}

pub struct Conn {
    pub id: u64,
    pub stream: UnixStream,
    pub session: Option<Session>,
    pub parked: Option<Parked>,
    pub replays: VecDeque<(RequestId, Transact, TransactResult)>,
    pub dead: bool,
    reader: FrameReader,
    out: Vec<u8>,
    opened: u64,
    partial_since: Option<u64>,
    writing: bool,
}

impl Conn {
    #[must_use]
    pub fn new(id: u64, stream: UnixStream, now: u64) -> Self {
        Self {
            id,
            stream,
            session: None,
            parked: None,
            replays: VecDeque::new(),
            dead: false,
            reader: FrameReader::new(),
            out: Vec::new(),
            opened: now,
            partial_since: None,
            writing: false,
        }
    }

    /// Reads everything available. Marks the connection dead on EOF,
    /// errors and oversized input.
    pub fn read(&mut self, now: u64) {
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let room = self.reader.room().min(buffer.len());
            if room == 0 {
                // Over the bound while a request is parked: a misbehaving
                // client, as it may not send more than one request at a time.
                self.dead = true;
                return;
            }
            match self.stream.read(&mut buffer[..room]) {
                Ok(0) => {
                    self.dead = true;
                    return;
                }
                Ok(n) => {
                    if self.reader.push(&buffer[..n]).is_err() {
                        self.dead = true;
                        return;
                    }
                    if self.partial_since.is_none() {
                        self.partial_since = Some(now);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(_) => {
                    self.dead = true;
                    return;
                }
            }
        }
    }

    /// The next request payload, unless one is still being answered.
    /// A bad length prefix kills the connection.
    pub fn next_request(&mut self, now: u64) -> Option<Vec<u8>> {
        if self.parked.is_some() || self.dead {
            return None;
        }
        match self.reader.next_frame() {
            Ok(Some(payload)) => {
                self.partial_since = self.reader.finish().is_err().then_some(now);
                Some(payload)
            }
            Ok(None) => None,
            Err(_) => {
                self.dead = true;
                None
            }
        }
    }

    pub fn send(&mut self, payload: &[u8]) {
        match encode_frame(payload) {
            Ok(frame) => self.out.extend_from_slice(&frame),
            Err(_) => self.dead = true,
        }
        self.flush();
    }

    /// Writes what it can. True when output is still pending.
    pub fn flush(&mut self) -> bool {
        while !self.out.is_empty() {
            match self.stream.write(&self.out) {
                Ok(0) => {
                    self.dead = true;
                    break;
                }
                Ok(n) => {
                    self.out.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.dead = true;
                    break;
                }
            }
        }
        if self.out.len() > MAX_OUTPUT {
            self.dead = true;
        }
        !self.out.is_empty()
    }

    /// Whether write interest must change, after updating the remembered state.
    pub fn write_interest_changed(&mut self) -> Option<bool> {
        let want = !self.out.is_empty();
        (want != self.writing).then(|| {
            self.writing = want;
            want
        })
    }

    /// Hello and stalled-frame timeouts.
    #[must_use]
    pub fn deadline(&self) -> Option<u64> {
        let hello = self
            .session
            .is_none()
            .then_some(self.opened + HELLO_TIMEOUT_MS);
        let frame = self.partial_since.map(|t| t + FRAME_TIMEOUT_MS);
        hello.into_iter().chain(frame).min()
    }

    pub fn remember(&mut self, id: RequestId, params: Transact, result: TransactResult) {
        if self.replays.len() == REPLAY_MEMORY {
            self.replays.pop_front();
        }
        self.replays.push_back((id, params, result));
    }
}
