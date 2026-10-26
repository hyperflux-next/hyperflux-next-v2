// SPDX-License-Identifier: GPL-2.0-only

//! `hyperfluxctl bench`: how long a read-only query takes with shorter
//! waits before the first GET, and with `--retry-us` other waits between
//! GETs (with `--reread-busy` also after a busy answer), on all online
//! devices or the one `--device` names. `--defaults` measures each device
//! at the timings the bridge uses for it. The bridge builds the queries
//! itself, and the measurement changes nothing. Each device's default wait
//! is set in devices.toml from these numbers on real receivers, not here.
//! Outside bench the bridge reads again 1 ms after a reply that doesn't
//! match.

use crate::args::Bench;
use crate::{Context, EXIT_OK, Failure, bridge, lines, print_json};
use hfx_proto::client::{BenchReads, Client};
use hfx_proto::devices;
use hfx_proto::ids::{
    BenchIterations, FirstGetDelayUs, GetIntervalUs, LeaseDurationMs, LeaseId, RouteToken,
};
use hfx_proto::messages::{BenchOutcome, BenchQuery, BenchResult, BenchSample, LeaseOutcome};
use hfx_proto::messages::{DeviceInfo, Presence, Route, RouteList, Snapshot};
use hfx_proto::negotiation::{FEATURE_BENCH, FEATURE_BENCH_INTERVAL, FEATURE_NATIVE_ROUTES};
use hfx_proto::report;
use serde::Serialize;
use serde_json::json;
use std::fmt;
use std::path::Path;

const LEASE_MS: u32 = 60_000;

/// Samples by what came back. Matched answers are told apart by the
/// receiver's status byte. The last three kinds never got an answer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub ok: usize,
    pub busy: usize,
    pub failed: usize,
    /// Status 0x04: the device did not answer the receiver, or is asleep.
    pub no_response: usize,
    pub not_supported: usize,
    /// A status byte the receiver isn't known to send.
    pub other_status: usize,
    /// Nothing that matched came back within the 40 ms of reads.
    pub unanswered: usize,
    pub transfer_failed: usize,
    pub not_sent: usize,
}

impl Counts {
    fn add(&mut self, sample: &BenchSample) {
        let count = match (sample.outcome, sample.status) {
            (BenchOutcome::Ok, _) => &mut self.ok,
            (BenchOutcome::DeviceStatus, Some(report::STATUS_BUSY)) => &mut self.busy,
            (BenchOutcome::DeviceStatus, Some(report::STATUS_FAILURE)) => &mut self.failed,
            (BenchOutcome::DeviceStatus, Some(report::STATUS_TIMEOUT)) => &mut self.no_response,
            (BenchOutcome::DeviceStatus, Some(report::STATUS_NOT_SUPPORTED)) => {
                &mut self.not_supported
            }
            (BenchOutcome::DeviceStatus, _) => &mut self.other_status,
            (BenchOutcome::NoResponse, _) => &mut self.unanswered,
            (BenchOutcome::Failed, _) => &mut self.transfer_failed,
            (BenchOutcome::NotSent, _) => &mut self.not_sent,
        };
        *count += 1;
    }

