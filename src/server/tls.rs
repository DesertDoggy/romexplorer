//! TLS on the server's network listeners.
//!
//! Once accounts exist, a username and password cross the LAN at every sign-in, and a device
//! token with every request. Over plain HTTP anyone else on the same Wi-Fi reads them, and
//! hashing on the server does nothing for the wire. So the listener speaks TLS.
//!
//! **The certificate** is self-signed on first start (`data/tls/`) unless the user supplies
//! one. There is no CA for a machine at home and its address is usually DHCP, so clients don't
//! check a name: native clients and other romexplorer machines pin the certificate itself
//! (`romexplorer_types::pinning`), and a browser shows its warning once.
//!
//! **One port, sniffed.** Each connection's first byte says what it is. A TLS handshake is
//! served. Plain HTTP from this machine is served as before (local tools, and the console on
//! `http://127.0.0.1`). Plain HTTP from anywhere else gets a redirect to `https://` and never
//! reaches the API -- so an old bookmark still lands somewhere useful.
//!
//! **Handshakes run on their own tasks**, with a timeout, and finished connections are handed
//! to axum through a channel: one client that connects and says nothing can't hold up the rest.

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Handshakes in flight at once; more wait for a slot rather than piling up.
const MAX_PENDING: usize = 256;

/// A loaded certificate, ready to serve.
#[derive(Clone)]
pub struct Tls {
    pub acceptor: TlsAcceptor,
    /// SHA-256 of the certificate, hex -- what clients pin and what the console shows.
    pub fingerprint: String,
}

impl std::fmt::Debug for Tls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tls").field("fingerprint", &self.fingerprint).finish_non_exhaustive()
    }
}

/// The certificate and key to serve: the user's own if both paths are given, else the
/// self-signed pair in `dir`, created on first use.
pub fn load_or_create(dir: &Path, custom: Option<(PathBuf, PathBuf)>) -> io::Result<Tls> {
    let (cert_path, key_path) = match custom {
        Some(pair) => pair,
        None => {
            let pair = (dir.join("cert.pem"), dir.join("key.pem"));
            if !pair.0.is_file() || !pair.1.is_file() {
                generate(dir, &pair.0, &pair.1)?;
            }
            pair
        }
    };
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(&cert_path)
        .and_then(|it| it.collect::<Result<Vec<_>, _>>())
        .map_err(|e| io::Error::other(format!("{}: {e}", cert_path.display())))?;
    let first = certs.first().ok_or_else(|| io::Error::other(format!("{}: no certificate in it", cert_path.display())))?;
    let fingerprint = romexplorer_types::pinning::fingerprint(first.as_ref());
    let key = PrivateKeyDer::from_pem_file(&key_path).map_err(|e| io::Error::other(format!("{}: {e}", key_path.display())))?;
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| io::Error::other(format!("{}: {e}", cert_path.display())))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Tls { acceptor: TlsAcceptor::from(Arc::new(config)), fingerprint })
}

/// A self-signed certificate for this machine's names and addresses. They are informational
/// -- clients pin the certificate rather than check a name -- but a browser shows them, and
/// with the right ones a user-installed copy of the certificate works without a warning.
fn generate(dir: &Path, cert_path: &Path, key_path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut names = vec!["localhost".to_string(), "127.0.0.1".to_string(), "::1".to_string()];
    if let Some(host) = sysinfo::System::host_name().filter(|h| !h.is_empty()) {
        names.push(host.clone());
        names.push(format!("{host}.local"));
    }
    if let Some(ip) = primary_address() {
        names.push(ip.to_string());
    }
    names.dedup();
    let certified = rcgen::generate_simple_self_signed(names).map_err(io::Error::other)?;
    let key_pem = certified.signing_key.serialize_pem();
    let mut key_file = crate::database::vault::create_private(key_path).or_else(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists {
            std::fs::remove_file(key_path)?;
            crate::database::vault::create_private(key_path)
        } else {
            Err(e)
        }
    })?;
    std::io::Write::write_all(&mut key_file, key_pem.as_bytes())?;
    std::fs::write(cert_path, certified.cert.pem())?;
    log::info!("created a self-signed TLS certificate in {}", dir.display());
    Ok(())
}

