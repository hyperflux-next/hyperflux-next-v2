// SPDX-License-Identifier: GPL-2.0-only

#pragma once

#include "page_model.hpp"
#include "route_session.hpp"

#include <hyperflux/client.hpp>

#include <condition_variable>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

class RGBController;

namespace hyperflux::openrgb
{

// OpenRGB's controller for one device route, with the session it talks
// through. The controller goes first: its threads use the session.
struct RouteController
{
    RouteController();
    RouteController(RouteController&&) = delete;
    ~RouteController();

    std::string key; // "<receiver_id>/<device_id>"
    std::unique_ptr<RouteSession> session;
    std::unique_ptr<RGBController> controller; // null when OpenRGB can't drive the device
};

struct DiscoveryUpdate
{
    BridgeStatus status;
    std::optional<Snapshot> snapshot;
    std::vector<std::string> removed; // keys; applied before `added`
    std::vector<std::unique_ptr<RouteController>> added;
};

struct DiscoveryConfig
{
    std::function<std::vector<Endpoint>()> endpoints = socket_candidates;
    Milliseconds wait_timeout {30'000};
    Milliseconds failed_after {10'000};
    Milliseconds first_backoff {25};
    Milliseconds last_backoff {2'000};
    // Builds OpenRGB's controller for a route. It talks to the device, which
    // is why it runs here and not on the GUI thread.
    std::function<std::unique_ptr<RGBController>(RouteSession&, const Route&)> make_controller;
};

// Follows the bridge on its own connection and thread: the route list and
// the snapshot, refreshed through `wait` instead of polling. Every change
// goes to `post` as one update, in order.
class Discovery
{
public:
    using Post = std::function<void(std::shared_ptr<DiscoveryUpdate>)>;

    Discovery(DiscoveryConfig config, Post post);
    Discovery(const Discovery&) = delete;
    Discovery& operator=(const Discovery&) = delete;
    // Stops and joins; whatever is in flight is interrupted.
    ~Discovery();

    [[nodiscard]] static bool eligible(const Route& route);

private:
    void run();
    void follow(Client& client, const Endpoint& endpoint);
    void publish(const RouteList& routes, Snapshot snapshot, const Endpoint& endpoint);
    void post_status();
    bool sleep(Milliseconds duration);
    bool stopping();

    DiscoveryConfig config_;
    Post post_;
    BridgeStatus status_;
    std::map<std::string, std::string> known_; // key -> route token

    std::mutex mutex_;
    std::condition_variable wake_;
    bool stopping_ = false;
    Client* current_ = nullptr;
    RouteSession* building_ = nullptr;
    std::thread thread_;
};

} // namespace hyperflux::openrgb
