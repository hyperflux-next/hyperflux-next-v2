// SPDX-License-Identifier: GPL-2.0-only

//! `hyperfluxctl report`: one file the owner of a device we haven't tested
//! can make and attach to a device-support request. It gathers `devices`,
//! a guided capture, `bench --defaults`, `doctor`, `probe receiver` and the
//! bridge's own journal lines for this boot, masked the way the support
//! bundle is.

use crate::args::Bench;
use crate::{Context, EXIT_OK, Failure, bridge, bundle, commands, create_private, probe, utc};
use hfx_proto::ids::{EventLimit, Sequence, WaitTimeoutMs};
use hfx_proto::messages::{BenchQuery, Capture, DeviceKind, Snapshot};
use hfx_proto::negotiation::FEATURE_CAPTURE;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

pub const DEFAULT_SECONDS: u32 = 180;
const BENCH_ITERATIONS: u16 = 200;
/// Journal lines worth keeping: the OpenRazer side, refused commands, serial
/// reads and the cable twin.
const JOURNAL_KEYWORDS: [&str; 5] = ["openrazer", "refused", "serial", "cable", "keeper"];

/// The bridge builds the queries, and `--defaults` measures each device at
/// the wait it uses for it.
fn bench_defaults() -> Bench {
    Bench {
        device: None,
        delays_us: Vec::new(),
        retries_us: Vec::new(),
        iterations: BENCH_ITERATIONS,
        query: BenchQuery::Firmware,
        reread_busy: false,
        defaults: true,
    }
}

