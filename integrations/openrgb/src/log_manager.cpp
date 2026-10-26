// SPDX-License-Identifier: GPL-2.0-only
//
// The OpenRGB sources compiled into the plugin log through LogManager, and
// OpenRGB's own LogManager isn't reachable from a plugin. This one hands
// each line to the host through the plugin API instead.

#include "log.hpp"

#include "LogManager.h"
#include "OpenRGBPluginInterface.h"

#include <atomic>
#include <cstdarg>
#include <cstdio>

namespace hyperflux::openrgb
{
namespace
{

// The host's plugin API object outlives the plugin. A thread that read the
// pointer just before Unload cleared it still writes somewhere valid.
std::atomic<OpenRGBPluginAPIInterface*> target {nullptr};

void forward(const char* file, int line, unsigned int level, const char* format, va_list arguments)
{
    OpenRGBPluginAPIInterface* api = target.load();
    if(api == nullptr)
        return;
    char text[1024];
    std::vsnprintf(text, sizeof(text), format, arguments);
    api->LogEntry(file, line, level, "%s", text);
}

} // namespace

void set_log_target(OpenRGBPluginAPIInterface* api) noexcept
{
    target = api;
}

void log_info(const char* format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    forward("hyperflux-next-openrgb", 0, LL_INFO, format, arguments);
    va_end(arguments);
}

} // namespace hyperflux::openrgb

const char* LogManager::LOG_CODES[] = {"Fatal", "Error", "Warning", "Info", "Verbose", "Debug", "Trace", "Dialog"};
LogManager* LogManager::instance = nullptr;

LogManager::LogManager() = default;
LogManager::~LogManager() = default;

LogManager* LogManager::get()
{
    static LogManager manager;
    return &manager;
}

void LogManager::LogEntry(const char* filename, int line, unsigned int level, const char* format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    hyperflux::openrgb::forward(filename, line, level, format, arguments);
    va_end(arguments);
}

void LogManager::LogEntry_va(const char* filename, int line, unsigned int level, const char* format, va_list arguments)
{
    hyperflux::openrgb::forward(filename, line, level, format, arguments);
}
