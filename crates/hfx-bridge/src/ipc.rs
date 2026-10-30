// SPDX-License-Identifier: GPL-2.0-only

//! What the bridge and the device keeper say to each other. The bridge
//! listens on `keeper.sock` beside its own socket and the keeper connects.
//! Each side checks the other's uid with SO_PEERCRED before it reads a
//! byte. Messages use the client protocol's framing, a big-endian u32
//! length and then JSON, but a frame may hold at most 1 KiB and has to be
//! one of the fixed shapes below, with no field more or less. A frame that
//! doesn't parse ends the connection. One that parses but asks for
//! something the other side won't do is refused and logged, and the
//! connection stays.

use hfx_proto::framing::{FrameReader, encode_frame};
use hfx_proto::ids::{HexBytes, ReceiverId, ReportHex};
use hfx_proto::messages::Lane;
use rustix::fd::{AsFd, BorrowedFd};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;

pub const VERSION: u32 = 1;
pub const SOCKET_NAME: &str = "keeper.sock";
/// The longest message, a reply with its report, takes about 300 bytes.
pub const MAX_FRAME: usize = 1024;
/// Output waiting for the peer to take it. Past this it counts as gone.
const MAX_OUTPUT: usize = 256 << 10;
/// Vendor reports go over with this much of their start.
pub const VENDOR_HEAD: usize = 16;

/// From the keeper. Key reports and the mouse's movement never come over
/// as anything but `Activity`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum FromKeeper {
    /// `uhid` says whether the keeper could open /dev/uhid.
    Hello {
        version: u32,
        uhid: bool,
    },
    Activity {
        receiver: ReceiverId,
        lane: Lane,
        time_ms: u64,
    },
    /// A vendor report from interface 1: its first bytes and its length.
    Vendor {
        receiver: ReceiverId,
        time_ms: u64,
        report: HexBytes,
        len: u16,
    },
    /// The kernel's GET of an OpenRazer device's feature report, or with
    /// `set` its SET.
    Request {
        device: u64,
        id: u32,
        set: Option<ReportHex>,
    },
    Started {
        device: u64,
        flags: u64,
    },
    /// The driver let go of an OpenRazer device (UHID_STOP).
    Stopped {
        device: u64,
    },
    /// The device is gone: making it failed, its file failed, or it was
    /// destroyed as the bridge asked. `why` says which.
    Ended {
        device: u64,
        why: String,
    },
}

/// From the bridge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ToKeeper {
    Hello {
        version: u32,
    },
    Create {
        device: u64,
        template: Template,
    },
    Destroy {
        device: u64,
    },
    /// The answer to a request: for a GET the report, or EIO without one;
    /// for a SET 0 or EIO.
    Reply {
        device: u64,
        id: u32,
        errno: u16,
        data: Option<ReportHex>,
    },
    Battery {
        device: u64,
        level: u8,
        charging: bool,
    },
}

/// All the keeper ever makes. It names them from the catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum Template {
    /// The feature-only device the patched OpenRazer drivers bind.
    #[serde(rename = "openrazer")]
    OpenRazer { product_id: u16, label: String },
    /// A mouse or keyboard collection that never sends, and battery report 2.
    Battery {
        receiver: ReceiverId,
        lane: Lane,
        product_id: u16,
    },
}

/// The hex digits of a receiver id as `bridge::receiver_id` makes it.
#[must_use]
pub fn receiver_hex(id: &ReceiverId) -> Option<&str> {
    let hex = id.as_str().strip_prefix("receiver-")?;
    let digit = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    (hex.len() == 24 && hex.bytes().all(digit)).then_some(hex)
}

/// Text that may go into a log line and the snapshot.
#[must_use]
pub fn plain(text: &str) -> bool {
    !text.is_empty() && text.len() <= 200 && text.bytes().all(|b| (0x20..0x7f).contains(&b))
}

/// The uid of a system user, from /etc/passwd, where systemd-sysusers
/// puts it.
#[must_use]
pub fn system_uid(name: &str) -> Option<u32> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next()? == name).then_some(())?;
        fields.nth(1)?.parse().ok()
    })
}

/// A connected socket to the other side, non-blocking in both directions.
pub struct Channel {
    stream: UnixStream,
    reader: FrameReader,
    out: Vec<u8>,
    writing: bool,
    pub dead: bool,
}

impl Channel {
    /// # Errors
    /// Says why when the peer doesn't run as `uid` or the socket cannot be
    /// set up.
    pub fn new(stream: UnixStream, uid: u32) -> Result<Self, String> {
        let peer = rustix::net::sockopt::socket_peercred(&stream)
            .map_err(|e| e.to_string())?
            .uid
            .as_raw();
        if peer != uid {
            return Err("it runs as the wrong user".into());
        }
        stream.set_nonblocking(true).map_err(|e| e.to_string())?;
        Ok(Self {
            stream,
            reader: FrameReader::new(),
            out: Vec::new(),
            writing: false,
            dead: false,
        })
    }

