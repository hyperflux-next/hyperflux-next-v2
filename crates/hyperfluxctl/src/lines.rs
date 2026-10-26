// SPDX-License-Identifier: GPL-2.0-only

//! How devices read in text and JSON. The user docs quote these lines, so
//! the wording here is part of the interface.

use hfx_proto::messages::{
    Battery, Charge, Contact, DeviceInfo, DeviceKind, Lane, OpenRazerDevice, OpenRazerState,
    Presence, ReceiverInfo, Slot, Snapshot, Support, Unreported,
};
use hfx_proto::present::{age_phrase, inventory_phrase};
use serde::Serialize;
use serde_json::{Value, json};

/// For the stand-in, product id 0, of a device the receiver has not named.
pub const UNNAMED: &str = "move or wake the device so the receiver names it";

const fn lane_word(lane: Lane) -> &'static str {
    match lane {
        Lane::Pointer => "pointer",
        Lane::Keyboard => "keyboard",
    }
}

/// "Razer Basilisk V3 Pro 35K", "Unknown device 1532:00ff", or
/// "Unidentified pointer device".
#[must_use]
pub fn name(device: &DeviceInfo) -> String {
    match &device.name {
        Some(name) => name.to_string(),
        None if device.product_id == 0 => format!("Unidentified {} device", lane_word(device.lane)),
        None => format!(
            "Unknown device {:04x}:{:04x}",
            device.vendor_id, device.product_id
        ),
    }
}

#[must_use]
pub fn label(device: &DeviceInfo) -> String {
    match device.support {
        Support::Checked => format!("{} (not yet tested through the receiver)", name(device)),
        _ => name(device),
    }
}

#[must_use]
pub const fn presence(presence: Presence) -> &'static str {
    match presence {
        Presence::Online => "online",
        Presence::Asleep => "asleep",
        Presence::Unreachable => "not reachable",
        Presence::Waiting => "waiting for activity",
    }
}

fn age(time: Option<hfx_proto::ids::MonotonicMs>, now_ms: u64) -> Option<u64> {
    time.map(|t| now_ms.saturating_sub(t.get()))
}

/// "charging", "full" or nothing. Full is charging at 100 %.
fn charge_word(battery: &Battery) -> Option<&'static str> {
    match (battery.charge, battery.level.map(|l| l.get())) {
        (Some(Charge::Charging), Some(100)) => Some("full"),
        (Some(Charge::Charging), _) => Some("charging"),
        _ => None,
    }
}

/// The charge word, or that the device has no way to tell.
fn charge_note(battery: &Battery) -> Option<&'static str> {
    let unreported = battery.unreported == Some(Unreported::Charge);
    charge_word(battery).or(unreported.then_some("charging not reported"))
}

/// "not reported yet", or "not reported" for a device that can't tell.
fn no_level(battery: &Battery) -> &'static str {
    match battery.unreported {
        Some(Unreported::Battery) => "not reported",
        _ => "not reported yet",
    }
}

fn stale_age(battery: &Battery, now_ms: u64) -> Option<String> {
    let age = age(battery.updated_ms, now_ms).unwrap_or(0);
    battery.stale.then(|| age_phrase(age))
}

/// "battery 79%, charging" or "battery 64%, no update for 42 minutes".
#[must_use]
pub fn battery_phrase(battery: &Battery, now_ms: u64) -> String {
    let Some(level) = battery.level else {
        return format!("battery {}", no_level(battery));
    };
    let charge = charge_note(battery)
        .map(|w| format!(", {w}"))
        .unwrap_or_default();
    let stale = stale_age(battery, now_ms)
        .map(|a| format!(", no update for {a}"))
        .unwrap_or_default();
    format!("battery {level}%{charge}{stale}")
}

/// The status and watch line: `<name>[ (not yet tested through the receiver)]: <presence>, <battery>`.
#[must_use]
pub fn device_line(device: &DeviceInfo, now_ms: u64) -> String {
    let battery = battery_phrase(&device.battery, now_ms);
    let hint = (device.product_id == 0).then(|| format!("; {UNNAMED}"));
    let hint = hint.unwrap_or_default();
    format!(
        "{}: {}, {battery}{hint}",
        label(device),
        presence(device.presence)
    )
}

