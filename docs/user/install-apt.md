# Install on Ubuntu or Debian

This guide covers Ubuntu 26.10 (stonking) and Debian 14 (forky). On
Debian, Full includes the OpenRGB plugin, for Debian's own OpenRGB 1.0.
On Ubuntu the plugin is optional and works with OpenRGB 1.1 from
openrgb.org: see [the OpenRGB page](#the-openrgb-page).

Older releases aren't supported. Ubuntu 26.04 LTS and Debian 13 package
OpenRazer versions that lack some of the supported keyboards, and Debian
13 has no OpenRGB. Debian 14 is still Debian's testing release, and its
own packages change more often than in a stable release.

You need x86_64, the HyperFlux receiver plugged in with your keyboard and
mouse paired to it, and an account that can use `sudo`. Keep the
receiver, keyboard and mouse connected the whole time.

## 1. Add the repository

You do this once. The HyperFlux Next V2 project runs the repository, not
your distribution. Its package lists are signed with the project signing
key, and apt rejects them without it.

```sh
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl --proto '=https' --tlsv1.2 -fL -o /etc/apt/keyrings/hyperflux-next.asc \
  https://hyperflux-next.github.io/hyperflux-next-v2/hyperflux-next-repository-key.asc
gpg --show-keys --with-subkey-fingerprints /etc/apt/keyrings/hyperflux-next.asc
```

Compare the fingerprints with the ones in the notes of the
[latest release](https://github.com/hyperflux-next/hyperflux-next-v2/releases/latest).
If they differ, stop and remove the file with
`sudo rm /etc/apt/keyrings/hyperflux-next.asc`.

Then add the repository. Use `ubuntu` or `debian` in the address:

```sh
sudo curl --proto '=https' --tlsv1.2 -fL -o /etc/apt/sources.list.d/hyperflux-next.sources \
  https://hyperflux-next.github.io/hyperflux-next-v2/ubuntu/hyperflux-next.sources
```

The file has six lines. On Debian the address ends in `/debian` and the
suite is `forky`:

```text
Types: deb
URIs: https://hyperflux-next.github.io/hyperflux-next-v2/ubuntu
Suites: stonking
Components: main
Architectures: amd64
Signed-By: /etc/apt/keyrings/hyperflux-next.asc
```

## 2. Install

Full makes the devices work in OpenRazer apps such as Polychromatic, and
on Debian in OpenRGB too. It brings Core with it. Core is just the
bridge, its device keeper and `hyperfluxctl`. With Core alone, batteries
show on the desktop but lighting apps can't reach the devices. If you
aren't sure, take Full.

Full builds OpenRazer's kernel drivers with DKMS, which needs your
kernel's headers. `linux-headers-generic` on Ubuntu and
`linux-headers-amd64` on Debian follow the standard kernels. Core needs
none.

```sh
sudo apt update
sudo apt install hyperflux-next-v2-full linux-headers-generic   # Debian: linux-headers-amd64
```

For Core, install `hyperflux-next-v2-linux` instead.

If your distribution's `openrazer-driver-dkms` is installed, apt
replaces it. Full brings its own build of the same driver, and the stock
`openrazer-daemon` works with it.

## 3. Enable and reboot

Installing starts nothing. Enable the bridge and your desktop's socket
for it, then reboot:

```sh
sudo systemctl enable hyperflux-next-bridge.service "hyperflux-next-client@$(id -u).socket"
sudo reboot
```

With Full and Secure Boot on, enrol the DKMS key before you reboot, as
[Secure Boot](secure-boot.md) shows. Until you do, the OpenRazer drivers
will not load.

Other desktop users each need their own
`hyperflux-next-client@UID.socket`.

## 4. Check

After you log in again, run `hyperfluxctl doctor`. On Debian with Full
it prints:

```text
HyperFlux Next V2 is ready.
Driver active, bridge running, 1 receiver.
No action required.
```

On Ubuntu, Full adds `Optional OpenRGB plugin is not installed.` until
you set up [the OpenRGB page](#the-openrgb-page). Core adds
`Optional Full integration is not installed.` Both are expected. For
anything else, do what Doctor's `Do:` line says, or look up its code in
[troubleshooting](troubleshooting.md).

## Apps

The apps are separate projects with their own packages:

```sh
sudo apt install openrazer-daemon python3-openrazer   # Ubuntu and Debian
sudo apt install openrgb                             # Debian only
sudo gpasswd -a "$USER" plugdev
```

OpenRazer talks to members of `plugdev` and nobody else, so log out and
back in after joining. Polychromatic is not packaged in Ubuntu 26.10 or
Debian 14. Its website, polychromatic.app, lists other ways to install
it. Then see [first run](first-run.md).

## The OpenRGB page

The plugin shows the devices in OpenRGB and adds a HyperFlux Next V2 page
with their batteries. Nothing else needs it.

On Debian, Full brings the plugin and Debian's own `openrgb` loads it.
Keep that `openrgb`. The plugin is built for OpenRGB 1.0, and the 1.1
package from openrgb.org does not load it.

Ubuntu's own OpenRGB is too old for the plugin. Get OpenRGB 1.1 from
[openrgb.org](https://openrgb.org/releases.html) instead. You need the
Debian Trixie package for x86_64, `openrgb_1.1_amd64_trixie_8e16de2.deb`,
and the two Trixie hidapi-hotplug packages that the page links to:

- `libhidapi-hotplug-hidraw0_0.15.0-1_trixie_amd64.deb`
- `libhidapi-hotplug-libusb0_0.15.0-1_trixie_amd64.deb`

The Bookworm package fails to install on Ubuntu 26.10. In the folder you
saved the files to, run:

```sh
sudo apt install ./libhidapi-hotplug-*_trixie_amd64.deb ./openrgb_1.1_amd64_trixie_8e16de2.deb
sudo apt install hyperflux-next-v2-openrgb-plugin
```

If OpenRGB 1.1 is already there when you install Full, apt brings the
plugin along, and you can skip the second line. Without OpenRGB 1.1,
Full installs as usual and leaves the plugin out.

apt keeps that OpenRGB through updates, because Ubuntu's is an older
version. The plugin package is built for 1.1. If you install another
OpenRGB release later, apt removes the plugin.
