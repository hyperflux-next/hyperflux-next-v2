// SPDX-License-Identifier: GPL-2.0-only

//! The server side of docs/protocol.md: one function per method, plus the
//! parked requests (`wait`, `capture`, and transact or bench waiting for
//! the receiver).

use crate::bridge::{BenchRun, Bridge, Origin, failure_code};
use crate::leases::{Acquire, Holder};
use crate::server::{Conn, Parked, Session};
use crate::state;
use crate::transport::{Failure, Uncertain};
use hfx_proto::devices::{self, CommandRefusal};
use hfx_proto::errors::{ErrorCode, rpc_error_with};
use hfx_proto::ids::{
    ComponentVersion, HexBytes, LeaseId, MonotonicMs, ReportHex, RequestId, RouteToken, Sequence,
    ServerInstanceId,
};
use hfx_proto::messages::{
    AcquireLease, Bench, BenchOutcome, BenchQuery, BenchResult, BenchSample, Capture, CaptureKind,
    CaptureLane, CaptureResult, CaptureSource, Empty, Hello, Inventory, Lane, LeaseOutcome,
    LeaseRenewed, LeaseResult, ProbeReceiver, ReceiverProbe, ReleaseLease, RenewLease, RouteList,
    Snapshot, Transact, TransactOutcome, TransactResult, WaitParams, WaitResult, Welcome,
};
use hfx_proto::negotiation::{FEATURE_BENCH_INTERVAL, negotiate};
use hfx_proto::report::{self, Report};
use hfx_proto::rpc::{Call, decode_request, encode_error, encode_result};
use serde::Serialize;

fn reply<T: Serialize>(conn: &mut Conn, id: &RequestId, result: &T) {
    match encode_result(id, result) {
        Ok(bytes) => conn.send(&bytes),
        Err(_) => error(conn, id, ErrorCode::Internal),
    }
}

fn error(conn: &mut Conn, id: &RequestId, code: ErrorCode) {
    error_with(conn, id, code, None);
}

fn error_with(conn: &mut Conn, id: &RequestId, code: ErrorCode, extra: Option<&str>) {
    match encode_error(id, &rpc_error_with(code, extra)) {
        Ok(bytes) => conn.send(&bytes),
        Err(_) => conn.dead = true,
    }
}

fn seq(value: u64) -> Sequence {
    Sequence::new(value).expect("sequences have no minimum")
}

fn ms(value: u64) -> MonotonicMs {
    MonotonicMs::new(value).expect("Ms has no minimum")
}

fn not_sent(code: ErrorCode) -> TransactResult {
    TransactResult {
        outcome: TransactOutcome::NotSent,
        response: None,
        code: Some(code.text()),
    }
}

fn sample(result: Result<Report, Failure>, gets: u8, elapsed_us: u32) -> BenchSample {
    let (outcome, status) = match result {
        Ok(r) if r[report::STATUS] == report::STATUS_SUCCESS => {
            (BenchOutcome::Ok, Some(r[report::STATUS]))
        }
        Ok(r) => (BenchOutcome::DeviceStatus, Some(r[report::STATUS])),
        Err(Failure::MayHaveBeenSent(Uncertain::NoReply)) => (BenchOutcome::NoResponse, None),
        Err(Failure::MayHaveBeenSent(Uncertain::Transfer)) => (BenchOutcome::Failed, None),
        Err(Failure::NotSent(_)) => (BenchOutcome::NotSent, None),
    };
    BenchSample {
        outcome,
        status,
        elapsed_us,
        gets,
    }
}

const NOT_SENT_SAMPLE: BenchSample = BenchSample {
    outcome: BenchOutcome::NotSent,
    status: None,
    elapsed_us: 0,
    gets: 0,
};

impl Bridge {
    /// Handles the requests that are ready. True if there were any.
    pub(crate) fn process_requests(&mut self) -> bool {
        let mut handled = false;
        let ids: Vec<u64> = self.conns.keys().copied().collect();
        for id in ids {
            while let Some(payload) = self
                .conns
                .get_mut(&id)
                .and_then(|c| c.next_request(self.now))
            {
                handled = true;
                let Some(mut conn) = self.conns.remove(&id) else {
                    break;
                };
                self.request(&mut conn, &payload);
                self.conns.insert(id, conn);
            }
        }
        handled
    }

