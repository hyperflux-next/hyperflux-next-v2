// SPDX-License-Identifier: GPL-2.0-only

//! The error catalog: the `HFX-nnn` codes the bridge sends and Doctor
//! reports, with the text users see.
//!
//! Numbering: 1xx are sent on the wire by the bridge, 2xx describe the
//! service, 3xx drivers, 4xx the OpenRazer and OpenRGB integration. A code
//! keeps its meaning forever and retired codes are not reused. Retired so
//! far: HFX-301 and HFX-302, which were about the receiver kernel module.
//!
//! Texts may contain `{name}` placeholders that the reporter fills in with
//! [`render`]. docs/user/troubleshooting.md has one section per code,
//! anchored `#hfx-nnn`.

use crate::ids::{ErrorCodeText, HumanMessage};
use crate::messages::RpcError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Warning,
    Error,
}

impl Severity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorCode {
    // Wire errors and outcome codes.
    NoCommonVersion,
    FeatureUnavailable,
    InvalidRequest,
    ClientIdInUse,
    NoLease,
    Busy,
    StaleGeneration,
    NotControllable,
    NothingSent,
    MayHaveBeenSent,
    DeadlineBeforeSend,
    Internal,
    // Service.
    ServiceDown,
    ReceiverNotReady,
    NoDeviceOnline,
    NoSocketAccess,
    NoBatteryEntry,
    InventoryNotRecognised,
    OpenRazerDeviceFailed,
    KeeperDown,
    // Driver.
    RetiredDriverInstalled,
    SecureBootBlocked,
    InertDriverRecords,
    // OpenRazer / OpenRGB integration.
    OpenRazerDriverNotActive,
    OpenRazerDriverNotPatched,
    OpenRazerGroupMissing,
    OpenRazerNotReady,
    FullMismatch,
    KeyLinkMissing,
    BrokenInputLink,
    UhidNotWritable,
    UhidSharedWithUser,
    LampArrayOpenToUser,
}

pub struct CatalogEntry {
    pub code: ErrorCode,
    pub id: &'static str,
    pub severity: Severity,
    /// What happened, for the user. Also the wire `message` of 1xx codes.
    pub summary: &'static str,
    /// Why, in technical terms.
    pub cause: &'static str,
    /// What to do about it.
    pub action: &'static str,
    /// How to check that it worked.
    pub verify: &'static str,
}

const fn entry(
    code: ErrorCode,
    id: &'static str,
    severity: Severity,
    summary: &'static str,
    cause: &'static str,
    action: &'static str,
    verify: &'static str,
) -> CatalogEntry {
    CatalogEntry {
        code,
        id,
        severity,
        summary,
        cause,
        action,
        verify,
    }
}

use ErrorCode as C;
use Severity::{Error as E, Warning as W};

