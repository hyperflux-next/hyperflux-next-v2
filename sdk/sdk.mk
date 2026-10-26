# SPDX-License-Identifier: GPL-2.0-only
#
# SDK tests, included by the top-level Makefile. The C++ SDK is built with
# the nlohmann/json copy in the pinned OpenRGB 1.1 tree, the one the plugin
# uses. The first run fetches that tree.

SDK_BUILD = build/sdk-cpp
SDK_JSON = .upstreams/openrgb-$(OPENRGB_QT6_VERSION)
CMAKE ?= cmake

TEST_TARGETS += sdk-test
.PHONY: sdk-test sdk-cpp-test sdk-python-test

sdk-test: sdk-cpp-test sdk-python-test

sdk-cpp-test:
	sh integrations/openrgb/fetch.sh $(SDK_JSON) 6
	$(CMAKE) -S sdk/cpp -B $(SDK_BUILD) -DHFX_NLOHMANN_JSON_DIR=$(abspath $(SDK_JSON))/dependencies/json -DHFX_WERROR=ON
	$(CMAKE) --build $(SDK_BUILD) --parallel
	ctest --test-dir $(SDK_BUILD) --output-on-failure

sdk-python-test:
	PYTHONDONTWRITEBYTECODE=1 $(PYTHON) -m pytest -q -p no:cacheprovider sdk/python/tests
