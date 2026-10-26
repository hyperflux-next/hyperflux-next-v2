# Updates and removal

## Updating

HyperFlux Next V2 updates come with your normal system update. Close
OpenRGB and Polychromatic first, then run the usual command:

- Arch: `sudo pacman -Syu`
- Ubuntu and Debian: `sudo apt update && sudo apt upgrade`
- Fedora: `sudo dnf upgrade`
- openSUSE: `sudo zypper dup`

An update never starts, stops or restarts the bridge or its device
keeper. To run the new version, reboot, or restart both yourself:

```sh
sudo systemctl restart hyperflux-next-bridge.service hyperflux-next-keeper.service
```

Then run `hyperfluxctl doctor`. When an update brings new OpenRazer
drivers, Doctor reports [HFX-401](troubleshooting.md#hfx-401) until you
reboot.

## Going back one version

The repository keeps the previous release next to the current one. All
the HyperFlux Next V2 packages have to be at the same version, so take
every one of them back and reboot. Replace `VERSION` with the version
you want.

- Arch: pacman keeps earlier packages in its cache.

  ```sh
  sudo pacman -U /var/cache/pacman/pkg/hyperflux-next-v2-{linux,openrazer-driver-dkms,full,openrgb-plugin}-VERSION-*.pkg.tar.zst
  ```

- Ubuntu and Debian:

  ```sh
  sudo apt install hyperflux-next-v2-{linux,openrazer-driver-dkms,full,openrgb-plugin}=VERSION-1
  ```

- Fedora:

  ```sh
  sudo dnf downgrade hyperflux-next-v2-linux hyperflux-next-v2-openrazer-driver-dkms hyperflux-next-v2-full hyperflux-next-v2-openrgb-plugin
  ```

- openSUSE:

  ```sh
  sudo zypper install --oldpackage hyperflux-next-v2-{linux,openrazer-driver-dkms,full}=VERSION
  ```

Name the packages you have. With Core that is `linux` alone. On
Ubuntu and Fedora, `openrgb-plugin` is there only if you installed it.

## Removing Full and keeping Core

Core came in as a dependency of Full, so first mark it as a package you
want. Otherwise removing Full takes Core with it on Arch and Fedora. On
Ubuntu and Debian, the next `apt autoremove` does the same.

- Arch:

  ```sh
  sudo pacman -D --asexplicit hyperflux-next-v2-linux
  sudo pacman -Rs hyperflux-next-v2-full
  ```

- Ubuntu and Debian:

  ```sh
  sudo apt-mark manual hyperflux-next-v2-linux
  sudo apt purge hyperflux-next-v2-full hyperflux-next-v2-openrazer-driver-dkms
  ```

- Fedora:

  ```sh
  sudo dnf mark user hyperflux-next-v2-linux
  sudo dnf remove hyperflux-next-v2-full hyperflux-next-v2-openrazer-driver-dkms
  ```

- openSUSE:

  ```sh
  sudo zypper remove hyperflux-next-v2-full hyperflux-next-v2-openrazer-driver-dkms
  ```

After that, apps can't reach the receiver's devices. If you use
OpenRazer, install your distribution's own driver again:
`openrazer-driver-dkms` on Arch, Ubuntu and Debian, or
`openrazer-kernel-modules-dkms` from `hardware:razer` on Fedora and
openSUSE.

## Removing HyperFlux Next V2

Turn the services off first. Removing the packages doesn't do that on
every distribution.

```sh
sudo systemctl disable --now hyperflux-next-bridge.service "hyperflux-next-client@$(id -u).socket"
sudo systemctl stop hyperflux-next-keeper.service
```

Then remove the packages. With Core, leave out the first two names.

- Arch:

  ```sh
  sudo pacman -Rs hyperflux-next-v2-full hyperflux-next-v2-openrazer-driver-dkms hyperflux-next-v2-linux
  ```

- Ubuntu and Debian:

  ```sh
  sudo apt purge hyperflux-next-v2-full hyperflux-next-v2-openrazer-driver-dkms hyperflux-next-v2-linux
  ```

- Fedora:

  ```sh
  sudo dnf remove hyperflux-next-v2-full hyperflux-next-v2-openrazer-driver-dkms hyperflux-next-v2-linux
  ```

- openSUSE:

  ```sh
  sudo zypper remove hyperflux-next-v2-full hyperflux-next-v2-openrazer-driver-dkms hyperflux-next-v2-linux
  ```

The DKMS copy of the OpenRazer drivers goes with its package. OpenRGB,
OpenRazer and Polychromatic stay, because they are separate packages.

The `hyperflux-next` and `hyperflux-keeper` users stay too, as do the
`hyperflux-next`, `hyperflux-keeper` and `hyperflux` groups.
Distributions keep system users.

`/var/lib/hyperflux-next` is where the bridge keeps how long each device
takes to answer. `apt purge` deletes it. On the other distributions it
stays, and `sudo rm -r /var/lib/hyperflux-next` removes it.

## Removing the repository

After the packages are gone:

- Arch: delete the `[hyperflux-next]` section from `/etc/pacman.conf`,
  then remove the key. `pacman-key --list-keys` shows its fingerprint.

  ```sh
  sudo pacman-key --delete FINGERPRINT
  ```

- Ubuntu and Debian:

  ```sh
  sudo rm /etc/apt/sources.list.d/hyperflux-next.sources /etc/apt/keyrings/hyperflux-next.asc
  sudo apt update
  ```

- Fedora: `sudo rm /etc/yum.repos.d/hyperflux-next.repo`
- openSUSE: `sudo zypper removerepo hyperflux-next`

On Fedora and openSUSE the key stays in the RPM database.
`rpm -q gpg-pubkey --qf '%{NAME}-%{VERSION}-%{RELEASE} %{SUMMARY}\n'`
lists the imported keys. Remove the HyperFlux Next V2 one with
`sudo rpm -e` and its name.
