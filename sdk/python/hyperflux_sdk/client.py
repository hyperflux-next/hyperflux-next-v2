# SPDX-License-Identifier: GPL-2.0-only
"""A small blocking client for the HyperFlux Next V2 bridge, protocol version 1 (docs/protocol.md).

It reads: hello, snapshot, wait and list-routes. Replies that break the protocol raise
ProtocolError instead of being half understood.
"""

import json
import keyword
import os
import secrets
import socket
import struct
import time
from dataclasses import make_dataclass

PROTOCOL_VERSION = 1
MAX_MESSAGE_BYTES = 1_048_576
SYSTEM_SOCKET = "/run/hyperflux-next/bridge.sock"
SOCKET_ENV = "HYPERFLUX_NEXT_SOCKET"


class HyperFluxError(Exception):
    """Base class of everything this module raises."""


class ConnectError(HyperFluxError):
    """Nothing is listening on the socket, or it can't be opened."""


class PeerError(HyperFluxError):
    """The socket belongs to someone other than the expected user."""


class ProtocolError(HyperFluxError):
    """The bridge broke the protocol or the connection failed; it can't be used again."""


class BridgeError(HyperFluxError):
    """The bridge answered with an HFX-1xx error. The connection stays usable."""

    def __init__(self, code, message):
        super().__init__(f"[{code}] {message}")
        self.code = code
        self.message = message


# ---------------------------------------------------------------- decoding


def _check(test, wanted):
    def check(value, where):
        if not test(value):
            raise ProtocolError(f"{where} is not {wanted}")
        return value

    return check


def _text(low=1, high=128):
    return _check(lambda v: isinstance(v, str) and low <= len(v.encode()) <= high, f"{low} to {high} bytes of text")


def _integer(low, high):
    # bool is an int in Python, but true isn't a number on the wire.
    return _check(lambda v: type(v) is int and low <= v <= high, f"an integer from {low} to {high}")


def _decimal(minimum=0):
    # u64 values are canonical decimal strings: no sign, no leading zero.
    def canonical(v):
        return isinstance(v, str) and v.isascii() and v.isdigit() and (v == "0" or v[0] != "0") \
            and minimum <= int(v) < 2**64

    check = _check(canonical, "a canonical decimal string")
    return lambda value, where: int(check(value, where))


_flag = _check(lambda v: isinstance(v, bool), "a boolean")


def _word(*words):
    return _check(lambda v: v in words, "one of " + ", ".join(words))


def _items(decode, maximum):
    check = _check(lambda v: isinstance(v, list) and len(v) <= maximum, f"a list of at most {maximum}")
    return lambda value, where: tuple(decode(item, where) for item in check(value, where))


def _null(check):
    """A nullable field: it may be null or missing."""

    def nullable(value, where):
        return check(value, where)

    nullable.nullable = True
    return nullable


def _record(name, *fields, **methods):
    """A frozen dataclass whose decode() refuses unknown fields, as the bridge does."""
    names = [field for field, _ in fields]

    def decode(value, where=name):
        if not isinstance(value, dict):
            raise ProtocolError(f"{where} is not an object")
        unknown = set(value) - set(names)
        if unknown:
            raise ProtocolError(f"{where} has unknown fields {sorted(unknown)}")
        decoded = []
        for field, check in fields:
            item = value.get(field)
            if item is None and not getattr(check, "nullable", False):
                raise ProtocolError(f"{where}.{field} is missing")
            decoded.append(None if item is None else check(item, f"{where}.{field}"))
        return cls(*decoded)

    # A field named like a keyword ("class") gets a trailing underscore, as PEP 8 suggests.
    attributes = [field + "_" if keyword.iskeyword(field) else field for field in names]
    cls = make_dataclass(name, attributes, frozen=True, namespace={"decode": staticmethod(decode), **methods})
    return cls


_LANE = _word("pointer", "keyboard")
_KIND = _word("mouse", "keyboard", "unknown")
_U16 = _integer(0, 65535)

Welcome = _record("Welcome", ("version", _integer(1, 1)), ("server_instance_id", _text()),
                  ("bridge_version", _text(1, 64)), ("features", _items(_text(1, 64), 32)))
Battery = _record("Battery", ("level", _null(_integer(0, 100))), ("charge", _null(_word("charging", "discharging"))),
                  ("updated_ms", _null(_decimal())), ("stale", _flag), ("rising", _null(_flag)),
                  ("unreported", _null(_word("charge", "battery"))))
FirstGet = _record("FirstGet", ("delay_us", _U16), ("learned_from_us", _U16), ("saved_us", _null(_U16)))
_U8, _U32 = _integer(0, 255), _integer(0, 2**32 - 1)
Refused = _record("Refused", ("class", _U8), ("id", _U8), ("size", _U8), ("count", _U32))
OpenRazer = _record("OpenRazer", ("state", _word("live", "cable", "starting", "retrying", "blocked", "not-offered")),
                    ("reason", _null(_text(1, 512))))
