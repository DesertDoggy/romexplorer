//! Answers network searches for romexplorer servers (`romexplorer_types::discovery`): a UDP
//! socket on the listener's own port number that replies to a probe from a private address
//! with this machine's name, port and TLS. Runs beside the TCP listener and stops with it.

use std::net::{IpAddr, SocketAddr};

use romexplorer_types::discovery::{Announce, PROBE};

/// Answers probes on `bind:port` until `stop` resolves. A port already taken for UDP (another
/// program, or a second romexplorer on this machine) only means this one isn't found by
/// searching; the address can still be typed in, so that is logged, not an error.
pub async fn answer(bind: IpAddr, port: u16, tls: bool, kind: &'static str, stop: impl std::future::Future<Output = ()>) {
    // A listener only this machine can reach has nobody to be found by. Otherwise the socket
    // takes every address: a broadcast reaches only a socket bound to the wildcard.
    if bind.is_loopback() || port == 0 {
        return;
    }
    let any = if bind.is_ipv4() { IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED) } else { IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED) };
    let socket = match tokio::net::UdpSocket::bind(SocketAddr::new(any, port)).await {
        Ok(s) => s,
        Err(e) => {
            log::warn!("network search answers on UDP port {port} are off: {e}");
            return;
        }
    };
    let announce = Announce { app: "romexplorer".into(), name: crate::api::mobile::host_name(), port, tls, kind: kind.into(), version: env!("CARGO_PKG_VERSION").into() };
    let Ok(reply) = serde_json::to_vec(&announce) else { return };
    let serve = async {
        let mut buf = [0u8; 64];
        loop {
            let Ok((n, from)) = socket.recv_from(&mut buf).await else { continue };
            if &buf[..n] == PROBE && super::is_private_ip(from.ip()) {
                let _ = socket.send_to(&reply, from).await;
            }
        }
    };
    tokio::select! {
        _ = serve => {}
        _ = stop => {}
    }
}

/// Searches the local network from this machine: what the Sources dialog lists to add. This
/// machine's own listeners are left out.
pub async fn search(wait: std::time::Duration) -> std::io::Result<Vec<romexplorer_types::discovery::Found>> {
    use romexplorer_types::discovery::{Found, PORTS};
    let socket = tokio::net::UdpSocket::bind(SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0)).await?;
    socket.set_broadcast(true)?;
    for port in PORTS {
        let _ = socket.send_to(PROBE, SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::BROADCAST), port)).await;
    }
    let me = crate::api::mobile::host_name();
    let mut found: Vec<Found> = Vec::new();
    let mut buf = [0u8; 2048];
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Ok((n, from))) = tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await {
        let Ok(announce) = serde_json::from_slice::<Announce>(&buf[..n]) else { continue };
        if announce.app != "romexplorer" || announce.name == me {
            continue;
        }
        let f = Found { ip: from.ip().to_string(), announce };
        if !found.iter().any(|g| g.ip == f.ip && g.announce.port == f.announce.port) {
            found.push(f);
        }
    }
    found.sort_by(|a, b| a.announce.name.cmp(&b.announce.name).then(a.announce.port.cmp(&b.announce.port)));
    Ok(found)
}
