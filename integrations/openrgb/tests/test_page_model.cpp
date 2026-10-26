// SPDX-License-Identifier: GPL-2.0-only
//
// What the HyperFlux Next V2 page says for each state of the bridge and of
// each device.

#include "check.hpp"

#include "page_model.hpp"

using namespace hyperflux;
using namespace hyperflux::openrgb;

namespace
{

constexpr std::uint64_t now = 10'000'000;
constexpr std::uint64_t minute = 60'000;

Device device(const char* id, DeviceKind kind, std::uint16_t product, const char* name)
{
    Device d;
    d.device_id = id;
    d.kind = kind;
    d.vendor_id = 0x1532;
    d.product_id = product;
    if(name != nullptr)
        d.name = name;
    d.support = Support::Tested;
    d.presence = Presence::Online;
    return d;
}

Battery reading(int level, std::optional<Charge> charge, std::uint64_t age_ms, bool stale)
{
    return {level, charge, now - age_ms, stale, false, std::nullopt};
}

PageInput ready(std::vector<Device> devices)
{
    PageInput input;
    input.loaded = true;
    input.status.state = BridgeStatus::State::Ready;
    input.now_ms = now;
    Receiver receiver;
    receiver.receiver_id = "receiver-1";
    receiver.generation_id = 1;
    receiver.devices = std::move(devices);
    input.snapshot = Snapshot {7, now, {receiver}};
    return input;
}

const DeviceRow& only_row(const Page& page)
{
    static const DeviceRow none;
    CHECK(page.devices.size() == 1);
    return page.devices.empty() ? none : page.devices.front();
}

using Notes = std::vector<std::string>;

void ages()
{
    CHECK(age_phrase(0) == "1 minute");
    CHECK(age_phrase(119'999) == "1 minute");
    CHECK(age_phrase(2 * minute) == "2 minutes");
    CHECK(age_phrase(60 * minute) == "1 hour");
    CHECK(age_phrase(47 * 60 * minute) == "47 hours");
    CHECK(age_phrase(48 * 60 * minute) == "2 days");
}

void status_follows_the_plugin()
{
    PageInput input;
    CHECK(make_page(input).status == "Not loaded");
    input.loaded = true;
    input.stopped = true;
    CHECK(make_page(input).status == "Stopped");

    const Page none = make_page(ready({}));
    CHECK(none.status == "Connected" && !none.notice && none.empty);
    if(none.empty)
        CHECK(none.empty->headline == "No devices yet" && none.empty->hint.message.starts_with("Move or click"));
    PageInput nothing = ready({});
    nothing.snapshot->receivers.clear();
    const Page bare = make_page(nothing);
    CHECK(bare.empty && bare.empty->headline == "No receiver found"
          && bare.empty->hint.command == "hyperfluxctl doctor --explain");
}

void battery_states()
{
    auto mouse = device("pointer-00cd", DeviceKind::Mouse, 0x00cd, "Razer Basilisk V3 Pro 35K");
    mouse.battery = reading(79, Charge::Charging, minute, false);
    DeviceRow row = only_row(make_page(ready({mouse})));
    CHECK(row.name == "Razer Basilisk V3 Pro 35K" && row.key == "receiver-1/pointer-00cd" && row.type == "Mouse");
    CHECK(row.percent == 79 && row.state == "Charging" && row.sign == Sign::Charging && !row.low);
    CHECK(row.details == "Mouse · Wireless" && row.notes.empty());

    mouse.battery = reading(100, Charge::Charging, minute, false);
    row = only_row(make_page(ready({mouse})));
    CHECK(row.state == "Full" && row.sign == Sign::Full);

    // Discharging is the normal case and needs no words.
    mouse.battery = reading(55, Charge::Discharging, minute, false);
    row = only_row(make_page(ready({mouse})));
    CHECK(row.state.empty() && row.sign == Sign::None);

    // Low at 20 % and below, like hyperfluxctl, unless it's charging.
    mouse.battery = reading(20, Charge::Discharging, minute, false);
    row = only_row(make_page(ready({mouse})));
    CHECK(row.low && row.state == "Low battery");
    mouse.battery = reading(21, std::nullopt, minute, false);
    CHECK(!only_row(make_page(ready({mouse}))).low);
    mouse.battery = reading(8, Charge::Charging, minute, false);
    row = only_row(make_page(ready({mouse})));
    CHECK(!row.low && row.state == "Charging");

    mouse.battery = reading(64, std::nullopt, 42 * minute, true);
    row = only_row(make_page(ready({mouse})));
    CHECK(row.percent == 64 && row.state.empty() && row.notes == Notes {"Last update 42 minutes ago"});
    // A stale low reading is still low.
    mouse.battery = reading(12, std::nullopt, 42 * minute, true);
    row = only_row(make_page(ready({mouse})));
    CHECK(row.low && row.state == "Low battery");

    // A time ahead of ours can't be turned into an age.
    mouse.battery = Battery {64, std::nullopt, now + minute, true, false, std::nullopt};
    CHECK(only_row(make_page(ready({mouse}))).notes == Notes {"No recent update"});

    mouse.battery = Battery {};
    row = only_row(make_page(ready({mouse})));
    CHECK(!row.percent && !row.low && row.notes == Notes {"Battery level not reported yet"});
    mouse.battery.unreported = Unreported::Battery;
    CHECK(only_row(make_page(ready({mouse}))).notes == Notes {"Battery level not reported"});

    // No alarm while the level climbs, and no guess about charging.
    mouse.battery = reading(12, std::nullopt, minute, false);
    mouse.battery.rising = true;
    mouse.battery.unreported = Unreported::Charge;
    row = only_row(make_page(ready({mouse})));
    CHECK(!row.low && row.state.empty() && row.notes == Notes {"Charging not reported"});
}

void presence_and_support()
{
    auto keyboard = device("keyboard-0290", DeviceKind::Keyboard, 0x0290, "Razer DeathStalker V2 Pro");
    keyboard.support = Support::Checked;
    keyboard.presence = Presence::Asleep;
    keyboard.battery = reading(14, std::nullopt, 42 * minute, true);
    DeviceRow row = only_row(make_page(ready({keyboard})));
    CHECK(row.state == "Asleep · Low battery" && row.sign == Sign::Asleep);
    CHECK(row.details == "Keyboard · Wireless");
    CHECK(row.notes == (Notes {"Not yet tested through the receiver", "Last update 42 minutes ago"}));
    keyboard.battery = Battery {};
    keyboard.presence = Presence::Unreachable;
    row = only_row(make_page(ready({keyboard})));
    CHECK(row.state == "Not reachable" && row.sign == Sign::Unreachable);
    keyboard.presence = Presence::Waiting;
    row = only_row(make_page(ready({keyboard})));
    CHECK(row.state == "Waiting for activity" && row.sign == Sign::Waiting);

    auto stranger = device("pointer-0099", DeviceKind::Unknown, 0x0099, nullptr);
    stranger.support = Support::Unknown;
    row = only_row(make_page(ready({stranger})));
    CHECK(row.name == "Unknown device 1532:0099" && row.type == "Device" && row.details == "Device · Wireless");
    auto unnamed = device("keyboard-0000", DeviceKind::Unknown, 0, nullptr);
    unnamed.lane = Lane::Keyboard;
    CHECK(only_row(make_page(ready({unnamed}))).name == "Unidentified keyboard device");
}

void devices_openrgb_drives_itself()
{
    auto mouse = device("pointer-00cd", DeviceKind::Mouse, 0x00cd, "Razer Basilisk V3 Pro 35K");
    mouse.presence = Presence::Asleep;
    mouse.battery = reading(79, std::nullopt, minute, false);
    PageInput input = ready({mouse});
    input.direct["receiver-1/pointer-00cd"] = "Wired";
    DeviceRow row = only_row(make_page(input));
    CHECK(row.details == "Mouse · Wired" && row.state.empty() && row.sign == Sign::None);
    CHECK(row.notes == Notes {"OpenRGB controls it over the cable until it is unplugged."});
    input.direct["receiver-1/pointer-00cd"] = "";
    row = only_row(make_page(input));
    CHECK(row.details == "Mouse");
    CHECK(row.notes == Notes {"OpenRGB controls it directly, as its own entry in the device list."});
}

void rows_sort_by_type_then_name()
{
    const Page page = make_page(ready({device("pointer-00cd", DeviceKind::Mouse, 0x00cd, "Razer Basilisk V3 Pro 35K"),
                                       device("keyboard-02d5", DeviceKind::Keyboard, 0x02d5, "Razer Z"),
                                       device("keyboard-0296", DeviceKind::Keyboard, 0x0296, "Razer A")}));
    CHECK(page.devices.size() == 3);
    if(page.devices.size() == 3)
        CHECK(page.devices[0].name == "Razer A" && page.devices[1].name == "Razer Z" && page.devices[2].type == "Mouse");
}

void notices_without_bridge()
{
    auto mouse = device("pointer-00cd", DeviceKind::Mouse, 0x00cd, "Razer Basilisk V3 Pro 35K");
    mouse.battery = reading(79, std::nullopt, 3 * minute, false);
    PageInput input = ready({mouse});
    input.status = {BridgeStatus::State::Recovering, false, "the bridge closed the connection"};
    Page page = make_page(input);
    CHECK(page.status == "Reconnecting…" && page.detail == "the bridge closed the connection");
    CHECK(page.notice && page.notice->message == "These readings are from 3 minutes ago.");
    CHECK(page.devices.size() == 1 && !page.empty);

    input.status.state = BridgeStatus::State::Failed;
    page = make_page(input);
    CHECK(page.status == "Not running" && page.notice);
    if(page.notice)
    {
        CHECK(page.notice->message
              == "These readings are from 3 minutes ago. The bridge isn't answering. "
                 "Run hyperfluxctl doctor --explain in a terminal to see the fix.");
        CHECK(page.notice->command == "hyperfluxctl doctor --explain");
    }

    input.snapshot.reset();
    page = make_page(input);
    CHECK(page.notice && page.notice->message.starts_with("The bridge isn't answering. Run"));
    input.status.state = BridgeStatus::State::Connecting;
    page = make_page(input);
    CHECK(page.status == "Connecting…" && !page.notice && !page.empty);

    input.status = {BridgeStatus::State::Recovering, true, "Supported protocol versions: 2."};
    page = make_page(input);
    CHECK(page.status == "Needs a restart" && page.notice);
    if(page.notice)
        CHECK(page.notice->message.ends_with("Restart the computer to finish the update."));
}

} // namespace

int main()
{
    ages();
    status_follows_the_plugin();
    battery_states();
    presence_and_support();
    devices_openrgb_drives_itself();
    rows_sort_by_type_then_name();
    notices_without_bridge();
    return finish("page model");
}
