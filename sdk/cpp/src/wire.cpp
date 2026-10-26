// SPDX-License-Identifier: GPL-2.0-only

#include <hyperflux/wire.hpp>

#include <algorithm>
#include <array>
#include <initializer_list>
#include <tuple>

namespace hyperflux::wire
{
namespace
{

[[noreturn]] void violation(const std::string& what)
{
    throw Error(Error::Kind::Protocol, "the bridge sent an invalid message: " + what);
}

template<std::size_t N>
using Words = std::array<std::string_view, N>;

constexpr Words<2> lanes {"pointer", "keyboard"};
constexpr Words<3> kinds {"mouse", "keyboard", "unknown"};
constexpr Words<4> supports {"tested", "checked", "listed", "unknown"};
constexpr Words<4> presences {"online", "asleep", "unreachable", "waiting"};
constexpr Words<2> contacts {"on-mat", "off-mat"};
constexpr Words<2> charges {"charging", "discharging"};
constexpr Words<2> unreporteds {"charge", "battery"};
constexpr Words<2> receiver_states {"active", "suspended"};
constexpr Words<2> route_states {"available", "unavailable"};
constexpr Words<3> accesses {"qualified", "unsupported", "unknown"};
constexpr Words<2> lease_outcomes {"granted", "busy"};
constexpr Words<3> outcomes {"matched", "not-sent", "may-have-been-sent"};
constexpr Words<1> transports {"hidraw"};

// u64 values travel as canonical decimal strings: digits only, no leading
// zero except "0".
std::optional<std::uint64_t> parse_decimal(const std::string& text)
{
    if(text.empty() || text.size() > 20 || (text.size() > 1 && text[0] == '0'))
        return std::nullopt;
    std::uint64_t value = 0;
    for(const char digit : text)
    {
        const auto add = static_cast<std::uint64_t>(digit - '0');
        if(digit < '0' || digit > '9' || value > (UINT64_MAX - add) / 10)
            return std::nullopt;
        value = value * 10 + add;
    }
    return value;
}

// One object of a reply. Fields it doesn't list are rejected, as the
// bridge rejects them in requests. A nullable field may also be missing.
class Fields
{
public:
    Fields(const Json& value, std::initializer_list<std::string_view> names, std::string what)
        : value_(value), what_(std::move(what))
    {
        if(!value.is_object())
            violation(what_ + " is not an object");
        for(const auto& item : value.items())
            if(std::find(names.begin(), names.end(), item.key()) == names.end())
                violation(what_ + " has an unknown field \"" + item.key() + "\"");
    }

    const Json* get(const char* name) const
    {
        const auto found = value_.find(name);
        return found == value_.end() || found->is_null() ? nullptr : &*found;
    }

    const Json& need(const char* name) const
    {
        const Json* found = get(name);
        if(found == nullptr)
            fail(name, "is missing");
        return *found;
    }

    std::optional<std::string> opt_text(const char* name, std::size_t min = 1, std::size_t max = 128) const
    {
        const Json* found = get(name);
        if(found == nullptr)
            return std::nullopt;
        if(!found->is_string())
            fail(name, "is not a string");
        auto text = found->get<std::string>();
        if(text.size() < min || text.size() > max)
            fail(name, "has a length out of bounds");
        return text;
    }

    std::string text(const char* name, std::size_t min = 1, std::size_t max = 128) const
    {
        need(name);
        return *opt_text(name, min, max);
    }

    std::optional<std::uint64_t> opt_decimal(const char* name, std::uint64_t min = 0) const
    {
        const auto text = opt_text(name, 1, 20);
        if(!text)
            return std::nullopt;
        const auto value = parse_decimal(*text);
        if(!value || *value < min)
            fail(name, "is not a canonical decimal string");
        return value;
    }

    std::uint64_t decimal(const char* name, std::uint64_t min = 0) const
    {
        need(name);
        return *opt_decimal(name, min);
    }

