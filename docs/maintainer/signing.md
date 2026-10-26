# Signing

One OpenPGP key, the project signing key, signs everything users install:

- the Arch packages and database
- the APT `Release` files
- the RPMs and their metadata
- the site's `SHA256SUMS`

CI never sees it. The one thing that uses it is `scripts/repo-sign.sh`,
run on the maintainer's machine.

The key, "HyperFlux Next V2 packages", was made on 2026-10-25:

| Key | Fingerprint | Use | Expires |
| --- | --- | --- | --- |
| primary | `2068 74CA DD1B 64B3 2483  E2A8 5FC5 921D E97A F7AA` | certifies only | 2029-10-24 |
| subkey | `69F3 5BC0 4765 FB65 A86C  D4C9 3E3C 3891 CB26 21D9` | signs | 2028-10-24 |

Both are ed25519. They live in a `GNUPGHOME` used for nothing else. The
primary key belongs offline, with a backup of that directory. The
subkey's fingerprint is `SIGNING_KEY` in `packaging/repository.conf`, and
the exported public key is
`packaging/keys/hyperflux-next-repository-key.asc`. List both
fingerprints in each release's notes. The install guides tell people to
compare them there.

`repo-sign.sh` needs `gpg`, `repo-add` (pacman), `rpmsign` and podman or
docker. It uses whichever `GNUPGHOME` is set, always with the exact
subkey (`--local-user FINGERPRINT!`) and SHA-512 digests. The RPMs are
signed first and their metadata is rebuilt afterwards, because signing
changes the files.

`repo-verify.sh` imports nothing but the public key from the repository,
into a throwaway keyring and RPM database. Then it checks all the
signatures, and the checksums the signed indexes vouch for. Run it before
publishing (`repo-publish.sh` runs it again too).

## Expiry and rotation

The key has an expiry date, in the table above. Extend it well before it
passes, using the primary key. `gpg --quick-set-expire FINGERPRINT 2y '*'`
extends the subkeys, and a second run without `'*'` extends the primary
key. Commit the new public key, publish a release, and tell users to
fetch the key again. They have to, because pacman, apt, dnf and zypper
don't refresh it by themselves:

- Arch: `sudo pacman-key --add` and `--lsign-key` again
- Ubuntu and Debian: the `curl` line of the install guide
- Fedora and openSUSE: `sudo rpm --import` of the new file

Replacing the key works the same way, except that users must trust the
new one before the first release signed with it. Publish it one release
ahead, with both fingerprints in that release's notes.