    /// Every kind but ok, as the table names it after a number.
    fn others(&self) -> [(usize, &'static str); 8] {
        [
            (self.busy, "busy"),
            (self.failed, "failed"),
            (self.no_response, "no response/asleep"),
            (self.not_supported, "not supported"),
            (self.other_status, "with another status"),
            (self.unanswered, "without an answer"),
            (self.transfer_failed, "failed transfers"),
            (self.not_sent, "not sent"),
        ]
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Summary {
    pub first_get_delay_us: u32,
    pub get_interval_us: u32,
    pub counts: Counts,
    pub median_us: Option<u32>,
    pub p90_us: Option<u32>,
    pub max_us: Option<u32>,
    pub mean_gets: Option<f64>,
    pub samples: Vec<BenchSample>,
}

/// One device's runs, one summary per first-GET delay and retry interval.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Measured {
    pub device_id: String,
    pub name: String,
    /// The wait the bridge uses for this device outside bench.
    pub default_first_get_delay_us: u16,
    pub results: Vec<Summary>,
}

/// Timings count matched answers with status 0x02 only. Anything else is
/// counted by kind.
#[must_use]
pub fn summarize(result: BenchResult) -> Summary {
    let mut counts = Counts::default();
    result.samples.iter().for_each(|s| counts.add(s));
    let ok: Vec<&BenchSample> = result
        .samples
        .iter()
        .filter(|s| s.outcome == BenchOutcome::Ok)
        .collect();
    let mut times: Vec<u32> = ok.iter().map(|s| s.elapsed_us).collect();
    times.sort_unstable();
    // Nearest rank: the smallest time at least `share` of the runs reach.
    let rank = |share: f64| {
        let index = (share * times.len() as f64).ceil() as usize;
        times.get(index.saturating_sub(1)).copied()
    };
    let gets: f64 = ok.iter().map(|s| f64::from(s.gets)).sum();
    Summary {
        first_get_delay_us: result.first_get_delay_us,
        get_interval_us: result.get_interval_us,
        counts,
        median_us: rank(0.5),
        p90_us: rank(0.9),
        max_us: times.last().copied(),
        mean_gets: (!ok.is_empty()).then(|| gets / ok.len() as f64),
        samples: result.samples,
    }
}

fn ms(us: Option<u32>) -> String {
    us.map_or_else(
        || "-".into(),
        |us| format!("{:.1} ms", f64::from(us) / 1000.0),
    )
}

const HEADER: &str = "first GET  retry     ok          median       p90       max   GETs";

/// A summary's cells, from the first-GET wait to the GETs.
fn row(s: &Summary, iterations: u16) -> String {
    let gets = s
        .mean_gets
        .map_or_else(|| "-".into(), |g| format!("{g:.1}"));
    let delay = format!("{} us", s.first_get_delay_us);
    let retry = format!("{} us", s.get_interval_us);
    let ok = format!("{}/{iterations}", s.counts.ok);
    let (median, p90, max) = (ms(s.median_us), ms(s.p90_us), ms(s.max_us));
    format!("{delay:<11}{retry:<10}{ok:<9}{median:>9}{p90:>10}{max:>10}{gets:>7}")
}

/// What else came back, unless that's nothing: "1 busy, 2 not sent".
fn others(s: &Summary, skip_busy: bool) -> Option<String> {
    let others: Vec<String> = s
        .counts
        .others()
        .iter()
        .skip(usize::from(skip_busy))
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} {what}"))
        .collect();
    (!others.is_empty()).then(|| others.join(", "))
}

/// With `reread_busy` a last column counts the exchanges that were still
/// busy at the end.
#[must_use]
pub fn table(device: &Measured, iterations: u16, reread_busy: bool) -> String {
    let busy = if reread_busy { "  busy" } else { "" };
    let mut out = format!(
        "{} ({}), default first GET {} us\n{HEADER}{busy}\n",
        device.name, device.device_id, device.default_first_get_delay_us
    );
    for s in &device.results {
        let busy = reread_busy.then(|| format!("{:>6}", s.counts.busy));
        let row = row(s, iterations);
        out.push_str(&format!("{row}{}\n", busy.unwrap_or_default()));
    }
    for s in &device.results {
        // Busy comes first, and the busy column already has it.
        if let Some(others) = others(s, reread_busy) {
            let (delay, retry) = (s.first_get_delay_us, s.get_interval_us);
            out.push_str(&format!("{delay} us, retry {retry} us: {others}\n"));
        }
    }
    out
}

/// `--defaults`: one row per device, at the timings the bridge uses for it.
#[must_use]
pub fn defaults_table(devices: &[Measured], iterations: u16) -> String {
    let (mut out, mut notes) = (format!("{:<15}{HEADER}\n", "device"), String::new());
    for device in devices {
        for s in &device.results {
            out.push_str(&format!("{:<15}{}\n", device.device_id, row(s, iterations)));
            if let Some(others) = others(s, false) {
                notes.push_str(&format!("{}: {others}\n", device.device_id));
            }
        }
    }
    out + &notes
}

struct Target<'a> {
    route: &'a Route,
    token: &'a RouteToken,
    name: String,
}

/// The wait the bridge has learned for the route's device, or the catalog's
/// from a bridge that does not report one.
pub(crate) fn learned_wait(snapshot: &Snapshot, route: &Route) -> u16 {
    let learned = device_of(snapshot, route).and_then(|d| d.first_get);
    learned.map_or_else(
        || devices::first_get_delay_us(route.product_id),
        |w| w.delay_us,
    )
}

