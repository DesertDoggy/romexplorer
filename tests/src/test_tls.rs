//! The TLS listener and certificate pinning, over real loopback sockets. No database: a tiny
//! router stands in for the API, so what is tested is the transport -- what `server::tls`
//! accepts, what it refuses, and that a client pinning the certificate gets through while one
//! expecting a different certificate does not.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ConnectInfo;
use axum::routing::get;
use axum::serve::ListenerExt;
use axum::Router;
use romexplorer::server::tls::{self, TlsListener};
use romexplorer_types::pinning::{client_config, Pin};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A router behind the TLS listener on an ephemeral loopback port; returns its address and the
/// certificate's fingerprint.
async fn start(name: &str) -> (SocketAddr, String, Scratch) {
    let dir = std::env::temp_dir().join(format!("rx_test_tls_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let tls = tls::load_or_create(&dir, None).unwrap();
    let fingerprint = tls.fingerprint.clone();
    let app = Router::new()
        .route("/hello", get(|| async { "hello" }))
        // Proves `ConnectInfo<SocketAddr>` still reaches handlers through the custom listener --
        // the private-IP check depends on it.
        .route("/peer", get(|ConnectInfo(addr): ConnectInfo<SocketAddr>| async move { addr.ip().to_string() }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let listener = TlsListener::new(listener, tls).unwrap().tap_io(|_| {});
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
    });
    (addr, fingerprint, Scratch(dir))
}

fn client(pin: Pin) -> reqwest::Client {
    reqwest::Client::builder().use_preconfigured_tls(client_config(pin)).timeout(Duration::from_secs(10)).build().unwrap()
}

#[tokio::test]
async fn a_pinned_client_gets_through() {
    let (addr, fingerprint, _dir) = start("pinned").await;
    let body = client(Pin::Expect(fingerprint)).get(format!("https://{addr}/hello")).send().await.unwrap().text().await.unwrap();
    assert_eq!(body, "hello");
    let peer = client(Pin::Expect(String::new())).get(format!("https://{addr}/peer")).send().await;
    assert!(peer.is_err(), "an empty pin is not a wildcard");
}

#[tokio::test]
async fn a_different_certificate_is_refused() {
    let (addr, _, _dir) = start("wrong").await;
    let wrong = "0".repeat(64);
    let err = client(Pin::Expect(wrong)).get(format!("https://{addr}/hello")).send().await.unwrap_err();
    let text = format!("{err:?}");
    assert!(text.contains("certificate changed"), "the error says why: {text}");
}

/// The first connection: whatever the server presents is recorded, for the caller to show and
/// store -- and it is the certificate's real fingerprint.
#[tokio::test]
async fn the_first_connection_captures_the_fingerprint() {
    let (addr, fingerprint, _dir) = start("capture").await;
    let slot = Arc::new(Mutex::new(None));
    let peer = client(Pin::Capture(slot.clone())).get(format!("https://{addr}/peer")).send().await.unwrap().text().await.unwrap();
    assert_eq!(peer, "127.0.0.1", "ConnectInfo came through the TLS listener");
    assert_eq!(slot.lock().unwrap().as_deref(), Some(fingerprint.as_str()));
}

/// This machine can still use plain HTTP (the console on http://127.0.0.1, local tools).
#[tokio::test]
async fn plain_http_from_this_machine_is_served() {
    let (addr, _, _dir) = start("plain").await;
    let body = reqwest::get(format!("http://{addr}/hello")).await.unwrap().text().await.unwrap();
    assert_eq!(body, "hello");
}

/// One client that opens a TLS handshake and then says nothing must not stop anyone else.
#[tokio::test]
async fn a_stalled_handshake_does_not_block_others() {
    let (addr, fingerprint, _dir) = start("stall").await;
    let mut stalled = tokio::net::TcpStream::connect(addr).await.unwrap();
    stalled.write_all(&[0x16, 0x03, 0x01]).await.unwrap();
    let started = std::time::Instant::now();
    let body = client(Pin::Expect(fingerprint)).get(format!("https://{addr}/hello")).send().await.unwrap().text().await.unwrap();
    assert_eq!(body, "hello");
    assert!(started.elapsed() < Duration::from_secs(5), "served while the other handshake hangs");
    // And a connection that never sends anything gets closed rather than kept forever.
    let mut buf = [0u8; 1];
    let closed = tokio::time::timeout(Duration::from_secs(15), stalled.read(&mut buf)).await;
    assert!(matches!(closed, Ok(Ok(0)) | Ok(Err(_))), "the stalled handshake is dropped after the timeout");
}
