# Troubleshooting

## Start here

Both of these commands are read-only:

```sh
hyperfluxctl status
hyperfluxctl doctor
```

When everything works, Doctor prints:

```text
HyperFlux Next V2 is ready.
Driver active, bridge running, 1 receiver.
No action required.
```

Otherwise it names the first problem and its code. Then it says why it
happens, gives one thing to do (`Do:`) and tells you how to check the
result (`Verify:`):

```text
HyperFlux Next V2 needs attention.
Primary issue: [HFX-204] This login can't reach the HyperFlux Next V2 service.
Why: ...
Do: Run: sudo systemctl enable --now "hyperflux-next-client@$(id -u).socket"
Verify: Run hyperfluxctl doctor again as yourself and check that the bridge is running.
```

Do that, then run Doctor again.

Some findings are warnings. HyperFlux Next V2 works, but something is
worth a look. If warnings are all Doctor finds, its first line says so,
and the first warning is explained in the same way:

```text
HyperFlux Next V2 is ready, with 1 warning.
Driver active, bridge running, 1 receiver.
Warning: [HFX-409] Programs other than the device keeper can create devices that OpenRazer would trust.
Why: ...
```

and `hyperfluxctl status` ends with:

```text
Next: no action required (1 warning: run hyperfluxctl doctor)
```

Doctor exits with status 2 when it finds anything (warnings included) and
with 0 when it finds nothing.

`hyperfluxctl doctor --explain` lists every finding, not just the first.
`hyperfluxctl doctor --fix` offers to run the fixes it knows. It shows
each command and asks before running it. Fixes that need root go through
`sudo` and always ask.

