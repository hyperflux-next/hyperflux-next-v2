// SPDX-License-Identifier: GPL-2.0-only
//
// The client against the golden messages in crates/hfx-proto/tests/fixtures
// and against a scripted bridge on a socket pair.

#include <hyperflux/client.hpp>
#include <hyperflux/wire.hpp>

#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <functional>
#include <iostream>
#include <iterator>
#include <set>
#include <sstream>
#include <thread>

#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

using namespace hyperflux;
using namespace std::chrono_literals;

namespace
{

int failures = 0;

#define CHECK(condition)                                                                   \
    do                                                                                     \
    {                                                                                      \
        if(!(condition))                                                                   \
        {                                                                                  \
            std::cerr << __FILE__ << ":" << __LINE__ << ": CHECK(" #condition ") failed\n"; \
            ++failures;                                                                    \
        }                                                                                  \
    } while(false)

template<typename Body>
void expect_error(Error::Kind kind, const std::string& code, Body body, int line)
{
    try
    {
        body();
        std::cerr << __FILE__ << ":" << line << ": expected an error\n";
        ++failures;
    }
    catch(const Error& error)
    {
        if(error.kind() != kind || error.code() != code)
        {
            std::cerr << __FILE__ << ":" << line << ": unexpected error: " << error.what() << "\n";
            ++failures;
        }
    }
}
#define EXPECT_ERROR(kind, code, ...) expect_error(kind, code, [&] { __VA_ARGS__; }, __LINE__)

std::set<std::string> covered;

std::string fixture(const std::string& name)
{
    covered.insert(name);
    std::ifstream file(std::string(HFX_FIXTURES) + "/" + name + ".json", std::ios::binary);
    std::string text((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
    CHECK(!text.empty() && text.back() == '\n');
    if(!text.empty())
    {
        text.pop_back();
    }
    return text;
}

std::string id_of(const std::string& message)
{
    return wire::Json::parse(message).at("id").get<std::string>();
}

wire::Json reply(const std::string& name)
{
    const auto text = fixture(name);
    return wire::result(text, id_of(text));
}

constexpr const char* token = "3f9a6c1e8b2d47f0a5c3e9b1d6f2a8c4e7b0d3f6a9c2e5b8d1f4a7c0e3b6d9f2";
constexpr const char* lease = "lease-a41f09c2-12";

Report battery_query()
{
    Report report {};
    report[1] = 0x1f;
    report[5] = 2;
    report[6] = 0x07;
    report[7] = 0x80;
    report[88] = 0x02 ^ 0x07 ^ 0x80;
    return report;
}

void requests_match_fixtures()
{
    const ClientConfig config {"openrgb-feature-4211-9c3e7a10", "OpenRGB", {"native-routes"}, {"wait"}};
    CHECK(wire::request("r1", "hello", wire::hello_params(config)) == fixture("hello-request"));
    CHECK(wire::request("r2", "snapshot", wire::Params::object()) == fixture("snapshot-request"));
    CHECK(wire::request("r3", "wait", wire::wait_params(41, 7, 30000ms)) == fixture("wait-request"));
    CHECK(wire::request("r4", "list-routes", wire::Params::object()) == fixture("list-routes-request"));
    CHECK(wire::request("r5", "acquire-lease", wire::acquire_lease_params(token, 30000ms))
          == fixture("acquire-lease-request"));
    CHECK(wire::request("r6", "renew-lease", wire::renew_lease_params(lease, 30000ms))
          == fixture("renew-lease-request"));
    CHECK(wire::request("r7", "release-lease", wire::release_lease_params(lease)) == fixture("release-lease-request"));
    CHECK(wire::request("r8", "transact", wire::transact_params(lease, token, battery_query(), 1000ms))
          == fixture("transact-request"));
    CHECK(wire::request("r11", "teleport", wire::Params::object()) == fixture("unknown-method-request"));
}

void replies_decode()
{
    const auto welcome = wire::welcome(reply("hello-reply"));
    CHECK(welcome.version == 1 && welcome.bridge_version == "1.0.0");
    CHECK(welcome.has("native-routes") && welcome.has("wait") && !welcome.has("bench"));

    EXPECT_ERROR(Error::Kind::Bridge, "HFX-101", static_cast<void>(reply("hello-error-version")));
    EXPECT_ERROR(Error::Kind::Bridge, "HFX-102", static_cast<void>(reply("hello-error-feature")));
    EXPECT_ERROR(Error::Kind::Bridge, "HFX-102", static_cast<void>(reply("error-feature-reply")));

    const auto snapshot = wire::snapshot(reply("snapshot-reply"));
    CHECK(snapshot.revision == 41 && snapshot.now_ms == 5413001 && snapshot.receivers.size() == 1);
    CHECK(snapshot.transport == Transport::Hidraw);
    const auto& receiver = snapshot.receivers.at(0);
    CHECK(receiver.generation_id == 3 && receiver.product_id == 0x00cf && receiver.writable && !receiver.suspended);
    const auto& mouse = receiver.devices.at(0);
    CHECK(mouse.device_id == "pointer-00cd" && mouse.kind == DeviceKind::Mouse && mouse.product_id == 0x00cd);
    CHECK(mouse.name == "Razer Basilisk V3 Pro 35K" && mouse.support == Support::Tested);
    CHECK(mouse.presence == Presence::Online && mouse.contact == Contact::OnMat && !mouse.controlled_by);
    CHECK(mouse.battery.level == 79 && mouse.battery.charge == Charge::Charging && !mouse.battery.stale);
    CHECK(mouse.battery.updated_ms == 5410002u);
    const auto& keyboard = receiver.devices.at(1);
    CHECK(keyboard.presence == Presence::Asleep && keyboard.controlled_by == "OpenRazer");
    CHECK(keyboard.battery.level == 64 && !keyboard.battery.charge && keyboard.battery.stale);

    const auto waited = wire::wait_result(reply("wait-reply"));
    CHECK(waited.state == 42 && waited.routes == 7 && !waited.timed_out);

    const auto routes = wire::route_list(reply("list-routes-reply"));
    CHECK(routes.revision == 7 && routes.routes.size() == 2);
    CHECK(routes.routes.at(0).device_id == "keyboard-0296" && routes.routes.at(0).kind == DeviceKind::Keyboard);
    CHECK(routes.routes.at(1).token == std::string(token) && routes.routes.at(1).available);
    CHECK(routes.routes.at(1).access == Access::Qualified && routes.routes.at(1).lane == Lane::Pointer);

    const auto granted = wire::lease_result(reply("acquire-lease-reply"));
    CHECK(granted.granted && granted.granted->id == lease && granted.granted->expires_ms == 5443001);
    const auto busy = wire::lease_result(reply("acquire-lease-busy-reply"));
    CHECK(!busy.granted && busy.holder == "OpenRazer");
    const auto renewed = wire::lease_renewed(reply("renew-lease-reply"));
    CHECK(renewed.id == lease && renewed.expires_ms == 5458210);
    wire::empty(reply("release-lease-reply"));

    const auto matched = wire::transact_result(reply("transact-reply"));
    CHECK(matched.outcome == Outcome::Matched && matched.code.empty());
    CHECK(matched.response[0] == 0x02 && matched.response[1] == 0x1f && matched.response[9] == 0xca);
    const auto not_sent = wire::transact_result(reply("transact-not-sent-reply"));
    CHECK(not_sent.outcome == Outcome::NotSent && not_sent.code == "HFX-105");
    const auto maybe = wire::transact_result(reply("transact-may-have-been-sent-reply"));
    CHECK(maybe.outcome == Outcome::MayHaveBeenSent && maybe.code == "HFX-110");
}

void all_fixtures_covered()
{
    // bench, capture and probe-receiver are hyperfluxctl's. This SDK doesn't speak them.
    std::set<std::string> left;
    for(const auto& entry : std::filesystem::directory_iterator(HFX_FIXTURES))
    {
        const auto name = entry.path().stem().string();
        if(!covered.contains(name) && !name.starts_with("bench-") && !name.starts_with("capture-") && !name.starts_with("probe-"))
        {
            left.insert(name);
        }
    }
    for(const auto& name : left)
    {
        std::cerr << "fixture " << name << " isn't checked\n";
    }
    CHECK(left.empty());
}

void contradictions()
{
    const auto bad = [](const std::string& text)
    { EXPECT_ERROR(Error::Kind::Protocol, "", static_cast<void>(wire::result(text, "r1"))); };
    bad(R"({"id":"r2","result":{}})");
    bad(R"({"id":"r1","result":{},"error":{"code":"HFX-102","message":"x"}})");
    bad(R"({"id":"r1"})");
    bad(R"({"id":"r1","result":{},"extra":1})");
    bad("not json");

    const auto decode = [](auto decoder, const std::string& text)
    { EXPECT_ERROR(Error::Kind::Protocol, "", static_cast<void>(decoder(wire::Json::parse(text)))); };
    decode(wire::transact_result, R"({"outcome":"matched","response":null,"code":null})");
    decode(wire::transact_result, R"({"outcome":"not-sent","response":null,"code":null})");
    decode(wire::lease_result, R"({"outcome":"granted","lease_id":null,"expires_ms":null,"holder":null})");
    decode(wire::wait_result, R"({"state":"01","routes":"7","timed_out":false})");
    decode(wire::wait_result, R"({"state":7,"routes":"7","timed_out":false})");
    decode(wire::welcome, R"({"version":1,"server_instance_id":"b","bridge_version":"1","features":["wait","bench"]})");

    auto routes = wire::Json::parse(fixture("list-routes-reply"))["result"];
    routes["routes"][0]["state"] = "unavailable";
    decode(wire::route_list, routes.dump());
    routes = wire::Json::parse(fixture("list-routes-reply"))["result"];
    std::swap(routes["routes"][0], routes["routes"][1]);
    decode(wire::route_list, routes.dump());

    // Missing nullable fields read as null.
    const auto busy = wire::lease_result(wire::Json::parse(R"({"outcome":"busy","holder":"OpenRGB"})"));
    CHECK(!busy.granted && busy.holder == "OpenRGB");
}

// ---------------------------------------------------------------- fake bridge

void write_frame(int fd, const std::string& payload)
{
    const auto size = static_cast<std::uint32_t>(payload.size());
    std::string frame {static_cast<char>(size >> 24), static_cast<char>(size >> 16), static_cast<char>(size >> 8),
                       static_cast<char>(size)};
    frame += payload;
    CHECK(::send(fd, frame.data(), frame.size(), MSG_NOSIGNAL) == static_cast<ssize_t>(frame.size()));
}

bool read_exact(int fd, char* data, std::size_t size)
{
    std::size_t got = 0;
    while(got < size)
    {
        const ssize_t n = ::recv(fd, data + got, size - got, 0);
        if(n <= 0)
        {
            return false;
        }
        got += static_cast<std::size_t>(n);
    }
    return true;
}

using Answer = std::function<std::optional<std::string>(const wire::Json& request)>;

// Answers each request with `answer` (no reply when it returns nothing)
// until the client goes away.
class FakeBridge
{
public:
    explicit FakeBridge(Answer answer)
    {
        int fds[2];
        CHECK(::socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, fds) == 0);
        client_fd = fds[0];
        thread_ = std::thread([fd = fds[1], answer = std::move(answer)]
        {
            unsigned char prefix[4];
            while(read_exact(fd, reinterpret_cast<char*>(prefix), 4))
            {
                std::string payload((std::size_t {prefix[0]} << 24) | (prefix[1] << 16) | (prefix[2] << 8) | prefix[3],
                                    '\0');
                if(!read_exact(fd, payload.data(), payload.size()))
                {
                    break;
                }
                if(const auto text = answer(wire::Json::parse(payload)))
                {
                    write_frame(fd, *text);
                }
            }
            ::close(fd);
        });
    }

    ~FakeBridge() { thread_.join(); }

    int client_fd = -1;

private:
    std::thread thread_;
};

std::string answer_with(const wire::Json& request, const std::string& name)
{
    auto message = wire::Json::parse(fixture(name));
    message["id"] = request.at("id");
    return message.dump();
}

ClientConfig test_config()
{
    return {unique_client_id("sdk-test"), "SDK test", {"native-routes"}, {"wait"}, 2000ms};
}

void conversation()
{
    FakeBridge bridge([](const wire::Json& request) -> std::optional<std::string>
    {
        const auto method = request.at("method").get<std::string>();
        if(method == "hello")
        {
            CHECK(request["params"]["client_id"].get<std::string>().starts_with("sdk-test-"));
            return answer_with(request, "hello-reply");
        }
        if(method == "snapshot")
        {
            return answer_with(request, "snapshot-reply");
        }
        if(method == "acquire-lease")
        {
            return answer_with(request, "acquire-lease-busy-reply");
        }
        if(method == "transact")
        {
            CHECK(request["params"]["report"] == wire::hex(battery_query()));
            return answer_with(request, "transact-reply");
        }
        return std::nullopt;
    });
    {
        auto client = Client::handshake(bridge.client_fd, test_config());
        CHECK(client.welcome().has("wait"));
        CHECK(client.snapshot().receivers.at(0).devices.size() == 2);
        CHECK(client.acquire_lease(token, 30000ms).holder == "OpenRazer");
        CHECK(client.transact(lease, token, battery_query(), 1000ms).response[9] == 0xca);
        CHECK(client.usable());
    }
}

void bridge_error_keeps_connection()
{
    FakeBridge bridge([](const wire::Json& request) -> std::optional<std::string>
    {
        const auto method = request.at("method").get<std::string>();
        if(method == "hello")
        {
            return answer_with(request, "hello-reply");
        }
        if(method == "acquire-lease")
        {
            wire::Json error {{"id", request["id"]},
                              {"error", {{"code", "HFX-107"}, {"message", "The receiver reconnected."}}}};
            return error.dump();
        }
        return answer_with(request, "snapshot-reply");
    });
    auto client = Client::handshake(bridge.client_fd, test_config());
    EXPECT_ERROR(Error::Kind::Bridge, "HFX-107", static_cast<void>(client.acquire_lease(token, 30000ms)));
    CHECK(client.usable());
    CHECK(client.snapshot().revision == 41);
}

void wrong_reply_id_ends_connection()
{
    FakeBridge bridge([](const wire::Json& request) -> std::optional<std::string>
    {
        if(request.at("method") == "hello")
        {
            return answer_with(request, "hello-reply");
        }
        auto message = wire::Json::parse(fixture("snapshot-reply"));
        message["id"] = "somebody-else";
        return message.dump();
    });
    auto client = Client::handshake(bridge.client_fd, test_config());
    EXPECT_ERROR(Error::Kind::Protocol, "", static_cast<void>(client.snapshot()));
    CHECK(!client.usable());
    EXPECT_ERROR(Error::Kind::Io, "", static_cast<void>(client.snapshot()));
}

void interrupt_wakes_wait()
{
    FakeBridge bridge([](const wire::Json& request) -> std::optional<std::string>
    {
        if(request.at("method") == "hello")
        {
            return answer_with(request, "hello-reply");
        }
        return std::nullopt; // a wait that never ends
    });
    auto client = Client::handshake(bridge.client_fd, test_config());
    std::thread stopper([&client] { std::this_thread::sleep_for(50ms); client.interrupt(); });
    const auto started = std::chrono::steady_clock::now();
    EXPECT_ERROR(Error::Kind::Io, "", static_cast<void>(client.wait(1, 1, 30000ms)));
    CHECK(std::chrono::steady_clock::now() - started < 5s);
    stopper.join();
}

void hello_refusal()
{
    FakeBridge bridge([](const wire::Json& request) -> std::optional<std::string>
    { return answer_with(request, "hello-error-version"); });
    EXPECT_ERROR(Error::Kind::Bridge, "HFX-101", static_cast<void>(Client::handshake(bridge.client_fd, test_config())));
}

void peer_check()
{
    auto dir = std::filesystem::temp_directory_path() / ("hyperflux-sdk-test-" + std::to_string(::getpid()));
    std::filesystem::create_directories(dir);
    const auto path = (dir / "bridge.sock").string();
    const int listener = ::socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    sockaddr_un address {};
    address.sun_family = AF_UNIX;
    std::snprintf(address.sun_path, sizeof(address.sun_path), "%s", path.c_str());
    CHECK(::bind(listener, reinterpret_cast<sockaddr*>(&address), sizeof(address)) == 0);
    CHECK(::listen(listener, 4) == 0);
    const Endpoint wrong {path, Endpoint::Peer::Uid, static_cast<unsigned>(::getuid()) + 1};
    EXPECT_ERROR(Error::Kind::Peer, "", static_cast<void>(Client::connect(wrong, test_config())));
    const Endpoint missing {(dir / "missing.sock").string(), Endpoint::Peer::Any, 0};
    EXPECT_ERROR(Error::Kind::Connect, "", static_cast<void>(Client::connect(missing, test_config())));
    ::close(listener);
    std::filesystem::remove_all(dir);
}

void socket_from_environment()
{
    ::setenv("HYPERFLUX_NEXT_SOCKET", "/tmp/somewhere.sock", 1);
    const auto chosen = socket_candidates();
    CHECK(chosen.size() == 1 && chosen[0].path == "/tmp/somewhere.sock" && chosen[0].peer == Endpoint::Peer::Any);
    ::unsetenv("HYPERFLUX_NEXT_SOCKET");
    const auto defaults = socket_candidates();
    CHECK(defaults.back().path == "/run/hyperflux-next/bridge.sock");
    CHECK(defaults.back().peer == Endpoint::Peer::SocketOwner);
    if(::geteuid() != 0)
    {
        CHECK(defaults.size() == 2 && defaults[0].peer == Endpoint::Peer::Uid && defaults[0].uid == 0);
    }
    CHECK(unique_client_id("x") != unique_client_id("x"));
}

} // namespace

int main()
{
    requests_match_fixtures();
    replies_decode();
    contradictions();
    conversation();
    bridge_error_keeps_connection();
    wrong_reply_id_ends_connection();
    interrupt_wakes_wait();
    hello_refusal();
    all_fixtures_covered();
    peer_check();
    socket_from_environment();
    if(failures != 0)
    {
        std::cerr << failures << " check(s) failed\n";
        return 1;
    }
    std::cout << "all SDK checks passed\n";
    return 0;
}
