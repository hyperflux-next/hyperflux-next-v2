# Install on Fedora or openSUSE

The packages are built for Fedora 44 and 45, and openSUSE Tumbleweed, on
x86_64.
Full works with OpenRazer apps such as Polychromatic. Neither
distribution ships an OpenRGB that can load the HyperFlux Next V2 plugin
(both package a 1.0 release candidate), but on Fedora, OpenRGB 1.1 from
openrgb.org can, as [the OpenRGB page](#the-openrgb-page) shows.

Distributions based on Fedora 44, 45 or Tumbleweed can use the same
packages.
On image-based ones, such as Bazzite or openSUSE Aeon, you can layer Core
but not Full: see [image-based systems](#image-based-systems).

You need the HyperFlux receiver plugged in with your keyboard and mouse
paired to it, and an account that can use `sudo`. Keep the receiver,
keyboard and mouse connected the whole time.

Steps 1 to 3 can also be done by a script:

```sh
curl -fsSL https://hyperflux-next.github.io/hyperflux-next-v2/install.sh | sh
```

Along with Full it installs `kernel-devel` on Fedora and
`kernel-default-devel` on openSUSE, where it also enables `dkms.service`
(step 3 says why). dnf asks whether to import the project key for the
package list, as step 2 explains: answer `y`. If
OpenRazer's own `openrazer-kernel-modules-dkms` is installed, dnf replaces
it and zypper asks you how to solve the conflict. For Core, end the line
with `sh -s -- --core`.

## 1. Add the repository

You do this once. The repository is the HyperFlux Next V2 project's own,
and the packages and the package list are signed with its signing key.

```sh
curl --proto '=https' --tlsv1.2 -fLO https://hyperflux-next.github.io/hyperflux-next-v2/hyperflux-next-repository-key.asc
gpg --show-keys --with-subkey-fingerprints hyperflux-next-repository-key.asc
```

Compare the fingerprints with the ones in the notes of the
[latest release](https://github.com/hyperflux-next/hyperflux-next-v2/releases/latest).
If they differ, stop and delete the file. If they match:

```sh
sudo rpm --import hyperflux-next-repository-key.asc
```

On Fedora:

```sh
sudo curl --proto '=https' --tlsv1.2 -fL -o /etc/yum.repos.d/hyperflux-next.repo \
  https://hyperflux-next.github.io/hyperflux-next-v2/fedora/hyperflux-next.repo
```

On openSUSE:

```sh
sudo zypper addrepo https://hyperflux-next.github.io/hyperflux-next-v2/opensuse/hyperflux-next.repo
sudo zypper refresh
```

The Fedora file looks like this. openSUSE's points at
`opensuse/tumbleweed/$basearch/` and adds `autorefresh=1`.

```ini
[hyperflux-next]
name=HyperFlux Next V2 for Fedora
baseurl=https://hyperflux-next.github.io/hyperflux-next-v2/fedora/$releasever/$basearch/
type=rpm-md
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=https://hyperflux-next.github.io/hyperflux-next-v2/hyperflux-next-repository-key.asc
```

## 2. Install

Full makes the devices work in OpenRazer apps and brings Core with it.
Core is just the bridge, its device keeper and `hyperfluxctl`. With Core
alone, batteries show on the desktop but lighting apps can't reach the
devices. If you aren't sure, take Full.

Full builds OpenRazer's kernel drivers with DKMS, which needs your
kernel's headers: `kernel-devel` on Fedora, `kernel-default-devel` on
openSUSE. Core needs none.

On Tumbleweed, bring the system up to date first with `sudo zypper dup`,
and reboot if that brought a new kernel. Its repository carries headers
for the newest kernel and no other. On a system that is behind, DKMS
builds the driver for a kernel you aren't running, and Doctor reports
[HFX-401](troubleshooting.md#hfx-401).

```sh
sudo dnf install hyperflux-next-v2-full kernel-devel                # Fedora
sudo zypper install hyperflux-next-v2-full kernel-default-devel     # openSUSE
```

For Core, install `hyperflux-next-v2-linux` instead.

The first time, dnf shows the project key again and asks whether to
import it, this time for the package list. Its fingerprint is the primary
one you compared in step 1,
`2068 74CA DD1B 64B3 2483 E2A8 5FC5 921D E97A F7AA`. Answer `y`.

If OpenRazer's own `openrazer-kernel-modules-dkms` is installed, the two
packages conflict. On Fedora, run
`sudo dnf install --allowerasing hyperflux-next-v2-full`. On openSUSE,
pick the solution that removes `openrazer-kernel-modules-dkms`. Full
brings its own build of the same driver.

## 3. Enable and reboot

Installing starts nothing. With Full and Secure Boot on, first enrol the
DKMS key, as [Secure Boot](secure-boot.md) shows. Then enable the bridge
and your own socket. On Fedora, or on openSUSE with Core:

```sh
sudo systemctl enable hyperflux-next-bridge.service "hyperflux-next-client@$(id -u).socket"
```

With Full on openSUSE, enable `dkms.service` too. openSUSE ships it
turned off, and it is what installs the OpenRazer drivers for your kernel
at boot:

```sh
sudo systemctl enable dkms.service hyperflux-next-bridge.service "hyperflux-next-client@$(id -u).socket"
```

Then reboot:

```sh
sudo reboot
```

Other desktop users each need their own
`hyperflux-next-client@UID.socket`.

## 4. Check

After you log in again, run `hyperfluxctl doctor`. With Full:

```text
HyperFlux Next V2 is ready.
Driver active, bridge running, 1 receiver.
Optional OpenRGB plugin is not installed.
No action required.
```

On Fedora, the third line goes away once you have the OpenRGB plugin.
With Core it reads `Optional Full integration is not installed.` For
anything else, do what Doctor's `Do:` line says, or look up its code in
[troubleshooting](troubleshooting.md).

## Image-based systems

Bazzite, Fedora Silverblue and Kinoite, and openSUSE Aeon and Kalpa keep
`/usr` read-only and add packages on top of the system image. Core layers
on them. Full does not, as DKMS cannot build its drivers into a read-only
`/usr`, and OpenRazer apps such as Polychromatic get no devices there. You
still get the batteries in the desktop, `hyperfluxctl` and Doctor. These
steps were tested on Fedora CoreOS 44 and openSUSE MicroOS, which layer
packages the same way. On the Fedora ones the OpenRGB plugin works too, with
the packages from [the OpenRGB page](#the-openrgb-page) layered by
`rpm-ostree install` in place of `dnf install`.

The script above layers Core on them by itself. By hand, check the key as
in step 1 first.

On Bazzite, Silverblue and Kinoite, rpm-ostree reads the key from a file:

```sh
sudo install -m 0644 hyperflux-next-repository-key.asc /etc/pki/rpm-gpg/RPM-GPG-KEY-hyperflux-next
curl --proto '=https' --tlsv1.2 -fsSL https://hyperflux-next.github.io/hyperflux-next-v2/fedora/hyperflux-next.repo |
  sed 's|^gpgkey=.*|gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-hyperflux-next|' |
  sudo tee /etc/yum.repos.d/hyperflux-next.repo >/dev/null
sudo rpm-ostree install --apply-live hyperflux-next-v2-linux
sudo systemctl enable hyperflux-next-bridge.service "hyperflux-next-client@$(id -u).socket"
sudo reboot
```

A layered package must exist for the new release when the system moves to
one, or the whole update stops. The site has packages for each new Fedora
before it is released. If an update still stops on
`hyperflux-next-v2-linux`, take it off, update, and layer it again later:

```sh
sudo rpm-ostree uninstall hyperflux-next-v2-linux
```

`--apply-live` makes Core usable at once, and its units can be enabled
before the reboot.

On Aeon and Kalpa, everything happens in a new snapshot, which starts at
the next boot. Note your user id first (`id -u`), because inside the
snapshot's shell you are root:

```sh
sudo transactional-update shell
```

In the shell that opens, with your own id in place of 1000:

```sh
zypper addrepo https://hyperflux-next.github.io/hyperflux-next-v2/opensuse/hyperflux-next.repo
zypper refresh
zypper install hyperflux-next-v2-linux
systemctl enable hyperflux-next-bridge.service hyperflux-next-client@1000.socket
exit
```

`zypper refresh` shows the key's fingerprint and asks whether to trust
it. Trust it if it matches the release notes, and stop otherwise.
Then reboot, and check as in step 4: Doctor's third line says the Full
integration isn't installed.

## Apps

Fedora and openSUSE don't package OpenRazer. Its developers publish it,
along with Polychromatic, in the `hardware:razer` project on the openSUSE
Build Service:

```sh
# Fedora
sudo dnf config-manager addrepo --from-repofile=https://download.opensuse.org/repositories/hardware:/razer/Fedora_$(rpm -E %fedora)/hardware:razer.repo
sudo dnf install openrazer-daemon python3-openrazer polychromatic

# openSUSE
sudo zypper addrepo https://download.opensuse.org/repositories/hardware:/razer/openSUSE_Tumbleweed/hardware:razer.repo
sudo zypper install openrazer-daemon python3-openrazer polychromatic
```

`openrazer-daemon` installs on top of HyperFlux Next V2's driver package.
OpenRazer talks to members of `plugdev` and nobody else. The driver
package creates that group. Run `sudo gpasswd -a "$USER" plugdev`, then
log out and back in. After that, see [first run](first-run.md).

## The OpenRGB page

The plugin shows the devices in OpenRGB and adds a HyperFlux Next V2 page
with their batteries. Nothing else needs it.

On Fedora it needs OpenRGB 1.1 from
[openrgb.org](https://openrgb.org/releases.html). Get the Fedora 44
package, `openrgb_1.1_x86_64_f44_8e16de2.rpm`, and the hidapi-hotplug
package that the page links to, `hidapi-hotplug-0.15.0-3.fc43.x86_64.rpm`.
Both are builds for older Fedora releases and work on 44 and 45. In the
folder you saved them to, run:

```sh
sudo dnf install ./hidapi-hotplug-0.15.0-3.fc43.x86_64.rpm ./openrgb_1.1_x86_64_f44_8e16de2.rpm
sudo dnf install hyperflux-next-v2-openrgb-plugin
```

If OpenRGB 1.1 is already there when you install Full, dnf brings the
plugin along, and you can skip the second line.

dnf keeps that OpenRGB through updates, because Fedora's is an older
version. The plugin package is built for 1.1. Remove the plugin before
you install another OpenRGB release.

openSUSE has no OpenRGB that loads the plugin. The Fedora package asks
for libraries under Fedora's package names and won't install. The
Flatpak and the AppImage from openrgb.org bring their own, older Qt, and
they turn the plugin down.
