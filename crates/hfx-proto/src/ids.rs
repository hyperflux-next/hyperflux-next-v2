// SPDX-License-Identifier: GPL-2.0-only

//! Scalar wire types.
//!
//! Each scalar gets its own type so it's checked once, while decoding, and
//! cannot be passed where a different kind of value belongs. A value outside
//! its bound makes the whole message invalid (docs/protocol.md, "Encoding").

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalarError {
    pub type_name: &'static str,
    pub reason: &'static str,
}

impl fmt::Display for ScalarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.type_name, self.reason)
    }
}

impl std::error::Error for ScalarError {}

macro_rules! bounded_string {
    ($(#[$meta:meta])* $name:ident, $min:expr, $max:expr) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Bounds count bytes, not characters.
            pub const MIN_LEN: usize = $min;
            pub const MAX_LEN: usize = $max;

            /// # Errors
            /// Fails when the length is outside the bound.
            pub fn new(value: impl Into<String>) -> Result<Self, ScalarError> {
                let value = value.into();
                if value.len() < Self::MIN_LEN || value.len() > Self::MAX_LEN {
                    return Err(ScalarError {
                        type_name: stringify!($name),
                        reason: "length out of bounds",
                    });
                }
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = ScalarError;
            fn try_from(value: &str) -> Result<Self, ScalarError> {
                Self::new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                Self::new(String::deserialize(d)?).map_err(de::Error::custom)
            }
        }
    };
}

bounded_string!(RequestId, 1, 128);
bounded_string!(
    /// One live connection per client id.
    ClientId, 1, 128
);
bounded_string!(ClientName, 1, 128);
bounded_string!(ReceiverId, 1, 128);
bounded_string!(DeviceId, 1, 128);
bounded_string!(LeaseId, 1, 128);
bounded_string!(ServerInstanceId, 1, 128);
bounded_string!(ComponentVersion, 1, 64);
bounded_string!(HumanMessage, 1, 512);
bounded_string!(Feature, 1, 64);
bounded_string!(DeviceName, 1, 128);
bounded_string!(
    /// An `HFX-nnn` code from the error catalog.
    ErrorCodeText, 1, 32
);
bounded_string!(
    /// Opaque; the bridge mints 64 lowercase hex digits but only the length
    /// is checked, so a garbled token is "unknown", not a protocol error.
    RouteToken, 64, 64
);

/// A `u64` carried as a canonical decimal string. Values above 2^53 then
/// survive JSON parsers that store numbers as doubles.
macro_rules! decimal_u64 {
    ($(#[$meta:meta])* $name:ident, $min:expr) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u64);

        impl $name {
            pub const MIN: u64 = $min;

            /// # Errors
            /// Fails below the minimum.
            pub const fn new(value: u64) -> Result<Self, ScalarError> {
                if value < Self::MIN {
                    return Err(ScalarError {
                        type_name: stringify!($name),
                        reason: "below the minimum",
                    });
                }
                Ok(Self(value))
            }

            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let value = d.deserialize_str(DecimalVisitor(stringify!($name)))?;
                Self::new(value).map_err(de::Error::custom)
            }
        }
    };
}

struct DecimalVisitor(&'static str);

impl Visitor<'_> for DecimalVisitor {
    type Value = u64;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a decimal string for {}", self.0)
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<u64, E> {
        parse_canonical_decimal(value)
            .ok_or_else(|| E::custom(format!("{}: not a canonical decimal string", self.0)))
    }
}

/// Accepts `0` or digits without a leading zero that fit in a `u64`.
#[must_use]
pub fn parse_canonical_decimal(value: &str) -> Option<u64> {
    let bytes = value.as_bytes();
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        return None;
    }
    value.parse().ok()
}

decimal_u64!(
    /// Counts receiver connections (>= 1). A replug gets a new one.
    GenerationId, 1
);
decimal_u64!(
    /// Change counter of a bridge stream; 0 means nothing happened yet.
    Sequence, 0
);
decimal_u64!(
    /// CLOCK_BOOTTIME in milliseconds on the bridge host. A client on the
    /// same host can compare it with its own CLOCK_BOOTTIME.
    MonotonicMs, 0
);

macro_rules! ranged_int {
    ($(#[$meta:meta])* $name:ident, $ty:ty, $min:expr, $max:expr) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name($ty);

        impl $name {
            pub const MIN: $ty = $min;
            pub const MAX: $ty = $max;

            /// # Errors
            /// Fails outside the bound.
            pub const fn new(value: $ty) -> Result<Self, ScalarError> {
                if value < Self::MIN || value > Self::MAX {
                    return Err(ScalarError {
                        type_name: stringify!($name),
                        reason: "out of range",
                    });
                }
                Ok(Self(value))
            }

            #[must_use]
            pub const fn get(self) -> $ty {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                self.0.serialize(s)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                Self::new(<$ty>::deserialize(d)?).map_err(de::Error::custom)
            }
        }
    };
}

ranged_int!(ProtocolVersion, u16, 1, u16::MAX);
ranged_int!(BatteryPercent, u8, 0, 100);
ranged_int!(LeaseDurationMs, u32, 1_000, 60_000);
ranged_int!(ExchangeTimeoutMs, u16, 1, 1_000);
ranged_int!(
    /// 0 asks for the current state without waiting.
    WaitTimeoutMs, u32, 0, 60_000
);
ranged_int!(EventLimit, u16, 1, 256);
ranged_int!(
    /// Only the benchmark may shorten the first-GET wait, and never lengthen
    /// it past the proven default.
    FirstGetDelayUs, u16, 1_000, 8_000
);
ranged_int!(
    /// Bench only: read again this soon after a reply that doesn't match.
    /// Outside bench the interval is always 1 ms.
    GetIntervalUs, u16, 1_000, 10_000
);
ranged_int!(BenchIterations, u16, 1, 100);

/// One 90-byte Razer feature report as exactly 180 lowercase hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReportHex([u8; crate::report::REPORT_LEN]);

impl ReportHex {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; crate::report::REPORT_LEN]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn bytes(&self) -> &[u8; crate::report::REPORT_LEN] {
        &self.0
    }

    /// # Errors
    /// Fails unless the text is exactly 180 lowercase hex digits.
    pub fn parse(text: &str) -> Result<Self, ScalarError> {
        let error = ScalarError {
            type_name: "ReportHex",
            reason: "not 180 lowercase hex digits",
        };
        let bytes = decode_lower_hex(text).ok_or(error)?;
        let array: [u8; crate::report::REPORT_LEN] = bytes.try_into().map_err(|_| error)?;
        Ok(Self(array))
    }
}

