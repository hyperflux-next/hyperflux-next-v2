// SPDX-License-Identifier: GPL-2.0-only

//! A small blocking client, used by hyperfluxctl and by the bridge's own
//! integration tests. One request in flight at a time, as the protocol
//! requires.

use crate::framing::{FrameError, read_frame, write_frame};
use crate::ids::{
    BenchIterations, ClientId, ClientName, ExchangeTimeoutMs, Feature, FirstGetDelayUs,
    GetIntervalUs, LeaseDurationMs, LeaseId, ProtocolVersion, ReceiverId, ReportHex, RequestId,
    RouteToken, Sequence, WaitTimeoutMs, lower_hex,
};
use crate::messages::{
    AcquireLease, Bench, BenchQuery, BenchResult, Capture, CaptureResult, Empty, Hello,
    LeaseRenewed, LeaseResult, ProbeReceiver, ReceiverProbe, ReleaseLease, RenewLease, RouteList,
    RpcError, Snapshot, Transact, TransactResult, WaitParams, WaitResult, Welcome,
};
use crate::negotiation::CURRENT_VERSION;
use crate::paths;
use crate::report::Report;
use crate::rpc::{
    Call, DecodeError, EncodeError, Reply, Request, Validate, decode_reply, encode_request,
};
use serde::de::DeserializeOwned;
use std::fmt;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How a bench query reads after the first GET. Anything but the default
/// needs the `bench-interval` feature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchReads {
    /// None for the bridge's usual 1 ms.
    pub interval: Option<GetIntervalUs>,
    pub reread_busy: bool,
}

/// Who must be on the other end of the socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerCheck {
    Any,
    /// The listening socket's owner, e.g. 0 for a socket systemd created.
    Uid(u32),
    /// Whoever owns the socket file. The bridge creates its own socket.
    SocketOwner,
}

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub client_id: ClientId,
    pub client_name: ClientName,
    pub required_features: Vec<Feature>,
    pub optional_features: Vec<Feature>,
    /// Applies to every request. Long polls add their own timeout on top.
    pub timeout: Duration,
    pub peer: PeerCheck,
}

impl ClientConfig {
    /// A config with a fresh unique client id and a 2 second timeout.
    ///
    /// # Panics
    /// If `prefix` or `name` are empty or too long.
    #[must_use]
    pub fn new(prefix: &str, name: &str, required: &[&str], optional: &[&str]) -> Self {
        let features = |list: &[&str]| -> Vec<Feature> {
            list.iter()
                .map(|f| Feature::new(*f).expect("feature names are short"))
                .collect()
        };
        Self {
            client_id: unique_client_id(prefix),
            client_name: ClientName::new(name).expect("client names are short"),
            required_features: features(required),
            optional_features: features(optional),
            timeout: Duration::from_secs(2),
            peer: PeerCheck::Any,
        }
    }
}

#[derive(Debug)]
pub enum ClientError {
    Connect(io::ErrorKind),
    Peer {
        expected: u32,
        actual: u32,
    },
    Frame(FrameError),
    Encode(EncodeError),
    Decode(DecodeError),
    /// The bridge closed the connection.
    Closed,
    /// The bridge answered with an error.
    Bridge(RpcError),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(kind) => write!(f, "can't connect to the bridge: {kind}"),
            Self::Peer { expected, actual } => {
                write!(
                    f,
                    "bridge socket belongs to uid {actual}, expected {expected}"
                )
            }
            Self::Frame(error) => error.fmt(f),
            Self::Encode(error) => error.fmt(f),
            Self::Decode(error) => error.fmt(f),
            Self::Closed => f.write_str("the bridge closed the connection"),
            Self::Bridge(error) => write!(f, "[{}] {}", error.code, error.message),
        }
    }
}

impl std::error::Error for ClientError {}

impl ClientError {
    /// The bridge's error code, if this is a bridge error.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Bridge(error) => Some(error.code.as_str()),
            _ => None,
        }
    }
}

/// `<prefix>-<pid>-<8 random hex digits>`. Two processes, or two runs of
/// the same command, never collide.
///
/// # Panics
/// If `prefix` is longer than 100 bytes.
#[must_use]
pub fn unique_client_id(prefix: &str) -> ClientId {
    let mut random = [0_u8; 4];
    // A failing entropy source still leaves the pid to tell runs apart.
    let _ = getrandom::fill(&mut random);
    let id = format!("{prefix}-{}-{}", std::process::id(), lower_hex(&random));
    ClientId::new(id).expect("prefix is short")
}

