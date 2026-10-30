# Contributing

Bug reports, device reports and pull requests are welcome.

Work happens on `dev`. `main` moves when CI is green, and not before.
Branch from `dev` and keep each pull request to one change. If the change
alters behaviour that a docs page describes, update that page in the same
commit.

Before pushing:

```sh
make check            # rustfmt, clippy, cargo tests, Python cross-checks, fragment tests
make lint-packaging   # shellcheck, unit and udev rule checks
```

The OpenRazer patches and the OpenRGB plugin have their own targets
(`make openrazer-test KDIR=...`, `make openrgb-plugin`).
[ARCHITECTURE.md](ARCHITECTURE.md#9-build-and-test) lists them. It also
shows how to run the whole stack against the simulator, with no hardware.
To build and test the packages for one distribution in its container,
run `scripts/package.sh TARGET --install-test`.

Some files are contracts between parts. `docs/protocol.md` goes with the
fixtures in `crates/hfx-proto/tests/fixtures/`, and `devices.toml` has
copies in `crates/hfx-proto`, the OpenRazer patch and the OpenRGB plugin.
Change them together with whatever depends on them. The tests in `tests/`
check that the copies agree with `devices.toml`, and that it matches the
pinned OpenRazer and OpenRGB.

The docs quote some numbers from the code and the pins, such as how long a
battery reading stays fresh or which Ubuntu release is supported.
`tests/test_docs.py` lists each one, and fails until the page and its
source say the same.

A new crate needs a line in the root `Cargo.toml` saying why. Please
don't paste serial numbers, receiver captures from someone else's
machine, or private paths into issues, tests or commits.

By contributing you agree that your work is distributed under the
project's licence, GPL-2.0-only.
