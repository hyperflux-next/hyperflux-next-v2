// SPDX-License-Identifier: GPL-2.0-only

//! Request parameters and results of protocol version 1.
//!
//! docs/protocol.md is the normative description. Field names, order and
//! enum spellings here must match it. Fields are encoded in declaration
//! order, nullable fields are always present (as `null` when empty), and
//! unknown fields are rejected.

use crate::ids::{
    BatteryPercent, BenchIterations, ClientId, ClientName, ComponentVersion, DeviceId, DeviceName,
    ErrorCodeText, EventLimit, ExchangeTimeoutMs, Feature, FirstGetDelayUs, GenerationId,
    GetIntervalUs, HexBytes, HumanMessage, LeaseDurationMs, LeaseId, MonotonicMs, ProtocolVersion,
    ReceiverId, ReportHex, RouteToken, Sequence, ServerInstanceId, WaitTimeoutMs,
};
use serde::{Deserialize, Serialize};

/// Most features a client may list in each of `required_features` and
/// `optional_features`.
pub const MAX_HELLO_FEATURES: usize = 16;
pub const MAX_RECEIVERS: usize = 16;
pub const MAX_DEVICES_PER_RECEIVER: usize = 2;
pub const MAX_ROUTES: usize = 32;
pub const MAX_BENCH_SAMPLES: usize = 100;
pub const MAX_CAPTURE_EVENTS: usize = 256;
/// The size of the Razer feature reports a route carries.
pub const ROUTE_REPORT_BYTES: u16 = 90;

/// Params of methods that take none: always `{}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

// ---------------------------------------------------------------- hello

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub client_id: ClientId,
    pub client_name: ClientName,
    pub min_version: ProtocolVersion,
    pub max_version: ProtocolVersion,
    pub required_features: Vec<Feature>,
    pub optional_features: Vec<Feature>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Welcome {
    pub version: ProtocolVersion,
    pub server_instance_id: ServerInstanceId,
    pub bridge_version: ComponentVersion,
    /// The enabled features, sorted ascending.
    pub features: Vec<Feature>,
}