    fn request(&mut self, conn: &mut Conn, payload: &[u8]) {
        let Ok(request) = decode_request(payload) else {
            // A protocol violation: close without a reply.
            conn.dead = true;
            return;
        };
        let id = request.id;
        let call = request.call;
        if let Call::Hello(hello) = call {
            return self.hello(conn, &id, hello);
        }
        let Some(session) = &conn.session else {
            return error(conn, &id, ErrorCode::InvalidRequest);
        };
        if !matches!(call, Call::Transact(_)) && conn.replays.iter().any(|(r, ..)| *r == id) {
            return error(conn, &id, ErrorCode::InvalidRequest);
        }
        let Some(method) = call.method() else {
            return error(conn, &id, ErrorCode::FeatureUnavailable);
        };
        let features = &session.welcome.features;
        let enabled = |feature: &str| features.iter().any(|f| f.as_str() == feature);
        // A bench interval or busy re-read needs a feature of its own.
        let reads = matches!(&call, Call::Bench(b) if b.get_interval_us.is_some() || b.reread_busy);
        if method.feature().is_some_and(|f| !enabled(f))
            || (reads && !enabled(FEATURE_BENCH_INTERVAL))
        {
            return error(conn, &id, ErrorCode::FeatureUnavailable);
        }
        match call {
            Call::Snapshot => self.snapshot(conn, &id),
            Call::Wait(params) => self.wait(conn, id, &params),
            Call::ListRoutes => {
                self.settle();
                let list = RouteList {
                    revision: seq(self.routes_rev),
                    routes: self.route_list(),
                };
                reply(conn, &id, &list);
            }
            Call::AcquireLease(params) => self.acquire_lease(conn, &id, &params),
            Call::RenewLease(params) => self.renew_lease(conn, &id, &params),
            Call::ReleaseLease(params) => self.release_lease(conn, &id, &params),
            Call::Transact(params) => self.transact(conn, id, params),
            Call::Bench(params) => self.bench(conn, id, params),
            Call::Capture(params) => self.capture_request(conn, id, &params),
            Call::ProbeReceiver(params) => self.probe_receiver(conn, id, &params),
            Call::Hello(_) | Call::Unknown(_) => error(conn, &id, ErrorCode::FeatureUnavailable),
        }
    }

    fn hello(&mut self, conn: &mut Conn, id: &RequestId, hello: Hello) {
        if let Some(session) = &conn.session {
            if session.hello == hello {
                let welcome = session.welcome.clone();
                return reply(conn, id, &welcome);
            }
            return error(conn, id, ErrorCode::InvalidRequest);
        }
        let negotiated = match negotiate(&hello) {
            Ok(n) => n,
            Err(e) => return error_with(conn, id, e.code(), e.detail().as_deref()),
        };
        let taken = self.conns.values().any(|c| {
            c.session
                .as_ref()
                .is_some_and(|s| s.hello.client_id == hello.client_id)
        });
        if taken {
            return error(conn, id, ErrorCode::ClientIdInUse);
        }
        let welcome = Welcome {
            version: negotiated.version,
            server_instance_id: ServerInstanceId::new(self.instance.clone()).expect("short"),
            bridge_version: ComponentVersion::new(hfx_proto::PRODUCT_VERSION).expect("short"),
            features: negotiated.features,
        };
        reply(conn, id, &welcome);
        conn.session = Some(Session { hello, welcome });
    }

    fn snapshot(&mut self, conn: &mut Conn, id: &RequestId) {
        self.settle();
        let snapshot = Snapshot {
            revision: seq(self.state_rev),
            now_ms: ms(self.now),
            transport: self.transport.kind(),
            receivers: self.receiver_infos(true),
        };
        reply(conn, id, &snapshot);
    }

    fn wait_result(&self, timed_out: bool) -> WaitResult {
        WaitResult {
            state: seq(self.state_rev),
            routes: seq(self.routes_rev),
            timed_out,
        }
    }

