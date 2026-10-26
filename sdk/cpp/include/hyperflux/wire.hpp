// SPDX-License-Identifier: GPL-2.0-only
//
// JSON encoding of protocol version 1. Client uses these. They're public so
// tests can hold them against the golden messages in
// crates/hfx-proto/tests/fixtures.

#pragma once

#include <hyperflux/client.hpp>

#include <nlohmann/json.hpp>

#include <optional>
#include <string>
#include <string_view>

namespace hyperflux::wire
{

using Json = nlohmann::json;
// Field order on the wire follows docs/protocol.md.
using Params = nlohmann::ordered_json;

inline constexpr std::size_t max_message_bytes = 1'048'576;

[[nodiscard]] std::string request(std::string_view id, std::string_view method, const Params& params);

[[nodiscard]] Params hello_params(const ClientConfig& config);
[[nodiscard]] Params wait_params(std::optional<std::uint64_t> state_after,
                                 std::optional<std::uint64_t> routes_after,
                                 Milliseconds timeout);
[[nodiscard]] Params acquire_lease_params(const std::string& route_token, Milliseconds duration);
[[nodiscard]] Params renew_lease_params(const std::string& lease_id, Milliseconds duration);
[[nodiscard]] Params release_lease_params(const std::string& lease_id);
[[nodiscard]] Params transact_params(const std::string& lease_id,
                                     const std::string& route_token,
                                     const Report& report,
                                     Milliseconds timeout);

// The result object of a reply to request `id`. An error reply throws
// Error::Kind::Bridge, and anything malformed throws Error::Kind::Protocol.
[[nodiscard]] Json result(std::string_view payload, std::string_view id);

[[nodiscard]] Welcome welcome(const Json& result);
[[nodiscard]] Snapshot snapshot(const Json& result);
[[nodiscard]] WaitResult wait_result(const Json& result);
[[nodiscard]] RouteList route_list(const Json& result);
[[nodiscard]] LeaseResult lease_result(const Json& result);
[[nodiscard]] Lease lease_renewed(const Json& result);
[[nodiscard]] TransactResult transact_result(const Json& result);
void empty(const Json& result);

[[nodiscard]] std::string hex(const Report& report);
[[nodiscard]] std::optional<Report> parse_report(std::string_view text);

} // namespace hyperflux::wire
