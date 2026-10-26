# Hardware

HyperFlux Next V2 works with the Razer HyperFlux V2 receiver (USB
`1532:00cf`) and the keyboards and mice paired to it.

The bridge treats each device according to the id it has through the
receiver. A device it doesn't know is listed as `Unknown device 1532:xxxx`
and left alone.

## Devices

The status column has three values.

- tested: I have used the device through the receiver with HyperFlux
  Next V2, on my own Arch machine. That covers battery, charging, and
  lighting from OpenRazer and OpenRGB, plus Ripple on the keyboard.
- checked: Razer lists the device as working with the HyperFlux V2.
  OpenRazer 3.12.4 or OpenRGB 1.1 supports it, and the entry in
  `devices.toml` matches theirs. It is
  not tested through the receiver. `hyperfluxctl` and the OpenRGB page
  say `not yet tested through the receiver`.
- listed: Razer lists the device, but neither release drives it. Until
  one does, the bridge has nothing to pass its commands to.

| Device | Kind | Through the receiver | By cable | Status | Works in |
| --- | --- | --- | --- | --- | --- |
| Basilisk V3 Pro 35K | mouse | `1532:00cd` | `1532:00cc` | tested | OpenRazer, OpenRGB |
| DeathStalker V2 Pro TKL | keyboard | `1532:0296` | `1532:0298` | tested | OpenRazer, OpenRGB |
| Naga V2 Pro | mouse | `1532:00a8` | `1532:00a7` | checked | OpenRazer, OpenRGB |
| Naga V3 Pro | mouse | `1532:00e8` | `1532:00e7` | checked | OpenRGB 1.1 |
| Basilisk V3 Pro | mouse | `1532:00ab` | `1532:00aa` | checked | OpenRazer, OpenRGB |
| Basilisk V3 Pro 35K Phantom Green Edition | mouse | `1532:00d7` | `1532:00d6` | checked | OpenRazer, OpenRGB |
| Cobra Pro | mouse | `1532:00b0` | `1532:00af` | checked | OpenRazer, OpenRGB |
| BlackWidow V3 Pro | keyboard | `1532:025c` | `1532:025a` | checked | OpenRazer, OpenRGB |
| DeathStalker V2 Pro | keyboard | `1532:0290` | `1532:0292` | checked | OpenRazer, OpenRGB |
| BlackWidow V4 Mini HyperSpeed | keyboard | `1532:02ba` | `1532:02b9` | checked | OpenRazer |
| BlackWidow V4 Low-Profile TKL HyperSpeed | keyboard | `1532:02d2` | `1532:02d4` | checked | OpenRGB |
| BlackWidow V4 Tenkeyless HyperSpeed | keyboard | `1532:02d5` | `1532:02d7` | checked | OpenRazer, OpenRGB |
| Cobra HyperSpeed | mouse | `1532:00db` | `1532:00da` | listed | |
| Basilisk V4 Pro | mouse | `1532:00e3` | `1532:00e2` | listed | |
| Basilisk V4 HyperSpeed | mouse | `1532:00ea` | `1532:00eb` | listed | |
| BlackWidow V4 Low-Profile HyperSpeed | keyboard | `1532:02c9` | `1532:02cc` | listed | |

Razer asks for the latest firmware on the mouse and the receiver before
you use them together. Razer Synapse installs it, and Synapse runs on
Windows.

In the last column, "OpenRazer" includes the apps built on it
(Polychromatic, for example). OpenRGB needs the plugin. Full brings it on
Arch and Debian, and on Ubuntu and Fedora it comes as a separate package
for OpenRGB 1.1 from openrgb.org. "OpenRGB 1.1" means the plugin for
OpenRGB 1.1 and not Debian's, which is for 1.0.

`hyperfluxctl`, the battery widget and the apps use the upstream names.
Three of them differ from the table: Naga Pro V3, Blackwidow V4 Low
Profile TKL and BlackWidow V4 Mini Hyperspeed. The four listed devices
are waiting for OpenRazer or OpenRGB to support them at all.

## The checked devices

For lighting, the bridge passes on the same commands that OpenRazer and
OpenRGB send the device through its own dongle, and it refuses commands
missing from the device's table. Lighting should therefore work in the
apps listed above. If an effect does nothing, please report it.

