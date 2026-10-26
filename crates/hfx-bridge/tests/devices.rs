// SPDX-License-Identifier: GPL-2.0-only

//! Receivers and devices through the whole bridge: presence, routes,
//! batteries, probes, resume and replug.

mod common;

use common::{Harness, Options, charge, device, level, presence_of, until};
use hfx_proto::devices;
use hfx_proto::ids::{ExchangeTimeoutMs, LeaseDurationMs};
use hfx_proto::messages::{
    Charge, Contact, DeviceKind, Inventory, Lane, Presence, RouteState, Slot, Snapshot, Support,
    Transport, Unreported,
};
use hfx_proto::report;
use std::time::Duration;

const POINTER: Lane = Lane::Pointer;
const KEYBOARD: Lane = Lane::Keyboard;

fn timeout(ms: u16) -> ExchangeTimeoutMs {
    ExchangeTimeoutMs::new(ms).unwrap()
}

#[test]
fn batteries_probed_at_start() {
    let h = Harness::start();
    let mut c = h.client(&[]);
    let s = until(&mut c, 5, |s| {
        level(s, 0, POINTER) == Some(79) && level(s, 0, KEYBOARD) == Some(64)
    });
    assert_eq!(s.transport, Transport::Hidraw);
    let mouse = device(&s, 0, POINTER).unwrap();
    assert_eq!(
        mouse.name.as_ref().unwrap().as_str(),
        "Razer Basilisk V3 Pro 35K"
    );
    assert_eq!(
        (mouse.support, mouse.presence),
        (Support::Tested, Presence::Online)
    );
    // The probe also asked 07/84, which the mouse answers with 1 on its mat.
    let s = until(&mut c, 5, |s| {
        charge(s, 0, POINTER) == Some(Charge::Charging)
    });
    // A queried keyboard charge isn't believed. Status reports decide.
    assert_eq!(charge(&s, 0, KEYBOARD), None);
    common::wait_for(5, || {
        h.shown(0, POINTER).is_some_and(|b| b.charging) && h.shown(0, KEYBOARD).is_some()
    });
    let mouse = h.shown(0, POINTER).unwrap();
    assert_eq!(mouse.capacity, 79);
    assert!(
        mouse.name.ends_with("Basilisk V3 Pro 35K"),
        "{}",
        mouse.name
    );
    let keyboard = h.shown(0, KEYBOARD).unwrap();
    assert_eq!((keyboard.capacity, keyboard.charging), (64, false));
    // Probes are queries, and the receiver saw each lane's selector in byte 1.
    for (lane, selector) in [(POINTER, 0x1f), (KEYBOARD, 0x9f)] {
        let requests = h.sim.requests(0, lane);
        assert!(!requests.is_empty());
        assert!(
            requests
                .iter()
                .all(|r| report::is_query(r) && r[1] == selector)
        );
    }
}

#[test]
fn battery_device_kind_and_name() {
    let h = Harness::start();
    let mut c = h.client(&[]);
    until(&mut c, 5, |s| level(s, 0, KEYBOARD).is_some());
    common::wait_for(5, || h.batteries.lock().unwrap().len() == 2);
    let batteries = h.batteries.lock().unwrap().clone();
    let id = c.snapshot().unwrap().receivers[0]
        .receiver_id
        .as_str()
        .trim_start_matches("receiver-")
        .to_owned();
    let keyboard = &batteries[&format!("hyperflux-next/battery/{id}/keyboard")];
    assert_eq!(keyboard.name, "Razer DeathStalker V2 Pro TKL");
    assert!(keyboard.keyboard);
    assert_eq!(
        keyboard.vendor, 0,
        "not Razer's id, which the patched drivers claim"
    );
    assert_eq!(keyboard.uniq, format!("hyperflux-{id}-keyboard"));
    assert!(!batteries[&format!("hyperflux-next/battery/{id}/pointer")].keyboard);
    drop(batteries);
    // Unpairing removes the device's battery; stopping removes the rest.
    h.sim.identity(0, POINTER, 0x00ff, 1);
    common::wait_for(5, || h.batteries.lock().unwrap().len() == 1);
    let batteries = h.batteries.clone();
    h.stop().unwrap();
    common::wait_for(5, || batteries.lock().unwrap().is_empty());
}