/// The address this machine reaches the network from. A UDP socket "connected" to a public
/// address sends nothing; it just makes the OS pick the outgoing interface.
fn primary_address() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    socket.local_addr().ok().map(|a| a.ip()).filter(|ip| !ip.is_unspecified())
}

/// One accepted connection: TLS, or plain HTTP from this machine.
pub enum Conn {
    Tls(Box<TlsStream<TcpStream>>),
    Plain(TcpStream),
}

impl AsyncRead for Conn {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
            Conn::Plain(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Conn {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
            Conn::Plain(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
            Conn::Plain(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
            Conn::Plain(s) => Pin::new(s).poll_shutdown(cx),
        }
    }

    fn poll_write_vectored(self: Pin<&mut Self>, cx: &mut Context<'_>, bufs: &[io::IoSlice<'_>]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_write_vectored(cx, bufs),
            Conn::Plain(s) => Pin::new(s).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            Conn::Tls(s) => s.is_write_vectored(),
            Conn::Plain(s) => s.is_write_vectored(),
        }
    }
}

/// An `axum::serve` listener that does the sniffing and the handshakes described above.
pub struct TlsListener {
    ready: tokio::sync::mpsc::Receiver<(Conn, SocketAddr)>,
    local: SocketAddr,
}

impl TlsListener {
    pub fn new(listener: TcpListener, tls: Tls) -> io::Result<Self> {
        let local = listener.local_addr()?;
        let (tx, ready) = tokio::sync::mpsc::channel(64);
        let slots = Arc::new(tokio::sync::Semaphore::new(MAX_PENDING));
        tokio::spawn(async move {
            loop {
                let (tcp, addr) = tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok(pair) => pair,
                        Err(e) => {
                            // Out of file descriptors and the like: back off rather than spin.
                            log::warn!("accept failed: {e}");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            continue;
                        }
                    },
                    _ = tx.closed() => break,
                };
                let Ok(permit) = slots.clone().acquire_owned().await else { break };
                let (tx, acceptor) = (tx.clone(), tls.acceptor.clone());
                tokio::spawn(async move {
                    if let Some(conn) = classify(tcp, addr, &acceptor).await {
                        let _ = tx.send((conn, addr)).await;
                    }
                    drop(permit);
                });
            }
        });
        Ok(Self { ready, local })
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = Conn;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready.recv().await {
            Some(pair) => pair,
            // The accept task only ends when this listener is dropped.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.local)
    }
}

/// What a new connection is, and what to do with it.
#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Tls,
    PlainLocal,
    PlainRemote,
}

/// A TLS record starts with 0x16 (handshake); an HTTP request with a method name in ASCII.
fn kind_of(first_byte: u8, peer: SocketAddr) -> Kind {
    if first_byte == 0x16 {
        Kind::Tls
    } else if peer.ip().is_loopback() || peer.ip().to_canonical().is_loopback() {
        Kind::PlainLocal
    } else {
        Kind::PlainRemote
    }
}

async fn classify(tcp: TcpStream, peer: SocketAddr, acceptor: &TlsAcceptor) -> Option<Conn> {
    let mut first = [0u8; 1];
    let n = tokio::time::timeout(HANDSHAKE_TIMEOUT, tcp.peek(&mut first)).await.ok()?.ok()?;
    if n == 0 {
        return None;
    }
    match kind_of(first[0], peer) {
        Kind::Tls => match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await {
            Ok(Ok(stream)) => Some(Conn::Tls(Box::new(stream))),
            Ok(Err(e)) => {
                // Most often a browser refusing the self-signed certificate before the user
                // has accepted it -- expected, so not a warning.
                log::debug!("TLS handshake with {peer} failed: {e}");
                None
            }
            Err(_) => None,
        },
        Kind::PlainLocal => Some(Conn::Plain(tcp)),
        Kind::PlainRemote => {
            redirect(tcp).await;
            None
        }
    }
}

