# HyperFlux Next V2 client protocol, version 1

This page describes how applications talk to `hyperflux-next-bridge`. A
client can read the receivers and devices, wait for changes and send Razer
feature reports to a device, and the page should be all you need to write
one.
The Rust implementation is `crates/hfx-proto`. The messages in
`crates/hfx-proto/tests/fixtures/` are exact examples (the C++ and Python
SDK tests use them too).

## Sockets

The bridge listens on a Unix stream socket.

| Socket | Who can connect |
| --- | --- |
| `/run/hyperflux-next/bridge.sock` | root and members of the `hyperflux` group. Owned by the `hyperflux-next` user. |
| `/run/user/<uid>/hyperflux-next-bridge.sock` | that user. Created by `hyperflux-next-client@<uid>.socket` and forwarded to the system socket by `systemd-socket-proxyd`. |

A client running as root uses the system socket. Everyone else tries the
per-user socket first, then the system socket. `HYPERFLUX_NEXT_SOCKET`
overrides both (tests and the simulator use it).

Clients should check who is listening with `SO_PEERCRED`. On the per-user
socket the peer is uid 0 (systemd created the socket), while on the system
socket it is the owner of the socket file.

## Framing

Every message, in both directions, is a 4-byte big-endian unsigned length
followed by that many bytes of UTF-8 JSON. The length is 1 to 1,048,576.
A length of 0 or more than 1,048,576 is a protocol violation. Write the
prefix and the payload in one write. That way a failure never leaves half a
frame behind.

## Connections

- One request at a time: send a request, read its reply, then send the next.
  Methods that wait (`wait`, `capture`) simply reply later.
- The first request must be `hello`. Anything else first gets HFX-103.
- At most 32 connections are open at once. The bridge accepts a 33rd and
  closes it immediately.
- The bridge closes a connection that has not said hello within 10
  seconds. It also closes one that stops in the middle of a frame for 10
  seconds.
- Protocol violations close the connection without a reply. These are bad
  framing, invalid JSON, a wrong envelope, unknown or duplicate fields,
  missing fields, wrong types and values out of range (see Encoding).
  Error replies (HFX-1xx) leave the connection open.
- When the connection closes, the client's leases end at once.

## Encoding

- Objects contain exactly the listed fields. Unknown fields and duplicate
  fields are violations. The bridge writes fields in the listed order, but
  readers must not depend on it.
- Fields marked *nullable* are always written, as `null` when empty. A
  reader accepts a missing nullable field as `null`.
- String bounds count UTF-8 bytes, not characters.
- u64 values (`GenerationId`, `Sequence`, `Ms`) are sent as decimal
  strings, because JSON parsers that use doubles lose precision above 2^53.
  Anything but the canonical form is rejected: digits, no sign, no leading
  zero except in `"0"`.
- Integers are JSON numbers without a fraction or exponent.
- Hex digits are always lowercase.

### Scalar types

| Type | Wire form | Bound |
| --- | --- | --- |
| `RequestId` | string | 1-128 bytes; chosen by the client, unique per connection |
| `ClientId` | string | 1-128 bytes; one live connection per value |
| `ClientName` | string | 1-128 bytes; shown to users ("OpenRGB") |
| `Feature` | string | 1-64 bytes |
| `ReceiverId`, `DeviceId`, `LeaseId`, `ServerInstanceId` | string | 1-128 bytes |
| `DeviceName` | string | 1-128 bytes |
| `Version` | number | 1-65535 |
| `RouteToken` | string | exactly 64 bytes (the bridge uses lowercase hex) |
| `ReportHex` | string | exactly 180 lowercase hex digits = one 90-byte report |
| `HexBytes` | string | 0-180 lowercase hex digits, even length |
| `GenerationId` | decimal string | 1 or more |
| `Sequence` | decimal string | 0 or more |
| `Ms` | decimal string | CLOCK_BOOTTIME of the bridge host, in milliseconds |
| `ErrorCode` | string | `HFX-` and three digits |
| `Message` | string | 1-512 bytes |

`Ms` is the time since boot including suspend. A client on the same machine
can compare it with its own `clock_gettime(CLOCK_BOOTTIME)`, or use the
snapshot's `now_ms`.

## Envelopes

Request:

```json
{"id": RequestId, "method": "snapshot", "params": {}}
```

`params` is always an object. Methods without parameters take `{}`.

