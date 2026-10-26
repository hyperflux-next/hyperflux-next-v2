# Privacy

HyperFlux Next V2 works on your computer alone. Its two services have no
network access. The files `hyperfluxctl` writes for bug reports stay
where they are until you share them.

This page covers what is read from the receiver, what is kept, what
those files hold, and who else on the computer can see what. The full
privilege model is in
[ARCHITECTURE.md](../../ARCHITECTURE.md#7-privilege-model).

## What the device keeper reads

The device keeper, `hyperflux-next-keeper`, runs as a system user of its
own, `hyperflux-keeper`. Of the two services, it is the one that reads
the receiver's input:

- Interface 1 carries the receiver keyboard's key reports, with the
  receiver's vendor reports among them. The keeper passes each vendor
  report to the bridge as its first 16 bytes and its length. These are
  identity and status reports, which the bridge turns into presence and
  battery readings, plus two other kinds that the bridge just records for
  `capture`. A key report becomes an activity event and nothing more:
  the receiver, the lane and the time. There is at most one such event
  every 250 ms, however fast you type. Anything else on interface 1 is
  dropped.
- Interface 0 carries the mouse's movement. The keeper empties it in
  batches. It sends the same kind of activity event, at most one every
  250 ms, without the movement.

The keeper doesn't keep or log any of it. The test
`keys_stay_in_keeper` in
[crates/hfx-bridge/tests/keeper.rs](../../crates/hfx-bridge/tests/keeper.rs)
feeds key reports into a simulated receiver and checks that:

- nothing but activity events and vendor reports comes out
- there is no more than one activity event per 250 ms
- none of them holds a key code

## What the bridge reads and sends

The bridge, `hyperflux-next-bridge`, runs as the system user
`hyperflux-next`. It has no access to interface 1, so it never sees what
you type. It uses interface 0 for feature reports (the receiver's
request-and-answer channel) and reads the list of paired devices on
interface 4.

The bridge's own queries ask the receiver which devices are paired,
their battery level and whether they are charging. With the patched
OpenRazer drivers loaded, it also reads a device's serial number twice
before it makes the device's copy for OpenRazer, but it keeps a hash of
the serial, not the serial itself.

Everything else comes from apps (OpenRazer, OpenRGB, `hyperfluxctl`).
That means commands in the device's command list, and the fixed
read-only queries of `bench` and `probe receiver`, which run when you ask
and at no other time.

With Full, OpenRazer's lighting also reaches the devices through the
keeper and the bridge. For Ripple and other effects that light the keys
you press, that lighting follows your typing, and the bridge passes it on
and keeps none of it. `capture` records answers to queries, never
writes.

Neither service can open a network connection. Both units allow Unix and
netlink sockets and nothing else
(`RestrictAddressFamilies=AF_UNIX AF_NETLINK`). The netlink ones are for
the kernel's device events. Both also block all IP traffic
(`IPAddressDeny=any`). The proxy behind your own socket is limited to
Unix sockets.

## What is kept

One file, `/var/lib/hyperflux-next/learned-waits.json`, records how long
each device takes to answer, and with it the bridge doesn't have to learn
the waits again after a restart. Each entry has:

- the receiver id
- the lane and the device's product id
- the firmware version the device reported, once the bridge has seen it
- the wait, in microseconds
- when the wait was last used (one unused for 30 days can be dropped)

The directory belongs to the bridge's user with mode 0700, and the file
has mode 0600. That user and root are the only ones who can read it.
[Removing HyperFlux Next V2](updates-removal.md#removing-hyperflux-next-v2)
says how to delete it.

A receiver id is `receiver-` followed by 24 hex digits. It is a hash of
the USB port the receiver is in and the receiver's USB ids, which means
any HyperFlux V2 in that port gets the same id and the same receiver in
another port gets a different one. The id tells ports apart, not
receivers.

Apart from the journal, nothing else outlasts the services:

- `/run/hyperflux-next` holds the bridge's socket, the keeper's socket
  and an empty lock file. systemd removes the directory when the bridge
  stops.
- Your own socket, `/run/user/<uid>/hyperflux-next-bridge.sock`, goes
  when its unit stops.
- The bridge keeps its last 1024 receiver events and query answers in
  memory, for `capture`.

Both services log to the system journal. They record:

- receivers coming and going
- which models are paired on which lane
- battery levels and charging
- the state of the OpenRazer copies
- refused commands, by class, id and size
- the keeper connecting

The lines name receivers by id. They never contain serial numbers, the
tokens apps use for a device, or key presses.

## Files for bug reports

`hyperfluxctl support-bundle`, `capture` and `report` each write one
file, in the current directory or wherever `--output` points. The file
is readable by you alone (mode 0600), and an existing file is never
replaced. Nothing is sent anywhere. Whether to share a file, and with
whom, is your decision.

The same rules hold for all three:

- They contain no serial numbers, and nothing that says what you typed
  or where the pointer went.
- Receiver ids become `receiver-1`, `receiver-2` and so on. A receiver
  keeps its number throughout the file.
- Input link names are left out, because they can contain a serial
  number.
- Doctor's suggested commands use `$USER` for your login name and
  `$(id -u)` for your user id. So does its finding when your socket is
  missing.

What each one holds:

- `support-bundle` has the HyperFlux Next V2 version, the kernel, the
  distribution and the Secure Boot state. It lists the receiver's hidraw
  nodes with their owner, group and mode, and the driver, bridge and Full
  state. It also has Doctor's findings and suggested commands, and each
  device's model, presence, battery and the app controlling it. There
  are no receiver reports, device answers or journal text in it.
  `support-bundle --preview` prints what a bundle holds and writes
  nothing.
- `capture` starts with the bridge's snapshot. Then it has everything the
  bridge records for 60 seconds (or as long as `--seconds` says), which
  covers activity events, vendor reports and the answers to queries from
  apps and the bridge, apart from serial number answers. It also has the
  first 64 bytes of the receiver's list of paired devices, with the
  product ids in it. The rest of that list, which could tell one receiver
  from another, is left out.
- `report` has what `devices`, a guided capture, `bench --defaults`,
  Doctor and `probe receiver` print. From this boot's journal it adds the
  bridge and keeper lines about OpenRazer, refused commands, serial
  reads, a device on its cable and the keeper.

## Who on the computer can see what

- Your socket has mode 0600. Only your account can use it, but any
  program you run can. Through it, a program sees what
  `hyperfluxctl devices` shows. It can also read the bridge's recent
  events as `capture` does, including when a key or the mouse was used.
  It can't see which key, or where the pointer went. The bridge has one
  view of the receiver, and every user with a socket of their own sees
  the same.
- The system socket in `/run/hyperflux-next` is for the `hyperflux`
  group. Its members are the bridge, the keeper and the proxies behind
  each user's socket. Nobody needs to join it. A person you add could
  use the bridge without a socket of their own.
- With Full, a udev rule gives the receiver keyboard's input node the
  extra names OpenRazer looks for to drive Ripple. It also gives the
  user logged in at the computer access to that node. Any program that
  user runs can then read the receiver keyboard's key presses, as
  OpenRazer does. Other accounts can't.
  [Known limitations](limitations.md#who-can-read-your-key-presses)
  shows how to turn the links off. The bridge and the keeper don't use
  them.
- To the kernel, the battery entries are ordinary batteries. Every
  account can see their level and charging, as with any other battery.
  Their phys and uniq strings carry the receiver id's hex digits.
- The patched OpenRazer drivers give each USB device that `razerkbd` and
  `razermouse` drive a `serial_sha256` file. It holds the SHA-256 of a
  fixed label and the device's serial number. Any account can read it,
  while OpenRazer's `device_serial` stays readable by OpenRazer's group
  alone. The bridge reads the hash to tell whether a device on its cable
  is the one paired with the receiver or a second unit. The hash keeps
  the serial out of plain sight, but it isn't strong secrecy. Razer's
  serials are short and follow a pattern. Someone with the hash could
  try serials until one matched.
