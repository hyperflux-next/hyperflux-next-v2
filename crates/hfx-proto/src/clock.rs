// SPDX-License-Identifier: GPL-2.0-only

//! The one clock all timestamps in the stack use: CLOCK_BOOTTIME. It keeps
//! counting during suspend. That way "no update for 40 minutes" is still
//! right after the laptop slept.

use rustix::time::{ClockId, clock_gettime};

#[must_use]
pub fn boottime_ns() -> u64 {
    let now = clock_gettime(ClockId::Boottime);
    let secs = u64::try_from(now.tv_sec).unwrap_or(0);
    let nanos = u64::try_from(now.tv_nsec).unwrap_or(0);
    secs.saturating_mul(1_000_000_000).saturating_add(nanos)
}

#[must_use]
pub fn boottime_ms() -> u64 {
    boottime_ns() / 1_000_000
}