    fn moved(&self, state_after: Option<u64>, routes_after: Option<u64>) -> bool {
        (state_after.is_none() && routes_after.is_none())
            || state_after.is_some_and(|a| self.state_rev > a)
            || routes_after.is_some_and(|a| self.routes_rev > a)
    }

    fn wait(&mut self, conn: &mut Conn, id: RequestId, params: &WaitParams) {
        self.settle();
        let state_after = params.state_after.map(Sequence::get);
        let routes_after = params.routes_after.map(Sequence::get);
        if self.moved(state_after, routes_after) {
            reply(conn, &id, &self.wait_result(false));
        } else if params.timeout_ms.get() == 0 {
            reply(conn, &id, &self.wait_result(true));
        } else {
            conn.parked = Some(Parked::Wait {
                id,
                state_after,
                routes_after,
                deadline: self.now + u64::from(params.timeout_ms.get()),
            });
        }
    }

    fn capture_result(&self, after: u64, max: usize) -> CaptureResult {
        let (events, gap, next) = self.capture.read(after, max);
        CaptureResult {
            next: seq(next),
            gap,
            events,
        }
    }

    fn capture_request(&mut self, conn: &mut Conn, id: RequestId, params: &Capture) {
        let after = params.after.map_or(self.capture.latest(), Sequence::get);
        // A new capture starts with each receiver's inventory as it was read.
        let receivers = self.receivers.values().filter(|_| params.after.is_none());
        for r in receivers.filter(|r| r.inventory.state != Inventory::Absent) {
            self.capture.push(&crate::capture::Entry {
                receiver_id: &r.id,
                generation: r.generation,
                time_ms: self.now,
                lane: CaptureLane::Receiver,
                kind: CaptureKind::Raw,
                source: CaptureSource::Inventory,
                data: &r.inventory.masked,
            });
        }
        let max = usize::from(params.max_events.get());
        let result = self.capture_result(after, max);
        if !result.events.is_empty() || result.gap || params.timeout_ms.get() == 0 {
            reply(conn, &id, &result);
        } else {
            conn.parked = Some(Parked::Capture {
                id,
                after,
                max,
                deadline: self.now + u64::from(params.timeout_ms.get()),
            });
        }
    }

    /// Answers parked waits and captures whose condition now holds.
    pub(crate) fn wake_parked(&mut self) {
        let latest = self.capture.latest();
        let ready: Vec<u64> = self
            .conns
            .values()
            .filter(|c| match &c.parked {
                Some(Parked::Wait {
                    state_after,
                    routes_after,
                    ..
                }) => self.moved(*state_after, *routes_after),
                Some(Parked::Capture { after, .. }) => latest > *after,
                _ => false,
            })
            .map(|c| c.id)
            .collect();
        for id in ready {
            self.answer_parked(id, false);
        }
    }

    pub(crate) fn expire_parked(&mut self) {
        let now = self.now;
        let due: Vec<u64> = self
            .conns
            .values()
            .filter(|c| {
                c.parked
                    .as_ref()
                    .and_then(Parked::deadline)
                    .is_some_and(|d| d <= now)
            })
            .map(|c| c.id)
            .collect();
        for id in due {
            self.answer_parked(id, true);
        }
    }

    fn answer_parked(&mut self, conn_id: u64, timed_out: bool) {
        let wait = self.wait_result(timed_out);
        let Some(conn) = self.conns.get_mut(&conn_id) else {
            return;
        };
        match conn.parked.take() {
            Some(Parked::Wait { id, .. }) => reply(conn, &id, &wait),
            Some(Parked::Capture { id, after, max, .. }) => {
                let result = {
                    let (events, gap, next) = self.capture.read(after, max);
                    CaptureResult {
                        next: seq(next),
                        gap,
                        events,
                    }
                };
                reply(conn, &id, &result);
            }
            other => conn.parked = other,
        }
    }

    // ------------------------------------------------------------ routes

