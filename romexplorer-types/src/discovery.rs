//! Finding romexplorer servers on the local network: a client broadcasts [`PROBE`] over UDP
//! to the usual ports, and each server or share listening there answers with an
//! [`Announce`], straight back to the asker. Plain UDP broadcast rather than mDNS: a phone
//! can send one and read the answer without asking Android for a multicast lock, and nothing
//! has to run on the network but romexplorer itself.
//!
//! A server answers only askers with a private address, and says no more than the sign-in
//! screen would: its name, port and whether it speaks TLS.

use serde::{Deserialize, Serialize};

/// What a client sends. Anything else is ignored.
pub const PROBE: &[u8] = b"romexplorer-discover/1";

/// The ports a search asks on: the server's default, and the next one, where a second
/// listener (a share) usually is.
pub const PORTS: [u16; 2] = [7337, 7338];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Announce {
    /// Always "romexplorer": tells a real answer from something else on the port.
    pub app: String,
    /// The machine's own name.
    pub name: String,
    /// The TCP port its API is on (the same number the probe reached).
    pub port: u16,
    pub tls: bool,
    /// `server` (the whole API: sign in there) or `share` (a client's read-only share).
    pub kind: String,
    /// romexplorer's version on that machine.
    #[serde(default)]
    pub version: String,
}

/// A server found by a search: where it answered from, and what it said.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Found {
    pub ip: String,
    #[serde(flatten)]
    pub announce: Announce,
}

impl Found {
    /// The address to type into the sign-in screen: `https://192.168.1.5:7337`.
    pub fn base(&self) -> String {
        let ip = if self.ip.contains(':') { format!("[{}]", self.ip) } else { self.ip.clone() };
        format!("{}://{ip}:{}", if self.announce.tls { "https" } else { "http" }, self.announce.port)
    }
}
