# Changelog

Each section is also the release's notes on GitHub.

## 1.0.1

Nothing in the packages changed. This release adds a way to install them
with one line.

### Added

- `install.sh` on the package site, which runs steps 1 to 3 of the install
  guide for your distribution:

  ```sh
  curl -fsSL https://hyperflux-next.github.io/hyperflux-next-v2/install.sh | sh
  ```

  The key it downloads has to match the hash written in the script, or it
  stops before changing anything. Your package manager still lists what
  it will install and asks first. For Core, end the line with
  `sh -s -- --core`.

### Signing key

Same key as 1.0.0:

- primary: `2068 74CA DD1B 64B3 2483  E2A8 5FC5 921D E97A F7AA`
- signing subkey: `69F3 5BC0 4765 FB65 A86C  D4C9 3E3C 3891 CB26 21D9`

## 1.0.0

HyperFlux Next V2 starts its own version numbers, error codes and client
protocol.

The packages have new names and replace the `hyperflux-next-*` packages of
2.x. The package repository has a new address as well, and
[moving from 2.x](docs/user/moving-from-2x.md) explains the switch.

There are packages for Arch Linux, Ubuntu 26.10, Debian 14, Fedora 44 and
openSUSE Tumbleweed, all for x86_64:

- `hyperflux-next-v2-linux` (Core)
- `hyperflux-next-v2-openrazer-driver-dkms`
- `hyperflux-next-v2-full` (Full)
- `hyperflux-next-v2-openrgb-plugin`, everywhere but openSUSE

### Changed

- Drop the receiver kernel module. Core needs no kernel headers and no
  Secure Boot key.
- The bridge reaches the receiver through hidraw, and batteries show
  through `/dev/uhid`.
- Core replaces `hyperflux-next-dkms`, and switching removes the 2.x
  module.
- Replace the root relay with two unprivileged services, each with a user
  of its own:
  - the bridge talks to the receiver and the apps
  - the device keeper, a small service the bridge starts, reads the
    receiver's input and holds the battery entries and the virtual
    devices OpenRazer drives
- Key presses never leave the keeper. The bridge learns only that a key
  was used, and a fault in the app-facing bridge can't expose what was
  typed or create input devices.
- Neither service polls. Each sleeps until the receiver, an app or a timer
  needs it.
- Both sandboxes allow no IP traffic, no view of other processes and no
  privileged system calls.
- Shorter waits for answers, down from 8 ms for the first read and 10 ms
  for the next:
  - first read 3 ms after sending to the Basilisk V3 Pro 35K
  - 4 ms for the DeathStalker V2 Pro TKL, 6 ms for the other supported
    devices
  - the next read after 1 ms
  - a busy answer no longer ends the exchange: the bridge reads on until
    the real one comes
  - on the mouse, a query takes about 4.2 ms in `hyperfluxctl bench` (was
    9.4 ms)
  - on the keyboard, about 5.2 ms (was 9.3 ms)
- Learn each device's own wait from these values, between 2 and 8 ms, show
  it in `hyperfluxctl devices` and keep it in `/var/lib/hyperflux-next` for
  the next start, while device and firmware stay the same.
- Ask every tested and checked device for its battery when the bridge
  starts and after resume, and again after ten minutes without a reading.
- A device that answers that it cannot tell is not asked again until it
  pairs again.
- Ask a mouse whether it is charging when it is picked up or put down:
  - needed because the receiver does not report the mat
  - the keeper tells the bridge when the mouse moves
  - on the Basilisk V3 Pro 35K, charging cleared about four seconds after
    a lift
  - charging came back at once on the mat
  - checked mice get the same checks (not confirmed through the receiver)
- Ask every checked device whether it charges (not confirmed through the
  receiver).
- Only the Basilisk V3 Pro 35K says whether it is on its mat.
- Keep an old battery reading in the widget, with its age, instead of
  dropping it.
- No low-battery warning in `hyperfluxctl battery --waybar` or on the
  OpenRGB page while the level rises.
- Step aside for a device that is plugged in by cable:
  - watch for a wired device with the model's cable id bound to its
    OpenRazer driver
  - while it is bound, remove the OpenRazer copy made through the receiver
  - remove it as soon as the kernel announces the wired device, before
    OpenRazer's daemon gets to it
  - make the copy again when the cable goes
  - tell a second unit apart by the serial hash the patched drivers publish
  - go by the id alone when there is no hash
  - stop asking the receiver for the serial number while the cable is in
- A device with no usable serial number still reaches OpenRazer.
- Ask the receiver which devices it has paired, the way other Razer
  receivers are asked:
  - without an answer, read its list of paired devices by the marker
    before their product ids
  - unlike the list, the answer keeps a mouse's id while it is on its
    cable
  - `hyperfluxctl devices` says which of the two named the devices
  - if neither did, a battery shows as an unidentified pointer or keyboard
    device until the receiver names it
- Log a failed OpenRazer device once per reason and delay, not at every
  retry.
- Offer apps all twelve devices that OpenRazer or OpenRGB support.
- The ten not tested through the receiver are labelled
  `not yet tested through the receiver`.
