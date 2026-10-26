// SPDX-License-Identifier: GPL-2.0-only

//! The mouse going on and off its mat, which the receiver never reports:
//! the bridge finds out by asking when the pointer starts or stops moving.
//! These run in real time, so they print what they measured.

mod common;

use common::{Harness, Options, charge, presence_of, until, wait_for};
use hfx_proto::ids::ExchangeTimeoutMs;
use hfx_proto::messages::{Lane, Presence};
use hfx_proto::report;
use std::thread;
use std::time::{Duration, Instant};

const POINTER: Lane = Lane::Pointer;

/// The mouse moves, a thousand reports a second out of interface 0, a
/// little more than the tested receiver sends. Returns when it stopped
/// and the longest gap between two reports.
fn move_mouse(h: &Harness, how_long: Duration) -> (Instant, Duration) {
    h.sim.set_motion(0, 1000);
    thread::sleep(how_long);
    let gap = h.sim.set_motion(0, 0);
    (Instant::now(), gap)
}

/// The time since `from`, printed as well.
fn measured(what: &str, from: Instant) -> Duration {
    let took = from.elapsed();
    eprintln!("{what}: {} ms", took.as_millis());
    took
}

fn asked(h: &Harness) -> usize {
    h.sim.requests(0, POINTER).len()
}

fn charging(h: &Harness) -> bool {
    h.shown(0, POINTER).is_some_and(|b| b.charging)
}

/// Charging on the mat, at rest long enough that the next movement counts
/// as a lift.
fn resting_on_the_mat() -> Harness {
    let h = Harness::start();
    wait_for(5, || charging(&h));
    thread::sleep(Duration::from_millis(5_200));
    h
}

/// `pid` on the pointer lane, off the mat.
fn off_the_mat(pid: u16) -> Harness {
    let (h, _) = Harness::with(Options::before(move |sim| {
        sim.identity(0, POINTER, pid, 1);
        sim.mat(0, false, Duration::ZERO);
    }));
    wait_for(5, || h.shown(0, POINTER).is_some_and(|b| !b.charging));
    h
}

/// Picked up, moved and put down. Each of the keeper's wake-ups empties the
/// pointer's input, which is then left alone for 100 ms. The movement
/// costs it about ten a second, and the bridge one for each activity event,
/// four a second at most.
#[test]
fn lift_and_put_down() {
    let pause = Duration::from_millis(hfx_bridge::state::POINTER_PAUSE_MS);
    for _ in 0..3 {
        let h = resting_on_the_mat();
        let before = asked(&h);
        h.sim.mat(0, false, Duration::ZERO);
        let woke = h.wakeups();
        let (stopped, gap) = move_mouse(&h, Duration::from_secs(4));
        let (woke, quiet) = (h.wakeups() - woke, asked(&h) == before);
        if gap >= pause {
            eprintln!("the test stood still for {} ms; again", gap.as_millis());
            continue;
        }
        eprintln!("woke {woke} times in 4 s of movement");
        assert!(woke <= 64, "{woke} wake-ups");
        assert!(quiet, "asked while the mouse moved");
        wait_for(3, || !charging(&h));
        measured("lift-off seen, after the mouse stopped", stopped);
        // Not charging isn't discharging: the query can't tell.
        let s = h.client(&[]).snapshot().unwrap();
        assert_eq!(charge(&s, 0, POINTER), None);
        h.sim.mat(0, true, Duration::ZERO);
        wait_for(5, || charging(&h));
        let took = measured("put-down seen", stopped);
        assert!(took < Duration::from_millis(4_500), "{took:?}");
        return;
    }
    panic!("the test stood still in every run");
}

/// A checked mouse whose list has 07:84 is watched like the 35K above.
/// Seen in the simulator, not on hardware.
#[test]
fn checked_mouse_put_down() {
    let h = off_the_mat(0x00ab);
    let (placed, _) = move_mouse(&h, Duration::from_millis(1_000));
    h.sim.mat(0, true, Duration::ZERO);
    wait_for(6, || charging(&h));
    let took = measured("put-down seen", placed);
    assert!(
        (Duration::from_millis(2_500)..Duration::from_secs(4)).contains(&took),
        "{took:?}"
    );
}

