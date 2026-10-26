// SPDX-License-Identifier: GPL-2.0-only

//! Presentation rules the clients share. The CLI and the OpenRGB page say
//! the same thing about the same reading.

use crate::messages::Inventory;

/// A battery reading older than this is stale. The tested devices report on
/// their own every few minutes, and the bridge asks an online device it
/// has not heard from in ten. Half an hour of silence means the device
/// doesn't answer: it is off, asleep or out of range.
pub const BATTERY_STALE_AFTER_MS: u64 = 30 * 60 * 1000;

/// "1 minute", "5 minutes", "2 hours", "3 days". Under a minute counts as
/// one minute, and from 48 hours on it's whole days.
#[must_use]
pub fn age_phrase(age_ms: u64) -> String {
    let minutes = (age_ms / 60_000).max(1);
    if minutes < 60 {
        return plural(minutes, "minute");
    }
    let hours = minutes / 60;
    if hours < 48 {
        return plural(hours, "hour");
    }
    plural(hours / 24, "day")
}

/// What named a receiver's devices, as the bridge's log and `hyperfluxctl
/// devices` put it.
#[must_use]
pub const fn inventory_phrase(inventory: Inventory) -> &'static str {
    match inventory {
        Inventory::Slots => "devices named by the receiver's paired slots",
        Inventory::KnownOffset => "pairing inventory recognised",
        Inventory::Marker => "pairing inventory recognised by its marker",
        Inventory::NotRecognised => "pairing inventory not recognised",
        Inventory::Absent => "no pairing inventory",
    }
}

fn plural(count: u64, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

#[cfg(test)]
mod tests {
    use super::age_phrase;

    #[test]
    fn age_phrases() {
        assert_eq!(age_phrase(0), "1 minute");
        assert_eq!(age_phrase(119_999), "1 minute");
        assert_eq!(age_phrase(120_000), "2 minutes");
        assert_eq!(age_phrase(59 * 60_000), "59 minutes");
        assert_eq!(age_phrase(60 * 60_000), "1 hour");
        assert_eq!(age_phrase(47 * 3_600_000), "47 hours");
        assert_eq!(age_phrase(48 * 3_600_000), "2 days");
    }
}