    std::optional<std::int64_t> opt_integer(const char* name, std::int64_t min, std::int64_t max) const
    {
        const Json* found = get(name);
        if(found == nullptr)
            return std::nullopt;
        // A fraction or exponent makes it a float, which the protocol forbids.
        if(!found->is_number_integer() || (found->is_number_unsigned() && found->get<std::uint64_t>() > INT64_MAX))
            fail(name, "is not an integer");
        const auto value = found->get<std::int64_t>();
        if(value < min || value > max)
            fail(name, "is out of range");
        return value;
    }

    std::int64_t integer(const char* name, std::int64_t min, std::int64_t max) const
    {
        need(name);
        return *opt_integer(name, min, max);
    }

    std::uint16_t u16(const char* name) const { return static_cast<std::uint16_t>(integer(name, 0, 65535)); }

    bool flag(const char* name) const
    {
        if(!need(name).is_boolean())
            fail(name, "is not a boolean");
        return need(name).get<bool>();
    }

    template<typename Enum, std::size_t N>
    std::optional<Enum> opt_word(const char* name, const Words<N>& words) const
    {
        const auto text = opt_text(name, 1, 64);
        if(!text)
            return std::nullopt;
        const auto found = std::find(words.begin(), words.end(), *text);
        if(found == words.end())
            fail(name, "has the unknown value \"" + *text + "\"");
        return static_cast<Enum>(found - words.begin());
    }

    template<typename Enum, std::size_t N>
    Enum word(const char* name, const Words<N>& words) const
    {
        need(name);
        return *opt_word<Enum>(name, words);
    }

    const Json& array(const char* name, std::size_t max) const
    {
        const Json& found = need(name);
        if(!found.is_array() || found.size() > max)
            fail(name, "is not an array of at most " + std::to_string(max));
        return found;
    }

private:
    [[noreturn]] void fail(const char* name, const std::string& problem) const
    {
        violation(what_ + "." + name + " " + problem);
    }