// ---------------------------------------------------------------- snapshot

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lane {
    Pointer,
    Keyboard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceKind {
    Mouse,
    Keyboard,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReceiverState {
    Active,
    Suspended,
}

/// How much is known about a device model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Support {
    /// Verified on real hardware through the receiver.
    Tested,
    /// Listed by Razer for the receiver and driven by the pinned OpenRazer
    /// or OpenRGB, with a catalog entry that matches theirs, but not tested
    /// through the receiver.
    Checked,
    /// Listed by Razer for the receiver, but no pinned release drives it.
    Listed,
    /// Product id not in the device table.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Presence {
    /// The device sent something since it was last seen leaving.
    Online,
    /// Its last answer said it is asleep or switched off (status 0x04).
    Asleep,
    /// The receiver reported the device gone.
    Unreachable,
    /// Paired, but nothing heard from it yet, or since the system resumed.
    Waiting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Contact {
    OnMat,
    OffMat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Charge {
    Charging,
    Discharging,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Battery {
    #[serde(default)]
    pub level: Option<BatteryPercent>,
    /// Null when unknown or when the reading is stale. "Full" is charging
    /// at 100 %. The bridge doesn't report it separately.
    #[serde(default)]
    pub charge: Option<Charge>,
    #[serde(default)]
    pub updated_ms: Option<MonotonicMs>,
    /// The last reading is older than `BATTERY_STALE_AFTER_MS`.
    pub stale: bool,
    /// The level went up at its last change and has not gone down since.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rising: bool,
    /// What the bridge has no way to learn from this device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreported: Option<Unreported>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Unreported {
    /// Nothing says whether it charges: no status decoder for it, and it
    /// is not asked (not a tested or checked device) or couldn't answer 07/84
    /// ("not supported", or an answer too short to hold a value).
    Charge,
    /// It answered the battery query with "not supported" and has not sent
    /// a status report.
    Battery,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceInfo {
    pub device_id: DeviceId,
    pub lane: Lane,
    pub kind: DeviceKind,
    pub vendor_id: u16,
    pub product_id: u16,
    #[serde(default)]
    pub name: Option<DeviceName>,
    pub support: Support,
    pub presence: Presence,
    #[serde(default)]
    pub last_seen_ms: Option<MonotonicMs>,
    #[serde(default)]
    pub contact: Option<Contact>,
    /// A qualified route is available right now.
    pub controllable: bool,
    /// Name of the client holding the device's lease, if any.
    #[serde(default)]
    pub controlled_by: Option<ClientName>,
    pub battery: Battery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_get: Option<FirstGet>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused: Vec<Refused>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<Slot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openrazer: Option<OpenRazerDevice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exchanges: Option<Exchanges>,
}

/// How the device's exchanges went since the bridge started: the bridge's
/// own and the apps', not bench's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exchanges {
    pub sent: u32,
    /// Answered at the first read.
    pub first_read: u32,
    /// Reads again after a reply that answered something else.
    pub reread_other: u32,
    pub reread_busy: u32,
    /// Ended without a matching answer.
    pub no_answer: u32,
}

/// What the receiver's paired slots say about the device: connected
/// through the receiver now, or not (off, or on its cable).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Slot {
    Connected,
    NotConnected,
}

/// The device's virtual OpenRazer device, as the bridge sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRazerDevice {
    pub state: OpenRazerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<HumanMessage>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpenRazerState {
    /// Created, for the patched driver to bind.
    Live,
    /// A wired device of the model is bound, and OpenRazer has that one.
    Cable,
    /// Reading the serial before creating it.
    Starting,
    /// A step failed and is tried again after a backoff.
    Retrying,
    /// Tried again in 5 minutes.
    Blocked,
    /// Nothing to create: no route, no OpenRazer support for the model, or
    /// no patched driver.
    NotOffered,
}

/// The first-GET wait the bridge uses for a device now, the catalog's, and
/// the one kept from an earlier run that learning went on from, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirstGet {
    pub delay_us: u16,
    pub learned_from_us: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_us: Option<u16>,
}

/// A command the device's list lacks (class, id and data size), and how
/// often the bridge refused it since the device paired.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Refused {
    pub class: u8,
    pub id: u8,
    pub size: u8,
    pub count: u32,
}

/// What named the receiver's devices: its paired slots, or failing them
/// the pairing inventory read at opening, by its marker where the tested
/// receiver has it or elsewhere. Otherwise nothing could: the inventory
/// isn't recognised, or there was none to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Inventory {
    Slots,
    KnownOffset,
    Marker,
    NotRecognised,
    Absent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiverInfo {
    pub receiver_id: ReceiverId,
    pub generation_id: GenerationId,
    pub vendor_id: u16,
    pub product_id: u16,
    pub state: ReceiverState,
    /// The bridge can send to this receiver.
    pub writable: bool,
    pub devices: Vec<DeviceInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory: Option<Inventory>,
}

/// How the bridge reaches receivers. There is one way today. The field
/// stays so a second one wouldn't change the snapshot's shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    /// The receiver's hidraw nodes, under the kernel's own HID drivers.
    Hidraw,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub revision: Sequence,
    pub now_ms: MonotonicMs,
    pub transport: Transport,
    pub receivers: Vec<ReceiverInfo>,
}