The bridge asks each device for its battery level when it connects, after
resume, and every ten minutes unless the device reports it on its own.
A device that answers that it cannot tell is not asked again, and shows
`battery not reported`.

Charging comes from the same question OpenRazer asks. A mouse is asked
when you pick it up or put it down, and once a minute. A keyboard is
asked with each battery question, and when OpenRazer finds it on its
cable or loses it there. Like OpenRazer, the bridge counts any answer
other than 0 as charging. On its mat, for example, the Basilisk V3 Pro
35K answers 6. Some devices say they cannot tell, or give an answer too
short to hold a value. The bridge stops asking those until they pair
again. `hyperfluxctl` and the OpenRGB page then say
`charging not reported` for them.

The Naga V3 Pro and the BlackWidow V4 Low-Profile TKL HyperSpeed get both
questions too, even though neither OpenRazer 3.12.4 nor OpenRGB 1.1 reads
the battery of either one. Owners' captures and open OpenRazer pull requests
show both answering the battery question on their own dongles. The same
captures show them answering the charging question, with 1 on their cables.

Only the Basilisk V3 Pro 35K says whether it's on its mat.

The desktop's battery widget shows a device without a charging state as
discharging, even on its cable, because that is what the kernel reports for
it. OpenRazer apps may still say charging, since they ask the device
themselves.

Ripple on a keyboard needs its key presses on the receiver's keyboard
node. The DeathStalker V2 Pro TKL delivers them there.

Not tested on any checked device:

- the answers to the battery and charging questions through the receiver
- lighting through the receiver
- Ripple on the checked keyboards

## Receivers

Testing uses one receiver. Several at once work in the simulator.

Not tested: two or more receivers on real hardware.

## A device plugged in by cable

By cable a device has its wired id, the one in the table above. While a
wired device with that id is bound to its OpenRazer driver, the bridge takes
away the copy it made for OpenRazer through the receiver, and OpenRazer and
Polychromatic then list the device once, as the wired one. The bridge does
this as soon as the kernel announces the wired device, before OpenRazer gets
to it. It makes the copy again when the cable comes out. In the meantime
`hyperfluxctl devices` says `openrazer: on its cable`. The battery entry
stays, and the other device on the receiver is unaffected.

When does a device count as wired? The Basilisk V3 Pro 35K became a wired
mouse as soon as its cable went in. With the DeathStalker V2 Pro TKL it
depends on the switch. On 2.4 GHz with the cable in, it charges and stays
on the receiver, with no wired copy, and nothing changes. With the switch
off and the cable in, it's a wired keyboard, and the bridge steps aside.

The bridge tells a second unit apart by its serial. OpenRazer lets only its
own group read the serial itself, but the patched drivers also publish a
world-readable `serial_sha256` (a hash of the serial), which the bridge can
read. If the hash on the cable differs from the paired device's, it is
another unit and the copy stays. If it matches, it is the paired device on
its cable, and the copy goes.

Without a hash to compare (an older driver that lacks the attribute, or an
unreadable serial), the bridge goes by the wired id alone, and a second
unit's cable then hides the paired device's copy while it is plugged in.

The hash is not the serial, so neither the report nor the attribute
gives the serial away. Still, it is no real secret
([privacy](privacy.md#who-on-the-computer-can-see-what) says why). It
keeps the serial out of plain sight and nothing more.

## Reporting a device

If you own a checked device, a report turns it into a tested one. Run

    hyperfluxctl report

and follow what it prints. It takes about three minutes. You'll be asked
to:

- leave the device alone
- use it
- put a mouse on and off the HyperFlux V2 mat
- plug the cable in and out
- let the device sleep and wake

The result is one `hyperflux-next-report-*.json` file. It holds the
output of `devices`, `doctor`, `probe receiver` and `bench --defaults`,
the guided capture, and this boot's log lines from the bridge and the
device keeper. Key presses and pointer movement are not recorded. There
are no serial numbers in it either, and
[privacy](privacy.md#files-for-bug-reports) lists what else it leaves
out.

Then open a
[device report](https://github.com/hyperflux-next/hyperflux-next-v2/issues/new?template=device_support.yml),
attach the file, give the device's name and tick what you saw work.
