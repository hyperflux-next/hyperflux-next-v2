# HyperFlux Next V2 architecture

HyperFlux Next V2 makes Razer devices paired to a Razer HyperFlux V2
receiver (USB `1532:00CF`) work on Linux as if they were plugged in. The
desktop's battery widget shows their batteries. OpenRazer and Polychromatic
control them, and OpenRGB shows them through a plugin. This file covers the
pieces, how data moves between them, the interfaces they share and why they
look the way they do.

Where two descriptions disagree, `docs/protocol.md` (the client protocol,
version 1) wins, and after it comes `crates/hfx-proto`, the Rust
implementation of the protocol, which also holds the error catalog, the
device table and the OpenRazer route constants. Then the device catalog,
`devices.toml`. This file comes last.

## 1. Pieces

- The bridge, `hyperflux-next-bridge` (`crates/hfx-bridge`), runs as the
  system user `hyperflux-next`. Nothing else writes to the receiver, which
  the bridge reaches through hidraw. It serves the client protocol and
  decodes batteries, with probes at start and after resume. It also decides
  which battery devices and which virtual HID devices (the ones OpenRazer
  binds) should exist, and the keeper holds those for it. One process and
  one event loop, plus an exchange thread per receiver.
- The device keeper, `hyperflux-next-keeper` (`keeper.rs` in
  `crates/hfx-bridge`), runs as the system user `hyperflux-keeper`. It reads
  the receiver's input. Vendor reports go on to the bridge, while key
  reports and mouse movement reach it as bare activity. Through `/dev/uhid`
  the keeper makes the battery devices and the OpenRazer devices from two
  fixed templates, and for the OpenRazer ones it passes the kernel's
  requests to the bridge and the bridge's answers back. One process, one
  event loop (section 4). Section 7 says why it's separate.
- The simulator, `hyperflux-next-sim` (`crates/hfx-bridge`), runs as any
  user. It's a fake receiver behind hidraw, for running the whole stack
  without hardware. A developer tool, not packaged.
- `hyperfluxctl` (`crates/hyperfluxctl`) runs as the user. Its commands:
  status, devices, battery and watch; doctor, report and support-bundle;
  bench, capture, stall-check and probe receiver.
- The patched OpenRazer drivers (`integrations/openrazer`) run in the
  kernel. They are OpenRazer 3.12.4's `razerkbd` and `razermouse`, changed
  to accept the bridge's virtual devices as well.
- The OpenRGB plugin (`integrations/openrgb`) loads inside OpenRGB. It shows
  the receiver's devices as OpenRGB controllers and adds a battery page.
- The C++ SDK (`sdk/cpp`) is the protocol client library the plugin uses.
- The Python SDK (`sdk/python`) is a small read-only client with an example.
  Source only.
- Packaging, units, udev rules, CI and docs live in `packaging`, `systemd`,
  `udev`, `.github` and `docs`.

There are four packages:

- `hyperflux-next-v2-linux` (Core): the bridge, the keeper, `hyperfluxctl`,
  the four units, sysusers, `udev/99-hyperflux-next.rules`, `uhid` in
  modules-load.d, the man page and completions. No DKMS part and no kernel
  module. It replaces the `hyperflux-next-dkms` and
  `hyperflux-v2-linux-dkms` packages.
- `hyperflux-next-v2-openrazer-driver-dkms`: the patched OpenRazer drivers
  as a DKMS tree, with OpenRazer's own udev rules.
- `hyperflux-next-v2-full` (Full): the OpenRazer access and key-link rules,
  the OpenRazer modules in modules-load.d, and `full.json`. It depends on
  the other two, and on Arch and Debian on the plugin package as well.
- `hyperflux-next-v2-openrgb-plugin`: the OpenRGB plugin and the nlohmann
  json notice. Depends on Core.

The plugin package is built for a different OpenRGB on each distribution:

| Distribution | Built for | Relation to OpenRGB | Full |
| --- | --- | --- | --- |
| Arch | Arch's OpenRGB 1.1, Qt 6 | optional | depends on it |
| Debian | Debian's OpenRGB 1.0, Qt 5 | enhances | depends on it |
| Ubuntu | openrgb.org's 1.1 Trixie package, Qt 6 | depends on `openrgb (>= 1.1), (<< 1.2~)` | recommends it |
| Fedora | openrgb.org's 1.1 Fedora 44 package, Qt 6 | requires `openrgb >= 1.1` and `< 1.2~` | recommends it |
| openSUSE | not built | | |

The openrgb.org packages link the distribution's own Qt 6. That's why a
plugin built against that Qt loads in them but not in their Flatpak (KDE
runtime, Qt 6.10) or AppImage (Qt 6.8), which bring an older Qt than the
6.11 and 6.12 of the Qt 6 builds.

The first three packages each take over a `hyperflux-next-*` name:
`hyperflux-next-linux`, `hyperflux-next-openrazer-driver-dkms` and
`hyperflux-next-full`. For pacman that means `conflicts`, `replaces` and
`provides`. Debian gets `Conflicts`, `Replaces` and `Provides`. RPM gets
`Obsoletes` below `1:1.0.1` and `Provides` at it. That version sorts above
every build of those names, which keeps a package from obsoleting itself.
The plugin package takes over no name, but on Debian it may still replace
files of `hyperflux-next-full` in case apt unpacks it first.
`tests/test_packaging.py` checks the three recipes. The units, the system
user and groups, the paths and the udev files have the same names as in the
packages being replaced.

The accounts and groups, from sysusers:

- `hyperflux-next`, a user and group with the bridge as its one member. The
  user owns the receiver's interface 0, for reading and writing. The group
  has interface 4.
- `hyperflux-keeper`, a user and group with the keeper alone in it. It can
  read interfaces 0 and 1 and use `/dev/uhid` (Core), and write the four
  OpenRazer `unbind` files (Full).
- `hyperflux`, a group for the bridge, the keeper and the
  `systemd-socket-proxyd` instances. It grants access to the sockets in
  `/run/hyperflux-next`.
- `openrazer` or `plugdev`, the group for OpenRazer's users, with access to
  OpenRazer's sysfs files. Each distribution's build picks the name.

Nobody else is ever added to `hyperflux-keeper`. The OpenRazer patch trusts
virtual devices because no other group can create them. Section 7 covers the
one exception.

## 2. Data flow

### Receiver access

The receiver stays on the kernel's own HID drivers (usbhid, hid-generic),
and its keyboard and mouse input reaches the desktop as usual. Between them,
the bridge and the device keeper open three of its hidraw nodes:

- Interface 0, read-write, by the bridge. It carries the 90-byte feature
  exchanges with either lane, picked by byte 1 (0x1f pointer, 0x9f
  keyboard). The paired-slots query (00:bf) uses it too.
- Interface 0 again, read-only, by the keeper. The mouse's movement comes
  out here as a boot mouse's reports without report ids. It counts as
  pointer activity.
- Interface 1, read-only, by the keeper. Passive vendor reports arrive here
  among the keys' input. Ids 0x05 and 0x09 are identity and status. Reports
  0x04 and 0x08 are just recorded. The key input counts as keyboard
  activity.
- Interface 4, read-write, by the bridge, once per open. It has the pairing
  inventory (feature report 1), which the bridge falls back on when the
  paired slots stay silent and reads by its marker and the two product ids
  after it.

The keeper passes the vendor reports and the activity on to the bridge,
which decodes the reports (section 3.8).

An exchange is one `HIDIOCSFEATURE`, then the device's first-GET wait, then
`HIDIOCGFEATURE` until a reply matches (`hfx-bridge/src/wire.rs`). A busy
answer matches too. It means the answer is still coming, and the bridge
reads on. Every read after the first is there for a reply that answers
something else, usually the receiver's previous answer. After such a reply,
or a busy one, the bridge reads again 1 ms later, and after a failed read
it waits 10 ms. No read starts more than 40 ms after the first, the span of
five reads 10 ms apart. Bench may read at other intervals (section 4,
"Bench").

The wait starts at `first_get_delay_us` from the device catalog:

- 3 ms for the Basilisk V3 Pro 35K
- 4 ms for the DeathStalker V2 Pro TKL
- 6 ms for the other listed devices
- 8 ms for a product missing from the catalog

The tested devices' waits and the 1 ms come from bench runs on hardware
(section 10, "Receiver timing"). Exchanges run on the receiver's exchange
thread.

Starting from the catalog's wait, the bridge learns each device's own wait
(`wire::Learned`). It keeps learned waits across restarts and replugs in
`/var/lib/hyperflux-next/learned-waits.json` (`waits.rs`). A kept wait
belongs to the receiver, lane and product that learned it, and to that
firmware as well once the bridge has seen the device's answer to 00:81. A
different product or firmware drops it. It also lapses after 30 idle days.

The bridge reads the file when the first receiver is admitted. After a
change it writes a new file and renames it over the old one, at most once a
minute. If the file is missing or unreadable, the catalog's waits apply. An
unreadable one is logged once.

