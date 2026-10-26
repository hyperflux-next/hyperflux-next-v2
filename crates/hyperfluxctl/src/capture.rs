// SPDX-License-Identifier: GPL-2.0-only

//! `hyperfluxctl capture`: records what the receiver reports so support for
//! a new device can be worked out from the file. The file is JSON lines: a
//! header with the current snapshot, then one bridge capture event per
//! line, with a `{"gap":true}` line wherever the bridge dropped events.
//! Receiver ids are masked as in `report`.

use crate::{Context, EXIT_OK, Failure, bridge, bundle, create_private, print_json, utc};
use hfx_proto::ids::{EventLimit, Sequence, WaitTimeoutMs};
use hfx_proto::messages::Capture;
use hfx_proto::negotiation::FEATURE_CAPTURE;
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// The longest a capture call waits, and the longest Ctrl-C goes unnoticed.
const SLICE: Duration = Duration::from_secs(1);

fn line(value: &impl serde::Serialize) -> Result<String, Failure> {
    let text = serde_json::to_string(value)
        .map_err(|e| Failure::new(format!("JSON output failed: {e}")))?;
    Ok(text + "\n")
}

/// # Errors
/// When the bridge cannot be reached, the file exists or cannot be written.
pub fn run(
    ctx: &mut Context<'_>,
    socket: Option<&Path>,
    json: bool,
    seconds: u32,
    output: Option<&Path>,
    stop: &AtomicBool,
) -> Result<i32, Failure> {
    let mut client = bridge::connect(socket, &[FEATURE_CAPTURE])?;
    let snapshot = client
        .snapshot()
        .map_err(|e| bridge::failed("snapshot", &e))?;
    let (stamp, started) = utc(SystemTime::now());
    let default = || PathBuf::from(format!("hyperflux-next-capture-{stamp}.jsonl"));
    let path = output.map_or_else(default, Path::to_path_buf);
    let mut file = create_private(&path)?;
    let write_error =
        |e: std::io::Error| Failure::new(format!("writing {} failed: {e}", path.display()));
    let mut header = json!({
        "capture": "hyperflux-next-capture", "version": hfx_proto::PRODUCT_VERSION,
        "started": started, "seconds": seconds,
        "bridge_version": client.welcome().bridge_version, "snapshot": snapshot,
    });
    let mut aliases = Vec::new();
    bundle::mask(&mut header, &mut aliases);
    file.write_all(line(&header)?.as_bytes())
        .map_err(write_error)?;
    if !json {
        writeln!(
            ctx.out,
            "Recording receiver reports for {seconds} seconds into {}.\n\
             Use the device meanwhile: move it, press keys, plug its cable in and out, \
             let it sleep. Ctrl-C stops early.",
            path.display()
        )?;
        ctx.out.flush()?;
    }
    let max_events = EventLimit::new(256).map_err(|e| Failure::new(e.to_string()))?;
    let deadline = Instant::now() + Duration::from_secs(u64::from(seconds));
    let mut after: Option<Sequence> = None;
    let (mut events, mut gaps) = (0, 0);
    while !stop.load(Ordering::Relaxed) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let wait = u32::try_from(left.min(SLICE).as_millis()).unwrap_or(1000);
        let timeout_ms = WaitTimeoutMs::new(wait).map_err(|e| Failure::new(e.to_string()))?;
        let result = client.capture(Capture {
            after,
            max_events,
            timeout_ms,
        });
        let result = result.map_err(|e| bridge::failed("capture", &e))?;
        let mut batch = String::new();
        if result.gap && after.is_some() {
            gaps += 1;
            batch.push_str("{\"gap\":true}\n");
        }
        for mut event in result.events.iter().map(|e| json!(e)) {
            bundle::mask(&mut event, &mut aliases);
            batch.push_str(&line(&event)?);
        }
        events += result.events.len();
        file.write_all(batch.as_bytes()).map_err(write_error)?;
        after = Some(result.next);
    }
    file.sync_all().map_err(write_error)?;
    if json {
        print_json(
            ctx.out,
            &json!({"output": path, "events": events, "gaps": gaps}),
        )?;
        return Ok(EXIT_OK);
    }
    let noun = if events == 1 { "event" } else { "events" };
    writeln!(
        ctx.out,
        "Capture written: {} ({events} {noun})",
        path.display()
    )?;
    if gaps > 0 {
        writeln!(
            ctx.out,
            "The bridge dropped events {gaps} times; the file marks where."
        )?;
    }
    Ok(EXIT_OK)
}
