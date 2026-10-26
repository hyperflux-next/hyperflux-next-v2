// SPDX-License-Identifier: GPL-2.0-only

//! `hyperfluxctl probe receiver`: the bridge's fixed read-only questions to
//! each receiver itself (`devices::RECEIVER_PROBES`) and the raw answers, to
//! see whether this receiver keeps a list of its devices the way other Razer
//! receivers do. It's a diagnostic, not an everyday command. The bridge
//! asks two of them itself, for the paired slots.

use crate::{Context, EXIT_OK, Failure, bridge, print_json};
use hfx_proto::devices::RECEIVER_PROBES;
use hfx_proto::negotiation::FEATURE_PROBE_RECEIVER;
use serde_json::json;
use std::path::Path;

fn spaced(bytes: &[u8]) -> String {
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    hex.join(" ")
}

/// # Errors
/// When the bridge cannot be reached or doesn't offer the probe.
pub fn run(ctx: &mut Context<'_>, socket: Option<&Path>, json: bool) -> Result<i32, Failure> {
    let mut client = bridge::connect(socket, &[FEATURE_PROBE_RECEIVER])?;
    let snapshot = client
        .snapshot()
        .map_err(|e| bridge::failed("snapshot", &e))?;
    let mut all = Vec::new();
    for r in &snapshot.receivers {
        let probe = client.probe_receiver(r.receiver_id.clone());
        let probe = probe.map_err(|e| bridge::failed("probe", &e))?;
        all.push(json!({"receiver_id": r.receiver_id, "inventory": probe.inventory, "answers": probe.answers}));
        if json {
            continue;
        }
        writeln!(ctx.out, "{}", r.receiver_id)?;
        for ((tx, class, command, _), answer) in RECEIVER_PROBES.iter().zip(&probe.answers) {
            // Status, data size and the first 16 data bytes.
            let answer = answer.as_ref().map_or("no answer".into(), |a| {
                let a = a.bytes();
                format!("status {:02x}, size {}: {}", a[0], a[5], spaced(&a[8..24]))
            });
            writeln!(ctx.out, "  {tx:02x} {class:02x}:{command:02x}  {answer}")?;
        }
        let inventory = spaced(probe.inventory.as_slice());
        writeln!(ctx.out, "  inventory  {inventory}")?;
    }
    if json {
        print_json(ctx.out, &json!({ "receivers": all }))?;
    }
    Ok(EXIT_OK)
}
