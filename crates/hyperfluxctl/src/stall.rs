// SPDX-License-Identifier: GPL-2.0-only

//! `hyperfluxctl stall-check`: does the cursor freeze while the mouse
//! moves, and was anything talking to a device at that moment? The
//! receiver relays feature exchanges to the mouse over the radio link that
//! carries its movement. A query might hold that movement up. This listens
//! to the receiver's pointer input (input.rs), finds gaps in the middle of
//! movement and puts them next to the bridge's capture stream and the
//! kernel log. With `--ab` it sends a read-only battery query itself in
//! the second half and compares the two halves.
//!
//! An overlap means two things happened at the same time, not that one
//! caused the other, and the output says so.

use crate::input::{self, Motion, Node, Recording};
use crate::system::System;
use crate::{Context, EXIT_ATTENTION, EXIT_FAILURE, EXIT_OK, Failure, args, bridge, print_json};
use hfx_proto::client::{BenchReads, Client};
use hfx_proto::clock::boottime_ms;
use hfx_proto::ids::{BenchIterations, EventLimit, FirstGetDelayUs, LeaseDurationMs};
use hfx_proto::ids::{RouteToken, Sequence, WaitTimeoutMs};
use hfx_proto::messages::{BenchOutcome, BenchQuery, Capture, CaptureEvent, CaptureKind};
use hfx_proto::messages::{CaptureLane, CaptureSource, Lane, LeaseOutcome};
use hfx_proto::negotiation::{FEATURE_BENCH, FEATURE_CAPTURE, FEATURE_NATIVE_ROUTES};
use hfx_proto::report;
use serde::Serialize;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// The rules, as hyperfluxctl(1) describes them.

/// A gap counts as a stall only inside movement: at least CONTEXT_REPORTS
/// motion reports within CONTEXT_MS before it and as many after it...
pub const CONTEXT_MS: u64 = 100;
pub const CONTEXT_REPORTS: usize = 5;
/// ...covering similar distances, the longer at most this many times the
/// shorter. A hand that stops and starts again slows down first.
pub const SPEED_RATIO: f64 = 3.0;
/// Longer gaps are pauses: the hand stopped.
pub const PAUSE_MS: u64 = 1_000;
/// The bridge records a reply when its exchange ends, at most this long
/// after the SET: an 8 ms first wait, 40 ms more of reads, transfers.
pub const EXCHANGE_MS: u64 = 60;
/// journald stamps kernel lines a little after the kernel wrote them.
pub const KERNEL_SLACK_MS: u64 = 250;
/// Overlaps point at the traffic when there are at least two and at least
/// this many times as many as chance would give.
pub const LINK_FACTOR: f64 = 2.0;
/// Less movement than this, in the run or in either half, tells nothing.
pub const MIN_MOVING_MS: u64 = 5_000;
/// `--ab` sends its query this often in the second half.
pub const QUERY_EVERY_MS: u64 = 500;
/// The halves differ when a one-sided binomial test of how the stalls
/// split between them, weighted by each half's movement, is below this.
pub const AB_SIGNIFICANCE: f64 = 0.05;
/// A kernel line is shown when it has a word from each list.
const KERNEL_SUBJECTS: [&str; 4] = ["usb", "xhci", "hid", "pm:"];
const KERNEL_EVENTS: [&str; 5] = ["disconnect", "reset", "resum", "suspend", "error"];

