// SPDX-License-Identifier: GPL-2.0-only

#pragma once

#include "hidapi.h"

#include <hyperflux/client.hpp>

#include <atomic>
#include <condition_variable>
#include <functional>
#include <mutex>
#include <optional>
#include <string>
#include <thread>

namespace hyperflux::openrgb
{
class RouteSession;
}

// What OpenRGB's RazerController holds as its hid_device (see hidapi.h).
struct hid_device_
{
    hyperflux::openrgb::RouteSession* session;
};

namespace hyperflux::openrgb
{

// One device route over its own bridge connection.
//
// OpenRGB sends reports in bursts: the rows of a keyboard frame, a mode
// change, the queries when a controller is built. A lease per report would
// cost two extra round trips each, so the session holds the device's lease
// for the burst instead: it takes it at the first report, renews it at half
// its term, and gives it back 25 ms after the last matched report, or at once
// after any other result, to let OpenRazer and the bridge's battery queries
// have their turn. Nothing is ever sent twice.
class RouteSession
{
public:
    using Connect = std::function<Client()>;
    using Clock = std::chrono::steady_clock;

    static constexpr Milliseconds lease_term {30'000};
    static constexpr Milliseconds idle_release {25};
    static constexpr Milliseconds send_budget {1'000};

    RouteSession(Connect connect, std::string route_token);
    RouteSession(const RouteSession&) = delete;
    RouteSession& operator=(const RouteSession&) = delete;
    ~RouteSession();

    // The device's response to `request`, or nothing when it wasn't matched.
    [[nodiscard]] std::optional<Report> exchange(const Report& request);

    // Makes the exchange in progress and every later one fail promptly.
    void interrupt() noexcept;

    [[nodiscard]] hid_device* hid() noexcept { return &hid_; }

    // The hidapi side keeps the last matched response until it is read.
    void keep_response(std::optional<Report> response);
    [[nodiscard]] std::optional<Report> take_response();

private:
    void release();
    void drop_connection();
    void run_releaser();

    Connect connect_;
    const std::string token_;

    std::mutex mutex_; // one request at a time on the connection
    std::condition_variable wake_;
    std::optional<Client> client_;
    std::optional<std::string> lease_;
    Clock::time_point renew_at_ {};
    Clock::time_point release_at_ {};
    bool stopping_ = false;

    // client_ only changes with both mutexes held, so interrupt() can reach
    // it while an exchange holds mutex_ for its whole round trip.
    std::mutex interrupt_mutex_;
    std::atomic<bool> interrupted_ {false};

    std::mutex response_mutex_;
    std::optional<Report> response_;

    hid_device_ hid_ {this};
    std::thread releaser_;
};

} // namespace hyperflux::openrgb
