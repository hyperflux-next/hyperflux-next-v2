// SPDX-License-Identifier: GPL-2.0-only
//
// The route session against a scripted bridge: one lease per burst, no
// resends, and the hidapi functions OpenRGB's RazerController calls.

#include "check.hpp"

#include "hidapi.h"
#include "route_session.hpp"

#include <hyperflux/wire.hpp>

#include <sys/socket.h>
#include <unistd.h>

#include <thread>
#include <vector>

using namespace hyperflux;
using namespace hyperflux::openrgb;
using namespace std::chrono_literals;
using Clock = std::chrono::steady_clock;

namespace
{

constexpr const char* token = "3f9a6c1e8b2d47f0a5c3e9b1d6f2a8c4e7b0d3f6a9c2e5b8d1f4a7c0e3b6d9f2";

bool read_exact(int fd, char* data, std::size_t size)
{
    for(std::size_t got = 0; got < size;)
    {
        const ssize_t n = ::recv(fd, data + got, size - got, 0);
        if(n <= 0)
            return false;
        got += static_cast<std::size_t>(n);
    }
    return true;
}

void write_frame(int fd, const std::string& payload)
{
    const auto size = static_cast<std::uint32_t>(payload.size());
    std::string frame {static_cast<char>(size >> 24), static_cast<char>(size >> 16), static_cast<char>(size >> 8),
                       static_cast<char>(size)};
    ::send(fd, (frame + payload).data(), frame.size() + payload.size(), MSG_NOSIGNAL);
}

class FakeBridge
{
public:
    enum class Transact { Matched, NotSent, Hang, Hangup };

    std::atomic<bool> busy {false};
    std::atomic<Transact> transact {Transact::Matched};
    std::atomic<int> connects {0};

    ~FakeBridge()
    {
        for(auto& thread : threads_)
            thread.join();
    }

    RouteSession::Connect connector()
    {
        return [this]
        {
            int fds[2];
            ::socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, fds);
            ++connects;
            threads_.emplace_back([this, fd = fds[1]] { serve(fd); });
            return Client::handshake(fds[0], {"openrgb-test", "OpenRGB", {"native-routes"}, {}, 2000ms});
        };
    }

    std::vector<std::string> methods()
    {
        std::lock_guard lock(mutex_);
        std::vector<std::string> result;
        for(const auto& [method, when] : log_)
            result.push_back(method);
        return result;
    }

    Clock::time_point when(const std::string& method, bool last = true)
    {
        std::lock_guard lock(mutex_);
        Clock::time_point found {};
        for(const auto& [name, time] : log_)
            if(name == method && (last || found == Clock::time_point {}))
                found = time;
        return found;
    }

private:
    void serve(int fd)
    {
        unsigned char prefix[4];
        while(read_exact(fd, reinterpret_cast<char*>(prefix), 4))
        {
            std::string payload((std::size_t {prefix[0]} << 24) | (prefix[1] << 16) | (prefix[2] << 8) | prefix[3], '\0');
            if(!read_exact(fd, payload.data(), payload.size()))
                break;
            const auto request = wire::Json::parse(payload);
            const auto method = request["method"].get<std::string>();
            {
                std::lock_guard lock(mutex_);
                log_.emplace_back(method, Clock::now());
            }
            wire::Json result = wire::Json::object();
            if(method == "hello")
                result = {{"version", 1}, {"server_instance_id", "bridge-test"}, {"bridge_version", "1.0.0"},
                          {"features", {"native-routes"}}};
            else if(method == "acquire-lease" && busy)
                result = {{"outcome", "busy"}, {"lease_id", nullptr}, {"expires_ms", nullptr}, {"holder", "OpenRazer"}};
            else if(method == "acquire-lease")
                result = {{"outcome", "granted"}, {"lease_id", "lease-1"}, {"expires_ms", "99"}, {"holder", nullptr}};
            else if(method == "transact" && transact == Transact::Hang)
                continue;
            else if(method == "transact" && transact == Transact::Hangup)
                break;
            else if(method == "transact" && transact == Transact::NotSent)
                result = {{"outcome", "not-sent"}, {"response", nullptr}, {"code", "HFX-109"}};
            else if(method == "transact")
            {
                auto response = wire::parse_report(request["params"]["report"].get<std::string>()).value();
                response[0] = 0x02;
                result = {{"outcome", "matched"}, {"response", wire::hex(response)}, {"code", nullptr}};
            }
            write_frame(fd, wire::Json {{"id", request["id"]}, {"result", result}}.dump());
        }
        ::close(fd);
    }

