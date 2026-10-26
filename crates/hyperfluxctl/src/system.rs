// SPDX-License-Identifier: GPL-2.0-only

//! Everything Doctor learns about the machine goes through [`System`]:
//! files are read below a root directory and programs run through a
//! [`Runner`]. That lets tests use a fixture tree and canned command output
//! instead of the real system.

use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Probes should answer quickly. A hung `busctl` mustn't hang Doctor.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub success: bool,
    /// Trimmed. Kept when the program fails too: `mokutil --test-key`
    /// answers through its exit status and its text.
    pub stdout: String,
}

pub trait Runner {
    /// Runs `argv` without a shell, stdin and stderr closed. `None` when it
    /// can't start, times out or prints something that isn't text.
    fn output(&self, argv: &[&str], timeout: Duration) -> Option<Output>;

    /// Runs with the terminal attached, so `sudo` can ask for a password
    /// and the user sees what happens.
    fn visible(&self, argv: &[String]) -> bool;
}

pub struct RealRunner;

impl Runner for RealRunner {
    fn output(&self, argv: &[&str], timeout: Duration) -> Option<Output> {
        let (program, args) = argv.split_first()?;
        let mut child = Command::new(program)
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout.take(MAX_BYTES).read_to_end(&mut bytes);
            bytes
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        };
        let text = String::from_utf8(reader.join().ok()?).ok()?;
        let stdout = text.trim().to_owned();
        Some(Output {
            success: status.success(),
            stdout,
        })
    }

    fn visible(&self, argv: &[String]) -> bool {
        let Some((program, args)) = argv.split_first() else {
            return false;
        };
        Command::new(program)
            .args(args)
            .status()
            .is_ok_and(|s| s.success())
    }
}

pub struct System {
    root: PathBuf,
    runner: Box<dyn Runner>,
    /// Effective uid and all groups of this process.
    pub uid: u32,
    pub groups: Vec<u32>,
    /// How long a just-created OpenRazer device gets to be listed before
    /// Doctor calls it missing; none for a fixture.
    pub settle: Duration,
}

impl System {
    /// The machine this runs on.
    #[must_use]
    pub fn real() -> Self {
        use rustix::process::{Gid, getegid, geteuid, getgroups};
        let mut groups: Vec<u32> = getgroups()
            .unwrap_or_default()
            .into_iter()
            .map(Gid::as_raw)
            .collect();
        groups.push(getegid().as_raw());
        let mut system = Self::new("/", Box::new(RealRunner), geteuid().as_raw(), groups);
        system.settle = Duration::from_secs(3);
        system
    }

    #[must_use]
    pub fn new(
        root: impl Into<PathBuf>,
        runner: Box<dyn Runner>,
        uid: u32,
        groups: Vec<u32>,
    ) -> Self {
        let root = root.into();
        Self {
            root,
            runner,
            uid,
            groups,
            settle: Duration::ZERO,
        }
    }

    /// `/sys/module` -> `<root>/sys/module`.
    #[must_use]
    pub fn path(&self, absolute: &str) -> PathBuf {
        self.root.join(absolute.trim_start_matches('/'))
    }

    #[must_use]
    pub fn read_bytes(&self, absolute: &str) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        let file = fs::File::open(self.path(absolute)).ok()?;
        file.take(MAX_BYTES).read_to_end(&mut bytes).ok()?;
        Some(bytes)
    }

    /// A small text file, trimmed. `None` if missing or unreadable.
    #[must_use]
    pub fn read(&self, absolute: &str) -> Option<String> {
        let text = String::from_utf8(self.read_bytes(absolute)?).ok()?;
        Some(text.trim().to_owned())
    }

    #[must_use]
    pub fn is_dir(&self, absolute: &str) -> bool {
        self.path(absolute).is_dir()
    }

    #[must_use]
    pub fn is_file(&self, absolute: &str) -> bool {
        self.path(absolute).is_file()
    }

    /// Exists in any form, including a dangling symlink.
    #[must_use]
    pub fn exists(&self, absolute: &str) -> bool {
        fs::symlink_metadata(self.path(absolute)).is_ok()
    }

    /// A symlink whose target is gone.
    #[must_use]
    pub fn dangling(&self, absolute: &str) -> bool {
        let path = self.path(absolute);
        path.is_symlink() && !path.exists()
    }

    /// Up to `limit` entry names of a directory, sorted.
    #[must_use]
    pub fn list(&self, absolute: &str, limit: usize) -> Option<Vec<String>> {
        let entries = fs::read_dir(self.path(absolute)).ok()?;
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .take(limit)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        Some(names)
    }

    /// Last component of a symlink's target (`driver` links in sysfs).
    #[must_use]
    pub fn link_name(&self, absolute: &str) -> Option<String> {
        let target = fs::read_link(self.path(absolute)).ok()?;
        Some(target.file_name()?.to_string_lossy().into_owned())
    }

    /// A symlink's target as written (DKMS's `kernel-*` links).
    #[must_use]
    pub fn link_target(&self, absolute: &str) -> Option<String> {
        let target = fs::read_link(self.path(absolute)).ok()?;
        Some(target.to_string_lossy().into_owned())
    }

    /// Owner, group and permission bits.
    #[must_use]
    pub fn access(&self, absolute: &str) -> Option<(u32, u32, u32)> {
        let meta = fs::metadata(self.path(absolute)).ok()?;
        Some((meta.uid(), meta.gid(), meta.mode() & 0o7777))
    }

    /// Users a POSIX ACL gives all of `perms` (0o4 read, 0o2 write),
    /// besides the file's owner: what a udev `uaccess` tag leaves behind for
    /// the logged-in user.
    #[must_use]
    pub fn acl_users(&self, absolute: &str, perms: u16) -> Vec<u32> {
        let mut value = vec![0_u8; 1024];
        let Ok(len) = rustix::fs::getxattr(
            self.path(absolute),
            "system.posix_acl_access",
            &mut value[..],
        ) else {
            return Vec::new();
        };
        posix_acl_users(&value[..len], perms)
    }

    /// Stdout of a successful run of `line`, split on spaces (never through
    /// a shell: every argument Doctor passes is a plain word).
    #[must_use]
    pub fn run(&self, line: &str) -> Option<String> {
        self.run_any(line, PROBE_TIMEOUT)
            .filter(|out| out.success)
            .map(|out| out.stdout)
    }

    #[must_use]
    pub fn run_any(&self, line: &str, timeout: Duration) -> Option<Output> {
        let argv: Vec<&str> = line.split_whitespace().collect();
        self.runner.output(&argv, timeout)
    }

    #[must_use]
    pub fn run_visible(&self, argv: &[String]) -> bool {
        self.runner.visible(argv)
    }

    /// The gid of a group, from `getent`, which counts NSS sources too.
    #[must_use]
    pub fn group_id(&self, name: &str) -> Option<u32> {
        self.getent("group", name)
    }

    #[must_use]
    pub fn user_id(&self, name: &str) -> Option<u32> {
        self.getent("passwd", name)
    }

    fn getent(&self, database: &str, name: &str) -> Option<u32> {
        let line = self.run(&format!("getent {database} {name}"))?;
        let mut fields = line.split(':');
        (fields.next()? == name).then_some(())?;
        fields.nth(1)?.parse().ok()
    }
}