    /// The device a token addresses, or why it doesn't address one.
    pub(crate) fn target(&self, token: &RouteToken) -> Result<(u64, Lane), ErrorCode> {
        for (key, rx) in &self.receivers {
            for d in rx.lanes.iter().flatten() {
                if d.token.as_deref() == Some(token.as_str()) {
                    return Ok((*key, d.lane));
                }
            }
        }
        Err(self
            .revoked
            .iter()
            .find(|(t, _)| t == token.as_str())
            .map_or(ErrorCode::NotControllable, |(_, why)| *why))
    }

    fn lease_ok(&self, conn: u64, lease: &LeaseId, rx: u64, lane: Lane) -> bool {
        self.leases.get(lease.as_str()).is_some_and(|l| {
            matches!(l.holder, Holder::Client { conn: c, .. } if c == conn)
                && l.rx == rx
                && l.lane == lane
                && l.expires > self.now
        })
    }

    fn own_lease(&self, conn: u64, lease: &LeaseId) -> Option<bool> {
        self.leases
            .get(lease.as_str())
            .map(|l| matches!(l.holder, Holder::Client { conn: c, .. } if c == conn))
    }

    fn acquire_lease(&mut self, conn: &mut Conn, id: &RequestId, params: &AcquireLease) {
        let (rx, lane) = match self.target(&params.route_token) {
            Ok(target) => target,
            Err(code) => return error(conn, id, code),
        };
        let name = conn
            .session
            .as_ref()
            .map(|s| s.hello.client_name.clone())
            .expect("after hello");
        let expires = self.now + u64::from(params.duration_ms.get());
        let holder = Holder::Client {
            conn: conn.id,
            name,
        };
        let result = match self.leases.acquire(rx, lane, holder, expires) {
            Acquire::Granted(lease) => LeaseResult {
                outcome: LeaseOutcome::Granted,
                lease_id: Some(LeaseId::new(lease.id.clone()).expect("short")),
                expires_ms: Some(ms(expires)),
                holder: None,
            },
            Acquire::Busy(lease) => LeaseResult {
                outcome: LeaseOutcome::Busy,
                lease_id: None,
                expires_ms: None,
                holder: Some(lease.holder.name()),
            },
            Acquire::Full => return error(conn, id, ErrorCode::Busy),
        };
        reply(conn, id, &result);
    }

    fn renew_lease(&mut self, conn: &mut Conn, id: &RequestId, params: &RenewLease) {
        let live = self
            .leases
            .get(params.lease_id.as_str())
            .is_some_and(|l| l.expires > self.now);
        if self.own_lease(conn.id, &params.lease_id) != Some(true) || !live {
            return error(conn, id, ErrorCode::NoLease);
        }
        let expires = self.now + u64::from(params.duration_ms.get());
        self.leases.renew(params.lease_id.as_str(), expires);
        let renewed = LeaseRenewed {
            lease_id: params.lease_id.clone(),
            expires_ms: ms(expires),
        };
        reply(conn, id, &renewed);
    }

    fn release_lease(&mut self, conn: &mut Conn, id: &RequestId, params: &ReleaseLease) {
        match self.own_lease(conn.id, &params.lease_id) {
            Some(false) => error(conn, id, ErrorCode::NoLease),
            Some(true) => {
                self.leases.remove(params.lease_id.as_str());
                reply(conn, id, &Empty {});
            }
            None => reply(conn, id, &Empty {}),
        }
    }

    // ------------------------------------------------------------ transact

    fn check_route(
        &self,
        conn: u64,
        lease: &LeaseId,
        token: &RouteToken,
    ) -> Result<(u64, Lane, &'static hfx_proto::devices::Device), ErrorCode> {
        let (rx, lane) = self.target(token)?;
        if !self.lease_ok(conn, lease, rx, lane) {
            return Err(ErrorCode::NoLease);
        }
        let entry = self
            .receivers
            .get(&rx)
            .and_then(|r| r.device(lane))
            .and_then(|d| d.entry)
            .ok_or(ErrorCode::NotControllable)?;
        Ok((rx, lane, entry))
    }

    fn usable(&self, rx: u64) -> bool {
        self.receivers.get(&rx).is_some_and(state::Receiver::usable)
    }

