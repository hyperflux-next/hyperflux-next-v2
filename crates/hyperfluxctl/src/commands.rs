// SPDX-License-Identifier: GPL-2.0-only

//! status, devices, battery, watch and doctor.

use crate::doctor::{self, Assessment, Facts};
use crate::{
    Context, EXIT_ATTENTION, EXIT_FAILURE, EXIT_OK, Failure, bridge, fix, lines, print_json,
};
use hfx_proto::client::Client;
use hfx_proto::ids::WaitTimeoutMs;
use hfx_proto::messages::Snapshot;
use hfx_proto::negotiation::FEATURE_WAIT;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::thread;
use std::time::Duration;

const WAIT_MS: u32 = 30_000;
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

pub(crate) fn assess(ctx: &Context<'_>, socket: Option<&Path>) -> (Facts, Assessment) {
    let reach = bridge::reach(&bridge::candidates(socket), &[]);
    let facts = doctor::probe(ctx.system, reach);
    let assessment = doctor::assess(&facts);
    (facts, assessment)
}

fn snapshot_of(socket: Option<&Path>) -> Result<Snapshot, Failure> {
    let mut client = bridge::connect(socket, &[])?;
    client
        .snapshot()
        .map_err(|e| bridge::failed("snapshot", &e))
}

pub(crate) fn devices_json(snapshot: &Snapshot) -> Value {
    let now = snapshot.now_ms.get();
    let devices: Vec<_> = lines::devices(snapshot)
        .map(|(r, d)| lines::device_json(r, d, now))
        .collect();
    let receivers: Vec<_> = (snapshot.receivers.iter())
        .map(|r| json!({"receiver_id": r.receiver_id, "generation_id": r.generation_id.get(), "inventory": r.inventory}))
        .collect();
    json!({ "devices": devices, "receivers": receivers })
}

/// # Errors
/// Output errors. An unreachable bridge is part of the status.
pub fn status(ctx: &mut Context<'_>, socket: Option<&Path>, json: bool) -> Result<i32, Failure> {
    let (facts, assessment) = assess(ctx, socket);
    let (driver, driver_text) = assessment.driver.words();
    let (bridge, bridge_text) = assessment.bridge.words();
    let (keeper, keeper_text) = assessment.keeper.words();
    if json {
        let devices = facts
            .snapshot()
            .map_or(json!([]), |s| devices_json(s)["devices"].take());
        let findings: Vec<_> = assessment.findings.iter().map(|f| f.code.id()).collect();
        let value = json!({
            "driver": driver, "driver_text": driver_text, "bridge": bridge, "keeper": keeper,
            "receivers": assessment.receivers, "devices": devices,
            "findings": findings, "next": assessment.next(),
        });
        print_json(ctx.out, &value)?;
        return Ok(EXIT_OK);
    }
    let receivers = assessment.receivers;
    writeln!(
        ctx.out,
        "Driver: {driver_text}\nBridge: {bridge_text}\nKeeper: {keeper_text}\nReceivers: {receivers}"
    )?;
    if let Some(snapshot) = facts.snapshot() {
        for (_, device) in lines::devices(snapshot) {
            writeln!(
                ctx.out,
                "{}",
                lines::device_line(device, snapshot.now_ms.get())
            )?;
        }
    }
    writeln!(ctx.out, "Next: {}", assessment.next())?;
    Ok(EXIT_OK)
}

fn nothing_paired(snapshot: &Snapshot) -> &'static str {
    if snapshot.receivers.is_empty() {
        "No receiver is registered with the bridge."
    } else {
        "No paired devices."
    }
}

/// # Errors
/// When the bridge cannot be reached.
pub fn devices(ctx: &mut Context<'_>, socket: Option<&Path>, json: bool) -> Result<i32, Failure> {
    let snapshot = snapshot_of(socket)?;
    if json {
        print_json(ctx.out, &devices_json(&snapshot))?;
        return Ok(EXIT_OK);
    }
    let now = snapshot.now_ms.get();
    let blocks: Vec<_> = lines::devices(&snapshot)
        .map(|(r, d)| lines::device_block(r, d, now))
        .collect();
    if blocks.is_empty() {
        writeln!(ctx.out, "{}", nothing_paired(&snapshot))?;
        for (r, inventory) in snapshot
            .receivers
            .iter()
            .filter_map(|r| Some((r, r.inventory?)))
        {
            let inventory = hfx_proto::present::inventory_phrase(inventory);
            writeln!(ctx.out, "{}: {inventory}", r.receiver_id)?;
        }
    }
    write!(ctx.out, "{}", blocks.join("\n"))?;
    Ok(EXIT_OK)
}

fn compact(value: &impl serde::Serialize) -> Result<String, Failure> {
    serde_json::to_string(value).map_err(|e| Failure::new(format!("JSON output failed: {e}")))
}

/// # Errors
/// When the bridge cannot be reached, except with `--waybar`: a bar shows
/// an empty module instead of an error.
pub fn battery(
    ctx: &mut Context<'_>,
    socket: Option<&Path>,
    json: bool,
    waybar: bool,
) -> Result<i32, Failure> {
    let snapshot = match snapshot_of(socket) {
        Ok(snapshot) => snapshot,
        Err(failure) if waybar => {
            writeln!(
                ctx.out,
                "{}",
                compact(&lines::waybar_unavailable(&failure.message))?
            )?;
            return Ok(EXIT_OK);
        }
        Err(failure) => return Err(failure),
    };
    let now = snapshot.now_ms.get();
    if waybar {
        writeln!(ctx.out, "{}", compact(&lines::waybar(&snapshot))?)?;
    } else if json {
        print_json(ctx.out, &devices_json(&snapshot))?;
    } else {
        let text: Vec<_> = lines::devices(&snapshot)
            .map(|(_, d)| lines::battery_line(d, now))
            .collect();
        let text = if text.is_empty() {
            nothing_paired(&snapshot).to_owned()
        } else {
            text.join("\n")
        };
        writeln!(ctx.out, "{text}")?;
    }
    Ok(EXIT_OK)
}

