# SPDX-License-Identifier: GPL-2.0-only
#
# The OpenRGB plugin, included by the top-level Makefile. OPENRGB_QT picks
# the OpenRGB release it is built for: 6 for OpenRGB 1.1, 5 for 1.0.
#
#   make openrgb-plugin           fetch the pinned OpenRGB and build the plugin
#   make openrgb-test             the plugin's tests
#   make openrgb-load-test        load it into a headless OpenRGB (OPENRGB=binary)
#   make install-openrgb-plugin   the hyperflux-next-v2-openrgb-plugin package
#                                 (packaging.mk adds the notice of the
#                                 nlohmann/json it carries)

OPENRGB_DIR := integrations/openrgb
openrgb_version = $(if $(filter 5,$(OPENRGB_QT)),$(OPENRGB_QT5_VERSION),$(OPENRGB_QT6_VERSION))
OPENRGB_SOURCE = .upstreams/openrgb-$(openrgb_version)
OPENRGB_BUILD = build/openrgb-qt$(OPENRGB_QT)
OPENRGB ?= openrgb
CMAKE ?= cmake
# Only this repository's own history names the commit. A tarball inside
# some other checkout (an AUR package, say) must not pick up that one.
openrgb_commit = $(if $(wildcard .git),$(shell git rev-parse --short=12 HEAD 2>/dev/null))

.PHONY: openrgb-source openrgb-plugin openrgb-test openrgb-load-test install-openrgb-plugin

openrgb-source:
	sh $(OPENRGB_DIR)/fetch.sh $(OPENRGB_SOURCE) $(OPENRGB_QT)

openrgb-plugin: openrgb-source
	$(CMAKE) -S $(OPENRGB_DIR) -B $(OPENRGB_BUILD) -DCMAKE_BUILD_TYPE=Release \
		-DOPENRGB_QT=$(OPENRGB_QT) -DOPENRGB_SOURCE=$(abspath $(OPENRGB_SOURCE)) \
		-DHFX_PROJECT_URL=$(PROJECT_URL) -DHFX_COMMIT=$(openrgb_commit) -DHFX_WERROR=ON
	$(CMAKE) --build $(OPENRGB_BUILD) --parallel

openrgb-test: openrgb-plugin
	ctest --test-dir $(OPENRGB_BUILD) --output-on-failure

openrgb-load-test: openrgb-plugin
	sh $(OPENRGB_DIR)/tests/headless-load.sh $(OPENRGB_BUILD)/hyperflux-next-openrgb.so $(OPENRGB)

install-openrgb-plugin:
	install -Dm0755 $(OPENRGB_BUILD)/hyperflux-next-openrgb.so $(DESTDIR)$(OPENRGBPLUGINDIR)/hyperflux-next-openrgb.so