/// Where a client should try to connect, in order. root uses the system
/// socket. Everyone else tries their per-user socket first and falls back
/// to the system socket (which works for members of the hyperflux group).
/// `HYPERFLUX_NEXT_SOCKET` overrides both.
#[must_use]
pub fn socket_candidates() -> Vec<(PathBuf, PeerCheck)> {
    if let Some(path) = std::env::var_os(paths::SOCKET_ENV) {
        return vec![(PathBuf::from(path), PeerCheck::Any)];
    }
    let system = (PathBuf::from(paths::SYSTEM_SOCKET), PeerCheck::SocketOwner);
    let uid = rustix::process::geteuid().as_raw();
    if uid == 0 {
        vec![system]
    } else {
        vec![(paths::user_socket(uid), PeerCheck::Uid(0)), system]
    }
}

/// Framing and request ids for one connection.
struct Channel {
    stream: UnixStream,
    timeout: Duration,
    id_prefix: String,
    counter: u64,
}

impl Channel {
    fn call<T: DeserializeOwned + Validate>(
        &mut self,
        call: Call,
        extra: Duration,
    ) -> Result<T, ClientError> {
        self.counter += 1;
        let id = RequestId::new(format!("{}-{}", self.id_prefix, self.counter))
            .expect("generated ids are short");
        let payload = encode_request(&Request {
            id: id.clone(),
            call,
        })
        .map_err(ClientError::Encode)?;
        // std rejects a zero socket timeout. Never pass one.
        let timeout = Some((self.timeout + extra).max(Duration::from_millis(1)));
        self.stream
            .set_read_timeout(timeout)
            .and_then(|()| self.stream.set_write_timeout(timeout))
            .map_err(|e| ClientError::Connect(e.kind()))?;
        write_frame(&mut self.stream, &payload).map_err(ClientError::Frame)?;
        let reply = read_frame(&mut self.stream)
            .map_err(ClientError::Frame)?
            .ok_or(ClientError::Closed)?;
        match decode_reply::<T>(&reply, &id).map_err(ClientError::Decode)? {
            Reply::Ok(value) => Ok(value),
            Reply::Err(error) => Err(ClientError::Bridge(error)),
        }
    }
}

/// A connection to the bridge, past the hello. Each request can fail with
/// a transport, decoding or bridge error ([`ClientError`]).
pub struct Client {
    channel: Channel,
    welcome: Welcome,
}

