// SPDX-License-Identifier: GPL-2.0-only

#pragma once

class OpenRGBPluginAPIInterface;

namespace hyperflux::openrgb
{

// Where log lines go: OpenRGB's log while the plugin is loaded, nowhere
// otherwise. Safe to call from any thread.
void set_log_target(OpenRGBPluginAPIInterface* api) noexcept;

void log_info(const char* format, ...) __attribute__((format(printf, 1, 2)));

} // namespace hyperflux::openrgb
