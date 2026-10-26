// SPDX-License-Identifier: GPL-2.0-only

#include "route_session.hpp"

#include "hidapi.h"

#include <algorithm>
#include <utility>

namespace hyperflux::openrgb
{

RouteSession::RouteSession(Connect connect, std::string route_token)
    : connect_(std::move(connect)), token_(std::move(route_token))
{
    releaser_ = std::thread([this] { run_releaser(); });
}

RouteSession::~RouteSession()
{
    {
        std::lock_guard lock(mutex_);
        stopping_ = true;
        if(!interrupted_)
            release();
    }
    wake_.notify_all();
    releaser_.join();
}

std::optional<Report> RouteSession::exchange(const Report& request)
{
    std::lock_guard lock(mutex_);
    if(interrupted_)
        return std::nullopt;
    try
    {
        if(!client_ || !client_->usable())
        {
            drop_connection();
            Client fresh = connect_();
            std::lock_guard guard(interrupt_mutex_);
            if(interrupted_)
                return std::nullopt;
            client_.emplace(std::move(fresh));
        }
        const auto now = Clock::now();
        if(lease_ && now >= renew_at_)
        {
            try
            {
                static_cast<void>(client_->renew_lease(*lease_, lease_term));
                renew_at_ = now + lease_term / 2;
            }
            catch(const Error& error)
            {
                // The lease ended on its own (expired, receiver replugged).
                // We take a fresh one below.
                if(error.kind() != Error::Kind::Bridge)
                    throw;
                lease_.reset();
            }
        }
        if(!lease_)
        {
            const auto lease = client_->acquire_lease(token_, lease_term);
            if(!lease.granted)
                return std::nullopt; // OpenRazer or a battery query has the device right now
            lease_ = lease.granted->id;
            renew_at_ = now + lease_term / 2;
        }
        const auto result = client_->transact(*lease_, token_, request, send_budget);
        if(result.outcome == Outcome::Matched)
        {
            release_at_ = Clock::now() + idle_release;
            wake_.notify_all();
            return result.response;
        }
        release();
    }
    catch(const Error& error)
    {
        // A bridge error leaves the connection in step. Anything else means
        // it's gone, and the lease went with it.
        if(error.kind() == Error::Kind::Bridge)
            release();
        else
            drop_connection();
    }
    return std::nullopt;
}

void RouteSession::interrupt() noexcept
{
    std::lock_guard guard(interrupt_mutex_);
    interrupted_ = true;
    if(client_)
        client_->interrupt();
}

void RouteSession::keep_response(std::optional<Report> response)
{
    std::lock_guard lock(response_mutex_);
    response_ = std::move(response);
}

std::optional<Report> RouteSession::take_response()
{
    std::lock_guard lock(response_mutex_);
    return std::exchange(response_, std::nullopt);
}

// Called with mutex_ held.
void RouteSession::release()
{
    release_at_ = {};
    const auto lease = std::exchange(lease_, std::nullopt);
    if(!lease || !client_ || !client_->usable())
        return;
    try
    {
        client_->release_lease(*lease);
    }
    catch(const Error& error)
    {
        if(error.kind() != Error::Kind::Bridge)
            drop_connection();
    }
}

// Called with mutex_ held.
void RouteSession::drop_connection()
{
    std::lock_guard guard(interrupt_mutex_);
    client_.reset();
    lease_.reset();
    release_at_ = {};
}

void RouteSession::run_releaser()
{
    std::unique_lock lock(mutex_);
    while(!stopping_)
    {
        if(release_at_ == Clock::time_point {})
            wake_.wait(lock);
        else if(Clock::now() < release_at_)
            wake_.wait_until(lock, release_at_);
        else
            release();
    }
}

} // namespace hyperflux::openrgb

// OpenRGB's report buffers carry the report id (always 0 here) in front of
// the 90 bytes, the way hidapi wants them.
extern "C" int hid_send_feature_report(hid_device* device, const unsigned char* data, size_t length)
{
    if(device == nullptr || data == nullptr || length != hyperflux::report_bytes + 1)
        return -1;
    hyperflux::Report request {};
    std::copy_n(data + 1, request.size(), request.begin());
    auto response = device->session->exchange(request);
    const bool matched = response.has_value();
    device->session->keep_response(std::move(response));
    return matched ? static_cast<int>(length) : -1;
}

extern "C" int hid_get_feature_report(hid_device* device, unsigned char* data, size_t length)
{
    if(device == nullptr || data == nullptr || length != hyperflux::report_bytes + 1)
        return -1;
    // Each response is read once. OpenRGB polls until an answer shows up, and
    // handing out the same one again would look like a second answer.
    const auto response = device->session->take_response();
    if(!response)
        return -1;
    std::copy(response->begin(), response->end(), data + 1);
    return static_cast<int>(length);
}

// The plugin owns the session and closes it once the controller is gone.
extern "C" void hid_close(hid_device*) {}
