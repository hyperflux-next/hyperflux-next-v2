// SPDX-License-Identifier: GPL-2.0-only

//! The golden messages in tests/fixtures decode and re-encode byte for byte.
//! The C++ and Python SDK tests read the same files.

use hfx_proto::ids::RequestId;
use hfx_proto::messages::{
    BenchResult, CaptureResult, Empty, LeaseRenewed, LeaseResult, ReceiverProbe, RouteList,
    Snapshot, TransactResult, WaitResult, Welcome,
};
use hfx_proto::rpc::{
    Call, Reply, Validate, decode_reply, decode_request, encode_error, encode_request,
    encode_result,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        &format!("{name}.json"),
    ]
    .iter()
    .collect();
    let mut bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(bytes.pop(), Some(b'\n'), "{name} ends with one newline");
    bytes
}

fn reply_id(bytes: &[u8]) -> RequestId {
    let value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    RequestId::new(value["id"].as_str().unwrap()).unwrap()
}

fn round_trip_reply<T: DeserializeOwned + Validate + Serialize>(name: &str) {
    let bytes = fixture(name);
    let id = reply_id(&bytes);
    let encoded = match decode_reply::<T>(&bytes, &id).unwrap_or_else(|e| panic!("{name}: {e}")) {
        Reply::Ok(value) => encode_result(&id, &value).unwrap(),
        Reply::Err(error) => encode_error(&id, &error).unwrap(),
    };
    assert_eq!(
        String::from_utf8(encoded).unwrap(),
        String::from_utf8(bytes).unwrap(),
        "{name}"
    );
}

#[test]
fn requests_round_trip() {
    for name in [
        "hello-request",
        "snapshot-request",
        "wait-request",
        "list-routes-request",
        "acquire-lease-request",
        "renew-lease-request",
        "release-lease-request",
        "transact-request",
        "bench-request",
        "bench-interval-request",
        "bench-reread-busy-request",
        "capture-request",
        "probe-receiver-request",
    ] {
        let bytes = fixture(name);
        let request = decode_request(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        let encoded = encode_request(&request).unwrap();
        assert_eq!(
            String::from_utf8(encoded).unwrap(),
            String::from_utf8(bytes).unwrap(),
            "{name}"
        );
    }
}

#[test]
fn unknown_method_fixture() {
    let request = decode_request(&fixture("unknown-method-request")).unwrap();
    assert_eq!(request.call, Call::Unknown("teleport".into()));
}

#[test]
fn replies_round_trip() {
    round_trip_reply::<Welcome>("hello-reply");
    round_trip_reply::<Welcome>("hello-error-version");
    round_trip_reply::<Welcome>("hello-error-feature");
    round_trip_reply::<Snapshot>("snapshot-reply");
    round_trip_reply::<WaitResult>("wait-reply");
    round_trip_reply::<RouteList>("list-routes-reply");
    round_trip_reply::<LeaseResult>("acquire-lease-reply");
    round_trip_reply::<LeaseResult>("acquire-lease-busy-reply");
    round_trip_reply::<LeaseRenewed>("renew-lease-reply");
    round_trip_reply::<Empty>("release-lease-reply");
    round_trip_reply::<TransactResult>("transact-reply");
    round_trip_reply::<TransactResult>("transact-not-sent-reply");
    round_trip_reply::<TransactResult>("transact-may-have-been-sent-reply");
    round_trip_reply::<BenchResult>("bench-reply");
    round_trip_reply::<CaptureResult>("capture-reply");
    round_trip_reply::<ReceiverProbe>("probe-receiver-reply");
    round_trip_reply::<Empty>("error-feature-reply");
}

#[test]
fn contradictory_replies_are_rejected() {
    let id = RequestId::new("r8").unwrap();
    let matched_without_response =
        br#"{"id":"r8","result":{"outcome":"matched","response":null,"code":null}}"#;
    assert!(decode_reply::<TransactResult>(matched_without_response, &id).is_err());

    let id = RequestId::new("r4").unwrap();
    let mut routes: serde_json::Value =
        serde_json::from_slice(&fixture("list-routes-reply")).unwrap();
    routes["result"]["routes"][0]["state"] = "unavailable".into();
    let bytes = serde_json::to_vec(&routes).unwrap();
    assert!(decode_reply::<RouteList>(&bytes, &id).is_err());
}

#[test]
fn missing_nullable_fields() {
    let id = RequestId::new("r5").unwrap();
    let bytes = br#"{"id":"r5","result":{"outcome":"busy","holder":"OpenRGB"}}"#;
    let Reply::Ok(result) = decode_reply::<LeaseResult>(bytes, &id).unwrap() else {
        panic!("expected a result");
    };
    assert_eq!(result.lease_id, None);
}