Learning carries on from a kept wait, within the same limits. The bridge
looks at each run of 50 exchanges with the device, counting its own and the
apps' but never bench's. If at least 49 had the answer at the first read,
the wait drops by 0.5 ms, and if more than 5 needed another read, it grows
by 0.5 ms. It stays between 2 and 8 ms.

An early read gets the previous answer. To the match rules, that answer
passes for one to an identical request. If the previous answer had the same
key (`wire::answer_key`), or is unknown, the exchange therefore waits at
least the catalog's wait and is left out of the count, a call the exchange
thread makes because it knows what the receiver answered last. A busy
answer at the first read counts as too early, the same as needing a second
read. An asleep answer counts neither way.

The wait has no effect on the match rules, the busy handling, the re-reads
or the deadline. The snapshot carries it as `first_get`, with `saved_us`
when it went on from a kept wait. `hyperfluxctl devices` shows it.

The bridge also counts each device's exchanges since the bridge started,
again leaving out bench. It counts how many went out, how many had the
answer at the first read and how many got none at all. Re-reads are counted
too, separately after a reply to something else and after a busy one. The
snapshot carries these counts (`exchanges`), and `hyperfluxctl devices`
shows them.

### Battery

```
device status report (passive, via the keeper)      \
bridge's own 07/80, 07/84 probes and charge checks   +--> bridge decodes --> keeper's uhid battery device --> hid-input power supply --> UPower --> desktop
app queries (OpenRazer, OpenRGB) seen by the bridge /          |
                                                               +--> snapshot / wait
```

Each paired device gets a virtual HID device of its own through `/dev/uhid`.
The keeper makes it when the bridge asks, and names it from the catalog:

| Field | Value |
| --- | --- |
| name | catalog `name` |
| phys | `hyperflux-next/battery/<receiver hex>/<lane>` |
| uniq | `hyperflux-<receiver hex>-<lane>` |
| vendor | 0x0000 |
| product | the device's wireless id |

Its descriptor has two parts. One is a mouse or keyboard application
collection that never sends anything, and the other is an input report
with Battery Strength (0-100) and Battery System Charging.

hid-generic binds the device, and hid-input turns the battery into a power
supply. UPower types the power supply by the input device next to it. A
mouse shows as a mouse. The keyboard collection is a boot keyboard's report
(modifiers, a reserved byte and six keys), because udev marks an input
device `ID_INPUT_KEYBOARD` only when it has every key from Esc to D, Left
Ctrl included. Vendor 0 and a phys outside the OpenRazer prefix keep the
patched drivers away.

In Linux 7.2, hid-input sets up every battery as discharging
(`hidinput_setup_battery`), and the charging usage is the one thing that
changes that (`hidinput_update_battery_charge_status`). The power supply
reads "unknown" in a single case: before the first level of a battery the
kernel doesn't query. A device whose charging the bridge can't learn
therefore shows as discharging on the desktop, even on its cable. Dropping
the usage wouldn't help. `hyperfluxctl` and the OpenRGB page say "charging
not reported" for it instead.

The kernel sends `UHID_START` early, before hid-input has set up the power
supply. Input reports sent before that are dropped. The keeper resends the
report 50 ms, 500 ms and 2 s after `UHID_START`, and on the same schedule
after `UHID_OPEN`. To match, the simulator's uhid stand-in drops reports for
20 ms after start.

Every request and matched response passes through the bridge. It takes
battery and charge from any successful 07/80 or 07/84 reply, whether it
asked or an application did.

On the simulator, the start-up probes have both readings a few milliseconds
after the receiver is admitted. The battery devices show them from the first
resend, 50 ms after they start. On the test machine the bridge was restarted
with both devices paired. The journal had the mouse's level 4.4 ms after the
receiver was admitted, and its charging 13.6 ms after.

Not timed: how soon the desktop's battery widget showed them.

### Lighting and settings through OpenRazer (Full)

```
openrazer-daemon (user) --sysfs--> razerkbd/razermouse (patched)
     --hid_hw_raw_request SET+GET--> uhid --/dev/uhid--> hyperflux-next-keeper
     --keeper.sock--> hyperflux-next-bridge
     --HIDIOCSFEATURE/HIDIOCGFEATURE--> receiver interface 0 --> device
```

The keeper reads the SET from `/dev/uhid` and hands it to the bridge, which
runs the exchange itself. Because the bridge acknowledges the SET first,
OpenRazer's GET is already waiting when the response arrives, and the bridge
answers it with the matched response or an error envelope. The keeper passes
both to the kernel. The cost is one socket hop each way, about 0.3 ms per
exchange on the simulator (section 6).

### Lighting through OpenRGB (the plugin package)

```
OpenRGB + plugin --/run/user/UID/hyperflux-next-bridge.sock--> systemd-socket-proxyd
     --/run/hyperflux-next/bridge.sock--> bridge --hidraw--> receiver --> device
```

Each report is one protocol round trip. The plugin holds a single lease for
a whole burst (`docs/protocol.md`, "Lighting bursts").

### Diagnosis

`hyperfluxctl` speaks the same protocol (snapshot, wait, bench, capture,
probe-receiver), but Doctor also reads system state directly: sysfs, `/dev`
permissions, `modinfo`, `systemctl` and `busctl`. `stall-check` reads the
receiver's pointer input nodes and `journalctl -k`. Nothing is changed
unless `doctor --fix` asks first.

## 3. Interfaces

### 3.1 Client protocol

`docs/protocol.md` describes the protocol (version 1), and `hfx_proto::{rpc,
messages, negotiation, framing, client}` implements it. The golden messages
in `crates/hfx-proto/tests/fixtures/` are the conformance set for the Rust,
C++ and Python clients. A fixture changes together with `docs/protocol.md`
or not at all. All three clients reject unknown fields, which is why a new
field means changing each of them in the same commit.

### 3.2 OpenRazer route contract

`crates/hfx-proto/src/openrazer.rs` holds the values that the bridge and the
OpenRazer patch must agree on:

- `BUS_VIRTUAL`
- the `hyperflux-next/native/v1/` phys prefix and the label rule
- the 19-byte feature-only report descriptor
- the `HFXNERR1` error envelope, with `-EAGAIN` for not sent and
  `-ENOTRECOVERABLE` for may have been sent
- the capability `hyperflux_native_transport = razer-feature-report-v1`

The patched modules expose the capability twice. For `modinfo` it's a
`MODULE_INFO` tag, but the bridge can't read `/usr/lib/modules` from inside
its sandbox and gets a read-only module parameter instead:
`/sys/module/<module>/parameters/hyperflux_native_transport`. The Python
test checks that the patch contains these values.

The virtual device (UHID_CREATE2):

| Field | Value |
| --- | --- |
| name | catalog `name` |
| phys | prefix + `native-` + first 16 hex digits of SHA-256(`"hyperflux-next uhid\0"` \|\| receiver_id \|\| generation as u64 LE \|\| device_id) |
| uniq | empty |
| bus | 0x06 |
| vendor | 0x1532 |
| product | the device's wireless id |
| version | 0x0100 |
| country | 0 |
| descriptor | the one above |

The patched driver binds it only if the product id is already in its own USB
table.

### 3.3 Device facts

`devices.toml` is the catalog. The files below copy parts of it, and
`tests/test_device_tables.py` checks each copy in the format given here.
Missing files are skipped.

- `crates/hfx-proto/src/devices.rs`: the `RECEIVER` and `DEVICES` literals
  as written. One field per line, with commands as `cmd(0x.., 0x.., sizes)`
  or as the name of a shared `&[Command]` list.
- `crates/hfx-proto/src/openrazer.rs`: `MODULES`, `PATCHED_MODULES` and the
  contract constants.
- `integrations/openrgb/src/device_table.hpp`: one row per device with an
  `[openrgb]` part, in the form `{0x00cd, DeviceKind::Mouse,
  "basilisk_v3_pro_35k_wireless_device"},`. Devices that OpenRGB 1.0 lacks
  go between `#if HFX_OPENRGB_QT >= 6` and `#endif`.
- `integrations/openrazer/70-hyperflux-next-openrazer-keys.rules`: one
  `SYMLINK+="input/by-id/hyperflux-next-$env{ID_PATH_TAG}-<suffix>"` per
  catalog `key_event_link`. The matches on `ENV{ID_MODEL_ID}=="00cf"`,
  `ENV{ID_USB_INTERFACE_NUM}=="01"` and `ENV{ID_INPUT_KEYBOARD}=="1"` sit in
  one rule.
- `integrations/openrazer/hyperflux-next-openrazer.modules-load.conf`: the
  four OpenRazer modules in catalog order, then `uhid`, one per line.
- `integrations/openrazer/patches/*.patch`: contains the phys prefix, the
  capability name and value, `HFXNERR1`, and the descriptor bytes as `0x..`
  literals in order.

Each device's command list holds every command (class, id, data sizes) that
OpenRazer 3.12.4 or OpenRGB 1.1 sends it, including the queries OpenRGB
makes while it builds a controller:

- 0f/81 with 80 bytes
- 0f/80 with 80, for the devices it addresses with LED id 0
- 00/86 with 2

