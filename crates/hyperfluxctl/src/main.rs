// SPDX-License-Identifier: GPL-2.0-only

//! hyperfluxctl. See `hyperfluxctl --help` and hyperfluxctl(1).

use hyperfluxctl::system::System;
use hyperfluxctl::{Context, TerminalPrompt};
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let system = System::real();
    let mut stdout = std::io::stdout();
    let mut prompt = TerminalPrompt;
    let mut ctx = Context {
        out: &mut stdout,
        system: &system,
        prompt: &mut prompt,
    };
    let code = match hyperfluxctl::run(&args, &mut ctx) {
        Ok(code) => code,
        Err(failure) => {
            let _ = ctx.out.flush();
            if !failure.quiet {
                eprintln!("hyperfluxctl: {}", failure.message);
                if failure.usage {
                    eprintln!("Run hyperfluxctl --help to see the commands.");
                }
            }
            failure.exit
        }
    };
    let _ = stdout.flush();
    std::process::exit(code);
}
