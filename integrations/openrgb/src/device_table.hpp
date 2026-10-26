// SPDX-License-Identifier: GPL-2.0-only

#pragma once

#include <hyperflux/client.hpp>

#include <array>
#include <cstdint>
#include <string_view>

namespace hyperflux::openrgb
{

struct TableRow
{
    std::uint16_t product_id; // the device's id through the receiver
    DeviceKind kind;
    std::string_view openrgb_key; // its razer_device record in OpenRGB's RazerDevices.cpp
};

// Devices with an [openrgb] part in devices.toml. tests/test_device_tables.py
// holds this table to the catalog, and tests/check-device-table.py to the
// pinned OpenRGB sources. OpenRGB 1.0, the Qt 5 build's, has no record for
// the rows that need 1.1.
inline constexpr auto device_table = std::to_array<TableRow>({
    {0x00a8, DeviceKind::Mouse, "naga_pro_v2_wireless_device"},
    {0x00ab, DeviceKind::Mouse, "basilisk_v3_pro_wireless_device"},
    {0x00b0, DeviceKind::Mouse, "cobra_pro_wireless_device"},
    {0x00cd, DeviceKind::Mouse, "basilisk_v3_pro_35k_wireless_device"},
    {0x00d7, DeviceKind::Mouse, "basilisk_v3_pro_35k_pg_wireless_device"},
#if HFX_OPENRGB_QT >= 6
    {0x00e8, DeviceKind::Mouse, "naga_pro_v3_wireless_device"},
#endif
    {0x025c, DeviceKind::Keyboard, "blackwidow_v3_pro_wireless_device"},
    {0x0290, DeviceKind::Keyboard, "deathstalker_v2_pro_wireless_device"},
    {0x0296, DeviceKind::Keyboard, "deathstalker_v2_pro_tkl_wireless_device"},
    {0x02d2, DeviceKind::Keyboard, "blackwidow_v4_lowprofile_tkl_wireless_device"},
    {0x02d5, DeviceKind::Keyboard, "blackwidow_v4_tkl_wireless_device"},
});

} // namespace hyperflux::openrgb