#[test]
fn early_battery_report_resent() {
    let h = Harness::start();
    let mut c = h.client(&[]);
    until(&mut c, 5, |s| level(s, 0, KEYBOARD) == Some(64));
    // The report sent right on UHID_START arrives before the simulated
    // kernel has the power supply, like on hardware, and is dropped.
    common::wait_for(2, || h.shown(0, KEYBOARD).is_some());
    let id = c.snapshot().unwrap().receivers[0]
        .receiver_id
        .as_str()
        .trim_start_matches("receiver-")
        .to_owned();
    let phys = format!("hyperflux-next/battery/{id}/keyboard");
    std::thread::sleep(Duration::from_millis(2500));
    let keyboard = h.batteries.lock().unwrap()[&phys].clone();
    assert_eq!(keyboard.level, Some(64));
    assert_eq!(
        keyboard.reports + keyboard.dropped,
        4,
        "one report on start and three more, then quiet: {keyboard:?}"
    );
}

#[test]
fn status_reports() {
    let h = Harness::start();
    let mut c = h.client(&[]);
    // Let the start-up probes finish: a reply the simulator gave before one
    // of the status reports below could otherwise be read after it, and the
    // older reading would win.
    until(&mut c, 5, |s| {
        level(s, 0, KEYBOARD).is_some() && charge(s, 0, POINTER) == Some(Charge::Charging)
    });
    // Off the mat and not charging reads as discharging.
    h.sim.status(0, POINTER, 202, 0, 0);
    let s = until(&mut c, 5, |s| {
        charge(s, 0, POINTER) == Some(Charge::Discharging)
    });
    let mouse = device(&s, 0, POINTER).unwrap();
    assert_eq!(mouse.battery.level.unwrap().get(), 79);
    assert_eq!(mouse.contact, Some(Contact::OffMat));
    common::wait_for(5, || h.shown(0, POINTER).is_some_and(|b| !b.charging));
    // On the cable the charge byte lags. 6/0 is unknown, not discharging.
    h.sim.status(0, POINTER, 210, 6, 0);
    let s = until(&mut c, 5, |s| level(s, 0, POINTER) == Some(82));
    assert_eq!(charge(&s, 0, POINTER), None);
    assert_eq!(device(&s, 0, POINTER).unwrap().contact, None);
    // A full keyboard on its cable is charging at 100 %.
    h.sim.status(0, KEYBOARD, 255, 1, 1);
    let s = until(&mut c, 5, |s| level(s, 0, KEYBOARD) == Some(100));
    assert_eq!(charge(&s, 0, KEYBOARD), Some(Charge::Charging));
    common::wait_for(5, || {
        h.shown(0, KEYBOARD)
            .is_some_and(|b| b.capacity == 100 && b.charging)
    });
    h.sim.status(0, KEYBOARD, 250, 0, 0);
    let s = until(&mut c, 5, |s| level(s, 0, KEYBOARD) == Some(98));
    assert_eq!(charge(&s, 0, KEYBOARD), Some(Charge::Discharging));
}

#[test]
fn presence() {
    let h = Harness::start();
    let mut c = h.client(&["native-routes"]);
    let token = common::token(&mut c, POINTER);
    // A device that answers 0x04 is asleep; its route stays available.
    h.sim.set_asleep(0, POINTER, true);
    let lease = common::lease(&mut c, &token);
    let request = report::build(0x1f, 0x00, 0x81, &[0, 0]);
    let result = c.transact(&lease, &token, &request, timeout(1000)).unwrap();
    assert_eq!(result.response.unwrap().bytes()[0], report::STATUS_TIMEOUT);
    let s = until(&mut c, 5, |s| {
        presence_of(s, 0, POINTER) == Some(Presence::Asleep)
    });
    assert!(device(&s, 0, POINTER).unwrap().controllable);
    h.sim.set_asleep(0, POINTER, false);
    h.sim.activity(0, POINTER);
    until(&mut c, 5, |s| {
        presence_of(s, 0, POINTER) == Some(Presence::Online)
    });
    // A keyboard identity with route byte 2 means the receiver lost it.
    h.sim.identity(0, KEYBOARD, 0x0296, 2);
    let s = until(&mut c, 5, |s| {
        presence_of(s, 0, KEYBOARD) == Some(Presence::Unreachable)
    });
    assert!(!device(&s, 0, KEYBOARD).unwrap().controllable);
    let keyboard = common::route(&mut c, KEYBOARD);
    assert_eq!(
        (keyboard.state, keyboard.route_token),
        (RouteState::Unavailable, None)
    );
    h.sim.identity(0, KEYBOARD, 0x0296, 0);
    until(&mut c, 5, |s| device(s, 0, KEYBOARD).unwrap().controllable);
}

