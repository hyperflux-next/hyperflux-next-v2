// SPDX-License-Identifier: GPL-2.0-only

//! Everything the bridge reads out of report bytes: battery level, charge,
//! mat contact, answers to battery queries and the serial number. Pure
//! functions, so the property tests can throw arbitrary bytes at them.

use hfx_proto::devices::ChargeDecoder;
use hfx_proto::messages::{Charge, Contact};
use hfx_proto::report::{self, Report};
use sha2::{Digest, Sha256};

/// The receiver reports levels on a linear 0..=255 scale.
#[must_use]
pub fn level_percent(raw: u8) -> u8 {
    u8::try_from((u16::from(raw) * 100 + 127) / 255).unwrap_or(100)
}

/// Charge from a passive status report. Combinations seen on hardware are
/// decoded. The rest is unknown rather than a guess.
#[must_use]
pub fn status_charge(decoder: ChargeDecoder, power: u8, charge: u8) -> Option<Charge> {
    match decoder {
        ChargeDecoder::MouseStatus => match (charge, power) {
            (1, _) => Some(Charge::Charging),
            // Not charging counts as discharging only off the mat. On a cable
            // the mouse reports power 6 and a charge byte that lags behind.
            (0, 0) => Some(Charge::Discharging),
            _ => None,
        },
        ChargeDecoder::KeyboardStatus => match (u16::from(power) << 8) | u16::from(charge) {
            0x0101 | 0x0001 => Some(Charge::Charging),
            0x0000 => Some(Charge::Discharging),
            _ => None,
        },
        ChargeDecoder::None => None,
    }
}

/// Mat contact from the mouse's power byte.
#[must_use]
pub fn contact(power: u8) -> Option<Contact> {
    match power {
        0 => Some(Contact::OffMat),
        1 => Some(Contact::OnMat),
        _ => None,
    }
}

/// What the desktop is told about charging, as the power supply's status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryStatus {
    Unknown,
    Discharging,
    Charging,
    Full,
}