Each code has a section [below](#error-codes). Some behaviour isn't a
fault at all, and the [known limitations](limitations.md) cover it.

## A device is missing

1. Wake it (move the mouse or press a key). A sleeping device gives no
   answers.
2. Run `hyperfluxctl devices`. It lists the devices paired with the
   receiver, each with its presence (`online`, `asleep`, `not reachable`
   or `waiting for activity`) and who controls it at the moment. The
   `openrazer:` line tells you if OpenRazer has the device through the
   receiver or on its cable. If it has neither, the line says why.
3. Close all the lighting apps but one, then reopen or refresh that one.
4. Run `hyperfluxctl doctor` and do what it says.

If the mouse is missing right after boot, switch it off and on again.

Polychromatic misses a wireless keyboard coming back after you switch it
off and on. Wait a few seconds, then press F5 on its Devices tab.

Binding the receiver to another driver by hand does not make a device
appear. Neither does adding it to an app as a custom device.

## The battery doesn't show

A device shows up in the desktop's battery widget once its level is
known. The bridge asks the paired devices when it starts and after
resume. That normally takes a few seconds. To see what the bridge has,
run `hyperfluxctl battery`:

```text
Razer Basilisk V3 Pro 35K: 79% charging
Razer DeathStalker V2 Pro TKL: 64% (42 minutes old)
```

`not reported yet` means the device hasn't answered. Wake it and wait a
minute. `not reported` means the device answered that it can't tell. The
bridge then stops asking it until it pairs again.

A reading older than 30 minutes is marked with its age and stays in the
widget. A device that is switched off keeps showing its last level.

## Cursor stutters

If the pointer freezes for a moment now and then while you move the mouse,
measure it:

```sh
hyperfluxctl stall-check --ab
```

Keep moving the mouse in circles until the countdown ends (about a
minute). During the first half the check just listens. During the
second it asks the mouse for its battery twice a second, the kind of
query the bridge and apps send.

The output lists every gap over 50 ms in the movement, each with whatever
HyperFlux Next V2 sent to a device at that moment, plus any USB resets or
disconnects from the kernel log. At the end the check
compares the two halves and gives a verdict. An overlap only means a
stall and a query happened at the same time. The comparison between the
halves tells you more. Lighting changes are not
recorded by the bridge, so the check cannot match stalls with them.

Reading the mouse's input needs the `input` group. Without it the check
tells you, and you can run it once with `sudo` instead. Put what it
prints (or its `--json` output) in your [bug report](#asking-for-help).
The rules it applies are in `man hyperfluxctl`.

## Asking for help

1. Keep what `hyperfluxctl doctor` prints.
2. Run `hyperfluxctl support-bundle --preview`. It prints what a bundle
   would contain and writes nothing.
3. Open a [bug report](https://github.com/hyperflux-next/hyperflux-next-v2/issues/new?template=bug_report.yml)
   and paste both. [SUPPORT.md](../../SUPPORT.md) lists other places to ask.

`hyperfluxctl support-bundle --output FILE` writes the bundle itself. You
are the only one who can read the file, and an existing file is never
overwritten. A bundle holds:

- the version of HyperFlux Next V2, the kernel and the distribution
- the state of the driver, the bridge and Full
- Doctor's findings
- each receiver and device, by model, presence, battery and who controls it

Serial numbers, receiver ids, input link names, receiver reports, device
replies and journal text are left out. Nothing is uploaded. You decide
whether to attach it.

For a device that is not supported yet, the maintainer may ask for a
capture. `hyperfluxctl capture --output device.jsonl` records the
receiver's reports for a minute while you use the device. Give the file
to the person who asked and nobody else.

[Privacy](privacy.md) lists what a bundle, a capture and a device report
each hold and leave out.

## Error codes

1xx codes come from the bridge and usually reach you through an app. 2xx
are about the bridge service, 3xx about drivers and 4xx about OpenRazer
and OpenRGB.

### HFX-101

The app and the bridge have no protocol version in common, and the error
message lists the versions the bridge speaks. Update whichever of the two
is older (the app or HyperFlux Next V2).

### HFX-102

The app asked for a feature the running bridge does not have (usually
because the app is newer than HyperFlux Next V2). Update HyperFlux Next V2
first. If the error stays, report it to the app's project.

### HFX-103

HyperFlux Next V2 turned down a request from the app. Either it broke a
protocol rule, or it carried a report that the device's command list does
not allow. Retrying gets the same answer. This is a bug in the app: report
it to the app's project and include the message.

### HFX-104

Two connections said hello with the same client id. Most likely two
copies of the same app are running. Close one. If it happens with a single
copy, report it to the app's project.

### HFX-105

Another app has taken over the device, or the app's hold on it ran out.
Close the other lighting app and try again.

### HFX-106

Too many connections or holds were open at once, and the bridge did not
start the operation. Wait a moment and try again. If you see this a lot,
close the apps you are not using.

### HFX-107

The receiver reconnected, and the app still had a handle from before. Apps
get the new handle by themselves. If one doesn't, refresh or restart it.

### HFX-108

Either HyperFlux Next V2 does not know the device, or the device does not
support the query the app sent. `hyperfluxctl devices` shows what each
device supports. The [hardware list](hardware.md) has the devices
HyperFlux Next V2 can control.

### HFX-109

Nothing went out to the device. Either it is out of reach, or it has been
silent since the receiver was plugged in or since the system resumed. A
receiver that is not writable causes the same error. Wake the device and
wait until `hyperfluxctl devices` shows it `online`, then try again.

### HFX-110

The receiver took the request, but no matching answer came back. Because
the device may or may not have acted on it, neither the bridge nor its
apps send such a request again by themselves. Look at the device before
you retry.

### HFX-111

The receiver was busy for longer than the app would wait. The request
timed out before it was sent. Try again.

### HFX-112

One of the bridge's internal checks failed, and it stopped the operation.
Create a support bundle and [report it](#asking-for-help). Restarting the
bridge (`sudo systemctl restart hyperflux-next-bridge.service`) usually
gets things going again.

### HFX-201

Doctor says "The HyperFlux Next V2 service isn't running." Start and
enable the bridge:

```sh
sudo systemctl enable --now hyperflux-next-bridge.service
```

If the bridge keeps stopping,
`journalctl -b -u hyperflux-next-bridge.service` shows why. Keep that
output for a bug report. Note that lines about a receiver name it by its
id, and that journalctl adds the time and the computer's name to each
line. Serial numbers and key presses are never logged.

### HFX-202

The bridge sees the receiver but can't use it yet. It writes to the
receiver's hidraw interface 0 and reads interface 4, while the device
keeper reads interfaces 0 and 1. Core's udev rule sets them up like this:

- interface 0: user `hyperflux-next`, group `hyperflux-keeper`, mode 0640
- interface 1: group `hyperflux-keeper`, mode 0640
- interface 4: group `hyperflux-next`, mode 0660

If Doctor also reports [HFX-303](#hfx-303), fix that first. To apply
the rule, unplug the receiver for a few seconds and then plug it in again,
or reboot.

### HFX-203

The receiver works, but none of its paired devices is online. Move the
device or click a button. If it is still missing after 30 seconds, switch
it off and on. Devices paired with another receiver don't count, and
neither do devices on a cable.

### HFX-204

Your login has no socket of its own, and the system socket is for members
of the `hyperflux` group. Enable your own socket once:

```sh
sudo systemctl enable --now "hyperflux-next-client@$(id -u).socket"
```

Each desktop user needs one, so there's no need to join `hyperflux`.
Don't join `hyperflux-next` or `hyperflux-keeper` to get around this
either. Those groups belong to the bridge and the device keeper.

### HFX-205

The bridge knows the battery level, but UPower does not list the device.
Either its power supply entry is missing, or the entry has no level to
read (`capacity` gives "No data available").

If Doctor also reports [HFX-208](#hfx-208) or [HFX-408](#hfx-408), fix
that first. If not, restart the bridge:

```sh
sudo systemctl restart hyperflux-next-bridge.service
```

Then check `upower --dump`. The device should be there with its battery.
If it is still missing, [report it](#asking-for-help).

### HFX-206

When the bridge opens a receiver, it asks which devices are paired, and if
it gets no answer it can read, it reads the receiver's list of paired
devices instead. Neither worked on this receiver (the list could not be
read, or its layout is not one the bridge knows).

Nothing is broken. Each device appears as soon as the receiver names it,
so move or wake them one by one. Until then, a battery report from one of
them shows as an "Unidentified pointer device" or "Unidentified keyboard
device".

You can help get this receiver supported. Record what it reports while
you move and wake the devices, then attach the file to a
[report](#asking-for-help):

```sh
hyperfluxctl capture --seconds 60 --output receiver.jsonl
```

The file has the start of the list and the product ids in it. The rest is
left out, because it could tell your receiver apart from another one.

### HFX-207

One of your devices is missing from OpenRazer and Polychromatic. It is
online, but a step the bridge takes before creating its OpenRazer device
failed (usually reading the serial number), and the bridge keeps trying.
Doctor names the device and the step. `hyperfluxctl devices` shows the
same on the `openrazer:` line.

1. Switch the device off and on, or move it.
2. If the finding is still there, restart the bridge:

   ```sh
   sudo systemctl restart hyperflux-next-bridge.service
   ```

3. If the restart doesn't help, [report it](#asking-for-help) with a
   support bundle.

This finding leaves out devices that are asleep, and devices on their
cable (OpenRazer has the wired one then).

### HFX-208

The device keeper is a second, small service
(`hyperflux-next-keeper.service`) that the bridge starts. It holds the
battery entries and the devices OpenRazer drives, and it also reads the
receiver's own reports for the bridge. Without it the desktop shows no
batteries and OpenRazer lists nothing.

Doctor tells you whether the keeper is not running, or runs but has not
reached the bridge. Either way, restart it:

```sh
sudo systemctl restart hyperflux-next-keeper.service
```

If it stops again, `journalctl -b -u hyperflux-next-keeper.service` has
the reason. When the log keeps saying the bridge closed the connection,
the keeper runs as a different user than the bridge expects. Reinstall
`hyperflux-next-v2-linux` to fix that.

### HFX-303

A receiver driver from 2.x or a pre-release is still around, and it can
take the receiver away from the bridge whether it is loaded, installed
for one of your kernels or registered in DKMS with its source (DKMS would
then build it for the next kernel). Doctor says what it found and what to
do, and where there is a command for it, `hyperfluxctl doctor --fix`
offers to run it. Reboot afterwards.

Switching to HyperFlux Next V2 normally removes the 2.x
`hyperflux-next-dkms` package. If it is still installed:

```sh
sudo pacman -R hyperflux-next-dkms       # Arch
sudo apt purge hyperflux-next-dkms       # Ubuntu, Debian
sudo dnf remove hyperflux-next-dkms      # Fedora
sudo zypper remove hyperflux-next-dkms   # openSUSE
```

To remove a DKMS registration, run `sudo dkms remove NAME/VERSION --all`.
When nothing but the loaded copy is left, a reboot is enough.

### HFX-304

Secure Boot is keeping the kernel from loading the OpenRazer drivers
that come with Full (Core has no kernel module). It lets a module in when
the firmware trusts the key it was signed with, and because DKMS signs
the drivers with a key generated on your computer, you enrol that key
once:

```sh
mokutil --sb-state
sudo mokutil --import /var/lib/dkms/mok.pub
```

On Ubuntu, run `sudo update-secureboot-policy --enroll-key` instead.
Choose a one-time password and reboot. In the blue MokManager screen,
pick Enroll MOK, Continue, Yes. [Secure Boot](secure-boot.md) has the
steps for each distribution and how to check the result.

### HFX-305

Leftover DKMS records of an old receiver driver, with no source to build
from and no module for any kernel you have. Usually the driver was built
for kernels that have since been removed. The records are harmless, which
is why Doctor lists this last. Delete them whenever it suits you. Doctor
prints the command, for example:

```sh
sudo rm -r /var/lib/dkms/hid-hyperflux-next
```

`hyperfluxctl doctor --fix` can run it for you, after asking.

### HFX-401

One of `razerkbd`, `razermouse`, `razerkraken` or `razeraccessory` is
missing, or the kernel still has an older copy loaded. Reboot first. If
Doctor still reports it afterwards, reinstall
`hyperflux-next-v2-openrazer-driver-dkms` (see [HFX-402](#hfx-402)).

### HFX-402

The installed OpenRazer driver is some other build, which cannot drive
the devices on the receiver. Most often the distribution's own OpenRazer
driver has come back. Reinstall the HyperFlux Next V2 one, then reboot:

- Arch: `sudo pacman -S hyperflux-next-v2-openrazer-driver-dkms`
- Ubuntu, Debian: `sudo apt install --reinstall hyperflux-next-v2-openrazer-driver-dkms`
- Fedora: `sudo dnf reinstall hyperflux-next-v2-openrazer-driver-dkms`
- openSUSE: `sudo zypper install --force hyperflux-next-v2-openrazer-driver-dkms`

### HFX-403

The group OpenRazer uses for access (`openrazer` on Arch, `plugdev`
elsewhere) does not exist. Create it from the file the driver package
installs:

```sh
sudo systemd-sysusers openrazer.conf
```

Without that file, reinstall `hyperflux-next-v2-openrazer-driver-dkms` as
in [HFX-402](#hfx-402).

### HFX-404

Doctor says "OpenRazer isn't ready for this login." That has one of three
causes:

- your user is not in the OpenRazer group yet
- `openrazer-daemon` is not running
- the daemon has not listed every device the bridge created for it

A device that is asleep or out of range is not part of this check.
OpenRazer gets it once it answers. Until then Doctor just notes that it
is waiting for it. If the bridge itself failed to create a device, you get
[HFX-207](#hfx-207) instead.

Join the group Doctor names (`openrazer` on Arch, `plugdev` elsewhere),
log out and back in, and check the daemon:

```sh
sudo gpasswd -a "$USER" openrazer   # Arch
sudo gpasswd -a "$USER" plugdev     # Ubuntu, Debian, Fedora, openSUSE
systemctl --user status openrazer-daemon
```

Run Doctor as your normal user, not with `sudo`.

### HFX-405

`hyperflux-next-v2-full` and `hyperflux-next-v2-linux` are at different
versions. Update both together in your normal system update.

### HFX-406

Key presses don't set off Ripple or the other effects that react to
them. OpenRazer needs a key-press link that is missing, and it looks for
that link only when it first finds the keyboard. Reboot, and the udev rules
create it. If the link is still missing after that, reinstall
`hyperflux-next-v2-full`.

Doctor stays quiet about this if you turned the links off on purpose, as
described in [limitations](limitations.md#ripple-and-key-presses).

### HFX-407

OpenRazer lists no devices at all. It stops when it reaches a link in
`/dev/input/by-id` whose device is gone. Doctor prints the link. Remove
it and restart OpenRazer:

```sh
sudo rm /dev/input/by-id/NAME-FROM-DOCTOR
systemctl --user restart openrazer-daemon
```

Then press F5 in Polychromatic. HyperFlux Next V2 never removes the link
for you.

### HFX-408

The battery entries and the devices OpenRazer drives are virtual
devices, and the keeper creates them through `/dev/uhid`. Right now it
can't. At boot Core loads the
`uhid` module, and Core's udev rules give `/dev/uhid` to the keeper's
group, `hyperflux-keeper`. A reboot applies both. If the finding is still
there after it, reinstall `hyperflux-next-v2-linux`.

### HFX-409

The patched OpenRazer drivers trust a virtual device because only the
keeper's group can create one, but here an ACL also lets some user write
`/dev/uhid`, and that user's programs could create devices OpenRazer
would trust. Usually a `uaccess` tag sets that ACL
(Sunshine's `60-sunshine.rules` has one, for example).

Sunshine needs the tag for its virtual input, and for that reason
HyperFlux Next V2 never changes it for you. If you can do without
Sunshine's virtual input:

1. Copy Sunshine's rules file to `/etc/udev/rules.d/`, keeping its name.
2. Remove `TAG+="uaccess"` from its `uhid` line.
3. Reboot.

### HFX-410

Your login can open the receiver's lighting interfaces. Interfaces 3 and
4 are HID LampArrays, and OpenRGB 1.0 and 1.1 have a generic "HID
LampArray Device" driver that opens any LampArray it can. OpenRGB would
then show each paired device twice: once as it does now, and once as a
LampArray device named after the receiver. The two would fight over the
lights. On exit, OpenRGB leaves LampArray autonomous mode off.

Core's udev rule keeps interface 3 owned by root and gives interface 4
to the `hyperflux-next` group. Nobody but the bridge should be in that
group.

Doctor names the node and how it is open. If your login is in the
`hyperflux-next` group, leave it, then log out and back in:

```sh
sudo gpasswd -d "$USER" hyperflux-next
```

Reach the bridge through your own socket instead ([HFX-204](#hfx-204)).

If you are not in that group, some other udev rule opens the node, either
with a `uaccess` tag (Doctor then sees an ACL entry for your uid) or by
setting a `MODE` or `GROUP` of its own. Such rules usually name hidraw or
Razer's vendor id, and this finds them:

```sh
grep -rn -e hidraw -e 1532 /etc/udev/rules.d /usr/lib/udev/rules.d
```

A file in `/usr/lib/udev/rules.d` can be overridden. Copy it to
`/etc/udev/rules.d/` under the same name, and the copy takes the
original's place. Take out the part that opens the receiver (`1532:00cf`)
to you. Then reboot, or unplug the receiver and plug it back in after a
few seconds.