#[test]
fn resume_probes_again() {
    let h = Harness::start();
    let mut c = h.client(&["native-routes"]);
    let old = common::token(&mut c, POINTER);
    common::token(&mut c, KEYBOARD);
    let asked = h.sim.requests(0, POINTER).len();
    h.sleep_and_wake();
    // Whatever the receivers said before is old news until they answer.
    let s = until(&mut c, 5, |s| {
        presence_of(s, 0, POINTER) == Some(Presence::Waiting)
    });
    assert!(
        s.receivers[0]
            .devices
            .iter()
            .all(|d| !d.controllable && d.battery.level.is_some())
    );
    let s = until(&mut c, 5, |s| device(s, 0, POINTER).unwrap().controllable);
    assert_eq!(device(&s, 0, POINTER).unwrap().presence, Presence::Online);
    assert!(h.sim.requests(0, POINTER).len() > asked);
    assert_ne!(
        common::route(&mut c, POINTER).route_token,
        Some(old.clone())
    );
    let error = c
        .acquire_lease(&old, LeaseDurationMs::new(1000).unwrap())
        .unwrap_err();
    assert_eq!(error.code(), Some("HFX-109"));
}

#[test]
fn replug_is_new_generation() {
    let h = Harness::start();
    let mut c = h.client(&["native-routes"]);
    let token = common::token(&mut c, POINTER);
    common::wait_for(5, || h.shown(0, POINTER).is_some());
    let s = c.snapshot().unwrap();
    let (id, generation) = (
        s.receivers[0].receiver_id.clone(),
        s.receivers[0].generation_id,
    );
    h.sim.unplug(0);
    until(&mut c, 5, |s| s.receivers.is_empty());
    assert!(c.list_routes().unwrap().routes.is_empty());
    common::wait_for(5, || h.shown(0, POINTER).is_none());
    h.sim.plug(0);
    let s = until(&mut c, 5, |s| {
        s.receivers.len() == 1 && level(s, 0, POINTER).is_some()
    });
    assert_eq!(
        s.receivers[0].receiver_id, id,
        "the receiver id belongs to the port"
    );
    assert!(s.receivers[0].generation_id > generation);
    let error = c
        .acquire_lease(&token, LeaseDurationMs::new(1000).unwrap())
        .unwrap_err();
    assert_eq!(error.code(), Some("HFX-107"));
    common::wait_for(5, || h.shown(0, POINTER).is_some());
}

/// Unplugs the receiver and plugs it back in with nodes udev hasn't opened
/// to the bridge yet, under the same names as before.
fn replug_without_access(h: &Harness, c: &mut hfx_proto::client::Client) {
    until(c, 5, |s| level(s, 0, POINTER).is_some());
    h.sim.unplug(0);
    until(c, 5, |s| s.receivers.is_empty());
    h.sim.plug_without_access(0);
}

#[test]
fn replug_waits_for_udev() {
    let h = Harness::start();
    let mut c = h.client(&[]);
    replug_without_access(&h, &mut c);
    // Past the bridge's own retries: from here it needs udev's event.
    std::thread::sleep(Duration::from_millis(3000));
    assert!(c.snapshot().unwrap().receivers.is_empty());
    h.sim.grant_access(0, true);
    let s = until(&mut c, 2, |s| {
        s.receivers.len() == 1 && level(s, 0, POINTER).is_some()
    });
    assert!(s.receivers[0].writable);
    common::wait_for(2, || h.shown(0, POINTER).is_some());
}

#[test]
fn missed_udev_event_retry() {
    let h = Harness::start();
    let mut c = h.client(&[]);
    replug_without_access(&h, &mut c);
    std::thread::sleep(Duration::from_millis(300));
    h.sim.grant_access(0, false);
    until(&mut c, 3, |s| {
        s.receivers.len() == 1 && level(s, 0, POINTER).is_some()
    });
}

