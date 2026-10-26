// SPDX-License-Identifier: GPL-2.0-only

//! Command line parsing. Everything is checked before anything is probed or
//! connected. A typo never costs a bridge round trip.

use hfx_proto::messages::BenchQuery;
use std::path::PathBuf;

pub const USAGE: &str = "\
Usage:
  hyperfluxctl status
  hyperfluxctl devices
  hyperfluxctl battery [--waybar]
  hyperfluxctl watch
  hyperfluxctl doctor [--explain] [--fix [--yes]]
  hyperfluxctl bench [--device ID] [--delay-us N]... [--retry-us N]... [--iterations N]
                     [--query firmware|battery] [--reread-busy [--quick]]
  hyperfluxctl bench --defaults [--device ID] [--iterations N] [--query firmware|battery]
  hyperfluxctl capture [--seconds N] [--output PATH]
  hyperfluxctl stall-check [--seconds N] [--threshold-ms N] [--ab]
  hyperfluxctl report [--seconds N] [--output PATH]
  hyperfluxctl support-bundle [--preview] [--output PATH]
  hyperfluxctl probe receiver

Every command takes --json and --socket PATH.
";

pub const DEFAULT_DELAYS_US: [u16; 5] = [8000, 6000, 5000, 4000, 3000];
/// The grid `--reread-busy` measures unless told otherwise, and the cells
/// `--quick` keeps of it.
const REREAD_DELAYS_US: [u16; 5] = [1000, 2000, 3000, 4000, 6000];
const REREAD_RETRIES_US: [u16; 4] = [1000, 2000, 5000, 10_000];
const QUICK: ([u16; 3], [u16; 2]) = ([2000, 3000, 4000], [1000, 2000]);
const MAX_DELAYS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bench {
    pub device: Option<String>,
    pub delays_us: Vec<u16>,
    /// Empty: the bridge's usual 1 ms, with no override sent.
    pub retries_us: Vec<u16>,
    pub iterations: u16,
    pub query: BenchQuery,
    /// Read again after a busy answer, within the same deadline.
    pub reread_busy: bool,
    /// Measure each device at its own timings, as the bridge uses them.
    pub defaults: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StallCheck {
    pub seconds: u32,
    pub threshold_ms: u32,
    pub ab: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Status,
    Devices,
    Battery {
        waybar: bool,
    },
    Watch,
    Doctor {
        explain: bool,
        fix: bool,
        yes: bool,
    },
    Bench(Bench),
    Capture {
        seconds: u32,
        output: Option<PathBuf>,
    },
    StallCheck(StallCheck),
    ProbeReceiver,
    Report {
        seconds: u32,
        output: Option<PathBuf>,
    },
    SupportBundle {
        preview: bool,
        output: Option<PathBuf>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub command: Command,
    pub json: bool,
    pub socket: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

fn error<T>(message: impl Into<String>) -> Result<T, UsageError> {
    Err(UsageError(message.into()))
}

/// The options one command was given.
#[derive(Default)]
struct Options {
    flags: Vec<String>,
    values: Vec<(String, String)>,
}

impl Options {
    /// `flags` take no value and `valued` take one. Only `--delay-us` and
    /// `--retry-us` repeat.
    fn parse(words: &[String], flags: &[&str], valued: &[&str]) -> Result<Self, UsageError> {
        let mut options = Self::default();
        let mut words = words.iter();
        while let Some(word) = words.next() {
            if flags.contains(&word.as_str()) {
                options.flags.push(word.clone());
            } else if valued.contains(&word.as_str()) {
                let value = match words.next() {
                    Some(value) if !value.starts_with("--") => value.clone(),
                    _ => return error(format!("{word} needs a value")),
                };
                let repeats = word == "--delay-us" || word == "--retry-us";
                if !repeats && options.value(word).is_some() {
                    return error(format!("{word} given twice"));
                }
                options.values.push((word.clone(), value));
            } else {
                return error(format!("unexpected {word:?}"));
            }
        }
        Ok(options)
    }

    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|f| f == name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn number<T: std::str::FromStr + PartialOrd + std::fmt::Display>(
        &self,
        name: &str,
        range: (T, T),
        default: T,
    ) -> Result<T, UsageError> {
        self.value(name)
            .map_or(Ok(default), |text| parse_number(name, text, range))
    }
}

fn parse_number<T>(name: &str, text: &str, (min, max): (T, T)) -> Result<T, UsageError>
where
    T: std::str::FromStr + PartialOrd + std::fmt::Display,
{
    match text.parse::<T>() {
        Ok(value) if value >= min && value <= max => Ok(value),
        _ => error(format!("{name} takes a number from {min} to {max}")),
    }
}

/// Pulls `--json` and `--socket` out wherever they are and leaves the
/// command's own words.
fn globals(args: &[String]) -> Result<(bool, Option<PathBuf>, Vec<String>), UsageError> {
    let (mut json, mut socket, mut rest) = (false, None, Vec::new());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let path = match arg.as_str() {
            "--json" => {
                json = true;
                continue;
            }
            "--socket" => iter.next().filter(|p| !p.starts_with("--")).cloned(),
            _ => match arg.strip_prefix("--socket=") {
                Some(path) => Some(path.to_owned()).filter(|p| !p.is_empty()),
                None => {
                    rest.push(arg.clone());
                    continue;
                }
            },
        };
        let Some(path) = path else {
            return error("--socket needs a value");
        };
        if socket.replace(PathBuf::from(path)).is_some() {
            return error("--socket given twice");
        }
    }
    Ok((json, socket, rest))
}

/// # Errors
/// Returns a one-line reason for anything it doesn't understand.
pub fn parse(args: &[String]) -> Result<Invocation, UsageError> {
    let (json, socket, words) = globals(args)?;
    let Some((name, rest)) = words.split_first() else {
        return error("no command given");
    };
    let help = rest.iter().any(|w| w == "--help" || w == "-h");
    let bare = |command| Options::parse(rest, &[], &[]).map(|_| command);
    let command = match name.as_str() {
        "help" | "--help" | "-h" => Command::Help,
        _ if help => Command::Help,
        "--version" | "version" => Command::Version,
        "status" => bare(Command::Status)?,
        "devices" => bare(Command::Devices)?,
        "watch" => bare(Command::Watch)?,
        "battery" => {
            let options = Options::parse(rest, &["--waybar"], &[])?;
            Command::Battery {
                waybar: options.flag("--waybar"),
            }
        }
        "doctor" => {
            let options = Options::parse(rest, &["--explain", "--fix", "--yes"], &[])?;
            let (explain, fix, yes) = (
                options.flag("--explain"),
                options.flag("--fix"),
                options.flag("--yes"),
            );
            if yes && !fix {
                return error("--yes only goes with --fix");
            }
            if fix && json {
                return error("--fix asks before each change, so it can't be combined with --json");
            }
            Command::Doctor { explain, fix, yes }
        }
        "bench" => Command::Bench(bench(rest)?),
        "capture" => {
            let options = Options::parse(rest, &[], &["--seconds", "--output"])?;
            let seconds = options.number("--seconds", (1, 3600), 60)?;
            Command::Capture {
                seconds,
                output: options.value("--output").map(PathBuf::from),
            }
        }
        "stall-check" => {
            let options = Options::parse(rest, &["--ab"], &["--seconds", "--threshold-ms"])?;
            Command::StallCheck(StallCheck {
                seconds: options.number("--seconds", (10, 600), 60)?,
                threshold_ms: options.number("--threshold-ms", (10, 500), 50)?,
                ab: options.flag("--ab"),
            })
        }
        "probe" => match rest.split_first() {
            Some((what, more)) if what == "receiver" => {
                Options::parse(more, &[], &[]).map(|_| Command::ProbeReceiver)?
            }
            _ => return error("probe takes receiver"),
        },
        "report" => {
            let options = Options::parse(rest, &[], &["--seconds", "--output"])?;
            Command::Report {
                seconds: options.number("--seconds", (1, 1800), 180)?,
                output: options.value("--output").map(PathBuf::from),
            }
        }
        "support-bundle" => {
            let options = Options::parse(rest, &["--preview"], &["--output"])?;
            let (preview, output) = (
                options.flag("--preview"),
                options.value("--output").map(PathBuf::from),
            );
            if preview && output.is_some() {
                return error("--preview writes nothing, so it takes no --output");
            }
            Command::SupportBundle { preview, output }
        }
        other => return error(format!("unknown command {other:?}")),
    };
    Ok(Invocation {
        command,
        json,
        socket,
    })
}

fn bench(words: &[String]) -> Result<Bench, UsageError> {
    let options = Options::parse(
        words,
        &["--reread-busy", "--quick", "--defaults"],
        &[
            "--device",
            "--delay-us",
            "--retry-us",
            "--iterations",
            "--query",
        ],
    )?;
    let all = |name: &str, max: u16| {
        let given = options.values.iter().filter(|(k, _)| k == name);
        let values = given.map(|(k, v)| parse_number(k, v, (1000_u16, max)));
        let values = values.collect::<Result<Vec<_>, _>>()?;
        if values.len() > MAX_DELAYS {
            return error(format!("{name} takes at most {MAX_DELAYS} values"));
        }
        Ok(values)
    };
    let mut delays_us = all("--delay-us", 8000)?;
    let mut retries_us = all("--retry-us", 10_000)?;
    let (reread_busy, defaults) = (options.flag("--reread-busy"), options.flag("--defaults"));
    let quick = options.flag("--quick");
    if defaults && (reread_busy || quick || !delays_us.is_empty() || !retries_us.is_empty()) {
        return error("--defaults takes no --delay-us, --retry-us, --reread-busy or --quick");
    }
    if quick {
        if !reread_busy {
            return error("--quick only goes with --reread-busy");
        }
        if !delays_us.is_empty() || !retries_us.is_empty() {
            return error("--quick picks its own waits, so it takes no --delay-us or --retry-us");
        }
        (delays_us, retries_us) = (QUICK.0.to_vec(), QUICK.1.to_vec());
    }
    if reread_busy && delays_us.is_empty() {
        delays_us = REREAD_DELAYS_US.to_vec();
    }
    if reread_busy && retries_us.is_empty() {
        retries_us = REREAD_RETRIES_US.to_vec();
    }
    if delays_us.is_empty() && !defaults {
        delays_us = DEFAULT_DELAYS_US.to_vec();
    }
    let query = match options.value("--query") {
        None | Some("firmware") => BenchQuery::Firmware,
        Some("battery") => BenchQuery::Battery,
        Some(_) => return error("--query is firmware or battery"),
    };
    Ok(Bench {
        device: options.value("--device").map(String::from),
        delays_us,
        retries_us,
        iterations: options.number("--iterations", (1, 1000), 20)?,
        query,
        reread_busy,
        defaults,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(line: &str) -> Result<Invocation, UsageError> {
        parse(
            &line
                .split_whitespace()
                .map(String::from)
                .collect::<Vec<_>>(),
        )
    }

    fn command(line: &str) -> Command {
        parse_str(line).unwrap().command
    }

    fn refused(line: &str) -> String {
        parse_str(line).unwrap_err().0
    }

    #[test]
    fn global_options_go_anywhere() {
        let a = parse_str("--json status --socket /tmp/s").unwrap();
        assert_eq!(a, parse_str("status --socket=/tmp/s --json").unwrap());
        assert!(a.json);
        assert_eq!(a.socket, Some(PathBuf::from("/tmp/s")));
        assert_eq!(refused("status --socket"), "--socket needs a value");
        assert_eq!(
            refused("--socket a --socket b status"),
            "--socket given twice"
        );
    }

    #[test]
    fn simple_commands() {
        assert_eq!(command("status"), Command::Status);
        assert_eq!(
            command("battery --waybar"),
            Command::Battery { waybar: true }
        );
        assert_eq!(command("doctor --help"), Command::Help);
        assert_eq!(command("--version"), Command::Version);
        assert_eq!(refused("status --explain"), "unexpected \"--explain\"");
        assert_eq!(command("probe receiver"), Command::ProbeReceiver);
        assert_eq!(refused("probe mouse"), "probe takes receiver");
        assert_eq!(
            command("report --seconds 60"),
            Command::Report {
                seconds: 60,
                output: None
            }
        );
        assert_eq!(
            refused("report --seconds 0"),
            "--seconds takes a number from 1 to 1800"
        );
        assert_eq!(refused("frobnicate"), "unknown command \"frobnicate\"");
        assert_eq!(refused(""), "no command given");
    }

    #[test]
    fn doctor_flags() {
        let all = Command::Doctor {
            explain: true,
            fix: true,
            yes: true,
        };
        assert_eq!(command("doctor --fix --explain --yes"), all);
        assert_eq!(refused("doctor --yes"), "--yes only goes with --fix");
        assert!(refused("doctor --fix --json").contains("can't be combined"));
    }

    #[test]
    fn bench_options() {
        let bench = |line| match command(line) {
            Command::Bench(bench) => bench,
            other => panic!("{other:?}"),
        };
        let plain = bench("bench");
        assert_eq!(
            (plain.delays_us, plain.iterations),
            (DEFAULT_DELAYS_US.to_vec(), 20)
        );
        let line = "bench --device pointer-00cd --delay-us 7000 --delay-us 2000 --query battery \
                    --iterations 5 --retry-us 1000 --retry-us 10000";
        let given = bench(line);
        assert_eq!(given.device.as_deref(), Some("pointer-00cd"));
        assert_eq!(given.retries_us, [1000, 10_000]);
        assert_eq!(
            (given.delays_us, given.iterations, given.query),
            (vec![7000, 2000], 5, BenchQuery::Battery)
        );
        assert!(!given.reread_busy);
        // The command line, then why it's refused.
        for case in [
            "bench --retry-us 500: --retry-us takes a number from 1000 to 10000",
            "bench --delay-us 900: --delay-us takes a number from 1000 to 8000",
            "bench --iterations 0: --iterations takes a number from 1 to 1000",
            "bench --query lighting: --query is firmware or battery",
            "bench --device: --device needs a value",
            "bench --quick: --quick only goes with --reread-busy",
        ] {
            let (line, why) = case.split_once(": ").unwrap();
            assert_eq!(refused(line), why);
        }
        let grid = bench("bench --reread-busy");
        let axes = (grid.delays_us, grid.retries_us, grid.reread_busy);
        assert_eq!(
            axes,
            (REREAD_DELAYS_US.into(), REREAD_RETRIES_US.into(), true)
        );
        let quick = bench("bench --quick --reread-busy");
        assert_eq!(
            (quick.delays_us, quick.retries_us),
            (QUICK.0.into(), QUICK.1.into())
        );
        assert!(refused("bench --reread-busy --quick --delay-us 1000").contains("no --delay-us"));
        let own = bench("bench --defaults --iterations 200");
        assert_eq!(
            (own.defaults, own.delays_us, own.iterations),
            (true, vec![], 200)
        );
        assert!(refused("bench --defaults --retry-us 1000").contains("--defaults takes no"));
    }

    #[test]
    fn stall_check_options() {
        let check = |seconds, threshold_ms, ab| {
            Command::StallCheck(StallCheck {
                seconds,
                threshold_ms,
                ab,
            })
        };
        assert_eq!(command("stall-check"), check(60, 50, false));
        assert_eq!(
            command("stall-check --ab --seconds 90 --threshold-ms 30"),
            check(90, 30, true)
        );
        assert_eq!(
            refused("stall-check --threshold-ms 1000"),
            "--threshold-ms takes a number from 10 to 500"
        );
    }

    #[test]
    fn file_writing_commands() {
        let capture = Command::Capture {
            seconds: 5,
            output: Some("a.jsonl".into()),
        };
        assert_eq!(command("capture --seconds 5 --output a.jsonl"), capture);
        assert_eq!(
            command("capture"),
            Command::Capture {
                seconds: 60,
                output: None
            }
        );
        assert_eq!(
            refused("capture --output a --output b"),
            "--output given twice"
        );
        assert_eq!(refused("capture --output --json"), "--output needs a value");
        let preview = Command::SupportBundle {
            preview: true,
            output: None,
        };
        assert_eq!(command("support-bundle --preview"), preview);
        assert!(refused("support-bundle --preview --output x").contains("writes nothing"));
    }
}
