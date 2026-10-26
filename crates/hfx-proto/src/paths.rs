// SPDX-License-Identifier: GPL-2.0-only

//! Runtime paths and names of an installed system. Install locations are
//! defined once in the top-level Makefile, and these must match a default
//! install (PREFIX=/usr).

use std::path::PathBuf;

pub const RUNTIME_DIR: &str = "/run/hyperflux-next";
pub const SYSTEM_SOCKET: &str = "/run/hyperflux-next/bridge.sock";
pub const BRIDGE_LOCK: &str = "/run/hyperflux-next/bridge.lock";
/// Where the bridge listens for the device keeper.
pub const KEEPER_SOCKET: &str = "/run/hyperflux-next/keeper.sock";
pub const USER_RUNTIME_ROOT: &str = "/run/user";
pub const USER_SOCKET_NAME: &str = "hyperflux-next-bridge.sock";
/// Overrides the socket for all clients (tests, the simulator).
pub const SOCKET_ENV: &str = "HYPERFLUX_NEXT_SOCKET";

pub const UHID_DEVICE: &str = "/dev/uhid";
pub const HID_DRIVERS_DIR: &str = "/sys/bus/hid/drivers";
pub const SYS_MODULE_DIR: &str = "/sys/module";

pub const FULL_COMPONENT: &str = "/usr/share/hyperflux-next/components/full.json";
pub const OPENRGB_PLUGIN: &str = "/usr/lib/openrgb/plugins/hyperflux-next-openrgb.so";

pub const BRIDGE_UNIT: &str = "hyperflux-next-bridge.service";
pub const KEEPER_UNIT: &str = "hyperflux-next-keeper.service";

/// System user the bridge runs as. It owns the receiver's interface 0,
/// and its group interface 4.
pub const BRIDGE_USER: &str = "hyperflux-next";
/// System user the device keeper runs as. Its group may read interfaces 0
/// and 1 and write /dev/uhid.
pub const KEEPER_USER: &str = "hyperflux-keeper";
/// The keeper's status for systemd (`StatusText`) while the bridge has it,
/// and while it waits for one.
pub const KEEPER_CONNECTED: &str = "connected to the bridge";
pub const KEEPER_WAITING: &str = "waiting for the bridge";

#[must_use]
pub fn user_socket(uid: u32) -> PathBuf {
    PathBuf::from(format!("{USER_RUNTIME_ROOT}/{uid}/{USER_SOCKET_NAME}"))
}
