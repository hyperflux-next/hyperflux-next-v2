# Known limitations

What doesn't work, or works differently than over a cable, and what to do
about it.

## Distributions

- I test releases on real hardware on Arch Linux. The Ubuntu, Debian,
  Fedora and openSUSE packages are built, installed and removed again in
  each distribution's container for every change. They are not tested on
  real hardware, and distributions based on any of the five aren't part of
  the container test. If something fails there, run `hyperfluxctl doctor`
  and [report it](../../SUPPORT.md).
- Packages are built for x86_64 alone.
- Ubuntu 26.04 LTS and Debian 13 aren't supported. Their OpenRazer
  versions (3.11.0 and 3.10.2) lack the BlackWidow V4 Tenkeyless
  HyperSpeed. Debian 13's also lacks the V4 Mini HyperSpeed, and Debian
  doesn't package OpenRGB for 13. Distributions based on an Ubuntu LTS
  release, such as Linux Mint, aren't supported either: their OpenRazer is
  just as old, or older.
- On image-based distributions such as Bazzite or SteamOS, `/usr` is
  read-only, and the packages can't be installed there.
- The bridge is a systemd service, so distributions that boot another
  init, such as Artix or Devuan, aren't supported.
- Ubuntu supports 26.10 until July 2027. Debian 14 is still Debian's
  testing release and changes more often than a stable one.
- HyperFlux Next V2 is not in any distribution's official repositories.
  It comes from its own signed repository.

## Which OpenRGB loads the plugin

The plugin is compiled against one OpenRGB release and works in that
release alone. OpenRGB also turns it down when OpenRGB's Qt has another
major version, or is older than the Qt the plugin was built with.

- Arch: Arch's own OpenRGB 1.1.
- Debian: Debian's own OpenRGB 1.0. The 1.1 package from openrgb.org
  will not load it, because the Debian plugin is built for 1.0 with Qt 5.
- Ubuntu: OpenRGB 1.1 from openrgb.org (the Debian Trixie package), with
  the separate `hyperflux-next-v2-openrgb-plugin`.
- Fedora: OpenRGB 1.1 from openrgb.org (the Fedora 44 package), with the
  separate `hyperflux-next-v2-openrgb-plugin`.
- openSUSE: none.

Ubuntu 26.10 packages an OpenRGB snapshot from before 1.0. Fedora 44 and
openSUSE Tumbleweed have a 1.0 release candidate. Those load plugins for
OpenRGB's older plugin interface and no others. On openSUSE the Fedora
package from openrgb.org fails to install, because it asks for libraries
by Fedora's package names.

The Flatpak and the AppImage from openrgb.org reject the plugin on every
distribution, because they bring their own Qt (6.10 in the Flatpak's KDE
runtime and 6.8 in the AppImage). The plugin packages use Qt 6.11 or newer,
and Debian's uses Qt 5. The AppImage still looks in
`/usr/lib/openrgb/plugins` and lists the plugin as incompatible, which is
harmless.

OpenRazer apps work everywhere, and Doctor reports Full as ready without
the plugin.

The Naga V3 Pro shows in the plugin for OpenRGB 1.1 but not in Debian's
plugin. OpenRGB 1.0 has no entry for it.

## Ripple and key presses

