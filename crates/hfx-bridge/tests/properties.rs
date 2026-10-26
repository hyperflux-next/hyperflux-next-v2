// SPDX-License-Identifier: GPL-2.0-only

//! Property tests for everything that reads bytes from another process or
//! from a device: UHID events, passive reports, replies, the inventory,
//! uevents and sysfs names. None may panic, and what they accept must
//! keep the rules the bridge relies on.

use hfx_bridge::decode::{self, Reading};
use hfx_bridge::transport::{Event, Record, Source};
use hfx_bridge::{platform, sysfs, uhid, wire};
use hfx_proto::devices::ChargeDecoder;
use hfx_proto::messages::{Charge, Lane};
use hfx_proto::report::{self, Report};
use proptest::prelude::*;

fn report_strategy() -> impl Strategy<Value = Report> {
    prop::collection::vec(any::<u8>(), 90).prop_map(|v| v.try_into().unwrap())
}

fn lane() -> impl Strategy<Value = Lane> {
    prop_oneof![Just(Lane::Pointer), Just(Lane::Keyboard)]
}

proptest! {
    #[test]
    fn uhid_decoder_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..300)) {
        let _ = uhid::decode(&bytes);
        let _ = uhid::decode_written(&bytes);
    }

    #[test]
    fn uhid_set_reports_round_trip(id in any::<u32>(), data in prop::collection::vec(any::<u8>(), 0..200)) {
        let decoded = uhid::decode(&uhid::kernel_set(id, &data));
        prop_assert_eq!(decoded, Some(uhid::Event::SetReport { id, number: 0, kind: uhid::FEATURE_REPORT, data }));
    }

    #[test]
    fn passive_report_products(bytes in prop::collection::vec(any::<u8>(), 0..16)) {
        match wire::passive_report(&bytes) {
            Some(Event::Identity { product_id, .. }) => prop_assert!(product_id != 0 && product_id != 0xffff),
            Some(Event::Status { level, power, charge, extra, .. }) => {
                prop_assert_eq!([level, power, charge, extra], [bytes[2], bytes[3], bytes[4], bytes[5]]);
            }
            other => prop_assert!(other.is_none()),
        }
        let vendor = bytes.first().is_some_and(|b| wire::VENDOR_REPORTS.contains(b));
        let raw = wire::raw_report(&bytes, bytes.len() as u16);
        let whole = matches!(raw, Some(Event::Raw { len, .. }) if usize::from(len) == bytes.len());
        prop_assert_eq!(whole, vendor);
    }

    #[test]
    fn other_selector_never_matches(request in report_strategy(), response in report_strategy(), lane in lane()) {
        let sent = wire::route(&request, lane);
        let mut foreign = response;
        foreign[1] = sent[1] ^ 0x80;
        prop_assert!(!wire::matches(&sent, &foreign));
        if wire::matches(&sent, &response) {
            prop_assert_eq!(response[6..8].to_vec(), sent[6..8].to_vec());
            prop_assert_eq!(wire::restore(&response, request[1])[1], request[1]);
        }
    }

    #[test]
    fn harvest_only_successful_answers(request in report_strategy(), response in report_strategy()) {
        for decoder in [ChargeDecoder::MouseStatus, ChargeDecoder::KeyboardStatus, ChargeDecoder::None] {
            match decode::harvest(decoder, &request, &response) {
                Some(Reading::Level(level)) => {
                    prop_assert!(level <= 100);
                    prop_assert_eq!((response[0], request[6], request[7]), (2, 0x07, 0x80));
                }
                Some(Reading::Charge(charge)) => {
                    prop_assert_eq!((charge, request[7], response[9]), (Charge::Charging, 0x84, 1));
                    prop_assert_ne!(decoder, ChargeDecoder::KeyboardStatus);
                }
                Some(Reading::NotCharging) => {
                    prop_assert_eq!((request[7], response[9]), (0x84, 0));
                    prop_assert_ne!(decoder, ChargeDecoder::KeyboardStatus);
                }
                None => {}
            }
        }
    }

    #[test]
    fn serial_parsing_never_panics(request in report_strategy(), response in report_strategy()) {
        if decode::serial_fingerprint(&request, &response).is_ok() {
            prop_assert_eq!(response[0], report::STATUS_SUCCESS);
        }
        let _ = decode::fingerprint(&response[8..30]);
    }

    #[test]
    fn inventory_parsing_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..600)) {
        let _ = wire::read_inventory(Some(&bytes));
    }

    #[test]
    fn uevent_parsing_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..200)) {
        let _ = platform::parse_uevent(&bytes);
        let _ = platform::udev_subsystem(&bytes);
    }

    #[test]
    fn hid_names_never_panic(name in "\\PC{0,24}") {
        let _ = sysfs::parse_hid_name(&name);
    }

    #[test]
    fn levels_stay_in_range(raw in any::<u8>()) {
        let percent = decode::level_percent(raw);
        prop_assert!(percent <= 100);
        prop_assert_eq!(decode::level_percent(hfx_bridge::sim::raw_level(percent)), percent);
    }

    #[test]
    fn receivers_survive_any_event_sequence(events in prop::collection::vec(event(), 0..64)) {
        let mut rx = receiver();
        for (i, (event, replayed)) in events.into_iter().enumerate() {
            let record = Record { time_ms: i as u64, replayed, source: Source::Passive, event };
            let applied = rx.apply(&record, 1000);
            prop_assert!(applied.removed.len() <= 1);
            for d in rx.lanes.iter().flatten() {
                prop_assert!(d.battery.level.is_none_or(|l| l <= 100));
                // A catalog device keeps its entry on either lane. Off its own
                // lane its status bytes aren't read, and mat contact needs the
                // mouse-status decoder.
                prop_assert!(d.own_lane() || d.decoder() == ChargeDecoder::None);
                prop_assert!(d.contact.is_none() || d.decoder() == ChargeDecoder::MouseStatus);
            }
        }
    }
}

