// SPDX-License-Identifier: GPL-2.0-only
//
// A blocking client for the HyperFlux Next V2 bridge, protocol version 1
// (docs/protocol.md). One connection, one request at a time. The only
// call that may come from another thread is interrupt().

#pragma once

#include <array>
#include <atomic>
#include <chrono>
#include <cstdint>
#include <optional>
#include <stdexcept>
#include <string>
#include <string_view>
#include <vector>

namespace hyperflux
{

inline constexpr int protocol_version = 1;
inline constexpr std::size_t report_bytes = 90;
using Report = std::array<std::uint8_t, report_bytes>;
using Milliseconds = std::chrono::milliseconds;

class Error : public std::runtime_error
{
public:
    enum class Kind
    {
        Connect,  // nothing is listening, or no permission to connect
        Peer,     // the socket belongs to someone unexpected
        Io,       // the connection failed, timed out or was interrupted
        Protocol, // the bridge sent something the protocol doesn't allow
        Bridge,   // the bridge answered with an HFX-1xx error
    };

    Error(Kind kind, const std::string& message, std::string code = {});

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    // "HFX-101" and so on, for Kind::Bridge only.
    [[nodiscard]] const std::string& code() const noexcept { return code_; }

private:
    Kind kind_;
    std::string code_;
};

enum class Lane { Pointer, Keyboard };
enum class DeviceKind { Mouse, Keyboard, Unknown };
enum class Support { Tested, Checked, Listed, Unknown };
enum class Presence { Online, Asleep, Unreachable, Waiting };
enum class Contact { OnMat, OffMat };
enum class Charge { Charging, Discharging };
enum class Unreported { Charge, Battery };
enum class Access { Qualified, Unsupported, Unknown };
enum class Outcome { Matched, NotSent, MayHaveBeenSent };
enum class Transport { Hidraw };

struct Welcome
{
    int version = 0;
    std::string server_instance_id;
    std::string bridge_version;
    std::vector<std::string> features;

    [[nodiscard]] bool has(std::string_view feature) const;
};

struct Battery
{
    std::optional<int> level;
    std::optional<Charge> charge;
    std::optional<std::uint64_t> updated_ms;
    bool stale = false;
    bool rising = false;
    std::optional<Unreported> unreported;
};

struct Device
{
    std::string device_id;
    Lane lane = Lane::Pointer;
    DeviceKind kind = DeviceKind::Unknown;
    std::uint16_t vendor_id = 0;
    std::uint16_t product_id = 0;
    std::optional<std::string> name;
    Support support = Support::Unknown;
    Presence presence = Presence::Waiting;
    std::optional<std::uint64_t> last_seen_ms;
    std::optional<Contact> contact;
    bool controllable = false;
    std::optional<std::string> controlled_by;
    Battery battery;
};

struct Receiver
{
    std::string receiver_id;
    std::uint64_t generation_id = 0;
    std::uint16_t vendor_id = 0;
    std::uint16_t product_id = 0;
    bool suspended = false;
    bool writable = false;
    std::vector<Device> devices;
};

struct Snapshot
{
    std::uint64_t revision = 0;
    std::uint64_t now_ms = 0;
    std::vector<Receiver> receivers;
    // How the bridge reaches receivers. Last here, unlike on the wire, so
    // {revision, now_ms, receivers} still builds one.
    Transport transport = Transport::Hidraw;
};

struct WaitResult
{
    std::uint64_t state = 0;
    std::uint64_t routes = 0;
    bool timed_out = false;
};

struct Route
{
    // Set exactly when the route is qualified and available.
    std::optional<std::string> token;
    std::string receiver_id;
    std::uint64_t generation_id = 0;
    std::string device_id;
    Lane lane = Lane::Pointer;
    DeviceKind kind = DeviceKind::Unknown;
    std::uint16_t vendor_id = 0;
    std::uint16_t product_id = 0;
    bool available = false;
    Access access = Access::Unknown;
};

struct RouteList
{
    std::uint64_t revision = 0;
    std::vector<Route> routes;
};

struct Lease
{
    std::string id;
    std::uint64_t expires_ms = 0;
};

struct LeaseResult
{
    // Empty when the device is busy; holder then says who has it.
    std::optional<Lease> granted;
    std::optional<std::string> holder;
};

struct TransactResult
{
    Outcome outcome = Outcome::NotSent;
    Report response {}; // valid when matched
    std::string code;   // HFX code when not matched
};

struct Endpoint
{
    enum class Peer
    {
        Any,
        Uid,         // the listener must run as `uid`
        SocketOwner, // the listener must be the socket file's owner
    };

