// SPDX-License-Identifier: GPL-2.0-only

//! When to ask a mouse whether it is charging.
//!
//! The receiver doesn't say when the mouse goes on or off its charging mat.
//! A 60 s `hyperfluxctl capture` on hardware (Basilisk V3 Pro 35K, the
//! HyperFlux V2 mat) had no passive report at all while the mouse was
//! lifted off the mat and put back three times. All the news came from
//! replies to queries an app happened to send. So we ask, and the pointer
//! tells us when: a mouse that starts moving after a rest was probably
//! just lifted, and one that stops was probably just put down. Without
//! either we still ask once a minute while the mouse is online.
//!
//! This module decides when a check is wanted. The check is the ordinary
//! battery probe (07/80, then 07/84) with its lease and match rules, and
//! `Device::probe_at` keeps probes 2 s apart and out of the pointer's
//! movement.

/// Movement after this much stillness: the mouse was probably just lifted.
pub const LIFTED_AFTER_MS: u64 = 5_000;
/// The check after a lift is wanted this much later, and then waits for
/// the pointer to pause. A mouse that keeps moving keeps it waiting.
pub const LIFT_CHECK_MS: u64 = 1_000;
/// Still for this long: the mouse was probably just put down.
pub const PUT_DOWN_AFTER_MS: u64 = 3_000;
/// A second check after a put-down, because the mouse's first status after
/// power arrives can still read not charging.
pub const RECHECK_MS: u64 = 15_000;
/// With nothing else to go on, an online mouse is asked this often. That
/// is all the charge traffic an idle bridge has.
pub const REFRESH_MS: u64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Watch {
    /// The last pointer activity, or when watching began.
    last_activity: u64,
    moving: bool,
    lift: Option<u64>,
    recheck: Option<u64>,
    /// The last charge reading, or the last check.
    refreshed: u64,
}

impl Watch {
    #[must_use]
    pub const fn new(now: u64) -> Self {
        Self {
            last_activity: now,
            moving: false,
            lift: None,
            recheck: None,
            refreshed: now,
        }
    }

    pub fn activity(&mut self, at: u64) {
        if at.saturating_sub(self.last_activity) >= LIFTED_AFTER_MS {
            self.lift = Some(at + LIFT_CHECK_MS);
        }
        self.last_activity = self.last_activity.max(at);
        self.moving = true;
    }

    /// A reply or a status report said whether the mouse charges.
    pub fn refreshed(&mut self, at: u64) {
        self.refreshed = self.refreshed.max(at);
    }

    /// When a check is wanted next. The once-a-minute check waits while a
    /// lift or put-down check is coming anyway.
    #[must_use]
    pub fn due(&self) -> u64 {
        let put_down = self
            .moving
            .then_some(self.last_activity + PUT_DOWN_AFTER_MS);
        [self.lift, put_down, self.recheck]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(self.refreshed + REFRESH_MS)
    }

    /// A check was handed on at `now`, or there was nobody to ask: what was
    /// due by then is done.
    pub fn served(&mut self, now: u64) {
        self.lift = self.lift.filter(|at| *at > now);
        self.recheck = self.recheck.filter(|at| *at > now);
        if self.moving && self.last_activity + PUT_DOWN_AFTER_MS <= now {
            self.moving = false;
            self.recheck = Some(now + RECHECK_MS);
        }
        self.refreshed(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serves every check the watch wants up to `until`, as the bridge
    /// would, and returns when they were.
    fn checks(w: &mut Watch, until: u64) -> Vec<u64> {
        let mut out = Vec::new();
        while w.due() <= until {
            let at = w.due();
            w.served(at);
            out.push(at);
        }
        out
    }

    #[test]
    fn lift_then_put_down() {
        let mut w = Watch::new(0);
        // Picked up after a rest and moved for 2 s.
        for at in (10_000..=12_000).step_by(250) {
            w.activity(at);
        }
        assert_eq!(checks(&mut w, 12_000), [11_000]);
        // Put down: once, and again 15 s later.
        assert_eq!(checks(&mut w, 40_000), [15_000, 30_000]);
        assert_eq!(w.due(), 90_000);
        // An app's 07/84 reply pushes the once-a-minute check back.
        w.refreshed(50_000);
        assert_eq!(w.due(), 110_000);
    }

    #[test]
    fn short_pause_isnt_a_lift() {
        let mut w = Watch::new(0);
        w.activity(10_000);
        assert_eq!(checks(&mut w, 13_000), [11_000, 13_000]);
        // Moving again 4 s after the last movement: no lift check. The
        // put-down after it is checked, and its recheck replaces the earlier
        // one.
        w.activity(14_000);
        assert_eq!(checks(&mut w, 40_000), [17_000, 32_000]);
    }
}