Reply, success or error:

```json
{"id": RequestId, "result": {...}}
{"id": RequestId, "error": {"code": "HFX-102", "message": "..."}}
```

The `id` is the request's id. A client must drop the connection if a reply
carries any other id. A request with a well-formed envelope but an unknown
method name gets HFX-102 and the connection stays open.

## Methods

| Method | Feature | Result |
| --- | --- | --- |
| `hello` | | `Welcome` |
| `snapshot` | | `Snapshot` |
| `wait` | `wait` | `WaitResult` |
| `list-routes` | `native-routes` | `RouteList` |
| `acquire-lease` | `native-routes` | `LeaseResult` |
| `renew-lease` | `native-routes` | `LeaseRenewed` |
| `release-lease` | `native-routes` | `{}` |
| `transact` | `native-routes` | `TransactResult` |
| `bench` | `bench` | `BenchResult` |
| `capture` | `capture` | `CaptureResult` |
| `probe-receiver` | `probe-receiver` | `ReceiverProbe` |

Calling a method whose feature wasn't enabled in `hello` gets HFX-102.

### hello

Params:

| Field | Type | |
| --- | --- | --- |
| `client_id` | `ClientId` | unique per connection; use something like `openrgb-feature-<pid>-<random hex>` |
| `client_name` | `ClientName` | |
| `min_version` | `Version` | |
| `max_version` | `Version` | |
| `required_features` | array of `Feature` | at most 16, no duplicates |
| `optional_features` | array of `Feature` | at most 16, no duplicates, none also in `required_features` |

Breaking a list rule is a violation. Otherwise the bridge:

1. Answers HFX-103 if `min_version > max_version`.
2. Picks the highest version it serves inside `[min_version, max_version]`.
   If there's none, it answers HFX-101. The message then ends with
   `Supported protocol versions: 1.`, which tells the client what to offer.
3. Answers HFX-102 if it does not serve a required feature. The message
   ends with `Missing features: a, b.` (sorted).
4. Answers HFX-104 if another live connection already uses `client_id`.

After an error the connection stays open, and the client may say hello
again with other values. After a successful hello, an identical hello gets
the same `Welcome` again. A different one gets HFX-103.

`Welcome`:

| Field | Type | |
| --- | --- | --- |
| `version` | `Version` | always 1 today |
| `server_instance_id` | `ServerInstanceId` | new for every bridge process |
| `bridge_version` | string 1-64 | e.g. `1.0.0` |
| `features` | array of `Feature` | enabled features: requested ones the bridge serves, sorted ascending |

Served features: `bench`, `bench-interval`, `capture`, `native-routes`,
`probe-receiver`, `wait`. Unknown optional features are ignored.

### snapshot

Params `{}`. Result `Snapshot`:

| Field | Type | |
| --- | --- | --- |
| `revision` | `Sequence` | the state revision this snapshot shows |
| `now_ms` | `Ms` | bridge time when the snapshot was taken |
| `transport` | `"hidraw"` | how the bridge reaches receivers: through their hidraw nodes. `"hidraw"` is currently the only value |
| `receivers` | array of `ReceiverInfo` | at most 16, sorted by `receiver_id`, then `generation_id` |

`ReceiverInfo`:

| Field | Type | |
| --- | --- | --- |
| `receiver_id` | `ReceiverId` | stable for the USB port the receiver is plugged into |
| `generation_id` | `GenerationId` | changes every time the receiver is plugged in |
| `vendor_id`, `product_id` | number | `5426`, `207` (1532:00CF) |
| `state` | `"active"` \| `"suspended"` | always `"active"` today: the bridge doesn't see the receiver's runtime autosuspend, and an exchange wakes it. Clients still accept `"suspended"` |
| `writable` | bool | the bridge can send to this receiver |
| `devices` | array of `DeviceInfo` | at most 2, pointer lane first |
| `inventory` | string, optional | what named the receiver's devices: `"slots"` (its answer to the bridge's paired-slots query, 00:bf), or failing that the pairing inventory read when the bridge opened the receiver, by `"known-offset"` (its marker where the tested receiver has it) or `"marker"` (found elsewhere). `"not-recognised"` and `"absent"` (no interface 4, or the read failed) mean neither could. Left out by bridges that don't report it |

`DeviceInfo`:

