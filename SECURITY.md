# Security

Please report vulnerabilities privately through
[GitHub's security advisories](https://github.com/hyperflux-next/hyperflux-next-v2/security/advisories/new),
not in a public issue, with the version (`hyperfluxctl --version`), your
distribution and the steps to reproduce it. You'll get an answer within
seven days.

Fixes go into the latest release only. They are built for every
distribution in the [install list](README.md#install) and reach users
with their normal system update.

Report anything that lets a local user or app get past the bridge's
checks. That includes:

- sending reports that the device's command list does not allow
- controlling a device without a lease
- reaching the receiver without going through the bridge
- creating virtual input devices
- affecting another user's socket
- getting key presses out of the device keeper

[ARCHITECTURE.md](ARCHITECTURE.md#7-privilege-model) describes what the
bridge and the keeper are each allowed to do, and what limits them.
