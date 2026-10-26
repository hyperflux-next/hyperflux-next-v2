// SPDX-License-Identifier: GPL-2.0-only
//
// What the HyperFlux Next V2 page in OpenRGB's Information list says,
// worked out from plain data so it can be tested without Qt.

#pragma once

#include <hyperflux/client.hpp>

#include <cstdint>
#include <map>
#include <optional>
#include <string>
#include <vector>

namespace hyperflux::openrgb
{

// The plugin's connection to the bridge.
struct BridgeStatus
{
    enum class State
    {
        Connecting, // not connected yet since the plugin loaded
        Ready,
        Recovering, // lost the connection and trying again
        Failed,     // still trying, but it has been failing for a while
    };

    State state = State::Connecting;
    // The bridge refused this plugin's hello: different protocol versions.
    bool version_mismatch = false;
    std::string error;
};

struct PageInput
{
    bool loaded = false;
    bool stopped = false;
    BridgeStatus status;
    std::optional<Snapshot> snapshot; // kept while reconnecting, with its age shown
    // Devices that OpenRGB drives itself (over a cable, say) under the same
    // serial, keyed "<receiver_id>/<device_id>": "Wired", "Wireless" or ""
    // as OpenRGB's own controller name says.
    std::map<std::string, std::string> direct;
    std::uint64_t now_ms = 0; // CLOCK_BOOTTIME, the bridge's clock
};

// At or below this a battery is low, as in `hyperfluxctl battery --waybar`.
constexpr int low_battery = 20;

// The icon a card shows beside its state words.
enum class Sign { None, Charging, Full, Asleep, Unreachable, Waiting };

struct DeviceRow
{
    std::string key;
    std::string name;
    std::string type;    // "Mouse", "Keyboard" or "Device"
    std::string details; // "Mouse · Wireless"
    std::optional<int> percent;
    bool low = false; // low, and neither charging nor rising: the one thing the page colours
    std::string state; // "Charging", "Asleep · Low battery"
    Sign sign = Sign::None;
    // Small print: "Not yet tested through the receiver", "Last update 42 minutes ago".
    std::vector<std::string> notes;

    bool operator==(const DeviceRow&) const = default;
};

struct Notice
{
    std::string message;
    std::string command; // part of message, shown in a fixed font

    bool operator==(const Notice&) const = default;
};

// Shown instead of the cards when the bridge has nothing to list.
struct Empty
{
    std::string headline;
    Notice hint;

    bool operator==(const Empty&) const = default;
};

struct Page
{
    std::string status; // "Connected", "Reconnecting…", "Stopped", ...
    std::string detail; // the last error, as the status's tooltip
    std::optional<Notice> notice;
    std::vector<DeviceRow> devices;
    std::optional<Empty> empty;

    bool operator==(const Page&) const = default;
};

[[nodiscard]] Page make_page(const PageInput& input);

// "1 minute", "5 minutes", "2 hours", "3 days" (hfx_proto::present::age_phrase).
[[nodiscard]] std::string age_phrase(std::uint64_t age_ms);

[[nodiscard]] std::uint64_t boottime_ms() noexcept;

} // namespace hyperflux::openrgb