    fn transact(&mut self, conn: &mut Conn, id: RequestId, params: Transact) {
        if let Some((_, earlier, result)) = conn.replays.iter().find(|(r, ..)| *r == id) {
            if *earlier == params {
                let result = result.clone();
                return reply(conn, &id, &result);
            }
            return error(conn, &id, ErrorCode::InvalidRequest);
        }
        let mut unlisted = None;
        let checked = self
            .check_route(conn.id, &params.lease_id, &params.route_token)
            .and_then(|(rx, lane, entry)| {
                let check = devices::check_command(entry, params.report.bytes());
                unlisted = (check == Err(CommandRefusal::NotListed)).then_some((rx, lane));
                check.map_err(|_| ErrorCode::InvalidRequest)?;
                if self.usable(rx) {
                    Ok((rx, lane))
                } else {
                    Err(ErrorCode::NothingSent)
                }
            });
        if let Some((rx, lane)) = unlisted {
            self.refused(rx, lane, params.report.bytes());
        }
        let (rx, lane) = match checked {
            Ok(target) => target,
            Err(code) => {
                let result = not_sent(code);
                reply(conn, &id, &result);
                return conn.remember(id, params, result);
            }
        };
        let origin = Origin::Transact {
            conn: conn.id,
            id: id.clone(),
            params: Box::new(params.clone()),
        };
        let report = *params.report.bytes();
        match self.submit(
            rx,
            lane,
            report,
            u64::from(params.timeout_ms.get()),
            0,
            origin,
        ) {
            Ok(()) => conn.parked = Some(Parked::Job),
            Err(_) => {
                let result = not_sent(ErrorCode::NothingSent);
                reply(conn, &id, &result);
                conn.remember(id, params, result);
            }
        }
    }

    pub(crate) fn transact_done(
        &mut self,
        conn: u64,
        id: RequestId,
        params: Transact,
        result: Result<Report, Failure>,
    ) {
        let result = match result {
            Ok(response) => TransactResult {
                outcome: TransactOutcome::Matched,
                response: Some(ReportHex::from_bytes(response)),
                code: None,
            },
            Err(failure) => match failure_code(failure) {
                (true, code) => TransactResult {
                    outcome: TransactOutcome::MayHaveBeenSent,
                    response: None,
                    code: Some(code.text()),
                },
                (false, code) => not_sent(code),
            },
        };
        let Some(conn) = self.conns.get_mut(&conn) else {
            return;
        };
        conn.parked = None;
        reply(conn, &id, &result);
        conn.remember(id, params, result);
    }

    // ------------------------------------------------------------ bench

    fn bench(&mut self, conn: &mut Conn, id: RequestId, params: Bench) {
        let (rx, lane, entry) =
            match self.check_route(conn.id, &params.lease_id, &params.route_token) {
                Ok(target) => target,
                Err(code) => return error(conn, &id, code),
            };
        let (class, command) = match params.query {
            BenchQuery::Firmware => (0x00, 0x81),
            BenchQuery::Battery => (0x07, 0x80),
        };
        if !state::supports_query(entry, class, command) {
            return error(conn, &id, ErrorCode::NotControllable);
        }
        if !self.usable(rx) {
            return error(conn, &id, ErrorCode::NothingSent);
        }
        let tx = match lane {
            Lane::Pointer => hfx_proto::devices::RECEIVER.pointer_lane_selector,
            Lane::Keyboard => hfx_proto::devices::RECEIVER.keyboard_lane_selector,
        };
        let request = report::build(tx, class, command, &[0, 0]);
        // The device mode query: read-only, and all routed devices take it.
        let spoiler = state::supports_query(entry, 0x00, 0x84)
            .then(|| report::build(tx, 0x00, 0x84, &[0, 0]));
        let (first, delay) = match spoiler {
            Some(spoiler) => (spoiler, 0),
            None => (request, params.first_get_delay_us.get()),
        };
        let run = Box::new(BenchRun {
            params,
            request,
            spoiler,
            spoiling: spoiler.is_some(),
            samples: Vec::new(),
        });
        let origin = Origin::Bench {
            conn: conn.id,
            id: id.clone(),
            run,
        };
        match self.submit(rx, lane, first, 1_000, delay, origin) {
            Ok(()) => conn.parked = Some(Parked::Job),
            Err(_) => error(conn, &id, ErrorCode::NothingSent),
        }
    }

