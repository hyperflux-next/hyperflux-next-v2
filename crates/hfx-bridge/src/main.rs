// SPDX-License-Identifier: GPL-2.0-only

//! hyperflux-next-bridge: see the library documentation and ARCHITECTURE.md.

use hfx_bridge::{Config, Source};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str =
    "Usage: hyperflux-next-bridge [--socket PATH] [--sim PATH] [--read-only] [--no-openrazer]";

const HELP: &str = "\
Serves HyperFlux Next V2 clients, and has the device keeper show device
batteries on the desktop and make the virtual devices OpenRazer binds.
systemd starts it; it takes no configuration file.

  --socket PATH    listen here instead of /run/hyperflux-next/bridge.sock,
                   and for the device keeper beside it, at keeper.sock
  --sim PATH       use the hyperflux-next-sim simulator listening at PATH
  --read-only      never send to a receiver: no batteries, no control
  --no-openrazer   don't create virtual OpenRazer devices
  --help           show this text
  --version        show the version";

struct Args {
    socket: Option<PathBuf>,
    sim: Option<PathBuf>,
    read_only: bool,
    no_openrazer: bool,
}

enum Parsed {
    Run(Args),
    Print(String),
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut parsed = Args {
        socket: None,
        sim: None,
        read_only: false,
        no_openrazer: false,
    };
    while let Some(arg) = args.next() {
        let mut path = |slot: &mut Option<PathBuf>| -> Result<(), String> {
            let value = args.next().ok_or(format!("{arg} needs a path"))?;
            if slot.replace(PathBuf::from(value)).is_some() {
                return Err(format!("{arg} given twice"));
            }
            Ok(())
        };
        match arg.as_str() {
            "--socket" => path(&mut parsed.socket)?,
            "--sim" => path(&mut parsed.sim)?,
            "--read-only" => parsed.read_only = true,
            "--no-openrazer" => parsed.no_openrazer = true,
            "--help" | "-h" => return Ok(Parsed::Print(format!("{USAGE}\n\n{HELP}"))),
            "--version" => return Ok(Parsed::Print(hfx_proto::PRODUCT_VERSION.to_owned())),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    Ok(Parsed::Run(parsed))
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(Parsed::Print(text)) => {
            println!("{text}");
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Run(args)) => args,
        Err(message) => {
            eprintln!("hyperflux-next-bridge: {message}");
            return ExitCode::FAILURE;
        }
    };
    let stop = match hfx_bridge::platform::stop_on_signals() {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("hyperflux-next-bridge: can't set up signal handling: {e}");
            return ExitCode::FAILURE;
        }
    };
    let openrazer =
        (!args.no_openrazer && args.sim.is_none()).then(hfx_bridge::platform::OpenRazerEnv::system);
    // Beside the system socket only the keeper's own user may connect;
    // beside one of a developer's, the developer's keeper.
    let system = args.socket.is_none();
    let keeper_uid = if system {
        hfx_bridge::ipc::system_uid(hfx_proto::paths::KEEPER_USER)
    } else {
        Some(rustix::process::geteuid().as_raw())
    };
    let Some(keeper_uid) = keeper_uid else {
        eprintln!(
            "hyperflux-next-bridge: there is no {} user; run systemd-sysusers",
            hfx_proto::paths::KEEPER_USER
        );
        return ExitCode::FAILURE;
    };
    let config = Config {
        system,
        socket: args
            .socket
            .unwrap_or_else(|| PathBuf::from(hfx_proto::paths::SYSTEM_SOCKET)),
        source: args.sim.map_or(Source::System, Source::Sim),
        read_only: args.read_only,
        keeper_uid,
        openrazer,
        sim_openrazer: !args.no_openrazer,
        pretend_suspend: None,
        // systemd's StateDirectory=. Without it nothing is kept.
        state_dir: std::env::var_os("STATE_DIRECTORY").map(PathBuf::from),
    };
    match hfx_bridge::run(config, stop) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("hyperflux-next-bridge: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Parsed, String> {
        parse(list.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn command_line() {
        let Ok(Parsed::Run(a)) = args(&["--sim", "/tmp/s", "--read-only", "--socket", "/tmp/b"])
        else {
            panic!()
        };
        assert_eq!(a.sim, Some(PathBuf::from("/tmp/s")));
        assert_eq!(a.socket, Some(PathBuf::from("/tmp/b")));
        assert!(a.read_only && !a.no_openrazer);
        assert!(matches!(args(&["--version"]), Ok(Parsed::Print(v)) if v == "1.0.0"));
        assert!(matches!(args(&["--help"]), Ok(Parsed::Print(h)) if h.starts_with("Usage:")));
        assert!(args(&["--socket"]).is_err());
        assert!(args(&["--sim", "a", "--sim", "b"]).is_err());
        assert!(args(&["--config", "x"]).is_err());
        assert!(args(&["--transport", "kernel"]).is_err());
    }
}
