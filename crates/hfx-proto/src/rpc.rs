// SPDX-License-Identifier: GPL-2.0-only

//! Request and reply envelopes.
//!
//! ```text
//! request: {"id": RequestId, "method": name, "params": {...}}
//! reply:   {"id": RequestId, "result": {...}}
//!        | {"id": RequestId, "error": {"code": "HFX-nnn", "message": "..."}}
//! ```
//!
//! A payload that fails [`decode_request`] is a protocol violation and the
//! bridge closes the connection without replying. A well-formed request for
//! an unknown method decodes to [`Call::Unknown`] so the bridge can answer
//! HFX-102 and keep the connection.

use crate::framing::MAX_MESSAGE_BYTES;
use crate::ids::RequestId;
use crate::messages::{
    AcquireLease, Bench, BenchResult, Capture, CaptureResult, Empty, Hello, LeaseOutcome,
    LeaseRenewed, LeaseResult, MAX_BENCH_SAMPLES, MAX_CAPTURE_EVENTS, MAX_DEVICES_PER_RECEIVER,
    MAX_RECEIVERS, MAX_ROUTES, ProbeReceiver, ROUTE_REPORT_BYTES, ReceiverProbe, ReleaseLease,
    RenewLease, Route, RouteAccess, RouteList, RouteState, RpcError, Snapshot, Transact,
    TransactOutcome, TransactResult, WaitParams, WaitResult, Welcome,
};
use crate::negotiation::{
    FEATURE_BENCH, FEATURE_CAPTURE, FEATURE_NATIVE_ROUTES, FEATURE_PROBE_RECEIVER, FEATURE_WAIT,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    Hello,
    Snapshot,
    Wait,
    ListRoutes,
    AcquireLease,
    RenewLease,
    ReleaseLease,
    Transact,
    Bench,
    Capture,
    ProbeReceiver,
}

impl Method {
    pub const ALL: [Self; 11] = [
        Self::Hello,
        Self::Snapshot,
        Self::Wait,
        Self::ListRoutes,
        Self::AcquireLease,
        Self::RenewLease,
        Self::ReleaseLease,
        Self::Transact,
        Self::Bench,
        Self::Capture,
        Self::ProbeReceiver,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::Snapshot => "snapshot",
            Self::Wait => "wait",
            Self::ListRoutes => "list-routes",
            Self::AcquireLease => "acquire-lease",
            Self::RenewLease => "renew-lease",
            Self::ReleaseLease => "release-lease",
            Self::Transact => "transact",
            Self::Bench => "bench",
            Self::Capture => "capture",
            Self::ProbeReceiver => "probe-receiver",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }

    /// The feature that must be enabled in hello before the method may be
    /// called. `hello` and `snapshot` need none.
    #[must_use]
    pub const fn feature(self) -> Option<&'static str> {
        match self {
            Self::Hello | Self::Snapshot => None,
            Self::Wait => Some(FEATURE_WAIT),
            Self::ListRoutes
            | Self::AcquireLease
            | Self::RenewLease
            | Self::ReleaseLease
            | Self::Transact => Some(FEATURE_NATIVE_ROUTES),
            Self::Bench => Some(FEATURE_BENCH),
            Self::Capture => Some(FEATURE_CAPTURE),
            Self::ProbeReceiver => Some(FEATURE_PROBE_RECEIVER),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Hello(Hello),
    Snapshot,
    Wait(WaitParams),
    ListRoutes,
    AcquireLease(AcquireLease),
    RenewLease(RenewLease),
    ReleaseLease(ReleaseLease),
    Transact(Transact),
    Bench(Bench),
    Capture(Capture),
    ProbeReceiver(ProbeReceiver),
    /// Well-formed envelope, method name this version doesn't know.
    Unknown(String),
}

impl Call {
    #[must_use]
    pub fn method(&self) -> Option<Method> {
        Some(match self {
            Self::Hello(_) => Method::Hello,
            Self::Snapshot => Method::Snapshot,
            Self::Wait(_) => Method::Wait,
            Self::ListRoutes => Method::ListRoutes,
            Self::AcquireLease(_) => Method::AcquireLease,
            Self::RenewLease(_) => Method::RenewLease,
            Self::ReleaseLease(_) => Method::ReleaseLease,
            Self::Transact(_) => Method::Transact,
            Self::Bench(_) => Method::Bench,
            Self::Capture(_) => Method::Capture,
            Self::ProbeReceiver(_) => Method::ProbeReceiver,
            Self::Unknown(_) => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: RequestId,
    pub call: Call,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Not JSON, wrong shape, unknown or duplicate field, value out of bounds.
    Json(String),
    /// Decodes, but breaks a rule that isn't expressible in the types.
    NotCanonical(&'static str),
    /// The reply's id is not the id of the request it answers.
    IdMismatch,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(message) => write!(f, "invalid message: {message}"),
            Self::NotCanonical(rule) => write!(f, "invalid message: {rule}"),
            Self::IdMismatch => f.write_str("reply id doesn't match the request"),
        }
    }
}

impl std::error::Error for DecodeError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncodeError {
    Json(String),
    TooLarge(usize),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(message) => write!(f, "encoding failed: {message}"),
            Self::TooLarge(n) => write!(f, "message of {n} bytes is above {MAX_MESSAGE_BYTES}"),
        }
    }
}

impl std::error::Error for EncodeError {}

fn json_error(error: &serde_json::Error) -> DecodeError {
    DecodeError::Json(error.to_string())
}

/// Longest method name accepted in an envelope.
const MAX_METHOD_NAME: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRequest<'a> {
    id: RequestId,
    method: String,
    #[serde(borrow)]
    params: &'a RawValue,
}