| Field | Type | |
| --- | --- | --- |
| `device_id` | `DeviceId` | `<lane>-<product id as 4 hex digits>`, e.g. `pointer-00cd` |
| `lane` | `"pointer"` \| `"keyboard"` | the receiver channel |
| `kind` | `"mouse"` \| `"keyboard"` \| `"unknown"` | |
| `vendor_id`, `product_id` | number | the device's wireless id |
| `name` | `DeviceName`, nullable | e.g. `Razer Basilisk V3 Pro 35K`; null for unknown devices |
| `support` | `"tested"` \| `"checked"` \| `"listed"` \| `"unknown"` | tested on real hardware; listed by Razer, driven by the pinned OpenRazer or OpenRGB and checked against them, not yet tested through the receiver; listed by Razer but not controllable; not in the device table |
| `presence` | see below | |
| `last_seen_ms` | `Ms`, nullable | last time the device sent anything |
| `contact` | `"on-mat"` \| `"off-mat"`, nullable | only for the Basilisk V3 Pro 35K (the mouse-status decoder), from its status reports |
| `controllable` | bool | a qualified route is available now |
| `controlled_by` | `ClientName`, nullable | who holds the device's lease; `OpenRazer` when the bridge's own OpenRazer route does |
| `battery` | `Battery` | |
| `first_get` | object, optional | `delay_us` (number): how long the bridge now waits after sending to the device before its first read; `learned_from_us` (number): the catalog's wait it started from; `saved_us` (number, optional): the wait kept from an earlier run of the bridge that learning went on from, left out when there was none. Left out by bridges that don't report it |
| `refused` | array, optional | commands the bridge turned down as missing from the device's list, at most 16: `class`, `id` and `size` (numbers, the report's class, command id and data size) and `count` (number, since the device paired). Left out when there are none |
| `slot` | `"connected"` \| `"not-connected"`, optional | what the receiver's paired slots last said: connected through the receiver now, or not (switched off, or on its cable). Left out when the slots haven't named this device |
| `openrazer` | object, optional | the device's virtual OpenRazer device: `state` (below) and `reason` (`HumanMessage`, optional: the last failure for `retrying` and `blocked`, why not for `not-offered`). Left out by bridges that don't report it |
| `exchanges` | object, optional | counts of the device's exchanges since the bridge started, including the bridge's own and the apps' but not bench's, all numbers: `sent` (requests that went out), `first_read` (answered at the first read), `reread_other` (reads again after a reply that answered something else, usually the receiver's previous answer), `reread_busy` (reads again after a busy answer) and `no_answer` (ended without a matching answer). The counts go on across replugs. Left out by bridges that don't report it, and for a device with no exchange yet |

`openrazer.state`:

| Value | Meaning |
| --- | --- |
| `live` | created, for the patched driver to bind |
| `cable` | a wired device of the same model is bound, and OpenRazer uses that one instead |
| `starting` | the serial reads that come before creating it are under way |
| `retrying` | a step failed, and the bridge tries again after a backoff |
| `blocked` | the bridge tries again in 5 minutes |
| `not-offered` | nothing to create: OpenRazer doesn't drive the model, the device has no route, or the patched driver or `/dev/uhid` isn't there |

A catalog device announced on the other lane keeps its kind, name, support
and route. You can spot such a device by a `kind` that doesn't match its
`lane`.

Product id 0 (device id `pointer-0000` or `keyboard-0000`) is a placeholder.
It appears when a status report arrives on a lane with no identity, before
the receiver has named the device, but input alone doesn't create one, since
a mouse's macro keys come in as keyboard input. The placeholder has kind and
support `unknown`, no name and no route token. Its battery level is all it
carries. Once the receiver names the device, the real device replaces it and
takes over its battery reading.

The bridge learns `delay_us` from the device's answers, in steps of 500
microseconds between 2000 and 8000 (ARCHITECTURE.md, section 2). Like any
other field, it moves the state revision when it changes. It changes at
most once per 50 exchanges with the device.

`presence`:

| Value | Meaning |
| --- | --- |
| `online` | the device has sent something: input, a status report, or a reply |
| `asleep` | its last reply had status 0x04: the device is asleep or switched off |
| `unreachable` | the receiver reported the device gone |
| `waiting` | paired, but silent so far, or silent since the system resumed. Moving or clicking it wakes it |

`Battery`:

| Field | Type | |
| --- | --- | --- |
| `level` | number 0-100, nullable | null until the first reading |
| `charge` | `"charging"` \| `"discharging"`, nullable | null when unknown or stale |
| `updated_ms` | `Ms`, nullable | time of the last reading |
| `stale` | bool | the last reading is more than 30 minutes old |
| `rising` | bool, optional | the last change in level was upward, with no drop since; false while stale. Left out when false |
| `unreported` | `"charge"` \| `"battery"`, optional | `charge`: the bridge has no way to learn whether the device charges (no status decoder covers it, and either it is not asked because it is neither a tested nor a checked device, or it answered 07/84 with "not supported" or too short to hold a value). `battery`: the battery query got "not supported", and no status report has come from the device. Left out otherwise |

0 % is a real reading. "Full" is not a separate value: show it when
`charge` is `charging` and `level` is 100. When `level` is null, `charge`
and `updated_ms` are null and `stale` is false.

Clients show "charging not reported" for `unreported: charge`, and no
low-battery warning while `rising` is true or `charge` is `charging`. The
bridge doesn't turn a rising level into `charging`.

The state revision goes up whenever anything in the snapshot changes,
including `stale` turning true, but not for `now_ms`, `last_seen_ms`,
`refused` or `exchanges`, which aren't worth a wake-up.

### wait

Blocks until something changes. Params:

| Field | Type | |
| --- | --- | --- |
| `state_after` | `Sequence`, nullable | a snapshot `revision` the client has |
| `routes_after` | `Sequence`, nullable | a route list `revision` the client has |
| `timeout_ms` | number 0-60000 | |

The bridge replies as soon as one of these holds:

- the current state revision is greater than `state_after`
- the current route revision is greater than `routes_after`
- `timeout_ms` has passed

A null field never triggers a reply. If both are null, the reply is
immediate. Result `WaitResult`:

| Field | Type | |
| --- | --- | --- |
| `state` | `Sequence` | current state revision |
| `routes` | `Sequence` | current route revision |
| `timed_out` | bool | true when nothing changed |

The usual loop is `snapshot`, then `wait(state_after = revision, timeout 30000)`,
then `snapshot` again when `state` moved. Revisions restart when the bridge
restarts, but the connection is new by then as well.

### list-routes

A route is the way to send reports to one device. Params `{}`. Result
`RouteList`:

| Field | Type | |
| --- | --- | --- |
| `revision` | `Sequence` | route revision |
| `routes` | array of `Route` | at most 32, sorted by `receiver_id`, `generation_id`, `device_id` |

`Route`:

| Field | Type | |
| --- | --- | --- |
| `route_token` | `RouteToken`, nullable | set if and only if `access` is `qualified` and `state` is `available` |
| `receiver_id` | `ReceiverId` | |
| `generation_id` | `GenerationId` | |
| `device_id` | `DeviceId` | |
| `lane` | `"pointer"` \| `"keyboard"` | |
| `kind` | `"mouse"` \| `"keyboard"` \| `"unknown"` | |
| `vendor_id`, `product_id` | number | |
| `state` | `"available"` \| `"unavailable"` | available: receiver writable and not going away, device online or asleep |
| `access` | `"qualified"` \| `"unsupported"` \| `"unknown"` | qualified: in the device table with a command list |
| `report_bytes` | number | always 90 |

There is one route per device in every snapshot. A token stays the same
while its route stays available, but the route gets a new token when it
becomes available again, and every generation gets new tokens. Tokens are
never reused. The route revision increases when a route appears, disappears,
or changes any field.

### acquire-lease

A lease gives one connection the right to send reports to one device. A
device has at most one lease at a time. Params:

| Field | Type | |
| --- | --- | --- |
| `route_token` | `RouteToken` | |
| `duration_ms` | number 1000-60000 | |

Result `LeaseResult`:

| Field | Type | |
| --- | --- | --- |
| `outcome` | `"granted"` \| `"busy"` | |
| `lease_id` | `LeaseId`, nullable | set when granted |
| `expires_ms` | `Ms`, nullable | set when granted |
| `holder` | `ClientName`, nullable | set when busy: who holds it (possibly this connection) |

Errors: HFX-107 (token from a receiver generation that ended), HFX-108
(unknown token or not a qualified route), HFX-109 (route unavailable right
now), HFX-106 (too many leases).

A lease ends when it is released or expires, when its connection closes,
when the receiver goes away, or when another device pairs on that lane.
The bridge's OpenRazer route and its own battery queries take leases too.
An application can therefore be told `busy` with holder `OpenRazer` or
`HyperFlux`.

### renew-lease

Params `lease_id` (`LeaseId`) and `duration_ms` (1000-60000). Result
`LeaseRenewed` with `lease_id` and the new `expires_ms` (now + duration).
Renewing a lease this connection does not hold, or one that already ended,
gets HFX-105.

### release-lease

Params `lease_id`. Result `{}`. Releasing a lease that already ended is fine.
Releasing another connection's lease gets HFX-105.

### transact

Send one 90-byte report and get the device's matching response. Params:

| Field | Type | |
| --- | --- | --- |
| `lease_id` | `LeaseId` | must cover the route's device |
| `route_token` | `RouteToken` | |
| `report` | `ReportHex` | the request (layout below) |
| `timeout_ms` | number 1-1000 | budget for getting the request onto the wire |

Result `TransactResult`:

| Field | Type | |
| --- | --- | --- |
| `outcome` | `"matched"` \| `"not-sent"` \| `"may-have-been-sent"` | |
| `response` | `ReportHex`, nullable | set exactly when matched |
| `code` | `ErrorCode`, nullable | set exactly when not matched |

`not-sent` means the report provably never reached the receiver, and the
client may try again. `may-have-been-sent` (always HFX-110) means it might
have. A client must never resend such a report automatically.

The bridge checks, in this order, and stops at the first failure with
`not-sent`:

1. The token: HFX-108 if it is unknown, HFX-107 if it was revoked because
   its generation ended, HFX-109 if the route became unavailable.
2. The lease: HFX-105 if this connection does not hold it, if it ended, or
   if it covers another device.
3. The report and command (below): HFX-103.
4. The receiver: HFX-109 if it is not writable or is gone, HFX-111 if it
   was still busy with other exchanges when `timeout_ms` ran out.

Then it sends the report to the receiver and waits for the matching
response, which takes about 4-5 ms on the tested devices, 7-10 ms on the
others and 60 ms at worst. `matched` returns the device's response as it
came. Its status byte may say busy (0x01, when the device stayed busy
until the reads ran out), failure (0x03), timeout (0x04, asleep or off)
or not supported (0x05). The bridge does not reinterpret it. The
response's transaction id (byte 1) is the one the client sent.

#### Replays

The bridge remembers the last 64 transact request ids of each connection.
Sending a remembered id again with the same params returns the remembered
result without sending anything. With different params it gets HFX-103.

#### Report layout

A Razer feature report:

| Bytes | |
| --- | --- |
| 0 | status, `0x00` in requests |
| 1 | transaction id, any non-zero value; the bridge swaps in the receiver's lane selector and puts yours back in the response |
| 2-3 | remaining packets, `0x0000` |
| 4 | protocol type, `0x00` |
| 5 | data size, the number of argument bytes, 1-80 |
| 6 | command class |
| 7 | command id, with bit 7 set for a query |
| 8-87 | arguments; bytes after the data size must be zero |
| 88 | XOR of bytes 2-87 |
| 89 | `0x00` |

#### Command check

The class, id and data size must be in the device's command list
(`devices.toml`), which has the commands that OpenRazer 3.12.4 and OpenRGB
1.1 send to the device. Setting the device mode (class `0x00`, id
`0x04`) accepts mode `0x00` or `0x03` with a zero second byte, and nothing
else.

#### Lighting bursts

Acquiring and releasing a lease around every report costs two extra round
trips. Hold one lease across a burst instead. Acquire it before the first
report and renew it when half the duration has passed. Release it 25 ms
after the last report, or right after any result that isn't `matched`.
Leases of 30 s work well.

### bench

Opt-in latency measurement. `hyperfluxctl bench` uses it to find out
whether a shorter wait between sending a request and the first read still
works on a given receiver, using nothing but read-only queries that the
bridge builds itself. Params:

| Field | Type | |
| --- | --- | --- |
| `lease_id` | `LeaseId` | |
| `route_token` | `RouteToken` | |
| `query` | `"firmware"` \| `"battery"` | command 00/81 or 07/80 |
| `first_get_delay_us` | number 1000-8000 | outside bench the bridge uses the device's learned wait (`first_get` in the snapshot) |
| `iterations` | number 1-100 | |
| `get_interval_us` | number 1000-10000, optional | the wait before reading again after a reply that didn't match, instead of the usual 1000; only with the `bench-interval` feature, and left out otherwise |
| `reread_busy` | boolean, optional, default `false` | `true`: a matching busy answer (status 0x01) doesn't end the exchange, and the bridge reads again; only with the `bench-interval` feature, and left out otherwise |

Errors: HFX-105, HFX-107, HFX-108 (also when the device does not support
the query) and HFX-109, as in transact's checks 1, 2 and 4. Sending
`get_interval_us`, or `reread_busy` set to `true`, without
`bench-interval` enabled gets HFX-102.

`get_interval_us` affects how often the bridge reads and nothing else.
Whatever the interval, the reads stop at the first wait plus 40 ms after the
SET, which leaves room for up to 41 reads at the usual 1 ms and for five at
10 ms. The match rules stay the same. Outside bench the interval is 1 ms,
and after a read that failed the next one comes 10 ms later.

With `reread_busy`, a matching busy answer does not end the exchange. The
bridge keeps reading at `get_interval_us` (1 ms without it) until a
matching answer with another status arrives or the same deadline passes.
If the reads run out while the device is still busy, the sample is that
busy answer (`device-status`, status 0x01). The other match rules stay as
they are. Every other exchange (the bridge's own and the apps') reads on
after a busy answer in this way. Without `reread_busy`, a sample is the
first matching answer, which lets bench show how often a device answers
busy.

Before each measured query the bridge sends a device mode query (00/84)
with the device's learned wait, and does not time it. A read that comes
too early gets the receiver's previous answer. Had that been the answer
to an identical query just before, it would match and pass for a fast
one. None of bench's exchanges change the learned wait.

Result `BenchResult`: `first_get_delay_us` (number), `get_interval_us`
(number: the one asked for, or 1000) and `samples`, one per iteration:

| Field | Type | |
| --- | --- | --- |
| `outcome` | `"ok"` \| `"device-status"` \| `"no-response"` \| `"failed"` \| `"not-sent"` | ok: matched with status 0x02; device-status: matched with another status; no-response: sent, nothing matched; failed: a transfer failed after sending |
| `status` | number, nullable | the receiver's status byte when matched: 0x01 busy, 0x02 success, 0x03 failure, 0x04 no response (also an asleep device), 0x05 not supported |
| `elapsed_us` | number | time from the SET to the end of the last read |
| `gets` | number 0-41 | read attempts used; at most 5 with a `get_interval_us` of 10000 |

### capture

Records what the receiver reports, to help add support for a new device.
Params:

| Field | Type | |
| --- | --- | --- |
| `after` | `Sequence`, nullable | last sequence the client has; null = only new events |
| `max_events` | number 1-256 | |
| `timeout_ms` | number 0-60000 | how long to wait if nothing is buffered |

The bridge keeps the last 1024 events. It replies with the events after
`after` (up to `max_events`), waiting up to `timeout_ms` for the first one.
Result `CaptureResult`:

| Field | Type | |
| --- | --- | --- |
| `next` | `Sequence` | pass as `after` next time |
| `gap` | bool | events after `after` were already dropped |
| `events` | array of `CaptureEvent` | ascending `sequence` |

`CaptureEvent`:

| Field | Type | |
| --- | --- | --- |
| `sequence` | `Sequence` | |
| `receiver_id` | `ReceiverId` | |
| `generation_id` | `GenerationId` | |
| `time_ms` | `Ms` | |
| `lane` | `"receiver"` \| `"pointer"` \| `"keyboard"` | |
| `kind` | `"receiver"` \| `"identity"` \| `"status"` \| `"activity"` \| `"raw"` \| `"overflow"` \| `"reply"` | |
| `source` | `"kernel"` \| `"passive"` \| `"inventory"` \| `"input"` \| `"bridge"` \| `"app"` | |
| `data` | `HexBytes` | receiver events: as below; replies: the 90-byte response |

A receiver event's data is 8 bytes, padded with zeros, except for `raw`.
Numbers are little-endian:

| `kind` | `data` |
| --- | --- |
| `receiver` | flags as a 4-byte number: bit 0 suspended, bit 1 going away, bit 3 ready to send |
| `identity` | product id (2 bytes), the mouse's contact byte or the keyboard's route byte, the keyboard's status byte |
| `status` | the status report's level, power, charge and extra bytes |
| `activity` | nothing |
| `raw` | `passive`: a vendor report (id 0x04, 0x05, 0x08 or 0x09) as it came, up to 16 bytes, report id first, before what it parses into. `inventory`: see below |

The bridge records a `receiver` event with `source` `kernel` when it opens
a receiver. It doesn't send `overflow` events today. Vendor reports 0x04
and 0x08 have lane `receiver`, since nothing says which lane sends them.
Each kind of vendor report is recorded at most 4 times a second.

A capture started with `after` null begins with one `raw` event per
receiver, with `source` `inventory` and lane `receiver`. It holds 72 bytes
of the pairing inventory as read when the bridge opened the receiver:

- the inventory's first 64 bytes
- its length and the marker's offset, 2 bytes each (0 when it wasn't
  recognised)
