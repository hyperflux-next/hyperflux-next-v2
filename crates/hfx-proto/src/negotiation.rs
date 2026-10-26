// SPDX-License-Identifier: GPL-2.0-only

//! Version and feature negotiation (the `hello` method).
//!
//! The bridge serves a set of protocol versions, not a range, so a later
//! release can drop a version in the middle. Today the set is just `[1]`.

use crate::errors::ErrorCode;
use crate::ids::{Feature, ProtocolVersion};
use crate::messages::{Hello, MAX_HELLO_FEATURES};
use std::collections::BTreeSet;

pub const SERVED_VERSIONS: &[u16] = &[1];
pub const CURRENT_VERSION: u16 = 1;

/// Routes, leases, transact and the route list.
pub const FEATURE_NATIVE_ROUTES: &str = "native-routes";
/// The `wait` long-poll.
pub const FEATURE_WAIT: &str = "wait";
/// Opt-in latency measurement of shorter first-GET waits.
pub const FEATURE_BENCH: &str = "bench";
/// Bench's `get_interval_us` and `reread_busy`: another wait between
/// reads, and reading on after a busy answer.
pub const FEATURE_BENCH_INTERVAL: &str = "bench-interval";
/// Recording of receiver reports for adding new devices.
pub const FEATURE_CAPTURE: &str = "capture";
/// The bridge's fixed read-only queries to the receiver itself.
pub const FEATURE_PROBE_RECEIVER: &str = "probe-receiver";

/// The features this bridge serves, sorted ascending (the order of the
/// `features` list in the welcome).
pub const SERVED_FEATURES: &[&str] = &[
    FEATURE_BENCH,
    FEATURE_BENCH_INTERVAL,
    FEATURE_CAPTURE,
    FEATURE_NATIVE_ROUTES,
    FEATURE_PROBE_RECEIVER,
    FEATURE_WAIT,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Negotiated {
    pub version: ProtocolVersion,
    /// Sorted ascending.
    pub features: Vec<Feature>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NegotiationError {
    /// min_version > max_version.
    InvertedRange,
    NoCommonVersion,
    /// Required features the bridge doesn't serve, sorted.
    MissingFeatures(Vec<Feature>),
}

impl NegotiationError {
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::InvertedRange => ErrorCode::InvalidRequest,
            Self::NoCommonVersion => ErrorCode::NoCommonVersion,
            Self::MissingFeatures(_) => ErrorCode::FeatureUnavailable,
        }
    }

    /// Extra sentence for the wire message, if any.
    #[must_use]
    pub fn detail(&self) -> Option<String> {
        match self {
            Self::MissingFeatures(features) => Some(format!(
                "Missing features: {}.",
                features
                    .iter()
                    .map(Feature::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Self::InvertedRange | Self::NoCommonVersion => None,
        }
    }
}

/// `"1"` today; a later bridge serving 1 and 2 would give `"1, 2"`.
#[must_use]
pub fn supported_versions_text() -> String {
    SERVED_VERSIONS
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Structural rules a hello must meet to be decodable at all. Breaking one
/// makes the request invalid (the bridge closes the connection), the same
/// as malformed JSON.
#[must_use]
pub fn hello_is_canonical(hello: &Hello) -> bool {
    let required: BTreeSet<_> = hello.required_features.iter().collect();
    let optional: BTreeSet<_> = hello.optional_features.iter().collect();
    hello.required_features.len() <= MAX_HELLO_FEATURES
        && hello.optional_features.len() <= MAX_HELLO_FEATURES
        && required.len() == hello.required_features.len()
        && optional.len() == hello.optional_features.len()
        && required.is_disjoint(&optional)
}

/// Picks the highest served version inside the client's range and enables
/// every requested feature the bridge serves. Unknown optional features are
/// ignored, but unknown required ones fail.
///
/// # Errors
/// See [`NegotiationError`].
pub fn negotiate(hello: &Hello) -> Result<Negotiated, NegotiationError> {
    if hello.min_version > hello.max_version {
        return Err(NegotiationError::InvertedRange);
    }
    let version = SERVED_VERSIONS
        .iter()
        .rev()
        .copied()
        .find(|v| (hello.min_version.get()..=hello.max_version.get()).contains(v))
        .ok_or(NegotiationError::NoCommonVersion)?;

    let served = |feature: &Feature| SERVED_FEATURES.contains(&feature.as_str());
    let mut missing: Vec<Feature> = hello
        .required_features
        .iter()
        .filter(|f| !served(f))
        .cloned()
        .collect();
    if !missing.is_empty() {
        missing.sort();
        return Err(NegotiationError::MissingFeatures(missing));
    }

    let mut features: Vec<Feature> = hello
        .required_features
        .iter()
        .chain(&hello.optional_features)
        .filter(|f| served(f))
        .cloned()
        .collect();
    features.sort();
    Ok(Negotiated {
        version: ProtocolVersion::new(version).map_err(|_| NegotiationError::NoCommonVersion)?,
        features,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ClientId, ClientName};

    fn hello(min: u16, max: u16, required: &[&str], optional: &[&str]) -> Hello {
        Hello {
            client_id: ClientId::new("test-1").unwrap(),
            client_name: ClientName::new("Test").unwrap(),
            min_version: ProtocolVersion::new(min).unwrap(),
            max_version: ProtocolVersion::new(max).unwrap(),
            required_features: required.iter().map(|f| Feature::new(*f).unwrap()).collect(),
            optional_features: optional.iter().map(|f| Feature::new(*f).unwrap()).collect(),
        }
    }

    #[test]
    fn picks_version_one() {
        let result =
            negotiate(&hello(1, 9, &["native-routes"], &["wait", "future-thing"])).unwrap();
        assert_eq!(result.version.get(), 1);
        let names: Vec<_> = result.features.iter().map(Feature::as_str).collect();
        assert_eq!(names, ["native-routes", "wait"]);
    }

    #[test]
    fn no_common_version() {
        assert_eq!(
            negotiate(&hello(2, 8, &[], &[])),
            Err(NegotiationError::NoCommonVersion)
        );
        assert_eq!(
            negotiate(&hello(3, 2, &[], &[])),
            Err(NegotiationError::InvertedRange)
        );
    }

    #[test]
    fn missing_required_feature() {
        let error = negotiate(&hello(1, 1, &["zeta", "alpha", "wait"], &[])).unwrap_err();
        assert_eq!(error.code(), ErrorCode::FeatureUnavailable);
        assert_eq!(error.detail().unwrap(), "Missing features: alpha, zeta.");
    }

    #[test]
    fn duplicate_or_overlapping_features() {
        assert!(hello_is_canonical(&hello(1, 1, &["wait"], &["bench"])));
        assert!(!hello_is_canonical(&hello(1, 1, &["wait", "wait"], &[])));
        assert!(!hello_is_canonical(&hello(1, 1, &["wait"], &["wait"])));
        let many: Vec<String> = (0..17).map(|i| format!("f{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(!hello_is_canonical(&hello(1, 1, &[], &many)));
    }

    #[test]
    fn served_features_are_sorted() {
        let mut sorted = SERVED_FEATURES.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, SERVED_FEATURES);
    }
}
