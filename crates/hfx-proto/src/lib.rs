// SPDX-License-Identifier: GPL-2.0-only

//! The contract shared by the Rust parts of HyperFlux Next V2: the client
//! protocol (version 1), the error catalog and the device table.
//! docs/protocol.md is the normative description. This crate implements it.

#![forbid(unsafe_code)]

pub mod client;
pub mod clock;
pub mod devices;
pub mod errors;
pub mod framing;
pub mod ids;
pub mod messages;
pub mod negotiation;
pub mod openrazer;
pub mod paths;
pub mod present;
pub mod report;
pub mod rpc;

/// The product version, from the workspace manifest.
pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");
