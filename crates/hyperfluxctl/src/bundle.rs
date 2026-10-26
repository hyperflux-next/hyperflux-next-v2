// SPDX-License-Identifier: GPL-2.0-only

//! `hyperfluxctl support-bundle`: one JSON file to attach to a bug report.
//! It holds typed facts and nothing else: serials, reports and logs stay
//! out by not having a field. Doctor's findings go in masked, as in
//! `report`: no link names (those can carry serials), and `$USER` and
//! `$(id -u)` for this login. `mask` gives receivers the aliases `report`
//! and `capture` use too.

use crate::doctor::{self, Assessment, Facts, SECURE_BOOT_VAR};
use crate::system::System;
use crate::{Context, EXIT_OK, Failure, commands, create_private, fix, print_json, utc};
use hfx_proto::PRODUCT_VERSION;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BUNDLE_BYTES: usize = 1_048_576;

pub const INCLUDED: &[&str] = &[
    "HyperFlux Next V2 version, kernel release and distribution",
    "driver, bridge and Full integration state",
    "Doctor findings and their advice",
    "receivers and devices: model ids, presence, battery and who controls them",
];

pub const EXCLUDED: &[&str] = &[
    "serial numbers, receiver ids and other stable identifiers",
    "input link names, which can contain serial numbers",
    "receiver reports and device replies",
    "journal and terminal text",
];

/// `hfx-20261021T101500Z-1a2b3c4d`: readable time first, which makes
/// bundles sort, and a short hash so two in the same second still differ.
#[must_use]
pub fn bundle_id(assessment: &Assessment, now: SystemTime) -> String {
    let codes: Vec<&str> = assessment.findings.iter().map(|f| f.code.id()).collect();
    let nanos = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let (receivers, pid) = (assessment.receivers, std::process::id());
    let seed = format!("{PRODUCT_VERSION}|{receivers}|{codes:?}|{nanos}|{pid}");
    let digest = Sha256::digest(seed.as_bytes());
    let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("hfx-{}-{hex}", utc(now).0)
}

fn distribution(ctx: &Context<'_>) -> Option<String> {
    let text = ctx
        .system
        .read("/etc/os-release")
        .or_else(|| ctx.system.read("/usr/lib/os-release"))?;
    let field = |key: &str| {
        let value = text
            .lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))?;
        Some(value.trim_matches('"').to_owned())
    };
    let id = field("ID")?;
    Some(field("VERSION_ID").map_or_else(|| id.clone(), |version| format!("{id} {version}")))
}

/// Doctor as the bundle and `report` write it: link names left out before
/// the texts are filled in, and this login's uid and name as `$(id -u)` and
/// `$USER`. On screen Doctor keeps them, since they name the reader.
#[must_use]
pub fn shared_doctor(system: &System, facts: &Facts, mut assessment: Assessment) -> Value {
    let uid = system.uid.to_string();
    let runtime = format!("/run/user/{uid}/");
    let hide = |(k, v): &(&'static str, String)| {
        let v = match *k {
            "link" => "(link name left out)".to_owned(),
            "uid" if *v == uid => "$(id -u)".to_owned(),
            _ => v.replace(&runtime, "/run/user/$(id -u)/"),
        };
        (*k, v)
    };
    for finding in &mut assessment.findings {
        finding.details = finding.details.iter().map(hide).collect();
    }
    let fixes = fix::fixes(system, &assessment.findings);
    let mut doctor = doctor::json(facts, &assessment, &fixes);
    let user = system.run("id -un");
    let shared = |f: &fix::Fix| f.shared_json(system.uid, user.as_deref());
    doctor["fixes"] = fixes.iter().map(shared).collect();
    doctor
}

/// Calls `f` on every string in `value`.
fn strings(value: &mut Value, f: &mut impl FnMut(&mut String)) {
    match value {
        Value::String(text) => f(text),
        Value::Array(items) => items.iter_mut().for_each(|c| strings(c, f)),
        Value::Object(map) => map.values_mut().for_each(|c| strings(c, f)),
        _ => {}
    }
}

/// Writes `receiver-1`, `receiver-2` and so on, in sorted order after any
/// already in `aliases`, for each receiver id in `value` and the hex digits
/// other strings embed from it. Ids are found by their shape. A journal
/// line about a receiver that has gone counts too.
pub fn mask(value: &mut Value, aliases: &mut Vec<(String, String)>) {
    let mut found = Vec::new();
    strings(value, &mut |text| {
        for (at, _) in text.match_indices("receiver-") {
            let hex = |id: &&str| id[9..].bytes().all(|b| b.is_ascii_hexdigit());
            found.extend(text.get(at..at + 33).filter(hex).map(str::to_owned));
        }
    });
    found.sort();
    for id in found {
        if !aliases.iter().any(|(known, _)| *known == id) {
            let alias = format!("receiver-{}", aliases.len() + 1);
            aliases.push((id, alias));
        }
    }
    strings(value, &mut |text| {
        for (id, alias) in aliases.iter() {
            *text = text.replace(id, alias).replace(&id[9..], alias);
        }
    });
}

