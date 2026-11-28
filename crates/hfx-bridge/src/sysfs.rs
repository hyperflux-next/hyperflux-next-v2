// SPDX-License-Identifier: GPL-2.0-only

//! What the virtual devices read from /sys/bus/hid: which HID device a
//! driver has bound for one of our phys labels, and whether a wired Razer
//! device the same driver handles over USB is there.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const BUS_USB: u16 = 0x0003;

/// Bus, vendor and product of a HID device.
type HidIds = (u16, u16, u16);

/// `BBBB:VVVV:PPPP.NNNN`, the name of a HID device in sysfs.
#[must_use]
pub fn parse_hid_name(name: &str) -> Option<HidIds> {
    let (ids, instance) = name.split_once('.')?;
    let mut parts = ids.split(':');
    let mut hex = || -> Option<u16> {
        let part = parts.next()?;
        (part.len() == 4)
            .then(|| u16::from_str_radix(part, 16).ok())
            .flatten()
    };
    let parsed = (hex()?, hex()?, hex()?);
    (parts.next().is_none()
        && instance.len() == 4
        && instance.bytes().all(|b| b.is_ascii_hexdigit()))
    .then_some(parsed)
}

fn read_small(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(4096).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn entries(dir: &Path) -> io::Result<Vec<(String, HidIds)>> {
    let list = match std::fs::read_dir(dir) {
        Ok(list) => list,
        // A driver that isn't loaded has bound nothing.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in list {
        let name = entry?.file_name();
        if let Some(name) = name.to_str()
            && let Some(ids) = parse_hid_name(name)
        {
            out.push((name.to_owned(), ids));
        }
    }
    Ok(out)
}

/// The device `driver` has bound whose `HID_PHYS` is exactly `phys`.
///
/// # Errors
/// Fails when the driver directory can't be listed.
pub fn bound_device(hid_drivers: &Path, driver: &str, phys: &str) -> io::Result<Option<String>> {
    let dir = hid_drivers.join(driver);
    let wanted = format!("HID_PHYS={phys}");
    for (name, _) in entries(&dir)? {
        let Ok(uevent) = read_small(&dir.join(&name).join("uevent")) else {
            continue;
        };
        if uevent
            .split(|b| *b == b'\n')
            .any(|line| line == wanted.as_bytes())
        {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// Whether the device `driver` bound for `phys` has a power supply of
/// its own, which the driver registers under it.
#[must_use]
pub fn driver_battery(hid_drivers: &Path, driver: &str, phys: &str) -> bool {
    let Ok(Some(name)) = bound_device(hid_drivers, driver, phys) else {
        return false;
    };
    let supplies = hid_drivers.join(driver).join(name).join("power_supply");
    std::fs::read_dir(supplies).is_ok_and(|mut d| d.next().is_some())
}

#[must_use]
pub fn still_bound(hid_drivers: &Path, driver: &str, name: &str) -> bool {
    hid_drivers.join(driver).join(name).exists()
}

/// The hash in a `serial_sha256` attribute: the patched drivers write the
/// serial's fingerprint there as lowercase hex, world-readable, so the
/// bridge can read it although `device_serial` is its group's alone. None
/// for anything but exactly 32 bytes of lowercase hex.
fn serial_hash(bytes: &[u8]) -> Option<[u8; 32]> {
    let text = std::str::from_utf8(bytes).ok()?.trim_end();
    hfx_proto::ids::decode_lower_hex(text)?.try_into().ok()
}

/// Fingerprints read from `serial_sha256`, by the bound device's directory.
/// A device gets a new name each time it's plugged in, so a fingerprint
/// never goes stale, and each device's file is read once. The bridge looks
/// for wired devices whenever a HID device comes or goes, unplugging
/// included, and a read while the device is being removed holds up the
/// removal: on 2026-11-23 one such read died in the kernel, and the USB
/// hub stayed stuck until the reference was released by hand.
pub type Prints = HashMap<PathBuf, [u8; 32]>;

/// Whether a wired (USB) device with product id `wired_pid` is there: its
/// control interface bound to `driver`, or a part of it not bound to any
/// driver yet, since the kernel announces a device before its driver binds
/// it and OpenRazer's daemon starts on it soon after. If so, the serial
/// fingerprints that could be read from the patched drivers'
/// `serial_sha256`. OpenRazer drives a device through one interface, the
/// one with `device_type`, and the others only carry keys or pointer
/// movement. Those don't count alone: they are what's left of a device the
/// kernel didn't finish removing, which no app can use. We skip a missing
/// or unreadable hash. A hash compares equal to the bridge's own
/// fingerprint of the serial read through the receiver.
///
/// # Errors
/// Fails when the driver directory cannot be listed.
pub fn wired_twin(
    hid_drivers: &Path,
    driver: &str,
    wired_pid: u16,
    prints: &mut Prints,
) -> io::Result<Option<Vec<[u8; 32]>>> {
    let vendor = hfx_proto::devices::RECEIVER.vendor_id;
    let wired = |(_, ids): &(String, HidIds)| *ids == (BUS_USB, vendor, wired_pid);
    let dir = hid_drivers.join(driver);
    let control: Vec<PathBuf> = entries(&dir)?
        .into_iter()
        .filter(wired)
        .map(|(n, _)| dir.join(n))
        .filter(|path| path.join("device_type").exists())
        .collect();
    prints.retain(|path, _| path.exists());
    let all = hid_drivers.with_file_name("devices");
    let mut unbound = entries(&all).unwrap_or_default().into_iter().filter(wired);
    if control.is_empty() && !unbound.any(|(name, _)| !all.join(name).join("driver").exists()) {
        return Ok(None);
    }
    let mut found = Vec::new();
    for path in control {
        let print = prints.get(&path).copied().or_else(|| {
            let print = serial_hash(&read_small(&path.join("serial_sha256")).ok()?)?;
            prints.insert(path, print);
            Some(print)
        });
        found.extend(print);
    }
    Ok(Some(found))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode;

    /// A `serial_sha256` value the way the patched driver writes it.
    fn serial_attr(serial: &[u8]) -> String {
        format!(
            "{}\n",
            hfx_proto::ids::lower_hex(&decode::fingerprint(serial).unwrap())
        )
    }

    #[test]
    fn hid_names() {
        assert_eq!(
            parse_hid_name("0006:1532:00CD.0003"),
            Some((6, 0x1532, 0xcd))
        );
        assert_eq!(
            parse_hid_name("0003:1532:00CC.000A"),
            Some((3, 0x1532, 0xcc))
        );
        for bad in [
            "bind",
            "unbind",
            "new_id",
            "0006:1532:00CD",
            "0006:1532:00CD.03",
            "06:1532:00CD.0003",
        ] {
            assert_eq!(parse_hid_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn finds_only_our_phys() {
        let root = tempfile::tempdir().unwrap();
        let driver = root.path().join("razermouse");
        for (name, phys) in [
            (
                "0006:1532:00CD.0003",
                "hyperflux-next/native/v1/native-aaaa",
            ),
            (
                "0006:1532:00CD.0004",
                "hyperflux-next/native/v1/native-bbbb",
            ),
        ] {
            std::fs::create_dir_all(driver.join(name)).unwrap();
            std::fs::write(
                driver.join(name).join("uevent"),
                format!("DRIVER=razermouse\nHID_PHYS={phys}\nHID_UNIQ=\n"),
            )
            .unwrap();
        }
        assert_eq!(
            bound_device(
                root.path(),
                "razermouse",
                "hyperflux-next/native/v1/native-bbbb"
            )
            .unwrap()
            .as_deref(),
            Some("0006:1532:00CD.0004")
        );
        assert_eq!(
            bound_device(
                root.path(),
                "razermouse",
                "hyperflux-next/native/v1/native-b"
            )
            .unwrap(),
            None
        );
        assert_eq!(bound_device(root.path(), "razerkbd", "x").unwrap(), None);
    }

    #[test]
    fn wired_twins() {
        let root = tempfile::tempdir().unwrap();
        let (drivers, all) = (root.path().join("drivers"), root.path().join("devices"));
        let mut prints = Prints::new();
        let mut twin = |pid| wired_twin(&drivers, "razermouse", pid, &mut prints).unwrap();
        // Announced, and no driver has it yet.
        std::fs::create_dir_all(all.join("0003:1532:00CC.0007")).unwrap();
        std::fs::create_dir_all(all.join("0003:1532:00AA.0008/driver")).unwrap();
        assert_eq!((twin(0x00cc), twin(0x00aa)), (Some(Vec::new()), None));
        std::fs::create_dir(all.join("0003:1532:00CC.0007/driver")).unwrap();
        assert_eq!(twin(0x00cc), None);
        // Bound, with the hash on the control interface, and it equals the
        // fingerprint the bridge makes from the serial read through the receiver.
        let control = drivers.join("razermouse/0003:1532:00CC.0007");
        std::fs::create_dir_all(&control).unwrap();
        std::fs::create_dir_all(drivers.join("razermouse/0003:1532:00CC.0006")).unwrap();
        std::fs::write(control.join("device_type"), "Razer Basilisk V3 Pro 35K\n").unwrap();
        // A hash that isn't 32 bytes of lowercase hex is no hash.
        std::fs::write(control.join("serial_sha256"), "not a hash\n").unwrap();
        assert_eq!(twin(0x00cc), Some(Vec::new()));
        // Unreadable is unknown, never an error.
        std::fs::remove_file(control.join("serial_sha256")).unwrap();
        std::fs::create_dir(control.join("serial_sha256")).unwrap();
        assert_eq!(twin(0x00cc), Some(Vec::new()));
        std::fs::remove_dir(control.join("serial_sha256")).unwrap();
        std::fs::write(
            control.join("serial_sha256"),
            serial_attr(b"PM2342H100000001"),
        )
        .unwrap();
        let print = decode::fingerprint(b"PM2342H100000001").unwrap();
        assert_eq!(twin(0x00cc), Some(vec![print]));
        // Read once: later calls don't open the file again.
        std::fs::remove_file(control.join("serial_sha256")).unwrap();
        assert_eq!(twin(0x00cc), Some(vec![print]));
        assert_eq!(
            wired_twin(&drivers, "razerkbd", 0x0298, &mut Prints::new()).unwrap(),
            None
        );
    }

    #[test]
    fn half_removed_device() {
        let root = tempfile::tempdir().unwrap();
        let (drivers, all) = (root.path().join("drivers"), root.path().join("devices"));
        let mut prints = Prints::new();
        // The control interface went, the two key interfaces stayed bound.
        for name in ["0003:1532:0298.0053", "0003:1532:0298.0054"] {
            std::fs::create_dir_all(drivers.join("razerkbd").join(name)).unwrap();
            std::fs::create_dir_all(all.join(name).join("driver")).unwrap();
        }
        assert_eq!(
            wired_twin(&drivers, "razerkbd", 0x0298, &mut prints).unwrap(),
            None
        );
    }

    #[test]
    fn prints_follow_devices() {
        let root = tempfile::tempdir().unwrap();
        let (drivers, all) = (root.path().join("drivers"), root.path().join("devices"));
        let mut prints = Prints::new();
        let name = "0003:1532:0298.0052";
        let control = drivers.join("razerkbd").join(name);
        std::fs::create_dir_all(&control).unwrap();
        std::fs::create_dir_all(all.join(name).join("driver")).unwrap();
        std::fs::write(
            control.join("device_type"),
            "Razer DeathStalker V2 Pro TKL\n",
        )
        .unwrap();
        std::fs::write(
            control.join("serial_sha256"),
            serial_attr(b"PM2329F67300033"),
        )
        .unwrap();
        wired_twin(&drivers, "razerkbd", 0x0298, &mut prints).unwrap();
        assert_eq!(prints.len(), 1);
        // Unplugged: the next look forgets it.
        std::fs::remove_dir_all(&control).unwrap();
        std::fs::remove_dir_all(all.join(name)).unwrap();
        assert_eq!(
            wired_twin(&drivers, "razerkbd", 0x0298, &mut prints).unwrap(),
            None
        );
        assert!(prints.is_empty());
    }
}