/// The snapshot's entry for the route's device.
fn device_of<'a>(snapshot: &'a Snapshot, route: &Route) -> Option<&'a DeviceInfo> {
    let mut all = lines::devices(snapshot);
    let found = all.find(|(r, d)| {
        r.receiver_id == route.receiver_id
            && r.generation_id == route.generation_id
            && d.device_id == route.device_id
    });
    found.map(|(_, d)| d)
}

#[derive(Serialize)]
struct Skipped {
    device_id: String,
    name: String,
    why: String,
}

impl Skipped {
    fn new(route: &Route, name: &str, why: impl Into<String>) -> Self {
        Self {
            device_id: route.device_id.to_string(),
            name: name.to_owned(),
            why: why.into(),
        }
    }
}

impl fmt::Display for Skipped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Skipped {} ({}): {}.",
            self.name, self.device_id, self.why
        )
    }
}

/// The device `--device` names, or all online devices the bridge offers a
/// route for, in the snapshot's order. The others with a route come back
/// as skipped, with why.
fn targets<'a>(
    snapshot: &Snapshot,
    routes: &'a RouteList,
    wanted: Option<&str>,
) -> Result<(Vec<Target<'a>>, Vec<Skipped>), Failure> {
    if let Some(id) = wanted {
        let Some(route) = routes.routes.iter().find(|r| r.device_id.as_str() == id) else {
            let why = format!("there's no device {id}; hyperfluxctl devices lists the ids");
            return Err(Failure::new(why));
        };
        let Some(token) = &route.route_token else {
            let why =
                format!("{id} can't be measured right now: it isn't online or can't be controlled");
            return Err(Failure::new(why));
        };
        let name = device_of(snapshot, route).map_or_else(|| id.to_owned(), lines::name);
        return Ok((vec![Target { route, token, name }], Vec::new()));
    }
    let (mut found, mut skipped) = (Vec::new(), Vec::new());
    for (receiver, device) in lines::devices(snapshot) {
        let route = routes.routes.iter().find(|r| {
            r.receiver_id == receiver.receiver_id
                && r.generation_id == receiver.generation_id
                && r.device_id == device.device_id
        });
        let Some(route) = route else { continue };
        let name = lines::name(device);
        match (device.presence, &route.route_token) {
            (Presence::Online, Some(token)) => found.push(Target { route, token, name }),
            (Presence::Online, None) => {
                skipped.push(Skipped::new(route, &name, "can't be controlled right now"));
            }
            (presence, _) => skipped.push(Skipped::new(route, &name, lines::presence(presence))),
        }
    }
    if found.is_empty() {
        let why = "no device can be measured right now; hyperfluxctl devices shows why";
        return Err(Failure::new(why));
    }
    Ok((found, skipped))
}

fn invalid(error: hfx_proto::ids::ScalarError) -> Failure {
    Failure::new(error.to_string())
}

/// The device's lease, or who holds it instead.
fn lease(client: &mut Client, target: &Target<'_>) -> Result<Result<LeaseId, String>, Failure> {
    let duration = LeaseDurationMs::new(LEASE_MS).map_err(invalid)?;
    let lease = client.acquire_lease(target.token, duration);
    let lease = lease.map_err(|e| bridge::failed("acquire-lease", &e))?;
    Ok(match (lease.outcome, lease.lease_id) {
        (LeaseOutcome::Granted, Some(id)) => Ok(id),
        _ => Err(lease
            .holder
            .map_or_else(|| "another program".into(), |h| h.to_string())),
    })
}

