//! Searching the local network for romexplorer servers from this device
//! (`romexplorer_types::discovery`): what the sign-in screen lists under "On this network".
//! Plain blocking UDP on a thread of its own, so it works the same in the desktop app and on
//! the phone, which has no async runtime to lend it.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use romexplorer_types::discovery::{Announce, Found, PORT, PROBE};

fn search_blocking(wait: Duration) -> Vec<Found> {
    let Ok(socket) = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))) else { return Vec::new() };
    let _ = socket.set_broadcast(true);
    for to in romexplorer_types::discovery::broadcast_targets() {
        let _ = socket.send_to(PROBE, SocketAddr::from((to, PORT)));
    }
    let mut found: Vec<Found> = Vec::new();
    let mut buf = [0u8; 2048];
    let end = Instant::now() + wait;
    while let Some(left) = end.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) {
        let _ = socket.set_read_timeout(Some(left));
        let Ok((n, from)) = socket.recv_from(&mut buf) else { break };
        let Ok(announce) = serde_json::from_slice::<Announce>(&buf[..n]) else { continue };
        if announce.app != "romexplorer" {
            continue;
        }
        let f = Found { ip: from.ip().to_string(), announce };
        if !found.iter().any(|g| g.ip == f.ip && g.announce.port == f.announce.port) {
            found.push(f);
        }
    }
    found.sort_by(|a, b| a.announce.name.cmp(&b.announce.name).then(a.announce.port.cmp(&b.announce.port)));
    found
}

/// The romexplorer servers and shares that answered within about a second and a half.
pub async fn search() -> Vec<Found> {
    let out = std::sync::Arc::new(std::sync::Mutex::new(None::<Vec<Found>>));
    let slot = out.clone();
    std::thread::spawn(move || {
        let found = search_blocking(Duration::from_millis(1500));
        *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(found);
    });
    loop {
        if let Some(found) = out.lock().unwrap_or_else(|p| p.into_inner()).take() {
            return found;
        }
        futures_timer::Delay::new(Duration::from_millis(100)).await;
    }
}
