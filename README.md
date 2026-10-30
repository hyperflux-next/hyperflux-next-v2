# HyperFlux Next V2

HyperFlux Next V2 is Linux support for the Razer HyperFlux V2 receiver
(USB `1532:00cf`). It lets OpenRazer and Polychromatic control the
keyboards and mice paired to the receiver. A plugin shows them in OpenRGB,
and their batteries appear in the desktop's battery widget. Typing and
pointing work without HyperFlux Next V2, as they always have.

Not affiliated with or endorsed by Razer.

## Devices

I test with the Razer Basilisk V3 Pro 35K and the Razer DeathStalker V2 Pro
TKL on the receiver.

These are checked:

- Naga V2 Pro
- Naga V3 Pro, in OpenRGB 1.1 only
- Basilisk V3 Pro
- Basilisk V3 Pro 35K Phantom Green Edition
- Cobra Pro
- BlackWidow V3 Pro
- DeathStalker V2 Pro
- BlackWidow V4 Mini HyperSpeed, in OpenRazer only
- BlackWidow V4 Low-Profile TKL HyperSpeed, in OpenRGB only
- BlackWidow V4 Tenkeyless HyperSpeed

Razer lists them for the receiver, and each one works with OpenRazer
3.12.4 or OpenRGB 1.1, the releases HyperFlux Next V2 builds on, but they
are not tested through the receiver. `hyperfluxctl` and the OpenRGB page
label them `not yet tested through the receiver`, and their charging state
may not show.

[Hardware](docs/user/hardware.md) has their USB ids, the apps each one
works in and what was tested on which device.

If you own one of the checked devices, a short
[device report](docs/user/hardware.md#reporting-a-device) helps the next
person.

## Install

There are packages for Arch Linux, Ubuntu 26.10, Debian 14, Fedora 44 and
openSUSE Tumbleweed, all on x86_64. A distribution based on one of these
releases can use the same packages, unless it doesn't run systemd or keeps
`/usr` read-only ([limitations](docs/user/limitations.md#distributions)).
Plug the receiver in before you start.

### The quick way

```sh
curl -fsSL https://hyperflux-next.github.io/hyperflux-next-v2/install.sh | sh
```

The script does steps 1 to 3 from [By hand](#by-hand) for you. Your package
manager shows what it's about to install and waits for a yes, and the script
asks before it reboots. After the reboot, `hyperfluxctl doctor` should say
`HyperFlux Next V2 is ready.`

### By hand

Each distribution has a guide with these steps in full:
[Arch](docs/user/install-arch.md),
[Ubuntu or Debian](docs/user/install-apt.md) and
[Fedora or openSUSE](docs/user/install-rpm.md).

1. Add the HyperFlux Next V2 package repository, which the project runs
   and signs. You do this once.
2. Install `hyperflux-next-v2-full`. Full connects the devices to OpenRazer,
   and on Arch and Debian to OpenRGB as well. On Ubuntu and Fedora the
   OpenRGB plugin is a separate package, for OpenRGB 1.1 from openrgb.org,
   and openSUSE has none. Core (`hyperflux-next-v2-linux`) is just the
   bridge, its device keeper and `hyperfluxctl`. It adds no kernel module.
3. Enable the bridge and your own socket, then reboot:

   ```sh
   sudo systemctl enable hyperflux-next-bridge.service "hyperflux-next-client@$(id -u).socket"
   sudo reboot
   ```

4. Check the result:

   ```sh
   hyperfluxctl doctor
   ```

   It prints `HyperFlux Next V2 is ready.` or tells you what to fix.

Replacing the older `hyperflux-next-*` packages (2.x or a test build)?
Read [moving from 2.x](docs/user/moving-from-2x.md) first. The packages
have new names, and 2.x installs need the new repository too.

## When something doesn't work

Start with `hyperfluxctl doctor` and do what it says. Each code it prints
is explained in [troubleshooting](docs/user/troubleshooting.md).

If that doesn't help, ask in Discussions or open a bug report with the
Doctor output. [Support](SUPPORT.md) has both links.

## More

The [documentation index](docs/README.md) lists every page. See also
[privacy](docs/user/privacy.md), [contributing](CONTRIBUTING.md),
[security](SECURITY.md) and the [changelog](CHANGELOG.md).

## Licence

HyperFlux Next V2 is free software under the GNU General Public License,
version 2 only. See [LICENSE](LICENSE).