    pub(crate) fn bench_step(
        &mut self,
        conn: u64,
        id: RequestId,
        mut run: Box<BenchRun>,
        rx: u64,
        lane: Lane,
        last: Option<(Result<Report, Failure>, u8, u32)>,
    ) {
        if let Some((result, gets, elapsed)) = last {
            if run.spoiling {
                run.spoiling = false;
            } else {
                run.samples.push(sample(result, gets, elapsed));
                run.spoiling = run.spoiler.is_some();
            }
        }
        let total = usize::from(run.params.iterations.get());
        if run.samples.len() < total {
            let still = self.target(&run.params.route_token) == Ok((rx, lane))
                && self.lease_ok(conn, &run.params.lease_id, rx, lane)
                && self.conns.contains_key(&conn);
            if still {
                let (request, delay) = match run.spoiler.filter(|_| run.spoiling) {
                    Some(spoiler) => (spoiler, 0),
                    None => (run.request, run.params.first_get_delay_us.get()),
                };
                match self.submit(
                    rx,
                    lane,
                    request,
                    1_000,
                    delay,
                    Origin::Bench { conn, id, run },
                ) {
                    Ok(()) => return,
                    Err(Origin::Bench {
                        id: back,
                        run: returned,
                        ..
                    }) => return self.bench_finish(conn, back, *returned),
                    Err(_) => return,
                }
            }
        }
        self.bench_finish(conn, id, *run);
    }

    fn bench_finish(&mut self, conn: u64, id: RequestId, mut run: BenchRun) {
        let total = usize::from(run.params.iterations.get());
        run.samples.resize(total, NOT_SENT_SAMPLE);
        let interval = run.params.get_interval_us.map(|i| i.get());
        let result = BenchResult {
            first_get_delay_us: u32::from(run.params.first_get_delay_us.get()),
            get_interval_us: u32::from(interval.unwrap_or(crate::wire::REREAD_INTERVAL_US)),
            samples: run.samples,
        };
        if let Some(conn) = self.conns.get_mut(&conn) {
            conn.parked = None;
            reply(conn, &id, &result);
        }
    }

    // ------------------------------------------------------------ probe-receiver

    fn probe_receiver(&mut self, conn: &mut Conn, id: RequestId, params: &ProbeReceiver) {
        let mut receivers = self.receivers.iter();
        let rx = receivers.find(|(_, r)| r.id == params.receiver_id.as_str());
        let rx = rx.map(|(rx, _)| *rx).filter(|rx| self.usable(*rx));
        let Some(rx) = rx else {
            return error(conn, &id, ErrorCode::NothingSent);
        };
        conn.parked = Some(Parked::Job);
        self.probe_next(rx, Origin::ReceiverProbe(conn.id, id, Vec::new()));
    }

    /// Sends the next receiver probe, one at a time, or answers with what
    /// they got once none is left or the receiver can't take one.
    pub(crate) fn probe_next(&mut self, rx: u64, origin: Origin) {
        let Origin::ReceiverProbe(_, _, answers) = &origin else {
            return;
        };
        let wait = crate::wire::FIRST_GET_DELAY_US;
        let origin = match devices::receiver_probe(answers.len()) {
            Some(request) => match self.submit(rx, Lane::Pointer, request, 1_000, wait, origin) {
                Ok(()) => return,
                Err(origin) => origin,
            },
            None => origin,
        };
        let Origin::ReceiverProbe(conn, id, answers) = origin else {
            return;
        };
        let read = self.receivers.get(&rx).map(|r| r.inventory);
        let read = read.filter(|read| read.state != Inventory::Absent);
        let inventory = HexBytes::new(read.map_or(Vec::new(), |read| read.masked[..22].to_vec()));
        let result = ReceiverProbe {
            inventory: inventory.expect("22 bytes"),
            answers,
        };
        if let Some(conn) = self.conns.get_mut(&conn) {
            conn.parked = None;
            reply(conn, &id, &result);
        }
    }
}
