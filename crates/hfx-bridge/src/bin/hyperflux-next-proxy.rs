// SPDX-License-Identifier: GPL-2.0-only

//! hyperflux-next-proxy: see `proxy.rs` and ARCHITECTURE.md.

use rustix::net::AddressFamily;
use rustix::net::sockopt::{socket_acceptconn, socket_domain};
use std::os::fd::AsFd;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "Usage: hyperflux-next-proxy [--socket PATH] [--listen PATH]";

const HELP: &str = "\
Passes one user's connections on to the HyperFlux Next V2 bridge. systemd
starts it from hyperflux-next-client@UID.socket, with the listening socket
as standard input; it takes no configuration file.

  --socket PATH    reach the bridge here instead of /run/hyperflux-next/bridge.sock
  --listen PATH    listen here itself, instead of on standard input
  --help           show this text
  --version        show the version";

fn main() -> ExitCode {
    let (mut socket, mut listen) = (None, None);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let slot = match arg.as_str() {
            "--socket" => &mut socket,
            "--listen" => &mut listen,
            "--help" | "-h" => {
                println!("{USAGE}\n\n{HELP}");
                return ExitCode::SUCCESS;
            }
            "--version" => {
                println!("{}", hfx_proto::PRODUCT_VERSION);
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("hyperflux-next-proxy: unknown argument {other}\n{USAGE}");
                return ExitCode::FAILURE;
            }
        };
        match args.next() {
            Some(path) if slot.is_none() => *slot = Some(PathBuf::from(path)),
            _ => {
                eprintln!("hyperflux-next-proxy: {arg} needs one path\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }
    let listener = match listen {
        Some(path) => UnixListener::bind(&path)
            .map_err(|e| format!("can't listen on {}: {e}", path.display())),
        None => from_systemd(),
    };
    let listener = match listener {
        Ok(listener) => listener,
        Err(message) => {
            eprintln!("hyperflux-next-proxy: {message}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let bridge = socket.unwrap_or_else(|| PathBuf::from(hfx_proto::paths::SYSTEM_SOCKET));
    let error = hfx_bridge::proxy::serve(&listener, &bridge, hfx_bridge::proxy::MAX_CONNECTIONS);
    eprintln!("hyperflux-next-proxy: can't take connections any more: {error}");
    ExitCode::FAILURE
}

/// StandardInput=socket in the unit. Anything else on standard input means
/// it wasn't systemd that started it.
fn from_systemd() -> Result<UnixListener, String> {
    let stdin = std::io::stdin();
    let listening = socket_domain(stdin.as_fd()) == Ok(AddressFamily::UNIX)
        && socket_acceptconn(stdin.as_fd()) == Ok(true);
    if !listening {
        return Err("standard input isn't a listening Unix socket; systemd starts this".into());
    }
    let fd = stdin
        .as_fd()
        .try_clone_to_owned()
        .map_err(|e| format!("can't take the listening socket: {e}"))?;
    Ok(UnixListener::from(fd))
}
