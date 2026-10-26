// SPDX-License-Identifier: GPL-2.0-only

//! The client against a scripted bridge on a socket pair.

use hfx_proto::client::{Client, ClientConfig, ClientError};
use hfx_proto::errors::{ErrorCode, rpc_error};
use hfx_proto::framing::{read_frame, write_frame};
use hfx_proto::ids::{ComponentVersion, Feature, ProtocolVersion, ServerInstanceId};
use hfx_proto::messages::{Snapshot, Welcome};
use hfx_proto::rpc::{Call, decode_request, encode_error, encode_result};
use std::os::unix::net::UnixStream;
use std::thread;

fn welcome() -> Welcome {
    Welcome {
        version: ProtocolVersion::new(1).unwrap(),
        server_instance_id: ServerInstanceId::new("bridge-test").unwrap(),
        bridge_version: ComponentVersion::new("1.0.0").unwrap(),
        features: vec![Feature::new("wait").unwrap()],
    }
}

/// Answers each request with `answer`, then closes.
fn fake_bridge(mut stream: UnixStream, answer: fn(&hfx_proto::rpc::Request) -> Vec<u8>) {
    while let Ok(Some(payload)) = read_frame(&mut stream) {
        let request = decode_request(&payload).unwrap();
        write_frame(&mut stream, &answer(&request)).unwrap();
    }
}

#[test]
fn hello_then_snapshot() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let server = thread::spawn(move || {
        fake_bridge(theirs, |request| match &request.call {
            Call::Hello(hello) => {
                assert!(hello.client_id.as_str().starts_with("test-"));
                encode_result(&request.id, &welcome()).unwrap()
            }
            Call::Snapshot => {
                let snapshot: Snapshot = serde_json::from_str(
                    r#"{"revision":"1","now_ms":"10","transport":"hidraw","receivers":[]}"#,
                )
                .unwrap();
                encode_result(&request.id, &snapshot).unwrap()
            }
            other => panic!("unexpected {other:?}"),
        });
    });
    let config = ClientConfig::new("test", "Test", &[], &["wait"]);
    let mut client = Client::handshake(ours, &config).unwrap();
    assert!(client.has_feature("wait"));
    assert_eq!(client.snapshot().unwrap().revision.get(), 1);
    drop(client);
    server.join().unwrap();
}

#[test]
fn version_refusal_is_reported() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let server = thread::spawn(move || {
        fake_bridge(theirs, |request| {
            encode_error(&request.id, &rpc_error(ErrorCode::NoCommonVersion)).unwrap()
        });
    });
    let config = ClientConfig::new("test", "Test", &[], &[]);
    let error = Client::handshake(ours, &config).err().unwrap();
    assert_eq!(error.code(), Some("HFX-101"));
    server.join().unwrap();
}

#[test]
fn closed_connection() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    drop(theirs);
    let config = ClientConfig::new("test", "Test", &[], &[]);
    assert!(matches!(
        Client::handshake(ours, &config),
        Err(ClientError::Closed | ClientError::Frame(_))
    ));
}

#[test]
fn client_ids_are_unique() {
    let a = hfx_proto::client::unique_client_id("hyperfluxctl");
    let b = hfx_proto::client::unique_client_id("hyperfluxctl");
    assert_ne!(a, b);
    assert!(
        a.as_str()
            .starts_with(&format!("hyperfluxctl-{}-", std::process::id()))
    );
}