/// Neither device reports "full". Full is charging at 100 %.
#[must_use]
pub fn battery_status(level: u8, charge: Option<Charge>) -> BatteryStatus {
    match charge {
        Some(Charge::Charging) if level >= 100 => BatteryStatus::Full,
        Some(Charge::Charging) => BatteryStatus::Charging,
        Some(Charge::Discharging) => BatteryStatus::Discharging,
        None => BatteryStatus::Unknown,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    Level(u8),
    Charge(Charge),
    /// Not charging right now, which isn't the same as discharging.
    NotCharging,
}

fn zero_args(request: &Report) -> bool {
    request[report::ARGS..report::CHECKSUM]
        .iter()
        .all(|b| *b == 0)
}

/// A battery fact from a matched exchange, whoever sent it. A success
/// answer to the exact query counts. A busy, timed-out or foreign reply
/// says nothing about the battery.
#[must_use]
pub fn harvest(decoder: ChargeDecoder, request: &Report, response: &Report) -> Option<Reading> {
    if response[report::STATUS] != report::STATUS_SUCCESS
        || !report::answers(request, response)
        || request[report::CLASS] != 0x07
        || request[report::DATA_SIZE] != 2
        || !zero_args(request)
    {
        return None;
    }
    // The tested keyboard answers 07/84 with 0 while full on its cable, so
    // its status reports decide. For everyone else any value but 0 is
    // charging, as OpenRazer reads it: the Basilisk V3 Pro 35K answers 6 on
    // its mat and 1 on its cable. 0 ends a charging reading without making
    // it discharging: a mouse may just sit on its mat, a keyboard be full.
    match (request[report::COMMAND], response[report::ARGS + 1]) {
        (0x80, raw) => Some(Reading::Level(level_percent(raw))),
        _ if decoder == ChargeDecoder::KeyboardStatus => None,
        (0x84, 0) => Some(Reading::NotCharging),
        (0x84, _) => Some(Reading::Charge(Charge::Charging)),
        _ => None,
    }
}

/// The firmware version in an answer to 00:81, which OpenRazer shows as
/// `v1.2` for `[1, 2]`.
#[must_use]
pub fn firmware(request: &Report, response: &Report) -> Option<[u8; 2]> {
    let asked = request[report::CLASS] == 0x00 && request[report::COMMAND] == 0x81;
    let sized = response[report::DATA_SIZE] >= 2;
    (asked && sized && query_answered(request, response))
        .then(|| [response[report::ARGS], response[report::ARGS + 1]])
}

/// A query reply OpenRazer may see: success, and the same transaction id,
/// class and command as the request.
#[must_use]
pub fn query_answered(request: &Report, response: &Report) -> bool {
    response[report::STATUS] == report::STATUS_SUCCESS && report::answers(request, response)
}

/// Worth asking again: the device was busy, or the reply belongs to some
/// other request.
#[must_use]
pub fn query_retryable(request: &Report, response: &Report) -> bool {
    response[report::STATUS] == report::STATUS_BUSY || !report::answers(request, response)
}

pub const SERIAL_ARGS: u8 = 22;

#[must_use]
pub fn serial_request(transaction_id: u8) -> Report {
    report::build(transaction_id, 0x00, 0x82, &[0; SERIAL_ARGS as usize])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SerialError {
    /// The receiver answered for the device with 0x04: it is asleep or out
    /// of range, and asking again before it shows signs of life is no use.
    NoAnswer,
    /// Busy or failure from the device, or a reply to something else:
    /// worth retrying soon.
    Retry,
    /// A reply that can't be a serial number: the route waits longer.
    Invalid,
    /// A well-formed reply whose serial is empty or not printable.
    Unusable,
}

/// The fingerprint of the serial number in a reply to [`serial_request`].
/// The serial itself is never kept.
///
/// # Errors
/// See [`SerialError`].
pub fn serial_fingerprint(request: &Report, response: &Report) -> Result<[u8; 32], SerialError> {
    if response[report::STATUS] != report::STATUS_SUCCESS {
        return Err(match response[report::STATUS] {
            report::STATUS_TIMEOUT => SerialError::NoAnswer,
            report::STATUS_BUSY | report::STATUS_FAILURE => SerialError::Retry,
            _ => SerialError::Invalid,
        });
    }
    if !report::answers(request, response) || response[2..5] != request[2..5] {
        return Err(SerialError::Retry);
    }
    if response[report::DATA_SIZE] < SERIAL_ARGS
        || response[report::CHECKSUM] != report::checksum(response)
        || response[report::RESERVED] != 0
    {
        return Err(SerialError::Invalid);
    }
    let end = report::ARGS + usize::from(SERIAL_ARGS);
    fingerprint(&response[report::ARGS..end]).ok_or(SerialError::Unusable)
}

/// SHA-256 over a serial number with NUL and white space trimmed. None for
/// an empty or non-printable serial, which must never match anything.
#[must_use]
pub fn fingerprint(raw: &[u8]) -> Option<[u8; 32]> {
    let trim = |b: &u8| *b == 0 || b.is_ascii_whitespace();
    let start = raw.iter().position(|b| !trim(b))?;
    let end = raw.iter().rposition(|b| !trim(b))? + 1;
    let serial = &raw[start..end];
    if !serial.iter().all(u8::is_ascii_graphic) {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(b"hyperflux-next serial\0");
    hash.update(serial);
    Some(hash.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(request: &Report, status: u8, byte9: u8) -> Report {
        let mut r = *request;
        r[0] = status;
        r[9] = byte9;
        r[report::CHECKSUM] = report::checksum(&r);
        r
    }

    #[test]
    fn levels() {
        assert_eq!(level_percent(202), 79);
        assert_eq!(level_percent(0), 0);
        assert_eq!(level_percent(255), 100);
        assert_eq!(level_percent(1), 0);
        assert_eq!(level_percent(2), 1);
        assert_eq!(level_percent(163), 64);
    }

    #[test]
    fn mouse_charge_table() {
        let d = ChargeDecoder::MouseStatus;
        assert_eq!(status_charge(d, 1, 1), Some(Charge::Charging));
        assert_eq!(status_charge(d, 6, 1), Some(Charge::Charging));
        assert_eq!(status_charge(d, 0, 1), Some(Charge::Charging));
        assert_eq!(status_charge(d, 0, 0), Some(Charge::Discharging));
        assert_eq!(status_charge(d, 1, 0), None);
        assert_eq!(status_charge(d, 6, 0), None);
        assert_eq!(status_charge(d, 0, 6), None);
    }

    #[test]
    fn keyboard_charge_table() {
        let d = ChargeDecoder::KeyboardStatus;
        assert_eq!(status_charge(d, 1, 1), Some(Charge::Charging));
        assert_eq!(status_charge(d, 0, 1), Some(Charge::Charging));
        assert_eq!(status_charge(d, 0, 0), Some(Charge::Discharging));
        assert_eq!(status_charge(d, 1, 0), None);
        assert_eq!(status_charge(d, 2, 0), None);
        assert_eq!(status_charge(ChargeDecoder::None, 0, 1), None);
    }

    #[test]
    fn full_is_charging_at_100() {
        assert_eq!(
            battery_status(100, Some(Charge::Charging)),
            BatteryStatus::Full
        );
        assert_eq!(
            battery_status(99, Some(Charge::Charging)),
            BatteryStatus::Charging
        );
        assert_eq!(
            battery_status(100, Some(Charge::Discharging)),
            BatteryStatus::Discharging
        );
        assert_eq!(battery_status(100, None), BatteryStatus::Unknown);
    }

    #[test]
    fn harvested_replies() {
        let mouse = ChargeDecoder::MouseStatus;
        let level = report::build(0x1f, 0x07, 0x80, &[0, 0]);
        assert_eq!(
            harvest(mouse, &level, &reply(&level, 2, 202)),
            Some(Reading::Level(79))
        );
        for status in [1, 3, 4, 5] {
            assert_eq!(harvest(mouse, &level, &reply(&level, status, 202)), None);
        }
        let mut other_lane = reply(&level, 2, 128);
        other_lane[1] = 0x9f;
        assert_eq!(harvest(mouse, &level, &other_lane), None);
        let with_args = report::build(0x1f, 0x07, 0x80, &[1, 0]);
        assert_eq!(harvest(mouse, &with_args, &reply(&with_args, 2, 9)), None);

        let charging = report::build(0x1f, 0x07, 0x84, &[0, 0]);
        assert_eq!(
            harvest(mouse, &charging, &reply(&charging, 2, 1)),
            Some(Reading::Charge(Charge::Charging))
        );
        assert_eq!(
            harvest(mouse, &charging, &reply(&charging, 2, 0)),
            Some(Reading::NotCharging)
        );
        let on_mat = harvest(mouse, &charging, &reply(&charging, 2, 6));
        assert_eq!(on_mat, Some(Reading::Charge(Charge::Charging)));
        let keyboard = ChargeDecoder::KeyboardStatus;
        assert_eq!(harvest(keyboard, &charging, &reply(&charging, 2, 1)), None);
        assert_eq!(harvest(keyboard, &charging, &reply(&charging, 2, 0)), None);
        let none = ChargeDecoder::None;
        let one = Some(Reading::Charge(Charge::Charging));
        assert_eq!(harvest(none, &charging, &reply(&charging, 2, 1)), one);
        let zero = harvest(none, &charging, &reply(&charging, 2, 0));
        assert_eq!(zero, Some(Reading::NotCharging));
    }

    #[test]
    fn serial_replies() {
        let request = serial_request(0x1f);
        assert_eq!(request[report::CHECKSUM], 0x94);
        let mut answer = request;
        answer[0] = 2;
        answer[8..23].copy_from_slice(b"PM2342H10000042");
        answer[report::CHECKSUM] = report::checksum(&answer);
        let print = serial_fingerprint(&request, &answer).unwrap();
        let mut padded = [0_u8; 22];
        padded[2..17].copy_from_slice(b"PM2342H10000042");
        assert_eq!(fingerprint(&padded), Some(print));

        let mut busy = answer;
        busy[0] = 1;
        assert_eq!(serial_fingerprint(&request, &busy), Err(SerialError::Retry));
        let mut asleep = answer;
        asleep[0] = 4;
        assert_eq!(
            serial_fingerprint(&request, &asleep),
            Err(SerialError::NoAnswer)
        );
        let mut unsupported = answer;
        unsupported[0] = 5;
        assert_eq!(
            serial_fingerprint(&request, &unsupported),
            Err(SerialError::Invalid)
        );
        let mut other_tx = answer;
        other_tx[1] = 0x9f;
        assert_eq!(
            serial_fingerprint(&request, &other_tx),
            Err(SerialError::Retry)
        );
        let mut empty = request;
        empty[0] = 2;
        let unusable = Err(SerialError::Unusable);
        assert_eq!(serial_fingerprint(&request, &empty), unusable);
        // A longer data size still has the serial in front.
        let mut longer = answer;
        longer[report::DATA_SIZE] = 24;
        longer[report::CHECKSUM] = report::checksum(&longer);
        assert_eq!(serial_fingerprint(&request, &longer), Ok(print));
        longer[report::DATA_SIZE] = 21;
        longer[report::CHECKSUM] = report::checksum(&longer);
        let short = serial_fingerprint(&request, &longer);
        assert_eq!(short, Err(SerialError::Invalid));
        assert_eq!(fingerprint(b"PM23\x01"), None);
    }
}