    std::string path;
    Peer peer = Peer::Any;
    unsigned uid = 0;
};

// HYPERFLUX_NEXT_SOCKET if set. Otherwise root gets the system socket, and
// everyone else the per-user socket first, then the system socket.
[[nodiscard]] std::vector<Endpoint> socket_candidates();

// "<prefix>-<pid>-<8 random hex digits>".
[[nodiscard]] std::string unique_client_id(std::string_view prefix);

struct ClientConfig
{
    std::string client_id;
    std::string client_name;
    std::vector<std::string> required_features;
    std::vector<std::string> optional_features;
    // Per request, with wait and transact adding their own timeout on top.
    Milliseconds timeout {5000};
};

class Client
{
public:
    // Connects, checks the peer and says hello.
    [[nodiscard]] static Client connect(const Endpoint& endpoint, const ClientConfig& config);
    // Tries socket_candidates() in order and returns the first that answers.
    [[nodiscard]] static Client connect_default(const ClientConfig& config);
    // Says hello over a socket that is already connected, and takes the fd.
    [[nodiscard]] static Client handshake(int fd, const ClientConfig& config);

    Client(Client&& other) noexcept;
    Client& operator=(Client&& other) noexcept;
    Client(const Client&) = delete;
    Client& operator=(const Client&) = delete;
    ~Client();

    [[nodiscard]] const Welcome& welcome() const noexcept { return welcome_; }

    [[nodiscard]] Snapshot snapshot();
    // Returns when the state or route revision passes the given value, or
    // after `timeout`. A null revision never triggers.
    [[nodiscard]] WaitResult wait(std::optional<std::uint64_t> state_after,
                                  std::optional<std::uint64_t> routes_after,
                                  Milliseconds timeout);
    [[nodiscard]] RouteList list_routes();
    [[nodiscard]] LeaseResult acquire_lease(const std::string& route_token, Milliseconds duration);
    [[nodiscard]] Lease renew_lease(const std::string& lease_id, Milliseconds duration);
    void release_lease(const std::string& lease_id);
    // Never repeat a report after it came back may-have-been-sent, or after
    // an Io error: it may have reached the device.
    [[nodiscard]] TransactResult transact(const std::string& lease_id,
                                          const std::string& route_token,
                                          const Report& report,
                                          Milliseconds timeout);

    // Makes the call in progress, and every later one, fail with Io.
    void interrupt() noexcept;
    // False after an Io or Protocol error. The connection can't be used again.
    [[nodiscard]] bool usable() const noexcept;

private:
    Client(int fd, Milliseconds timeout) noexcept;
    // Defined and used in client.cpp only.
    template<typename Result, typename Params, typename Decode>
    Result call(std::string_view method, const Params& params, Milliseconds extra, Decode decode);
    void send_frame(const std::string& payload, std::chrono::steady_clock::time_point deadline);
    std::string read_frame(std::chrono::steady_clock::time_point deadline);
    void close() noexcept;

    int fd_ = -1;
    Milliseconds timeout_;
    std::uint64_t next_id_ = 1;
    std::atomic<bool> broken_ {false};
    Welcome welcome_;
};

} // namespace hyperflux
