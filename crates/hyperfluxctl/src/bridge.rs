// SPDX-License-Identifier: GPL-2.0-only

//! Finding the bridge. A normal user goes through the per-user socket that
//! systemd forwards, then the system socket (members of the hyperflux
//! group), and root goes straight to the system socket. Each run says hello
//! with its own client id, which keeps two hyperfluxctl processes apart.

use hfx_proto::client::{self, Client, ClientConfig, ClientError, PeerCheck};
use hfx_proto::paths;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

pub const CLIENT_NAME: &str = "hyperfluxctl";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub peer: PeerCheck,
    /// The per-user socket, whose absence means the client socket unit
    /// isn't enabled for this user.
    pub per_user: bool,
}

/// `--socket` wins. Otherwise the protocol's order (which also honours
/// `HYPERFLUX_NEXT_SOCKET`).
#[must_use]
pub fn candidates(explicit: Option<&Path>) -> Vec<Candidate> {
    if let Some(path) = explicit {
        return vec![Candidate {
            path: path.to_path_buf(),
            peer: PeerCheck::Any,
            per_user: false,
        }];
    }
    let user_socket = paths::user_socket(rustix::process::geteuid().as_raw());
    client::socket_candidates()
        .into_iter()
        .map(|(path, peer)| Candidate {
            per_user: path == user_socket && peer == PeerCheck::Uid(0),
            path,
            peer,
        })
        .collect()
}

pub enum Reach {
    Up(Box<Client>),
    /// The per-user socket is missing and the system socket refused us.
    NoAccess {
        user_socket: PathBuf,
    },
    /// Nothing usable answered; the text says why.
    Down(String),
}

#[must_use]
pub fn config(required: &[&str]) -> ClientConfig {
    ClientConfig::new(CLIENT_NAME, CLIENT_NAME, required, &[])
}

/// Tries each candidate in order and stops at the first bridge that
/// accepts the hello.
#[must_use]
pub fn reach(candidates: &[Candidate], required: &[&str]) -> Reach {
    let mut user_missing = None;
    let mut system_denied = false;
    let mut last = String::from("no socket to try");
    for candidate in candidates {
        let mut config = config(required);
        config.peer = candidate.peer;
        match Client::connect(&candidate.path, &config) {
            Ok(client) => return Reach::Up(Box::new(client)),
            Err(error) => {
                if candidate.per_user && matches!(error, ClientError::Connect(ErrorKind::NotFound))
                {
                    user_missing = Some(candidate.path.clone());
                }
                if !candidate.per_user
                    && matches!(error, ClientError::Connect(ErrorKind::PermissionDenied))
                {
                    system_denied = true;
                }
                last = describe(&candidate.path, &error);
                // A bridge that answered and refused us won't change its
                // mind on the other socket: it's the same bridge.
                if matches!(error, ClientError::Bridge(_)) {
                    break;
                }
            }
        }
    }
    match user_missing {
        Some(user_socket) if system_denied => Reach::NoAccess { user_socket },
        _ => Reach::Down(last),
    }
}

fn describe(path: &Path, error: &ClientError) -> String {
    let shown = path.display();
    match error {
        ClientError::Connect(ErrorKind::NotFound) => format!("{shown} doesn't exist"),
        ClientError::Connect(ErrorKind::ConnectionRefused) => {
            format!("nothing is listening on {shown}")
        }
        ClientError::Connect(ErrorKind::PermissionDenied) => {
            format!("no permission to open {shown}")
        }
        ClientError::Bridge(rpc) => format!(
            "the bridge refused hyperfluxctl: [{}] {}",
            rpc.code, rpc.message
        ),
        other => format!("{shown}: {other}"),
    }
}

/// For commands that can't do anything without the bridge.
///
/// # Errors
/// A message for the user when no bridge answered.
pub fn connect(explicit: Option<&Path>, required: &[&str]) -> Result<Client, String> {
    match reach(&candidates(explicit), required) {
        Reach::Up(client) => Ok(*client),
        Reach::NoAccess { user_socket } => Err(format!(
            "this login can't reach the HyperFlux Next V2 bridge ({} is missing); run hyperfluxctl doctor",
            user_socket.display()
        )),
        Reach::Down(why) => Err(format!(
            "can't reach the HyperFlux Next V2 bridge: {why}; run hyperfluxctl doctor"
        )),
    }
}

/// Turns a protocol error into one line for the user.
#[must_use]
pub fn failed(what: &str, error: &ClientError) -> crate::Failure {
    crate::Failure::new(format!("{what} failed: {error}"))
}