Exchanges = _record("Exchanges", ("sent", _U32), ("first_read", _U32), ("reread_other", _U32),
                    ("reread_busy", _U32), ("no_answer", _U32))
Device = _record(
    "Device",
    ("device_id", _text()), ("lane", _LANE), ("kind", _KIND), ("vendor_id", _U16), ("product_id", _U16),
    ("name", _null(_text())),
    ("support", _word("tested", "checked", "listed", "unknown")),
    ("presence", _word("online", "asleep", "unreachable", "waiting")),
    ("last_seen_ms", _null(_decimal())), ("contact", _null(_word("on-mat", "off-mat"))),
    ("controllable", _flag), ("controlled_by", _null(_text())), ("battery", Battery.decode),
    ("first_get", _null(FirstGet.decode)), ("refused", _null(_items(Refused.decode, 16))),
    ("slot", _null(_word("connected", "not-connected"))), ("openrazer", _null(OpenRazer.decode)),
    ("exchanges", _null(Exchanges.decode)),
)
Receiver = _record(
    "Receiver",
    ("receiver_id", _text()), ("generation_id", _decimal(1)), ("vendor_id", _U16), ("product_id", _U16),
    ("state", _word("active", "suspended")), ("writable", _flag), ("devices", _items(Device.decode, 2)),
    ("inventory", _null(_word("slots", "known-offset", "marker", "not-recognised", "absent"))),
)
Snapshot = _record(
    "Snapshot", ("revision", _decimal()), ("now_ms", _decimal()), ("transport", _word("hidraw")),
    ("receivers", _items(Receiver.decode, 16)),
    devices=lambda self: [device for receiver in self.receivers for device in receiver.devices],
)
WaitResult = _record("WaitResult", ("state", _decimal()), ("routes", _decimal()), ("timed_out", _flag))
Route = _record(
    "Route",
    ("route_token", _null(_text(64, 64))), ("receiver_id", _text()), ("generation_id", _decimal(1)),
    ("device_id", _text()), ("lane", _LANE), ("kind", _KIND), ("vendor_id", _U16), ("product_id", _U16),
    ("state", _word("available", "unavailable")), ("access", _word("qualified", "unsupported", "unknown")),
    ("report_bytes", _integer(90, 90)),
)
RouteList = _record("RouteList", ("revision", _decimal()), ("routes", _items(Route.decode, 32)))
_Error = _record("_Error", ("code", _text(1, 32)), ("message", _text(1, 512)))
decode_snapshot = Snapshot.decode
decode_wait = WaitResult.decode


def decode_welcome(result):
    welcome = Welcome.decode(result)
    if list(welcome.features) != sorted(set(welcome.features)):
        raise ProtocolError("welcome features must be sorted and unique")
    return welcome


def decode_route_list(result):
    routes = RouteList.decode(result)
    for route in routes.routes:
        usable = route.state == "available" and route.access == "qualified"
        if (route.route_token is not None) != usable:
            raise ProtocolError("a route token must be present exactly on qualified available routes")
    keys = [(r.receiver_id, r.generation_id, r.device_id) for r in routes.routes]
    if any(a >= b for a, b in zip(keys, keys[1:])):
        raise ProtocolError("routes must be sorted and unique")
    return routes


def _unique_keys(pairs):
    keys = [key for key, _ in pairs]
    if len(keys) != len(set(keys)):
        raise ProtocolError("a reply object repeats a field")
    return dict(pairs)


def _no_float(text):
    raise ProtocolError(f"{text} is not an integer")


def decode_reply(payload, request_id):
    """The result object of the reply to `request_id`. Error replies raise
    BridgeError; anything malformed raises ProtocolError."""
    try:
        reply = json.loads(payload.decode("utf-8"), object_pairs_hook=_unique_keys, parse_float=_no_float,
                           parse_constant=_no_float)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ProtocolError(f"the reply is not JSON: {error}") from None
    if not isinstance(reply, dict) or set(reply) - {"id", "result", "error"}:
        raise ProtocolError("the reply is not an envelope")
    if reply.get("id") != request_id:
        raise ProtocolError("the reply id doesn't match the request")
    result, error = reply.get("result"), reply.get("error")
    if (result is None) == (error is None):
        raise ProtocolError("a reply has exactly one of result and error")
    if error is not None:
        error = _Error.decode(error, "error")
        raise BridgeError(error.code, error.message)
    if not isinstance(result, dict):
        raise ProtocolError("the result is not an object")
    return result


# ---------------------------------------------------------------- encoding


