// SPDX-License-Identifier: GPL-2.0-only

#include "discovery.hpp"

#include "device_table.hpp"

#include "RGBController.h"

#include <algorithm>
#include <exception>
#include <utility>

namespace hyperflux::openrgb
{
namespace
{

using Clock = std::chrono::steady_clock;

ClientConfig client_config(const char* role, std::vector<std::string> optional)
{
    return {unique_client_id(std::string("openrgb-") + role), "OpenRGB", {"native-routes"}, std::move(optional)};
}

} // namespace

RouteController::RouteController() = default;
RouteController::~RouteController() = default;

Discovery::Discovery(DiscoveryConfig config, Post post) : config_(std::move(config)), post_(std::move(post))
{
    thread_ = std::thread([this] { run(); });
}

Discovery::~Discovery()
{
    {
        std::lock_guard lock(mutex_);
        stopping_ = true;
        if(current_ != nullptr)
            current_->interrupt();
        if(building_ != nullptr)
            building_->interrupt();
    }
    wake_.notify_all();
    thread_.join();
}

bool Discovery::eligible(const Route& route)
{
    if(!route.available || route.access != Access::Qualified || !route.token || route.vendor_id != 0x1532)
        return false;
    return std::any_of(device_table.begin(), device_table.end(), [&](const TableRow& row)
                       { return row.product_id == route.product_id && row.kind == route.kind; });
}

bool Discovery::stopping()
{
    std::lock_guard lock(mutex_);
    return stopping_;
}

// False when woken to stop.
bool Discovery::sleep(Milliseconds duration)
{
    std::unique_lock lock(mutex_);
    return !wake_.wait_for(lock, duration, [this] { return stopping_; });
}

void Discovery::run()
{
    auto backoff = config_.first_backoff;
    std::optional<Clock::time_point> failing_since;
    bool connected_once = false;
    while(!stopping())
    {
        std::optional<Error> failure;
        for(const auto& endpoint : config_.endpoints())
        {
            try
            {
                Client client = Client::connect(endpoint, client_config("discovery", {"wait"}));
                struct Current
                {
                    Discovery& discovery;
                    ~Current()
                    {
                        std::lock_guard lock(discovery.mutex_);
                        discovery.current_ = nullptr;
                    }
                } current {*this};
                {
                    std::lock_guard lock(mutex_);
                    if(stopping_)
                        return;
                    current_ = &client;
                }
                connected_once = true;
                failing_since.reset();
                backoff = config_.first_backoff;
                follow(client, endpoint);
                break;
            }
            catch(const Error& error)
            {
                failure = error;
                // Only "nobody there" moves on to the next socket. An answer
                // from a bridge, even a refusal, is the answer.
                if(error.kind() != Error::Kind::Connect)
                    break;
            }
            catch(const std::exception& error)
            {
                failure = Error(Error::Kind::Protocol, error.what());
                break;
            }
        }
        if(stopping())
            return;
        if(!failure)
            failure = Error(Error::Kind::Connect, "there is no bridge socket to try");
        const auto now = Clock::now();
        if(!failing_since)
            failing_since = now;
        status_.error = failure->what();
        status_.version_mismatch =
            failure->kind() == Error::Kind::Bridge && (failure->code() == "HFX-101" || failure->code() == "HFX-102");
        status_.state = now - *failing_since >= config_.failed_after ? BridgeStatus::State::Failed
                        : connected_once                             ? BridgeStatus::State::Recovering
                                                                     : BridgeStatus::State::Connecting;
        post_status();
        if(!sleep(backoff))
            return;
        backoff = std::min(backoff * 2, config_.last_backoff);
    }
}

void Discovery::follow(Client& client, const Endpoint& endpoint)
{
    auto routes = client.list_routes();
    auto snapshot = client.snapshot();
    status_ = {BridgeStatus::State::Ready, false, {}};
    publish(routes, snapshot, endpoint);
    while(!stopping())
    {
        if(!client.welcome().has("wait"))
        {
            // A bridge without wait still answers; ask again now and then.
            if(!sleep(Milliseconds(2'000)))
                return;
            routes = client.list_routes();
            snapshot = client.snapshot();
            publish(routes, snapshot, endpoint);
            continue;
        }
        const auto changed = client.wait(snapshot.revision, routes.revision, config_.wait_timeout);
        if(changed.timed_out)
            continue;
        if(changed.routes != routes.revision)
            routes = client.list_routes();
        if(changed.state != snapshot.revision)
            snapshot = client.snapshot();
        publish(routes, snapshot, endpoint);
    }
}

void Discovery::publish(const RouteList& routes, Snapshot snapshot, const Endpoint& endpoint)
{
    auto update = std::make_shared<DiscoveryUpdate>();
    update->status = status_;
    update->snapshot = std::move(snapshot);

    std::map<std::string, const Route*> wanted;
    for(const auto& route : routes.routes)
        if(eligible(route))
            wanted[route.receiver_id + "/" + route.device_id] = &route;

    // A route that went away, or came back with a new token, gets a new
    // controller: the old one's token is dead.
    for(auto known = known_.begin(); known != known_.end();)
    {
        const auto found = wanted.find(known->first);
        if(found == wanted.end() || *found->second->token != known->second)
        {
            update->removed.push_back(known->first);
            known = known_.erase(known);
        }
        else
        {
            ++known;
        }
    }

    for(const auto& [key, route] : wanted)
    {
        if(known_.contains(key) || stopping())
            continue;
        auto made = std::make_unique<RouteController>();
        made->key = key;
        made->session = std::make_unique<RouteSession>(
            [endpoint] { return Client::connect(endpoint, client_config("feature", {})); }, *route->token);
        {
            std::lock_guard lock(mutex_);
            building_ = made->session.get();
        }
        try
        {
            if(config_.make_controller)
                made->controller = config_.make_controller(*made->session, *route);
        }
        catch(const std::exception&)
        {
            // Left without a controller: OpenRGB just won't list the device.
        }
        {
            std::lock_guard lock(mutex_);
            building_ = nullptr;
        }
        known_[key] = *route->token;
        update->added.push_back(std::move(made));
    }
    post_(std::move(update));
}

void Discovery::post_status()
{
    auto update = std::make_shared<DiscoveryUpdate>();
    update->status = status_;
    post_(std::move(update));
}

} // namespace hyperflux::openrgb