fn params<T: DeserializeOwned>(raw: &RawValue) -> Result<T, DecodeError> {
    serde_json::from_str(raw.get()).map_err(|e| json_error(&e))
}

/// # Errors
/// Any [`DecodeError`] means the bridge must close the connection.
pub fn decode_request(payload: &[u8]) -> Result<Request, DecodeError> {
    let raw: RawRequest<'_> = serde_json::from_slice(payload).map_err(|e| json_error(&e))?;
    let call = match Method::from_name(&raw.method) {
        Some(Method::Hello) => {
            let hello: Hello = params(raw.params)?;
            if !crate::negotiation::hello_is_canonical(&hello) {
                return Err(DecodeError::NotCanonical(
                    "hello features must be unique, disjoint and at most 16 per list",
                ));
            }
            Call::Hello(hello)
        }
        Some(Method::Snapshot) => {
            params::<Empty>(raw.params)?;
            Call::Snapshot
        }
        Some(Method::Wait) => Call::Wait(params(raw.params)?),
        Some(Method::ListRoutes) => {
            params::<Empty>(raw.params)?;
            Call::ListRoutes
        }
        Some(Method::AcquireLease) => Call::AcquireLease(params(raw.params)?),
        Some(Method::RenewLease) => Call::RenewLease(params(raw.params)?),
        Some(Method::ReleaseLease) => Call::ReleaseLease(params(raw.params)?),
        Some(Method::Transact) => Call::Transact(params(raw.params)?),
        Some(Method::Bench) => Call::Bench(params(raw.params)?),
        Some(Method::Capture) => Call::Capture(params(raw.params)?),
        Some(Method::ProbeReceiver) => Call::ProbeReceiver(params(raw.params)?),
        None => {
            if raw.method.is_empty() || raw.method.len() > MAX_METHOD_NAME {
                return Err(DecodeError::NotCanonical(
                    "method name length out of bounds",
                ));
            }
            if !raw.params.get().trim_start().starts_with('{') {
                return Err(DecodeError::NotCanonical("params must be an object"));
            }
            Call::Unknown(raw.method)
        }
    };
    Ok(Request { id: raw.id, call })
}

fn finish(payload: String) -> Result<Vec<u8>, EncodeError> {
    if payload.len() > MAX_MESSAGE_BYTES {
        return Err(EncodeError::TooLarge(payload.len()));
    }
    Ok(payload.into_bytes())
}

fn to_json<T: Serialize + ?Sized>(value: &T) -> Result<String, EncodeError> {
    serde_json::to_string(value).map_err(|e| EncodeError::Json(e.to_string()))
}