impl Client {
    /// Connects and says hello.
    ///
    /// # Errors
    /// Fails when the socket cannot be reached, belongs to the wrong user,
    /// or the bridge refuses the hello.
    pub fn connect(path: &Path, config: &ClientConfig) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(path).map_err(|e| ClientError::Connect(e.kind()))?;
        let expected = match config.peer {
            PeerCheck::Any => None,
            PeerCheck::Uid(uid) => Some(uid),
            PeerCheck::SocketOwner => Some(
                rustix::fs::stat(path)
                    .map_err(|e| ClientError::Connect(io::Error::from(e).kind()))?
                    .st_uid,
            ),
        };
        if let Some(expected) = expected {
            let actual = rustix::net::sockopt::socket_peercred(&stream)
                .map_err(|e| ClientError::Connect(io::Error::from(e).kind()))?
                .uid
                .as_raw();
            if actual != expected {
                return Err(ClientError::Peer { expected, actual });
            }
        }
        Self::handshake(stream, config)
    }

    /// Says hello over an already connected stream.
    ///
    /// # Errors
    /// As [`Client::connect`].
    pub fn handshake(stream: UnixStream, config: &ClientConfig) -> Result<Self, ClientError> {
        let mut random = [0_u8; 4];
        let _ = getrandom::fill(&mut random);
        let mut channel = Channel {
            stream,
            timeout: config.timeout,
            id_prefix: format!("r{}", lower_hex(&random)),
            counter: 0,
        };
        let version = ProtocolVersion::new(CURRENT_VERSION).expect("the current version is valid");
        let hello = Hello {
            client_id: config.client_id.clone(),
            client_name: config.client_name.clone(),
            min_version: version,
            max_version: version,
            required_features: config.required_features.clone(),
            optional_features: config.optional_features.clone(),
        };
        let welcome = channel.call(Call::Hello(hello), Duration::ZERO)?;
        Ok(Self { channel, welcome })
    }

    #[must_use]
    pub const fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    #[must_use]
    pub fn has_feature(&self, feature: &str) -> bool {
        self.welcome.features.iter().any(|f| f.as_str() == feature)
    }

    /// Sends one request and waits for its reply. `extra` extends the
    /// socket timeout for long polls.
    pub fn call<T: DeserializeOwned + Validate>(
        &mut self,
        call: Call,
        extra: Duration,
    ) -> Result<T, ClientError> {
        self.channel.call(call, extra)
    }

    pub fn snapshot(&mut self) -> Result<Snapshot, ClientError> {
        self.call(Call::Snapshot, Duration::ZERO)
    }

    /// Blocks until the state or route revision moves past the given
    /// values, or `timeout_ms` passes.
    pub fn wait(
        &mut self,
        state_after: Option<Sequence>,
        routes_after: Option<Sequence>,
        timeout: WaitTimeoutMs,
    ) -> Result<WaitResult, ClientError> {
        let extra = Duration::from_millis(u64::from(timeout.get()));
        self.call(
            Call::Wait(WaitParams {
                state_after,
                routes_after,
                timeout_ms: timeout,
            }),
            extra,
        )
    }

    pub fn list_routes(&mut self) -> Result<RouteList, ClientError> {
        self.call(Call::ListRoutes, Duration::ZERO)
    }

    pub fn acquire_lease(
        &mut self,
        route_token: &RouteToken,
        duration: LeaseDurationMs,
    ) -> Result<LeaseResult, ClientError> {
        let params = AcquireLease {
            route_token: route_token.clone(),
            duration_ms: duration,
        };
        self.call(Call::AcquireLease(params), Duration::ZERO)
    }

    pub fn renew_lease(
        &mut self,
        lease_id: &LeaseId,
        duration: LeaseDurationMs,
    ) -> Result<LeaseRenewed, ClientError> {
        let params = RenewLease {
            lease_id: lease_id.clone(),
            duration_ms: duration,
        };
        self.call(Call::RenewLease(params), Duration::ZERO)
    }

    pub fn release_lease(&mut self, lease_id: &LeaseId) -> Result<(), ClientError> {
        let params = ReleaseLease {
            lease_id: lease_id.clone(),
        };
        self.call::<Empty>(Call::ReleaseLease(params), Duration::ZERO)
            .map(|_| ())
    }

    /// One report, one result. Never call this again for the same report
    /// after an error or a `may-have-been-sent` outcome.
    ///
    /// # Errors
    /// A transport error after the request was written means the report
    /// may have been sent.
    pub fn transact(
        &mut self,
        lease_id: &LeaseId,
        route_token: &RouteToken,
        report: &Report,
        timeout: ExchangeTimeoutMs,
    ) -> Result<TransactResult, ClientError> {
        let params = Transact {
            lease_id: lease_id.clone(),
            route_token: route_token.clone(),
            report: ReportHex::from_bytes(*report),
            timeout_ms: timeout,
        };
        let extra = Duration::from_millis(u64::from(timeout.get()));
        self.call(Call::Transact(params), extra)
    }

    pub fn bench(
        &mut self,
        lease_id: &LeaseId,
        route_token: &RouteToken,
        query: BenchQuery,
        first_get_delay: FirstGetDelayUs,
        iterations: BenchIterations,
        reads: BenchReads,
    ) -> Result<BenchResult, ClientError> {
        let params = Bench {
            lease_id: lease_id.clone(),
            route_token: route_token.clone(),
            query,
            first_get_delay_us: first_get_delay,
            iterations,
            get_interval_us: reads.interval,
            reread_busy: reads.reread_busy,
        };
        // Each iteration is bounded by the 1 s exchange deadline.
        let extra = Duration::from_secs(u64::from(iterations.get()));
        self.call(Call::Bench(params), extra)
    }

    pub fn capture(&mut self, params: Capture) -> Result<CaptureResult, ClientError> {
        let extra = Duration::from_millis(u64::from(params.timeout_ms.get()));
        self.call(Call::Capture(params), extra)
    }

    pub fn probe_receiver(&mut self, id: ReceiverId) -> Result<ReceiverProbe, ClientError> {
        // Four queries, one after the other, each within 1 s.
        let params = ProbeReceiver { receiver_id: id };
        self.call(Call::ProbeReceiver(params), Duration::from_secs(4))
    }
}
