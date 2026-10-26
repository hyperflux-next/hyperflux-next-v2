// SPDX-License-Identifier: GPL-2.0-only

#include "mirror.hpp"

#include "OpenRGBPluginInterface.h"
#include "RGBController.h"

#include <algorithm>
#include <string>
#include <utility>
#include <vector>

namespace hyperflux::openrgb
{
namespace
{

// UpdateZoneMode runs the zone-mode hook and then signals a whole-mode
// update on the same thread. The hook notes its mirror here so that the
// signal doesn't apply the device mode on top.
thread_local const Mirror* zone_mode_signal = nullptr;

template<typename Value>
Value field(const nlohmann::json& description, const char* key, Value fallback)
{
    const auto found = description.find(key);
    return found != description.end() && !found->is_null() ? found->get<Value>() : fallback;
}

template<typename Item, typename Convert>
std::vector<Item> items(const nlohmann::json& description, const char* key, Convert convert)
{
    std::vector<Item> result;
    const auto found = description.find(key);
    if(found != description.end() && found->is_array())
        for(const auto& item : *found)
            result.push_back(convert(item));
    return result;
}

RGBController_Setup setup_from(RGBController& local)
{
    const nlohmann::json description = RGBController::GetDeviceDescriptionJSON(&local);
    RGBController_Setup setup {};
    setup.description = field<std::string>(description, "description", {});
    setup.location = field<std::string>(description, "location", {});
    setup.name = field<std::string>(description, "name", {});
    setup.serial = field<std::string>(description, "serial", {});
    setup.vendor = field<std::string>(description, "vendor", {});
    setup.version = field<std::string>(description, "version", {});
    setup.active_mode = field<int>(description, "active_mode", 0);
    setup.flags = field<controller_flags>(description, "flags", 0U);
    setup.type = field<device_type>(description, "type", DEVICE_TYPE_UNKNOWN);
    setup.modes = items<mode>(description, "modes", RGBController::SetModeDescriptionJSON);
    setup.zones = items<zone>(description, "zones",
                              [](const nlohmann::json& item)
                              {
                                  zone result = RGBController::SetZoneDescriptionJSON(item);
                                  // The local controller owns the layout. Resizing
                                  // only the host's copy would shift LED indices.
                                  result.flags &= ~static_cast<zone_flags>(ZONE_FLAGS_MANUALLY_CONFIGURABLE);
                                  return result;
                              });
    setup.leds = items<led>(description, "leds", RGBController::SetLEDDescriptionJSON);
    return setup;
}

} // namespace

Mirror::Mirror(OpenRGBPluginAPIInterface& api, RGBController& local) noexcept : api_(&api), local_(&local) {}

std::unique_ptr<Mirror> Mirror::create(OpenRGBPluginAPIInterface& api, RGBController& local)
{
    std::unique_ptr<Mirror> mirror(new Mirror(api, local));
    RGBController_Setup setup = setup_from(local);
    setup.object_ptr = mirror.get();
    setup.DeviceSaveMode = &Mirror::save_mode;
    setup.DeviceUpdateZoneMode = &Mirror::update_zone_mode;
    mirror->host_ = api.CreateVirtualRGBController(&setup);
    if(mirror->host_ == nullptr)
        return nullptr;
    // Registering publishes the controller to profiles and the GUI, which
    // may change it straight away. Forwarding has to be in place first.
    mirror->host_->RegisterUpdateCallback(&Mirror::host_updated, mirror.get());
    api.RegisterVirtualRGBController(mirror->host_);
    mirror->registered_ = true;
    return mirror;
}

Mirror::~Mirror()
{
    if(host_ == nullptr)
        return;
    if(registered_)
    {
        api_->UnregisterVirtualRGBController(host_);
        // OpenRGB holds its update mutex while it runs callbacks. Once this
        // returns, no forward is running or can start.
        host_->UnregisterUpdateCallback(this);
    }
    // Deleting the host controller joins its device thread. After that no
    // hook can reach this mirror.
    api_->DeleteVirtualRGBController(host_);
}

void Mirror::retire() noexcept
{
    RGBControllerInterface* const host = std::exchange(host_, nullptr);
    if(host == nullptr)
        return;
    api_->UnregisterVirtualRGBController(host);
    host->UnregisterUpdateCallback(this);
    // The hooks point into this mirror and into the plugin library.
    // UpdateVirtualRGBController swaps them under the controller's exclusive
    // lock. No hook call starts after this.
    RGBController_Setup setup {};
    try
    {
        setup = setup_from(*local_);
    }
    catch(...)
    {
        // An empty description still clears the hooks.
    }
    setup.object_ptr = nullptr;
    setup.DeviceSaveMode = nullptr;
    setup.DeviceUpdateZoneMode = nullptr;
    api_->UpdateVirtualRGBController(host, &setup);
}

bool Mirror::copy_colours() noexcept
{
    const unsigned int count = std::min(host_->GetLEDCount(), local_->GetLEDCount());
    bool coloured = false;
    for(unsigned int led = 0; led < count; ++led)
    {
        const RGBColor colour = host_->GetColor(led);
        coloured = coloured || colour != ToRGBColor(0, 0, 0);
        local_->SetColor(led, colour);
    }
    if(coloured)
        lit_ = true;
    return lit_;
}

void Mirror::copy_mode() noexcept
{
    const int active = host_->GetActiveMode();
    if(active < 0 || static_cast<unsigned int>(active) >= local_->GetModeCount())
        return;
    const auto mode = static_cast<unsigned int>(active);
    local_->SetModeSpeed(mode, host_->GetModeSpeed(mode));
    local_->SetModeBrightness(mode, host_->GetModeBrightness(mode));
    local_->SetModeDirection(mode, host_->GetModeDirection(mode));
    local_->SetModeColorMode(mode, host_->GetModeColorMode(mode));
    const unsigned int colours = host_->GetModeColorsCount(mode);
    local_->SetModeColorsCount(mode, colours);
    for(unsigned int colour = 0; colour < colours; ++colour)
        local_->SetModeColor(mode, colour, host_->GetModeColor(mode, colour));
    // SetActiveMode queues the update on the local device thread.
    local_->SetActiveMode(active);
}

void Mirror::copy_zone_mode(unsigned int zone) noexcept
{
    if(zone >= local_->GetZoneCount())
        return;
    const int active = host_->GetZoneActiveMode(zone);
    if(active >= 0 && static_cast<unsigned int>(active) < local_->GetZoneModeCount(zone))
    {
        const auto mode = static_cast<unsigned int>(active);
        local_->SetZoneModeSpeed(zone, mode, host_->GetZoneModeSpeed(zone, mode));
        local_->SetZoneModeBrightness(zone, mode, host_->GetZoneModeBrightness(zone, mode));
        local_->SetZoneModeDirection(zone, mode, host_->GetZoneModeDirection(zone, mode));
        local_->SetZoneModeColorMode(zone, mode, host_->GetZoneModeColorMode(zone, mode));
        const unsigned int colours = host_->GetZoneModeColorsCount(zone, mode);
        local_->SetZoneModeColorsCount(zone, mode, colours);
        for(unsigned int colour = 0; colour < colours; ++colour)
            local_->SetZoneModeColor(zone, mode, colour, host_->GetZoneModeColor(zone, mode, colour));
    }
    local_->SetZoneActiveMode(zone, active);
}

void Mirror::host_updated(void* object, unsigned int reason, void*) noexcept
{
    auto* mirror = static_cast<Mirror*>(object);
    if(mirror == nullptr || mirror->host_ == nullptr)
        return;
    if(reason == RGBCONTROLLER_UPDATE_REASON_UPDATELEDS)
    {
        // Covers UpdateLEDs, UpdateZoneLEDs and UpdateSingleLED. The device
        // I/O happens on the local controller's own thread.
        if(mirror->copy_colours())
            mirror->local_->UpdateLEDs();
    }
    else if(reason == RGBCONTROLLER_UPDATE_REASON_UPDATEMODE)
    {
        if(zone_mode_signal == mirror)
            zone_mode_signal = nullptr;
        else
            mirror->copy_mode();
    }
}

void Mirror::update_zone_mode(void* object, int zone) noexcept
{
    auto* mirror = static_cast<Mirror*>(object);
    if(mirror == nullptr || zone < 0)
        return;
    zone_mode_signal = mirror;
    mirror->copy_zone_mode(static_cast<unsigned int>(zone));
}

void Mirror::save_mode(void* object) noexcept
{
    // The host has applied the mode through UpdateMode before it saves.
    if(auto* mirror = static_cast<Mirror*>(object))
        mirror->local_->SaveMode();
}

void MirrorSet::add(RGBController& local)
{
    if(unloading_ || mirrors_.contains(&local))
        return;
    if(auto mirror = Mirror::create(*api_, local))
        mirrors_.emplace(&local, std::move(mirror));
}

void MirrorSet::remove(RGBController& local) noexcept
{
    const auto found = mirrors_.find(&local);
    if(found == mirrors_.end())
        return;
    auto mirror = std::move(found->second);
    mirrors_.erase(found);
    if(unloading_)
        mirror->retire();
}

void MirrorSet::clear() noexcept
{
    while(!mirrors_.empty())
        remove(*const_cast<RGBController*>(mirrors_.begin()->first));
}

} // namespace hyperflux::openrgb
