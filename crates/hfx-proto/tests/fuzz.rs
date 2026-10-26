// SPDX-License-Identifier: GPL-2.0-only

//! Property tests for the parsers that read bytes from another process:
//! frames, requests and replies. They must never panic, and what they
//! accept must survive a round trip.

use hfx_proto::framing::{FrameReader, MAX_MESSAGE_BYTES, encode_frame};
use hfx_proto::ids::{ReportHex, RequestId};
use hfx_proto::messages::{RouteList, Snapshot, TransactResult};
use hfx_proto::rpc::{decode_reply, decode_request, encode_request};
use proptest::prelude::*;

proptest! {
    #[test]
    fn frame_reader_never_panics(chunks in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..64), 0..16)) {
        let mut reader = FrameReader::new();
        for chunk in &chunks {
            if reader.push(chunk).is_err() {
                break;
            }
            while let Ok(Some(frame)) = reader.next_frame() {
                prop_assert!(!frame.is_empty() && frame.len() <= MAX_MESSAGE_BYTES);
            }
        }
        prop_assert!(reader.room() <= FrameReader::MAX_BUFFERED);
    }

    #[test]
    fn frames_survive_any_split(payload in prop::collection::vec(any::<u8>(), 1..300), split in 0usize..304) {
        let frame = encode_frame(&payload).unwrap();
        let split = split.min(frame.len());
        let mut reader = FrameReader::new();
        reader.push(&frame[..split]).unwrap();
        let early = reader.next_frame().unwrap();
        reader.push(&frame[split..]).unwrap();
        let frame_out = early.or_else(|| reader.next_frame().unwrap());
        prop_assert_eq!(frame_out, Some(payload));
    }

    #[test]
    fn request_decoder_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = decode_request(&bytes);
    }

    #[test]
    fn accepted_requests_round_trip(text in "\\{\"id\":\"[a-z0-9]{1,8}\",\"method\":\"(snapshot|list-routes|hello|wait)\",\"params\":\\{\\}\\}") {
        if let Ok(request) = decode_request(text.as_bytes()) {
            let again = decode_request(&encode_request(&request).unwrap()).unwrap();
            prop_assert_eq!(again, request);
        }
    }

    #[test]
    fn reply_decoder_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let id = RequestId::new("r1").unwrap();
        let _ = decode_reply::<Snapshot>(&bytes, &id);
        let _ = decode_reply::<RouteList>(&bytes, &id);
        let _ = decode_reply::<TransactResult>(&bytes, &id);
    }

    #[test]
    fn report_hex_round_trips(bytes in prop::collection::vec(any::<u8>(), 90)) {
        let array: [u8; 90] = bytes.try_into().unwrap();
        let hex = serde_json::to_string(&ReportHex::from_bytes(array)).unwrap();
        let back: ReportHex = serde_json::from_str(&hex).unwrap();
        prop_assert_eq!(back.bytes(), &array);
    }
}