/// The battery command's line: `<name>: N%[ charging| full][ (<age> old)]`.
#[must_use]
pub fn battery_line(device: &DeviceInfo, now_ms: u64) -> String {
    let battery = &device.battery;
    let Some(level) = battery.level else {
        return format!("{}: {}", name(device), no_level(battery));
    };
    let charge = match (charge_word(battery), battery.unreported) {
        (Some(w), _) => format!(" {w}"),
        (None, Some(Unreported::Charge)) => " (charging not reported)".into(),
        _ => String::new(),
    };
    let stale = stale_age(battery, now_ms)
        .map(|a| format!(" ({a} old)"))
        .unwrap_or_default();
    format!("{}: {level}%{charge}{stale}", name(device))
}

/// "live", "retrying (serial read not answered)" and so on.
fn openrazer_phrase(o: &OpenRazerDevice) -> String {
    let state = match o.state {
        OpenRazerState::Live => "live",
        OpenRazerState::Cable => "on its cable, where OpenRazer has the wired device",
        OpenRazerState::Starting => "starting",
        OpenRazerState::Retrying => "retrying",
        OpenRazerState::Blocked => "blocked for 5 minutes",
        OpenRazerState::NotOffered => "not offered",
    };
    let why = o.reason.as_ref();
    why.map_or_else(|| state.to_owned(), |why| format!("{state} ({why})"))
}

/// The block `hyperfluxctl devices` prints for one device.
#[must_use]
pub fn device_block(receiver: &ReceiverInfo, device: &DeviceInfo, now_ms: u64) -> String {
    // age_phrase rounds up to a minute, which reads wrong for a device
    // that answered a second ago.
    let ago = |t| {
        age(t, now_ms).map(|a| match a {
            0..60_000 => "less than a minute ago".to_owned(),
            a => format!("{} ago", age_phrase(a)),
        })
    };
    let seen =
        ago(device.last_seen_ms).map_or_else(|| "never seen".into(), |a| format!("last seen {a}"));
    let contact = match device.contact {
        Some(Contact::OnMat) => "on the mat",
        Some(Contact::OffMat) => "off the mat",
        None => "not reported",
    };
    let battery = &device.battery;
    let battery = match battery.level {
        None => no_level(battery).to_owned(),
        Some(level) => {
            let charge = charge_note(battery)
                .map(|w| format!(", {w}"))
                .unwrap_or_default();
            let read = ago(battery.updated_ms)
                .map(|a| format!(", read {a}"))
                .unwrap_or_default();
            let stale = if battery.stale { " (stale)" } else { "" };
            format!("{level}%{charge}{read}{stale}")
        }
    };
    let control = match (device.controllable, &device.controlled_by) {
        (_, Some(holder)) => format!("held by {holder}"),
        (true, None) => "available, not in use".into(),
        (false, None) => "not available".into(),
    };
    // A catalog device the receiver put on the other lane.
    let odd = match (device.kind, device.lane) {
        (DeviceKind::Mouse, Lane::Keyboard) => ", not the usual one for a mouse",
        (DeviceKind::Keyboard, Lane::Pointer) => ", not the usual one for a keyboard",
        _ => "",
    };
    let lane = format!("{}{odd}", lane_word(device.lane));
    let support = match device.support {
        Support::Tested => "tested on hardware",
        Support::Checked => {
            "checked against Razer, OpenRazer and OpenRGB; not yet tested through the receiver"
        }
        Support::Listed => "listed by Razer, not usable yet",
        Support::Unknown if device.product_id == 0 => UNNAMED,
        Support::Unknown => "not in the device table",
    };
    let ids = format!(
        "{} ({:04x}:{:04x})",
        device.device_id, device.vendor_id, device.product_id
    );
    let inventory = receiver.inventory.map(inventory_phrase);
    let inventory = inventory.map(|i| format!(", {i}")).unwrap_or_default();
    let on = format!(
        "{}, generation {}{inventory}",
        receiver.receiver_id, receiver.generation_id
    );
    let presence = presence(device.presence);
    let ms = |us: u16| f64::from(us) / 1000.0;
    let timing = device.first_get.map_or_else(String::new, |w| {
        let (now, from) = (ms(w.delay_us), ms(w.learned_from_us));
        let kept = w.saved_us.map(|s| format!(", resumed at {:.1}", ms(s)));
        let kept = kept.unwrap_or_default();
        format!("  timing: wait {now:.1} ms (learned from {from:.1}{kept})\n")
    });
    let slot = device.slot.map_or("", |slot| match slot {
        Slot::Connected => "  slot: connected through the receiver\n",
        Slot::NotConnected => "  slot: not connected through the receiver (off, or on its cable)\n",
    });
    let openrazer = device.openrazer.as_ref().map(openrazer_phrase);
    let openrazer = openrazer
        .map(|o| format!("  openrazer: {o}\n"))
        .unwrap_or_default();
    let exchanges = device.exchanges.map_or_else(String::new, |e| {
        format!(
            "  exchanges: {} since the bridge started, {} answered at the first read, {} not at all; \
             read again {} times after another reply, {} after busy\n",
            e.sent, e.first_read, e.no_answer, e.reread_other, e.reread_busy
        )
    });
    format!(
        "{}\n  id: {ids}\n  receiver: {on}\n  lane: {lane}\n  support: {support}\n  \
         presence: {presence}, {seen}\n  contact: {contact}\n  battery: {battery}\n  control: {control}\n\
         {slot}{openrazer}{timing}{exchanges}",
        name(device)
    )
}