/// Interface number, driver and hidraw node with its owner, group and mode.
fn interfaces(facts: &Facts) -> Value {
    let one = |i: &doctor::Interface| {
        let node = i.hidraw.as_ref().map(|(node, access)| match access {
            Some((uid, gid, mode)) => format!("{node} user {uid}, group {gid}, mode {mode:04o}"),
            None => format!("{node} missing"),
        });
        json!({"interface": i.number, "driver": i.driver, "hidraw": node})
    };
    Value::Array(facts.interfaces.iter().map(one).collect())
}

fn receivers(facts: &Facts) -> Value {
    let Some(snapshot) = facts.snapshot() else {
        return json!([]);
    };
    let device = |d: &hfx_proto::messages::DeviceInfo| {
        json!({
            "product_id": format!("{:04x}:{:04x}", d.vendor_id, d.product_id),
            "kind": d.kind, "support": d.support, "presence": d.presence,
            "battery_level": d.battery.level, "battery_stale": d.battery.stale,
            "controllable": d.controllable, "controlled_by": d.controlled_by,
        })
    };
    let receiver = |r: &hfx_proto::messages::ReceiverInfo| {
        let devices: Vec<Value> = r.devices.iter().map(device).collect();
        let generation = r.generation_id.get();
        json!({"generation_id": generation, "state": r.state, "writable": r.writable, "devices": devices})
    };
    Value::Array(snapshot.receivers.iter().map(receiver).collect())
}

/// # Errors
/// When the file exists, cannot be written, or would be too large.
pub fn run(
    ctx: &mut Context<'_>,
    socket: Option<&Path>,
    json: bool,
    preview: bool,
    output: Option<&Path>,
) -> Result<i32, Failure> {
    let (facts, assessment) = commands::assess(ctx, socket);
    let now = SystemTime::now();
    let id = bundle_id(&assessment, now);
    let default = || PathBuf::from(format!("hyperflux-next-support-{id}.json"));
    let path = output.map_or_else(default, Path::to_path_buf);
    if preview {
        let value = json!({
            "bundle_id": id, "output": path, "mode": "0600", "file_written": false,
            "included": INCLUDED, "excluded": EXCLUDED,
        });
        print_json(ctx.out, &value)?;
        return Ok(EXIT_OK);
    }
    let doctor = shared_doctor(ctx.system, &facts, assessment);
    let secure_boot = ctx
        .system
        .read_bytes(SECURE_BOOT_VAR)
        .map(|b| b.get(4) == Some(&1));
    let mut bundle = json!({
        "bundle": "hyperflux-next-support-bundle",
        "bundle_id": id,
        "created": utc(now).1,
        "version": PRODUCT_VERSION,
        "host": {
            "kernel": ctx.system.read("/proc/sys/kernel/osrelease"),
            "distribution": distribution(ctx),
            "secure_boot": secure_boot,
        },
        "receiver_access": {
            "receivers_attached": facts.receivers_attached,
            "interfaces": interfaces(&facts),
            "uhid": facts.uhid.map(|(_, gid, mode)| format!("group {gid}, mode {mode:04o}")),
            "old_driver": {
                "loaded": facts.leftovers.loaded, "package": facts.leftovers.package,
                "dkms": facts.leftovers.dkms, "inert_dkms": facts.leftovers.inert,
            },
        },
        "doctor": doctor,
        "receivers": receivers(&facts),
    });
    mask(&mut bundle, &mut Vec::new());
    let failed = |why: String| Failure::new(format!("support bundle failed: {why}"));
    let bytes = serde_json::to_vec_pretty(&bundle).map_err(|e| failed(e.to_string()))?;
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(failed("it would be larger than 1 MiB".into()));
    }
    let mut file = create_private(&path).map_err(|e| failed(e.message))?;
    let written = std::io::Write::write_all(&mut file, &bytes).and_then(|()| file.sync_all());
    written.map_err(|e| failed(e.to_string()))?;
    if json {
        print_json(ctx.out, &json!({"output": path, "bundle_id": id}))?;
    } else {
        writeln!(ctx.out, "Support bundle written: {}", path.display())?;
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiver_aliases() {
        let id = |c: &str| format!("receiver-{}", c.repeat(24));
        let hex = "a".repeat(24);
        let mut doc = json!([id("b"), {"id": id("a")}, format!("battery/{hex}/pointer")]);
        let mut aliases = Vec::new();
        // Sorted: the alias does not depend on where an id was seen first,
        // and the hex digits another string embeds are masked too.
        mask(&mut doc, &mut aliases);
        assert_eq!(
            doc,
            json!(["receiver-2", {"id": "receiver-1"}, "battery/receiver-1/pointer"])
        );
        // Line by line, as a capture does: a receiver that turns up later
        // gets the next number.
        let mut line = json!(format!("{} gone, {} admitted", id("b"), id("0")));
        mask(&mut line, &mut aliases);
        assert_eq!(line, "receiver-2 gone, receiver-3 admitted");
    }
}
