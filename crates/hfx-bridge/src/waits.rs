// SPDX-License-Identifier: GPL-2.0-only

//! Learned first-GET waits (`wire::Learned`), kept across restarts in
//! `learned-waits.json` in the bridge's state directory, which the unit's
//! `StateDirectory=` makes `/var/lib/hyperflux-next`. On hardware the waits
//! rose by 1.5 to 2 ms over two hours of use, and every restart began again
//! from the catalog's, with re-reads until they had risen again.
//!
//! A wait is kept per receiver and lane, for the product that learned it
//! and for its firmware once the bridge has seen an answer to 00:81, and
//! goes when another product or firmware turns up, or after 30 days
//! unused. The file is written at most once a minute, after a change. One
//! that is missing or unreadable means the catalog's waits.

use hfx_proto::messages::Lane;
use serde::{Deserialize, Serialize};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

pub const FILE: &str = "learned-waits.json";
pub const WRITE_EVERY_MS: u64 = 60_000;
pub const KEEP_UNUSED_S: u64 = 30 * 86_400;
/// Using a wait moves its date at most once a day, so that alone doesn't
/// mean a write each time a device pairs.
const USED_STEP_S: u64 = 86_400;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    receiver_id: String,
    lane: Lane,
    product_id: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    firmware: Option<[u8; 2]>,
    wait_us: u16,
    /// Seconds since 1970, since it has to outlast a reboot.
    used: u64,
}

#[derive(Debug, Default)]
pub struct Waits {
    path: Option<PathBuf>,
    /// None until the first receiver is admitted.
    saved: Option<Vec<Entry>>,
    changed: bool,
    written: Option<u64>,
}

/// Receiver id, lane and product id.
pub type Device<'a> = (&'a str, Lane, u16);

fn unix_s() -> u64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    now.map_or(0, |d| d.as_secs())
}

fn write(path: &Path, saved: &[Entry]) -> std::io::Result<()> {
    let new = path.with_extension("json.new");
    let mut file = std::fs::File::create(&new)?;
    file.write_all(&serde_json::to_vec(saved)?)?;
    file.sync_all()?;
    std::fs::rename(new, path)
}

impl Waits {
    #[must_use]
    pub fn new(dir: Option<&Path>) -> Self {
        let path = dir.map(|d| d.join(FILE));
        Self {
            path,
            ..Self::default()
        }
    }

    /// Reads the file the first time a receiver is admitted.
    pub fn load(&mut self) {
        let (Some(path), None) = (&self.path, &self.saved) else {
            return;
        };
        let read =
            std::fs::read(path).map_err(|e| (e.kind() == ErrorKind::NotFound, e.to_string()));
        let parsed =
            read.and_then(|b| serde_json::from_slice(&b).map_err(|e| (false, e.to_string())));
        let mut saved: Vec<Entry> = parsed.unwrap_or_else(|(missing, why)| {
            if !missing {
                eprintln!(
                    "{}: {why}; starting from the catalog's waits",
                    path.display()
                );
            }
            Vec::new()
        });
        let (now, all) = (unix_s(), saved.len());
        saved.retain(|e| now - e.used.min(now) < KEEP_UNUSED_S);
        self.changed = saved.len() < all;
        self.saved = Some(saved);
    }

    fn forget(&mut self, (receiver_id, lane, _): Device) {
        if let Some(saved) = self.saved.as_mut() {
            saved.retain(|e| e.receiver_id != receiver_id || e.lane != lane);
            self.changed = true;
        }
    }

    /// The wait kept for the device's lane, if it is for this product.
    /// Another product's goes.
    fn entry(&mut self, device: Device) -> Option<&mut Entry> {
        let (receiver_id, lane, product_id) = device;
        let waits = self.saved.as_mut()?;
        let i = waits
            .iter()
            .position(|e| e.receiver_id == receiver_id && e.lane == lane)?;
        if waits[i].product_id != product_id {
            self.forget(device);
            return None;
        }
        self.saved.as_mut().map(|s| &mut s[i])
    }

    /// The wait to go on from for a device that has just paired, if one
    /// was kept for it.
    pub fn start(&mut self, device: Device) -> Option<u16> {
        let now = unix_s();
        let e = self.entry(device)?;
        let (wait, old) = (e.wait_us, now.saturating_sub(e.used) >= USED_STEP_S);
        if old {
            e.used = now;
        }
        self.changed |= old;
        Some(wait)
    }