/// Each delay with each retry interval. Runs the bridge won't take in one
/// request (over 100) go in several, with the samples put together.
fn measure(
    client: &mut Client,
    lease: &LeaseId,
    token: &RouteToken,
    options: &Bench,
    delays_us: &[u16],
) -> Result<Vec<Summary>, Failure> {
    let duration = LeaseDurationMs::new(LEASE_MS).map_err(invalid)?;
    let retries: Vec<Option<GetIntervalUs>> = if options.retries_us.is_empty() {
        vec![None]
    } else {
        let given = options.retries_us.iter().map(|r| GetIntervalUs::new(*r));
        given
            .map(|r| r.map(Some))
            .collect::<Result<_, _>>()
            .map_err(invalid)?
    };
    let mut summaries = Vec::new();
    for delay in delays_us {
        let delay = FirstGetDelayUs::new(*delay).map_err(invalid)?;
        for retry in &retries {
            let reads = BenchReads {
                interval: *retry,
                reread_busy: options.reread_busy || options.defaults,
            };
            let mut all: Option<BenchResult> = None;
            for done in (0..options.iterations).step_by(usize::from(BenchIterations::MAX)) {
                let renewed = client.renew_lease(lease, duration);
                renewed.map_err(|e| bridge::failed("renewing the lease", &e))?;
                let n = (options.iterations - done).min(BenchIterations::MAX);
                let n = BenchIterations::new(n).map_err(invalid)?;
                let result = client.bench(lease, token, options.query, delay, n, reads);
                let result = result.map_err(|e| bridge::failed("bench", &e))?;
                match &mut all {
                    Some(all) => all.samples.extend(result.samples),
                    None => all = Some(result),
                }
            }
            summaries.extend(all.map(summarize));
        }
    }
    Ok(summaries)
}

