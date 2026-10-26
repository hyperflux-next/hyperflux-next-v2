// SPDX-License-Identifier: GPL-2.0-only

//! hyperfluxctl: status, devices, battery, watch, doctor, bench, capture,
//! stall-check, probe receiver, report and support-bundle for HyperFlux
//! Next V2. The binaries are thin. All the work is here so the tests can
//! run all the commands against a fake bridge and a fixture tree.
//!
//! Exit codes: 0 fine, 1 usage error or failure, 2 Doctor found something
//! or stall-check found stalls, 3 stall-check can't read the pointer.

// The one exception is the evdev clock ioctl in input.rs.
#![deny(unsafe_code)]

pub mod args;
pub mod bench;
pub mod bridge;
pub mod bundle;
pub mod capture;
pub mod commands;
pub mod doctor;
pub mod fix;
pub mod input;
pub mod lines;
pub mod probe;
pub mod report;
pub mod stall;
pub mod system;

use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, IsTerminal, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};
use system::System;

pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILURE: i32 = 1;
pub const EXIT_ATTENTION: i32 = 2;
pub const EXIT_NO_INPUT: i32 = 3;

/// Asks the user before Doctor changes anything.
pub trait Prompt {
    /// Both ends are a terminal: there's someone to ask.
    fn interactive(&self) -> bool;
    fn confirm(&mut self, question: &str) -> bool;
}

pub struct TerminalPrompt;

impl Prompt for TerminalPrompt {
    fn interactive(&self) -> bool {
        io::stdin().is_terminal() && io::stdout().is_terminal()
    }

    fn confirm(&mut self, question: &str) -> bool {
        let mut stdout = io::stdout();
        if write!(stdout, "{question}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            return false;
        }
        let mut answer = String::new();
        io::stdin().lock().read_line(&mut answer).is_ok()
            && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }
}

pub struct Context<'a> {
    pub out: &'a mut dyn Write,
    pub system: &'a System,
    pub prompt: &'a mut dyn Prompt,
}

#[derive(Debug)]
pub struct Failure {
    pub message: String,
    /// Print the hint about --help.
    pub usage: bool,
    /// Stdout went away (`| head`); nothing worth saying.
    pub quiet: bool,
    pub exit: i32,
}

impl Failure {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            usage: false,
            quiet: false,
            exit: EXIT_FAILURE,
        }
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self {
            quiet: error.kind() == io::ErrorKind::BrokenPipe,
            ..Self::new(format!("output failed: {error}"))
        }
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

/// One pretty JSON document and a newline.
///
/// # Errors
/// When serializing or writing fails.
pub fn print_json<T: Serialize>(out: &mut dyn Write, value: &T) -> Result<(), Failure> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| Failure::new(format!("JSON output failed: {e}")))?;
    writeln!(out, "{text}")?;
    Ok(())
}

/// Creates a file only this user can read, and never replaces one.
///
/// # Errors
/// When the file exists or cannot be created.
pub fn create_private(path: &Path) -> Result<File, Failure> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| match e.kind() {
            io::ErrorKind::AlreadyExists => Failure::new(format!(
                "{} already exists; nothing was overwritten",
                path.display()
            )),
            _ => Failure::new(format!("can't create {}: {e}", path.display())),
        })
}

/// `(20261021T101500Z, 2026-10-21T10:15:00Z)` for a point in time, in UTC.
#[must_use]
pub fn utc(time: SystemTime) -> (String, String) {
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rest = secs % 86_400;
    let (h, m, s) = (rest / 3600, rest / 60 % 60, rest % 60);
    // Days since 1970-01-01 to a civil date (proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        format!("{year:04}{month:02}{day:02}T{h:02}{m:02}{s:02}Z"),
        format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z"),
    )
}

/// Set by the first Ctrl-C or SIGTERM, for commands that stop early and
/// still finish their output.
fn stop_flag() -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        // A second Ctrl-C ends the process the usual way.
        let _ = signal_hook::flag::register_conditional_shutdown(signal, 130, Arc::clone(&stop));
        let _ = signal_hook::flag::register(signal, Arc::clone(&stop));
    }
    stop
}

/// Parses and runs one command line (without the program name).
///
/// # Errors
/// Usage errors and failures, for the caller to print; exit code 1.
pub fn run(args: &[String], ctx: &mut Context<'_>) -> Result<i32, Failure> {
    let invocation = args::parse(args).map_err(|e| Failure {
        usage: true,
        ..Failure::new(e.0)
    })?;
    let socket = invocation.socket.as_deref();
    let json = invocation.json;
    match invocation.command {
        args::Command::Help => {
            ctx.out.write_all(args::USAGE.as_bytes())?;
            Ok(EXIT_OK)
        }
        args::Command::Version => {
            writeln!(
                ctx.out,
                "hyperfluxctl (HyperFlux Next V2) {}",
                hfx_proto::PRODUCT_VERSION
            )?;
            Ok(EXIT_OK)
        }
        args::Command::Status => commands::status(ctx, socket, json),
        args::Command::Devices => commands::devices(ctx, socket, json),
        args::Command::Battery { waybar } => commands::battery(ctx, socket, json, waybar),
        args::Command::Watch => commands::watch(ctx, socket, json, None),
        args::Command::Doctor { explain, fix, yes } => {
            commands::doctor(ctx, socket, json, (explain, fix, yes))
        }
        args::Command::Bench(options) => bench::run(ctx, socket, json, &options),
        args::Command::Capture { seconds, output } => {
            capture::run(ctx, socket, json, seconds, output.as_deref(), &stop_flag())
        }
        args::Command::StallCheck(options) => stall::run(ctx, socket, json, &options, &stop_flag()),
        args::Command::ProbeReceiver => probe::run(ctx, socket, json),
        args::Command::Report { seconds, output } => {
            report::run(ctx, socket, json, seconds, output.as_deref(), &stop_flag())
        }
        args::Command::SupportBundle { preview, output } => {
            bundle::run(ctx, socket, json, preview, output.as_deref())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn utc_dates() {
        assert_eq!(utc(UNIX_EPOCH).1, "1970-01-01T00:00:00Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400 + 3_661);
        assert_eq!(
            utc(leap),
            ("20000229T010101Z".into(), "2000-02-29T01:01:01Z".into())
        );
        let later = UNIX_EPOCH + Duration::from_secs(1_792_576_800);
        assert_eq!(utc(later).1, "2026-10-21T10:00:00Z");
    }
}
