//! Answers network searches for romexplorer servers (`romexplorer_types::discovery`): a UDP
//! socket on the fixed discovery port that replies to a probe from a private address with
//! one announcement per listener -- this machine's name, the listener's port and TLS. Runs
//! beside the TCP listeners and stops with them.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use romexplorer_types::discovery::{Announce, PORT, PROBE};

/// One listener to announce: its TCP port and what it is (`server` or `share`).
pub struct Listener {
    pub port: u16,
    pub kind: &'static str,
}

/// The discovery socket: the wildcard address (a broadcast reaches only a socket bound to
/// it), shared with any other romexplorer on this machine so both are found.
fn bind_shared() -> std::io::Result<tokio::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    let _ = socket.set_reuse_port(true);
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    tokio::net::UdpSocket::from_std(socket.into())
}

/// Answers probes until `stop` resolves. The discovery port already taken by another program
/// only means this server isn't found by searching; its address can still be typed in, so
/// that is logged, not an error.
pub async fn answer(bind: IpAddr, tls: bool, listeners: Vec<Listener>, stop: impl std::future::Future<Output = ()>) {
    // A server only this machine can reach has nobody to be found by.
    if bind.is_loopback() || listeners.is_empty() {
        return;
    }
    let socket = match bind_shared() {
        Ok(s) => s,
        Err(e) => {
            log::warn!("network search answers on UDP port {PORT} are off: {e}");
            return;
        }
    };
    let name = crate::api::mobile::host_name();
    let replies: Vec<Vec<u8>> = listeners
        .iter()
        .filter(|l| l.port != 0)
        .filter_map(|l| serde_json::to_vec(&Announce { app: "romexplorer".into(), name: name.clone(), port: l.port, tls, kind: l.kind.into(), version: env!("CARGO_PKG_VERSION").into() }).ok())
        .collect();
    let serve = async {
        let mut buf = [0u8; 64];
        loop {
            let Ok((n, from)) = socket.recv_from(&mut buf).await else { continue };
            if &buf[..n] == PROBE && super::is_private_ip(from.ip()) {
                for reply in &replies {
                    let _ = socket.send_to(reply, from).await;
                }
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
    use romexplorer_types::discovery::{Found, PORT};
    let socket = tokio::net::UdpSocket::bind(SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0)).await?;
    socket.set_broadcast(true)?;
    for to in romexplorer_types::discovery::broadcast_targets() {
        let _ = socket.send_to(PROBE, SocketAddr::new(IpAddr::V4(to), PORT)).await;
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