// ---------------------------------------------------------------- wait

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitParams {
    #[serde(default)]
    pub state_after: Option<Sequence>,
    #[serde(default)]
    pub routes_after: Option<Sequence>,
    pub timeout_ms: WaitTimeoutMs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitResult {
    pub state: Sequence,
    pub routes: Sequence,
    pub timed_out: bool,
}

// ---------------------------------------------------------------- routes

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RouteState {
    Available,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RouteAccess {
    Qualified,
    Unsupported,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    /// Present exactly when `access` is qualified and `state` is available.
    #[serde(default)]
    pub route_token: Option<RouteToken>,
    pub receiver_id: ReceiverId,
    pub generation_id: GenerationId,
    pub device_id: DeviceId,
    pub lane: Lane,
    pub kind: DeviceKind,
    pub vendor_id: u16,
    pub product_id: u16,
    pub state: RouteState,
    pub access: RouteAccess,
    pub report_bytes: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteList {
    pub revision: Sequence,
    pub routes: Vec<Route>,
}

// ---------------------------------------------------------------- leases

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcquireLease {
    pub route_token: RouteToken,
    pub duration_ms: LeaseDurationMs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LeaseOutcome {
    Granted,
    Busy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseResult {
    pub outcome: LeaseOutcome,
    #[serde(default)]
    pub lease_id: Option<LeaseId>,
    #[serde(default)]
    pub expires_ms: Option<MonotonicMs>,
    /// Set when busy: the name of the client that holds the device.
    #[serde(default)]
    pub holder: Option<ClientName>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenewLease {
    pub lease_id: LeaseId,
    pub duration_ms: LeaseDurationMs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseRenewed {
    pub lease_id: LeaseId,
    pub expires_ms: MonotonicMs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseLease {
    pub lease_id: LeaseId,
}

// ---------------------------------------------------------------- transact

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transact {
    pub lease_id: LeaseId,
    pub route_token: RouteToken,
    pub report: ReportHex,
    pub timeout_ms: ExchangeTimeoutMs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransactOutcome {
    Matched,
    /// Provably never reached the receiver. The client may retry.
    NotSent,
    /// May have reached the device. Never resend it automatically.
    MayHaveBeenSent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactResult {
    pub outcome: TransactOutcome,
    #[serde(default)]
    pub response: Option<ReportHex>,
    #[serde(default)]
    pub code: Option<ErrorCodeText>,
}

// ---------------------------------------------------------------- bench

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BenchQuery {
    /// Command 00/81.
    Firmware,
    /// Command 07/80.
    Battery,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bench {
    pub lease_id: LeaseId,
    pub route_token: RouteToken,
    pub query: BenchQuery,
    pub first_get_delay_us: FirstGetDelayUs,
    pub iterations: BenchIterations,
    /// Sent with the `bench-interval` feature and left out without it, so a
    /// bridge that does not know the field never sees it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub get_interval_us: Option<GetIntervalUs>,
    /// Read again after a matching busy answer instead of ending there.
    /// Also `bench-interval` only, and left out when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reread_busy: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BenchOutcome {
    /// Matched with status 0x02.
    Ok,
    /// Matched with another status (busy, failure, timeout, not supported).
    DeviceStatus,
    /// Sent, but no matching response within the GET attempts.
    NoResponse,
    /// A transfer failed after the SET.
    Failed,
    NotSent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchSample {
    pub outcome: BenchOutcome,
    #[serde(default)]
    pub status: Option<u8>,
    pub elapsed_us: u32,
    pub gets: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchResult {
    pub first_get_delay_us: u32,
    pub get_interval_us: u32,
    pub samples: Vec<BenchSample>,
}

// ---------------------------------------------------------------- probe-receiver

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeReceiver {
    pub receiver_id: ReceiverId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiverProbe {
    /// The first 22 bytes of the pairing inventory read at opening, if any.
    pub inventory: HexBytes,
    /// The matching answer to each of `devices::RECEIVER_PROBES`, in order.
    pub answers: Vec<Option<ReportHex>>,
}

// ---------------------------------------------------------------- capture

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    #[serde(default)]
    pub after: Option<Sequence>,
    pub max_events: EventLimit,
    pub timeout_ms: WaitTimeoutMs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureLane {
    Receiver,
    Pointer,
    Keyboard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureKind {
    Receiver,
    Identity,
    Status,
    Activity,
    Raw,
    Overflow,
    Reply,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureSource {
    /// The receiver's own state, recorded when the bridge opens the receiver.
    Kernel,
    Passive,
    Inventory,
    Input,
    /// A query the bridge sent itself (battery probes, bench).
    Bridge,
    /// A query sent by a client or by OpenRazer through a route.
    App,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvent {
    pub sequence: Sequence,
    pub receiver_id: ReceiverId,
    pub generation_id: GenerationId,
    pub time_ms: MonotonicMs,
    pub lane: CaptureLane,
    pub kind: CaptureKind,
    pub source: CaptureSource,
    pub data: HexBytes,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureResult {
    pub next: Sequence,
    /// Events after `after` were already dropped from the bridge's buffer.
    pub gap: bool,
    pub events: Vec<CaptureEvent>,
}

// ---------------------------------------------------------------- errors

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcError {
    pub code: ErrorCodeText,
    pub message: HumanMessage,
}
