# First run

## Check that HyperFlux Next V2 is ready

`hyperfluxctl status` shows the state of the receiver, the bridge, its
device keeper and each paired device:

```sh
hyperfluxctl status
```

```text
Driver: active
Bridge: running
Keeper: running, connected to the bridge
Receivers: 1
Razer Basilisk V3 Pro 35K: online, battery 79%, charging
Razer DeathStalker V2 Pro TKL: asleep, battery 64%, no update for 42 minutes
Next: no action required
```

`Driver` says whether the receiver can be reached through the kernel's
hidraw driver. `Bridge` says whether the bridge service is running.
`Keeper` is about the device keeper, which holds the battery entries and
the OpenRazer devices. It says whether the keeper runs and has reached
the bridge.

Below those, each paired device gets a line with its presence (`online`,
`asleep`, `not reachable` or `waiting for activity`) and its battery.

A device marked `(not yet tested through the receiver)` is one of the
checked devices. It should work, but it isn't among the tested ones
([hardware](hardware.md) explains the difference). If you have one, a
short [device report](hardware.md#reporting-a-device) will help the next
person.

The last line, `Next`, tells you what to do, if anything.

For a full check, run:

```sh
hyperfluxctl doctor
```

Doctor goes through everything in order and says what to fix first. Any
of the commands takes `--json` for use in scripts. `man hyperfluxctl`
describes them all.

## Lighting apps

The lighting apps are separate projects. Your install guide says where
each one comes from. With Core alone they have no access to the devices on
the receiver, but the keyboard and mouse still work normally and their
batteries still show.

Try the apps one at a time. That way a problem points at one of them.

1. Open Polychromatic. Your keyboard and mouse appear under their own
   names. Apply an effect, check that the device changes, then close
   Polychromatic.
2. Open OpenRGB, if you have the plugin. Full brings it on Arch and
   Debian. On Ubuntu and Fedora it is a separate package for OpenRGB 1.1
   from openrgb.org. Change a colour and check that the device follows.

OpenRGB also gets a HyperFlux Next V2 page under its Information tab,
with one card per paired device. A card shows the battery, how the device
is connected and how old the reading is. It also says whether the device
is charging, or `Charging not reported` for a device that never says.
If the bridge is out of reach, the page tells you what to run.

HyperFlux Next V2 needs no change to any OpenRazer, Polychromatic or
OpenRGB setting. Your effects and profiles stay in those apps.

## Batteries on the desktop

While the bridge runs, each device's battery shows in the desktop's
battery widget (UPower) under the device's own name, such as Basilisk V3
Pro 35K. There is nothing to set up.

The bridge asks the devices for their levels when it starts and after
resume, and the battery usually shows up within seconds. A device that has
gone ten minutes without reporting its level is asked again. One that's
switched off keeps its last reading, and `hyperfluxctl battery` shows how
old it is.

If the bridge has no way to learn whether a device is charging, the
widget shows it as discharging, even on its cable, while
`hyperfluxctl battery` says `charging not reported` for it
([limitations](limitations.md#batteries) explains why).

## A status bar module

For a status bar, `hyperfluxctl battery --waybar` prints one JSON object
with these fields:

| Field | Contents |
| --- | --- |
| `text` | all the levels, such as `79% 64%` |
| `tooltip` | one line per device |
| `percentage` | the emptiest battery |
| `class` | `normal`, `low` (20 % or less), `critical` (10 % or less), `charging`, `stale`, `unknown` or `unavailable`. Not `low` or `critical` while the level is rising |

The answer comes from what the bridge already knows. It does not ask the
devices.

For Waybar, add this to `config.jsonc`:

```jsonc
"custom/hyperflux": {
    "exec": "hyperfluxctl battery --waybar",
    "return-type": "json",
    "interval": 60
}
```

and, for example, this to `style.css`:

```css
#custom-hyperflux.low { color: #e5c07b; }
#custom-hyperflux.critical { color: #e06c75; }
```

For Polybar, with `jq` installed:

```ini
[module/hyperflux]
type = custom/script
exec = hyperfluxctl battery --waybar | jq -r .text
interval = 60
```

## If something doesn't work

Run `hyperfluxctl doctor` first, and look up any code it prints in
[troubleshooting](troubleshooting.md).

If you still need help, please open a bug report and include what
[asking for help](troubleshooting.md#asking-for-help) lists.