#[test]
fn slow_charging_status_rechecked() {
    let h = off_the_mat(0x00cd);
    let (placed, _) = move_mouse(&h, Duration::from_millis(1_000));
    // The mouse's first status on the mat still reads not charging.
    h.sim.mat(0, true, Duration::from_secs(5));
    let before = asked(&h);
    thread::sleep(Duration::from_millis(4_000));
    assert_eq!(asked(&h), before + 2, "07/80 and 07/84 once, 3 s after");
    assert!(!charging(&h));
    wait_for(17, || charging(&h));
    let took = measured("put-down with a slow first status seen", placed);
    assert!(
        (Duration::from_secs(17)..Duration::from_millis(19_500)).contains(&took),
        "{took:?}"
    );
}

#[test]
fn no_query_while_moving() {
    // When the whole test stops for a moment, the simulated mouse stops
    // too, and the bridge rightly asks. Such a run proves nothing, and we
    // go again.
    let pause = Duration::from_millis(hfx_bridge::state::POINTER_PAUSE_MS);
    for _ in 0..3 {
        let h = resting_on_the_mat();
        let before = asked(&h);
        h.sim.mat(0, false, Duration::ZERO);
        // Longer than anyone would wait for a pause.
        let (stopped, gap) = move_mouse(&h, Duration::from_secs(12));
        let quiet = asked(&h) == before;
        let gap = gap.max(stopped.elapsed());
        if gap >= pause {
            eprintln!("the test stood still for {} ms; again", gap.as_millis());
            continue;
        }
        assert!(quiet, "asked while the mouse moved");
        wait_for(3, || asked(&h) > before);
        let took = measured("asked after the mouse stopped", stopped);
        assert!(took >= pause, "{took:?}");
        wait_for(2, || !charging(&h));
        return;
    }
    panic!("the test stood still in every run");
}

#[test]
fn asleep_mouse_not_asked() {
    let h = Harness::start();
    let mut c = h.client(&["native-routes"]);
    let token = common::token(&mut c, POINTER);
    // Its put-down check would be due 3 s after this.
    move_mouse(&h, Duration::from_millis(500));
    // A real mouse falls asleep long after it stops. The keeper's last
    // activity event trails the movement by up to 350 ms, more on a busy
    // machine, and the bridge rightly takes a late one for waking up.
    thread::sleep(Duration::from_millis(1_000));
    h.sim.set_asleep(0, POINTER, true);
    let lease = common::lease(&mut c, &token);
    let request = report::build(0x1f, 0x00, 0x81, &[0, 0]);
    let timeout = ExchangeTimeoutMs::new(1000).unwrap();
    c.transact(&lease, &token, &request, timeout).unwrap();
    c.release_lease(&lease).unwrap();
    until(&mut c, 5, |s| {
        presence_of(s, 0, POINTER) == Some(Presence::Asleep)
    });
    let before = asked(&h);
    thread::sleep(Duration::from_millis(4_000));
    assert_eq!(asked(&h), before, "an asleep mouse was asked");
}

#[test]
fn app_lease_defers_the_check() {
    let h = Harness::start();
    let (mut c, _, lease) = common::leased(&h, &["native-routes"], POINTER);
    let before = asked(&h);
    h.sim.mat(0, false, Duration::ZERO);
    move_mouse(&h, Duration::from_millis(500));
    // Past the put-down check, which found the app holding the mouse.
    thread::sleep(Duration::from_millis(3_500));
    assert_eq!(asked(&h), before, "asked while an app held the mouse");
    assert!(charging(&h));
    c.release_lease(&lease).unwrap();
    let released = Instant::now();
    wait_for(3, || !charging(&h));
    measured("asked after the app let go", released);
}
