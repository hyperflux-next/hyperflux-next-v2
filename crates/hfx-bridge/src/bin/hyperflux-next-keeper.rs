// SPDX-License-Identifier: GPL-2.0-only

//! hyperflux-next-keeper: see `keeper.rs` and ARCHITECTURE.md.

use hfx_bridge::Source;
use hfx_bridge::keeper::Config;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "Usage: hyperflux-next-keeper [--socket PATH] [--sim PATH]";

const HELP: &str = "\
Reads the receiver's input for the HyperFlux Next V2 bridge and holds the
virtual devices it asks for: the battery devices and the ones OpenRazer
binds. systemd starts it; it takes no configuration file.

  --socket PATH    reach the bridge here instead of /run/hyperflux-next/keeper.sock
  --sim PATH       read the hyperflux-next-sim simulator at PATH, and keep the
                   virtual devices in this process
  --help           show this text
  --version        show the version";

fn main() -> ExitCode {
    let (mut socket, mut sim) = (None, None);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let slot = match arg.as_str() {
            "--socket" => &mut socket,
            "--sim" => &mut sim,
            "--help" | "-h" => {
                println!("{USAGE}\n\n{HELP}");
                return ExitCode::SUCCESS;
            }
            "--version" => {
                println!("{}", hfx_proto::PRODUCT_VERSION);
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("hyperflux-next-keeper: unknown argument {other}\n{USAGE}");
                return ExitCode::FAILURE;
            }
        };
        match args.next() {
            Some(path) if slot.is_none() => *slot = Some(PathBuf::from(path)),
            _ => {
                eprintln!("hyperflux-next-keeper: {arg} needs one path\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }
    let stop = match hfx_bridge::platform::stop_on_signals() {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("hyperflux-next-keeper: can't set up signal handling: {e}");
            return ExitCode::FAILURE;
        }
    };
    // The system bridge runs as its own user. A developer's runs as they do.
    let bridge_uid = match socket {
        None => hfx_bridge::ipc::system_uid(hfx_proto::paths::BRIDGE_USER),
        Some(_) => Some(rustix::process::geteuid().as_raw()),
    };
    let Some(bridge_uid) = bridge_uid else {
        eprintln!(
            "hyperflux-next-keeper: there is no {} user; run systemd-sysusers",
            hfx_proto::paths::BRIDGE_USER
        );
        return ExitCode::FAILURE;
    };
    // A simulated receiver must not put devices on the real desktop.
    let (uhid, hid_drivers) = match sim {
        Some(_) => (hfx_bridge::sim::local_uhid(None, true).0, None),
        None => (
            hfx_bridge::platform::system_uhid(),
            Some(PathBuf::from(hfx_proto::paths::HID_DRIVERS_DIR)),
        ),
    };
    let config = Config {
        source: sim.map_or(Source::System, Source::Sim),
        socket: socket.unwrap_or_else(|| PathBuf::from(hfx_proto::paths::KEEPER_SOCKET)),
        bridge_uid,
        uhid,
        hid_drivers,
        connected: None,
    };
    match hfx_bridge::keeper::run(config, stop) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("hyperflux-next-keeper: {message}");
            ExitCode::FAILURE
        }
    }
}
