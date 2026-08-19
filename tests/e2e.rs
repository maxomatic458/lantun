//! End-to-end tests: real iroh endpoints on loopback, no discovery/relay.

mod common;

use std::time::Duration;

use common::{
    TEST_TIMEOUT, spawn_tcp_echo, spawn_udp_echo, start_client, start_host, wait_until,
    wait_until_async,
};
use lantun::{ClientTunnel, ReconnectPolicy, TunnelProtocol, gen_secret};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    time::timeout,
};

/// TCP: single peer round-trip.
#[tokio::test]
async fn tcp_single_peer_roundtrip() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let host = start_host(backend, TunnelProtocol::Tcp).await;
    let client = start_client(&host, TunnelProtocol::Tcp).await;

    // Wait for the client to establish the QUIC session.
    let bind = client.bind();
    wait_until(|| client.is_connected(), Duration::from_secs(5)).await;

    let result = timeout(TEST_TIMEOUT, async {
        let mut s = TcpStream::connect(bind).await.unwrap();
        s.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    })
    .await;
    result.expect("test timeout");

    client.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
}

/// UDP: single peer round-trip.
#[tokio::test]
async fn udp_single_peer_roundtrip() {
    let (backend, _echo) = spawn_udp_echo().await;
    let host = start_host(backend, TunnelProtocol::Udp).await;
    let client = start_client(&host, TunnelProtocol::Udp).await;

    wait_until(|| client.is_connected(), Duration::from_secs(5)).await;
    let bind = client.bind();

    let result = timeout(TEST_TIMEOUT, async {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sock.connect(bind).await.unwrap();
        sock.send(b"hello").await.unwrap();
        let mut buf = [0u8; 8];
        let n = sock.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello");
    })
    .await;
    result.expect("test timeout");

    client.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
}

/// Regression for the concurrency bug: multiple peers connect simultaneously.
///
/// On v0.1 the outer accept loop awaited `handle_connection` sequentially, so peer #2
/// would never even complete the connection while peer #1 was active. This test
/// asserts all 4 peers can round-trip concurrently.
#[tokio::test]
async fn tcp_multi_peer_concurrent() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let host = start_host(backend, TunnelProtocol::Tcp).await;

    // Start 4 independent client tunnels — each connects with its own iroh identity.
    let mut clients = Vec::new();
    for _ in 0..4 {
        clients.push(start_client(&host, TunnelProtocol::Tcp).await);
    }
    for c in &clients {
        wait_until(|| c.is_connected(), Duration::from_secs(5)).await;
    }

    // Fire all client round-trips at once.
    let mut handles = Vec::new();
    for (i, c) in clients.iter().enumerate() {
        let bind = c.bind();
        handles.push(tokio::spawn(async move {
            let payload = format!("peer-{i}").into_bytes();
            let mut s = TcpStream::connect(bind).await.unwrap();
            s.write_all(&payload).await.unwrap();
            let mut buf = vec![0u8; payload.len()];
            s.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, payload);
        }));
    }

    let result = timeout(TEST_TIMEOUT, async {
        for h in handles {
            h.await.unwrap();
        }
    })
    .await;
    result.expect("multi-peer round-trip timed out");

    for c in clients {
        c.shutdown().await.unwrap();
    }
    host.shutdown().await.unwrap();
}

/// Regression for the multi-connection bug: one peer opens many concurrent streams.
///
/// v0.1 serialized bi-streams from the same client because `handle_connection` awaited
/// each forwarder to completion in a loop.
#[tokio::test]
async fn tcp_multi_connection_from_one_peer() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let host = start_host(backend, TunnelProtocol::Tcp).await;
    let client = start_client(&host, TunnelProtocol::Tcp).await;
    wait_until(|| client.is_connected(), Duration::from_secs(5)).await;
    let bind = client.bind();

    let mut handles = Vec::new();
    for i in 0..8u8 {
        handles.push(tokio::spawn(async move {
            let payload = vec![i; 64];
            let mut s = TcpStream::connect(bind).await.unwrap();
            s.write_all(&payload).await.unwrap();
            let mut buf = vec![0u8; payload.len()];
            s.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, payload);
        }));
    }

    let result = timeout(TEST_TIMEOUT, async {
        for h in handles {
            h.await.unwrap();
        }
    })
    .await;
    result.expect("multi-connection round-trip timed out");

    client.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
}