/// Reads the request line and headers of a plain-HTTP request and answers with a redirect to
/// the same place over `https://`. Nothing is passed to the API.
async fn redirect(mut tcp: TcpStream) {
    let mut head = Vec::with_capacity(1024);
    let mut buf = [0u8; 1024];
    let read = async {
        while head.len() < 8192 && !head.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = tcp.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            head.extend_from_slice(&buf[..n]);
        }
        io::Result::Ok(())
    };
    if !matches!(tokio::time::timeout(Duration::from_secs(5), read).await, Ok(Ok(()))) {
        return;
    }
    let response = redirect_response(&String::from_utf8_lossy(&head));
    let _ = tcp.write_all(response.as_bytes()).await;
    let _ = tcp.shutdown().await;
}

/// `308` to `https://<Host><path>`, or `400` if there is no Host to send them to.
fn redirect_response(head: &str) -> String {
    let mut lines = head.split("\r\n");
    let target = lines.next().and_then(|l| l.split(' ').nth(1)).filter(|t| t.starts_with('/')).unwrap_or("/");
    let host = lines.find_map(|l| {
        let (name, value) = l.split_once(':')?;
        name.trim().eq_ignore_ascii_case("host").then(|| value.trim().to_string())
    });
    // Only characters a host (with port, or a bracketed v6 address) can hold -- the value is
    // echoed back in a header.
    match host.filter(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))) {
        Some(host) => {
            let target: String = target.chars().filter(|c| !c.is_control()).collect();
            format!("HTTP/1.1 308 Permanent Redirect\r\nLocation: https://{host}{target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        }
        None => "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 29\r\nConnection: close\r\n\r\nthis server only speaks HTTPS".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_is_told_apart_by_its_first_byte() {
        let lan: SocketAddr = "192.168.1.20:5000".parse().unwrap();
        let local: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let mapped: SocketAddr = "[::ffff:127.0.0.1]:5000".parse().unwrap();
        assert_eq!(kind_of(0x16, lan), Kind::Tls);
        assert_eq!(kind_of(0x16, local), Kind::Tls);
        assert_eq!(kind_of(b'G', local), Kind::PlainLocal);
        assert_eq!(kind_of(b'G', mapped), Kind::PlainLocal);
        assert_eq!(kind_of(b'G', lan), Kind::PlainRemote);
        assert_eq!(kind_of(b'P', lan), Kind::PlainRemote);
    }

    #[test]
    fn plain_http_from_the_lan_is_sent_to_https() {
        let r = redirect_response("GET /app/?x=1 HTTP/1.1\r\nHost: 192.168.1.5:7337\r\nUser-Agent: x\r\n\r\n");
        assert!(r.starts_with("HTTP/1.1 308"), "{r}");
        assert!(r.contains("Location: https://192.168.1.5:7337/app/?x=1\r\n"), "{r}");
        let host_case = redirect_response("GET / HTTP/1.1\r\nhOsT: nas.local\r\n\r\n");
        assert!(host_case.contains("Location: https://nas.local/\r\n"));
        // No usable Host: say so rather than redirect somewhere made up.
        assert!(redirect_response("GET / HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 400"));
        assert!(redirect_response("GET / HTTP/1.1\r\nHost: evil\r\nX: y\r\n\r\n").contains("https://evil/"));
        assert!(redirect_response("GET / HTTP/1.1\r\nHost: a\"b\r\n\r\n").starts_with("HTTP/1.1 400"), "a Host that isn't a host");
    }

    #[test]
    fn a_certificate_is_made_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("rx_tls_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = load_or_create(&dir, None).unwrap();
        let again = load_or_create(&dir, None).unwrap();
        assert_eq!(first.fingerprint, again.fingerprint, "the second start serves the same certificate");
        assert_eq!(first.fingerprint.len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join("key.pem")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