OpenRGB, and not OpenRazer, drives the BlackWidow V4 Low-Profile TKL
HyperSpeed and the Naga V3 Pro. Their lists also keep 00/84, the query bench
sends first (section 4). The bridge refuses anything else before it reaches
the receiver, whoever sends it, its own OpenRazer routes included
(`hfx_proto::devices::check_command`).

Each device has a `tier` (`tested`, `checked` or `listed`) and a
`[device.sources]` table naming where the entry comes from. Two tests
compare the entries with the pinned sources:
`integrations/openrgb/tests/check-device-table.py` and
`tests/test_upstream_tables.py` (section 10, "Device catalog").

### 3.4 Paths, sockets and units

Runtime paths are constants in `hfx_proto::paths`, and install paths are
variables in the top-level `Makefile`. They must agree for a default
install.

| Path | Owner | |
| --- | --- | --- |
| `/usr/lib/hyperflux-next/hyperflux-next-bridge` | Core | |
| `/usr/lib/hyperflux-next/hyperflux-next-keeper` | Core | |
| `/usr/bin/hyperfluxctl` | Core | |
| `/run/hyperflux-next/` | `RuntimeDirectory=`, 2750 `hyperflux-next:hyperflux` | |
| `/run/hyperflux-next/bridge.sock` | bridge, 0660 | system socket |
| `/run/hyperflux-next/bridge.lock` | bridge, 0600, `flock` | one bridge at a time |
| `/run/hyperflux-next/keeper.sock` | bridge, 0660 | where the keeper connects (section 3.8) |
| `/var/lib/hyperflux-next/` | `StateDirectory=`, 0700 `hyperflux-next` | |
| `/var/lib/hyperflux-next/learned-waits.json` | bridge, 0600 | learned first-GET waits (section 2) |
| `/run/user/<uid>/hyperflux-next-bridge.sock` | `hyperflux-next-client@<uid>.socket` | per-user socket |
| `/usr/share/hyperflux-next/components/full.json` | Full | tells Doctor Full is installed |
| `/usr/lib/openrgb/plugins/hyperflux-next-openrgb.so` | plugin package | OpenRGB's system plugin directory, in the distributions' and openrgb.org's packages alike |

`full.json` holds one JSON object, `{"component":"full","version":"1.0.0"}`.
Doctor reports HFX-405 when the file is unreadable or its version differs
from `hyperfluxctl`'s own. When Full is ready, Doctor also looks for the
plugin file and notes a missing plugin (on Ubuntu and Fedora the plugin is
a separate install).

The bridge unit is `Type=notify` with `NotifyAccess=main` and
`WatchdogSec=30s`. It restarts with `Restart=on-failure` and
`RestartSec=2s`. `ExecStart=/usr/lib/hyperflux-next/hyperflux-next-bridge`
takes no arguments. There's no configuration file.

The bridge runs as `User=hyperflux-next`, `Group=hyperflux` and
`SupplementaryGroups=hyperflux-next`. It gets
`RuntimeDirectory=hyperflux-next` with `RuntimeDirectoryMode=2750`, and
`StateDirectory=hyperflux-next` with `StateDirectoryMode=0700`. The state
directory is made writable by systemd, which names it in `$STATE_DIRECTORY`,
and without that variable the bridge keeps no state. No package needs a
tmpfiles.d line for the directory, because systemd makes it when the bridge
starts. The RPM lists it as `%ghost`, and Debian's purge removes it.

The unit is ordered `After=systemd-udevd.service` and
`Before=systemd-user-sessions.service`. The second one matters at shutdown.
It makes the user's openrazer-daemon stop before the virtual devices go
away. Without it, shutdown hangs for 90 s. With
`Wants=hyperflux-next-keeper.service`, enabling the bridge is enough.

The bridge's sandbox:

- `RestrictAddressFamilies=AF_UNIX AF_NETLINK` with `IPAddressDeny=any`
- `DevicePolicy=closed` with `DeviceAllow=char-hidraw rw`
- `ProtectSystem=strict`, `ProtectKernelTunables=yes` and
  `ProtectKernelModules=yes`
- `ProtectProc=invisible` with `ProcSubset=pid`
- `SystemCallFilter=@system-service`, minus `@mount`, `@privileged` and
  `@resources`
- `RemoveIPC=yes`, `UMask=0077`, empty capability sets and
  `NoNewPrivileges=yes`
- `ReadWritePaths=` for `/run/hyperflux-next`, with the state directory
  writable as well

The unit also sets `Documentation=man:hyperfluxctl(1)` and
`WantedBy=multi-user.target`. `tests/test_packaging.py` checks most of these
lines.

The keeper's unit, `hyperflux-next-keeper.service`, is `Type=notify` too,
with the bridge's watchdog, restart settings and sandbox. It runs as
`User=hyperflux-keeper`, `Group=hyperflux` and
`SupplementaryGroups=hyperflux-keeper`. Its device policy is
`DeviceAllow=char-hidraw r` and `DeviceAllow=/dev/uhid rw`. Its
`ReadWritePaths=` are the four optional OpenRazer `unbind` files, and it
writes nowhere else, not even in `/run/hyperflux-next`, where it just
connects.

The keeper is ordered `After=` the bridge, whose socket it reaches. Like the
bridge, it's `Before=systemd-user-sessions.service`, since it holds the
virtual devices. It has no `[Install]` section, because the bridge pulls it
in. It has no runtime or state directory either, and the bridge's runtime
directory can't be shared, as systemd removes it when the bridge stops.

`hyperflux-next-client@.socket` and its `@.service` set
`DefaultDependencies=no`, with `Conflicts=` and `Before=shutdown.target`.
The socket has `SocketMode=0600` and `SocketUser=%i`. The proxy runs as
`%i:hyperflux`, and reads `/run/hyperflux-next` without writing to it.

There are no other units or drop-ins.

### 3.5 udev rules

- `udev/99-hyperflux-next.rules` (Core) matches `1532:00cf`. Interface 0's
  hidraw node gets `OWNER="hyperflux-next", GROUP="hyperflux-keeper",
  MODE="0640"`. Interface 1's gets `GROUP="hyperflux-keeper", MODE="0640"`,
  and interface 4's `GROUP="hyperflux-next", MODE="0660"`. `/dev/uhid`
  (`static_node=uhid`) gets `GROUP="hyperflux-keeper", MODE="0660"`. The ids
  and the interface number are matched in separate rules, because udev
  matches all `ATTRS` of one rule on one parent.
- `integrations/openrazer/69-hyperflux-next-openrazer-access.rules` (Full)
  acts on `ACTION=="add", SUBSYSTEM=="drivers"` for the four OpenRazer
  drivers. It gives `/sys%p/unbind` the group `hyperflux-keeper` and mode
  0220.
- `integrations/openrazer/70-hyperflux-next-openrazer-keys.rules` (Full)
  makes per-receiver key-press links for Ripple, on interface 1's keyboard
  node and nowhere else.
- OpenRazer's `99-razer.rules` and `razer_mount` (openrazer-driver-dkms) are
  the upstream files plus bind rules for virtual devices, with the group
  filled in at install.

### 3.6 Build fragments

The top-level `Makefile` defines every install path and build switch, and
includes one fragment per component. A fragment adds targets and appends to
the shared lists. It never defines paths.

- `integrations/openrazer/openrazer.mk`: `openrazer-tree` fetches OpenRazer
  into `.upstreams/openrazer` and patches it. It also has `openrazer-test`,
  plus `install-openrazer-dkms-tree` and `install-openrazer-full`. The first
  install target is a prerequisite of `install-openrazer-dkms`, the second
  of `install-full`.
- `integrations/openrgb/openrgb.mk`: `openrgb-plugin`, `openrgb-test` and
  `openrgb-load-test`, and `install-openrgb-plugin` for the plugin package.
- `sdk/sdk.mk`: `sdk-test`, which it adds to `TEST_TARGETS`.
- `crates/hyperfluxctl/cli.mk`: `install-man` and `install-completions`,
  both prerequisites of `install-linux`.
- `packaging/packaging.mk`: the package and repository targets, and
  `install-linux-files`, `install-full-files` and
  `install-openrgb-plugin-files`.

There's one DKMS source tree,
`$(DKMSSRCDIR)/openrazer-driver-$(OPENRAZER_VERSION)/`, with the patched
OpenRazer drivers.

Upstream pins are `KEY=value` lines, readable by make and by sh alike.
`integrations/openrazer/PIN` has:

- `OPENRAZER_VERSION=3.12.4`
- `OPENRAZER_COMMIT=1ef8a91712906a89fd332a2ed62bbd105315d6d1`
- `OPENRAZER_REPO=https://github.com/openrazer/openrazer.git`

`integrations/openrgb/PIN` has:

- `OPENRGB_QT6_COMMIT=8e16de27858cf2be6af54a33ce003df74cb9fd3e`, for 1.1
- `OPENRGB_QT5_COMMIT=81bbe18a84c2e507006f19dd252e397e40a56bfe`, for 1.0
- `OPENRGB_REPO=https://github.com/CalcProgrammer1/OpenRGB.git`

Each integration has a `fetch.sh <dir>` that clones the pinned commit,
checks it and applies its patches.

