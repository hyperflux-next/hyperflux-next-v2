# SPDX-License-Identifier: GPL-2.0-only
"""The client against the golden messages in crates/hfx-proto/tests/fixtures and a scripted bridge."""

import json
import socket
import struct
import sys
import threading
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path[:0] = [str(HERE.parent), str(HERE.parent / "examples")]

import batteries  # noqa: E402
from hyperflux_sdk import BridgeError, Client, ConnectError, PeerError, ProtocolError, age_phrase  # noqa: E402
from hyperflux_sdk import client as wire  # noqa: E402

FIXTURES = HERE.parents[2] / "crates/hfx-proto/tests/fixtures"
# Leases, transact, bench and capture are for clients that control devices. This one reads.
NOT_SPOKEN = ("acquire-lease-", "renew-lease-", "release-lease-", "transact-", "bench-", "capture-", "probe-")
checked = set()


def fixture(name):
    checked.add(name)
    data = (FIXTURES / f"{name}.json").read_bytes()
    assert data.endswith(b"\n")
    return data[:-1]


def reply(name):
    data = fixture(name)
    return wire.decode_reply(data, json.loads(data)["id"])


def test_requests_match_fixtures():
    hello = wire.hello_params("openrgb-feature-4211-9c3e7a10", "OpenRGB", ["native-routes"], ["wait"])
    assert wire.encode_request("r1", "hello", hello) == fixture("hello-request")
    assert wire.encode_request("r2", "snapshot", {}) == fixture("snapshot-request")
    assert wire.encode_request("r3", "wait", wire.wait_params(41, 7, 30000)) == fixture("wait-request")
    assert wire.encode_request("r4", "list-routes", {}) == fixture("list-routes-request")
    assert wire.encode_request("r11", "teleport", {}) == fixture("unknown-method-request")


def test_replies_decode():
    welcome = wire.decode_welcome(reply("hello-reply"))
    assert welcome.version == 1 and welcome.features == ("native-routes", "wait")

    snapshot = wire.decode_snapshot(reply("snapshot-reply"))
    assert (snapshot.revision, snapshot.now_ms, snapshot.transport) == (41, 5413001, "hidraw")
    mouse, keyboard = snapshot.devices()
    assert mouse.name == "Razer Basilisk V3 Pro 35K" and mouse.product_id == 0x00CD
    assert mouse.battery == wire.Battery(79, "charging", 5410002, False, None, None)
    assert mouse.contact == "on-mat" and mouse.controlled_by is None
    assert keyboard.presence == "asleep" and keyboard.controlled_by == "OpenRazer"
    assert keyboard.battery == wire.Battery(64, None, 1180000, True, None, None)
    assert keyboard.refused == (wire.Refused(0, 0x85, 1, 3),) and snapshot.receivers[0].inventory == "slots"
    assert (mouse.slot, mouse.openrazer) == ("connected", wire.OpenRazer("live", None))
    assert keyboard.openrazer == wire.OpenRazer("retrying", "serial read not answered")
    assert mouse.exchanges == wire.Exchanges(210, 203, 4, 5, 0) and keyboard.exchanges.no_answer == 1
    assert (mouse.first_get.saved_us, keyboard.first_get) == (None, wire.FirstGet(4500, 4000, 5000))

    assert wire.decode_wait(reply("wait-reply")) == wire.WaitResult(42, 7, False)
    # A missing nullable field reads as null.
    assert wire.Battery.decode({"stale": False}) == wire.Battery(None, None, None, False, None, None)
    unreported = wire.Battery.decode({"stale": False, "rising": True, "unreported": "charge"})
    assert (unreported.rising, unreported.unreported) == (True, "charge")

    routes = wire.decode_route_list(reply("list-routes-reply"))
    assert routes.revision == 7
    assert [r.device_id for r in routes.routes] == ["keyboard-0296", "pointer-00cd"]
    assert all(len(r.route_token) == 64 for r in routes.routes)


ERROR_REPLIES = [("hello-error-version", "HFX-101"), ("hello-error-feature", "HFX-102"),
                 ("error-feature-reply", "HFX-102")]


@pytest.mark.parametrize("name, code", ERROR_REPLIES)
def test_error_replies(name, code):
    with pytest.raises(BridgeError) as error:
        reply(name)
    assert error.value.code == code


def test_all_fixtures_checked():
    test_requests_match_fixtures()
    test_replies_decode()
    for name, code in ERROR_REPLIES:
        test_error_replies(name, code)
    names = {path.stem for path in FIXTURES.glob("*.json")}
    left = {name for name in names - checked if not name.startswith(NOT_SPOKEN)}
    assert not left


MALFORMED = [b'{"id":"r2","result":{}}', b'{"id":"r1"}', b'{"id":"r1","result":{},"extra":1}',
             b'{"id":"r1","result":{},"error":{"code":"HFX-102","message":"x"}}', b'{"id":"r1","id":"r1","result":{}}',
             b'{"id":"r1","result":[]}', b"not json", b"\xff"]