/// The devices of a snapshot with their receivers, in snapshot order.
pub fn devices(snapshot: &Snapshot) -> impl Iterator<Item = (&ReceiverInfo, &DeviceInfo)> {
    let all = snapshot.receivers.iter();
    all.flat_map(|r| r.devices.iter().map(move |d| (r, d)))
}

/// The JSON shape all the commands use for a device.
#[must_use]
pub fn device_json(receiver: &ReceiverInfo, device: &DeviceInfo, now_ms: u64) -> Value {
    let battery = &device.battery;
    let discharging = (battery.charge == Some(Charge::Discharging)).then_some("discharging");
    json!({
        "receiver_id": receiver.receiver_id,
        "generation_id": receiver.generation_id.get(),
        "device_id": device.device_id,
        "name": name(device),
        "vendor_id": device.vendor_id,
        "product_id": device.product_id,
        "kind": device.kind,
        "lane": device.lane,
        "support": device.support,
        "presence": device.presence,
        "last_seen_age_ms": age(device.last_seen_ms, now_ms),
        "contact": device.contact,
        "battery_level": battery.level,
        "battery_charge": charge_word(battery).or(discharging),
        "battery_age_ms": age(battery.updated_ms, now_ms),
        "battery_stale": battery.stale,
        "battery_rising": battery.rising,
        "battery_unreported": battery.unreported,
        "controllable": device.controllable,
        "controlled_by": device.controlled_by,
        "first_get": device.first_get,
        "refused": device.refused,
        "slot": device.slot,
        "openrazer": device.openrazer,
        "exchanges": device.exchanges,
        "line": device_line(device, now_ms),
    })
}

/// `{"text","tooltip","class","percentage"}` for a waybar custom module.
#[derive(Serialize)]
pub struct Waybar {
    pub text: String,
    pub tooltip: String,
    pub class: &'static str,
    pub percentage: u8,
}

/// The text lists the levels in snapshot order. Percentage and class
/// follow the emptiest battery, since that's the one to act on.
#[must_use]
pub fn waybar(snapshot: &Snapshot) -> Waybar {
    let now = snapshot.now_ms.get();
    let all: Vec<&DeviceInfo> = devices(snapshot).map(|(_, d)| d).collect();
    let tooltip = all
        .iter()
        .map(|d| battery_line(d, now))
        .collect::<Vec<_>>()
        .join("\n");
    let levels: Vec<(u8, &DeviceInfo)> = all
        .iter()
        .filter_map(|d| Some((d.battery.level?.get(), *d)))
        .collect();
    let Some(&(level, lowest)) = levels.iter().min_by_key(|(level, _)| *level) else {
        let tooltip = if tooltip.is_empty() {
            "No paired devices".into()
        } else {
            tooltip
        };
        return Waybar {
            text: String::new(),
            tooltip,
            class: "unknown",
            percentage: 0,
        };
    };
    // No alarm while the level climbs, charging reported or not.
    let rising = lowest.battery.rising;
    let class = match level {
        _ if lowest.battery.charge == Some(Charge::Charging) => "charging",
        0..=10 if !rising => "critical",
        11..=20 if !rising => "low",
        _ if lowest.battery.stale => "stale",
        _ => "normal",
    };
    let text = levels
        .iter()
        .map(|(l, _)| format!("{l}%"))
        .collect::<Vec<_>>()
        .join(" ");
    Waybar {
        text,
        tooltip,
        class,
        percentage: level,
    }
}