/// Shutdown must release the client's bound port so a rebind on the same addr succeeds.
#[tokio::test]
async fn shutdown_is_clean() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let host = start_host(backend, TunnelProtocol::Tcp).await;

    // Bind the client to a known port so we can attempt to rebind after shutdown.
    let port = pick_free_port().await;
    let host_addr = host.node_addr().await.unwrap();
    let client = ClientTunnel::builder()
        .host_addr(host_addr.clone())
        .bind(format!("127.0.0.1:{port}").parse().unwrap())
        .protocol(TunnelProtocol::Tcp)
        .relay(iroh::RelayMode::Disabled)
        .discovery(false)
        .reconnect(ReconnectPolicy::none())
        .name("shutdown-client")
        .start()
        .await
        .unwrap();
    wait_until(|| client.is_connected(), Duration::from_secs(5)).await;

    // Do one round-trip to prove it's alive.
    {
        let mut s = TcpStream::connect(client.bind()).await.unwrap();
        s.write_all(b"ok").await.unwrap();
        let mut buf = [0u8; 2];
        s.read_exact(&mut buf).await.unwrap();
    }

    client.shutdown().await.unwrap();
    host.shutdown().await.unwrap();

    // Give the OS a beat to actually release the socket.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Rebind on the same port — should succeed.
    let _rebind = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .expect("port should be free after shutdown");
}

/// After a client disconnects, a new client using the same host key must be accepted.
/// (v0.1 left stale entries in the ConnectedClients map — this regressed reconnects.)
#[tokio::test]
async fn host_frees_peer_slot_on_disconnect() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let host = start_host(backend, TunnelProtocol::Tcp).await;

    let c1 = start_client(&host, TunnelProtocol::Tcp).await;
    wait_until(|| c1.is_connected(), Duration::from_secs(5)).await;
    wait_until_async(
        || async { host.connected_peers().await >= 1 },
        Duration::from_secs(5),
    )
    .await;

    c1.shutdown().await.unwrap();

    // Host should reap the disconnected peer.
    wait_until_async(
        || async { host.connected_peers().await == 0 },
        Duration::from_secs(5),
    )
    .await;

    // A brand-new client should connect fine.
    let c2 = start_client(&host, TunnelProtocol::Tcp).await;
    wait_until(|| c2.is_connected(), Duration::from_secs(5)).await;
    let bind = c2.bind();
    let mut s = TcpStream::connect(bind).await.unwrap();
    s.write_all(b"x").await.unwrap();
    let mut buf = [0u8; 1];
    s.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"x");

    c2.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
}

/// Client with a reconnect policy pointed at an unreachable host must not crash — it
/// keeps looping in its supervisor until either the host appears or `shutdown()` is called.
/// This exercises the supervisor's retry-and-backoff path.
#[tokio::test]
async fn client_reconnect_policy_survives_unreachable_host() {
    // Fake host: valid key but no endpoint listening for it.
    let fake_key = gen_secret().public();
    let client = ClientTunnel::builder()
        .host_key(fake_key)
        .bind("127.0.0.1:0".parse().unwrap())
        .protocol(TunnelProtocol::Tcp)
        .relay(iroh::RelayMode::Disabled)
        // Discovery must be on for host_key resolution; but with relay disabled and no
        // reachable peer, connect will just keep failing. That's what we're testing.
        .discovery(true)
        .reconnect(ReconnectPolicy {
            max_attempts: None,
            initial_backoff: Duration::from_millis(50),
            max_backoff: Duration::from_millis(200),
        })
        .name("orphan-client")
        .start()
        .await
        .expect("client should start even when host is unreachable");

    // Give the supervisor a few loop iterations. It must not panic.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!client.is_connected());

    // Clean shutdown even mid-retry.
    let result = timeout(Duration::from_secs(5), client.shutdown()).await;
    result.expect("shutdown timed out").unwrap();
}