    #[must_use]
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }

    /// The frames that came in. Marks the channel dead at the end of the
    /// stream, on errors and on an oversized frame.
    pub fn read(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        let mut buffer = [0_u8; 4096];
        while !self.dead {
            match self.stream.read(&mut buffer) {
                Ok(0) => self.dead = true,
                Ok(n) => {
                    self.dead = self.reader.push(&buffer[..n]).is_err();
                    loop {
                        match self.reader.next_frame() {
                            Ok(Some(frame)) if frame.len() <= MAX_FRAME => frames.push(frame),
                            Ok(None) => break,
                            _ => {
                                self.dead = true;
                                break;
                            }
                        }
                    }
                    // The rest is the start of one frame.
                    let left = FrameReader::MAX_BUFFERED - self.reader.room();
                    self.dead |= left > MAX_FRAME + 4;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => self.dead = true,
            }
        }
        frames
    }

    pub fn send(&mut self, message: &impl Serialize) {
        let frame = serde_json::to_vec(message).ok();
        match frame.and_then(|payload| encode_frame(&payload).ok()) {
            Some(frame) => self.out.extend_from_slice(&frame),
            None => self.dead = true,
        }
        self.flush();
    }

    /// Writes what the socket takes.
    pub fn flush(&mut self) {
        while !self.out.is_empty() {
            match self.stream.write(&self.out) {
                Ok(0) => break,
                Ok(n) => {
                    self.out.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.dead = true;
                    break;
                }
            }
        }
        self.dead |= self.out.len() > MAX_OUTPUT;
    }

    /// Updates the remembered state and says if write interest must change.
    pub fn write_interest_changed(&mut self) -> Option<bool> {
        let want = !self.out.is_empty();
        (want != self.writing).then(|| {
            self.writing = want;
            want
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_keeper(text: &str) -> Option<ToKeeper> {
        serde_json::from_str(text).ok()
    }

    #[test]
    fn shapes_are_fixed() {
        let create = r#"{"kind":"create","device":17,"template":{"openrazer":{"product_id":205,"label":"native-0123456789abcdef"}}}"#;
        let Some(ToKeeper::Create { device: 17, .. }) = to_keeper(create) else {
            panic!("{create}")
        };
        let battery = ToKeeper::Create {
            device: 18,
            template: Template::Battery {
                receiver: ReceiverId::new("receiver-6b1f0e9c2a7d4e83b5c1f2a0").unwrap(),
                lane: Lane::Keyboard,
                product_id: 0x0296,
            },
        };
        let text = serde_json::to_string(&battery).unwrap();
        assert_eq!(to_keeper(&text), Some(battery));
        for bad in [
            create.replace("openrazer", "keyboard"),
            create.replace(r#""device":17"#, r#""device":17,"name":"x""#),
            create.replace(r#","label":"native-0123456789abcdef""#, ""),
            r#"{"kind":"input","device":17,"report":"020101"}"#.into(),
            r#"{"kind":"battery","device":17,"level":300,"charging":false}"#.into(),
            r#"{"kind":"destroy","device":-1}"#.into(),
        ] {
            assert_eq!(to_keeper(&bad), None, "{bad}");
        }
        let vendor =
            r#"{"kind":"vendor","receiver":"receiver-0","time_ms":5,"report":"0531ca","len":6}"#;
        assert!(serde_json::from_str::<FromKeeper>(vendor).is_ok());
        let keys = vendor.replace(r#""len":6"#, r#""len":6,"keys":"04""#);
        assert!(serde_json::from_str::<FromKeeper>(&keys).is_err());
    }

    #[test]
    fn receiver_ids_and_text() {
        let id = |s: &str| ReceiverId::new(s).unwrap();
        assert!(receiver_hex(&id("receiver-6b1f0e9c2a7d4e83b5c1f2a0")).is_some());
        for bad in [
            "receiver-6B1F0E9C2A7D4E83B5C1F2A0",
            "receiver-6b1f",
            "6b1f0e9c2a7d4e83b5c1f2a0aaaaaaaaa",
        ] {
            assert!(receiver_hex(&id(bad)).is_none(), "{bad}");
        }
        assert!(plain("can't open /dev/uhid (Permission denied)"));
        assert!(!plain("one line\nanother") && !plain("") && !plain(&"x".repeat(201)));
    }

    #[test]
    fn oversized_frames_end_the_channel() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let uid = rustix::process::geteuid().as_raw();
        let mut channel = Channel::new(ours, uid).unwrap();
        let mut theirs = theirs;
        let small = encode_frame(br#"{"kind":"destroy","device":1}"#).unwrap();
        theirs.write_all(&small).unwrap();
        assert_eq!(channel.read().len(), 1);
        assert!(!channel.dead);
        theirs
            .write_all(&((MAX_FRAME + 1) as u32).to_be_bytes())
            .unwrap();
        theirs.write_all(&vec![b' '; MAX_FRAME + 1]).unwrap();
        assert!(channel.read().is_empty() && channel.dead);
        let (other, _keep) = UnixStream::pair().unwrap();
        assert!(Channel::new(other, uid + 1).is_err());
    }
}
