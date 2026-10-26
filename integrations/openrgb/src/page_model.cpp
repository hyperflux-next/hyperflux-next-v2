// SPDX-License-Identifier: GPL-2.0-only

#include "page_model.hpp"

#include <algorithm>
#include <cstdio>
#include <ctime>
#include <tuple>

namespace hyperflux::openrgb
{
namespace
{

constexpr const char* doctor_command = "hyperfluxctl doctor --explain";

std::string type_of(DeviceKind kind)
{
    switch(kind)
    {
        case DeviceKind::Mouse: return "Mouse";
        case DeviceKind::Keyboard: return "Keyboard";
        case DeviceKind::Unknown: break;
    }
    return "Device";
}

std::string presence_of(Presence presence)
{
    switch(presence)
    {
        case Presence::Asleep: return "Asleep";
        case Presence::Unreachable: return "Not reachable";
        case Presence::Waiting: return "Waiting for activity";
        case Presence::Online: break;
    }
    return {};
}

// How old a reading is, or nothing when it has no time or a time ahead of
// ours, which would be another machine's clock.
std::optional<std::uint64_t> age_of(const Battery& battery, std::uint64_t now_ms)
{
    if(!battery.updated_ms || *battery.updated_ms > now_ms)
        return std::nullopt;
    return now_ms - *battery.updated_ms;
}

DeviceRow row_for(const Receiver& receiver, const Device& device, const PageInput& input)
{
    DeviceRow row;
    row.key = receiver.receiver_id + "/" + device.device_id;
    // Product id 0 stands in for a device the receiver hasn't named yet.
    char unknown[40];
    std::snprintf(unknown, sizeof(unknown), "Unknown device %04x:%04x", device.vendor_id, device.product_id);
    const char* unnamed = device.lane == Lane::Pointer ? "Unidentified pointer device" : "Unidentified keyboard device";
    row.name = device.name ? *device.name : device.product_id == 0 ? unnamed : unknown;
    row.type = type_of(device.kind);

    std::vector<std::string> state;
    std::string connection = "Wireless";
    const auto direct = input.direct.find(row.key);
    if(direct == input.direct.end())
    {
        if(const std::string presence = presence_of(device.presence); !presence.empty())
            state.push_back(presence);
        row.sign = device.presence == Presence::Asleep        ? Sign::Asleep
                   : device.presence == Presence::Unreachable ? Sign::Unreachable
                   : device.presence == Presence::Waiting     ? Sign::Waiting
                                                              : Sign::None;
    }
    else
    {
        // OpenRGB drives this one itself. What the receiver knows about its
        // presence isn't what the user is looking at.
        connection = direct->second;
        row.notes.push_back(direct->second == "Wired"
                                ? "OpenRGB controls it over the cable until it is unplugged."
                                : "OpenRGB controls it directly, as its own entry in the device list.");
    }

    const Battery& battery = device.battery;
    const bool charging = battery.level && !battery.stale && battery.charge == Charge::Charging;
    if(battery.level)
    {
        row.percent = *battery.level;
        // No alarm while the level climbs, charging reported or not.
        row.low = *battery.level <= low_battery && !charging && !battery.rising;
        // The bridge has no "full": it is charging at 100 %.
        if(charging)
            state.push_back(*battery.level >= 100 ? "Full" : "Charging");
        else if(row.low)
            state.push_back("Low battery");
        if(charging && row.sign == Sign::None)
            row.sign = *battery.level >= 100 ? Sign::Full : Sign::Charging;
        if(battery.stale)
        {
            const auto age = age_of(battery, input.now_ms);
            row.notes.push_back(age ? "Last update " + age_phrase(*age) + " ago" : "No recent update");
        }
        if(battery.unreported == Unreported::Charge)
            row.notes.push_back("Charging not reported");
    }
    else
    {
        const bool never = battery.unreported == Unreported::Battery;
        row.notes.push_back(never ? "Battery level not reported" : "Battery level not reported yet");
    }
    for(const auto& word : state)
        row.state += (row.state.empty() ? "" : " · ") + word;

    row.details = row.type + (connection.empty() ? "" : " · " + connection);
    if(device.support == Support::Checked)
        row.notes.insert(row.notes.begin(), "Not yet tested through the receiver");
    return row;
}

} // namespace

std::string age_phrase(std::uint64_t age_ms)
{
    const auto plural = [](std::uint64_t count, const char* unit)
    { return std::to_string(count) + " " + unit + (count == 1 ? "" : "s"); };
    const auto minutes = std::max<std::uint64_t>(age_ms / 60'000, 1);
    if(minutes < 60)
        return plural(minutes, "minute");
    if(minutes / 60 < 48)
        return plural(minutes / 60, "hour");
    return plural(minutes / 60 / 24, "day");
}

std::uint64_t boottime_ms() noexcept
{
    timespec now {};
    clock_gettime(CLOCK_BOOTTIME, &now);
    return static_cast<std::uint64_t>(now.tv_sec) * 1000 + static_cast<std::uint64_t>(now.tv_nsec) / 1'000'000;
}

Page make_page(const PageInput& input)
{
    Page page;
    if(!input.loaded || input.stopped)
    {
        page.status = input.loaded ? "Stopped" : "Not loaded";
        return page;
    }

    std::optional<std::uint64_t> newest;
    if(input.snapshot)
    {
        for(const auto& receiver : input.snapshot->receivers)
        {
            for(const auto& device : receiver.devices)
            {
                page.devices.push_back(row_for(receiver, device, input));
                const auto age = age_of(device.battery, input.now_ms);
                if(age && (!newest || *age < *newest))
                    newest = age;
            }
        }
    }
    // By type, then by name: the cards keep their places between updates.
    std::sort(page.devices.begin(), page.devices.end(), [](const DeviceRow& a, const DeviceRow& b)
              { return std::tie(a.type, a.name) < std::tie(b.type, b.name); });

    page.detail = input.status.error;
    const bool readings = !page.devices.empty() && newest;
    const std::string age = readings ? "These readings are from " + age_phrase(*newest) + " ago." : "";
    using State = BridgeStatus::State;
    const State state = input.status.state;

    if(state == State::Ready)
    {
        page.status = "Connected";
        if(page.devices.empty() && input.snapshot && input.snapshot->receivers.empty())
            page.empty = Empty {"No receiver found",
                                {"Plug in the HyperFlux V2 receiver. If it is plugged in, run " + std::string(doctor_command)
                                     + " in a terminal.",
                                 doctor_command}};
        else if(page.devices.empty())
            page.empty = Empty {"No devices yet",
                                {"Move or click a paired device to wake it. If it doesn't show up within 30 seconds, "
                                 "switch it off and on.",
                                 {}}};
        return page;
    }
    if(input.status.version_mismatch)
    {
        // Retrying can't fix this one: after an update the old bridge keeps
        // running until the computer restarts.
        page.status = "Needs a restart";
        page.notice = Notice {"This plugin and the running bridge are different versions. "
                              "Restart the computer to finish the update.",
                              {}};
        return page;
    }
    if(state == State::Failed)
    {
        page.status = "Not running";
        page.notice = Notice {(readings ? age + " " : "") + "The bridge isn't answering. Run " + doctor_command
                                  + " in a terminal to see the fix.",
                              doctor_command};
        return page;
    }
    page.status = state == State::Recovering ? "Reconnecting…" : "Connecting…";
    // Without readings on screen the status already says it all.
    if(readings)
        page.notice = Notice {age, {}};
    return page;
}

} // namespace hyperflux::openrgb
