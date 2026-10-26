# HyperFlux Next V2 SDKs

Clients for version 1 of the HyperFlux Next V2 bridge's protocol
(`docs/protocol.md`). The packages don't install them. Copy the source
into your project instead.

## C++

`sdk/cpp` covers hello, snapshot, wait, list-routes, leases and transact,
and checks the peer on connect. The OpenRGB plugin uses it. It needs
C++20 and nlohmann/json. Either set `HFX_NLOHMANN_JSON_DIR` to a
directory holding `nlohmann/json.hpp`, or have the `nlohmann_json` CMake
package installed.

```cpp
hyperflux::ClientConfig config {hyperflux::unique_client_id("myapp"), "My app", {}, {"wait"}};
auto client = hyperflux::Client::connect_default(config);
for(const auto& receiver : client.snapshot().receivers)
    for(const auto& device : receiver.devices)
        std::cout << device.name.value_or("unknown") << ": " << device.battery.level.value_or(-1) << "\n";
```

## Python

`sdk/python` covers hello, snapshot, wait and list-routes. It needs
Python 3.11 or later and nothing outside the standard library.

    PYTHONPATH=sdk/python python3 sdk/python/examples/batteries.py --watch

`make sdk-test` runs both test suites. They compare against the fixture
messages in `crates/hfx-proto/tests/fixtures`.