- Use `plugdev` as the OpenRazer group on Ubuntu, Debian, Fedora and
  openSUSE, as their OpenRazer expects. Arch uses `openrazer`.
- The client protocol is now version 1, and apps written for the 2.x
  protocol need an update.
- The OpenRGB plugin comes updated.
- The patched OpenRazer drivers build on Linux 6.1, 6.6, 6.12 and the latest
  release.
- The OpenRGB plugin has a package of its own:
  - built for OpenRGB 1.1 with Qt 6 and for OpenRGB 1.0 with Qt 5
  - Full brings it on Arch and Debian, whose own OpenRGB loads it
  - on Ubuntu and Fedora it is built for OpenRGB 1.1 from openrgb.org,
    which you install first
  - openSUSE has no OpenRGB that can load it

### Added

- Three support levels:
  - tested devices have been used through the receiver
  - checked devices are on Razer's list for the HyperFlux V2 and driven by
    OpenRazer 3.12.4 or OpenRGB 1.1, with device table entries that match
    theirs
  - listed devices are on Razer's list, but nothing drives them yet
- The Basilisk V3 Pro 35K Phantom Green Edition, as a checked device.
- The Naga V3 Pro, as a checked device in the OpenRGB 1.1 plugin only,
  since OpenRGB 1.0 (which Debian's plugin is for) and OpenRazer 3.12.4 do
  not know it.
- The Naga V2 Pro in OpenRazer and Polychromatic, and its battery level.
- OpenRazer reads the poll rate of the Basilisk V3 Pro, the Basilisk V3 Pro
  35K, the Cobra Pro and the BlackWidow V4 Tenkeyless HyperSpeed.
- Several receivers at once, each with its own key-press links for Ripple.
  So far this has run in the simulator and nowhere else.
- Presence `asleep`, for devices that answer that they are sleeping.
- "charging not reported" and "battery not reported", for a device that
  cannot tell the bridge:
  - shown in `hyperfluxctl` and on the OpenRGB page
  - the desktop's battery widget shows such a device as discharging,
    because that is all the kernel offers
- `hyperfluxctl devices` shows:
  - whether the receiver has a device connected right now
  - what became of its OpenRazer device: live, on its cable, or why not
  - counts of each device's exchanges since the bridge started, which show
    when a device needs other timings
- New `hyperfluxctl` commands: `battery` (with `--waybar`), `watch`,
  `bench`, `capture`, `stall-check` and `report`.
- `--json` on every command, `doctor --fix`, shell completions and a man
  page.
- `hyperfluxctl report` makes one file for a device-support request:
  - the devices output, a guided capture it prompts you through and a
    bench run
  - Doctor, the receiver probe, and this boot's logs from the bridge and
    the device keeper
  - no serial numbers, and receiver ids masked
- The device report form asks for the report file and the device's name.
- `capture` records the receiver's reports to help add a device, and
  leaves out key presses, receiver ids and the parts of the paired-device
  list that could tell receivers apart.
- `bench` measures every online device and counts busy, failed, no-response
  and unsupported answers apart.
- `bench --retry-us` tries other waits between reads, and
  `bench --defaults` measures at the bridge's own timings.
- `stall-check` looks for moments when the cursor freezes while the mouse
  moves.
- `stall-check` lists what HyperFlux Next V2 and the kernel were doing at
  that moment.
- `stall-check --ab` compares half a minute with battery queries and half
  without.
- New Doctor checks:
  - HFX-204 (this login can't reach the bridge)
  - HFX-205 (the desktop doesn't show a battery the bridge has)
  - HFX-206 (the bridge can't work out the receiver's paired devices)
  - HFX-207 (the bridge couldn't create a device's OpenRazer device).
    HFX-404 leaves such devices out.
  - HFX-208 (the device keeper is not running, or the bridge does not
    have it)
  - HFX-304 (Secure Boot blocks the OpenRazer drivers)
  - HFX-305 (DKMS records of an old driver that cannot load anything)
  - HFX-408 (`/dev/uhid` is not usable)
  - HFX-409 (another user can write `/dev/uhid`)
  - HFX-410 (your login can open the receiver's LampArray interfaces,
    which OpenRGB would list as a second copy of each device)
- A world-readable `serial_sha256` on the patched razerkbd and razermouse,
  the hash of a device's serial.
- With `serial_sha256` the bridge tells the paired device on its cable from
  a second unit of the model, and it never reads the serial for this.

### Security

- The shipped OpenRazer drivers now check `hid_is_usb()` before treating a
  device as USB, and a crafted `/dev/uhid` device with a Razer id can't
  make them read a non-USB device as one.
- The fix is a patch of its own, which OpenRazer can take as it is.

### Removed

- The root relay and its preflight service.
- The configuration file.
- Lighting restore.
- The `qualification` command, which `capture` replaces.

### Signing key

The packages and the repository are signed with a new project key. Check
that the key you downloaded has these fingerprints:

- primary: `2068 74CA DD1B 64B3 2483  E2A8 5FC5 921D E97A F7AA`
- signing subkey: `69F3 5BC0 4765 FB65 A86C  D4C9 3E3C 3891 CB26 21D9`