#[test]
fn new_device_on_lane() {
    let h = Harness::start();
    let mut c = h.client(&["native-routes"]);
    let keyboard = common::token(&mut c, KEYBOARD);
    let mouse = common::token(&mut c, POINTER);
    let lease = common::lease(&mut c, &mouse);
    common::wait_for(5, || h.shown(0, POINTER).is_some());
    // An unknown mouse pairs: kind unknown, no name, no route token.
    h.sim.identity(0, POINTER, 0x00ff, 1);
    let s = until(&mut c, 5, |s| {
        device(s, 0, POINTER).is_some_and(|d| d.product_id == 0x00ff)
    });
    let unknown = device(&s, 0, POINTER).unwrap();
    assert_eq!(unknown.device_id.as_str(), "pointer-00ff");
    assert_eq!(
        (unknown.support, unknown.name.as_ref()),
        (Support::Unknown, None)
    );
    assert!(!unknown.controllable && unknown.battery.level.is_none());
    // The old device's battery went with it.
    common::wait_for(5, || h.shown(0, POINTER).is_none());
    let battery = report::build(0x1f, 0x07, 0x80, &[0, 0]);
    let result = c.transact(&lease, &mouse, &battery, timeout(100)).unwrap();
    assert_eq!(result.code.unwrap().as_str(), "HFX-109");
    // The keyboard didn't notice.
    assert_eq!(common::route(&mut c, KEYBOARD).route_token, Some(keyboard));
    // A known keyboard announced on the pointer lane is still that keyboard,
    // without a status decoder or mat contact there.
    h.sim.identity(0, POINTER, 0x0296, 1);
    let s = until(&mut c, 5, |s| {
        device(s, 0, POINTER).is_some_and(|d| d.product_id == 0x0296 && d.controllable)
    });
    let odd = device(&s, 0, POINTER).unwrap();
    let what = (odd.kind, odd.support, odd.contact);
    assert_eq!(what, (DeviceKind::Keyboard, Support::Tested, None));
    assert_eq!(odd.battery.unreported, None, "07/84 still tells");
}

/// The simulator with `pid` on the keyboard lane, its status bytes set and
/// these commands answered with "not supported".
fn keyboard(pid: u16, power: u8, charge: u8, unsupported: &'static [(u8, u8)]) -> Harness {
    let options = Options::before(move |sim| {
        sim.identity(0, KEYBOARD, pid, 1);
        sim.status(0, KEYBOARD, 202, power, charge);
        sim.set_unsupported(0, KEYBOARD, unsupported);
    });
    Harness::with(options).0
}

fn battery_queries(h: &Harness, command: u8) -> usize {
    let requests = h.sim.requests(0, KEYBOARD);
    requests
        .iter()
        .filter(|r| r[6..8] == [0x07, command])
        .count()
}

#[test]
fn checked_keyboard_charging() {
    // The Low Profile TKL's list has no 07/84, but it is asked anyway. Any
    // answer but 0 is charging, as OpenRazer reads it: 6 is the 35K's on its
    // mat.
    let h = keyboard(0x02d2, 1, 6, &[]);
    let mut c = h.client(&[]);
    let s = until(&mut c, 5, |s| {
        charge(s, 0, KEYBOARD) == Some(Charge::Charging)
    });
    assert_eq!(level(&s, 0, KEYBOARD), Some(79));
    common::wait_for(5, || h.shown(0, KEYBOARD).is_some_and(|b| b.charging));
    // Its status bytes mean nothing to the bridge, so they don't undo it.
    h.sim.status(0, KEYBOARD, 200, 0, 0);
    let s = until(&mut c, 5, |s| level(s, 0, KEYBOARD) == Some(78));
    assert_eq!(charge(&s, 0, KEYBOARD), Some(Charge::Charging));
}

#[test]
fn battery_unsupported_asked_once() {
    // The Low Profile TKL's list has no battery query. It is asked anyway.
    let h = keyboard(0x02d2, 0, 0, &[(0x07, 0x80)]);
    let mut c = h.client(&[]);
    let unreported = |s: &Snapshot| device(s, 0, KEYBOARD).and_then(|d| d.battery.unreported);
    until(&mut c, 5, |s| unreported(s) == Some(Unreported::Battery));
    h.sleep_and_wake();
    until(&mut c, 5, |s| {
        presence_of(s, 0, KEYBOARD) == Some(Presence::Waiting)
    });
    h.sim.activity(0, KEYBOARD);
    until(&mut c, 5, |s| {
        presence_of(s, 0, KEYBOARD) == Some(Presence::Online)
    });
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(battery_queries(&h, 0x80), 1);
    // Its status reports still give the level, but nothing says whether it
    // charges.
    h.sim.status(0, KEYBOARD, 128, 1, 1);
    let s = until(&mut c, 5, |s| level(s, 0, KEYBOARD) == Some(50));
    assert_eq!(unreported(&s), Some(Unreported::Charge));
}