### 3.7 Version locations

The product version is `version` in the root `Cargo.toml`.
`scripts/set-version X.Y.Z` rewrites the other places that name it, and
`tests/test_versions.py` checks that they agree:

| File | Line |
| --- | --- |
| `integrations/openrgb/CMakeLists.txt` | `project(hyperflux-next-openrgb VERSION 1.0.0 ...)` |
| `sdk/cpp/CMakeLists.txt` | `project(hyperflux-sdk VERSION 1.0.0 ...)` |
| `sdk/python/pyproject.toml` | `version = "1.0.0"` |
| `packaging/arch/PKGBUILD` | `pkgver=1.0.0` |
| `packaging/debian/changelog` | first line `hyperflux-next-v2 (1.0.0-1) ...` |
| `packaging/rpm/hyperflux-next-v2.spec` | `Version: 1.0.0` |
| `CHANGELOG.md` | first heading `## 1.0.0` |

The keeper binary takes its version from the same crate.

### 3.8 Bridge and keeper

The keeper connects to the bridge at `keeper.sock`, next to the bridge's own
socket (`hfx-bridge/src/ipc.rs`). Before reading a byte, each side checks
the other with `SO_PEERCRED`. The system bridge accepts the
`hyperflux-keeper` user's uid and no other. The keeper accepts a bridge
running as `hyperflux-next`. Both are looked up in `/etc/passwd`, where
sysusers puts them. With `--socket`, either side expects its own uid
instead, which lets a developer's pair work while the system's stays out of
its reach. The bridge takes one keeper at a time.

Framing is the same as the client protocol's: a big-endian u32 length, then
JSON. A frame is at most 1 KiB. Every message has one of these shapes, with
exactly these fields:

| From | Kind | Fields |
| --- | --- | --- |
| keeper | `hello` | protocol version, whether `/dev/uhid` opens |
| keeper | `activity` | receiver id, lane, time |
| keeper | `vendor` | receiver id, time, the report's first 16 bytes as hex, its length |
| keeper | `request` | device, the kernel's request id, the SET's 90-byte report or none for a GET |
| keeper | `started`, `stopped` | device; `UHID_START`'s flags |
| keeper | `ended` | device, and why: it couldn't be made, its file failed, or it was destroyed on request |
| bridge | `hello` | protocol version |
| bridge | `create` | device, and a template: `openrazer` with a product id and a phys label, or `battery` with a receiver id, a lane and a product id |
| bridge | `destroy` | device |
| bridge | `reply` | device, request id, errno, and a GET's report |
| bridge | `battery` | device, level, charging |

Device ids are the bridge's. An unparseable frame ends the connection, but
a frame that parses and asks the other side for something it refuses to do
is logged once and dropped. The connection stays up.

The keeper refuses:

- a product the catalog lacks, or, for the `openrazer` template, one
  OpenRazer doesn't drive
- a label other than `native-` and 16 lowercase hex digits
- a receiver id other than `receiver-` and 24 of them
- a second device with an id or phys already in use
- more than 64 devices
- a reply to no outstanding request, or of the wrong shape (the kernel then
  gets EIO)
- a battery level above 100, or for anything but a battery

The keeper names every device from the catalog, as the bridge would. The
bridge sends no names. For OpenRazer devices the keeper passes on GETs and
SETs of feature report 0, as long as SETs are exactly 90 bytes and no more
than four requests are out at a time. Anything else the keeper answers with
EIO itself, as it does every request while no bridge is connected.

The bridge refuses a vendor report with an id outside the four, or with a
length that doesn't fit. It also refuses an `ended` reason over 200 bytes or
outside printable ASCII. An accepted reason goes into the log and the
snapshot. Times more than a minute old are taken as a minute old. Future
times are taken as now.

When the bridge goes away, the keeper answers the kernel's waiting requests
with EIO. It unbinds and destroys the OpenRazer devices, and destroys the
batteries. Then it reconnects after 100 ms, 200 ms, 400 ms and 1 s, and
every 2 s after that. A refusal gets the same backoff.

When the keeper goes away, the bridge forgets the devices it had there,
stops offering OpenRazer devices ("the device keeper isn't connected") and
stops publishing batteries.

Either way, after the next hello the bridge makes the batteries again at
once. OpenRazer devices follow after two serial reads.

## 4. Bridge behaviour other parts rely on

This is what the bridge does, in more detail than the protocol spells out.
Clients and packaging depend on it. A change here usually means changing
them too.

### Event loop

One thread runs epoll over:

- the listening sockets and the client connections
- the keeper's connection
- udev's netlink messages, for hotplug
- a kernel uevent netlink socket, for OpenRazer drivers and wired twins

Each round works out the next deadline from all the timers and sets the
epoll timeout from it. There's no periodic tick. The one regular wake-up is
the systemd watchdog ping, every `WATCHDOG_USEC / 2`. Exchanges run on one
worker thread per receiver, bounded by their 1 s deadline, and a receiver
has one exchange in flight at a time.

The keeper's loop works the same way. It watches each receiver's input fds
(hidraw interfaces 1 and 0), the `/dev/uhid` fds, the bridge's socket and
udev's messages. Interface 0 is armed with `EPOLLONESHOT`: a wake-up empties
its queue without blocking and sends the pointer's activity at the usual
rate, and the keeper re-arms it 100 ms later. Movement then costs the keeper
about ten wake-ups a second, and the bridge one per activity event. A mouse
at rest costs nothing.

### Discovery

At start the bridge scans `/sys/class/hidraw` for nodes whose HID device is
`1532:00CF`, and groups them by USB device. A receiver needs nodes for
interfaces 0 and 1. Interface 4 is optional. A hidraw uevent triggers a
rescan. New nodes under the same USB port mean a replug, and a new
generation.

udev sets the group a moment after a node appears, which is why an open that
fails with `EACCES` gets another try on each later hidraw uevent, even when
the rescan looks unchanged (udev reuses the names). Retries 100 ms, 500 ms
and 2 s later cover a missed event. The keeper finds and opens the receivers
the same way, on its own.

With `--read-only` the bridge never opens interface 0 for writing, and the
keeper still reads its input. Since hidraw has no writer role, the bridge
assumes no other program writes to interface 0 (section 7). When udev
reports the nodes gone, the generation is retired.

### Identifiers

- `receiver_id`: `receiver-` plus the first 24 hex digits of
  SHA-256(`"hyperflux-next receiver\0"` || USB sysfs path of the receiver ||
  vid BE || pid BE). It's stable for a port and needs no secret file.
- `device_id`: `<lane>-<pid as 4 hex>`.
- `server_instance_id`: `bridge-` plus 24 random hex digits.
- Route tokens: 32 random bytes, as hex.
- Lease ids: `lease-<8 random hex>-<counter>`.

The bridge's own lease holders are named `OpenRazer` (virtual devices) and
`HyperFlux` (battery probes and bench).

### Devices and presence

A lane's identity comes from the receiver's paired slots. When those are
silent, it comes from the pairing inventory. A passive identity report also
works. A different product id on a lane replaces the device: the old one's
route, lease, battery and virtual devices go, and the new one starts fresh.

A status report on a lane without an identity gets a stand-in device, with
product id 0 and a battery but no route. Once the receiver names the
device, the named device takes over the stand-in's reading.

Liveness evidence is activity, a passive identity or status report, or a
matched response with any status but 0x04. Presence follows from it:

- `online` after evidence.
- `asleep` after a matched 0x04 response, until the next evidence.
- `unreachable` after a keyboard identity with route byte 2. By that, the
  receiver says the keyboard is gone. The tested keyboard is the one device
  that has shown this. Any other gets a probe first, and turns `unreachable`
  when the probe gets no answer.
- `waiting` before any evidence in this generation, and after a system
  resume.

A catalog device paired on the lane it doesn't usually use keeps its entry.
Route, OpenRazer and OpenRGB work as usual, because exchanges go by lane
anyway. Its status reports on that lane aren't decoded, and it gets no mat
contact and no charge watch. The log names its usual lane. The snapshot
shows it by its `kind` and `lane`.

### Suspend

Runtime autosuspend is invisible to the bridge. Presence and routes stay,
`state` stays `active`, and an exchange wakes the receiver.

A system suspend freezes the bridge. On resume it notices the jump between
`CLOCK_BOOTTIME` and `CLOCK_MONOTONIC`, sets all devices to `waiting`
(which revokes their tokens) and sends battery probes. Routes come back with
the first evidence.

### Routes

A device has one route. The route is available when the receiver is writable
and not going away, and presence is `online` or `asleep`. It's qualified
when the device is in the table with a command list, on either lane. An
unavailable route loses its token, and the bridge remembers the last 256
revoked tokens and the reason for each: generation ended (HFX-107) or
unavailable (HFX-109).

A device has "returned" when evidence arrives after 60 s with none on that
lane, and a return triggers a battery probe unless the evidence was a
probe's own answer. It also resets the virtual device's publication backoff.
The route revision stays the same.

### Battery

