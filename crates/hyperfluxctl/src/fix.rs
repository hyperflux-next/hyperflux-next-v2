// SPDX-License-Identifier: GPL-2.0-only

//! `doctor --fix`: the findings that have one safe command, shown exactly
//! as they would run. Nothing runs without a yes on a terminal, and
//! anything that needs root goes through a visible `sudo` and is always
//! asked about, even with `--yes`.

use crate::doctor::{Finding, OLD_DKMS_PACKAGE, OPENRAZER_GROUP};
use crate::system::{System, shell_words};
use crate::{Context, Failure};
use hfx_proto::errors::ErrorCode;
use hfx_proto::paths;
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub code: ErrorCode,
    /// Ready to run, `sudo` included when needed.
    pub argv: Vec<String>,
    pub root: bool,
    /// What still has to happen afterwards.
    pub then: Option<&'static str>,
}

impl Fix {
    #[must_use]
    pub fn command(&self) -> String {
        shell_words(&self.argv)
    }

    #[must_use]
    pub fn json(&self) -> Value {
        json!({"code": self.code.id(), "command": self.command(), "root": self.root})
    }

    /// As [`Fix::json`], for a file someone else reads: `$(id -u)` and
    /// `"$USER"` for this login's uid and name, as in the catalog's advice.
    #[must_use]
    pub fn shared_json(&self, uid: u32, user: Option<&str>) -> Value {
        let unit = format!("hyperflux-next-client@{uid}.socket");
        let gpasswd = self.code == ErrorCode::OpenRazerNotReady;
        let word = |w: &String| match w {
            w if *w == unit => "\"hyperflux-next-client@$(id -u).socket\"".to_owned(),
            w if gpasswd && user == Some(w.as_str()) => "\"$USER\"".to_owned(),
            w => shell_words(std::slice::from_ref(w)),
        };
        let command: Vec<String> = self.argv.iter().map(word).collect();
        json!({"code": self.code.id(), "command": command.join(" "), "root": self.root})
    }
}

/// One fix per finding that has a safe one, in finding order.
#[must_use]
pub fn fixes(system: &System, findings: &[Finding]) -> Vec<Fix> {
    let make = |code, root: bool, line: &str, then| {
        let sudo = (root && system.uid != 0).then(|| "sudo".to_owned());
        let argv = sudo
            .into_iter()
            .chain(line.split_whitespace().map(String::from))
            .collect();
        Fix {
            code,
            argv,
            root,
            then,
        }
    };
    let mut list: Vec<Fix> = Vec::new();
    for finding in findings {
        let code = finding.code;
        let fix = match code {
            ErrorCode::ServiceDown => Some(make(
                code,
                true,
                &format!("systemctl enable --now {}", paths::BRIDGE_UNIT),
                None,
            )),
            ErrorCode::KeeperDown => Some(make(
                code,
                true,
                &format!("systemctl restart {}", paths::KEEPER_UNIT),
                None,
            )),
            ErrorCode::NoSocketAccess => {
                let unit = format!("hyperflux-next-client@{}.socket", system.uid);
                Some(make(
                    code,
                    true,
                    &format!("systemctl enable --now {unit}"),
                    None,
                ))
            }
            ErrorCode::RetiredDriverInstalled => {
                let remove = match finding.detail("package") {
                    Some("pacman") => Some(format!("pacman -R {OLD_DKMS_PACKAGE}")),
                    Some("apt") => Some(format!("apt remove {OLD_DKMS_PACKAGE}")),
                    Some("rpm") => Some(format!("rpm -e {OLD_DKMS_PACKAGE}")),
                    _ => finding
                        .detail("dkms")
                        .map(|r| format!("dkms remove {r} --all")),
                };
                remove.map(|line| make(code, true, &line, Some("Reboot afterwards.")))
            }
            ErrorCode::InertDriverRecords => finding
                .detail("paths")
                .map(|paths| make(code, true, &format!("rm -r {paths}"), None)),
            ErrorCode::OpenRazerGroupMissing => {
                Some(make(code, true, "systemd-sysusers openrazer.conf", None))
            }
            ErrorCode::OpenRazerNotReady => match finding.detail("access") {
                Some("not a member") => system.run("id -un").map(|user| {
                    let line = format!("gpasswd -a {user} {OPENRAZER_GROUP}");
                    make(code, true, &line, Some("Log out and back in afterwards."))
                }),
                Some("daemon not running") => Some(make(
                    code,
                    false,
                    "systemctl --user enable --now openrazer-daemon.service",
                    None,
                )),
                _ => None,
            },
            _ => None,
        };
        if let Some(fix) = fix.filter(|f| !list.iter().any(|known| known.argv == f.argv)) {
            list.push(fix);
        }
    }
    list
}

/// Shows the fixes and runs the ones the user agrees to. Returns false if
/// one that ran failed.
///
/// # Errors
/// When writing the output fails.
pub fn run(ctx: &mut Context<'_>, fixes: &[Fix], yes: bool) -> Result<bool, Failure> {
    writeln!(ctx.out)?;
    if fixes.is_empty() {
        writeln!(
            ctx.out,
            "Nothing here can be fixed automatically; follow the advice above."
        )?;
        return Ok(true);
    }
    writeln!(ctx.out, "Fixes:")?;
    for fix in fixes {
        writeln!(ctx.out, "  [{}] {}", fix.code.id(), fix.command())?;
    }
    if !ctx.prompt.interactive() {
        let text =
            "Not running on a terminal, so nothing was changed. Run the commands above yourself.";
        writeln!(ctx.out, "{text}")?;
        return Ok(true);
    }
    let mut all_ok = true;
    for fix in fixes {
        ctx.out.flush()?;
        let how = if fix.root { " as root" } else { "" };
        let question = format!("Run `{}`{how}? [y/N] ", fix.command());
        if (fix.root || !yes) && !ctx.prompt.confirm(&question) {
            writeln!(ctx.out, "Skipped.")?;
            continue;
        }
        writeln!(ctx.out, "Running: {}", fix.command())?;
        ctx.out.flush()?;
        if ctx.system.run_visible(&fix.argv) {
            writeln!(
                ctx.out,
                "Done.{}",
                fix.then.map(|t| format!(" {t}")).unwrap_or_default()
            )?;
        } else {
            all_ok = false;
            writeln!(
                ctx.out,
                "That didn't work; the command's own message above says why."
            )?;
        }
    }
    writeln!(ctx.out, "Run hyperfluxctl doctor again to see what's left.")?;
    Ok(all_ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_socket_unit() {
        let line = "sudo systemctl start hyperflux-next-client@1000.socket";
        let fix = Fix {
            code: ErrorCode::NoSocketAccess,
            argv: line.split(' ').map(String::from).collect(),
            root: true,
            then: None,
        };
        let shared = r#"sudo systemctl start "hyperflux-next-client@$(id -u).socket""#;
        assert_eq!(fix.shared_json(1000, None)["command"], shared);
    }
}