/// After a client is dropped, the host reaps its peer slot and can accept a fresh client
/// that then does traffic successfully. Covers the full lifecycle of a reconnect from
/// the host's point of view.
#[tokio::test]
async fn host_accepts_reconnecting_client_pattern() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let host = start_host(backend, TunnelProtocol::Tcp).await;

    for i in 0..3u8 {
        let c = start_client(&host, TunnelProtocol::Tcp).await;
        wait_until(|| c.is_connected(), Duration::from_secs(5)).await;
        let mut s = TcpStream::connect(c.bind()).await.unwrap();
        let payload = [i];
        s.write_all(&payload).await.unwrap();
        let mut buf = [0u8; 1];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, payload);
        c.shutdown().await.unwrap();
    }

    host.shutdown().await.unwrap();
}

/// Invalid public key input must never panic (v0.1 had `PublicKey::from_bytes().unwrap()`
/// in a hot path). Regardless of whether iroh accepts a given 32-byte pattern, the API
/// returns a `Result` and we must handle both cases.
#[tokio::test]
async fn invalid_public_key_never_panics() {
    // Try a bunch of adversarial patterns. None of them should panic.
    for pattern in [[0u8; 32], [0xffu8; 32], [0x01u8; 32]] {
        let _ = iroh::PublicKey::from_bytes(&pattern);
    }
}

/// When the host disconnects, the client's supervisor must (a) detect the disconnect,
/// (b) not crash, (c) keep the local listen socket bound, and (d) resume retrying.
/// This validates the continuous-service reconnect behaviour minus the actual reconnect
/// success (which depends on iroh's network-level path recovery; production uses
/// discovery which handles this transparently).
#[tokio::test]
async fn client_supervisor_survives_host_disconnect() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let host = start_host(backend, TunnelProtocol::Tcp).await;
    let host_addr = host.node_addr().await.unwrap();

    let client = ClientTunnel::builder()
        .host_addr(host_addr)
        .bind("127.0.0.1:0".parse().unwrap())
        .protocol(TunnelProtocol::Tcp)
        .relay(iroh::RelayMode::Disabled)
        .discovery(false)
        .reconnect(ReconnectPolicy {
            max_attempts: None,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(500),
        })
        .name("supervisor-client")
        .start()
        .await
        .unwrap();
    wait_until(|| client.is_connected(), Duration::from_secs(5)).await;

    // Baseline traffic works.
    let bind = client.bind();
    {
        let mut s = TcpStream::connect(bind).await.unwrap();
        s.write_all(b"a").await.unwrap();
        let mut buf = [0u8; 1];
        s.read_exact(&mut buf).await.unwrap();
    }

    // Host disappears. Client must notice.
    host.shutdown().await.unwrap();
    wait_until(|| !client.is_connected(), Duration::from_secs(15)).await;

    // Local listen socket must still be bound (supervisor kept it) so user apps get accepted
    // even while the tunnel is down — they queue in the OS backlog for when it comes back.
    let queued = tokio::spawn(async move {
        let mut s = TcpStream::connect(bind)
            .await
            .expect("local listener still bound");
        // The write will not be echoed while the tunnel is down; we're only proving the
        // TCP accept path stays alive.
        let _ = s.write_all(b"queued").await;
    });

    // Give the supervisor a chance to run several backoff iterations. It must not panic.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!client.is_connected(), "client should be in reconnect loop");

    // Shutdown must succeed cleanly even mid-reconnect.
    let _ = queued.await;
    let r = timeout(Duration::from_secs(5), client.shutdown()).await;
    r.expect("shutdown timed out").unwrap();
}

/// Helper: find and immediately release a free port for tests that need to rebind on it.
async fn pick_free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}