    std::mutex mutex_;
    std::vector<std::pair<std::string, Clock::time_point>> log_;
    std::vector<std::thread> threads_;
};

Report query(std::uint8_t command)
{
    Report report {};
    report[1] = 0x1f;
    report[5] = 2;
    report[6] = 0x07;
    report[7] = command;
    return report;
}

void a_burst_holds_one_lease()
{
    FakeBridge bridge;
    RouteSession session(bridge.connector(), token);
    for(std::uint8_t command : {0x80, 0x84, 0x80})
    {
        const auto response = session.exchange(query(command));
        CHECK(response && (*response)[0] == 0x02 && (*response)[7] == command);
    }
    std::this_thread::sleep_for(150ms);
    using List = std::vector<std::string>;
    CHECK(bridge.methods() == (List {"hello", "acquire-lease", "transact", "transact", "transact", "release-lease"}));
    // Given back after the burst went quiet, not after each report.
    CHECK(bridge.when("release-lease") - bridge.when("transact") >= 20ms);
    CHECK(bridge.connects == 1);
}

void refused_report_frees_device()
{
    FakeBridge bridge;
    bridge.transact = FakeBridge::Transact::NotSent;
    RouteSession session(bridge.connector(), token);
    CHECK(!session.exchange(query(0x80)));
    using List = std::vector<std::string>;
    CHECK(bridge.methods() == (List {"hello", "acquire-lease", "transact", "release-lease"}));
}

void busy_device_left_alone()
{
    FakeBridge bridge;
    bridge.busy = true;
    RouteSession session(bridge.connector(), token);
    CHECK(!session.exchange(query(0x80)));
    using List = std::vector<std::string>;
    CHECK(bridge.methods() == (List {"hello", "acquire-lease"}));
}

void no_resend_after_disconnect()
{
    FakeBridge bridge;
    bridge.transact = FakeBridge::Transact::Hangup;
    RouteSession session(bridge.connector(), token);
    CHECK(!session.exchange(query(0x80)));
    bridge.transact = FakeBridge::Transact::Matched;
    CHECK(session.exchange(query(0x80)));
    using List = std::vector<std::string>;
    CHECK(bridge.connects == 2);
    CHECK(bridge.methods() == (List {"hello", "acquire-lease", "transact", "hello", "acquire-lease", "transact"}));
}

void interrupt_ends_a_stuck_exchange()
{
    FakeBridge bridge;
    bridge.transact = FakeBridge::Transact::Hang;
    RouteSession session(bridge.connector(), token);
    std::thread stopper([&session] { std::this_thread::sleep_for(50ms); session.interrupt(); });
    const auto started = Clock::now();
    CHECK(!session.exchange(query(0x80)));
    CHECK(Clock::now() - started < 1500ms);
    stopper.join();
    CHECK(!session.exchange(query(0x80)));
    CHECK(bridge.connects == 1);
}

void responses_handed_out_once()
{
    FakeBridge bridge;
    RouteSession session(bridge.connector(), token);
    hid_device* device = session.hid();
    unsigned char buffer[91] {};
    const Report request = query(0x80);
    std::copy(request.begin(), request.end(), buffer + 1);
    CHECK(hid_send_feature_report(device, buffer, sizeof(buffer)) == 91);
    unsigned char answer[91] {};
    CHECK(hid_get_feature_report(device, answer, sizeof(answer)) == 91);
    CHECK(answer[0] == 0 && answer[1] == 0x02 && answer[2] == 0x1f && answer[8] == 0x80);
    CHECK(hid_get_feature_report(device, answer, sizeof(answer)) == -1);
    CHECK(hid_send_feature_report(device, buffer, 90) == -1);
    hid_close(device);
}

} // namespace

int main()
{
    a_burst_holds_one_lease();
    refused_report_frees_device();
    busy_device_left_alone();
    no_resend_after_disconnect();
    interrupt_ends_a_stuck_exchange();
    responses_handed_out_once();
    return finish("route session");
}
