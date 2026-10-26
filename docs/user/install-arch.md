# Install on Arch Linux

You need Arch Linux on x86_64, the HyperFlux receiver plugged in with
your keyboard and mouse paired to it, and an account that can use `sudo`.
Keep the receiver, keyboard and mouse connected the whole time.

## 1. Add the repository

This is a one-time step. The repository belongs to the HyperFlux Next V2
project, not to Arch Linux. The packages and the package list are signed
with the project signing key.

Download the key and look at its fingerprints:

```sh
curl --proto '=https' --tlsv1.2 -fLO https://hyperflux-next.github.io/hyperflux-next-v2/hyperflux-next-repository-key.asc
gpg --show-keys --with-subkey-fingerprints hyperflux-next-repository-key.asc
```

Compare them with the fingerprints in the notes of the
[latest release](https://github.com/hyperflux-next/hyperflux-next-v2/releases/latest).
If they differ, stop here and delete the file. If they match, give the key
to pacman and trust it:

```sh
sudo pacman-key --add hyperflux-next-repository-key.asc
sudo pacman-key --lsign-key "$(gpg --with-colons --show-keys hyperflux-next-repository-key.asc | awk -F: '$1 == "fpr" {print $10; exit}')"
```

Then open `/etc/pacman.conf` as root (`sudoedit /etc/pacman.conf`) and
add this at the end:

```ini
[hyperflux-next]
SigLevel = Required DatabaseRequired TrustedOnly
Server = https://hyperflux-next.github.io/hyperflux-next-v2/arch
```

From now on `sudo pacman -Syu` brings HyperFlux Next V2 updates too.

## 2. Install

There are two editions. Full makes the devices work in OpenRazer apps
such as Polychromatic and in OpenRGB, and brings Core with it. Core is
just the bridge, its device keeper and `hyperfluxctl`. With Core alone,
batteries show on the desktop but lighting apps can't reach the devices.
If in doubt, take Full.

Full builds OpenRazer's kernel drivers with DKMS. DKMS needs the headers
of every kernel you boot: `linux-headers` for `linux`, `linux-lts-headers`
for `linux-lts`, and so on. Core needs no kernel headers.

```sh
sudo pacman -Syu linux-headers hyperflux-next-v2-full
```

For Core, install `hyperflux-next-v2-linux` instead.

If Arch's `openrazer-driver-dkms` is installed, pacman reports a conflict
with `hyperflux-next-v2-openrazer-driver-dkms` and asks whether to remove
it. Answer `y`. Full brings its own build of the same driver, and the
stock `openrazer-daemon` works with it.

## 3. Enable and reboot

The packages start nothing by themselves. Enable the bridge and your
desktop's socket for it, then reboot:

```sh
sudo systemctl enable hyperflux-next-bridge.service "hyperflux-next-client@$(id -u).socket"
sudo reboot
```

Other desktop users each need their own
`hyperflux-next-client@UID.socket`.

## 4. Check

After you log in again:

```sh
hyperfluxctl doctor
```

```text
HyperFlux Next V2 is ready.
Driver active, bridge running, 1 receiver.
No action required.
```

With Core, Doctor also says `Optional Full integration is not installed.`
If it says `HyperFlux Next V2 needs attention.`, do what its `Do:` line
says. [Troubleshooting](troubleshooting.md) explains each code.

Then set up your apps as [first run](first-run.md) describes.

## The OpenRGB page

Full brings the OpenRGB plugin. It shows the devices in OpenRGB and adds
a HyperFlux Next V2 page with their batteries. OpenRGB itself is
optional. Arch's own OpenRGB 1.1 loads the plugin, and
`sudo pacman -S openrgb` is all it takes. There is nothing to get from
openrgb.org.

## The packages

Full installs all four, Core just the first. None of them adds a driver
for the receiver itself. The bridge reaches it through the kernel's own
hidraw driver.

- `hyperflux-next-v2-linux`: the bridge, its device keeper,
  `hyperfluxctl`, units, users and udev rules (Core)
- `hyperflux-next-v2-openrazer-driver-dkms`: OpenRazer 3.12.4's drivers
  with the HyperFlux Next V2 patches, in place of `openrazer-driver-dkms`
- `hyperflux-next-v2-openrgb-plugin`: the OpenRGB plugin
- `hyperflux-next-v2-full`: the OpenRazer rules and module list (Full)

HyperFlux Next V2 doesn't install or replace `openrazer-daemon`,
Polychromatic or OpenRGB. Full needs the other three packages at exactly
its own version, so pacman updates them together.

## Without the repository

Each release has the Arch packages and their signatures on its
[release page](https://github.com/hyperflux-next/hyperflux-next-v2/releases).
Download the four `arch-*.pkg.tar.zst` files and their `.sig` files into
one directory. For Core, you need just `hyperflux-next-v2-linux` and its
signature. Give pacman the key as in the first part of step 1. It then
checks each package's signature. Install them with:

```sh
sudo pacman -U ./arch-hyperflux-next-v2-*.pkg.tar.zst
```

Packages installed this way don't update with `pacman -Syu`. You can also
build them yourself: `packaging/arch/PKGBUILD` in the source tarball
works with plain `makepkg -si`.