/// # Errors
/// When the bridge cannot be reached, no device can be measured, the one
/// asked for is busy or missing, or a measurement fails.
pub fn run(
    ctx: &mut Context<'_>,
    socket: Option<&Path>,
    json: bool,
    options: &Bench,
) -> Result<i32, Failure> {
    let mut features = vec![FEATURE_NATIVE_ROUTES, FEATURE_BENCH];
    // --defaults reads on after busy answers, as the bridge's own exchanges do.
    if !options.retries_us.is_empty() || options.defaults {
        features.push(FEATURE_BENCH_INTERVAL);
    }
    let mut client = bridge::connect(socket, &features)?;
    let snapshot = client
        .snapshot()
        .map_err(|e| bridge::failed("snapshot", &e))?;
    let routes = client
        .list_routes()
        .map_err(|e| bridge::failed("list-routes", &e))?;
    let wanted = options.device.as_deref();
    let (targets, mut skipped) = targets(&snapshot, &routes, wanted)?;
    let query = if options.query == BenchQuery::Battery {
        "battery"
    } else {
        "firmware"
    };
    if !json {
        let what = match &targets[..] {
            [only] if wanted.is_some() => format!("{} ({})", only.name, only.route.device_id),
            _ => "every online device".to_owned(),
        };
        let per = match (options.retries_us.is_empty(), options.reread_busy) {
            _ if options.defaults => "device, at its default timings",
            (true, _) => "first-GET delay",
            (false, false) => "first-GET delay and retry interval",
            (false, true) => "first-GET delay and retry interval, reading again after busy answers",
        };
        writeln!(
            ctx.out,
            "Measuring {what} with the {query} query, {} times per {per}.\n\
             Only read-only queries are sent. Nothing on the devices or in HyperFlux Next V2 \
             changes, and each device keeps its default wait.",
            options.iterations
        )?;
        for skip in &skipped {
            writeln!(ctx.out, "{skip}")?;
        }
        ctx.out.flush()?;
    }
    let mut measured = Vec::new();
    let mut in_use = None;
    for target in &targets {
        let lease_id = match lease(&mut client, target)? {
            Ok(id) => id,
            Err(holder) => {
                let why = format!(
                    "{} is in use by {holder}; try again when it's idle",
                    target.name
                );
                if wanted.is_some() {
                    return Err(Failure::new(why));
                }
                let skip = Skipped::new(target.route, &target.name, format!("in use by {holder}"));
                if !json {
                    writeln!(ctx.out, "{skip}")?;
                }
                skipped.push(skip);
                in_use.get_or_insert(why);
                continue;
            }
        };
        let default = learned_wait(&snapshot, target.route);
        let delays = if options.defaults {
            vec![default]
        } else {
            options.delays_us.clone()
        };
        let results = measure(&mut client, &lease_id, target.token, options, &delays);
        // Closing the connection would end the lease too. Releasing
        // says so sooner.
        let _ = client.release_lease(&lease_id);
        let device = Measured {
            device_id: target.route.device_id.to_string(),
            name: target.name.clone(),
            default_first_get_delay_us: default,
            results: results?,
        };
        if !json && !options.defaults {
            let text = table(&device, options.iterations, options.reread_busy);
            write!(ctx.out, "\n{text}")?;
            ctx.out.flush()?;
        }
        measured.push(device);
    }
    if measured.is_empty() {
        let why = in_use.unwrap_or_else(|| "no device could be measured".into());
        return Err(Failure::new(why));
    }
    if json {
        let value = json!({
            "query": options.query, "iterations": options.iterations,
            "reread_busy": options.reread_busy, "defaults": options.defaults,
            "devices": measured, "skipped": skipped,
        });
        print_json(ctx.out, &value)?;
    } else if options.defaults {
        let text = defaults_table(&measured, options.iterations);
        write!(ctx.out, "\n{text}")?;
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(outcome: BenchOutcome, status: Option<u8>, elapsed_us: u32, gets: u8) -> BenchSample {
        BenchSample {
            outcome,
            status,
            elapsed_us,
            gets,
        }
    }

    fn ok(elapsed_us: u32, gets: u8) -> BenchSample {
        sample(BenchOutcome::Ok, Some(2), elapsed_us, gets)
    }

    fn result(first_get_delay_us: u32, samples: Vec<BenchSample>) -> BenchResult {
        BenchResult {
            first_get_delay_us,
            get_interval_us: 1000,
            samples,
        }
    }

    fn device(results: Vec<Summary>) -> Measured {
        Measured {
            device_id: "keyboard-0296".into(),
            name: "Razer DeathStalker V2 Pro TKL".into(),
            default_first_get_delay_us: 4000,
            results,
        }
    }

    #[test]
    fn summary_and_table() {
        let mut samples: Vec<BenchSample> = (1..=9).map(|i| ok(10_000 + i * 100, 1)).collect();
        samples.push(ok(21_000, 2));
        samples.push(sample(BenchOutcome::NoResponse, None, 47_000, 5));
        let summary = summarize(result(5000, samples));
        assert_eq!((summary.counts.ok, summary.counts.unanswered), (10, 1));
        let timings = (summary.median_us, summary.p90_us, summary.max_us);
        assert_eq!(timings, (Some(10_500), Some(10_900), Some(21_000)));
        assert_eq!(summary.mean_gets, Some(1.1));
        assert_eq!(
            table(&device(vec![summary]), 11, false),
            "Razer DeathStalker V2 Pro TKL (keyboard-0296), default first GET 4000 us\n\
             first GET  retry     ok          median       p90       max   GETs\n\
             5000 us    1000 us   10/11      10.5 ms   10.9 ms   21.0 ms    1.1\n\
             5000 us, retry 1000 us: 1 without an answer\n"
        );
        let nothing = summarize(result(
            1000,
            vec![sample(BenchOutcome::NotSent, None, 0, 0)],
        ));
        let rows = "1000 us    1000 us   0/1              -         -         -      -\n\
                    1000 us, retry 1000 us: 1 not sent\n";
        assert!(table(&device(vec![nothing]), 1, false).ends_with(rows));
    }

    #[test]
    fn answers_are_counted_by_status() {
        let status = |s: u8| sample(BenchOutcome::DeviceStatus, Some(s), 4100, 1);
        let mut samples = vec![ok(4300, 1), ok(4400, 1)];
        samples.extend([1, 1, 1, 3, 4, 4, 5, 7].map(status));
        samples.push(sample(BenchOutcome::Failed, None, 4200, 1));
        let summary = summarize(result(4000, samples));
        let counts = Counts {
            ok: 2,
            busy: 3,
            failed: 1,
            no_response: 2,
            not_supported: 1,
            other_status: 1,
            transfer_failed: 1,
            ..Counts::default()
        };
        assert_eq!(summary.counts, counts);
        // Only the two matched answers are timed.
        assert_eq!(summary.max_us, Some(4400));
        let text = table(&device(vec![summary.clone()]), 11, false);
        assert!(text.ends_with(
            "4000 us, retry 1000 us: 3 busy, 1 failed, 2 no response/asleep, \
             1 not supported, 1 with another status, 1 failed transfers\n"
        ));
        // Reading on after busy, the busy ones get a column of their own.
        let text = table(&device(vec![summary]), 11, true);
        assert!(text.contains(" max   GETs  busy\n4000 us    1000 us   2/11"));
        assert!(text.contains("4.4 ms    1.0     3\n4000 us, retry 1000 us: 1 failed, 2 no"));
    }
}
