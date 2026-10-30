# Moving from 2.x

HyperFlux Next V2 1.0 follows HyperFlux Next 2.2.0. Its version numbers
start again, and its packages have new names. Each new package takes the
place of an old one:

- `hyperflux-next-linux` becomes `hyperflux-next-v2-linux`.
- `hyperflux-next-dkms` has no successor. 1.0 has no receiver kernel
  module, and `hyperflux-next-v2-linux` replaces it.
- `hyperflux-next-openrazer-driver-dkms` becomes
  `hyperflux-next-v2-openrazer-driver-dkms`.
- `hyperflux-next-full` becomes `hyperflux-next-v2-full`.

When you switch, the old packages are removed. The bridge and socket
units keep their names, and the bridge stays enabled.

If Doctor reports [HFX-204](troubleshooting.md#hfx-204) afterwards,
enable your socket again as step 3 of your install guide shows.

## Switch the repository

1.0 is published from a new address,
`https://hyperflux-next.github.io/hyperflux-next-v2`. Your system still
points at the 2.x repository, and you need to swap the entry once.

1. Remove the old entry and its key.
   - On Arch, delete the `Include = /etc/pacman.d/hyperflux-next.conf` line
     or the `[hyperflux-next]` section from `/etc/pacman.conf`, whichever
     you have. Then run:

     ```sh
     sudo rm -f /etc/pacman.d/hyperflux-next.conf /var/lib/pacman/sync/hyperflux-next.*
     ```

     The second path is pacman's copy of the old package list. It's
     signed with the old key, and the new repository has the same name.

     The old key is still trusted. `pacman-key --list-keys` shows its
     fingerprint. Remove it with:

     ```sh
     sudo pacman-key --delete FINGERPRINT
     ```

   - On Ubuntu and Debian, run:

     ```sh
     sudo rm /etc/apt/sources.list.d/hyperflux-next.sources /etc/apt/keyrings/hyperflux-next.asc
     ```

   - On Fedora, run:

     ```sh
     sudo rm /etc/yum.repos.d/hyperflux-next.repo
     ```

   - On openSUSE, run:

     ```sh
     sudo zypper removerepo hyperflux-next
     ```

   On Fedora and openSUSE the old key stays in the RPM database. To find
   it, list the imported keys with
   `rpm -q gpg-pubkey --qf '%{NAME}-%{VERSION}-%{RELEASE} %{SUMMARY}\n'`.
   Then pass the HyperFlux Next one's name to `sudo rpm -e`.

2. Add the new entry as step 1 of your install guide shows:
   [Arch](install-arch.md), [Ubuntu or Debian](install-apt.md),
   [Fedora or openSUSE](install-rpm.md).

## Switch the packages

Run the command for your distribution:

- Arch: `sudo pacman -Syu`, and answer `Y` each time it offers to replace
  a `hyperflux-next-*` package with a `hyperflux-next-v2-*` one
- Ubuntu and Debian:
  `sudo apt update && sudo apt install hyperflux-next-v2-full`, or
  `hyperflux-next-v2-linux` if you only had Core
- Fedora: `sudo dnf upgrade`
- openSUSE: `sudo zypper dup`

On Ubuntu and Debian you have to name the new package, because apt
does not switch package names by itself. apt then removes the old packages
that the new one replaces.

Then reboot with the receiver connected, and run Doctor:

```sh
hyperfluxctl doctor
```

**Don't skip the reboot.** The old module stays loaded and holds the
receiver until you do.

If Doctor still reports [HFX-303](troubleshooting.md#hfx-303) after the
reboot, part of the old driver is still in place. Doctor names it and
says how to remove it.

### What Ubuntu and Debian keep

apt removes the old packages but keeps their configuration. That is why
`dpkg -l 'hyperflux-next-*'` still lists them as `rc`. This is harmless.

Be careful if you purge them, for example with `sudo apt purge '~c'`.
The old packages' scripts will turn off the units they used to own, and
those units now belong to the new packages. After a purge, enable the
units again:

```sh
sudo systemctl enable hyperflux-next-bridge.service "hyperflux-next-client@$(id -u).socket"
```

## From the 1:1.0.0 test build

An early test build of 1.0.0 used the old names at version `1:1.0.0-1`
(`hyperflux-next-linux`, `hyperflux-next-openrazer-driver-dkms` and
`hyperflux-next-full`). The new packages replace those too.

The repository address is the same, and only the packages change.
Switch them as above, then reboot.

If you installed the test build from files, install the new files the
same way. Install them all at once, or just the `linux` one for Core. On
Ubuntu and Fedora, leave the `openrgb-plugin` file out unless you have
OpenRGB 1.1 from openrgb.org. The OpenRGB page section of your install
guide explains why.

- Arch: `sudo pacman -U ./hyperflux-next-v2-*.pkg.tar.zst`, and answer
  `y` when it asks to remove each conflicting `hyperflux-next-*` package
- Ubuntu and Debian: `sudo apt install ./hyperflux-next-v2-*.deb`
- Fedora: `sudo dnf install ./hyperflux-next-v2-*.rpm`
- openSUSE: `sudo zypper install ./hyperflux-next-v2-*.rpm`

The notes on [what Ubuntu and Debian keep](#what-ubuntu-and-debian-keep)
apply here as well.

## What changes

There is no receiver kernel module any more. The bridge talks to the
receiver through the kernel's own hidraw driver, and each battery shows
through `/dev/uhid`. The `hyperflux-next-v2-linux` package replaces
`hyperflux-next-dkms`, and removing `hyperflux-next-dkms` also takes its
module out of DKMS. Core no longer needs kernel headers. Secure Boot
matters for Full's OpenRazer drivers and nothing else.

There is no configuration file either. `/etc/hyperflux-next/bridge.json`
is gone, and nothing reads it now. If you had edited it, Arch keeps your
copy as `bridge.json.pacsave`, and Fedora and openSUSE keep it as
`bridge.json.rpmsave`. On Ubuntu and Debian it stays in place until you
purge the old package. The empty `/etc/hyperflux-next` can be deleted.

`/var/lib/hyperflux-next` is in use again. The bridge keeps its learned
waits there.

HyperFlux Next V2's own device keeper now makes the OpenRazer virtual
devices. The root relay service and its preflight check go away with the
switch. That leaves two services, the bridge and the keeper. Each runs as
a user of its own, and neither runs as root. The bridge starts the
keeper, so the bridge is still the one to enable.

Other changes:

- Lighting restore is gone. It was off by default, and your apps keep
  their own lighting settings.
- Batteries appear sooner. The bridge asks each device for its battery at
  start and after resume, instead of waiting for the device to report it.
  An old reading stays in the battery widget, marked stale, rather than
  disappearing.
- All the checked devices are offered to apps, labelled
  `not yet tested through the receiver`. 2.x offered the two tested
  devices and no others.
- On Ubuntu, Debian, Fedora and openSUSE, OpenRazer's group is now
  `plugdev`, because that is the group their OpenRazer expects. 2.x used
  `openrazer` everywhere. Arch keeps `openrazer`. To join the new group,
  run this and then log in again:

  ```sh
  sudo gpasswd -a "$USER" plugdev
  ```

- Error codes now run from `HFX-1xx` to `HFX-4xx`, and each one has a
  section in [troubleshooting](troubleshooting.md). Two codes that the
  2.x docs mentioned often have new numbers: `HFX-INTEGRATION-007` is now
  HFX-406, and `HFX-INTEGRATION-008` is HFX-407.
- `hyperfluxctl` gains `battery`, `watch`, `bench`, `capture`,
  `stall-check`, `report` and `doctor --fix`. It also has `--json` on
  all commands, shell completions and a man page. `capture`
  replaces `qualification`.
- Apps speak protocol 1. The OpenRGB plugin in Full is updated with the
  rest. A third-party program written for the 2.x protocol will get
  HFX-101 until it is updated.

## Going back to 2.2.0

To go back, first remove the `hyperflux-next-v2-*` packages. They
replace the old names, and package managers will not install 2.2.0 next to
them.

Then install all four 2.2.0 packages (`hyperflux-next-dkms` included)
from the old repository or your package cache. Nowhere else has 2.2.0.
If it is not in your cache, point your system back at the old
repository first. Reboot afterwards.

If the switch goes wrong in a way Doctor doesn't explain, please open a
bug report. [Asking for help](troubleshooting.md#asking-for-help) says
what to include.
