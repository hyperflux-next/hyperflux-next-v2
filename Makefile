# SPDX-License-Identifier: GPL-2.0-only
#
# Build, test and install HyperFlux Next V2.
#
# Install paths and build switches are defined here and nowhere else.
# Each component adds its own targets in a fragment that is included below;
# the fragments use these variables and never define paths of their own.
# Package recipes call the install-<package> targets with DESTDIR set.

VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)

DESTDIR ?=
PREFIX ?= /usr
BINDIR ?= $(PREFIX)/bin
LIBDIR ?= $(PREFIX)/lib
LIBEXECDIR ?= $(LIBDIR)/hyperflux-next
DATADIR ?= $(PREFIX)/share
MANDIR ?= $(DATADIR)/man
DOCDIR ?= $(DATADIR)/doc/hyperflux-next
LICENSEDIR ?= $(DATADIR)/licenses
COMPONENTDIR ?= $(DATADIR)/hyperflux-next/components
SYSTEMDUNITDIR ?= $(LIBDIR)/systemd/system
SYSUSERSDIR ?= $(LIBDIR)/sysusers.d
UDEVDIR ?= $(LIBDIR)/udev
UDEVRULESDIR ?= $(UDEVDIR)/rules.d
MODULESLOADDIR ?= $(LIBDIR)/modules-load.d
DKMSSRCDIR ?= /usr/src
OPENRGBPLUGINDIR ?= $(LIBDIR)/openrgb/plugins
BASHCOMPDIR ?= $(DATADIR)/bash-completion/completions
ZSHCOMPDIR ?= $(DATADIR)/zsh/site-functions
FISHCOMPDIR ?= $(DATADIR)/fish/vendor_completions.d

# Group that owns OpenRazer's sysfs files and udev nodes. Arch packages
# OpenRazer with "openrazer"; Debian, Ubuntu, Fedora and openSUSE use the
# upstream "plugdev". Package recipes set it; hyperfluxctl is built with the
# same value (HFX_OPENRAZER_GROUP) so Doctor checks the right group.
OPENRAZER_GROUP ?= plugdev
# Qt major version of the OpenRGB the plugin is built for: 6 for OpenRGB
# 1.1, 5 for 1.0.
OPENRGB_QT ?= 6

PROJECT_URL ?= https://github.com/hyperflux-next/hyperflux-next-v2
SITE_URL ?= https://hyperflux-next.github.io/hyperflux-next-v2

CARGO ?= cargo
CARGO_FLAGS ?= --locked
RUST_TARGET_DIR ?= target/release
PYTHON ?= python3

# Upstream pins: KEY=value lines that both make and sh can read.
-include integrations/openrazer/PIN
-include integrations/openrgb/PIN

export VERSION OPENRAZER_GROUP PROJECT_URL

.PHONY: all build rust check test lint \
	install-linux install-openrazer-dkms install-full install-openrgb-plugin install-rust-linux

all: build

build: rust

rust:
	HFX_OPENRAZER_GROUP=$(OPENRAZER_GROUP) $(CARGO) build --release $(CARGO_FLAGS) --workspace

lint:
	$(CARGO) fmt --all --check
	$(CARGO) clippy $(CARGO_FLAGS) --workspace --all-targets -- -D warnings

# Fragments append their own test targets to this list.
TEST_TARGETS := test-rust test-tables
.PHONY: test-rust test-tables
test-rust:
	$(CARGO) test $(CARGO_FLAGS) --workspace
test-tables:
	$(PYTHON) -m pytest -q tests

# The packages, hyperflux-next-v2-linux, -openrazer-driver-dkms, -full and
# -openrgb-plugin. Fragments add prerequisites to these.
install-linux: install-rust-linux
install-openrazer-dkms:
install-full:
install-openrgb-plugin:

install-rust-linux:
	install -Dm0755 -t $(DESTDIR)$(LIBEXECDIR) $(RUST_TARGET_DIR)/hyperflux-next-bridge \
		$(RUST_TARGET_DIR)/hyperflux-next-keeper
	install -Dm0755 $(RUST_TARGET_DIR)/hyperfluxctl $(DESTDIR)$(BINDIR)/hyperfluxctl

# Component fragments.
-include integrations/openrazer/openrazer.mk
-include integrations/openrgb/openrgb.mk
-include sdk/sdk.mk
-include crates/hyperfluxctl/cli.mk
-include packaging/packaging.mk

test: $(TEST_TARGETS)

check: lint test
