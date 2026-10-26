# SPDX-License-Identifier: GPL-2.0-only
#
# System files of the linux, full and openrgb-plugin packages, package
# builds and the package repository. Paths come from the top-level Makefile.

PACKAGE_TARGETS := arch ubuntu debian fedora opensuse

.PHONY: install-linux-files install-full-files install-openrgb-plugin-files crate-notices dist \
	lint-packaging $(addprefix package-,$(PACKAGE_TARGETS)) \
	$(addprefix install-test-,$(PACKAGE_TARGETS))

install-linux: install-linux-files
install-full: install-full-files
install-openrgb-plugin: install-openrgb-plugin-files

install-linux-files:
	install -Dm0644 -t $(DESTDIR)$(SYSTEMDUNITDIR) systemd/hyperflux-next-bridge.service \
		systemd/hyperflux-next-keeper.service systemd/hyperflux-next-client@.socket \
		systemd/hyperflux-next-client@.service
	install -Dm0644 packaging/files/hyperflux-next.sysusers $(DESTDIR)$(SYSUSERSDIR)/hyperflux-next.conf
	install -Dm0644 udev/99-hyperflux-next.rules $(DESTDIR)$(UDEVRULESDIR)/99-hyperflux-next.rules
	install -Dm0644 packaging/files/hyperflux-next.modules-load $(DESTDIR)$(MODULESLOADDIR)/hyperflux-next.conf
	install -Dm0644 docs/user/troubleshooting.md $(DESTDIR)$(DOCDIR)/troubleshooting.md

install-full-files:
	install -d $(DESTDIR)$(COMPONENTDIR)
	sed -e 's/@VERSION@/$(VERSION)/' packaging/files/full.json.in >$(DESTDIR)$(COMPONENTDIR)/full.json
	chmod 0644 $(DESTDIR)$(COMPONENTDIR)/full.json

install-openrgb-plugin-files:
	install -Dm0644 -t $(DESTDIR)$(LICENSEDIR)/hyperflux-next-v2-openrgb-plugin \
		integrations/openrgb/LICENSE-nlohmann-json

# Notices of the crates compiled into the binaries, which recipes install
# with the package's other license files. cargo metadata reads every crate
# in Cargo.lock, test-only ones too, hence the fetch.
crate-notices: target/LICENSE-rust-crates
target/LICENSE-rust-crates: Cargo.lock scripts/crate-notices.py
	mkdir -p target
	$(CARGO) fetch $(CARGO_FLAGS)
	$(PYTHON) scripts/crate-notices.py >$@.tmp
	mv $@.tmp $@

# Source tarball of the working tree, named and laid out like GitHub's
# archive of the release tag, which is what the recipes download.
dist:
	scripts/package.sh dist

$(addprefix package-,$(PACKAGE_TARGETS)): package-%:
	scripts/package.sh $*

$(addprefix install-test-,$(PACKAGE_TARGETS)): install-test-%:
	scripts/package.sh $* --install-test

# Units are checked in a scratch root with stand-ins for the programs they
# start. This works before anything is built or installed. Older
# systemd-analyze (255 on Ubuntu 24.04) also looks for the standard targets
# and slices in that root, and we copy the host's in. The install test
# resolves the rules' user and group names, which exist only once Core is
# installed.
lint-packaging:
	shellcheck -x scripts/*.sh packaging/debian/*.postinst packaging/debian/*.prerm packaging/debian/*.postrm
	udevadm verify --no-style --resolve-names=late udev/99-hyperflux-next.rules
	root=$$(mktemp -d) && trap 'rm -rf "$$root"' EXIT && \
	$(MAKE) -s install-linux-files DESTDIR="$$root" && \
	install -D /bin/true "$$root$(LIBEXECDIR)/hyperflux-next-bridge" && \
	install -D /bin/true "$$root$(LIBEXECDIR)/hyperflux-next-keeper" && \
	install -D /bin/true "$$root$(LIBDIR)/systemd/systemd-socket-proxyd" && \
	for unit in socket service; do \
		cp "$$root$(SYSTEMDUNITDIR)/hyperflux-next-client@.$$unit" "$$root$(SYSTEMDUNITDIR)/hyperflux-next-client@1000.$$unit"; \
	done && \
	for unit in $(SYSTEMDUNITDIR)/*.target $(SYSTEMDUNITDIR)/*.slice; do \
		[ -e "$$root$$unit" ] || cp "$$unit" "$$root$$unit"; \
	done && \
	systemd-analyze verify --man=no --root="$$root" $(SYSTEMDUNITDIR)/hyperflux-next-bridge.service \
		$(SYSTEMDUNITDIR)/hyperflux-next-keeper.service \
		$(SYSTEMDUNITDIR)/hyperflux-next-client@1000.socket $(SYSTEMDUNITDIR)/hyperflux-next-client@1000.service