/// What a bar shows when the bridge cannot be reached.
#[must_use]
pub fn waybar_unavailable(why: &str) -> Waybar {
    let tooltip = format!("HyperFlux Next V2: {why}");
    Waybar {
        text: String::new(),
        tooltip,
        class: "unavailable",
        percentage: 0,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use hfx_proto::ids::BatteryPercent;

    /// The protocol fixture snapshot, 42 minutes after the keyboard's last
    /// battery reading.
    pub fn snapshot() -> Snapshot {
        let text = include_str!("../../hfx-proto/tests/fixtures/snapshot-reply.json");
        let reply: Value = serde_json::from_str(text).unwrap();
        let mut snapshot: Snapshot = serde_json::from_value(reply["result"].clone()).unwrap();
        snapshot.now_ms = hfx_proto::ids::MonotonicMs::new(1_180_000 + 42 * 60_000).unwrap();
        snapshot
    }

    fn set(device: &mut DeviceInfo, level: Option<u8>, charge: Option<Charge>, stale: bool) {
        device.battery.level = level.map(|l| BatteryPercent::new(l).unwrap());
        device.battery.charge = charge;
        device.battery.stale = stale;
    }

    #[test]
    fn battery_wording() {
        let mut s = snapshot();
        let now = s.now_ms.get();
        let mouse = &mut s.receivers[0].devices[0];
        set(mouse, Some(100), Some(Charge::Charging), false);
        assert_eq!(
            battery_line(mouse, now),
            "Razer Basilisk V3 Pro 35K: 100% full"
        );
        assert_eq!(
            device_line(mouse, now),
            "Razer Basilisk V3 Pro 35K: online, battery 100%, full"
        );
        set(mouse, Some(0), Some(Charge::Discharging), false);
        assert_eq!(battery_line(mouse, now), "Razer Basilisk V3 Pro 35K: 0%");
        set(mouse, None, None, false);
        assert_eq!(
            battery_line(mouse, now),
            "Razer Basilisk V3 Pro 35K: not reported yet"
        );
        assert_eq!(
            device_line(mouse, now),
            "Razer Basilisk V3 Pro 35K: online, battery not reported yet"
        );
    }

    #[test]
    fn unknown_and_checked_devices() {
        let mut s = snapshot();
        let now = s.now_ms.get();
        let device = &mut s.receivers[0].devices[0];
        device.support = Support::Checked;
        device.presence = Presence::Waiting;
        let line = "Razer Basilisk V3 Pro 35K (not yet tested through the receiver): waiting for activity, battery 79%, charging";
        assert_eq!(device_line(device, now), line);
        device.name = None;
        device.product_id = 0x00ff;
        device.support = Support::Unknown;
        device.presence = Presence::Unreachable;
        let line = "Unknown device 1532:00ff: not reachable, battery 79%, charging";
        assert_eq!(device_line(device, now), line);
        // Without a way to tell, it says so.
        device.battery.charge = None;
        device.battery.unreported = Some(Unreported::Charge);
        let line = "Unknown device 1532:00ff: 79% (charging not reported)";
        assert_eq!(battery_line(device, now), line);
        set(device, None, None, false);
        device.battery.unreported = Some(Unreported::Battery);
        assert_eq!(
            battery_line(device, now),
            "Unknown device 1532:00ff: not reported"
        );
    }

    #[test]
    fn waybar_lowest_battery() {
        let mut s = snapshot();
        let bar = waybar(&s);
        assert_eq!(
            (bar.text.as_str(), bar.class, bar.percentage),
            ("79% 64%", "stale", 64)
        );
        set(&mut s.receivers[0].devices[1], Some(9), None, false);
        assert_eq!(waybar(&s).class, "critical");
        set(&mut s.receivers[0].devices[1], Some(15), None, false);
        assert_eq!(waybar(&s).class, "low");
        s.receivers[0].devices[1].battery.rising = true;
        assert_eq!(waybar(&s).class, "normal");
        set(&mut s.receivers[0].devices[1], None, None, false);
        let bar = waybar(&s);
        assert_eq!(
            (bar.text.as_str(), bar.class, bar.percentage),
            ("79%", "charging", 79)
        );
        s.receivers.clear();
        assert_eq!(waybar(&s).class, "unknown");
    }
}