/// Client side.
///
/// # Errors
/// Fails for [`Call::Unknown`] (clients never send unknown methods) and for
/// oversized messages.
pub fn encode_request(request: &Request) -> Result<Vec<u8>, EncodeError> {
    let (method, params) = match &request.call {
        Call::Hello(p) => (Method::Hello, to_json(p)?),
        Call::Snapshot => (Method::Snapshot, to_json(&Empty {})?),
        Call::Wait(p) => (Method::Wait, to_json(p)?),
        Call::ListRoutes => (Method::ListRoutes, to_json(&Empty {})?),
        Call::AcquireLease(p) => (Method::AcquireLease, to_json(p)?),
        Call::RenewLease(p) => (Method::RenewLease, to_json(p)?),
        Call::ReleaseLease(p) => (Method::ReleaseLease, to_json(p)?),
        Call::Transact(p) => (Method::Transact, to_json(p)?),
        Call::Bench(p) => (Method::Bench, to_json(p)?),
        Call::Capture(p) => (Method::Capture, to_json(p)?),
        Call::ProbeReceiver(p) => (Method::ProbeReceiver, to_json(p)?),
        Call::Unknown(name) => return Err(EncodeError::Json(format!("unknown method {name}"))),
    };
    finish(format!(
        "{{\"id\":{},\"method\":{},\"params\":{params}}}",
        to_json(&request.id)?,
        to_json(method.name())?
    ))
}

/// Bridge side: a successful reply.
///
/// # Errors
/// Fails for oversized results.
pub fn encode_result<T: Serialize>(id: &RequestId, result: &T) -> Result<Vec<u8>, EncodeError> {
    finish(format!(
        "{{\"id\":{},\"result\":{}}}",
        to_json(id)?,
        to_json(result)?
    ))
}

/// Bridge side: an error reply.
///
/// # Errors
/// Fails if serialization fails, which bounded fields rule out.
pub fn encode_error(id: &RequestId, error: &RpcError) -> Result<Vec<u8>, EncodeError> {
    finish(format!(
        "{{\"id\":{},\"error\":{}}}",
        to_json(id)?,
        to_json(error)?
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply<T> {
    Ok(T),
    Err(RpcError),
}

/// Rules a decoded result must meet beyond its types. Clients reject a
/// reply that breaks them instead of acting on contradictory data.
pub trait Validate {
    /// # Errors
    /// Returns the rule that was broken.
    fn validate(&self) -> Result<(), &'static str> {
        Ok(())
    }
}

impl Validate for Empty {}
impl Validate for WaitResult {}
impl Validate for LeaseRenewed {}
impl Validate for ReceiverProbe {}

impl Validate for Welcome {
    fn validate(&self) -> Result<(), &'static str> {
        if self.features.windows(2).all(|pair| pair[0] < pair[1]) {
            Ok(())
        } else {
            Err("welcome features must be sorted and unique")
        }
    }
}

impl Validate for Snapshot {
    fn validate(&self) -> Result<(), &'static str> {
        if self.receivers.len() > MAX_RECEIVERS {
            return Err("too many receivers");
        }
        if self
            .receivers
            .iter()
            .any(|r| r.devices.len() > MAX_DEVICES_PER_RECEIVER)
        {
            return Err("too many devices on one receiver");
        }
        Ok(())
    }
}

fn route_key(route: &Route) -> (&str, u64, &str) {
    (
        route.receiver_id.as_str(),
        route.generation_id.get(),
        route.device_id.as_str(),
    )
}

impl Validate for RouteList {
    fn validate(&self) -> Result<(), &'static str> {
        if self.routes.len() > MAX_ROUTES {
            return Err("too many routes");
        }
        if !self
            .routes
            .windows(2)
            .all(|p| route_key(&p[0]) < route_key(&p[1]))
        {
            return Err("routes must be sorted and unique");
        }
        for route in &self.routes {
            let writable =
                route.access == RouteAccess::Qualified && route.state == RouteState::Available;
            if writable != route.route_token.is_some() {
                return Err("a route token must be present exactly on qualified available routes");
            }
            if route.report_bytes != ROUTE_REPORT_BYTES {
                return Err("routes carry 90-byte reports");
            }
        }
        Ok(())
    }
}

impl Validate for LeaseResult {
    fn validate(&self) -> Result<(), &'static str> {
        let consistent = match self.outcome {
            LeaseOutcome::Granted => {
                self.lease_id.is_some() && self.expires_ms.is_some() && self.holder.is_none()
            }
            LeaseOutcome::Busy => self.lease_id.is_none() && self.expires_ms.is_none(),
        };
        if consistent {
            Ok(())
        } else {
            Err("lease result fields contradict its outcome")
        }
    }
}