    const Json& value_;
    std::string what_;
};

std::int64_t count(Milliseconds value)
{
    return static_cast<std::int64_t>(value.count());
}

Battery battery(const Json& value)
{
    const Fields f(value, {"level", "charge", "updated_ms", "stale", "rising", "unreported"}, "battery");
    Battery result;
    if(const auto level = f.opt_integer("level", 0, 100))
        result.level = static_cast<int>(*level);
    result.charge = f.opt_word<Charge>("charge", charges);
    result.updated_ms = f.opt_decimal("updated_ms");
    result.stale = f.flag("stale");
    result.rising = f.get("rising") && f.flag("rising");
    result.unreported = f.opt_word<Unreported>("unreported", unreporteds);
    return result;
}

Device device(const Json& value)
{
    const Fields f(value,
                   {"device_id", "lane", "kind", "vendor_id", "product_id", "name", "support", "presence",
                    "last_seen_ms", "contact", "controllable", "controlled_by", "battery", "first_get", "refused",
                    "slot", "openrazer", "exchanges"},
                   "device");
    Device result;
    result.device_id = f.text("device_id");
    result.lane = f.word<Lane>("lane", lanes);
    result.kind = f.word<DeviceKind>("kind", kinds);
    result.vendor_id = f.u16("vendor_id");
    result.product_id = f.u16("product_id");
    result.name = f.opt_text("name");
    result.support = f.word<Support>("support", supports);
    result.presence = f.word<Presence>("presence", presences);
    result.last_seen_ms = f.opt_decimal("last_seen_ms");
    result.contact = f.opt_word<Contact>("contact", contacts);
    result.controllable = f.flag("controllable");
    result.controlled_by = f.opt_text("controlled_by");
    result.battery = battery(f.need("battery"));
    return result;
}

Receiver receiver(const Json& value)
{
    const Fields f(value,
                   {"receiver_id", "generation_id", "vendor_id", "product_id", "state", "writable", "devices",
                    "inventory"},
                   "receiver");
    Receiver result;
    result.receiver_id = f.text("receiver_id");
    result.generation_id = f.decimal("generation_id", 1);
    result.vendor_id = f.u16("vendor_id");
    result.product_id = f.u16("product_id");
    result.suspended = f.word<std::size_t>("state", receiver_states) == 1;
    result.writable = f.flag("writable");
    for(const auto& item : f.array("devices", 2))
        result.devices.push_back(device(item));
    return result;
}

Route route(const Json& value)
{
    const Fields f(value,
                   {"route_token", "receiver_id", "generation_id", "device_id", "lane", "kind", "vendor_id",
                    "product_id", "state", "access", "report_bytes"},
                   "route");
    Route result;
    result.token = f.opt_text("route_token", 64, 64);
    result.receiver_id = f.text("receiver_id");
    result.generation_id = f.decimal("generation_id", 1);
    result.device_id = f.text("device_id");
    result.lane = f.word<Lane>("lane", lanes);
    result.kind = f.word<DeviceKind>("kind", kinds);
    result.vendor_id = f.u16("vendor_id");
    result.product_id = f.u16("product_id");
    result.available = f.word<std::size_t>("state", route_states) == 0;
    result.access = f.word<Access>("access", accesses);
    if(f.integer("report_bytes", 0, 65535) != static_cast<std::int64_t>(report_bytes))
        violation("routes carry 90-byte reports");
    if(result.token.has_value() != (result.available && result.access == Access::Qualified))
        violation("a route token must be present exactly on qualified available routes");
    return result;
}

} // namespace

std::string request(std::string_view id, std::string_view method, const Params& params)
{
    Params envelope;
    envelope["id"] = id;
    envelope["method"] = method;
    envelope["params"] = params.is_null() ? Params::object() : params;
    return envelope.dump();
}

Params hello_params(const ClientConfig& config)
{
    return {{"client_id", config.client_id},
            {"client_name", config.client_name},
            {"min_version", protocol_version},
            {"max_version", protocol_version},
            {"required_features", config.required_features},
            {"optional_features", config.optional_features}};
}

Params wait_params(std::optional<std::uint64_t> state_after,
                   std::optional<std::uint64_t> routes_after,
                   Milliseconds timeout)
{
    const auto decimal = [](std::optional<std::uint64_t> value)
    { return value ? Params(std::to_string(*value)) : Params(nullptr); };
    return {{"state_after", decimal(state_after)}, {"routes_after", decimal(routes_after)}, {"timeout_ms", count(timeout)}};
}

Params acquire_lease_params(const std::string& route_token, Milliseconds duration)
{
    return {{"route_token", route_token}, {"duration_ms", count(duration)}};
}

Params renew_lease_params(const std::string& lease_id, Milliseconds duration)
{
    return {{"lease_id", lease_id}, {"duration_ms", count(duration)}};
}

Params release_lease_params(const std::string& lease_id)
{
    return {{"lease_id", lease_id}};
}

Params transact_params(const std::string& lease_id,
                       const std::string& route_token,
                       const Report& report,
                       Milliseconds timeout)
{
    return {{"lease_id", lease_id}, {"route_token", route_token}, {"report", hex(report)}, {"timeout_ms", count(timeout)}};
}

Json result(std::string_view payload, std::string_view id)
{
    Json reply = Json::parse(payload.begin(), payload.end(), nullptr, false);
    if(reply.is_discarded())
        violation("the reply is not JSON");
    const Fields f(reply, {"id", "result", "error"}, "reply");
    if(f.text("id") != id)
        violation("the reply id doesn't match the request");
    const Json* value = f.get("result");
    const Json* error = f.get("error");
    if((value == nullptr) == (error == nullptr))
        violation("a reply has exactly one of result and error");
    if(error != nullptr)
    {
        const Fields e(*error, {"code", "message"}, "error");
        throw Error(Error::Kind::Bridge, e.text("message", 1, 512), e.text("code", 1, 32));
    }
    if(!value->is_object())
        violation("the result is not an object");
    return *value;
}

Welcome welcome(const Json& value)
{
    const Fields f(value, {"version", "server_instance_id", "bridge_version", "features"}, "welcome");
    Welcome result;
    result.version = static_cast<int>(f.integer("version", 1, 65535));
    result.server_instance_id = f.text("server_instance_id");
    result.bridge_version = f.text("bridge_version", 1, 64);
    for(const auto& item : f.array("features", 32))
    {
        if(!item.is_string() || item.get<std::string>().empty() || item.get<std::string>().size() > 64)
            violation("welcome.features holds something that isn't a feature name");
        result.features.push_back(item.get<std::string>());
    }
    if(std::adjacent_find(result.features.begin(), result.features.end(), std::greater_equal<>()) != result.features.end())
        violation("welcome features must be sorted and unique");
    // Only version 1 was offered. Any other answer is a broken bridge.
    if(result.version != protocol_version)
        violation("the bridge picked a protocol version this client didn't offer");
    return result;
}

Snapshot snapshot(const Json& value)
{
    const Fields f(value, {"revision", "now_ms", "transport", "receivers"}, "snapshot");
    Snapshot result;
    result.revision = f.decimal("revision");
    result.now_ms = f.decimal("now_ms");
    result.transport = f.word<Transport>("transport", transports);
    for(const auto& item : f.array("receivers", 16))
        result.receivers.push_back(receiver(item));
    return result;
}

WaitResult wait_result(const Json& value)
{
    const Fields f(value, {"state", "routes", "timed_out"}, "wait result");
    return {f.decimal("state"), f.decimal("routes"), f.flag("timed_out")};
}

RouteList route_list(const Json& value)
{
    const Fields f(value, {"revision", "routes"}, "route list");
    RouteList result;
    result.revision = f.decimal("revision");
    for(const auto& item : f.array("routes", 32))
        result.routes.push_back(route(item));
    const auto key = [](const Route& r) { return std::tie(r.receiver_id, r.generation_id, r.device_id); };
    for(std::size_t i = 1; i < result.routes.size(); ++i)
        if(!(key(result.routes[i - 1]) < key(result.routes[i])))
            violation("routes must be sorted and unique");
    return result;
}

LeaseResult lease_result(const Json& value)
{
    const Fields f(value, {"outcome", "lease_id", "expires_ms", "holder"}, "lease result");
    const bool granted = f.word<std::size_t>("outcome", lease_outcomes) == 0;
    const auto id = f.opt_text("lease_id");
    const auto expires = f.opt_decimal("expires_ms");
    LeaseResult result;
    result.holder = f.opt_text("holder");
    if(granted != id.has_value() || granted != expires.has_value() || (granted && result.holder))
        violation("lease result fields contradict its outcome");
    if(granted)
        result.granted = Lease {*id, *expires};
    return result;
}

Lease lease_renewed(const Json& value)
{
    const Fields f(value, {"lease_id", "expires_ms"}, "renewed lease");
    return {f.text("lease_id"), f.decimal("expires_ms")};
}

TransactResult transact_result(const Json& value)
{
    const Fields f(value, {"outcome", "response", "code"}, "transact result");
    TransactResult result;
    result.outcome = f.word<Outcome>("outcome", outcomes);
    const auto response = f.opt_text("response", 180, 180);
    const auto code = f.opt_text("code", 1, 32);
    if((result.outcome == Outcome::Matched) != response.has_value() || response.has_value() == code.has_value())
        violation("transact result fields contradict its outcome");
    if(response)
    {
        const auto bytes = parse_report(*response);
        if(!bytes)
            violation("transact response is not 180 lowercase hex digits");
        result.response = *bytes;
    }
    result.code = code.value_or(std::string());
    return result;
}

void empty(const Json& value)
{
    static_cast<void>(Fields(value, {}, "result"));
}

std::string hex(const Report& report)
{
    static constexpr char digits[] = "0123456789abcdef";
    std::string text;
    for(const auto byte : report)
    {
        text.push_back(digits[byte >> 4]);
        text.push_back(digits[byte & 0x0f]);
    }
    return text;
}

std::optional<Report> parse_report(std::string_view text)
{
    // Lowercase only: the protocol has exactly one spelling of each report.
    const auto nibble = [](char c) { return c >= '0' && c <= '9' ? c - '0' : c >= 'a' && c <= 'f' ? c - 'a' + 10 : -1; };
    if(text.size() != report_bytes * 2)
        return std::nullopt;
    Report report {};
    for(std::size_t i = 0; i < report.size(); ++i)
    {
        const int high = nibble(text[2 * i]);
        const int low = nibble(text[2 * i + 1]);
        if(high < 0 || low < 0)
            return std::nullopt;
        report[i] = static_cast<std::uint8_t>((high << 4) | low);
    }
    return report;
}

} // namespace hyperflux::wire