The level is `(raw * 100 + 127) / 255`. The raw value is the level byte of a
passive status report, or byte 9 of a matched status-0x02 response to 07/80
with zero arguments.

How charge is read depends on the device's decoder
(`hfx_proto::devices::ChargeDecoder`). Decoders apply on the device's own
lane. Both were worked out on hardware:

- mouse-status reads the status report's charge and power bytes
- keyboard-status reads the status report's power/charge word

A queried 07/84 counts for every device except one with the keyboard-status
decoder. Any answer but 0 counts as charging, which is how OpenRazer reads
it. A 0 ends a charging reading without making it discharging (it could
still be a mouse on its mat, or a keyboard full on its cable). The
keyboard-status decoder ignores 07/84, because the tested keyboard answers 0
while full on its cable.

Only the mouse-status decoder gives mat contact. The charge is forgotten
when the level goes, when the device changes and on a new generation.

A device can lack a charge source in two ways. Either it has no decoder and
the bridge never asks it, or it answered 07/84 with "not supported" or with
an answer too short to hold a value, and either way the snapshot marks it
`unreported: charge`. A device that answered 07/80 with "not supported" and
has sent no status report is `unreported: battery`.

A level that went up at its last change is `rising` until it goes down. The
clients drop their low-battery warning while it climbs, whether or not
charging is reported.

Changes go to the keeper. It sends them as the battery device's input report
(level and charging flag), and again after `UHID_START` and `UHID_OPEN`
(section 2). Sometimes the keeper fails to make or keep a device, for
example with no `/dev/uhid` yet or `EHOSTDOWN` at boot. The bridge then asks
for it again after 1 s, 5 s and 30 s, and with the next reading. Readings
older than 30 minutes (`hfx_proto::present::BATTERY_STALE_AFTER_MS`) are
stale in the protocol, but UPower keeps showing them.

### Battery probes

A probe queries 07/80, then 07/84 unless the device's status reports decide
(above). The bridge probes:

- when a writable receiver is admitted
- after a system resume
- after a new identity
- when a device returns
- when the charge watch (below) wants it
- when an online device has gone ten minutes without a reading

For a device whose charge comes from 07/84 alone, the ten minutes also count
from its last 07/84 answer. Such a device is also asked when its wired twin
(below) comes or goes, since that means a cable went in or out.

Every tested and checked device is asked both, whatever its command list
says. The bridge's own queries are kept apart from what apps may send. A
07/80 or 07/84 with two zero bytes changes nothing. A device without the
command answers 0x05. That "not supported", or a 07/84 answer too short to
hold a value, stops the query for the device until it pairs again. A "not
supported" for 07/80 stops both queries. Either ends the charge watch.

The bridge asks when no one else holds the device's lease, and holds it as
`HyperFlux` for the length of the probe. A non-matching or busy
answer is retried after 2 s, 10 s and 60 s. A 0x04 answer stops the probes
until the next evidence. Probes never write.

Two probes of one lane start at least 2 s apart, whatever asked for them. On
the pointer lane a probe also waits for the pointer to be still for 300 ms.
That works out to 650 ms after the last activity event, because the keeper
reports at most one event every 250 ms and looks at the pointer every
100 ms while it moves.

