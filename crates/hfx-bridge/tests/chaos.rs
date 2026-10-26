// SPDX-License-Identifier: GPL-2.0-only

//! The receiver under chaotic timing: answers ready after a random 1.5 to
//! 9 ms, about a fifth of them busy until then, and the odd one silent, over
//! thousands of exchanges on both lanes with identical and different queries
//! mixed. The point is that a stale answer is never passed off as the right
//! one. Each answer carries the lane and command it belongs to (the
//! simulator echoes the request), so a reply for another lane, another
//! command, or an earlier request would show up as the wrong bytes. A fixed
//! seed makes a failure reproduce.

mod common;

use common::Harness;
use hfx_proto::client::Client;
use hfx_proto::ids::ExchangeTimeoutMs;
use hfx_proto::messages::{Lane, TransactOutcome};
use hfx_proto::report::{self, Report};
use std::time::{Duration, Instant};

/// SplitMix64, so the run is deterministic and needs no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// `n` in `0..bound`.
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// Query commands the simulator echoes unchanged (not one of its battery or
/// serial special cases), each with a data size the lane's device lists and
/// room for the tag. Rotating through them gives different queries; sending
/// one twice gives identical ones.
const COMMANDS: [(u8, u8, u8); 3] = [(0x0f, 0x80, 80), (0x0f, 0x81, 80), (0x0f, 0x03, 71)];

/// A request tagged with the lane and command it belongs to, which makes its
/// echoed answer its own and no one else's. The same (lane, command) always
/// makes the same bytes, as a real client's identical frames do, since the
/// receiver can't tell two same-keyed requests apart.
fn tagged(lane: Lane, class: u8, command: u8, size: u8) -> Report {
    let mut args = vec![0_u8; usize::from(size)];
    args[0] = match lane {
        Lane::Pointer => 0xaa,
        Lane::Keyboard => 0xbb,
    };
    args[1] = command;
    let tx = match lane {
        Lane::Pointer => 0x1f,
        Lane::Keyboard => 0x9f,
    };
    report::build(tx, class, command, &args)
}

#[test]
fn no_stale_answers() {
    let h = Harness::start();
    let mut c = h.client(&["native-routes"]);
    let mut lanes = Vec::new();
    for lane in [Lane::Pointer, Lane::Keyboard] {
        let token = common::token(&mut c, lane);
        let lease = common::lease(&mut c, &token);
        lanes.push((lane, token, lease));
    }
    let timeout = ExchangeTimeoutMs::new(200).unwrap();
    let mut rng = Rng(0x5eed_1234);
    let per_lane: u32 = 1_000;
    let deadline = Instant::now() + Duration::from_secs(120);
    let (mut matched, mut no_answer, mut identical, mut different) = (0_u64, 0_u64, 0, 0);
    let mut last_command = [0xff_u8; 2];

    let lease_ms = hfx_proto::ids::LeaseDurationMs::new(60_000).unwrap();
    for round in 0..per_lane {
        // Keep both leases alive. A run this long outlives one lease.
        if round % 200 == 0 {
            for (_, _, lease) in &lanes {
                c.renew_lease(lease, lease_ms).unwrap();
            }
        }
        for (idx, (lane, token, lease)) in lanes.iter().enumerate() {
            assert!(Instant::now() < deadline, "the run hung at round {round}");
            // About a fifth busy until the answer is ready, answers ready
            // after a random 1.5 to 9 ms, and the odd request silent.
            let after_us = 1_500 + rng.below(7_501);
            h.sim.set_answer_after(0, *lane, &[after_us]);
            h.sim.set_busy_until_ready(0, *lane, rng.below(100) < 20);
            if rng.below(100) < 6 {
                h.sim.set_silent(0, *lane, 1);
            }
            // Mostly a fresh command, sometimes the same one again.
            let repeat = rng.below(100) < 25;
            let pick = if repeat && last_command[idx] != 0xff {
                COMMANDS
                    .iter()
                    .find(|(_, cmd, _)| *cmd == last_command[idx])
                    .unwrap()
            } else {
                &COMMANDS[rng.below(COMMANDS.len() as u64) as usize]
            };
            let (class, command, size) = *pick;
            if command == last_command[idx] {
                identical += 1;
            } else {
                different += 1;
            }
            last_command[idx] = command;

            let request = tagged(*lane, class, command, size);
            let result = c.transact(lease, token, &request, timeout).unwrap();
            match result.outcome {
                TransactOutcome::Matched => {
                    matched += 1;
                    let answer = *result
                        .response
                        .expect("a matched outcome has a response")
                        .bytes();
                    // The status byte and the checksum may differ, nothing
                    // else. The tag, class and command are the request's:
                    // this is the request's own answer.
                    assert_eq!(
                        (answer[report::CLASS], answer[report::COMMAND]),
                        (class, command),
                        "an answer for another command came back"
                    );
                    let args = report::ARGS..report::ARGS + usize::from(size);
                    assert_eq!(
                        &answer[args.clone()],
                        &request[args],
                        "a stale answer (wrong tag) was accepted on {lane:?}"
                    );
                }
                TransactOutcome::NotSent | TransactOutcome::MayHaveBeenSent => no_answer += 1,
            }
        }
    }

    // The chaos actually happened: both kinds of query, some answered, some
    // not, and the busy and other re-reads were exercised.
    assert!(identical > 0 && different > 0, "queries weren't mixed");
    assert!(matched > u64::from(per_lane), "too few answers: {matched}");
    assert!(no_answer > 0, "the silent requests never showed");

    for (lane, _, _) in &lanes {
        let counts = exchanges(&mut c, *lane);
        // sent = answered at the first read + answered on a later read + none.
        assert!(
            counts.first_read + counts.no_answer <= counts.sent,
            "{lane:?} counters don't add up: {counts:?}"
        );
        assert!(counts.sent >= per_lane, "{lane:?} sent too few: {counts:?}");
        assert!(
            counts.reread_other + counts.reread_busy > 0,
            "{lane:?} never re-read under chaos"
        );
        // The learned wait never leaves its 2 to 8 ms band.
        let wait = learned(&mut c, *lane);
        assert!(
            (2_000..=8_000).contains(&wait),
            "{lane:?} learned wait {wait} out of band"
        );
    }
}

fn exchanges(c: &mut Client, lane: Lane) -> hfx_proto::messages::Exchanges {
    let snapshot = c.snapshot().unwrap();
    common::device(&snapshot, 0, lane)
        .unwrap()
        .exchanges
        .unwrap()
}

fn learned(c: &mut Client, lane: Lane) -> u16 {
    let snapshot = c.snapshot().unwrap();
    common::device(&snapshot, 0, lane)
        .unwrap()
        .first_get
        .unwrap()
        .delay_us
}