def encode_request(request_id, method, params):
    """Compact JSON with the fields in the documented order, as the bridge expects."""
    message = {"id": request_id, "method": method, "params": params}
    return json.dumps(message, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def hello_params(client_id, client_name, required_features=(), optional_features=()):
    return {"client_id": client_id, "client_name": client_name, "min_version": PROTOCOL_VERSION,
            "max_version": PROTOCOL_VERSION, "required_features": list(required_features),
            "optional_features": list(optional_features)}


def wait_params(state_after, routes_after, timeout_ms):
    return {"state_after": None if state_after is None else str(state_after),
            "routes_after": None if routes_after is None else str(routes_after), "timeout_ms": timeout_ms}


# ---------------------------------------------------------------- connection


def socket_candidates():
    """(path, peer) pairs to try in order; peer is a uid, "owner" of the socket file, or "any"."""
    override = os.environ.get(SOCKET_ENV)
    if override:
        return [(override, "any")]
    uid = os.geteuid()
    if uid == 0:
        return [(SYSTEM_SOCKET, "owner")]
    # systemd creates the per-user socket: root is listening there.
    return [(f"/run/user/{uid}/hyperflux-next-bridge.sock", 0), (SYSTEM_SOCKET, "owner")]


def unique_client_id(prefix):
    return f"{prefix}-{os.getpid()}-{secrets.token_hex(4)}"


class Client:
    """One connection to the bridge, from connect(); close() or a with block ends it."""

    def __init__(self, sock, timeout=5.0):
        self._sock = sock
        self._timeout = timeout
        self._next_id = 1
        self._broken = False
        self.welcome = None

    @classmethod
    def connect(cls, path=None, *, client_name="Python", client_id=None, timeout=5.0, peer="any"):
        """Connects to `path`, or the first of socket_candidates() that answers, and says
        hello. `peer` is the uid that must listen on `path`, "owner" or "any"."""
        if path is None:
            last = None
            for candidate, expected in socket_candidates():
                try:
                    return cls.connect(candidate, client_name=client_name, client_id=client_id, timeout=timeout,
                                       peer=expected)
                except ConnectError as error:
                    last = error
            raise last
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            sock.settimeout(timeout)
            sock.connect(path)
            expected = os.stat(path).st_uid if peer == "owner" else peer
            _, uid, _ = struct.unpack("3i", sock.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
        except OSError as error:
            sock.close()
            raise ConnectError(f"can't connect to {path}: {error}") from None
        if peer != "any" and uid != expected:
            sock.close()
            raise PeerError(f"{path} belongs to uid {uid}, expected {expected}")
        client = cls(sock, timeout)
        try:
            client.hello(client_id or unique_client_id("python"), client_name)
        except BaseException:
            client.close()
            raise
        return client

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()

    def close(self):
        self._broken = True
        self._sock.close()

    def hello(self, client_id, client_name, optional_features=("native-routes", "wait")):
        params = hello_params(client_id, client_name, (), optional_features)
        self.welcome = decode_welcome(self._call("hello", params))
        return self.welcome

    def has_feature(self, feature):
        return self.welcome is not None and feature in self.welcome.features

    def snapshot(self):
        return decode_snapshot(self._call("snapshot", {}))

    def wait(self, state_after=None, routes_after=None, timeout_ms=30_000):
        """Returns once a revision passes the given one, or after timeout_ms. Needs "wait"."""
        params = wait_params(state_after, routes_after, timeout_ms)
        return decode_wait(self._call("wait", params, timeout_ms / 1000))

    def list_routes(self):
        """Needs the "native-routes" feature."""
        return decode_route_list(self._call("list-routes", {}))

    def _call(self, method, params, extra=0.0):
        if self._broken:
            raise ProtocolError("the connection to the bridge is closed")
        request_id = f"r{self._next_id}"
        self._next_id += 1
        payload = encode_request(request_id, method, params)
        try:
            self._sock.settimeout(self._timeout + extra)
            self._sock.sendall(struct.pack(">I", len(payload)) + payload)
            (size,) = struct.unpack(">I", self._read(4))
            if not 1 <= size <= MAX_MESSAGE_BYTES:
                raise ProtocolError(f"the bridge announced a frame of {size} bytes")
            return decode_reply(self._read(size), request_id)
        except OSError as error:
            self._broken = True
            raise ProtocolError(f"the connection to the bridge failed: {error}") from None
        except ProtocolError:
            self._broken = True
            raise

    def _read(self, size):
        data = self._sock.recv(size, socket.MSG_WAITALL)
        if len(data) < size:
            raise ProtocolError("the bridge closed the connection")
        return data


# ---------------------------------------------------------------- presentation


def boottime_ms():
    """CLOCK_BOOTTIME in milliseconds, the clock of every bridge timestamp."""
    return time.clock_gettime_ns(time.CLOCK_BOOTTIME) // 1_000_000


def age_phrase(age_ms):
    """"1 minute", "5 minutes", "2 hours", "3 days", as hyperfluxctl says it."""
    minutes = max(age_ms // 60_000, 1)
    if minutes < 60:
        count, unit = minutes, "minute"
    elif minutes // 60 < 48:
        count, unit = minutes // 60, "hour"
    else:
        count, unit = minutes // 60 // 24, "day"
    return f"{count} {unit}" if count == 1 else f"{count} {unit}s"