/// The kernel's ACL xattr: a little-endian version word (2), then entries
/// of tag (u16), permissions (u16) and id (u32). Named users are tag 2, and
/// a mask entry (tag 0x10) limits what they get.
#[must_use]
pub fn posix_acl_users(value: &[u8], perms: u16) -> Vec<u32> {
    const USER: u16 = 0x02;
    const MASK: u16 = 0x10;
    if value.get(..4) != Some(&2_u32.to_le_bytes()[..]) {
        return Vec::new();
    }
    let entries: Vec<(u16, u16, u32)> = value[4..]
        .chunks_exact(8)
        .map(|e| {
            let word = |at: usize| u16::from_le_bytes([e[at], e[at + 1]]);
            (
                word(0),
                word(2),
                u32::from_le_bytes([e[4], e[5], e[6], e[7]]),
            )
        })
        .collect();
    let mask = entries.iter().find(|e| e.0 == MASK).map_or(perms, |e| e.1);
    let users = entries
        .iter()
        .filter(|e| e.0 == USER && e.1 & mask & perms == perms);
    users.map(|e| e.2).collect()
}

/// A module identity as the kernel prints it: short, plain characters.
#[must_use]
pub fn identity(text: Option<String>) -> Option<String> {
    let plain = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-');
    text.filter(|t| !t.is_empty() && t.len() <= 64 && t.bytes().all(plain))
}

/// `foo bar` -> `'foo bar'`, for showing a command the way a shell takes it.
#[must_use]
pub fn shell_words(argv: &[String]) -> String {
    let plain = |b: u8| b.is_ascii_alphanumeric() || b"_-./@=:,+".contains(&b);
    let quote = |arg: &String| {
        if !arg.is_empty() && arg.bytes().all(plain) {
            arg.clone()
        } else {
            format!("'{}'", arg.replace('\'', r"'\''"))
        }
    };
    argv.iter().map(quote).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        let argv = ["sudo", "rm", "/dev/input/by-id/a b", "it's"].map(String::from);
        assert_eq!(
            shell_words(&argv),
            r"sudo rm '/dev/input/by-id/a b' 'it'\''s'"
        );
    }

    #[test]
    fn acl_mask() {
        let acl = |entries: &[(u16, u16, u32)]| {
            let mut bytes = 2_u32.to_le_bytes().to_vec();
            for (tag, perm, id) in entries {
                bytes.extend(
                    tag.to_le_bytes()
                        .iter()
                        .chain(&perm.to_le_bytes())
                        .chain(&id.to_le_bytes()),
                );
            }
            bytes
        };
        let user = (2, 6, 1000);
        assert_eq!(
            posix_acl_users(&acl(&[(1, 6, 0), user, (0x10, 6, 0)]), 0o6),
            [1000]
        );
        assert!(posix_acl_users(&acl(&[user, (0x10, 4, 0)]), 0o2).is_empty());
        assert!(posix_acl_users(&acl(&[(2, 4, 1000)]), 0o2).is_empty());
        assert!(posix_acl_users(&acl(&[(2, 2, 1000)]), 0o6).is_empty());
        assert!(posix_acl_users(&[1, 0, 0, 0], 0o2).is_empty());
    }

    #[test]
    fn real_runner() {
        let out = RealRunner.output(&["sh", "-c", "echo hi; exit 3"], PROBE_TIMEOUT);
        assert_eq!(
            out,
            Some(Output {
                success: false,
                stdout: "hi".into()
            })
        );
        assert!(
            RealRunner
                .output(&["sleep", "5"], Duration::from_millis(50))
                .is_none()
        );
        assert!(
            RealRunner
                .output(&["/nonexistent/x"], PROBE_TIMEOUT)
                .is_none()
        );
    }
}