impl Validate for TransactResult {
    fn validate(&self) -> Result<(), &'static str> {
        let consistent = match self.outcome {
            TransactOutcome::Matched => self.response.is_some() && self.code.is_none(),
            TransactOutcome::NotSent | TransactOutcome::MayHaveBeenSent => {
                self.response.is_none() && self.code.is_some()
            }
        };
        if consistent {
            Ok(())
        } else {
            Err("transact result fields contradict its outcome")
        }
    }
}

impl Validate for BenchResult {
    fn validate(&self) -> Result<(), &'static str> {
        if self.samples.len() > MAX_BENCH_SAMPLES {
            Err("too many bench samples")
        } else {
            Ok(())
        }
    }
}

impl Validate for CaptureResult {
    fn validate(&self) -> Result<(), &'static str> {
        if self.events.len() > MAX_CAPTURE_EVENTS {
            return Err("too many capture events");
        }
        if !self
            .events
            .windows(2)
            .all(|p| p[0].sequence < p[1].sequence)
        {
            return Err("capture events must be in sequence order");
        }
        if self.events.last().is_some_and(|e| e.sequence > self.next) {
            return Err("capture next must not be behind its events");
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReply<'a> {
    id: RequestId,
    #[serde(default, borrow)]
    result: Option<&'a RawValue>,
    #[serde(default)]
    error: Option<RpcError>,
}

/// Client side. `expected_id` is the id of the request this answers.
///
/// # Errors
/// Any error means the client should drop the connection.
pub fn decode_reply<T: DeserializeOwned + Validate>(
    payload: &[u8],
    expected_id: &RequestId,
) -> Result<Reply<T>, DecodeError> {
    let raw: RawReply<'_> = serde_json::from_slice(payload).map_err(|e| json_error(&e))?;
    if &raw.id != expected_id {
        return Err(DecodeError::IdMismatch);
    }
    match (raw.result, raw.error) {
        (Some(result), None) => {
            let value: T = serde_json::from_str(result.get()).map_err(|e| json_error(&e))?;
            value.validate().map_err(DecodeError::NotCanonical)?;
            Ok(Reply::Ok(value))
        }
        (None, Some(error)) => Ok(Reply::Err(error)),
        _ => Err(DecodeError::NotCanonical(
            "a reply has exactly one of result and error",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> RequestId {
        RequestId::new(text).unwrap()
    }

    #[test]
    fn unknown_method_keeps_the_connection() {
        let request =
            decode_request(br#"{"id":"r1","method":"teleport","params":{"x":1}}"#).unwrap();
        assert_eq!(request.call, Call::Unknown("teleport".into()));
        assert!(decode_request(br#"{"id":"r1","method":"teleport","params":[]}"#).is_err());
    }

    #[test]
    fn malformed_envelopes_are_rejected() {
        for bad in [
            &br#"{"id":"r1","method":"snapshot"}"#[..],
            br#"{"id":"r1","method":"snapshot","params":{},"extra":1}"#,
            br#"{"id":"r1","id":"r2","method":"snapshot","params":{}}"#,
            br#"{"id":"","method":"snapshot","params":{}}"#,
            br#"{"id":"r1","method":"snapshot","params":{"a":1}}"#,
            br#"[]"#,
            b"\xff",
        ] {
            assert!(
                decode_request(bad).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn request_round_trip() {
        let request = Request {
            id: id("r7"),
            call: Call::ListRoutes,
        };
        let bytes = encode_request(&request).unwrap();
        assert_eq!(bytes, br#"{"id":"r7","method":"list-routes","params":{}}"#);
        assert_eq!(decode_request(&bytes).unwrap(), request);
    }

    #[test]
    fn reply_must_match_its_request() {
        let bytes = encode_result(&id("r1"), &Empty {}).unwrap();
        assert_eq!(bytes, br#"{"id":"r1","result":{}}"#);
        assert!(matches!(
            decode_reply::<Empty>(&bytes, &id("r1")),
            Ok(Reply::Ok(_))
        ));
        assert_eq!(
            decode_reply::<Empty>(&bytes, &id("r2")),
            Err(DecodeError::IdMismatch)
        );
        assert!(decode_reply::<Empty>(br#"{"id":"r1"}"#, &id("r1")).is_err());
    }

    #[test]
    fn method_names_round_trip() {
        for method in Method::ALL {
            assert_eq!(Method::from_name(method.name()), Some(method));
        }
    }
}
