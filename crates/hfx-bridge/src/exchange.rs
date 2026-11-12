// SPDX-License-Identifier: GPL-2.0-only

//! One worker thread per writable receiver runs the blocking ioctls, so the
//! event loop keeps answering while an exchange waits for the receiver. The
//! queue stays on the loop's side: the worker never holds more than one
//! job, which keeps the receiver to one exchange at a time and lets a
//! job whose deadline passes in the queue fail as "not sent".

use crate::state::lane_index;
use crate::transport::{Control, Failure, NotSent, Timing, Uncertain};
use crate::wire::{EXCHANGE_TIMEOUT_MAX_MS, GAP_AWAKE, GAP_TIGHT, Gap, same_arguments};
use hfx_proto::messages::Lane;
use hfx_proto::report::{self, Report};
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
        /// The receiver's gap between the lanes, when this exchange made it grow.
        gap_us: Option<u16>,
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
                let mut last: Option<Last> = None;
                // The lane of the last exchange that went out, and when it ended.
                let mut ended = None;
                // The gap between the lanes, and when each lane's device last
                // answered (asleep doesn't count).
                let mut gap = Gap::default();
                let mut heard: [Option<Instant>; 2] = [None; 2];
                for (job, mut work) in rx {
                    let key = crate::wire::answer_key(work.lane, &work.request);
                    let fresh = tell_apart(last.as_ref(), key, &mut work);
                    let floor = if fresh { 0 } else { work.repeat_wait_us };
                    work.timing.first_get_delay_us = work.timing.first_get_delay_us.max(floor);
                    if !wait_for_lane(ended, work.lane, &gap, &mut work.timing) {
                        let queued = Queued {
                            job,
                            work,
                            deadline: 0,
                        };
                        notify.send(failed(queued, NotSent::Deadline));
                        continue;
                    }
                    let start = Instant::now();
                    let keyboard = heard[lane_index(Lane::Keyboard)];
                    let tight = at_gap_end(ended, work.lane, &gap, keyboard, start);
                    let (lane, request, unrouted) = (work.lane, &work.request, work.unrouted);
                    let out =
                        crate::wire::exchange(&mut *control, lane, request, work.timing, unrouted);
                    let now = Instant::now();
                    ended = Some((lane, now));
                    let lost = matches!(
                        out.result,
                        Err(Failure::MayHaveBeenSent(Uncertain::NoReply))
                    ) && out.rereads[1] == 0;
                    let grown = if tight { gap.record(lost) } else { None };
                    let gap_us = grown.and_then(|us| u16::try_from(us).ok());
                    if out
                        .result
                        .is_ok_and(|r| r[report::STATUS] != report::STATUS_TIMEOUT)
                    {
                        heard[lane_index(lane)] = Some(now);
                    }
                    last = out.result.ok().map(|answer| Last {
                        key,
                        request: work.request,
                        answer,
                    });
                    let outcome = Outcome::Exchange {
                        request: work.request,
                        result: out.result,
                        gets: out.gets,
                        rereads: out.rereads,
                        elapsed_us: u32::try_from(start.elapsed().as_micros()).unwrap_or(u32::MAX),
                        fresh,
                        gap_us,
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

/// The receiver's last answer, and the request it answered.
struct Last {
    key: (Lane, [u8; 4]),
    request: Report,
    answer: Report,
}

/// Whether an early read can't take the receiver's last answer, which it
/// gets back, for the answer to `work`. It can't when the key differs. Nor
/// can it when the last answer was a success with its write's arguments
/// and `work` is a write with other arguments: `work`'s answer won't have
/// the old ones, and `work` is set to read past a reply that does.
fn tell_apart(last: Option<&Last>, key: (Lane, [u8; 4]), work: &mut Work) -> bool {
    let Some(last) = last else {
        return false;
    };
    if last.key != key {
        return true;
    }
    let echoed = last.answer[report::STATUS] == report::STATUS_SUCCESS
        && !report::is_query(&last.request)
        && same_arguments(&last.request, &last.answer)
        && !same_arguments(&last.request, &work.request);
    work.timing.stale = echoed.then_some(last.answer);
    echoed
}

/// Waits until the receiver takes a request for `lane` after the exchange
/// that `ended` (`wire::Gap`), and takes the wait off the exchange's time.
/// False, without waiting, when that would leave none.
fn wait_for_lane(
    ended: Option<(Lane, Instant)>,
    lane: Lane,
    gap: &Gap,
    timing: &mut Timing,
) -> bool {
    let Some((before, at)) = ended else {
        return true;
    };
    let wait = gap.wait(before, lane).saturating_sub(at.elapsed());
    if wait.is_zero() {
        return true;
    }
    let ms = u32::try_from(wait.as_micros().div_ceil(1_000)).unwrap_or(u32::MAX);
    if ms >= timing.timeout_ms {
        return false;
    }
    thread::sleep(wait);
    timing.timeout_ms -= ms;
    true
}

/// Whether a request went out at the end of the gap after the other lane,
/// to a keyboard `heard` from a moment ago. Lost, it says the gap is too
/// short.
fn at_gap_end(
    ended: Option<(Lane, Instant)>,
    lane: Lane,
    gap: &Gap,
    heard: Option<Instant>,
    start: Instant,
) -> bool {
    let Some((before, at)) = ended else {
        return false;
    };
    let wait = gap.wait(before, lane);
    !wait.is_zero()
        && start.duration_since(at) < wait + GAP_TIGHT
        && heard.is_some_and(|t| start.duration_since(t) < GAP_AWAKE)
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
            gap_us: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Exchanged;
    use crate::wire::KEYBOARD_AFTER_POINTER_US;
    use std::sync::Mutex;
    use std::time::Duration;

    type Seen = Arc<Mutex<Vec<(Lane, Instant, Timing)>>>;

    /// A device that answers every request at once, a write with its own
    /// arguments, and a note of when each exchange started and its timing.
    struct Echo(Seen);

    impl Control for Echo {
        fn exchange(&mut self, lane: Lane, request: &Report, timing: Timing) -> Exchanged {
            self.0.lock().unwrap().push((lane, Instant::now(), timing));
            let mut answer = *request;
            answer[report::STATUS] = report::STATUS_SUCCESS;
            Exchanged {
                result: Ok(answer),
                gets: 1,
                rereads: [0; 2],
            }
        }
    }

    /// Runs the jobs one after another, each with this many ms to go.
    fn run(jobs: &[(Lane, Report, u64)]) -> (Vec<(Lane, Instant, Timing)>, Vec<Outcome>) {
        let seen = Seen::default();
        let done = Completions::new().unwrap();
        let mut worker = Worker::start(Box::new(Echo(seen.clone())), done.notify()).unwrap();
        let mut outcomes = Vec::new();
        for (job, &(lane, request, deadline)) in (0..).zip(jobs) {
            let timing = Timing {
                timeout_ms: 1,
                first_get_delay_us: 2_000,
                get_interval_us: 0,
                reread_busy: true,
                stale: None,
            };
            let work = Work {
                lane,
                request,
                timing,
                repeat_wait_us: 3_000,
                unrouted: false,
            };
            worker.push(Queued {
                job,
                work,
                deadline,
            });
            assert!(worker.pump(0).is_empty());
            let finished = loop {
                if let Some(d) = done.drain().pop() {
                    break d;
                }
                thread::sleep(Duration::from_micros(200));
            };
            outcomes.push(finished.outcome);
            worker.finished();
        }
        let seen = seen.lock().unwrap().clone();
        (seen, outcomes)
    }

    fn row(n: u8) -> Report {
        report::build(0xff, 0x0f, 0x03, &[0, 0, n, 0, 16])
    }

    #[test]
    fn rows_tell_apart() {
        let query = report::build(0xff, 0x0f, 0x84, &[1, 5, 0]);
        let kb = Lane::Keyboard;
        let jobs = [row(0), row(1), row(1), query, query].map(|r| (kb, r, 1_000));
        let (seen, _) = run(&jobs);
        let timing: Vec<_> = seen
            .iter()
            .map(|(_, _, t)| (t.first_get_delay_us, t.stale.is_some()))
            .collect();
        // Unknown last answer, other arguments, the same row again, another
        // command, the same query again.
        let expected = [
            (3_000, false),
            (2_000, true),
            (3_000, false),
            (2_000, false),
            (3_000, false),
        ];
        assert_eq!(timing, expected);
    }

    #[test]
    fn keyboard_waits_after_pointer() {
        let (kb, pointer) = (Lane::Keyboard, Lane::Pointer);
        let jobs = [
            (pointer, row(0), 1_000),
            (kb, row(0), 1_000),
            (pointer, row(1), 1_000),
        ];
        let (seen, _) = run(&jobs);
        let gap = |i: usize| seen[i + 1].1 - seen[i].1;
        assert!(gap(0) >= Duration::from_micros(KEYBOARD_AFTER_POINTER_US));
        assert!(gap(1) < Duration::from_micros(KEYBOARD_AFTER_POINTER_US));
        // The wait comes off the keyboard exchange's time.
        assert!(seen[1].2.timeout_ms < 1_000);
        // Too little time left for it: never sent.
        let jobs = [(pointer, row(0), 1_000), (kb, row(0), 3)];
        let (seen, outcomes) = run(&jobs);
        assert_eq!(seen.len(), 1);
        let late = Err(Failure::NotSent(NotSent::Deadline));
        assert!(matches!(outcomes[1], Outcome::Exchange { result, .. } if result == late));
    }
}
