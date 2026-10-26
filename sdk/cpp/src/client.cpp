// SPDX-License-Identifier: GPL-2.0-only

#include <hyperflux/client.hpp>
#include <hyperflux/wire.hpp>

#include <algorithm>
#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <random>
#include <utility>

#include <poll.h>
#include <sys/random.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

namespace hyperflux
{
namespace
{

using Clock = std::chrono::steady_clock;

constexpr const char* system_socket = "/run/hyperflux-next/bridge.sock";
constexpr const char* socket_env = "HYPERFLUX_NEXT_SOCKET";

std::string errno_text(int error)
{
    return std::strerror(error);
}

// Waits until the fd is ready or the deadline passes. Returns false on
// timeout, and poll errors other than EINTR throw.
bool ready(int fd, short events, Clock::time_point deadline)
{
    for(;;)
    {
        const auto left = std::chrono::duration_cast<Milliseconds>(deadline - Clock::now()).count();
        if(left <= 0)
            return false;
        pollfd entry {fd, events, 0};
        const int result = ::poll(&entry, 1, static_cast<int>(std::min<long long>(left, 60'000)));
        if(result > 0)
            return true;
        if(result < 0 && errno != EINTR)
            throw Error(Error::Kind::Io, "poll on the bridge socket failed: " + errno_text(errno));
    }
}

void check_peer(int fd, const Endpoint& endpoint)
{
    unsigned expected = endpoint.uid;
    if(endpoint.peer == Endpoint::Peer::Any)
        return;
    if(endpoint.peer == Endpoint::Peer::SocketOwner)
    {
        struct stat info {};
        if(::stat(endpoint.path.c_str(), &info) != 0)
            throw Error(Error::Kind::Connect, "can't stat " + endpoint.path + ": " + errno_text(errno));
        expected = info.st_uid;
    }
    ucred peer {};
    socklen_t size = sizeof(peer);
    if(::getsockopt(fd, SOL_SOCKET, SO_PEERCRED, &peer, &size) != 0)
        throw Error(Error::Kind::Connect, "can't read the bridge socket's peer: " + errno_text(errno));
    if(peer.uid != expected)
    {
        throw Error(Error::Kind::Peer,
                    endpoint.path + " belongs to uid " + std::to_string(peer.uid) + ", expected "
                        + std::to_string(expected));
    }
}

} // namespace

Error::Error(Kind kind, const std::string& message, std::string code)
    : std::runtime_error(message), kind_(kind), code_(std::move(code))
{
}

bool Welcome::has(std::string_view feature) const
{
    return std::find(features.begin(), features.end(), feature) != features.end();
}

std::vector<Endpoint> socket_candidates()
{
    if(const char* path = std::getenv(socket_env); path != nullptr && *path != '\0')
    {
        return {{path, Endpoint::Peer::Any, 0}};
    }
    const Endpoint system {system_socket, Endpoint::Peer::SocketOwner, 0};
    const auto uid = static_cast<unsigned>(::geteuid());
    if(uid == 0)
    {
        return {system};
    }
    // systemd creates the per-user socket: the listener there is root.
    return {{"/run/user/" + std::to_string(uid) + "/hyperflux-next-bridge.sock", Endpoint::Peer::Uid, 0}, system};
}

std::string unique_client_id(std::string_view prefix)
{
    std::uint32_t random = 0;
    if(::getrandom(&random, sizeof(random), GRND_NONBLOCK) != static_cast<ssize_t>(sizeof(random)))
    {
        random = std::random_device {}();
    }
    char suffix[9];
    std::snprintf(suffix, sizeof(suffix), "%08x", random);
    return std::string(prefix) + "-" + std::to_string(::getpid()) + "-" + suffix;
}

Client::Client(int fd, Milliseconds timeout) noexcept : fd_(fd), timeout_(timeout) {}

Client::Client(Client&& other) noexcept
    : fd_(std::exchange(other.fd_, -1)),
      timeout_(other.timeout_),
      next_id_(other.next_id_),
      broken_(other.broken_.load()),
      welcome_(std::move(other.welcome_))
{
}

Client& Client::operator=(Client&& other) noexcept
{
    if(this != &other)
    {
        close();
        fd_ = std::exchange(other.fd_, -1);
        timeout_ = other.timeout_;
        next_id_ = other.next_id_;
        broken_ = other.broken_.load();
        welcome_ = std::move(other.welcome_);
    }
    return *this;
}

Client::~Client()
{
    close();
}

void Client::close() noexcept
{
    if(fd_ >= 0)
    {
        ::close(fd_);
        fd_ = -1;
    }
}

template<typename Result, typename Params, typename Decode>
Result Client::call(std::string_view method, const Params& params, Milliseconds extra, Decode decode)
{
    if(!usable())
        throw Error(Error::Kind::Io, "the connection to the bridge is closed");
    const std::string id = "r" + std::to_string(next_id_++);
    const auto deadline = Clock::now() + timeout_ + extra;
    try
    {
        send_frame(wire::request(id, method, params), deadline);
        return decode(wire::result(read_frame(deadline), id));
    }
    catch(const Error& error)
    {
        // After a bridge error the stream is still in step. After anything
        // else it isn't, and the next reply could belong to this request.
        if(error.kind() != Error::Kind::Bridge)
            broken_ = true;
        throw;
    }
}

Client Client::connect(const Endpoint& endpoint, const ClientConfig& config)
{
    sockaddr_un address {};
    address.sun_family = AF_UNIX;
    if(endpoint.path.empty() || endpoint.path.size() >= sizeof(address.sun_path))
        throw Error(Error::Kind::Connect, "the bridge socket path is empty or too long");
    std::memcpy(address.sun_path, endpoint.path.c_str(), endpoint.path.size() + 1);
    const int fd = ::socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if(fd < 0)
        throw Error(Error::Kind::Connect, "can't create a socket: " + errno_text(errno));
    if(::connect(fd, reinterpret_cast<const sockaddr*>(&address), sizeof(address)) != 0)
    {
        const int error = errno;
        ::close(fd);
        throw Error(Error::Kind::Connect, "can't connect to " + endpoint.path + ": " + errno_text(error));
    }
    try
    {
        check_peer(fd, endpoint);
    }
    catch(...)
    {
        ::close(fd);
        throw;
    }
    return handshake(fd, config);
}

Client Client::connect_default(const ClientConfig& config)
{
    std::optional<Error> last;
    for(const auto& endpoint : socket_candidates())
    {
        try
        {
            return connect(endpoint, config);
        }
        catch(const Error& error)
        {
            // A missing or refused socket means "try the next one". Anything
            // else is an answer from a bridge and stands.
            if(error.kind() != Error::Kind::Connect)
                throw;
            last = error;
        }
    }
    throw last.value_or(Error(Error::Kind::Connect, "no bridge socket to try"));
}

Client Client::handshake(int fd, const ClientConfig& config)
{
    Client client(fd, config.timeout);
    client.welcome_ = client.call<Welcome>("hello", wire::hello_params(config), Milliseconds::zero(), wire::welcome);
    return client;
}

void Client::interrupt() noexcept
{
    broken_ = true;
    if(fd_ >= 0)
        ::shutdown(fd_, SHUT_RDWR);
}

bool Client::usable() const noexcept
{
    return fd_ >= 0 && !broken_;
}

void Client::send_frame(const std::string& payload, Clock::time_point deadline)
{
    if(payload.empty() || payload.size() > wire::max_message_bytes)
        throw Error(Error::Kind::Protocol, "request of " + std::to_string(payload.size()) + " bytes can't be framed");
    // Prefix and payload go out together. A failed write never leaves half
    // a frame behind for the next request to collide with.
    const auto size = static_cast<std::uint32_t>(payload.size());
    std::string frame {static_cast<char>(size >> 24), static_cast<char>(size >> 16),
                       static_cast<char>(size >> 8), static_cast<char>(size)};
    frame += payload;
    std::size_t sent = 0;
    while(sent < frame.size())
    {
        if(!ready(fd_, POLLOUT, deadline))
            throw Error(Error::Kind::Io, "timed out sending to the bridge");
        const ssize_t written = ::send(fd_, frame.data() + sent, frame.size() - sent, MSG_NOSIGNAL);
        if(written < 0 && errno != EINTR && errno != EAGAIN)
            throw Error(Error::Kind::Io, "sending to the bridge failed: " + errno_text(errno));
        sent += written > 0 ? static_cast<std::size_t>(written) : 0;
    }
}

std::string Client::read_frame(Clock::time_point deadline)
{
    const auto fill = [&](char* data, std::size_t size)
    {
        std::size_t got = 0;
        while(got < size)
        {
            if(!ready(fd_, POLLIN, deadline))
                throw Error(Error::Kind::Io, "timed out waiting for the bridge");
            const ssize_t received = ::recv(fd_, data + got, size - got, 0);
            if(received == 0)
                throw Error(Error::Kind::Io, "the bridge closed the connection");
            if(received < 0 && errno != EINTR && errno != EAGAIN)
                throw Error(Error::Kind::Io, "reading from the bridge failed: " + errno_text(errno));
            got += received > 0 ? static_cast<std::size_t>(received) : 0;
        }
    };
    unsigned char prefix[4];
    fill(reinterpret_cast<char*>(prefix), sizeof(prefix));
    const std::size_t size = (std::size_t {prefix[0]} << 24) | (std::size_t {prefix[1]} << 16)
                           | (std::size_t {prefix[2]} << 8) | prefix[3];
    if(size == 0 || size > wire::max_message_bytes)
        throw Error(Error::Kind::Protocol, "the bridge announced a frame of " + std::to_string(size) + " bytes");
    std::string payload(size, '\0');
    fill(payload.data(), size);
    return payload;
}

Snapshot Client::snapshot()
{
    return call<Snapshot>("snapshot", wire::Params::object(), Milliseconds::zero(), wire::snapshot);
}

WaitResult Client::wait(std::optional<std::uint64_t> state_after,
                        std::optional<std::uint64_t> routes_after,
                        Milliseconds timeout)
{
    return call<WaitResult>("wait", wire::wait_params(state_after, routes_after, timeout), timeout, wire::wait_result);
}

RouteList Client::list_routes()
{
    return call<RouteList>("list-routes", wire::Params::object(), Milliseconds::zero(), wire::route_list);
}

LeaseResult Client::acquire_lease(const std::string& route_token, Milliseconds duration)
{
    return call<LeaseResult>("acquire-lease", wire::acquire_lease_params(route_token, duration),
                             Milliseconds::zero(), wire::lease_result);
}

Lease Client::renew_lease(const std::string& lease_id, Milliseconds duration)
{
    return call<Lease>("renew-lease", wire::renew_lease_params(lease_id, duration), Milliseconds::zero(),
                       wire::lease_renewed);
}

void Client::release_lease(const std::string& lease_id)
{
    call<void>("release-lease", wire::release_lease_params(lease_id), Milliseconds::zero(), wire::empty);
}

TransactResult Client::transact(const std::string& lease_id,
                                const std::string& route_token,
                                const Report& report,
                                Milliseconds timeout)
{
    return call<TransactResult>("transact", wire::transact_params(lease_id, route_token, report, timeout), timeout,
                                wire::transact_result);
}

} // namespace hyperflux
