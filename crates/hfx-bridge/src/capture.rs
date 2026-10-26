// SPDX-License-Identifier: GPL-2.0-only

//! The last 1024 receiver events and query replies, for `capture`.

use hfx_proto::ids::{GenerationId, HexBytes, MonotonicMs, ReceiverId, Sequence};
use hfx_proto::messages::{CaptureEvent, CaptureKind, CaptureLane, CaptureSource};
use std::collections::VecDeque;

pub const CAPACITY: usize = 1024;

#[derive(Default)]
pub struct Ring {
    events: VecDeque<CaptureEvent>,
    latest: u64,
}

pub struct Entry<'a> {
    pub receiver_id: &'a str,
    pub generation: u64,
    pub time_ms: u64,
    pub lane: CaptureLane,
    pub kind: CaptureKind,
    pub source: CaptureSource,
    pub data: &'a [u8],
}

impl Ring {
    pub fn push(&mut self, entry: &Entry<'_>) {
        let (Ok(receiver_id), Ok(generation_id), Ok(data)) = (
            ReceiverId::new(entry.receiver_id),
            GenerationId::new(entry.generation),
            HexBytes::new(entry.data.to_vec()),
        ) else {
            return;
        };
        self.latest += 1;
        if self.events.len() == CAPACITY {
            self.events.pop_front();
        }
        self.events.push_back(CaptureEvent {
            sequence: Sequence::new(self.latest).expect("sequences have no minimum"),
            receiver_id,
            generation_id,
            time_ms: MonotonicMs::new(entry.time_ms).expect("Ms has no minimum"),
            lane: entry.lane,
            kind: entry.kind,
            source: entry.source,
            data,
        });
    }

    #[must_use]
    pub fn latest(&self) -> u64 {
        self.latest
    }

    /// Events after `after`, at most `max`, whether some were already
    /// dropped, and the `next` cursor.
    #[must_use]
    pub fn read(&self, after: u64, max: usize) -> (Vec<CaptureEvent>, bool, u64) {
        let oldest = self.latest + 1 - self.events.len() as u64;
        let gap = after < self.latest && after + 1 < oldest;
        let events: Vec<CaptureEvent> = self
            .events
            .iter()
            .filter(|e| e.sequence.get() > after)
            .take(max)
            .cloned()
            .collect();
        let next = events
            .last()
            .map_or(after.min(self.latest), |e| e.sequence.get());
        (events, gap, next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(ring: &mut Ring, n: usize) {
        for _ in 0..n {
            ring.push(&Entry {
                receiver_id: "receiver-1",
                generation: 1,
                time_ms: 5,
                lane: CaptureLane::Pointer,
                kind: CaptureKind::Activity,
                source: CaptureSource::Input,
                data: &[0; 8],
            });
        }
    }

    #[test]
    fn cursor_and_gap() {
        let mut ring = Ring::default();
        assert_eq!(ring.read(0, 10), (vec![], false, 0));
        push(&mut ring, 3);
        let (events, gap, next) = ring.read(1, 10);
        assert_eq!((events.len(), gap, next), (2, false, 3));
        let (events, _, next) = ring.read(0, 2);
        assert_eq!((events.len(), next), (2, 2));
        push(&mut ring, CAPACITY);
        let (events, gap, next) = ring.read(1, 256);
        assert!(gap);
        assert_eq!(events[0].sequence.get(), 4);
        assert_eq!(next, 259);
        assert_eq!(ring.read(ring.latest(), 5), (vec![], false, ring.latest()));
        assert_eq!(ring.read(u64::MAX - 1, 5).2, ring.latest());
    }
}