The pause is there because a query travels over the same radio link as the
mouse's movement and could stall the cursor briefly. `hyperfluxctl
stall-check --ab` measures that. On the tested receiver it found no clear
link between queries and stalls, but the pause stays as a precaution,
because that's one receiver and one mouse.

### Charge watch

The receiver sends no report when the mouse is lifted off its charging mat
or put back. A 60 s `hyperfluxctl capture` on hardware had no passive report
at all, with the Basilisk V3 Pro 35K lifted off the HyperFlux V2 mat and put
back three times. The charge came only with the replies to an app's 07/84
query, and OpenRGB showed a change 20 s to a minute late, depending on when
an app asked. The DeathStalker V2 Pro TKL's cable changes do arrive as
status reports.

For a mouse on the pointer lane, the bridge asks on its own, with the
pointer's movement as the hint (`hfx-bridge/src/charge.rs`). That covers the
Basilisk V3 Pro 35K and the five checked mice. The watch is tested on the
35K. There, charging cleared 4.3 s after a lift while the mouse kept moving,
and came back at once on the mat.

The checks:

- Movement after 5 s without any probably means a lift. The check comes 1 s
  later, at the first pause.
- No movement for 3 s probably means a put-down. The check comes then, and
  again 15 s later, because the first status on the mat can still read not
  charging.
- An online mouse with no charge reading or check for 60 s gets one then.

A check is a battery probe, so the probe rules above apply. Those are the
lease (an app holding the device puts the check off by the probe's 2 s
retry), the match rules, the 2 s gap and the pause. Devices that aren't
online aren't asked. A pending lift or put-down check holds back the
once-a-minute one. Any 07/84 answer or status report counts as a reading,
whoever asked for it.

The simulator test (`tests/charge.rs`) moves the mouse at 1,000 reports a
second. Charging stops showing about 0.6 s after a lifted mouse stops
moving. It shows about 3 s after a put-down, or 18 s when the first status
on the mat reads not charging.

### Leases

Leases are per device. They end with release, expiry, disconnect, the end of
the generation or a device change. The internal holders follow the burst
rule: they acquire at the first report and release 25 ms after the last
matched one, or straight away after any other result.

### Bench

Each measured query follows a device mode query (00/84), sent with the
device's learned wait and not timed. An early read gets the receiver's
previous answer, and if that were the answer to an identical query, it
would match and pass for a fast one.

Like every other exchange, a measured query re-reads 1 ms after a
non-matching reply. With the `bench-interval` feature, a bench request may
carry a different interval, `get_interval_us`, from 1 to 10 ms
(`--retry-us`). The reads still end 40 ms plus the first wait after the SET.
At 1 ms that fits up to 41 reads, and at 10 ms five. The device mode query
before it keeps the usual timing, and the match rules stay as they are.

`reread_busy` in the same request (`--reread-busy`) makes the measured query
read on after a matching busy answer too, at that interval and until the
same deadline. `--defaults` sends neither. It uses the wait the bridge has
learned for the device, from the snapshot, and measures what the device's
other exchanges do. No bench exchange, measured or not, counts toward
learning.

The simulator has an answer ready after 3 ms on the pointer lane and 4 ms on
the keyboard lane, where `hyperfluxctl bench --defaults` gives medians of
3.2-3.3 ms and 4.2-4.3 ms in two runs, with one read each. With a 1 ms first
wait, the keyboard's median is 11.5 ms at 10 ms between reads, and 4.4 to
4.7 ms at 1 ms.

The simulator can also answer busy until the answer is ready. Then
`--reread-busy`, with a 1 ms first wait and 1 ms between reads, gives
medians of 3.5 ms on the pointer lane and 4.5 ms on the keyboard lane.
Without it, every exchange on the keyboard lane ends busy.

`tests/protocol.rs` checks that a read that gets the previous answer is
followed 1 ms later by one that gets the right answer. On hardware, `bench
--reread-busy --quick` set the waits and the interval (section 10, "Receiver
timing").

### Virtual OpenRazer devices

Virtual devices are on when the keeper is connected and could open
`/dev/uhid`, and a loaded `razerkbd` or `razermouse` reports the capability
parameter. The bridge checks again when the drivers register (uevent `add`,
subsystem `drivers`). Each qualified, available route of a device with an
`[openrazer]` part gets a virtual device. These rules apply to it:

- Two identical serial reads, 250 ms apart, come before UHID_CREATE2. A
  transaction id mismatch on a serial read is retryable, like any other
  query mismatch.
- A wired device of the same model suppresses the virtual device. This is
  `0003:1532:<wired pid>` bound to the entry's driver
  (`/sys/bus/hid/drivers/<driver>/`), or present in `/sys/bus/hid/devices`
  with no driver bound so far. The bridge looks on every hid uevent and
  before each serial read. A suppressed device gets no serial reads while
  such a device is there. The wired one counts as another unit when the
  bridge can read its serial hash (`serial_sha256`) and the hash differs
  from the device's own. Then the virtual device stays.
- A reply with a data size of 22 or more and an empty or unprintable serial
  counts as no serial, and after two such reads in a row, the device is
  published with a stand-in fingerprint made from the receiver id, lane and
  product id. The log says so once.
- A SET is admitted (report 0, 90 bytes, nothing pending, command check) and
  acknowledged before the exchange.
- Device mode 00/04 and 00/84 are answered locally.
- Queries are retried up to 5 times on BUSY or on a transaction, class or
  command mismatch. OpenRazer gets the answer only on status 0x02 with all
  three matching.
- Writes are sent once and returned byte for byte.
- A 0x04 reply recycles the device. For the next 60 s it is probed again at
  least every 2 s. That holds for the tested devices. For the others the
  bridge first asks 07/80 itself, and recycles when that gets 0x04 too, or
  when the lane shows no life for 10 s. Any life keeps the device.
- A route that backs off because its device answered 0x04 tries again at the
  lane's next evidence, without waiting out the backoff. The 250 ms between
  the two serial reads still holds.
- No route is blocked for good. A blocked route retries every 5 minutes.
- `UHID_STOP` (a driver hand-over) recovers up to 3 times.
- At teardown the keeper unbinds the driver on a worker thread while the
  bridge still answers GETs, then destroys the device after 5 s at most.
  When the `unbind` file is read-only, the keeper waits until no request is
  waiting for the bridge, plus 200 ms, before it destroys the device.

### Revisions

The state revision moves on any snapshot change except `now_ms`,
`last_seen_ms`, `refused` and `exchanges`. A reading turning stale counts,
and is scheduled as a timer. The route revision moves on any route change.
`wait` and `capture` park the request until their condition holds.

### Logging

The bridge logs plain lines on stderr, which journald collects. It writes
one line per event worth reading:

- a receiver admitted (with what the bridge made of its pairing inventory)
  or retired
- what named its devices
- a device changed
- route state
- a battery published or refused
- virtual device state, with reason and retry (once per reason and delay)
- a command refused for not being in the device's list, at most once a
  minute per device and command
- the keeper coming and going

The keeper logs the receivers it reads, the bridge coming and going,
teardown outcomes and what it refused. Serials, route tokens and key presses
are never logged.

## 5. Failure handling

- If the bridge crashes, systemd restarts it after 2 s. If it hangs, the
  watchdog kills it after 30 s. Clients reconnect. The plugin backs off from
  25 ms, doubling up to 2 s, and `hyperfluxctl watch` retries every 2 s. The
  keeper removes the bridge's battery and OpenRazer devices when the
  connection drops, and reconnects (section 3.8). The new bridge replaces
  the stale sockets, reopens the receiver, reads the inventory and probes.
  Batteries are back within a moment, and the OpenRazer devices after the
  serial check.
- If the keeper crashes or hangs, the same 2 s restart and 30 s watchdog
  apply. Its devices die with it (uhid closes them). In between, the bridge
  goes on serving apps but offers no OpenRazer devices. Passive reports and
  activity meanwhile are lost. Once the keeper is back, the bridge makes the
  batteries and OpenRazer devices again. Doctor reports HFX-208 while the
  keeper is down.
- Unplugging the receiver removes its hidraw nodes and retires the
  generation. Routes, leases, battery devices and virtual devices go.
- A replug means a new generation, full rediscovery and battery probes.
- Runtime autosuspend changes nothing for clients. The next exchange wakes
  the receiver.
- After a system suspend, the bridge spots the resume from the clock gap.
  Presence goes to `waiting`, tokens are revoked and probes go out. Routes
  return with the first evidence. Batteries stay in UPower throughout.
- A malformed event is logged once per kind and dropped.
- A receiver module from one of the DKMS packages Core replaces may still be
  loaded. It holds the receiver. There are no hidraw nodes. Doctor reports
  HFX-303 and HFX-202, and a reboot loads the ordinary HID drivers.
- When an exchange fails, the outcome depends on the errno class, and a
  request that "may have been sent" is never resent, by the bridge, the CLI
  or the plugin.
- A refused battery publish is retried after 1 s, 5 s and 30 s, and with the
  next reading. Doctor reports HFX-205 for a reading with no power supply
  entry, or one without a level.
- Virtual device problems back off per route (100 ms five times, then 1, 2,
  4, 8, 16 and 30 s), though when the device gave no answer, the next
  evidence cuts the backoff short. A blocked route retries every 5 minutes.
  Other routes are unaffected.
- Without `/dev/uhid` there are no battery entries and no OpenRazer devices.
  The keeper says so in its hello, the bridge logs it once, and Doctor
  reports HFX-408.
- A misbehaving client is disconnected. Limits protect the other clients: 32
  connections, and 10 s timeouts for hello and for frames.
- If the per-user socket is missing, `hyperfluxctl` falls back to the system
  socket, and if that one refuses too, it reports HFX-204 with the fix.

## 6. Performance and size

Idle CPU should be about zero. At rest the daemons wake for watchdog pings,
at most one charge check a minute per online mouse, and one battery probe
per device every ten quiet minutes. `scripts/idle-check.sh` counts the
bridge's and the keeper's wake-ups and the simulator's queries over 60 s. It
allows one charge check (07/80 and 07/84) a minute at most, and at most 2
wake-ups per ping, 8 per query and 10 more.

- Simulator, with the script's 10 s watchdog: 35 wake-ups of the two in
  60 s, for 24 pings (12 each) and one charge check of 2 queries.
- Simulator, with the movement at 1,000 reports a second on interface 0
  (`tests/charge.rs`): 55 wake-ups of the two event loops in 4 s. About 40
  of them were the keeper's.
- Test machine, with the mouse moving: the keeper woke 11 times a second and
  the bridge 6, at about 0.1 % of a core each.

The bridge's and the keeper's RSS should stay under 10 MiB each with one
receiver. `scripts/rss-check.sh` checks the bridge, keeper and simulator in
CI, and the simulated soak checks it too. On the simulator: 3.0 MiB each. In
a 10-minute soak of the debug builds, the bridge went from 5,400 to 6,012 kB
and the keeper from 4,620 to 4,720 kB.

The stripped binaries of the bridge, keeper and `hyperfluxctl` should be
under 1.5 MiB each. A CI size check runs on the release build
(`opt-level="s"`, fat LTO, `panic="abort"`, stripped), and as
`scripts/package.sh arch` packages them, they come to 844 KiB, 618 KiB and
999 KiB.

The project's own code should stay under 40,000 non-blank lines. That counts
Rust, C++, Python, shell, make, CMake and completions, tests included.
Patches, fixtures, vendored files and Markdown are excluded.
`scripts/loc-check.sh` checks it in CI, per part, against the budgets below.

Dependencies are few. The Rust code uses serde, serde_json, rustix, libc,
sha2, getrandom and signal-hook, plus proptest and tempfile for tests. No
async runtime. The C++ code uses Qt and OpenRGB's own nlohmann json. The
Python SDK: standard library only. A new dependency needs a line in the root
`Cargo.toml` saying why.

A report exchange should take about 4-10 ms: the device's first-GET wait
plus the transfers. OpenRGB needs one round trip per report, and OpenRazer
one socket hop each way. `hyperfluxctl bench` on hardware set the per-device
waits and the 1 ms re-reads, and `bench --defaults` checks them. On
hardware, a firmware query takes 4.2 ms for the Basilisk V3 Pro 35K at 3 ms,
and 5.2 ms for the DeathStalker V2 Pro TKL at 4 ms. On the simulator,
OpenRazer's 07/80 to the mouse through the keeper has a median of 4.04 ms.
About 0.3 ms of that is the socket hop to the keeper and back.

Batteries should be visible within seconds of boot or login. The battery
probes take care of that, and a simulator test checks it. On the simulator
the readings arrive a few ms after the receiver is admitted, and reach the
battery device 50 ms after it starts. On hardware, at a bridge restart, the
journal had the mouse's level 4.4 ms after admission (section 2).

Mouse charging should follow the mat within seconds. The charge watch does
this, checked by `tests/charge.rs`. On the simulator, charging clears 0.58 s
after a lifted mouse stops moving and shows again 2.9 s after a put-down, or
17.9 s when the first status on the mat reads not charging. On hardware it
cleared 4.3 s after a lift while the mouse kept moving, and came back at
once on the mat.

Line budgets per part:

| Part | Budget |
| --- | --- |
| hfx-bridge | 16,000 |
| hyperfluxctl | 9,000 |
| OpenRGB plugin + C++ SDK | 6,000 |
| hfx-proto | 5,000 |
| Python SDK | 600 |
| scripts and packaging | 2,000 |
| total | 40,000 |

`scripts/loc-check.sh` counts the lines in CI. The run fails when a part
goes over its budget, or the whole tree goes over the total. The total also
covers the cross-check tests and the integration scripts.

## 7. Privilege model

Two unprivileged services split what the receiver offers, and the one apps
talk to holds neither the keystrokes nor the power to make input devices.
The bridge runs as `hyperflux-next` and the device keeper as
`hyperflux-keeper`. Core's udev rule gives them this access:

| Node | Bridge | Keeper |
| --- | --- | --- |
| hidraw interface 0 | read and write, as the node's owner | read, as its group |
| hidraw interface 1 | nothing | read |
| hidraw interface 4 | read and write | nothing |
| `/dev/uhid` | nothing | read and write |
| the four OpenRazer `unbind` files (Full) | nothing | write |

Interfaces 2 (the keyboard's own input) and 3 stay root-only. Interfaces 3
and 4 are HID LampArrays, which no person should be able to open. Doctor
warns when someone can (HFX-410, and section 10, "Lighting interfaces").
Both units' device policies allow hidraw nodes, the keeper's for reading
alone. `/dev/uhid` is in the keeper's policy and not in the bridge's.

A node has one group, and interface 0 needs a writer and a second reader:
the rule makes the bridge's user the node's owner and the keeper's group its
group, with mode 0640. An ACL from the rule would do the same, but `setfacl`
isn't installed everywhere. Arch installs it by default, because systemd
depends on it, but the other four distributions' images in the install tests
lack it. On top of that, udev's `uaccess` builtin clears named users from
the ACL of a node another package tags.

`OWNER=` needs nothing but udev, and `udevadm verify` in the install tests
resolves it on all five. As the owner, the bridge could change the node's
mode, but that gives it nothing new, since it may already read and write
the node.

What the bridge can do:

- send any feature report to the receiver, since the command check lives in
  the same process
- read the mouse's movement on interface 0, which it holds open to write. It
  never reads it, but nothing would stop a bridge someone had taken over.
- ask the keeper for virtual devices, picked from the two templates and the
  catalog. That means an OpenRazer device for a product OpenRazer drives,
  which the patched drivers bind, and battery devices that never send
  anything but their battery report.
- answer the patched drivers' GETs for its OpenRazer devices however it
  likes

Without interface 1, the bridge never sees what was typed, and it can't make
a device that types or moves the pointer, or unbind a driver.

What the keeper can do:

- read the receiver's input on interfaces 0 and 1, a paired keyboard's
  keystrokes included. It passes on the vendor reports and the fact that a
  key or the mouse moved, nothing else. It neither keeps nor logs input.
- create virtual HID devices through `/dev/uhid`, including ones that type,
  if someone takes it over
- with Full, unbind the OpenRazer drivers from devices. A wired Razer
  keyboard would then lose its driver until replugged.

The keeper can't send anything to the receiver. It may read interface 0 but
not write to it, and has no access to interface 4. It has no socket of its
own. It connects to the bridge and acts on the fixed messages of section
3.8, nothing more. It's in the `hyperflux` group to reach `keeper.sock`.
That also lets it connect to the bridge's client socket, as any app can.

Neither daemon runs as root, and there's no kernel module. The process apps
talk to holds the smallest part. These limit both:

- The systemd sandbox. No capabilities, and `NoNewPrivileges`.
  `DevicePolicy=closed`, allowing hidraw and, for the keeper, `/dev/uhid`. A
  read-only file system apart from the bridge's runtime and state
  directories and the keeper's four `unbind` files. `AF_UNIX` and
  `AF_NETLINK` sockets and no IP traffic. `@system-service` system calls
  without the privileged and resource ones. A `/proc` showing its own
  process alone.
- The keeper creates two kinds of device and no others: the 19-byte
  feature-only descriptor for OpenRazer, and battery devices whose mouse or
  keyboard collection never sends a report. It refuses to write `unbind` for
  a device whose `HID_PHYS` isn't one of its own.
- The `hyperflux-keeper` group has no human members. The OpenRazer patch's
  trust check relies on that.

A bridge that someone has taken over can still do the four things listed for
it. A taken-over keeper can do its three. Either can also read what every
user may read outside `/home` and `/proc`. Either can connect to Unix
sockets that let every user in, such as the D-Bus system bus.

Neither can:

- open a network connection
- see other processes
- write anywhere but its own paths above
- open a device that isn't a hidraw node (or, for the keeper, `/dev/uhid`)
- open a hidraw node its user and groups have no access to, such as the
  receiver's interfaces 2 and 3
- load kernel modules
- change its user or groups
- gain privileges through a setuid program
- change its scheduling or resource limits
- map memory that is both writable and executable

`PrivateDevices=`, `PrivateNetwork=` and `PrivateUsers=` stay off. The first
would take away hidraw and `/dev/uhid`. The second would take the netlink
uevents, and the third the groups that grant access to them.

hidraw allows several openers. The bridge stays the sole writer as long as
nobody else can open interface 0 for writing. Root can. So could a user whom
another package's udev rules gave an ACL on the receiver's hidraw nodes. The
bridge skips replies to someone else's requests as non-matching, but their
writes still reach the device.

### The uaccess caveat

The OpenRazer trust check fails open when someone else can write
`/dev/uhid`. A `uaccess` tag on uhid makes logind add an ACL for the
logged-in user (Sunshine's `60-sunshine.rules` has one, for its virtual
input), and that user's programs can then create a device with HyperFlux
Next's phys prefix. The patched drivers would bind it.

HyperFlux Next V2 leaves the tag alone, because Sunshine needs it. With Full
installed, Doctor reports the ACL as HFX-409. The keeper's own access comes
from its group, not from an ACL, so a user named in the ACL is always
someone else. Without Full, nothing trusts uhid devices and the ACL is
irrelevant.

Clients need none of these rights. They use the protocol sockets.

## 8. Rules that came from real bugs

Each of these rules came from a real bug, and stays.

In the bridge's exchanges:

- Match the full transaction id byte and skip replies to other requests.
  Never replay an old battery reading.
- A query reply counts only with status 0x02 and a matching transaction id,
  class and command. This applies to the virtual devices and to battery
  harvest.
- Never resend anything that may have been sent. This holds for the CLI and
  the plugin as well.
- Ack the SET before the exchange. The event loop is poll-driven and never
  sleeps. The bridge and the plugin hold one lease per burst.

For batteries and charge:

- Take batteries from app replies too, not just passive reports, and retry a
  refused publish.
- Publish what the device says. Report full only when charging at 100 %. A
  queried 0 doesn't mean discharging, and any other answer means charging.
  Status values never seen before are unknown.
- Each device gets a uhid battery in the keeper, with a mouse or keyboard
  collection and the device's name, which puts batteries under the right
  device type and name.

For virtual devices:

- Full loads the OpenRazer modules at boot (modules-load.d), so hid-generic
  never binds the virtual device first. A `UHID_STOP` hand-over is retryable
  in the bridge.
- The keeper unbinds before it destroys, while the bridge still answers.
- A wired twin of the model hides the virtual device. A readable serial that
  differs is the one thing that keeps it.
- The bridge and keeper units, as device providers, stop after user sessions
  at shutdown.

In the protocol and `hyperfluxctl`:

- An unsupported protocol version gets HFX-101 listing the served versions,
  and the bridge keeps the connection.
- Doctor names the failing layer (receiver access, service, receiver,
  device, integration) and reads the driver actually bound.
- `hyperfluxctl` detects a retired driver.
- Doctor works for a normal user, trying the per-user socket first. Each CLI
  run has its own client id.

## 9. Build and test

```
cargo build --workspace          # debug build of all three crates
cargo test --workspace           # unit, integration and property tests
make lint                        # rustfmt --check and clippy -D warnings
make test                        # cargo tests, python tests and the fragments' tests
make openrazer-tree openrazer-test
make openrgb-plugin OPENRGB_QT=6
python3 -m pytest tests          # cross-checks: device tables, packaging, versions
```

Run the stack without hardware:

```
cargo run -p hfx-bridge --bin hyperflux-next-sim -- --socket /tmp/hfx-sim.sock &
cargo run -p hfx-bridge --bin hyperflux-next-bridge -- --sim /tmp/hfx-sim.sock --socket /tmp/hfx.sock &
cargo run -p hfx-bridge --bin hyperflux-next-keeper -- --sim /tmp/hfx-sim.sock --socket /tmp/keeper.sock &
HYPERFLUX_NEXT_SOCKET=/tmp/hfx.sock cargo run -p hyperfluxctl -- devices
```

The bridge listens for the keeper at `keeper.sock`, next to its own socket.
With `--sim`, the keeper keeps the battery devices in-process, and nothing
reaches the real `/dev/uhid`. OpenRazer devices stay in-process too. The
bridge asks for them when the simulator offers a sysfs tree with OpenRazer's
files (`Sim::set_sysfs`), and not otherwise. The simulator then passes that
tree's uevents, and the system's suspends, over the bridge's watch
connection. The soak test runs this way.

The integration tests run the bridge and the keeper on threads of the test,
each with its own sockets, as they would run as processes. The simulator
reads commands from stdin, and `help` lists them. There are status reports,
the mouse's movement at a given rate, sleep, pairing, busy and silent
devices, unplug, plug and a system suspend.

`Cargo.lock` is committed, and CI builds with `--locked`. There are six
workflows:

- `ci.yml` runs on pushes and pull requests.
- `release.yml` runs on version tags.
- `weekly.yml` runs every Monday, to catch new kernels, distribution images
  and OpenRazer releases early.
- `codeql.yml` runs CodeQL over the Rust, C++, Python and workflow code on
  pushes, pull requests and every Monday. It builds nothing.
- `dependency-review.yml` checks what a pull request adds to `Cargo.lock`
  and the workflows.
- `dependabot-merge.yml` merges Dependabot's updates into `dev` once their
  checks pass, except major versions (`docs/maintainer/release.md`).

CodeQL and dependency review need a public repository. A private copy skips
them.

### Hardware runs

I test with one HyperFlux V2 receiver on an Arch machine, with a Basilisk V3
Pro 35K and a DeathStalker V2 Pro TKL paired to it. On those two I've
checked:

- both batteries on the desktop, as a mouse and a keyboard, and charging on
  the mouse's mat, lifted off it, and on either device's cable
- Polychromatic with Ripple, and the keyboard's key matrix
- OpenRGB colour changes on both devices, and the battery page, in the dark
  theme too
- a cable going in and out on the mouse and on the switched-off keyboard,
  with the receiver copy leaving and coming back
- `serial_sha256` readable on both wired devices
- replug, suspend and resume, and a reboot after installing the packages
- the devices named by the paired slots, and `hyperfluxctl probe receiver`
- `bench --defaults --iterations 200`, answered 200 of 200 on both
- the learned waits picked up again after a restart
- the bridge and the keeper as two services with the owners and groups of
  section 7
- Doctor reporting ready, with HFX-409 as its one finding
- wake-ups at rest and with the mouse moving (section 6)

Not tested: a second receiver, a second unit of one model on a cable, and
the checked devices.

## 10. Decisions

### Kernel and processes

The receiver keeps the kernel's own HID drivers. The bridge reaches it
through hidraw, which does all the bridge needs, and no kernel module needs
building, signing or keeping in step with the kernel. Core needs nothing
beyond hidraw, uhid and HID battery support. Batteries are uhid devices, one
per paired device. That way hid-input creates the power supply, and UPower
types it by the input device next to it.

There are two daemons, and neither runs as root. The keeper holds the
receiver's input and `/dev/uhid`. A fault in the bridge, which apps talk to,
can't expose what was typed or make input devices (section 7). The bridge
listens and the keeper connects, because systemd removes the bridge's
runtime directory when the bridge stops. Core's udev rule sets the
permissions, not an ACL or a helper. That leaves Doctor with owners, groups
and modes to check.

The keeper reads the pointer's movement from interface 0. The receiver sends
it there as a boot mouse's reports, about 900 a second, and none of it comes
out of interface 1. Reading that node in bursts loses nothing anyone needs,
as each hidraw reader has its own buffer and the desktop's input takes the
kernel's input path.

Both daemons are event-driven: epoll, timers from deadlines, no polling
tick. The code is three crates with no code generators or build scripts. One
Python test checks each language's hand-written device table against
`devices.toml`.

### Receiver timing

The bridge runs exchanges on one thread per receiver, so the event loop
never sleeps on a transfer. The reads after a SET stop 40 ms after the
first, which is how long five reads 10 ms apart take. From the hidraw SET
on, a failed request may have been sent, and nothing resends it.

The first-GET wait is 3 ms for the Basilisk V3 Pro 35K and 4 ms for the
DeathStalker V2 Pro TKL. Those are the shortest waits at which `bench`
always had the answer at the first read. A query then takes about 4.2 ms on
the mouse and 5.2 ms on the keyboard, against 9.4 and 9.3 ms with an 8 ms
wait. Devices without bench runs get 6 ms.

A non-matching reply is read again after 1 ms rather than 10. An early read
gets the receiver's previous answer, not a busy one. The bridge reads past
busy answers too. At the catalog waits, about 5 % of the mouse's answers and
17 % of the keyboard's were busy at first.

The bridge learns each device's wait, starting from the catalog's, because
bench measured one unit each of two models. A request identical to the one
before never waits less than the catalog's value, since an early read
would take the previous answer for its own. Learned waits survive restarts
because real traffic answers more slowly than bench's query. After a couple
of hours' use they had gone from 3 to 5.0 ms on the mouse, and from 4 to
5.5 ms on the keyboard.

The snapshot counts each device's exchanges and re-reads. A report from
another unit then shows whether these rules fit it.

### Device identification

The receiver's paired slots (00:bf) name the devices. A slot keeps its
product id while the device is off or on its cable. The pairing inventory is
the fallback. It loses the mouse's id on its cable, and the bytes it's read
by lie past the 22 bytes the receiver declares for that report.

The bridge reads the inventory by its structure rather than matching it
against a stored copy, which would accept the tested unit and no other, and
would put bytes in the tree that may identify it. Queries that
keep their own transaction id, like 00:bf, take one fixed path
(`devices::RECEIVER_PROBES`). Neither `transact` nor OpenRazer can use it.

A new product id on a lane replaces that device alone, so re-pairing needs
no replug. A status report from a lane without an identity gets a stand-in.
A device connected before the bridge started still shows its battery that
way. Input alone makes no stand-in, since a mouse's macro keys arrive as
keyboard input.

### Device catalog

Every device that OpenRazer 3.12.4 or OpenRGB 1.1 can drive gets a route.
The tier says what's known: `tested` through the receiver, `checked` against
Razer's list and the upstream drivers, or `listed` when neither drives it.
Razer's side of the check is a dated `[sources]` snapshot, because Razer's
support pages answer scripts with a captcha.

The command check is class, id and data size per device. Argument values are
left to the firmware. That keeps the attack surface to what OpenRazer and
OpenRGB send, without a table of hundreds of rules. A refused command is
logged and counted in `refused`, so a missing list entry doesn't look like a
broken device.

### OpenRazer devices

The receiver copy of a device goes as soon as the kernel announces its wired
twin, about 2 s ahead of openrazer-daemon, because on the tested mouse the
daemon didn't finish setting up the wired device while the copy was still
there.
The slots can't tell the paired unit from a second one. That's why a
differing `serial_sha256` is the one thing that keeps the copy.

The patched drivers publish `serial_sha256` (patch 0003). The bridge has no
access to `device_serial`, and no need for the serial itself. Serials are
short, so the hash isn't strong secrecy. Patch 0002 makes the drivers ask
`hid_is_usb()` before treating a device as USB, since uhid lets whoever can
write `/dev/uhid` pick the bus. It's a separate patch so it can go upstream
on its own.

For devices outside the tested tier, a 0x04 answer has to be confirmed
before the virtual device is recycled. Recycling makes Polychromatic lose
the device. Another dongle is known to answer 0x04 on mere timeouts
(OpenRazer #2469).

Ripple's key-press links are per receiver, so two receivers don't fight over
one link. OpenRazer attaches every matching link to a keyboard, though. With
two receivers, each keyboard also sees the other's keys.

### Batteries and charging

The battery device has vendor 0. Every virtual Razer device appears in the
patched drivers' tables, and hid-generic won't bind a device that another
driver lists. With a Razer id the battery would get no driver.

The bridge takes readings from every matched 07/80 and 07/84 reply, whoever
asked. After ten quiet minutes it asks an online device itself. The
30-minute stale limit is then independent of how often a device reports. Its
own queries go to every tested and checked device, whatever the command list
says, because the lists hold what OpenRazer and OpenRGB send, and OpenRGB
never asks for a battery.

Like OpenRazer, the bridge reads any nonzero 07/84 answer as charging. The
Basilisk V3 Pro 35K gives 6 on its mat, 0 when lifted and 1 on its cable.
Mat contact comes from the mouse-status decoder alone, since the power
byte's meaning on other mice is unknown.

The receiver doesn't report the mat. A timer that followed it within seconds
would have to ask every few seconds, and the idle target rules that out.
Instead, the charge watch asks when the pointer starts or stops moving.

### Lighting interfaces

The receiver's interfaces 3 and 4 are HID LampArrays, and they stay closed
to people. Their layouts don't match the paired devices: interface 3
describes a full-size keyboard with a numpad. Lighting stays on the Razer
commands, whose layouts are checked against OpenRazer and OpenRGB. OpenRGB's
generic LampArray controller would also show each device twice and fight the
Razer controller over the lights.

A hid-lamparray helper posted for the kernel would bind both interfaces and
blank the lamps. Once it ships, the plan is a udev rule that writes 0 to its
`use_leds_uapi` attribute on interfaces 3 and 4.

### Protocol and tools

Protocol 1 has framing, hello, leases, routes and transact, and nothing for
features the bridge lacks. The connection is the session, so there are no
session credentials. Hello keeps version negotiation for later versions. An
unknown method gets HFX-102 without closing the connection. Newer clients
can then probe older bridges. Leases last at most 60 s, which limits stuck
locks.

The bridge has no configuration or status file. It takes two switches,
`--read-only` and `--no-openrazer`. It logs virtual-device states and leaves
Doctor to check what it can see. Doctor exits with 2 for any finding,
warnings included, so scripts see them.

### Packaging

Numbering starts at product 1.0.0 and protocol 1. Error codes go by layer:
`HFX-1xx` wire, `2xx` service, `3xx` driver, `4xx` integrations. New package
names need no epoch. Each package takes over the names listed in section 1.
When the RPM Core package replaces `hyperflux-next-linux`, it keeps the
bridge enabled, because that package's `%preun` disables it.

The OpenRGB plugin has its own package. Full depends on it where the
distribution's OpenRGB loads it, and recommends it instead where the plugin
needs OpenRGB 1.1 from openrgb.org. Nothing pulls in OpenRGB itself. The
SDKs ship as source only, and the simulator isn't packaged.

The code is GPL-2.0-only with SPDX headers. The one third-party notice,
nlohmann json (MIT), ships with the plugin.

### Tests

A simulator test pairs every catalog device in turn, and that way the paths
of devices outside the tested tier run in CI too. A chaos test runs
thousands of exchanges under a fixed seed, with random answer times, busy
answers and silences. No stale answer may get through.

The weekly soak runs both daemons under random events and bounds their
threads, open files and memory. Anonymous memory may grow by less than 1 MiB
an hour, an allowance that's there because glibc keeps some freed memory,
while the live heap stays under 300 kB.