/// What watch printed last per device: its line and its JSON.
type Key = (String, u64, String);
type Seen = BTreeMap<Key, (String, Value)>;

fn emit(
    ctx: &mut Context<'_>,
    json: bool,
    gone: bool,
    line: &str,
    device: &Value,
) -> Result<(), Failure> {
    if json {
        let mut object = device.clone();
        object["gone"] = Value::Bool(gone);
        writeln!(ctx.out, "{}", compact(&object)?)?;
    } else if gone {
        writeln!(
            ctx.out,
            "{}: gone",
            device["name"].as_str().unwrap_or("device")
        )?;
    } else {
        writeln!(ctx.out, "{line}")?;
    }
    Ok(())
}

/// Prints the lines that differ from what was printed before, in snapshot
/// order, and returns how many it printed.
fn print_changes(
    ctx: &mut Context<'_>,
    seen: &mut Seen,
    snapshot: &Snapshot,
    json: bool,
) -> Result<usize, Failure> {
    let now = snapshot.now_ms.get();
    let mut current: Vec<(Key, String, Value)> = Vec::new();
    for (receiver, device) in lines::devices(snapshot) {
        let ids = (
            receiver.receiver_id.to_string(),
            receiver.generation_id.get(),
        );
        let key = (ids.0, ids.1, device.device_id.to_string());
        let value = json!(lines::device_json(receiver, device, now));
        current.push((key, lines::device_line(device, now), value));
    }
    let mut printed = 0;
    for (key, (line, value)) in seen.iter() {
        if !current.iter().any(|(k, _, _)| k == key) {
            emit(ctx, json, true, line, value)?;
            printed += 1;
        }
    }
    for (key, line, value) in &current {
        if seen.get(key).map(|(old, _)| old) != Some(line) {
            emit(ctx, json, false, line, value)?;
            printed += 1;
        }
    }
    ctx.out.flush()?;
    *seen = current
        .into_iter()
        .map(|(k, line, value)| (k, (line, value)))
        .collect();
    Ok(printed)
}

/// Follows one connection until it fails. `limit` stops after that many
/// printed changes (tests). `None` runs until the process is interrupted.
fn follow(
    client: &mut Client,
    ctx: &mut Context<'_>,
    seen: &mut Seen,
    json: bool,
    limit: Option<usize>,
) -> Result<(), Failure> {
    let timeout = WaitTimeoutMs::new(WAIT_MS).map_err(|e| Failure::new(e.to_string()))?;
    let mut printed = 0;
    loop {
        let snapshot = client
            .snapshot()
            .map_err(|e| bridge::failed("snapshot", &e))?;
        printed += print_changes(ctx, seen, &snapshot, json)?;
        if limit.is_some_and(|l| printed >= l) {
            return Ok(());
        }
        // Only the state revision matters. A timeout just asks again.
        let revision = Some(snapshot.revision);
        while client
            .wait(revision, None, timeout)
            .map_err(|e| bridge::failed("wait", &e))?
            .timed_out
        {}
    }
}

/// # Errors
/// When the bridge cannot be reached at the start.
pub fn watch(
    ctx: &mut Context<'_>,
    socket: Option<&Path>,
    json: bool,
    limit: Option<usize>,
) -> Result<i32, Failure> {
    let mut seen = Seen::new();
    let mut client = bridge::connect(socket, &[FEATURE_WAIT])?;
    loop {
        match follow(&mut client, ctx, &mut seen, json, limit) {
            Ok(()) => return Ok(EXIT_OK),
            Err(failure) if limit.is_some() => return Err(failure),
            Err(failure) => eprintln!("hyperfluxctl: {}; reconnecting", failure.message),
        }
        // A crashed bridge is back within seconds; keep watching then.
        client = loop {
            thread::sleep(RECONNECT_DELAY);
            if let Ok(client) = bridge::connect(socket, &[FEATURE_WAIT]) {
                break client;
            }
        };
    }
}

/// `explain`, `fix` and `yes` as given on the command line.
pub type DoctorFlags = (bool, bool, bool);

/// # Errors
/// Output errors. Findings are the exit code, not a failure.
pub fn doctor(
    ctx: &mut Context<'_>,
    socket: Option<&Path>,
    json: bool,
    flags: DoctorFlags,
) -> Result<i32, Failure> {
    let (explain, run_fixes, yes) = flags;
    let (facts, assessment) = assess(ctx, socket);
    let fixes = fix::fixes(ctx.system, &assessment.findings);
    if json {
        print_json(ctx.out, &doctor::json(&facts, &assessment, &fixes))?;
    } else {
        ctx.out
            .write_all(doctor::text(&assessment, &facts.full, explain).as_bytes())?;
        if run_fixes && !assessment.findings.is_empty() && !fix::run(ctx, &fixes, yes)? {
            return Ok(EXIT_FAILURE);
        }
    }
    Ok(if assessment.findings.is_empty() {
        EXIT_OK
    } else {
        EXIT_ATTENTION
    })
}