impl fmt::Debug for ReportHex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ReportHex({})", lower_hex(&self.0))
    }
}

impl Serialize for ReportHex {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&lower_hex(&self.0))
    }
}

impl<'de> Deserialize<'de> for ReportHex {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(d)?).map_err(de::Error::custom)
    }
}

/// Up to 90 bytes as lowercase hex (capture payloads).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HexBytes(Vec<u8>);

impl HexBytes {
    pub const MAX_BYTES: usize = crate::report::REPORT_LEN;

    /// # Errors
    /// Fails above 90 bytes.
    pub fn new(bytes: Vec<u8>) -> Result<Self, ScalarError> {
        if bytes.len() > Self::MAX_BYTES {
            return Err(ScalarError {
                type_name: "HexBytes",
                reason: "longer than 90 bytes",
            });
        }
        Ok(Self(bytes))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl Serialize for HexBytes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&lower_hex(&self.0))
    }
}

impl<'de> Deserialize<'de> for HexBytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        let bytes = decode_lower_hex(&text)
            .ok_or_else(|| de::Error::custom("HexBytes: not lowercase hex"))?;
        Self::new(bytes).map_err(de::Error::custom)
    }
}

#[must_use]
pub fn lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

/// Uppercase digits are rejected: there's exactly one valid spelling.
#[must_use]
pub fn decode_lower_hex(text: &str) -> Option<Vec<u8>> {
    const fn value(digit: u8) -> Option<u8> {
        match digit {
            b'0'..=b'9' => Some(digit - b'0'),
            b'a'..=b'f' => Some(digit - b'a' + 10),
            _ => None,
        }
    }
    let digits = text.as_bytes();
    if !digits.len().is_multiple_of(2) {
        return None;
    }
    digits
        .chunks_exact(2)
        .map(|pair| Some((value(pair[0])? << 4) | value(pair[1])?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_strings() {
        assert_eq!(parse_canonical_decimal("0"), Some(0));
        assert_eq!(
            parse_canonical_decimal("18446744073709551615"),
            Some(u64::MAX)
        );
        for bad in [
            "",
            "00",
            "01",
            "+1",
            "-1",
            " 1",
            "1 ",
            "1.0",
            "18446744073709551616",
        ] {
            assert_eq!(parse_canonical_decimal(bad), None, "{bad:?}");
        }
        assert!(serde_json::from_str::<GenerationId>("\"0\"").is_err());
        assert!(serde_json::from_str::<GenerationId>("1").is_err());
        let generation: GenerationId = serde_json::from_str("\"17\"").unwrap();
        assert_eq!(serde_json::to_string(&generation).unwrap(), "\"17\"");
    }

    #[test]
    fn string_bounds_count_bytes() {
        assert!(ClientId::new("").is_err());
        assert!(ClientId::new("a".repeat(128)).is_ok());
        assert!(ClientId::new("a".repeat(129)).is_err());
        assert!(ClientId::new("é".repeat(65)).is_err());
        assert!(RouteToken::new("a".repeat(63)).is_err());
    }

    #[test]
    fn integer_ranges() {
        assert!(serde_json::from_str::<LeaseDurationMs>("999").is_err());
        assert!(serde_json::from_str::<LeaseDurationMs>("60001").is_err());
        assert!(serde_json::from_str::<ExchangeTimeoutMs>("1001").is_err());
        assert!(serde_json::from_str::<FirstGetDelayUs>("8001").is_err());
        assert!(serde_json::from_str::<BatteryPercent>("101").is_err());
        assert!(serde_json::from_str::<ProtocolVersion>("1.0").is_err());
        assert!(serde_json::from_str::<WaitTimeoutMs>("0").is_ok());
    }

    #[test]
    fn report_hex() {
        let mut bytes = [0_u8; 90];
        bytes[0] = 0xab;
        bytes[89] = 0x0f;
        let text = lower_hex(&bytes);
        assert_eq!(text.len(), 180);
        assert_eq!(
            ReportHex::parse(&text).unwrap(),
            ReportHex::from_bytes(bytes)
        );
        assert!(ReportHex::parse(&text.to_uppercase()).is_err());
        assert!(ReportHex::parse(&text[..178]).is_err());
        assert!(ReportHex::parse(&format!("{text}00")).is_err());
    }
}