/// Runs a command that writes one JSON document to a throwaway context and
/// gives back what it wrote. Errors from the command come straight back,
/// and the report fails the same way the command would.
fn gathered(
    ctx: &mut Context<'_>,
    run: impl FnOnce(&mut Context<'_>) -> Result<i32, Failure>,
) -> Result<Value, Failure> {
    let mut out = Vec::new();
    {
        let mut sub = Context {
            out: &mut out,
            system: ctx.system,
            prompt: ctx.prompt,
        };
        run(&mut sub)?;
    }
    serde_json::from_slice(&out).map_err(|e| Failure::new(format!("couldn't read a section: {e}")))
}

/// The guided capture's steps: what to ask the owner to do, and whether
/// it is only worth doing with a mouse.
const PHASES: [(&str, bool); 5] = [
    ("Leave the device alone.", false),
    ("Use it: move the mouse and press some keys.", false),
    ("Lift the mouse off its mat, then put it back.", true),
    ("Unplug the device's cable, then plug it in again.", false),
    ("Let the device sleep, then wake it.", false),
];

fn has_mouse(snapshot: &Snapshot) -> bool {
    snapshot
        .receivers
        .iter()
        .flat_map(|r| &r.devices)
        .any(|d| d.kind == DeviceKind::Mouse)
}

/// Prompts the owner through the phases while collecting the bridge's
/// capture events. Returns the snapshot it started from and the events.
fn guided_capture(
    ctx: &mut Context<'_>,
    socket: Option<&Path>,
    seconds: u32,
    stop: &AtomicBool,
) -> Result<(Snapshot, Vec<Value>, Value), Failure> {
    let mut client = bridge::connect(socket, &[FEATURE_CAPTURE])?;
    let snapshot = client
        .snapshot()
        .map_err(|e| bridge::failed("snapshot", &e))?;
    let phases: Vec<&str> = PHASES
        .iter()
        .filter(|(_, mouse_only)| !mouse_only || has_mouse(&snapshot))
        .map(|(say, _)| *say)
        .collect();
    let each = Duration::from_secs(u64::from(seconds)) / u32::try_from(phases.len()).unwrap_or(1);
    let max_events = EventLimit::new(256).map_err(|e| Failure::new(e.to_string()))?;
    let mut events = Vec::new();
    let mut after: Option<Sequence> = None;
    let mut gaps = 0;
    for (n, say) in phases.iter().enumerate() {
        let (secs, steps) = (each.as_secs(), phases.len());
        writeln!(ctx.out, "[{}/{steps}] {say} ({secs}s)", n + 1)?;
        ctx.out.flush()?;
        let until = Instant::now() + each;
        while !stop.load(Ordering::Relaxed) {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let slice = u32::try_from(left.min(Duration::from_secs(1)).as_millis()).unwrap_or(1000);
            let timeout_ms =
                WaitTimeoutMs::new(slice.max(1)).map_err(|e| Failure::new(e.to_string()))?;
            let result = client
                .capture(Capture {
                    after,
                    max_events,
                    timeout_ms,
                })
                .map_err(|e| bridge::failed("capture", &e))?;
            gaps += usize::from(result.gap && after.is_some());
            events.extend(result.events.iter().map(|e| json!(e)));
            after = Some(result.next);
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
    }
    let meta = json!({"seconds": seconds, "events": events.len(), "gaps": gaps});
    Ok((snapshot, events, meta))
}

/// `journalctl` for the bridge and the keeper this boot, kept to the lines
/// that matter. Says so when the journal cannot be read, rather than leaving
/// it out.
fn journal(ctx: &Context<'_>) -> Value {
    let line = "journalctl -b -o cat --no-pager -u hyperflux-next-bridge.service \
                -u hyperflux-next-keeper.service";
    let Some(out) = ctx.system.run_any(line, Duration::from_secs(5)) else {
        return json!({"readable": false, "lines": []});
    };
    if !out.success {
        return json!({"readable": false, "lines": []});
    }
    let kept: Vec<&str> = out
        .stdout
        .lines()
        .filter(|l| {
            let lower = l.to_ascii_lowercase();
            JOURNAL_KEYWORDS.iter().any(|k| lower.contains(k))
        })
        .collect();
    json!({"readable": true, "lines": kept})
}

/// # Errors
/// When the bridge cannot be reached, the file exists or cannot be written.
pub fn run(
    ctx: &mut Context<'_>,
    socket: Option<&Path>,
    json_only: bool,
    seconds: u32,
    output: Option<&Path>,
    stop: &AtomicBool,
) -> Result<i32, Failure> {
    let now = SystemTime::now();
    let (stamp, created) = utc(now);
    let default = || PathBuf::from(format!("hyperflux-next-report-{stamp}.json"));
    let path = output.map_or_else(default, Path::to_path_buf);
    // Reserve the name now, 0600, so a long capture can't be overwritten
    // meanwhile and the owner learns of a clash before doing the capture.
    let mut file = create_private(&path)?;
    let write_error =
        |e: std::io::Error| Failure::new(format!("writing {} failed: {e}", path.display()));
    if !json_only {
        writeln!(
            ctx.out,
            "Making a device report in {}. It takes about {seconds} seconds; \
             do what each step asks. Ctrl-C stops early.",
            path.display()
        )?;
        ctx.out.flush()?;
    }

    let (snapshot, capture_events, capture_meta) = guided_capture(ctx, socket, seconds, stop)?;
    let devices = commands::devices_json(&snapshot);
    let bench = gathered(ctx, |c| {
        crate::bench::run(c, socket, true, &bench_defaults())
    })?;
    let (facts, assessment) = commands::assess(ctx, socket);
    let doctor = bundle::shared_doctor(ctx.system, &facts, assessment);
    let probe = gathered(ctx, |c| probe::run(c, socket, true))?;

    let mut report = json!({
        "report": "hyperflux-next-report",
        "version": hfx_proto::PRODUCT_VERSION,
        "created": created,
        "host": {
            "kernel": ctx.system.read("/proc/sys/kernel/osrelease").map(|s| s.trim().to_owned()),
        },
        "devices": devices,
        "capture": {"guide": capture_meta, "events": capture_events},
        "bench": bench,
        "doctor": doctor,
        "probe_receiver": probe,
        "journal": journal(ctx),
    });
    bundle::mask(&mut report, &mut Vec::new());

    let bytes = serde_json::to_vec_pretty(&report)
        .map_err(|e| Failure::new(format!("report failed: {e}")))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(write_error)?;
    if json_only {
        crate::print_json(ctx.out, &json!({"output": path}))?;
    } else {
        writeln!(ctx.out, "Report written: {}", path.display())?;
    }
    Ok(EXIT_OK)
}