@pytest.mark.parametrize("payload", MALFORMED)
def test_malformed_replies(payload):
    with pytest.raises(ProtocolError):
        wire.decode_reply(payload, "r1")


def broken_routes(change):
    routes = json.loads(fixture("list-routes-reply"))["result"]
    change(routes["routes"])
    return routes


@pytest.mark.parametrize("decode, result", [
    (wire.decode_wait, {"state": "01", "routes": "7", "timed_out": False}),
    (wire.decode_wait, {"state": 7, "routes": "7", "timed_out": False}),
    (wire.decode_wait, {"state": "7", "routes": "7", "timed_out": 0}),
    (wire.decode_wait, {"state": "7", "routes": "7", "timed_out": False, "more": 1}),
    (wire.decode_welcome, {"version": 1, "server_instance_id": "b", "bridge_version": "1", "features": ["wait", "a"]}),
    (wire.Battery.decode, {"level": 101, "charge": None, "updated_ms": None, "stale": False}),
    (wire.decode_route_list, broken_routes(lambda routes: routes[0].update(state="unavailable"))),
    (wire.decode_route_list, broken_routes(lambda routes: routes.reverse())),
])
def test_contradictions(decode, result):
    with pytest.raises(ProtocolError):
        decode(result)


def test_age_phrases():
    for age_ms, phrase in ((0, "1 minute"), (119_999, "1 minute"), (120_000, "2 minutes"), (3_600_000, "1 hour"),
                           (47 * 3_600_000, "47 hours"), (48 * 3_600_000, "2 days")):
        assert age_phrase(age_ms) == phrase


# ---------------------------------------------------------------- fake bridge


def serve(sock, answer):
    """Answers each request with answer(request) until the client leaves."""
    with sock:
        while True:
            prefix = sock.recv(4, socket.MSG_WAITALL)
            if len(prefix) < 4:
                return
            (size,) = struct.unpack(">I", prefix)
            request = json.loads(sock.recv(size, socket.MSG_WAITALL))
            text = answer(request)
            if text is not None:
                sock.sendall(struct.pack(">I", len(text)) + text)


def answer_with(request, name):
    message = json.loads(fixture(name))
    message["id"] = request["id"]
    return json.dumps(message, separators=(",", ":")).encode()


def scripted(request):
    replies = {"hello": "hello-reply", "snapshot": "snapshot-reply", "wait": "wait-reply",
               "list-routes": "list-routes-reply"}
    return answer_with(request, replies[request["method"]])


def test_conversation():
    asked = []

    def answer(request):
        asked.append(request["method"])
        if request["method"] == "wait":
            return answer_with(request, "error-feature-reply")
        if asked.count("snapshot") == 2:
            return answer_with({"id": "somebody-else"}, "snapshot-reply")
        return scripted(request)

    ours, theirs = socket.socketpair()
    thread = threading.Thread(target=serve, args=(theirs, answer), daemon=True)
    thread.start()
    with Client(ours, timeout=2.0) as client:
        assert client.hello("python-test", "Test").bridge_version == "1.0.0"
        assert client.has_feature("wait") and client.snapshot().revision == 41
        # A bridge error leaves the connection usable...
        with pytest.raises(BridgeError):
            client.wait(state_after=41)
        assert len(client.list_routes().routes) == 2
        # ...a reply to some other request doesn't.
        with pytest.raises(ProtocolError):
            client.snapshot()
        with pytest.raises(ProtocolError):
            client.list_routes()
    thread.join(2)
    assert asked == ["hello", "snapshot", "wait", "list-routes", "snapshot"]


def listening_bridge(path, answer):
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(path))
    server.listen(1)

    def run():
        with server:
            conn, _ = server.accept()
            serve(conn, answer)

    thread = threading.Thread(target=run, daemon=True)
    thread.start()
    return thread


def test_peer_check(tmp_path):
    path = tmp_path / "bridge.sock"
    listening_bridge(path, scripted)
    with pytest.raises(PeerError):
        Client.connect(str(path), peer=12345)
    with pytest.raises(ConnectError):
        Client.connect(str(tmp_path / "missing.sock"))


def test_example(tmp_path, monkeypatch, capsys):
    path = tmp_path / "bridge.sock"
    thread = listening_bridge(path, scripted)
    monkeypatch.setenv("HYPERFLUX_NEXT_SOCKET", str(path))
    monkeypatch.setattr(batteries, "boottime_ms", lambda: 5413001)
    assert batteries.main(["batteries.py"]) == 0
    thread.join(2)
    assert capsys.readouterr().out.splitlines() == [
        "Razer Basilisk V3 Pro 35K: 79% charging",
        "Razer DeathStalker V2 Pro TKL: 64% (1 hour old)",
    ]
