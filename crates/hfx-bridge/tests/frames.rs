// SPDX-License-Identifier: GPL-2.0-only

//! Writes in quick succession, as drawn lighting sends them, on the
//! simulated receiver: every catalog device, devices that answer with and
//! without the request's arguments, and the receiver's gap between lanes.

mod common;

use common::Harness;
use hfx_proto::client::Client;
use hfx_proto::devices;
use hfx_proto::ids::{ExchangeTimeoutMs, LeaseId, RouteToken};
use hfx_proto::messages::{Lane, TransactOutcome, TransactResult};
use hfx_proto::report::{self, Report};
use std::time::{Duration, Instant};

const KB: Lane = Lane::Keyboard;

/// A write with `n` in each argument byte. The device mode takes only 0
/// and 3, so it gets those in turn.
fn write(class: u8, id: u8, size: u8, n: u8) -> Report {
    let mut args = vec![n; usize::from(size)];
    if (class, id) == (0x00, 0x04) {
        args = vec![3 * (n % 2), 0];
    }
    report::build(0x1f, class, id, &args)
}

fn row(n: u8) -> Report {
    write(0x0f, 0x03, 71, n)
}

struct Leased {
    c: Client,
    token: RouteToken,
    lease: LeaseId,
}

impl Leased {
    fn new(h: &Harness, lane: Lane) -> Self {
        let (c, token, lease) = common::leased(h, &["native-routes"], lane);
        Self { c, token, lease }
    }

    fn send(&mut self, request: &Report) -> (TransactResult, Duration) {
        let timeout = ExchangeTimeoutMs::new(1000).unwrap();
        let start = Instant::now();
        let result = self
            .c
            .transact(&self.lease, &self.token, request, timeout)
            .unwrap();
        (result, start.elapsed())
    }
}

fn arguments(result: &TransactResult) -> Option<&[u8]> {
    let response = result.response.as_ref()?.bytes();
    Some(&response[report::DATA_SIZE..report::CHECKSUM]).filter(|_| {
        result.outcome == TransactOutcome::Matched
            && response[report::STATUS] == report::STATUS_SUCCESS
    })
}

#[test]
fn every_write_gets_its_answer() {
    // Every write each catalog device takes, three times with other
    // arguments. The answers come after the longest first wait there is,
    // so each first read gets the receiver's last answer back.
    for d in devices::DEVICES.iter().filter(|d| d.qualified()) {
        let (lane, pid) = (d.kind.lane(), d.product_id);
        let options = common::Options::before(move |sim| sim.identity(0, lane, pid, 1));
        let (h, _) = Harness::with(options);
        h.sim.set_answer_after(0, lane, &[9_500]);
        let mut l = Leased::new(&h, lane);
        for command in d.commands.iter().filter(|c| c.id & 0x80 == 0) {
            for &size in command.sizes {
                for n in 1..=3 {
                    let request = write(command.class, command.id, size, n);
                    let (result, _) = l.send(&request);
                    let own = &request[report::DATA_SIZE..report::CHECKSUM];
                    let (class, id) = (command.class, command.id);
                    let what = format!("{} {class:02x}:{id:02x}:{size} #{n}", d.name);
                    assert_eq!(arguments(&result), Some(own), "{what}: {result:?}");
                }
            }
        }
        drop(l);
        h.stop().unwrap();
    }
}

#[test]
fn rows_wait_the_learned_time() {
    // A keyboard that answers after 1 ms: its learned wait comes down from
    // the catalog's 4 ms to 2 ms on queries that differ in turn.
    let h = Harness::start();
    h.sim.set_answer_after(0, KB, &[1_000]);
    let mut l = Leased::new(&h, KB);
    let queries = [0x81, 0x84].map(|id| report::build(0x9f, 0x00, id, &[0, 0]));
    for query in queries.iter().cycle().take(250) {
        l.send(query);
    }
    // Five rows after a first one: the quickest of them, and their answers.
    let rows = |l: &mut Leased| {
        l.send(&queries[0]);
        let sent: Vec<_> = (1..=6).map(|n| l.send(&row(n))).collect();
        let quickest = sent[1..].iter().map(|(_, t)| *t).min().unwrap();
        (quickest, sent)
    };
    // Each row answered with its own arguments: 2 ms, not 4.
    let (quickest, sent) = rows(&mut l);
    assert!(quickest < Duration::from_micros(3_500), "{quickest:?}");
    assert!(sent.iter().all(|(r, _)| arguments(r).is_some()));
    // A keyboard that answers with zeros gives the bridge nothing to tell
    // one row's answer from the last by. Every row waits the catalog's 4 ms.
    h.sim.set_echo(0, KB, false);
    let (quickest, sent) = rows(&mut l);
    assert!(quickest >= Duration::from_millis(4), "{quickest:?}");
    assert!(sent.iter().all(|(r, _)| arguments(r).is_some()));
    drop(l);
    h.stop().unwrap();
}

#[test]
fn answer_without_arguments_counts() {
    // The keyboard answers the first row with its arguments and the second
    // with zeros, after the first read. The second gets its own answer,
    // not the first row's.
    let h = Harness::start();
    let mut l = Leased::new(&h, KB);
    let (first, _) = l.send(&row(1));
    assert_eq!(
        arguments(&first),
        Some(&row(1)[report::DATA_SIZE..report::CHECKSUM])
    );
    h.sim.set_echo(0, KB, false);
    h.sim.set_answer_after(0, KB, &[9_500]);
    let (second, _) = l.send(&row(2));
    let zeros = arguments(&second).map(|a| a[3..].iter().all(|b| *b == 0));
    assert_eq!(zeros, Some(true), "{second:?}");
    drop(l);
    h.stop().unwrap();
}

#[test]
fn keyboard_waits_after_pointer() {
    let h = Harness::start();
    let mut pointer = Leased::new(&h, Lane::Pointer);
    let mut keyboard = Leased::new(&h, KB);
    let battery = report::build(0x1f, 0x07, 0x80, &[0, 0]);
    // A receiver that ignores the keyboard for 6 ms after the pointer's
    // answer: the bridge's 8 ms covers it.
    h.sim.set_deaf_after_pointer(0, Duration::from_millis(6));
    for n in 1..=5 {
        pointer.send(&battery);
        let (result, _) = keyboard.send(&row(n));
        assert!(arguments(&result).is_some(), "{result:?}");
    }
    // A pointer request after the keyboard's goes straight out.
    let quickest = (1..=5)
        .map(|n| {
            keyboard.send(&row(n));
            pointer.send(&battery).1
        })
        .min()
        .unwrap();
    assert!(quickest < Duration::from_millis(8), "{quickest:?}");
    // A receiver that needs 12 ms: the first keyboard requests are lost,
    // and each two lost make the bridge wait a millisecond longer.
    h.sim.set_deaf_after_pointer(0, Duration::from_millis(12));
    let lost: Vec<bool> = (1..=20)
        .map(|n| {
            pointer.send(&battery);
            arguments(&keyboard.send(&row(n)).0).is_none()
        })
        .collect();
    let first = lost.iter().filter(|l| **l).count();
    assert!((1..=12).contains(&first), "{lost:?}");
    assert!(lost[15..].iter().all(|l| !l), "{lost:?}");
    drop((pointer, keyboard));
    h.stop().unwrap();
}
