// SPDX-License-Identifier: GPL-2.0-only

//! The 90-byte Razer feature report a route carries.
//!
//! ```text
//! 0  status            0x00 in requests, 0x01..0x05 in responses
//! 1  transaction id    the bridge swaps it for the lane selector
//! 2  remaining packets (2 bytes, big-endian)
//! 4  protocol type     0x00
//! 5  data size         number of argument bytes used, <= 80
//! 6  command class
//! 7  command id        bit 7 set = a query (read)
//! 8  arguments         80 bytes
//! 88 checksum          XOR of bytes 2..=87
//! 89 reserved          0x00
//! ```

pub const REPORT_LEN: usize = 90;
pub type Report = [u8; REPORT_LEN];

pub const STATUS: usize = 0;
pub const TRANSACTION_ID: usize = 1;
pub const PROTOCOL_TYPE: usize = 4;
pub const DATA_SIZE: usize = 5;
pub const CLASS: usize = 6;
pub const COMMAND: usize = 7;
pub const ARGS: usize = 8;
pub const CHECKSUM: usize = 88;
pub const RESERVED: usize = 89;
pub const MAX_DATA_SIZE: u8 = 80;

pub const STATUS_NEW: u8 = 0x00;
pub const STATUS_BUSY: u8 = 0x01;
pub const STATUS_SUCCESS: u8 = 0x02;
pub const STATUS_FAILURE: u8 = 0x03;
/// Through the receiver this means the child is asleep or switched off.
pub const STATUS_TIMEOUT: u8 = 0x04;
pub const STATUS_NOT_SUPPORTED: u8 = 0x05;

/// Command id bit marking a query.
pub const QUERY_BIT: u8 = 0x80;

#[must_use]
pub fn checksum(report: &Report) -> u8 {
    report[2..CHECKSUM].iter().fold(0, |acc, byte| acc ^ byte)
}

#[must_use]
pub const fn is_query(report: &Report) -> bool {
    report[COMMAND] & QUERY_BIT != 0
}

/// A request with `args` as its arguments and a valid checksum.
///
/// # Panics
/// If `args` is longer than 80 bytes.
#[must_use]
pub fn build(transaction_id: u8, class: u8, command: u8, args: &[u8]) -> Report {
    let size = u8::try_from(args.len()).expect("at most 80 argument bytes");
    assert!(size <= MAX_DATA_SIZE, "at most 80 argument bytes");
    let mut report = [0_u8; REPORT_LEN];
    report[TRANSACTION_ID] = transaction_id;
    report[DATA_SIZE] = size;
    report[CLASS] = class;
    report[COMMAND] = command;
    report[ARGS..ARGS + args.len()].copy_from_slice(args);
    report[CHECKSUM] = checksum(&report);
    report
}

/// The shape a request must have before it goes to the receiver.
#[must_use]
pub fn request_shape_ok(report: &Report) -> bool {
    report[STATUS] == STATUS_NEW
        && report[PROTOCOL_TYPE] == 0
        && report[DATA_SIZE] <= MAX_DATA_SIZE
        && report[CHECKSUM] == checksum(report)
        && report[RESERVED] == 0
}

/// Same transaction id, class and command as `request`. The bridge already
/// matched these plus the remaining-packets bytes, but a client that gets a
/// reply through other means should check again.
#[must_use]
pub fn answers(request: &Report, response: &Report) -> bool {
    response[TRANSACTION_ID] == request[TRANSACTION_ID]
        && response[CLASS] == request[CLASS]
        && response[COMMAND] == request[COMMAND]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_range() {
        // The mouse serial query OpenRazer sends. Its checksum is 0x94.
        let report = build(0x1f, 0x00, 0x82, &[0; 22]);
        assert_eq!(report[CHECKSUM], 0x94);
        assert!(request_shape_ok(&report));
        let mut changed = report;
        changed[TRANSACTION_ID] = 0x9f;
        assert_eq!(checksum(&changed), checksum(&report));
        changed[CLASS] = 0x01;
        assert!(!request_shape_ok(&changed));
    }

    #[test]
    fn query_bit() {
        assert!(is_query(&build(0x1f, 0x07, 0x80, &[0, 0])));
        assert!(!is_query(&build(0x1f, 0x0f, 0x02, &[0; 6])));
    }
}