#[test]
fn charge_tables_are_exhaustive() {
    for power in 0..=255_u8 {
        for charge in 0..=255_u8 {
            let mouse = decode::status_charge(ChargeDecoder::MouseStatus, power, charge);
            let expected = match (charge, power) {
                (1, _) => Some(Charge::Charging),
                (0, 0) => Some(Charge::Discharging),
                _ => None,
            };
            assert_eq!(mouse, expected);
            let keyboard = decode::status_charge(ChargeDecoder::KeyboardStatus, power, charge);
            let expected = match (power, charge) {
                (0 | 1, 1) => Some(Charge::Charging),
                (0, 0) => Some(Charge::Discharging),
                _ => None,
            };
            assert_eq!(keyboard, expected);
        }
    }
}

fn event() -> impl Strategy<Value = (Event, bool)> {
    let pid = prop_oneof![Just(0x00cd_u16), Just(0x0296), Just(0x00ab), 1_u16..0xfffe];
    let event = prop_oneof![
        (lane(), pid, any::<u8>()).prop_map(|(lane, product_id, arg0)| Event::Identity {
            lane,
            product_id,
            arg0,
            arg1: 0
        }),
        (lane(), any::<[u8; 4]>()).prop_map(|(lane, b)| Event::Status {
            lane,
            level: b[0],
            power: b[1],
            charge: b[2],
            extra: b[3]
        }),
        lane().prop_map(|lane| Event::Activity { lane }),
        any::<(bool, bool)>().prop_map(|(suspended, leaving)| Event::State {
            suspended,
            leaving,
            ready: true
        }),
    ];
    (event, any::<bool>())
}

fn receiver() -> hfx_bridge::state::Receiver {
    hfx_bridge::state::Receiver {
        key: 1,
        node: "test".into(),
        id: "receiver-1".into(),
        generation: 1,
        vendor_id: 0x1532,
        product_id: 0xcf,
        suspended: false,
        leaving: false,
        writable: true,
        worker: hfx_bridge::exchange::Worker::default(),
        lanes: [None, None],
        raw: Default::default(),
        publish: Default::default(),
        inventory: wire::read_inventory(None),
        slots: Default::default(),
    }
}
