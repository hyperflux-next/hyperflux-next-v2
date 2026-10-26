// SPDX-License-Identifier: GPL-2.0-only

//! What the virtual devices read from /sys/bus/hid: which HID device a
//! driver has bound for one of our phys labels, and whether a wired Razer
//! device the same driver handles over USB is there.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

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

/// Whether a wired (USB) device with product id `wired_pid` is there: bound
/// to `driver`, or not bound to any driver yet, since the kernel announces
/// a device before its driver binds it and OpenRazer's daemon starts on it
/// soon after. If so, the serial fingerprints that could be read from the
/// patched drivers' `serial_sha256`. OpenRazer puts the attributes on a
/// single interface, and an unbound device has none yet. We skip a missing
/// or unreadable hash. A hash compares equal to the bridge's own
/// fingerprint of the serial read through the receiver.
///
/// # Errors
/// Fails when the driver directory cannot be listed.
pub fn wired_twin(
    hid_drivers: &Path,
    driver: &str,
    wired_pid: u16,
) -> io::Result<Option<Vec<[u8; 32]>>> {
    let vendor = hfx_proto::devices::RECEIVER.vendor_id;
    let wired = |(_, ids): &(String, HidIds)| *ids == (BUS_USB, vendor, wired_pid);
    let dir = hid_drivers.join(driver);
    let bound: Vec<String> = entries(&dir)?
        .into_iter()
        .filter(wired)
        .map(|(n, _)| n)
        .collect();
    let all = hid_drivers.with_file_name("devices");
    let mut unbound = entries(&all).unwrap_or_default().into_iter().filter(wired);
    if bound.is_empty() && !unbound.any(|(name, _)| !all.join(name).join("driver").exists()) {
        return Ok(None);
    }
    let hash = |name: &String| read_small(&dir.join(name).join("serial_sha256")).ok();
    let prints = bound.iter().filter_map(hash);
    Ok(Some(prints.filter_map(|s| serial_hash(&s)).collect()))
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
        let twin = |pid| wired_twin(&drivers, "razermouse", pid).unwrap();
        // Announced, and no driver has it yet.
        std::fs::create_dir_all(all.join("0003:1532:00CC.0007")).unwrap();
        std::fs::create_dir_all(all.join("0003:1532:00AA.0008/driver")).unwrap();
        assert_eq!((twin(0x00cc), twin(0x00aa)), (Some(Vec::new()), None));
        std::fs::create_dir(all.join("0003:1532:00CC.0007/driver")).unwrap();
        assert_eq!(twin(0x00cc), None);
        // Bound, with the hash on a single interface, and it equals the
        // fingerprint the bridge makes from the serial read through the receiver.
        let control = drivers.join("razermouse/0003:1532:00CC.0007");
        std::fs::create_dir_all(&control).unwrap();
        std::fs::create_dir_all(drivers.join("razermouse/0003:1532:00CC.0006")).unwrap();
        std::fs::write(
            control.join("serial_sha256"),
            serial_attr(b"PM2342H100000001"),
        )
        .unwrap();
        let print = decode::fingerprint(b"PM2342H100000001").unwrap();
        assert_eq!(twin(0x00cc), Some(vec![print]));
        // A hash that isn't 32 bytes of lowercase hex is no hash.
        std::fs::write(control.join("serial_sha256"), "not a hash\n").unwrap();
        assert_eq!(twin(0x00cc), Some(Vec::new()));
        // Unreadable is unknown, never an error.
        std::fs::remove_file(control.join("serial_sha256")).unwrap();
        std::fs::create_dir(control.join("serial_sha256")).unwrap();
        assert_eq!(twin(0x00cc), Some(Vec::new()));
        assert_eq!(wired_twin(&drivers, "razerkbd", 0x0298).unwrap(), None);
    }
}
