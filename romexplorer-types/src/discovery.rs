//! Finding romexplorer servers on the local network: a client broadcasts [`PROBE`] over UDP
//! to [`PORT`], and each server answers with one [`Announce`] per listener (its API, and its
//! share when advertised), straight back to the asker. Plain UDP broadcast rather than mDNS: a phone
//! can send one and read the answer without asking Android for a multicast lock, and nothing
//! has to run on the network but romexplorer itself.
//!
//! A server answers only askers with a private address, and says no more than the sign-in
//! screen would: its name, port and whether it speaks TLS.

use serde::{Deserialize, Serialize};

/// What a client sends. Anything else is ignored.
pub const PROBE: &[u8] = b"romexplorer-discover/1";

/// The one UDP port every search asks on, whatever TCP ports the servers listen on: each
/// answer carries its own [`Announce::port`], so a server moved off 7337 is still found.
pub const PORT: u16 = 7337;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Announce {
    /// Always "romexplorer": tells a real answer from something else on the port.
    pub app: String,
    /// The machine's own name.
    pub name: String,
    /// The TCP port its API (or share) is on.
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

/// Where a search sends its probe: the limited broadcast (255.255.255.255), plus this
/// device's own subnet's broadcast (taken as a /24 of the address it would reach the network
/// from). Android and some routers drop the limited broadcast on Wi-Fi, and a machine with
/// more than one network sends it out of only one of them; the subnet's own is delivered.
pub fn broadcast_targets() -> Vec<std::net::Ipv4Addr> {
    use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
    let mut out = vec![Ipv4Addr::BROADCAST];
    // Connecting a UDP socket sends nothing; it only picks the route, and with it the
    // address this device has on that network.
    let local = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
        .and_then(|s| s.connect(SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), 9))).and_then(|_| s.local_addr()));
    if let Ok(SocketAddr::V4(a)) = local {
        let [a1, a2, a3, _] = a.ip().octets();
        if !a.ip().is_unspecified() && !a.ip().is_loopback() {
            out.push(Ipv4Addr::new(a1, a2, a3, 255));
        }
    }
    out
}