#[test]
fn charging_unsupported_asked_once() {
    // "Not supported" ends the question until the device pairs again. A
    // resume doesn't.
    let h = keyboard(0x02d2, 0, 0, &[(0x07, 0x84)]);
    let mut c = h.client(&[]);
    let unreported = |s: &Snapshot| device(s, 0, KEYBOARD).and_then(|d| d.battery.unreported);
    until(&mut c, 5, |s| unreported(s) == Some(Unreported::Charge));
    h.sleep_and_wake();
    until(&mut c, 5, |s| {
        presence_of(s, 0, KEYBOARD) == Some(Presence::Waiting)
    });
    h.sim.activity(0, KEYBOARD);
    common::wait_for(5, || battery_queries(&h, 0x80) == 2);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(battery_queries(&h, 0x84), 1);
}

#[test]
fn gone_keyboard_asked_first() {
    let h = keyboard(0x0290, 0, 0, &[]);
    let mut c = h.client(&[]);
    until(&mut c, 5, |s| level(s, 0, KEYBOARD).is_some());
    let presence = |s: &Snapshot| presence_of(s, 0, KEYBOARD);
    // It answers: the route byte didn't mean gone for this keyboard.
    let asked = battery_queries(&h, 0x80);
    h.sim.identity(0, KEYBOARD, 0x0290, 2);
    common::wait_for(5, || battery_queries(&h, 0x80) > asked);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(presence(&c.snapshot().unwrap()), Some(Presence::Online));
    // No answer: it is gone.
    h.sim.set_silent(0, KEYBOARD, 1);
    h.sim.identity(0, KEYBOARD, 0x0290, 2);
    until(&mut c, 5, |s| presence(s) == Some(Presence::Unreachable));
}

#[test]
fn slots_name_the_devices() {
    // Even without an inventory the bridge can read.
    let h = Harness::with(Options::before(|sim| sim.set_inventory_marker(0, None))).0;
    let mut c = h.client(&[]);
    let s = until(&mut c, 5, |s| {
        s.receivers.first().is_some_and(|r| r.devices.len() == 2)
    });
    assert_eq!(s.receivers[0].inventory, Some(Inventory::Slots));
    assert_eq!(device(&s, 0, POINTER).unwrap().slot, Some(Slot::Connected));
    let asked = h.sim.unrouted(0);
    assert!(asked.len() == 1 && asked[0] == devices::receiver_probe(0).unwrap());
    // Switched off: after a resume the slot says so.
    h.sim.set_asleep(0, KEYBOARD, true);
    h.sleep_and_wake();
    h.sim.activity(0, POINTER);
    let slot = |s: &Snapshot| device(s, 0, KEYBOARD).and_then(|d| d.slot);
    until(&mut c, 5, |s| slot(s) == Some(Slot::NotConnected));
    // Back on, it shows life, and the slots are read again.
    h.sim.set_asleep(0, KEYBOARD, false);
    h.sim.activity(0, KEYBOARD);
    until(&mut c, 5, |s| slot(s) == Some(Slot::Connected));
}

#[test]
fn inventory_layouts() {
    let with_marker = |marker| {
        let options = Options::before(move |sim| {
            sim.set_slots(0, false);
            sim.set_inventory_marker(0, marker);
        });
        let h = Harness::with(options).0;
        let mut c = h.client(&[]);
        let s = until(&mut c, 5, |s| !s.receivers.is_empty());
        (h, c, s.receivers[0].inventory)
    };
    // Moved, the marker is still found.
    let (_h, mut c, found) = with_marker(Some(100));
    let s = until(&mut c, 5, |s| s.receivers[0].devices.len() == 2);
    assert_eq!(
        (found, s.receivers[0].inventory),
        (Some(Inventory::Marker), found)
    );
    // Without one, a status report shows for a stand-in until the receiver
    // names the device, which then takes over the battery.
    let (h, mut c, found) = with_marker(None);
    assert_eq!(found, Some(Inventory::NotRecognised));
    h.sim.status(0, POINTER, 202, 1, 1);
    let s = until(&mut c, 5, |s| level(s, 0, POINTER) == Some(79));
    let stand_in = device(&s, 0, POINTER).unwrap();
    let what = (stand_in.product_id, stand_in.kind, stand_in.controllable);
    assert_eq!(what, (0, DeviceKind::Unknown, false));
    let shown = |name| h.shown(0, POINTER).is_some_and(|b| b.name == name);
    common::wait_for(5, || shown("Unidentified pointer device"));
    h.sim.identity(0, POINTER, 0x00cd, 1);
    common::wait_for(5, || shown("Razer Basilisk V3 Pro 35K"));
}

