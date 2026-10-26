// SPDX-License-Identifier: GPL-2.0-only

//! hyperflux-next-sim: plays one or more HyperFlux V2 receivers for a bridge
//! started with `--sim`, driven by commands on stdin. A development tool,
//! not packaged.

use hfx_bridge::sim::{HELP, Sim};
use std::io::BufRead;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "Usage: hyperflux-next-sim [--socket PATH] [--receivers N]";

fn default_socket() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
        .join("hyperflux-next-sim.sock")
}

/// Reading stdin from a background job of a terminal would stop the whole
/// process (SIGTTIN), simulated receivers included.
fn in_background() -> bool {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
        return false;
    };
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .map_or(Vec::new(), |(_, rest)| rest.split_whitespace().collect());
    match (fields.get(2), fields.get(5)) {
        (Some(pgrp), Some(tpgid)) => *tpgid != "-1" && *tpgid != "0" && pgrp != tpgid,
        _ => false,
    }
}

fn main() -> ExitCode {
    let mut socket = default_socket();
    let mut receivers = 1_usize;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = match arg.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}\n\n{HELP}");
                return ExitCode::SUCCESS;
            }
            "--version" => {
                println!("{}", hfx_proto::PRODUCT_VERSION);
                return ExitCode::SUCCESS;
            }
            "--socket" | "--receivers" => args.next(),
            other => {
                eprintln!("hyperflux-next-sim: unknown argument {other}\n{USAGE}");
                return ExitCode::FAILURE;
            }
        };
        let Some(value) = value else {
            eprintln!("hyperflux-next-sim: {arg} needs a value");
            return ExitCode::FAILURE;
        };
        if arg == "--socket" {
            socket = PathBuf::from(value);
        } else {
            match value.parse() {
                Ok(n) if (1..=16).contains(&n) => receivers = n,
                _ => {
                    eprintln!("hyperflux-next-sim: --receivers takes 1 to 16");
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    if std::fs::symlink_metadata(&socket).is_ok_and(|m| {
        use std::os::unix::fs::FileTypeExt;
        m.file_type().is_socket()
    }) {
        let _ = std::fs::remove_file(&socket);
    }
    let sim = Sim::new(receivers);
    if let Err(e) = sim.serve(&socket) {
        eprintln!("hyperflux-next-sim: {}: {e}", socket.display());
        return ExitCode::FAILURE;
    }
    eprintln!(
        "simulating {receivers} receiver(s) on {}; start the bridge with --sim {0}",
        socket.display()
    );
    if in_background() {
        eprintln!("running in the background: commands on stdin are off");
    } else {
        eprintln!("type help for commands");
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            match sim.command(&line) {
                Ok(true) => {}
                Ok(false) => {
                    let _ = std::fs::remove_file(&socket);
                    return ExitCode::SUCCESS;
                }
                Err(message) => eprintln!("{message}"),
            }
        }
    }
    // No more commands (stdin closed or unusable): keep serving until killed.
    loop {
        std::thread::park();
    }
}