    /// The device's learned wait moved, and `firmware` is what it runs, if
    /// the bridge knows.
    pub fn learned(&mut self, device: Device, firmware: Option<[u8; 2]>, wait_us: u16) {
        let used = unix_s();
        if let Some(e) = self.entry(device) {
            (e.wait_us, e.used, e.firmware) = (wait_us, used, firmware.or(e.firmware));
        } else if let Some(saved) = self.saved.as_mut() {
            let (receiver_id, lane, product_id) = (device.0.to_owned(), device.1, device.2);
            let e = Entry {
                receiver_id,
                lane,
                product_id,
                firmware,
                wait_us,
                used,
            };
            saved.push(e);
        }
        self.changed |= self.saved.is_some();
    }

    /// The device answered 00:81. A wait kept for other firmware goes:
    /// true when that happened.
    pub fn firmware(&mut self, device: Device, firmware: [u8; 2]) -> bool {
        let Some(e) = self.entry(device) else {
            return false;
        };
        let was = e.firmware.replace(firmware);
        self.changed |= was != Some(firmware);
        let other = was.is_some_and(|was| was != firmware);
        if other {
            self.forget(device);
        }
        other
    }

    /// When [`Waits::save`] has something to write.
    #[must_use]
    pub fn due(&self) -> Option<u64> {
        let ready = self.changed && self.path.is_some();
        ready.then(|| self.written.map_or(0, |at| at + WRITE_EVERY_MS))
    }

    /// Writes the file, through a new one renamed over it, when due. A
    /// write that fails is tried again after the next change.
    pub fn save(&mut self, now: u64) {
        let (Some(path), Some(saved)) = (&self.path, &self.saved) else {
            return;
        };
        if self.due().is_none_or(|at| at > now) {
            return;
        }
        (self.written, self.changed) = (Some(now), false);
        if let Err(e) = write(path, saved) {
            eprintln!("{}: can't write it ({e})", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUSE: Device = ("receiver-1", Lane::Pointer, 0x00cd);
    const KEYBOARD: Device = ("receiver-1", Lane::Keyboard, 0x0296);

    fn loaded(dir: &Path) -> Waits {
        let mut w = Waits::new(Some(dir));
        w.load();
        w
    }

    #[test]
    fn kept_until_device_changes() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = loaded(dir.path());
        w.learned(MOUSE, None, 5_000);
        w.learned(KEYBOARD, Some([1, 2]), 5_500);
        assert!(!w.firmware(MOUSE, [2, 0x33]));
        w.save(1_000);
        let mut w = loaded(dir.path());
        assert_eq!(w.start(KEYBOARD), Some(5_500));
        assert_eq!(w.start(("receiver-2", Lane::Keyboard, 0x0296)), None);
        assert!(!w.firmware(KEYBOARD, [1, 2]) && w.firmware(MOUSE, [2, 0x34]));
        assert_eq!(w.start(MOUSE), None);
        assert_eq!(w.start(("receiver-1", Lane::Keyboard, 0x0290)), None);
        assert_eq!(w.start(KEYBOARD), None);
    }

    #[test]
    fn written_once_a_minute() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = loaded(dir.path());
        w.save(0);
        assert!(!dir.path().join(FILE).exists() && w.due().is_none());
        w.learned(MOUSE, None, 3_500);
        w.save(5_000);
        w.learned(MOUSE, None, 4_000);
        assert_eq!(w.due(), Some(65_000));
        w.save(64_999);
        assert_eq!(loaded(dir.path()).start(MOUSE), Some(3_500));
        w.save(65_000);
        assert_eq!(loaded(dir.path()).start(MOUSE), Some(4_000));
        assert!(w.due().is_none());
    }

    #[test]
    fn old_or_unreadable_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = |text: &str| std::fs::write(dir.path().join(FILE), text).unwrap();
        let days_ago = |days: u64| {
            let used = unix_s() - days * 86_400;
            let e =
                r#""receiver_id":"receiver-1","lane":"pointer","product_id":205,"wait_us":5000"#;
            format!(r#"[{{{e},"used":{used}}}]"#)
        };
        file(&days_ago(29));
        assert_eq!(loaded(dir.path()).start(MOUSE), Some(5_000));
        for bad in [&days_ago(31), "", "[{}]", "not json"] {
            file(bad);
            assert_eq!(loaded(dir.path()).start(MOUSE), None);
        }
        // Without a state directory nothing is kept, and nothing fails.
        let mut w = Waits::new(None);
        w.load();
        w.learned(MOUSE, None, 5_000);
        w.save(0);
        assert_eq!((w.start(MOUSE), w.due()), (None, None));
    }
}