pub const CATALOG: &[CatalogEntry] = &[
    entry(
        C::NoCommonVersion,
        "HFX-101",
        E,
        "This application and the HyperFlux Next V2 service don't share a protocol version.",
        "The client's min_version..max_version range contains no version the bridge serves.",
        "Update the application or HyperFlux Next V2 so both speak a common protocol version.",
        "Start the application again and check that it connects.",
    ),
    entry(
        C::FeatureUnavailable,
        "HFX-102",
        E,
        "This feature isn't available from the running HyperFlux Next V2 service.",
        "The method is unknown, or it needs a feature that wasn't enabled in hello.",
        "Ask for the feature in hello, or update HyperFlux Next V2.",
        "Connect again and check the features listed in the welcome.",
    ),
    entry(
        C::InvalidRequest,
        "HFX-103",
        E,
        "The application sent a request HyperFlux Next V2 can't accept.",
        "The request breaks a protocol rule, reuses a request id with different content, or carries a report the device's command list doesn't allow.",
        "Fix the request. Resending it unchanged gives the same error.",
        "Repeat the operation and check that it's accepted.",
    ),
    entry(
        C::ClientIdInUse,
        "HFX-104",
        E,
        "Another connection is already using this client id.",
        "Two live connections said hello with the same client_id.",
        "Use a client_id that's unique per connection, for example with the process id and a random suffix.",
        "Connect again and check that hello succeeds.",
    ),
    entry(
        C::NoLease,
        "HFX-105",
        W,
        "The application doesn't hold control of this device.",
        "The lease is unknown, expired, held by another client, or covers a different device.",
        "Acquire a new lease for the device before sending more reports.",
        "Check that acquire-lease returns granted.",
    ),
    entry(
        C::Busy,
        "HFX-106",
        W,
        "HyperFlux Next V2 is busy and didn't start this operation.",
        "A bounded limit (connections, leases or replay memory) is full.",
        "Try again in a moment.",
        "Repeat the operation and check that it's accepted.",
    ),
    entry(
        C::StaleGeneration,
        "HFX-107",
        W,
        "The receiver reconnected, so this device handle is out of date.",
        "The route token belongs to a receiver generation that has ended.",
        "List the routes again and use the new token.",
        "Check that list-routes shows the device with a new token.",
    ),
    entry(
        C::NotControllable,
        "HFX-108",
        W,
        "HyperFlux Next V2 can't control this device that way.",
        "The route token is unknown, the device has no qualified route, or the device doesn't support the requested query.",
        "List the routes again. Only qualified routes can be controlled.",
        "Check that list-routes shows the device as qualified.",
    ),
    entry(
        C::NothingSent,
        "HFX-109",
        W,
        "The receiver couldn't take the request, so nothing was sent.",
        "The route is unavailable (the device is unreachable, or hasn't been heard from since the receiver was plugged in or the system resumed), the receiver isn't writable, or the exchange was refused before it was sent.",
        "Wait for the device to come back, then try again.",
        "Check that the device shows as online in hyperfluxctl devices.",
    ),
    entry(
        C::MayHaveBeenSent,
        "HFX-110",
        E,
        "The request may have reached the device, so HyperFlux Next V2 won't repeat it.",
        "The SET report was sent, but no matching response arrived or a later transfer failed.",
        "Read the device state again before deciding to send it once more. Never resend automatically.",
        "Check the device state with a read command.",
    ),
    entry(
        C::DeadlineBeforeSend,
        "HFX-111",
        W,
        "The request ran out of time before it could be sent.",
        "Its timeout_ms elapsed while it waited for the receiver.",
        "Send it again with a fresh timeout if it's still wanted.",
        "Repeat the operation and check that it's sent.",
    ),
    entry(
        C::Internal,
        "HFX-112",
        E,
        "HyperFlux Next V2 stopped this operation because an internal check failed.",
        "An invariant inside the bridge didn't hold.",
        "Create a support bundle with hyperfluxctl support-bundle and report it.",
        "Check that the problem doesn't come back after restarting the bridge.",
    ),
    entry(
        C::ServiceDown,
        "HFX-201",
        E,
        "The HyperFlux Next V2 service isn't running.",
        "hyperflux-next-bridge.service is {state}, or it didn't answer on its socket.",
        "Start it with: sudo systemctl enable --now hyperflux-next-bridge.service",
        "Run hyperfluxctl doctor again and check that the bridge is running.",
    ),
    entry(
        C::ReceiverNotReady,
        "HFX-202",
        E,
        "The receiver is plugged in, but the bridge can't use it yet.",
        "The bridge can't use the receiver: {receiver}. Its hidraw interface 0 has to belong to the hyperflux-next user and the hyperflux-keeper group with mode 0640, interface 1 to the hyperflux-keeper group with mode 0640, and interface 4 to the hyperflux-next group with mode 0660.",
        "Unplug the receiver, wait a few seconds and plug it back in, or reboot, so the HyperFlux Next V2 udev rules apply. If Doctor also reports HFX-303, deal with that first. If it keeps happening, create a support bundle.",
        "Run hyperfluxctl doctor again and check that the receiver is listed.",
    ),
    entry(
        C::NoDeviceOnline,
        "HFX-203",
        W,
        "The service is running, but no paired device is online yet.",
        "The receiver is ready, but no paired device has a usable route.",
        "Move or click the device. If it doesn't show up within 30 seconds, switch it off and on again.",
        "Run hyperfluxctl devices and check that the device is online.",
    ),
    entry(
        C::NoSocketAccess,
        "HFX-204",
        E,
        "This login can't reach the HyperFlux Next V2 service.",
        "The per-user socket {socket} is missing, and this user isn't allowed to open the system socket.",
        "Run: sudo systemctl enable --now \"hyperflux-next-client@$(id -u).socket\"",
        "Run hyperfluxctl doctor again as yourself and check that the bridge is running.",
    ),
    entry(
        C::NoBatteryEntry,
        "HFX-205",
        W,
        "The desktop doesn't show the battery of a paired device.",
        "The bridge has a battery reading for {device}, but {problem}, so UPower doesn't list it.",
        "If Doctor also reports HFX-208 or HFX-408, fix that first. Otherwise restart the bridge with: sudo systemctl restart hyperflux-next-bridge.service. If the battery still doesn't show, create a support bundle.",
        "Run upower --dump and check that the device is listed with its battery.",
    ),
    entry(
        C::InventoryNotRecognised,
        "HFX-206",
        W,
        "The bridge can't tell which devices are paired with the receiver.",
        "The receiver's pairing inventory {problem}, so a device that was already connected when the bridge started only shows up once the receiver names it.",
        "Move or wake each device. To help support this receiver, record what it reports with hyperfluxctl capture --seconds 60 --output receiver.jsonl and attach the file to a report.",
        "Run hyperfluxctl devices and check that every device is listed with its name.",
    ),
    entry(
        C::OpenRazerDeviceFailed,
        "HFX-207",
        E,
        "The bridge couldn't create the OpenRazer device for a paired device.",
        "The bridge couldn't create the OpenRazer device for {device}: {reason}. Until it does, OpenRazer and Polychromatic don't list the device.",
        "Switch the device off and on, or move it. If the finding stays, restart the bridge with: sudo systemctl restart hyperflux-next-bridge.service, and if that doesn't help, create a support bundle.",
        "Run hyperfluxctl devices and check that the device's OpenRazer line says live.",
    ),
    entry(
        C::KeeperDown,
        "HFX-208",
        E,
        "The device keeper isn't working, so batteries and OpenRazer devices are missing.",
        "hyperflux-next-keeper.service {problem}. The keeper holds the battery devices and the OpenRazer devices for the bridge, and reads the receiver's own reports for it.",
        "Restart it with: sudo systemctl restart hyperflux-next-keeper.service. If it stops again, its log says why: journalctl -b -u hyperflux-next-keeper.service",
        "Run hyperfluxctl doctor again and check that the finding is gone.",
    ),
    entry(
        C::RetiredDriverInstalled,
        "HFX-303",
        W,
        "An old receiver driver is still on this system and can take the receiver away from the bridge.",
        "Left over from an earlier release: {registration}. HyperFlux Next V2 reaches the receiver without a kernel module of its own.",
        "{remedy}",
        "Run hyperfluxctl doctor again and check that no old driver is listed.",
    ),
    entry(
        C::SecureBootBlocked,
        "HFX-304",
        E,
        "Secure Boot is blocking a driver HyperFlux Next V2 installed.",
        "Secure Boot is on and {module} isn't signed with a key the firmware trusts, so the kernel won't load it.",
        "Enroll the DKMS signing key once and reboot. The troubleshooting guide lists the commands for your distribution.",
        "After the reboot, run hyperfluxctl doctor and check that the driver is active.",
    ),
    entry(
        C::InertDriverRecords,
        "HFX-305",
        W,
        "DKMS still keeps records of an old receiver driver, but nothing uses them.",
        "DKMS has records of {records} from an earlier release. They can't put a module in place: there's no source tree to build from, and no module for any kernel installed now.",
        "Delete them when it suits you: sudo rm -r {paths}. hyperfluxctl doctor --fix asks before it runs that.",
        "Run hyperfluxctl doctor again and check that the finding is gone.",
    ),
    entry(
        C::OpenRazerDriverNotActive,
        "HFX-401",
        E,
        "The OpenRazer driver from HyperFlux Next V2 isn't active yet.",
        "{module} is missing, or an older copy of it is still loaded.",
        "Reboot. If Doctor still reports it, reinstall hyperflux-next-v2-openrazer-driver-dkms.",
        "Run hyperfluxctl doctor again and check that the OpenRazer driver is ready.",
    ),
    entry(
        C::OpenRazerDriverNotPatched,
        "HFX-402",
        E,
        "The installed OpenRazer driver isn't the HyperFlux Next V2 build.",
        "{module} doesn't advertise the hyperflux_native_transport capability.",
        "Reinstall hyperflux-next-v2-openrazer-driver-dkms, then reboot.",
        "Run hyperfluxctl doctor again and check that the OpenRazer driver is ready.",
    ),
    entry(
        C::OpenRazerGroupMissing,
        "HFX-403",
        E,
        "OpenRazer's access group is missing.",
        "The {group} group doesn't exist on this system.",
        "Run: sudo systemd-sysusers openrazer.conf. If that file is missing, reinstall hyperflux-next-v2-openrazer-driver-dkms.",
        "Run hyperfluxctl doctor again and check that the group finding is gone.",
    ),
    entry(
        C::OpenRazerNotReady,
        "HFX-404",
        E,
        "OpenRazer isn't ready for this login.",
        "This login isn't in the {group} group yet, openrazer-daemon isn't on the session bus, or it hasn't listed every device HyperFlux Next V2 offers it.",
        "Run: sudo gpasswd -a \"$USER\" {group}, log out and back in, and check that openrazer-daemon runs (systemctl --user status openrazer-daemon).",
        "As your normal user, run hyperfluxctl doctor and check that OpenRazer is ready.",
    ),
    entry(
        C::FullMismatch,
        "HFX-405",
        E,
        "The Full packages don't match the installed HyperFlux Next V2 version.",
        "The Full component file is unreadable or names another version.",
        "Install the hyperflux-next-v2-full package with the same version as hyperflux-next-v2-linux.",
        "Run hyperfluxctl doctor again and check that Full is ready.",
    ),
    entry(
        C::KeyLinkMissing,
        "HFX-406",
        W,
        "Ripple and other effects that react to key presses stay still.",
        "The key-press link {link} that OpenRazer looks for is missing. OpenRazer only looks for it when it adds the keyboard.",
        "Reboot so the udev rules create the link. If it's still missing afterwards, reinstall hyperflux-next-v2-full.",
        "Pick Ripple in Polychromatic and press a key.",
    ),
    entry(
        C::BrokenInputLink,
        "HFX-407",
        E,
        "A broken input link stops OpenRazer from listing any device.",
        "{link} points to a file that no longer exists, and openrazer-daemon stops adding devices when it reaches it.",
        "Run: sudo rm {link} and then: systemctl --user restart openrazer-daemon. HyperFlux Next V2 never removes it for you.",
        "Check that Polychromatic lists your devices again.",
    ),
    entry(
        C::UhidNotWritable,
        "HFX-408",
        E,
        "The device keeper can't create the virtual devices, so battery entries and OpenRazer devices are missing.",
        "The keeper needs /dev/uhid, with group hyperflux-keeper and mode 0660, but {uhid}.",
        "Reboot so the HyperFlux Next V2 udev rules apply and the uhid module loads. If it's still reported, reinstall hyperflux-next-v2-linux.",
        "Run hyperfluxctl doctor again and check that the finding is gone.",
    ),
    entry(
        C::UhidSharedWithUser,
        "HFX-409",
        W,
        "Programs other than the device keeper can create devices that OpenRazer would trust.",
        "An ACL lets uid {uid} write /dev/uhid, usually a uaccess tag such as the one in Sunshine's 60-sunshine.rules. Programs running as that user can create a virtual device that the patched OpenRazer drivers would trust as one of HyperFlux Next V2's.",
        "If you don't need Sunshine's virtual input, copy that rules file to /etc/udev/rules.d under the same name, remove the uaccess tag from its uhid line and reboot. Sunshine needs the tag for its virtual input, so HyperFlux Next V2 never removes it for you.",
        "Run hyperfluxctl doctor again and check that the warning is gone.",
    ),
    entry(
        C::LampArrayOpenToUser,
        "HFX-410",
        W,
        "Programs you run can open the receiver's lighting interfaces, so OpenRGB's LampArray driver would show your devices twice.",
        "{why}. Interfaces 3 and 4 are the receiver's HID LampArrays, and OpenRGB's generic LampArray driver opens any it can: it would list each paired device again under the receiver's name, the two would fight over the lights, and it leaves LampArray autonomous mode off when it exits.",
        "If this login is in the hyperflux-next group, leave it: sudo gpasswd -d \"$USER\" hyperflux-next, then log out and back in. Otherwise find the udev rule that opens the node to you and override it in /etc/udev/rules.d. The troubleshooting guide shows how.",
        "Run hyperfluxctl doctor again and check that the warning is gone.",
    ),
];

