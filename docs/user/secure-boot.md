# Secure Boot

This page is about Full. Core adds no kernel module: the bridge reaches
the receiver through the kernel's own hidraw driver, and Secure Boot has
no effect on that.

With Secure Boot on, Ubuntu, Debian, Fedora and openSUSE load kernel
modules signed with a key the firmware trusts and no others, but DKMS
signs the OpenRazer drivers that Full installs with a key it made on your
computer. You tell the firmware to trust that key once. Doctor reports
[HFX-304](troubleshooting.md#hfx-304) when this step is missing.

1. Check whether Secure Boot is on:

   ```sh
   mokutil --sb-state
   ```

   If it prints `SecureBoot disabled`, there is nothing to do.

2. Ask the firmware to trust the key. Choose a one-time password when
   asked. You type it once more after the restart.

   - Ubuntu: `sudo update-secureboot-policy --enroll-key`. If it says
     there is no key to enrol, use
     `sudo mokutil --import /var/lib/shim-signed/mok/MOK.der` instead.
   - Debian, Fedora and openSUSE:
     `sudo mokutil --import /var/lib/dkms/mok.pub`

3. Reboot. A blue MokManager screen appears before the system starts.
   Choose "Enroll MOK", "Continue" and "Yes", type the password, then
   choose "Reboot".

4. Check that the drivers are signed and loaded:

   ```sh
   modinfo -F signer razerkbd
   hyperfluxctl doctor
   ```

   `modinfo` prints the key's name. An empty line means the module is not
   signed. Reinstall `hyperflux-next-v2-openrazer-driver-dkms` with your
   package manager, which makes DKMS build and sign it again, and reboot.

The key stays enrolled. Later driver updates are signed with the same key
and load without another visit to MokManager.

Arch's own kernels don't require signed modules, with or without Secure
Boot. On Arch there is nothing to do unless you set up module signing
yourself.
