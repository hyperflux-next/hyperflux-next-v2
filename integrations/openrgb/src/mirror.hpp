// SPDX-License-Identifier: GPL-2.0-only

#pragma once

#include <atomic>
#include <map>
#include <memory>

class OpenRGBPluginAPIInterface;
class RGBController;
class RGBControllerInterface;

namespace hyperflux::openrgb
{

// Shows one of the plugin's own controllers in OpenRGB.
//
// Plugin API 5 hosts only list controllers they created, so the host makes
// a virtual controller from the local one's description and the mirror
// forwards every change made to it back to the local controller, which
// does the device I/O. LED and mode changes arrive through the host
// controller's update callback, on the thread that made them: OpenRGB 1.0
// and 1.1 keep only CONTROLLER_FLAG_VIRTUAL from a virtual controller's
// flags, so the host's device thread would drop a request that arrives
// while it runs a hook. Zone modes and saving go through their hooks.
//
// Create and destroy mirrors on the GUI thread: registering refreshes
// OpenRGB's device list there and blocks other registering threads until
// the refresh is done.
class Mirror
{
public:
    // Null when the host refuses the description.
    [[nodiscard]] static std::unique_ptr<Mirror> create(OpenRGBPluginAPIInterface& api, RGBController& local);
    Mirror(const Mirror&) = delete;
    Mirror& operator=(const Mirror&) = delete;
    ~Mirror();

    // Takes the host controller out of OpenRGB without deleting it and cuts
    // every hook into the plugin. See MirrorSet::begin_unload.
    void retire() noexcept;

private:
    Mirror(OpenRGBPluginAPIInterface& api, RGBController& local) noexcept;

    static void host_updated(void* mirror, unsigned int reason, void* controller) noexcept;
    static void update_zone_mode(void* mirror, int zone) noexcept;
    static void save_mode(void* mirror) noexcept;
    bool copy_colours() noexcept;
    void copy_mode() noexcept;
    void copy_zone_mode(unsigned int zone) noexcept;

    OpenRGBPluginAPIInterface* api_;
    RGBController* local_;
    RGBControllerInterface* host_ = nullptr;
    bool registered_ = false;
    // Set once a frame with colour in it has arrived. OpenRGB hands every
    // new controller an all-black buffer first, which would blank the
    // device as soon as it shows up.
    std::atomic<bool> lit_ {false};
};

class MirrorSet
{
public:
    explicit MirrorSet(OpenRGBPluginAPIInterface& api) noexcept : api_(&api) {}
    ~MirrorSet() { clear(); }

    void add(RGBController& local);
    void remove(RGBController& local) noexcept;
    void clear() noexcept;

    // On exit OpenRGB stops following its device list before it unloads
    // plugins. Its device pages still hold our host controllers and use
    // them while they are destroyed, after Unload has returned. From here
    // on, removing a mirror retires its host controller and leaves it
    // allocated: a small leak instead of a use after free.
    void begin_unload() noexcept { unloading_ = true; }

    [[nodiscard]] OpenRGBPluginAPIInterface& api() const noexcept { return *api_; }

private:
    OpenRGBPluginAPIInterface* api_;
    std::map<const RGBController*, std::unique_ptr<Mirror>> mirrors_;
    bool unloading_ = false;
};

} // namespace hyperflux::openrgb