#[test]
fn read_only_sends_nothing() {
    let (h, _) = Harness::with(Options {
        read_only: true,
        ..Options::default()
    });
    let mut c = h.client(&[]);
    // Admitted means the bridge takes the keeper's reports: none is lost.
    until(&mut c, 5, |s| s.receivers.iter().any(|r| !r.writable));
    h.sim.status(0, POINTER, 202, 1, 1);
    until(&mut c, 5, |s| level(s, 0, POINTER) == Some(79));
    std::thread::sleep(Duration::from_millis(50));
    assert!(h.sim.requests(0, POINTER).is_empty() && h.shown(0, POINTER).is_none());
}

#[test]
fn two_receivers() {
    let (h, _) = Harness::with(Options {
        receivers: 2,
        ..Options::default()
    });
    let mut c = h.client(&["native-routes"]);
    let s = until(&mut c, 5, |s| {
        s.receivers.len() == 2
            && (s.receivers.iter())
                .all(|r| r.devices.len() == 2 && r.devices.iter().all(|d| d.controllable))
    });
    assert!(s.receivers[0].receiver_id < s.receivers[1].receiver_id);
    let routes = c.list_routes().unwrap().routes;
    let tokens: std::collections::BTreeSet<_> = routes
        .iter()
        .filter_map(|r| r.route_token.clone())
        .collect();
    assert_eq!((routes.len(), tokens.len()), (4, 4));
    for i in 0..2 {
        common::wait_for(5, || {
            h.shown(i, POINTER).is_some() && h.shown(i, KEYBOARD).is_some()
        });
    }
}

#[test]
fn refused_battery_retried() {
    let (h, _) = Harness::with(Options {
        refuse_uhid: 2,
        ..Options::default()
    });
    let mut c = h.client(&[]);
    until(&mut c, 5, |s| level(s, 0, POINTER).is_some());
    // Refused twice, then accepted on the 1 s retry (or the next reading).
    common::wait_for(4, || {
        h.shown(0, POINTER).is_some() && h.shown(0, KEYBOARD).is_some()
    });
}

#[test]
fn probe_retries() {
    let (h, _) = Harness::with(Options::before(|sim| {
        sim.set_silent(0, POINTER, 1);
        sim.set_asleep(0, KEYBOARD, true);
    }));
    let mut c = h.client(&[]);
    // The pointer's first probe gets no answer; the retry 2 s later does.
    let s = until(&mut c, 5, |s| level(s, 0, POINTER) == Some(79));
    assert_eq!(device(&s, 0, KEYBOARD).unwrap().presence, Presence::Asleep);
    let asked = h.sim.requests(0, KEYBOARD).len();
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(
        h.sim.requests(0, KEYBOARD).len(),
        asked,
        "an asleep device isn't asked again"
    );
    h.sim.set_asleep(0, KEYBOARD, false);
    h.sim.activity(0, KEYBOARD);
    until(&mut c, 5, |s| level(s, 0, KEYBOARD) == Some(64));
}

#[test]
fn other_lane_reply_ignored() {
    let h = Harness::start();
    let mut c = h.client(&["native-routes"]);
    let token = common::token(&mut c, POINTER);
    until(&mut c, 5, |s| level(s, 0, POINTER) == Some(79));
    h.sim.status(0, POINTER, 202, 1, 1);
    let lease = common::lease(&mut c, &token);
    h.sim.misroute(0, POINTER, 1);
    let battery = report::build(0x1f, 0x07, 0x80, &[0, 0]);
    let result = c.transact(&lease, &token, &battery, timeout(1000)).unwrap();
    assert_eq!(result.code.unwrap().as_str(), "HFX-110");
    let s = c.snapshot().unwrap();
    assert_eq!(level(&s, 0, POINTER), Some(79));
    // The next one is answered properly, with the caller's own id back.
    let battery = report::build(0x3f, 0x07, 0x80, &[0, 0]);
    let result = c.transact(&lease, &token, &battery, timeout(1000)).unwrap();
    assert_eq!(result.response.unwrap().bytes()[1], 0x3f);
    assert_eq!(h.sim.requests(0, POINTER).last().unwrap()[1], 0x1f);
}
