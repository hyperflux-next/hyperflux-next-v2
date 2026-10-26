// SPDX-License-Identifier: GPL-2.0-only

//! One worker thread per writable receiver runs the blocking ioctls, so the
//! event loop keeps answering while an exchange waits for the receiver. The
//! queue stays on the loop's side: the worker never holds more than one
//! job, which keeps the receiver to one exchange at a time and lets a
//! job whose deadline passes in the queue fail as "not sent".

use crate::transport::{Control, Failure, NotSent, Timing};
use crate::wire::EXCHANGE_TIMEOUT_MAX_MS;
use hfx_proto::messages::Lane;
use hfx_proto::report::Report;
use rustix::event::{EventfdFlags, eventfd};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::io::Errno;
use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Instant;

pub struct Work {
    pub lane: Lane,
    /// As the caller sent it. The worker routes it.
    pub request: Report,
    pub timing: Timing,
    /// The least first wait when the last answer could pass for this one's.
    pub repeat_wait_us: u16,
    /// One of the bridge's receiver probes, sent as it is.
    pub unrouted: bool,
}

pub enum Outcome {
    Exchange {
        request: Report,
        result: Result<Report, Failure>,
        gets: u8,
        rereads: [u8; 2],
        elapsed_us: u32,
        /// The wait wasn't raised: the last answer couldn't pass for this one's.
        fresh: bool,
    },
    /// A teardown's write to an OpenRazer driver's unbind file.
    Unbind(Result<(), Errno>),
}

pub struct Done {
    pub job: u64,
    pub outcome: Outcome,
}

/// Lets other threads hand results to the loop and wake it.
#[derive(Clone)]
pub struct Notify {
    tx: Sender<Done>,
    wake: Arc<OwnedFd>,
}

impl Notify {
    pub fn send(&self, done: Done) {
        if self.tx.send(done).is_ok() {
            let _ = rustix::io::write(&*self.wake, &1_u64.to_ne_bytes());
        }
    }
}

pub struct Completions {
    rx: Receiver<Done>,
    notify: Notify,
}

impl Completions {
    /// # Errors
    /// Fails when the eventfd cannot be created.
    pub fn new() -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let wake = Arc::new(eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)?);
        Ok(Self {
            rx,
            notify: Notify { tx, wake },
        })
    }

    #[must_use]
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.notify.wake.as_fd()
    }

    #[must_use]
    pub fn notify(&self) -> Notify {
        self.notify.clone()
    }

    pub fn drain(&self) -> Vec<Done> {
        let mut counter = [0_u8; 8];
        let _ = rustix::io::read(&*self.notify.wake, &mut counter);
        self.rx.try_iter().collect()
    }
}

pub struct Queued {
    pub job: u64,
    pub work: Work,
    pub deadline: u64,
}

/// A receiver's side of its worker: the queue and the job in flight.
#[derive(Default)]
pub struct Worker {
    worker: Option<Sender<(u64, Work)>>,
    queue: VecDeque<Queued>,
    busy: bool,
}

impl Worker {
    /// # Errors
    /// Fails when the thread cannot be spawned.
    pub fn start(mut control: Box<dyn Control>, notify: Notify) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel::<(u64, Work)>();
        thread::Builder::new()
            .name("hfx-exchange".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                // What the receiver last answered, or None if that is unknown.
                let mut last = None;
                for (job, mut work) in rx {
                    // An early read gets the last answer back, and it can pass for this one's.
                    let key = crate::wire::answer_key(work.lane, &work.request);
                    let fresh = last.is_some_and(|l| l != key);
                    let floor = if fresh { 0 } else { work.repeat_wait_us };
                    work.timing.first_get_delay_us = work.timing.first_get_delay_us.max(floor);
                    let start = Instant::now();
                    let (lane, request, unrouted) = (work.lane, &work.request, work.unrouted);
                    let out =
                        crate::wire::exchange(&mut *control, lane, request, work.timing, unrouted);
                    last = out.result.is_ok().then_some(key);
                    let outcome = Outcome::Exchange {
                        request: work.request,
                        result: out.result,
                        gets: out.gets,
                        rereads: out.rereads,
                        elapsed_us: u32::try_from(start.elapsed().as_micros()).unwrap_or(u32::MAX),
                        fresh,
                    };
                    notify.send(Done { job, outcome });
                }
            })?;
        Ok(Self {
            worker: Some(tx),
            queue: VecDeque::new(),
            busy: false,
        })
    }

    #[must_use]
    pub fn running(&self) -> bool {
        self.worker.is_some()
    }

    #[must_use]
    pub fn next_deadline(&self) -> Option<u64> {
        if self.busy {
            return None;
        }
        self.queue.iter().map(|q| q.deadline).min()
    }

    pub fn push(&mut self, queued: Queued) {
        self.queue.push_back(queued);
    }

    /// The loop calls this when a job of this receiver finished.
    pub fn finished(&mut self) {
        self.busy = false;
    }

    /// Hands the next job to the worker. Jobs whose deadline passed while
    /// queued come back as ETIME results: provably never sent.
    pub fn pump(&mut self, now: u64) -> Vec<Done> {
        let mut expired = Vec::new();
        while !self.busy {
            let Some(mut next) = self.queue.pop_front() else {
                break;
            };
            let left = next.deadline.saturating_sub(now);
            if left == 0 {
                expired.push(failed(next, NotSent::Deadline));
                continue;
            }
            next.work.timing.timeout_ms = u32::try_from(left)
                .unwrap_or(u32::MAX)
                .clamp(1, EXCHANGE_TIMEOUT_MAX_MS);
            let Some(worker) = &self.worker else {
                expired.push(failed(next, NotSent::Unavailable));
                continue;
            };
            match worker.send((next.job, next.work)) {
                Ok(()) => self.busy = true,
                Err(mpsc::SendError((job, work))) => expired.push(failed(
                    Queued {
                        job,
                        work,
                        deadline: 0,
                    },
                    NotSent::Unavailable,
                )),
            }
        }
        expired
    }

    /// Stops the worker (it finishes its current job first) and returns
    /// everything still queued as "not sent".
    pub fn close(&mut self) -> Vec<Done> {
        self.worker = None;
        self.queue
            .drain(..)
            .map(|q| failed(q, NotSent::Unavailable))
            .collect()
    }
}

fn failed(queued: Queued, why: NotSent) -> Done {
    Done {
        job: queued.job,
        outcome: Outcome::Exchange {
            request: queued.work.request,
            result: Err(Failure::NotSent(why)),
            gets: 0,
            rereads: [0; 2],
            elapsed_us: 0,
            fresh: false,
        },
    }
}