- the pointer's and the keyboard's product ids, big-endian as in the report

The rest is left out, since it may tell one receiver from another.

An `identity` event with `source` `bridge` is a device that the receiver's
paired slots named. Replies are recorded for queries (command id bit 7)
and nothing else, and never for the serial number query (00/82) or the
paired-slots query. Ordinary key presses and pointer movement are never
recorded. An `activity` event says that input happened, and no more.

### probe-receiver

Needs the `probe-receiver` feature. The bridge sends its fixed set of
read-only queries to the receiver itself, one after the other, and returns
the answers. This is for finding out how the receiver keeps its list of
paired devices. The bridge also sends the first and the third on its own
to name the devices (`inventory` `"slots"` in the snapshot).

Params: `receiver_id` (`ReceiverId`). By transaction id, class:command and
data size, the queries are `08 00:bf 49`, `88 00:bf 49`, `3f 00:bf 31` and
`1f 00:c5 3`, all with zero arguments. They go out as they are, without
the lane selector. No other request can do that. Each waits 8 ms before
its first read and is matched like any exchange. Result `ReceiverProbe`:

| Field | Type | |
| --- | --- | --- |
| `inventory` | `HexBytes` | the pairing inventory's first 22 bytes, as read when the bridge opened the receiver; empty without one |
| `answers` | array of `ReportHex`, nullable | the matching answer to each query, in the order above; null when none matched or it wasn't sent |

