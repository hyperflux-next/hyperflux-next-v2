// SPDX-License-Identifier: GPL-2.0-only
//
// OpenRGB's RazerController reaches its device through hidapi. Inside the
// plugin it includes this header instead, and hidapi is three functions
// (route_session.cpp): a hid_device is one bridge route, sending a feature
// report is one transact, and getting one returns the matched response.
// They are hidden symbols and never mix with the host's real hidapi.

#pragma once

#include <cstddef>

typedef struct hid_device_ hid_device;

extern "C"
{
__attribute__((visibility("hidden"))) int hid_send_feature_report(hid_device* device, const unsigned char* data,
                                                                   size_t length);
__attribute__((visibility("hidden"))) int hid_get_feature_report(hid_device* device, unsigned char* data,
                                                                  size_t length);
__attribute__((visibility("hidden"))) void hid_close(hid_device* device);
}
