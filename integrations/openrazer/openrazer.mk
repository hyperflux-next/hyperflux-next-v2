# SPDX-License-Identifier: GPL-2.0-only
#
# OpenRazer 3.12.4 with the HyperFlux Next V2 transport: the patched source
# tree in .upstreams/openrazer, its tests, and the files the
# openrazer-driver-dkms and full packages install. Paths come from the
# top-level Makefile.

OPENRAZER_DIR := integrations/openrazer
OPENRAZER_TREE := .upstreams/openrazer
OPENRAZER_BUILD := .upstreams/openrazer-build
OPENRAZER_DKMS_TREE = $(DESTDIR)$(DKMSSRCDIR)/openrazer-driver-$(OPENRAZER_VERSION)
# Build against KDIR when it is given, otherwise the running kernel.
openrazer_kdir = $(or $(KDIR),/lib/modules/$(shell uname -r)/build)
openrazer_capability = $(shell sed -n 's/^pub const CAPABILITY_VALUE: &str = "\(.*\)";/\1/p' \
	crates/hfx-proto/src/openrazer.rs)

.PHONY: openrazer-tree openrazer-test install-openrazer-dkms-tree install-openrazer-full

install-openrazer-dkms: install-openrazer-dkms-tree
install-full: install-openrazer-full

# fetch.sh does nothing when the tree already matches the pin and patches.
openrazer-tree:
	sh $(OPENRAZER_DIR)/fetch.sh $(OPENRAZER_TREE)

# Needs the pinned archive (downloaded once), kernel headers and, for the
# rules, a udevadm that has the verify command. Every module has to call
# hid_is_usb(), which before 6.6 is inline and refers to usb_hid_driver,
# and serial_sha256 hashes with the kernel's SHA-256 library.
openrazer-test: openrazer-tree
	sh $(OPENRAZER_DIR)/tests/check-patches.sh $(OPENRAZER_TREE)
	rm -rf $(OPENRAZER_BUILD)
	cp -R $(OPENRAZER_TREE)/driver $(OPENRAZER_BUILD)
	$(MAKE) -C $(openrazer_kdir) M=$(abspath $(OPENRAZER_BUILD)) modules
	for module in razerkbd razermouse; do \
		test "$$(modinfo -F hyperflux_native_transport $(OPENRAZER_BUILD)/$$module.ko)" = "$(openrazer_capability)" && \
		modinfo -F parm $(OPENRAZER_BUILD)/$$module.ko | grep -q '^hyperflux_native_transport:' || \
		{ echo "$$module.ko does not carry the transport capability" >&2; exit 1; }; \
	done
	for module in razeraccessory razerkbd razerkraken razermouse; do \
		nm -u $(OPENRAZER_BUILD)/$$module.ko | grep -qE ' (hid_is_usb|usb_hid_driver)$$' && \
		nm -u $(OPENRAZER_BUILD)/$$module.ko | grep -q ' sha256$$' || \
		{ echo "$$module.ko lacks the USB check or the SHA-256 library" >&2; exit 1; }; \
	done
	for module in razerkbd razermouse; do \
		grep -q serial_sha256 $(OPENRAZER_BUILD)/$$module.ko || \
		{ echo "$$module.ko has no serial_sha256 attribute" >&2; exit 1; }; \
	done
	sh $(OPENRAZER_DIR)/tests/check-install.sh
	if udevadm verify --help >/dev/null 2>&1; then \
		udevadm verify --no-summary --resolve-names=late \
			$(OPENRAZER_DIR)/69-hyperflux-next-openrazer-access.rules \
			$(OPENRAZER_DIR)/70-hyperflux-next-openrazer-keys.rules \
			$(OPENRAZER_TREE)/install_files/udev/99-razer.rules; \
	else \
		echo "udevadm has no verify command; rules not checked"; \
	fi
	$(PYTHON) -m pytest -q tests/test_device_tables.py tests/test_upstream_tables.py -k openrazer

# The DKMS tree is upstream's, unchanged apart from the patches. Its DKMS
# name and version stay openrazer-driver 3.12.4. The group in the udev
# files is set here because distributions disagree on it.
install-openrazer-dkms-tree: openrazer-tree
	install -d $(OPENRAZER_DKMS_TREE)/driver $(OPENRAZER_DKMS_TREE)/LICENSES
	install -m0644 $(OPENRAZER_TREE)/Makefile $(OPENRAZER_DKMS_TREE)/Makefile
	install -m0644 $(OPENRAZER_TREE)/install_files/dkms/dkms.conf $(OPENRAZER_DKMS_TREE)/dkms.conf
	install -m0644 $(OPENRAZER_TREE)/LICENSES/*.txt $(OPENRAZER_DKMS_TREE)/LICENSES/
	install -m0644 $(OPENRAZER_TREE)/driver/Makefile $(OPENRAZER_TREE)/driver/*.c \
		$(OPENRAZER_TREE)/driver/*.h $(OPENRAZER_DKMS_TREE)/driver/
	rm -f $(OPENRAZER_DKMS_TREE)/driver/*.mod.c
	install -d $(DESTDIR)$(UDEVDIR) $(DESTDIR)$(UDEVRULESDIR) $(DESTDIR)$(SYSUSERSDIR)
	sed 's/GROUP:="plugdev"/GROUP:="$(OPENRAZER_GROUP)"/' $(OPENRAZER_TREE)/install_files/udev/99-razer.rules \
		>$(DESTDIR)$(UDEVRULESDIR)/99-razer.rules
	sed 's/chgrp -R plugdev /chgrp -R $(OPENRAZER_GROUP) /' $(OPENRAZER_TREE)/install_files/udev/razer_mount \
		>$(DESTDIR)$(UDEVDIR)/razer_mount
	sed 's/@OPENRAZER_GROUP@/$(OPENRAZER_GROUP)/' $(OPENRAZER_DIR)/openrazer.sysusers.in \
		>$(DESTDIR)$(SYSUSERSDIR)/openrazer.conf
	chmod 0644 $(DESTDIR)$(UDEVRULESDIR)/99-razer.rules $(DESTDIR)$(SYSUSERSDIR)/openrazer.conf
	chmod 0755 $(DESTDIR)$(UDEVDIR)/razer_mount

install-openrazer-full:
	install -Dm0644 -t $(DESTDIR)$(UDEVRULESDIR) $(OPENRAZER_DIR)/69-hyperflux-next-openrazer-access.rules \
		$(OPENRAZER_DIR)/70-hyperflux-next-openrazer-keys.rules
	install -Dm0644 $(OPENRAZER_DIR)/hyperflux-next-openrazer.modules-load.conf \
		$(DESTDIR)$(MODULESLOADDIR)/hyperflux-next-openrazer.conf