An unknown or read-only receiver gets HFX-109.

## Error codes

1xx codes come from the bridge. 2xx to 4xx are Doctor findings
(`hyperfluxctl doctor`) and never appear on the wire. The message is the
summary below. HFX-101 and HFX-102 may add one sentence, as described
under `hello`.

| Code | Message | Used for |
| --- | --- | --- |
| HFX-101 | This application and the HyperFlux Next V2 service don't share a protocol version. | hello |
| HFX-102 | This feature isn't available from the running HyperFlux Next V2 service. | hello, unknown method, feature not enabled |
| HFX-103 | The application sent a request HyperFlux Next V2 can't accept. | method before hello, second different hello, inverted version range, reused request id, report or command refused |
| HFX-104 | Another connection is already using this client id. | hello |
| HFX-105 | The application doesn't hold control of this device. | lease problems |
| HFX-106 | HyperFlux Next V2 is busy and didn't start this operation. | lease limit |
| HFX-107 | The receiver reconnected, so this device handle is out of date. | token from an ended generation |
| HFX-108 | HyperFlux Next V2 can't control this device that way. | unknown or unqualified token, unsupported bench query |
| HFX-109 | The receiver couldn't take the request, so nothing was sent. | route unavailable, receiver not writable or gone |
| HFX-110 | The request may have reached the device, so HyperFlux Next V2 won't repeat it. | transact outcome `may-have-been-sent` |
| HFX-111 | The request ran out of time before it could be sent. | transact timeout before sending |
| HFX-112 | HyperFlux Next V2 stopped this operation because an internal check failed. | bug in the bridge |

The full catalog, Doctor codes included, is in
`crates/hfx-proto/src/errors.rs`. Each code is explained in
`docs/user/troubleshooting.md`.

## Changing the protocol

New methods and fields go behind a new feature name, and older clients
never see them. A change that an older client would misread needs a new
protocol version. The bridge then serves both versions for at least one
release, and HFX-101 tells old clients which versions are left.