/// A reply the bridge recorded: a query exchange that ended at `time_ms`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Traffic {
    pub time_ms: u64,
    pub lane: &'static str,
    /// Class and command, as in 07/80.
    pub command: String,
    /// `bridge`, `app`, or `stall-check` for the `--ab` queries.
    pub who: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KernelLine {
    pub time_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Stall {
    /// CLOCK_BOOTTIME of the last motion report before the gap.
    pub start_us: u64,
    pub duration_us: u64,
    pub traffic: Vec<Traffic>,
    pub kernel: Vec<KernelLine>,
}

/// Inter-report intervals while moving: pauses and gaps that aren't
/// stalls are left out, stalls are in.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Movement {
    pub moving_ms: u64,
    pub reports: usize,
    pub rate_hz: f64,
    pub p50_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Queries {
    pub sent: usize,
    pub answered: usize,
    /// Skipped because another program held the mouse.
    pub in_use: usize,
    pub failed: usize,
    /// When each query was under way, CLOCK_BOOTTIME ms.
    #[serde(skip)]
    pub windows: Vec<(u64, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Half {
    pub stalls: usize,
    pub moving_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Ab {
    pub passive: Half,
    pub queried: Half,
    pub queries: Queries,
    pub difference: &'static str,
}

/// What a run collected besides the pointer's own reports.
#[derive(Clone, Debug, Default)]
pub struct Inputs {
    pub start_ms: u64,
    pub end_ms: u64,
    pub threshold_ms: u32,
    /// None when the bridge could not be reached.
    pub traffic: Option<Vec<Traffic>>,
    /// None when the kernel log could not be read.
    pub kernel: Option<Vec<KernelLine>>,
    /// With `--ab`: when the queries began, and how they went.
    pub ab: Option<(u64, Queries)>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report {
    pub threshold_ms: u32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub nodes: Vec<Node>,
    pub movement: Movement,
    pub stalls: Vec<Stall>,
    pub dropped_events: usize,
    pub bridge: bool,
    pub replies: usize,
    /// Share of the run with an exchange possibly under way.
    pub traffic_share: f64,
    /// How many stalls would overlap traffic by chance.
    pub expected_overlaps: f64,
    pub kernel: Option<Vec<KernelLine>>,
    pub ab: Option<Ab>,
    pub verdict: String,
    #[serde(skip)]
    pub exit: i32,
}

/// (time, length) pairs in microseconds.
type Spans = Vec<(u64, u64)>;

fn distance(part: &[Motion]) -> f64 {
    part.iter()
        .map(|m| f64::from(m.dx).hypot(f64::from(m.dy)))
        .sum()
}

/// Whether movement surrounds the gap that ends at report `i`.
fn surrounded(motion: &[Motion], i: usize) -> bool {
    let window = CONTEXT_MS * 1000;
    let (before, after) = (motion[i - 1].time_us, motion[i].time_us);
    let from = motion.partition_point(|m| m.time_us < before.saturating_sub(window));
    let to = motion.partition_point(|m| m.time_us <= after + window);
    // The first report after the gap may carry movement that piled up
    // during it. It does not count towards the speed.
    let (ahead, behind) = (&motion[from..i], &motion[i + 1..to]);
    let (a, b) = (distance(ahead), distance(behind));
    let enough = ahead.len().min(behind.len()) >= CONTEXT_REPORTS;
    enough && a.max(b) <= SPEED_RATIO * a.min(b)
}

/// The intervals while moving as (end, length), and the stalls among them
/// as (start, length).
fn intervals(recording: &Recording, threshold_us: u64) -> (Spans, Spans) {
    let motion = &recording.motion;
    let (mut moving, mut stalls) = (Vec::new(), Vec::new());
    for i in 1..motion.len() {
        let (a, b) = (motion[i - 1].time_us, motion[i].time_us);
        let gap = b - a;
        // Dropped events leave a gap that the device never made.
        let stall = gap > threshold_us
            && gap <= PAUSE_MS * 1000
            && !recording.dropped.iter().any(|d| (a..=b).contains(d))
            && surrounded(motion, i);
        if gap <= threshold_us || stall {
            moving.push((b, gap));
        }
        if stall {
            stalls.push((a, gap));
        }
    }
    (moving, stalls)
}

fn movement(moving: &[(u64, u64)]) -> Movement {
    let mut gaps: Vec<u64> = moving.iter().map(|m| m.1).collect();
    gaps.sort_unstable();
    let total: u64 = gaps.iter().sum();
    // Nearest rank, as bench does it.
    let rank = |share: f64| {
        let index = (share * gaps.len() as f64).ceil() as usize;
        gaps.get(index.saturating_sub(1)).copied().unwrap_or(0)
    };
    Movement {
        moving_ms: total / 1000,
        reports: gaps.len(),
        rate_hz: gaps.len() as f64 * 1e6 / total.max(1) as f64,
        p50_us: rank(0.5),
        p99_us: rank(0.99),
        max_us: gaps.last().copied().unwrap_or(0),
    }
}

/// How much of `from..to` lies within `reach` before one of `times`
/// (sorted).
fn covered(times: &[u64], reach: u64, from: u64, to: u64) -> u64 {
    let (mut total, mut until) = (0, from);
    for &t in times {
        let (a, b) = (t.saturating_sub(reach).max(until), t.min(to));
        total += b.saturating_sub(a);
        until = until.max(b);
    }
    total
}

/// P(X >= k) for X ~ Binomial(n, p), 0 < p < 1.
fn at_least(k: usize, n: usize, p: f64) -> f64 {
    let mut term = (1.0 - p).powi(i32::try_from(n).unwrap_or(i32::MAX));
    let mut sum = 0.0;
    for i in 0..=n {
        if i >= k {
            sum += term;
        }
        term *= (n - i) as f64 / (i + 1) as f64 * p / (1.0 - p);
    }
    sum
}

fn ab(found: &[(u64, u64)], moving: &[(u64, u64)], middle_ms: u64, queries: Queries) -> Ab {
    let half = |queried: bool| {
        let mine = |end: u64| (end >= middle_ms * 1000) == queried;
        let stalls = found.iter().filter(|s| mine(s.0 + s.1)).count();
        let moving_us: u64 = moving.iter().filter(|m| mine(m.0)).map(|m| m.1).sum();
        let moving_ms = moving_us / 1000;
        Half { stalls, moving_ms }
    };
    let (passive, queried) = (half(false), half(true));
    let n = passive.stalls + queried.stalls;
    let p = queried.moving_ms as f64 / (passive.moving_ms + queried.moving_ms).max(1) as f64;
    let difference = if passive.moving_ms.min(queried.moving_ms) < MIN_MOVING_MS {
        "One half had too little movement to compare them."
    } else if n == 0 {
        "No stalls in either half."
    } else if at_least(queried.stalls, n, p) < AB_SIGNIFICANCE {
        "More stalls with the queries running: they can delay movement."
    } else if at_least(passive.stalls, n, 1.0 - p) < AB_SIGNIFICANCE {
        "Fewer stalls with the queries running, so they don't explain the stalls."
    } else {
        "No clear difference between the halves."
    };
    Ab {
        passive,
        queried,
        queries,
        difference,
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn verdict(report: &Report) -> (String, i32) {
    let moving = report.movement.moving_ms;
    let (threshold, expected) = (report.threshold_ms, report.expected_overlaps);
    if moving < MIN_MOVING_MS {
        let seconds = moving as f64 / 1000.0;
        let text = format!(
            "Too little movement to tell ({seconds:.1} s). Keep the mouse moving in circles \
             for the whole run."
        );
        return (text, EXIT_FAILURE);
    }
    if report.stalls.is_empty() {
        let seconds = (moving + 500) / 1000;
        return (
            format!("No stalls over {threshold} ms in {seconds} s of movement."),
            EXIT_OK,
        );
    }
    let stalls = plural(report.stalls.len(), "stall", "stalls");
    let during = report.stalls.iter().filter(|s| !s.traffic.is_empty());
    let during = during.count();
    let text = if !report.bridge {
        format!("{stalls}; the bridge couldn't be reached, so they weren't compared with traffic.")
    } else if during == 0 {
        format!("{stalls}, none overlapping HyperFlux traffic: radio or receiver side.")
    } else if during >= 2 && during as f64 >= LINK_FACTOR * expected {
        format!("{stalls}, {during} during HyperFlux traffic: queries can delay movement.")
    } else {
        format!(
            "{stalls}, {during} during HyperFlux traffic, about what chance alone would give \
             ({expected:.1}): no clear link."
        )
    };
    (text, EXIT_ATTENTION)
}

/// Finds the stalls in a recording and what overlapped each of them.
#[must_use]
pub fn analyze(recording: &Recording, inputs: Inputs) -> Report {
    let (moving, found) = intervals(recording, u64::from(inputs.threshold_ms) * 1000);
    let traffic = inputs.traffic.as_deref().unwrap_or_default();
    let kernel = inputs.kernel.as_deref().unwrap_or_default();
    let near = |from: u64, to: u64| {
        let traffic = traffic
            .iter()
            .filter(|t| (from..=to + EXCHANGE_MS).contains(&t.time_ms));
        let slack = |k: &&KernelLine| k.time_ms + KERNEL_SLACK_MS >= from;
        let kernel = kernel
            .iter()
            .filter(|k| k.time_ms <= to + KERNEL_SLACK_MS)
            .filter(slack);
        (traffic.cloned().collect(), kernel.cloned().collect())
    };
    let stalls: Vec<Stall> = found
        .iter()
        .map(|&(start_us, duration_us)| {
            let (traffic, kernel) = near(start_us / 1000, (start_us + duration_us).div_ceil(1000));
            Stall {
                start_us,
                duration_us,
                traffic,
                kernel,
            }
        })
        .collect();
    let mut times: Vec<u64> = traffic.iter().map(|t| t.time_ms).collect();
    times.sort_unstable();
    let (from, to) = (inputs.start_ms, inputs.end_ms);
    let span = to.saturating_sub(from).max(1) as f64;
    let share = |reach| covered(&times, reach, from, to) as f64 / span;
    let reach = |s: &Stall| s.duration_us.div_ceil(1000) + EXCHANGE_MS;
    let mut report = Report {
        threshold_ms: inputs.threshold_ms,
        start_ms: from,
        end_ms: to,
        nodes: Vec::new(),
        movement: movement(&moving),
        expected_overlaps: stalls.iter().fold(0.0, |sum, s| sum + share(reach(s))),
        stalls,
        dropped_events: recording.dropped.len(),
        bridge: inputs.traffic.is_some(),
        replies: traffic.len(),
        traffic_share: share(EXCHANGE_MS),
        ab: inputs.ab.map(|(middle, q)| ab(&found, &moving, middle, q)),
        kernel: inputs.kernel,
        verdict: String::new(),
        exit: EXIT_OK,
    };
    (report.verdict, report.exit) = verdict(&report);
    report
}

/// The replies among the capture events. A bridge reply during one of
/// `ours` (the `--ab` queries) is this check's own.
#[must_use]
pub fn traffic(events: &[CaptureEvent], ours: &[(u64, u64)]) -> Vec<Traffic> {
    let replies = events.iter().filter(|e| e.kind == CaptureKind::Reply);
    let traffic = replies.map(|e| {
        let time_ms = e.time_ms.get();
        let mine = ours.iter().any(|&(a, b)| (a..=b).contains(&time_ms));
        let byte = |i: usize| e.data.as_slice().get(i).copied().unwrap_or(0);
        Traffic {
            time_ms,
            lane: match e.lane {
                CaptureLane::Pointer => "pointer",
                CaptureLane::Keyboard => "keyboard",
                CaptureLane::Receiver => "receiver",
            },
            command: format!("{:02x}/{:02x}", byte(report::CLASS), byte(report::COMMAND)),
            who: match e.source {
                CaptureSource::Bridge if mine => "stall-check",
                CaptureSource::Bridge => "bridge",
                _ => "app",
            },
        }
    });
    traffic.collect()
}

/// A `journalctl -o short-unix` line about USB, suspend or HID, on
/// CLOCK_BOOTTIME (`offset_ms` is the wall clock minus that).
#[must_use]
pub fn kernel_line(line: &str, offset_ms: i64) -> Option<KernelLine> {
    let (stamp, rest) = line.split_once(' ')?;
    let unix_ms = (stamp.parse::<f64>().ok()? * 1000.0) as i64;
    let text = rest.split_once(": ").map_or(rest, |(_, text)| text);
    let lower = text.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    let time_ms = u64::try_from(unix_ms - offset_ms).ok()?;
    let text = text.to_owned();
    (has(&KERNEL_SUBJECTS) && has(&KERNEL_EVENTS)).then_some(KernelLine { time_ms, text })
}

fn kernel_log(system: &System, offset_ms: i64, from: u64, to: u64) -> Option<Vec<KernelLine>> {
    let unix = |ms: u64| (i64::try_from(ms).unwrap_or(0) + offset_ms) / 1000;
    let journal = |range: &str| {
        let line = format!("journalctl -k -q --no-pager -o short-unix {range}");
        let out = system.run_any(&line, Duration::from_secs(5));
        out.filter(|o| o.success).map(|o| o.stdout)
    };
    let range = format!("--since @{} --until @{}", unix(from) - 1, unix(to) + 2);
    let text = journal(&range)?;
    // An empty log may mean this user can't read the kernel's lines.
    if text.is_empty() && journal("-n 1")?.is_empty() {
        return None;
    }
    let lines = text.lines().filter_map(|l| kernel_line(l, offset_ms));
    Some(lines.filter(|k| (from..=to).contains(&k.time_ms)).collect())
}

/// The text form: movement, traffic, each stall, the halves, the verdict.
#[must_use]
pub fn text(report: &Report) -> String {
    let mut out = String::new();
    let ms = |us: u64| us as f64 / 1000.0;
    // Seconds into the run.
    let at = |us: u64| us.saturating_sub(report.start_ms * 1000) as f64 / 1e6;
    let m = &report.movement;
    let (moving, rate) = (m.moving_ms as f64 / 1000.0, m.rate_hz);
    let (p50, p99, max) = (ms(m.p50_us), ms(m.p99_us), ms(m.max_us));
    let _ = writeln!(
        out,
        "Movement: {moving:.1} s, {rate:.0} reports a second; between reports p50 {p50:.2} ms, \
         p99 {p99:.2} ms, max {max:.1} ms."
    );
    if report.dropped_events > 0 {
        let times = plural(report.dropped_events, "time", "times");
        out += &format!("The kernel dropped events {times}: this check fell behind there.\n");
    }
    if report.bridge {
        let replies = plural(report.replies, "reply", "replies");
        let share = report.traffic_share * 100.0;
        out += &format!(
            "HyperFlux traffic: {replies}, an exchange under way {share:.0}% of the time.\n"
        );
    }
    match report.kernel.as_deref() {
        None => out += "Kernel log: not readable (journalctl -k), so USB resets weren't checked.\n",
        Some([]) => out += "Kernel log: no USB resets, disconnects or resumes.\n",
        Some(lines) => {
            for k in lines.iter().take(10) {
                out += &format!("Kernel log at {:.3} s: {}\n", at(k.time_ms * 1000), k.text);
            }
            if lines.len() > 10 {
                out += &format!("Kernel log: {} more lines like these.\n", lines.len() - 10);
            }
        }
    }
    let _ = writeln!(
        out,
        "Stalls over {} ms: {}",
        report.threshold_ms,
        report.stalls.len()
    );
    for s in &report.stalls {
        let mut what: Vec<String> = Vec::new();
        for t in &s.traffic {
            let line = format!("{} {} ({})", t.lane, t.command, t.who);
            if !what.contains(&line) {
                what.push(line);
            }
        }
        if what.is_empty() && report.bridge {
            what.push("no HyperFlux traffic".into());
        }
        what.extend(s.kernel.iter().map(|k| format!("kernel: {}", k.text)));
        let (start, length, what) = (at(s.start_us), ms(s.duration_us), what.join("; "));
        let _ = writeln!(out, "  at {start:.3} s for {length:.1} ms: {what}");
    }
    if let Some(ab) = &report.ab {
        let half = |h: &Half| {
            let stalls = plural(h.stalls, "stall", "stalls");
            let seconds = h.moving_ms as f64 / 1e3;
            let rate = h.stalls as f64 * 60.0 / seconds.max(f64::EPSILON);
            format!("{stalls} in {seconds:.1} s of movement ({rate:.1} a minute)")
        };
        let q = &ab.queries;
        let (sent, answered, in_use, failed) = (q.sent, q.answered, q.in_use, q.failed);
        let _ = writeln!(out, "Without queries: {}.", half(&ab.passive));
        let _ = writeln!(
            out,
            "With a battery query every {QUERY_EVERY_MS} ms: {}. Queries: {sent} sent, \
             {answered} answered, {in_use} skipped while another program held the mouse, \
             {failed} failed.",
            half(&ab.queried)
        );
        let _ = writeln!(out, "{}", ab.difference);
    }
    let _ = writeln!(out, "Verdict: {}", report.verdict);
    if !report.stalls.is_empty() {
        out += "An overlap means both happened at the same time, not that one caused the \
                other. Writes, such as lighting changes, aren't in the bridge's capture, so \
                they can't be matched.\n";
    }
    out
}

/// The mouse's route, for the `--ab` queries.
struct Pointer {
    token: RouteToken,
    delay: FirstGetDelayUs,
}

fn pointer(client: &mut Client) -> Result<Pointer, Failure> {
    let routes = client.list_routes();
    let routes = routes.map_err(|e| bridge::failed("list-routes", &e))?;
    let snapshot = client.snapshot();
    let snapshot = snapshot.map_err(|e| bridge::failed("snapshot", &e))?;
    // The wait the bridge uses for the mouse, as bench --defaults measures.
    let found = routes.routes.iter().find_map(|r| {
        let token = r.route_token.clone().filter(|_| r.lane == Lane::Pointer)?;
        Some((token, crate::bench::learned_wait(&snapshot, r)))
    });
    let Some((token, delay)) = found else {
        let why = "--ab needs the mouse online and controllable; hyperfluxctl devices says why \
                   it isn't";
        return Err(Failure::new(why));
    };
    let delay = FirstGetDelayUs::new(delay).map_err(|e| Failure::new(e.to_string()))?;
    Ok(Pointer { token, delay })
}

/// One read-only battery query through bench, with a lease of its own so
/// apps can have the mouse in between.
fn query(client: &mut Client, pointer: &Pointer, queries: &mut Queries) {
    let started = boottime_ms();
    let duration = LeaseDurationMs::new(1_000).expect("in range");
    match client
        .acquire_lease(&pointer.token, duration)
        .map(|l| (l.outcome, l.lease_id))
    {
        Ok((LeaseOutcome::Granted, Some(id))) => {
            let (once, token) = (BenchIterations::new(1).expect("in range"), &pointer.token);
            let reads = BenchReads::default();
            let result = client.bench(&id, token, BenchQuery::Battery, pointer.delay, once, reads);
            let _ = client.release_lease(&id);
            let outcome = result.map(|r| r.samples.first().map(|s| s.outcome));
            queries.sent += usize::from(outcome.is_ok());
            queries.answered += usize::from(matches!(outcome, Ok(Some(BenchOutcome::Ok))));
            queries.failed += usize::from(outcome.is_err());
        }
        Ok(_) => queries.in_use += 1,
        Err(_) => queries.failed += 1,
    }
    queries.windows.push((started, boottime_ms()));
}

/// The capture events since `after`, without waiting.
fn drain(
    client: &mut Client,
    after: &mut Option<Sequence>,
    events: &mut Vec<CaptureEvent>,
) -> Result<(), Failure> {
    let max_events = EventLimit::new(256).expect("in range");
    let timeout_ms = WaitTimeoutMs::new(0).expect("in range");
    loop {
        let params = Capture {
            after: *after,
            max_events,
            timeout_ms,
        };
        let result = client
            .capture(params)
            .map_err(|e| bridge::failed("capture", &e))?;
        let full = result.events.len() == usize::from(max_events.get());
        events.extend(result.events);
        *after = Some(result.next);
        if !full {
            return Ok(());
        }
    }
}

fn wall_minus_boottime_ms() -> i64 {
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let wall = i64::try_from(wall.as_millis()).unwrap_or(0);
    wall - i64::try_from(boottime_ms()).unwrap_or(0)
}

/// # Errors
/// No receiver input, input this user cannot read (exit 3), or, with
/// `--ab`, no bridge or no mouse to query.
pub fn run(
    ctx: &mut Context<'_>,
    socket: Option<&Path>,
    json: bool,
    options: &args::StallCheck,
    stop: &AtomicBool,
) -> Result<i32, Failure> {
    let nodes = input::pointer_nodes(ctx.system);
    if nodes.is_empty() {
        let why = "no pointer input from a HyperFlux V2 receiver (USB 1532:00cf) was found; \
                   is it plugged in?";
        return Err(Failure::new(why));
    }
    let fds = nodes.iter().map(|n| input::open(ctx.system, n));
    let fds = fds.collect::<Result<Vec<_>, _>>()?;
    let mut features = vec![FEATURE_CAPTURE];
    if options.ab {
        features.extend([FEATURE_NATIVE_ROUTES, FEATURE_BENCH]);
    }
    let (mut client, unreachable) = match bridge::connect(socket, &features) {
        Ok(client) => (Some(client), None),
        Err(why) if !options.ab => (None, Some(why)),
        Err(why) => return Err(Failure::new(why)),
    };
    let pointer = match client.as_mut() {
        Some(c) if options.ab => Some(pointer(c)?),
        _ => None,
    };
    let (mut after, mut events) = (None, Vec::new());
    if let Some(c) = client.as_mut() {
        drain(c, &mut after, &mut events)?;
    }
    let interactive = !json && ctx.prompt.interactive();
    if !json {
        let name = |n: &Node| match n.interface {
            Some(i) => format!("{} (interface {i})", n.path),
            None => n.path.clone(),
        };
        let shown: Vec<String> = nodes.iter().map(name).collect();
        let (seconds, from) = (options.seconds, shown.join(" and "));
        let mut intro = format!(
            "Recording the pointer for {seconds} s from {from}.\n\
             Keep moving the mouse in circles the whole time. Ctrl-C stops early.\n"
        );
        if options.ab {
            let half = seconds / 2;
            intro += &format!(
                "Nothing is sent for the first {half} s. Then this check asks the mouse for its \
                 battery every {QUERY_EVERY_MS} ms, through bench's read-only path (a device \
                 mode query, then 07/80).\n"
            );
        }
        if let Some(why) = &unreachable {
            intro += &format!("Stalls won't be compared with HyperFlux traffic: {why}.\n");
        }
        ctx.out.write_all(intro.as_bytes())?;
        ctx.out.flush()?;
    }
    let offset_ms = wall_minus_boottime_ms();
    let start_ms = boottime_ms();
    let end = start_ms + u64::from(options.seconds) * 1000;
    let middle = options.ab.then_some((start_ms + end) / 2);
    let reading = Arc::new(AtomicBool::new(false));
    let reader = {
        let stop = Arc::clone(&reading);
        std::thread::spawn(move || input::record(fds, &stop))
    };
    let mut queries = Queries::default();
    let (mut next_second, mut next_query) = (start_ms, middle.unwrap_or(u64::MAX));
    loop {
        let now = boottime_ms();
        if now >= end || stop.load(Ordering::Relaxed) || reader.is_finished() {
            break;
        }
        if now >= next_second {
            if let Some(c) = client.as_mut() {
                drain(c, &mut after, &mut events)?;
            }
            if interactive {
                let phase = match middle {
                    Some(m) if now >= m => ", queries running",
                    Some(_) => ", no queries yet",
                    None => "",
                };
                let left = (end - now).div_ceil(1000);
                write!(ctx.out, "\r{left:>4} s left{phase}   ")?;
                ctx.out.flush()?;
            }
            next_second += 1000;
        }
        if let (Some(c), Some(p)) = (client.as_mut(), &pointer)
            && now >= next_query
        {
            query(c, p, &mut queries);
            next_query += QUERY_EVERY_MS;
        }
        let wake = next_second.min(next_query).min(end);
        let nap = wake.saturating_sub(boottime_ms()).max(1);
        std::thread::sleep(Duration::from_millis(nap));
    }
    let end_ms = boottime_ms();
    reading.store(true, Ordering::Relaxed);
    let recording = reader.join().unwrap_or_default();
    if interactive {
        write!(ctx.out, "\r{:40}\r", "")?;
    }
    if let Some(c) = client.as_mut() {
        drain(c, &mut after, &mut events)?;
    }
    let inputs = Inputs {
        start_ms,
        end_ms,
        threshold_ms: options.threshold_ms,
        traffic: client.is_some().then(|| traffic(&events, &queries.windows)),
        kernel: kernel_log(ctx.system, offset_ms, start_ms, end_ms),
        ab: middle.map(|m| (m, queries)),
    };
    let mut report = analyze(&recording, inputs);
    report.nodes = nodes;
    if json {
        print_json(ctx.out, &report)?;
    } else {
        if recording.lost_node {
            writeln!(ctx.out, "The receiver's input went away during the run.")?;
        }
        ctx.out.write_all(text(&report).as_bytes())?;
    }
    Ok(report.exit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn motion(time_us: u64, step: i32) -> Motion {
        let (dx, dy) = (step, step);
        Motion { time_us, dx, dy }
    }

    /// A report every `every_us` in `from_us..to_us`, `step` counts each way.
    fn moving(from_us: u64, to_us: u64, every_us: usize, step: i32) -> Vec<Motion> {
        let times = (from_us..to_us).step_by(every_us);
        times.map(|t| motion(t, step)).collect()
    }

    /// 1000 reports a second for `seconds`, with 70 ms missing after each
    /// second in `gaps`.
    fn with_gaps(seconds: u64, gaps: &[u64]) -> Recording {
        let inside = |t: u64| {
            gaps.iter()
                .any(|s| t > s * 1_000_000 && t < s * 1_000_000 + 70_000)
        };
        let all = moving(0, seconds * 1_000_000, 1000, 3).into_iter();
        let motion = all.filter(|m| !inside(m.time_us)).collect();
        Recording {
            motion,
            ..Recording::default()
        }
    }

    fn inputs(seconds: u64, traffic: Option<Vec<Traffic>>) -> Inputs {
        let (end_ms, kernel) = (seconds * 1000, Some(Vec::new()));
        Inputs {
            end_ms,
            threshold_ms: 50,
            traffic,
            kernel,
            ..Inputs::default()
        }
    }

    fn reply(time_ms: u64, who: &'static str) -> Traffic {
        let command = "07/80".to_owned();
        Traffic {
            time_ms,
            lane: "pointer",
            command,
            who,
        }
    }

    #[test]
    fn gap_is_a_stall() {
        let report = analyze(&with_gaps(10, &[5]), inputs(10, Some(vec![])));
        let stall = &report.stalls[0];
        assert_eq!(
            (report.stalls.len(), stall.start_us, stall.duration_us),
            (1, 5_000_000, 70_000)
        );
        let m = &report.movement;
        assert_eq!(
            (m.moving_ms, m.p50_us, m.p99_us, m.max_us),
            (9_999, 1000, 1000, 70_000)
        );
        // 9,929 one-millisecond intervals and the stall, in 9.999 s.
        assert!((m.rate_hz - 9_930.0 / 9.999).abs() < 1e-9, "{m:?}");
        let verdict = "1 stall, none overlapping HyperFlux traffic: radio or receiver side.";
        assert_eq!(
            (report.verdict.as_str(), report.exit),
            (verdict, EXIT_ATTENTION)
        );
    }

    #[test]
    fn pauses_are_not_stalls() {
        let found = |parts: [Vec<Motion>; 2], dropped: Vec<u64>| {
            let motion = parts.concat();
            let recording = Recording {
                motion,
                dropped,
                lost_node: false,
            };
            analyze(&recording, inputs(12, Some(vec![])))
        };
        // 1.5 s without movement: the hand stopped, and that time doesn't count.
        let parts = [
            moving(0, 5_000_000, 1000, 3),
            moving(6_500_000, 12_000_000, 1000, 3),
        ];
        let pause = found(parts, vec![]);
        assert_eq!((pause.stalls.len(), pause.movement.moving_ms), (0, 10_498));
        assert_eq!(pause.verdict, "No stalls over 50 ms in 10 s of movement.");
        // Slow before, fast after: a stop and a new start.
        let parts = [
            moving(0, 5_000_000, 1000, 1),
            moving(5_300_000, 10_000_000, 1000, 5),
        ];
        assert!(found(parts, vec![]).stalls.is_empty());
        // Too few reports before the gap to call it movement.
        let parts = [
            moving(0, 5_000_000, 50_000, 3),
            moving(5_100_000, 10_000_000, 1000, 3),
        ];
        assert!(found(parts, vec![]).stalls.is_empty());
        // The kernel dropped events in the gap. It may not be real.
        let lost = found([with_gaps(10, &[5]).motion, vec![]], vec![5_010_000]);
        assert_eq!((lost.stalls.len(), lost.dropped_events), (0, 1));
    }

    #[test]
    fn overlaps_are_matched_by_time() {
        // The stall runs from 5000 to 5070 ms. A reply may end up to 60 ms
        // after it, a kernel line up to 250 ms either side.
        let traffic = [4999, 5000, 5130, 5131].map(|t| reply(t, "app")).to_vec();
        let text = "usb 3-4: reset".to_owned();
        let kernel = [4749, 4750, 5320, 5321].map(|time_ms| KernelLine {
            time_ms,
            text: text.clone(),
        });
        let inputs = Inputs {
            kernel: Some(kernel.to_vec()),
            ..inputs(10, Some(traffic))
        };
        let report = analyze(&with_gaps(10, &[5]), inputs);
        let stall = &report.stalls[0];
        let traffic: Vec<u64> = stall.traffic.iter().map(|t| t.time_ms).collect();
        let kernel: Vec<u64> = stall.kernel.iter().map(|k| k.time_ms).collect();
        assert_eq!((traffic, kernel), (vec![5000, 5130], vec![4750, 5320]));
        // 60 ms before each reply: 4939-5000 and 5070-5131 of 10 s.
        assert!((report.traffic_share - 0.0122).abs() < 1e-9, "{report:?}");
        // A 70 ms stall overlaps when a reply ends up to 130 ms after it
        // starts: 4869-5131.
        assert!(
            (report.expected_overlaps - 0.0262).abs() < 1e-9,
            "{report:?}"
        );
        let chance = "1 stall, 1 during HyperFlux traffic, about what chance alone would give \
                      (0.0): no clear link.";
        assert_eq!(report.verdict, chance);
    }

    #[test]
    fn verdicts_and_text() {
        let busy = vec![
            reply(2020, "stall-check"),
            reply(4030, "bridge"),
            reply(6000, "app"),
        ];
        let report = analyze(&with_gaps(10, &[2, 4, 6]), inputs(10, Some(busy)));
        let verdict = "3 stalls, 3 during HyperFlux traffic: queries can delay movement.";
        assert_eq!(report.verdict, verdict);
        let text = text(&report);
        assert!(
            text.contains("\n  at 2.000 s for 70.0 ms: pointer 07/80 (stall-check)\n"),
            "{text}"
        );
        assert!(
            text.contains("\nHyperFlux traffic: 3 replies, an exchange under way 2% "),
            "{text}"
        );
        let unreachable = analyze(&with_gaps(10, &[2]), inputs(10, None));
        assert!(
            unreachable
                .verdict
                .starts_with("1 stall; the bridge couldn't be reached")
        );
        let still = analyze(&with_gaps(3, &[1]), inputs(3, Some(vec![])));
        assert_eq!(
            (still.exit, &still.verdict[..19]),
            (EXIT_FAILURE, "Too little movement")
        );
    }

    #[test]
    fn halves_are_compared() {
        let ab_of = |seconds, gaps: &[u64]| {
            let ab = Some((30_000, Queries::default()));
            let inputs = Inputs {
                ab,
                ..inputs(seconds, Some(vec![]))
            };
            analyze(&with_gaps(seconds, gaps), inputs).ab.unwrap()
        };
        let more = ab_of(60, &[31, 34, 37, 40, 43, 46]);
        assert_eq!((more.passive.stalls, more.queried.stalls), (0, 6));
        assert_eq!(
            (more.passive.moving_ms, more.queried.moving_ms),
            (29_999, 30_000)
        );
        assert!(more.difference.starts_with("More stalls with the queries"));
        assert!(
            ab_of(60, &[3, 6, 9, 12, 15, 18])
                .difference
                .starts_with("Fewer")
        );
        assert!(
            ab_of(60, &[5, 10, 40, 45])
                .difference
                .starts_with("No clear difference")
        );
        assert!(ab_of(60, &[59]).difference.starts_with("No clear"));
        // Stopped early: nothing to speak of in the second half.
        assert!(
            ab_of(31, &[2])
                .difference
                .starts_with("One half had too little")
        );
        // Six of six in one half at even odds is 1 in 64; three of four
        // isn't telling.
        assert!((at_least(6, 6, 0.5) - 1.0 / 64.0).abs() < 1e-12);
        assert!((at_least(3, 4, 0.5) - 5.0 / 16.0).abs() < 1e-12);
    }

    #[test]
    fn replies_from_capture_events() {
        let data = hfx_proto::ids::lower_hex(&report::build(0x1f, 0x07, 0x80, &[0, 0]));
        let event = |time: u64, kind: &str, source: &str| {
            let (time, receiver) = (time.to_string(), "receiver-1");
            serde_json::from_value::<CaptureEvent>(json!({
                "sequence": time, "receiver_id": receiver, "generation_id": "1", "time_ms": time,
                "lane": "pointer", "kind": kind, "source": source, "data": data,
            }))
            .unwrap()
        };
        let events = [
            event(100, "reply", "bridge"),
            event(200, "reply", "bridge"),
            event(300, "reply", "app"),
            event(400, "activity", "input"),
        ];
        let found = traffic(&events, &[(150, 250)]);
        let who: Vec<&str> = found.iter().map(|t| t.who).collect();
        assert_eq!(who, ["bridge", "stall-check", "app"]);
        assert_eq!(
            (found[0].lane, found[0].command.as_str()),
            ("pointer", "07/80")
        );
    }

    #[test]
    fn kernel_lines() {
        let offset = 1_792_576_800_000;
        let reset = "usb 3-4: reset full-speed USB device number 2 using xhci_hcd";
        let line = format!("1792576805.250000 host kernel: {reset}");
        let expected = KernelLine {
            time_ms: 5250,
            text: reset.into(),
        };
        assert_eq!(kernel_line(&line, offset), Some(expected));
        let suspend = kernel_line("1792576806.0 host kernel: PM: suspend exit", offset);
        assert_eq!(suspend.map(|k| k.time_ms), Some(6000));
        let bluetooth = "1792576805.3 host kernel: Bluetooth: hci0: ACL packet";
        assert_eq!(kernel_line(bluetooth, offset), None);
        assert_eq!(kernel_line("not a line", offset), None);
    }
}