impl ErrorCode {
    /// # Panics
    /// Never: every code has exactly one entry (checked by a test).
    #[must_use]
    pub fn entry(self) -> &'static CatalogEntry {
        CATALOG
            .iter()
            .find(|entry| entry.code == self)
            .expect("every error code has a catalog entry")
    }

    #[must_use]
    pub fn id(self) -> &'static str {
        self.entry().id
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        CATALOG
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.code)
    }

    /// The code as a wire value.
    ///
    /// # Panics
    /// Never: catalog ids are short (checked by a test).
    #[must_use]
    pub fn text(self) -> ErrorCodeText {
        ErrorCodeText::new(self.id()).expect("catalog ids fit ErrorCodeText")
    }

    /// Relative link to the troubleshooting section for this code.
    #[must_use]
    pub fn documentation(self) -> String {
        format!(
            "docs/user/troubleshooting.md#{}",
            self.id().to_ascii_lowercase()
        )
    }
}

/// Fills `{name}` placeholders. Unknown placeholders are left as they are:
/// a missing value shows up in the output instead of vanishing.
#[must_use]
pub fn render(text: &str, values: &[(&str, &str)]) -> String {
    let mut out = text.to_owned();
    for (name, value) in values {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// The wire error for `code`. HFX-101 also lists the served versions, the
/// only way a client can learn what to offer next time.
///
/// # Panics
/// Never: catalog summaries plus the appended text stay under 512 bytes
/// (checked by a test).
#[must_use]
pub fn rpc_error(code: ErrorCode) -> RpcError {
    rpc_error_with(code, None)
}

/// Like [`rpc_error`], with one extra sentence appended to the message.
///
/// # Panics
/// Never for the sentences the bridge uses. An oversized message falls back
/// to the plain summary.
#[must_use]
pub fn rpc_error_with(code: ErrorCode, extra: Option<&str>) -> RpcError {
    let entry = code.entry();
    let mut message = entry.summary.to_owned();
    if code == ErrorCode::NoCommonVersion {
        message.push_str(" Supported protocol versions: ");
        message.push_str(&crate::negotiation::supported_versions_text());
        message.push('.');
    }
    if let Some(extra) = extra {
        message.push(' ');
        message.push_str(extra);
    }
    let message = HumanMessage::new(message)
        .or_else(|_| HumanMessage::new(entry.summary))
        .expect("catalog summaries fit HumanMessage");
    RpcError {
        code: code.text(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const ALL: &[ErrorCode] = &[
        C::NoCommonVersion,
        C::FeatureUnavailable,
        C::InvalidRequest,
        C::ClientIdInUse,
        C::NoLease,
        C::Busy,
        C::StaleGeneration,
        C::NotControllable,
        C::NothingSent,
        C::MayHaveBeenSent,
        C::DeadlineBeforeSend,
        C::Internal,
        C::ServiceDown,
        C::ReceiverNotReady,
        C::NoDeviceOnline,
        C::NoSocketAccess,
        C::NoBatteryEntry,
        C::InventoryNotRecognised,
        C::OpenRazerDeviceFailed,
        C::KeeperDown,
        C::RetiredDriverInstalled,
        C::SecureBootBlocked,
        C::InertDriverRecords,
        C::OpenRazerDriverNotActive,
        C::OpenRazerDriverNotPatched,
        C::OpenRazerGroupMissing,
        C::OpenRazerNotReady,
        C::FullMismatch,
        C::KeyLinkMissing,
        C::BrokenInputLink,
        C::UhidNotWritable,
        C::UhidSharedWithUser,
        C::LampArrayOpenToUser,
    ];

    #[test]
    fn catalog_is_complete_and_unique() {
        assert_eq!(CATALOG.len(), ALL.len());
        let ids: BTreeSet<_> = CATALOG.iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), CATALOG.len());
        for code in ALL {
            let entry = code.entry();
            assert_eq!(ErrorCode::from_id(entry.id), Some(*code));
            let digits = entry.id.strip_prefix("HFX-").unwrap();
            assert!(digits.len() == 3 && digits.bytes().all(|b| b.is_ascii_digit()));
            for text in [entry.summary, entry.cause, entry.action, entry.verify] {
                assert!(!text.is_empty() && text.len() <= 400, "{}", entry.id);
                assert!(text.is_ascii(), "{}", entry.id);
            }
        }
    }

    #[test]
    fn wire_codes_have_no_placeholders() {
        for entry in CATALOG.iter().filter(|e| e.id.starts_with("HFX-1")) {
            assert!(!entry.summary.contains('{'), "{}", entry.id);
        }
    }

    #[test]
    fn version_error_lists_served_versions() {
        let error = rpc_error(ErrorCode::NoCommonVersion);
        assert_eq!(error.code.as_str(), "HFX-101");
        assert_eq!(
            error.message.as_str(),
            "This application and the HyperFlux Next V2 service don't share a protocol version. \
             Supported protocol versions: 1."
        );
    }

    #[test]
    fn render_fills_placeholders() {
        let text = render(
            C::BrokenInputLink.entry().action,
            &[("link", "/dev/input/by-id/x")],
        );
        assert!(text.starts_with("Run: sudo rm /dev/input/by-id/x and then"));
        assert_eq!(render("{a} {b}", &[("a", "1")]), "1 {b}");
    }
}