Ripple, and other effects that light the keys you press, need OpenRazer
to see the key presses. Full gives it per-receiver links for that.
OpenRazer looks for the links when it first finds the keyboard and not
after, so reboot once after installing Full. Doctor tells you when that
is needed ([HFX-406](troubleshooting.md#hfx-406)).

With two receivers, OpenRazer attaches all the matching links to each
keyboard, which means a keyboard also reacts to key presses on the other
receiver. Fixing that needs a change in OpenRazer. Two receivers have run
in the simulator, not on real hardware.

The links for the BlackWidow V3 Pro, the DeathStalker V2 Pro and the
BlackWidow V4 Mini HyperSpeed also match what OpenRazer looks for on the
same model by cable. A wired keyboard of that model, or the same one on
its cable, also reacts to key presses on the receiver's keyboard.

The ring can look like two lines, or start one key to the right.
OpenRazer draws the keyboard as six rows. It uses one key map for all
keyboards, and that map assumes a column of macro keys left of Esc. Both
problems happen over a cable too.

Ripple stops after a brightness change, because OpenRazer counts the
change as new lighting. Pick another effect and then Ripple again.

The ring can also skip keys. A full keyboard frame takes longer through
the receiver than over a cable. The ring moves on before every key along
its path has lit.

### Who can read your key presses

The links let your user account read the keyboard's key presses while
you're logged in at the computer. Any program you run can read them too.
Other accounts can't. To turn the links off:

```sh
sudo ln -s /dev/null /etc/udev/rules.d/70-hyperflux-next-openrazer-keys.rules
```

and reboot. Ripple then stays still, and Doctor stops warning about it.
To turn the links back on, remove that file and reboot.

## Apps

- OpenRGB may describe a black keyboard as Quartz. Nothing but the
  description is wrong.
- The keyboard's own Fn lighting shortcuts change the lighting inside the
  keyboard, and OpenRazer and Polychromatic don't notice. Apply the effect
  again from the app to take control back.
- Polychromatic may list the receiver as an unrecognised `1532:00CF`
  device. Ignore it. The keyboard and mouse appear under their own names.
- Polychromatic misses a device that was switched off and on. Press F5 on
  its Devices tab.
- Polychromatic's tray menu can miss a device that was asleep when you
  logged in, such as a keyboard you hadn't touched yet. A device on the
  receiver reaches OpenRazer when it first wakes, but the tray lists
  devices once, when it starts, and ignores ones OpenRazer adds later. Pick
  Refresh in the tray menu, or wake both devices before you log in.
- Some devices never report their brightness, even over a cable. Apps
  then show a default. The bridge does not make up a value.
- Say you have two units of the same model, one wireless on the receiver
  and one by cable. Both show if the bridge can read the cabled one's
  serial hash and it differs. Without the hash (an older OpenRazer
  driver, or a serial that cannot be read), the bridge has no way to tell
  them apart, and it hides the wireless one's OpenRazer copy while the
  cable is in ([hardware](hardware.md#a-device-plugged-in-by-cable)).

## Batteries

- Charging is tested on the Basilisk V3 Pro 35K and the DeathStalker V2
  Pro TKL. The checked devices are asked the same question OpenRazer
  asks, but not tested through the receiver. That includes the Naga V3
  Pro and the BlackWidow V4 Low-Profile TKL HyperSpeed. OpenRazer 3.12.4
  and OpenRGB 1.1 don't ask those two, though owners' captures show
  both answering on their own dongles
  ([hardware](hardware.md#the-checked-devices)).
- If the bridge cannot learn whether a device is charging, the desktop's
  battery widget shows it as discharging, even on its cable, because once
  a battery has a level, the kernel has no "unknown" state for it.
  `hyperfluxctl` and the OpenRGB page say `charging not reported`
  instead.
- The Basilisk V3 Pro 35K reports charging on its mat or cable. It keeps
  reporting it while it holds at a charge limit set elsewhere. The
  DeathStalker V2 Pro TKL reports charging on 2.4 GHz with its cable in.
  With its switch off and the cable in, it becomes a wired keyboard
  instead ([hardware](hardware.md#a-device-plugged-in-by-cable)). Neither
  reports "full" by itself. The widget shows full when a device charges
  at 100 %.
- The receiver gives no sign when a mouse goes on or off its mat. The
  bridge asks the mouse instead: when you pick it up and move it (at the
  first short pause, a second or more later), and 3 s and 18 s after you
  put it down. Those times were measured in the simulator. A change that
  leaves the pointer still shows within a minute.
- When a checked keyboard goes on its cable, charging shows at its next
  battery question, up to ten minutes later. With Full it shows within
  seconds, when OpenRazer finds the keyboard on its cable.
- UPower calls a device battery at 100 % full even while it discharges.
  The widget shows discharging again from 99 %.
- A reading turns stale after 30 minutes. The bridge asks a device that
  has gone ten minutes without reporting its level. A stale reading
  therefore means the device gave no answer or could not be asked,
  because it is off, asleep or out of range, or cannot report its level.
  The widget keeps the last reading, and `hyperfluxctl` and the OpenRGB
  page show its age. The 30 minutes are based on how often the tested
  devices report.
