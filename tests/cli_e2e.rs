//! Subprocess-based end-to-end tests. Each test spins up the real compiled `lantun`
//! binary as host and client processes, config-driven, using real n0 discovery + relay
//! (which on the same machine resolve to loopback direct addresses, keeping traffic off
//! the network for real). Sends real bytes through the tunnel, asserts they come back.
//!
//! Requires network access for the initial discovery lookup.

#![cfg(unix)]

mod cli_common;

use std::{net::SocketAddr, time::Duration};

use cli_common::{
    ClientSpec, HostSpec, LANTUN_BIN, OP_TIMEOUT, pick_free_port_tcp, pick_free_port_udp,
    spawn_client, spawn_host, spawn_tcp_echo, spawn_udp_echo, wait_tcp_ready, wait_udp_ready,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    time::timeout,
};

// ---------------------------- Basic round-trips ----------------------------

#[tokio::test]
async fn tcp_basic_roundtrip() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let (host, keys) = spawn_host("basic-tcp", vec![HostSpec::tcp("h", backend)]).await;

    let bind: SocketAddr = format!("127.0.0.1:{}", pick_free_port_tcp().await)
        .parse()
        .unwrap();
    let client = spawn_client("basic-tcp", vec![ClientSpec::tcp("c", bind, &keys[0])]).await;

    wait_tcp_ready(bind).await;

    // Two payloads, two connections, both should echo.
    for msg in [b"hello".as_slice(), b"world!".as_slice()] {
        let result = timeout(OP_TIMEOUT, async {
            let mut s = TcpStream::connect(bind).await.unwrap();
            s.write_all(msg).await.unwrap();
            let mut buf = vec![0u8; msg.len()];
            s.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, msg);
        })
        .await;
        result.expect("tcp round-trip timed out");
    }

    client.kill_and_wait().await;
    host.kill_and_wait().await;
}

#[tokio::test]
async fn udp_basic_roundtrip() {
    let (backend, _echo) = spawn_udp_echo().await;
    let (host, keys) = spawn_host("basic-udp", vec![HostSpec::udp("h", backend)]).await;

    let bind: SocketAddr = format!("127.0.0.1:{}", pick_free_port_udp().await)
        .parse()
        .unwrap();
    let client = spawn_client("basic-udp", vec![ClientSpec::udp("c", bind, &keys[0])]).await;

    wait_udp_ready(bind).await;

    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.connect(bind).await.unwrap();
    for msg in [b"one".as_slice(), b"two".as_slice()] {
        let result = timeout(OP_TIMEOUT, async {
            sock.send(msg).await.unwrap();
            let mut buf = [0u8; 16];
            let n = sock.recv(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], msg);
        })
        .await;
        result.expect("udp round-trip timed out");
    }

    client.kill_and_wait().await;
    host.kill_and_wait().await;
}

// ---------------------------- Concurrency ----------------------------

/// Regression for the v0.1 concurrency bug, but exercised through the real binary.
#[tokio::test]
async fn tcp_multi_concurrent_connections() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let (host, keys) = spawn_host("multi-tcp", vec![HostSpec::tcp("h", backend)]).await;

    let bind: SocketAddr = format!("127.0.0.1:{}", pick_free_port_tcp().await)
        .parse()
        .unwrap();
    let client = spawn_client("multi-tcp", vec![ClientSpec::tcp("c", bind, &keys[0])]).await;
    wait_tcp_ready(bind).await;

    let mut handles = Vec::new();
    for i in 0..16u8 {
        handles.push(tokio::spawn(async move {
            let payload = vec![i; 128];
            let mut s = TcpStream::connect(bind).await.unwrap();
            s.write_all(&payload).await.unwrap();
            let mut buf = vec![0u8; payload.len()];
            s.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, payload);
        }));
    }
    timeout(OP_TIMEOUT, async {
        for h in handles {
            h.await.unwrap();
        }
    })
    .await
    .expect("concurrent connections timed out");

    client.kill_and_wait().await;
    host.kill_and_wait().await;
}

/// Multiple different UDP source sockets sending through the same client tunnel.
/// Exercises the client-side per-source dispatch (mpsc → bi_udp_client_session).
#[tokio::test]
async fn udp_multi_source() {
    let (backend, _echo) = spawn_udp_echo().await;
    let (host, keys) = spawn_host("multi-udp", vec![HostSpec::udp("h", backend)]).await;

    let bind: SocketAddr = format!("127.0.0.1:{}", pick_free_port_udp().await)
        .parse()
        .unwrap();
    let client = spawn_client("multi-udp", vec![ClientSpec::udp("c", bind, &keys[0])]).await;
    wait_udp_ready(bind).await;

    let mut handles = Vec::new();
    for i in 0..5u8 {
        handles.push(tokio::spawn(async move {
            let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            sock.connect(bind).await.unwrap();
            let payload = vec![i; 32];
            // UDP is best-effort — retry a couple times if a dgram gets dropped.
            for _ in 0..3 {
                sock.send(&payload).await.unwrap();
                let mut buf = [0u8; 64];
                match timeout(Duration::from_secs(2), sock.recv(&mut buf)).await {
                    Ok(Ok(n)) if n == payload.len() && &buf[..n] == payload.as_slice() => return,
                    _ => continue,
                }
            }
            panic!("source {i} never got an echo back");
        }));
    }
    timeout(OP_TIMEOUT, async {
        for h in handles {
            h.await.unwrap();
        }
    })
    .await
    .expect("multi-source udp timed out");

    client.kill_and_wait().await;
    host.kill_and_wait().await;
}

// ---------------------------- Payload edge cases ----------------------------

/// 1 MB over TCP tunnel — validates framing / buffering with data larger than one MTU
/// worth of QUIC data.
#[tokio::test]
async fn tcp_large_payload() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let (host, keys) = spawn_host("large-tcp", vec![HostSpec::tcp("h", backend)]).await;

    let bind: SocketAddr = format!("127.0.0.1:{}", pick_free_port_tcp().await)
        .parse()
        .unwrap();
    let client = spawn_client("large-tcp", vec![ClientSpec::tcp("c", bind, &keys[0])]).await;
    wait_tcp_ready(bind).await;

    let payload: Vec<u8> = (0..1_000_000).map(|i| (i % 251) as u8).collect();
    let payload_clone = payload.clone();
    timeout(Duration::from_secs(30), async move {
        let mut s = TcpStream::connect(bind).await.unwrap();
        let (mut r, mut w) = s.split();
        let write_task = async {
            w.write_all(&payload_clone).await.unwrap();
            w.shutdown().await.unwrap();
        };
        let mut buf = vec![0u8; payload.len()];
        let read_task = async {
            r.read_exact(&mut buf).await.unwrap();
            buf
        };
        let (_, got) = tokio::join!(write_task, read_task);
        assert_eq!(got, payload);
    })
    .await
    .expect("1MB round-trip timed out");

    client.kill_and_wait().await;
    host.kill_and_wait().await;
}

/// Binary payload covering all 256 byte values, repeated. Catches any transparency /
/// framing bug that would corrupt specific bytes.
#[tokio::test]
async fn tcp_binary_data_transparency() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let (host, keys) = spawn_host("bin-tcp", vec![HostSpec::tcp("h", backend)]).await;

    let bind: SocketAddr = format!("127.0.0.1:{}", pick_free_port_tcp().await)
        .parse()
        .unwrap();
    let client = spawn_client("bin-tcp", vec![ClientSpec::tcp("c", bind, &keys[0])]).await;
    wait_tcp_ready(bind).await;

    // 4 copies of 0..255, mixed with some pathological bytes at the front.
    let mut payload: Vec<u8> = Vec::with_capacity(1024);
    payload.extend_from_slice(&[0, 0xff, 0, 0xff]);
    for _ in 0..4 {
        payload.extend((0..=255u8).collect::<Vec<u8>>());
    }
    let payload_clone = payload.clone();
    timeout(OP_TIMEOUT, async move {
        let mut s = TcpStream::connect(bind).await.unwrap();
        let (mut r, mut w) = s.split();
        let write_task = async {
            w.write_all(&payload_clone).await.unwrap();
            w.shutdown().await.unwrap();
        };
        let mut buf = vec![0u8; payload.len()];
        let read_task = async {
            r.read_exact(&mut buf).await.unwrap();
            buf
        };
        let (_, got) = tokio::join!(write_task, read_task);
        assert_eq!(got, payload);
    })
    .await
    .expect("binary transparency test timed out");

    client.kill_and_wait().await;
    host.kill_and_wait().await;
}

/// Rapid open/close cycles — each connection is short-lived, exercising the
/// per-bi-stream spawn/teardown path many times in quick succession.
#[tokio::test]
async fn tcp_rapid_open_close_cycles() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let (host, keys) = spawn_host("rapid-tcp", vec![HostSpec::tcp("h", backend)]).await;

    let bind: SocketAddr = format!("127.0.0.1:{}", pick_free_port_tcp().await)
        .parse()
        .unwrap();
    let client = spawn_client("rapid-tcp", vec![ClientSpec::tcp("c", bind, &keys[0])]).await;
    wait_tcp_ready(bind).await;

    timeout(Duration::from_secs(30), async {
        for i in 0..40u8 {
            let mut s = TcpStream::connect(bind).await.unwrap();
            let msg = [i];
            s.write_all(&msg).await.unwrap();
            let mut buf = [0u8; 1];
            s.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, msg);
            drop(s);
        }
    })
    .await
    .expect("rapid open/close timed out");

    client.kill_and_wait().await;
    host.kill_and_wait().await;
}

// ---------------------------- Restart / reconnect ----------------------------

/// Kill and restart the client subprocess. New client should connect and traffic resume.
/// (Mirrors "user restarted lantun on their laptop".)
#[tokio::test]
async fn client_process_restart_resumes_traffic() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let (host, keys) = spawn_host("cli-restart-h", vec![HostSpec::tcp("h", backend)]).await;

    let port = pick_free_port_tcp().await;
    let bind: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    let client1 = spawn_client("cli-restart-c1", vec![ClientSpec::tcp("c", bind, &keys[0])]).await;
    wait_tcp_ready(bind).await;
    {
        let mut s = TcpStream::connect(bind).await.unwrap();
        s.write_all(b"before").await.unwrap();
        let mut buf = [0u8; 6];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"before");
    }
    client1.kill_and_wait().await;

    // Small pause to let the OS actually release the local port.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let client2 = spawn_client("cli-restart-c2", vec![ClientSpec::tcp("c", bind, &keys[0])]).await;
    wait_tcp_ready(bind).await;
    {
        let mut s = TcpStream::connect(bind).await.unwrap();
        s.write_all(b"after!").await.unwrap();
        let mut buf = [0u8; 6];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"after!");
    }

    client2.kill_and_wait().await;
    host.kill_and_wait().await;
}

/// Mirror of `host_survives_many_client_restarts`: ONE long-running client process,
/// host cycled multiple times on the same iroh identity. Exercises the client's
/// auto-reconnect supervisor end-to-end through the CLI.
#[tokio::test]
async fn client_survives_many_host_restarts() {
    use lantun::gen_secret;

    let (backend, _echo) = spawn_tcp_echo().await;

    // Fixed secret so every host process presents the same iroh identity.
    let host_secret = gen_secret();
    let host_public = hex::encode(host_secret.public());

    let mut current_host = spawn_host(
        "restart-h0",
        vec![HostSpec::tcp_with_secret("h", backend, host_secret.clone())],
    )
    .await
    .0;

    let bind: SocketAddr = format!("127.0.0.1:{}", pick_free_port_tcp().await)
        .parse()
        .unwrap();
    let client = spawn_client("restart-c", vec![ClientSpec::tcp("c", bind, &host_public)]).await;
    wait_tcp_ready(bind).await;

    // Baseline round-trip before any restart.
    {
        let mut s = TcpStream::connect(bind).await.unwrap();
        s.write_all(b"boot").await.unwrap();
        let mut buf = [0u8; 4];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"boot");
    }

    for i in 0..3u8 {
        // Graceful shutdown so the client's `conn.closed()` fires immediately (SIGKILL
        // would work too but the client would wait for QUIC keepalive — ~30s — before
        // giving up on the dead connection, making the test unnecessarily slow).
        current_host.sigint_and_wait().await;

        // Give the OS a moment to release the UDP port.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Fresh host process on the same iroh identity.
        current_host = spawn_host(
            &format!("restart-h{}", i + 1),
            vec![HostSpec::tcp_with_secret("h", backend, host_secret.clone())],
        )
        .await
        .0;

        // The client's supervisor should reconnect once discovery re-resolves the host.
        // wait_tcp_ready polls with a real echo round-trip so this succeeds only when the
        // whole pipe is functional again.
        wait_tcp_ready(bind).await;

        // Extra explicit round-trip to prove it beyond the readiness probe.
        let mut s = TcpStream::connect(bind).await.unwrap();
        let payload = format!("cycle-{i}").into_bytes();
        s.write_all(&payload).await.unwrap();
        let mut buf = vec![0u8; payload.len()];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, payload);
    }

    client.kill_and_wait().await;
    current_host.kill_and_wait().await;
}

/// Host stays up while multiple client processes come and go — the host's peer map
/// must not leak entries or refuse new peers.
#[tokio::test]
async fn host_survives_many_client_restarts() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let (host, keys) = spawn_host("h-survive", vec![HostSpec::tcp("h", backend)]).await;

    for i in 0..3u8 {
        let bind: SocketAddr = format!("127.0.0.1:{}", pick_free_port_tcp().await)
            .parse()
            .unwrap();
        let client = spawn_client(
            &format!("survive-c{i}"),
            vec![ClientSpec::tcp("c", bind, &keys[0])],
        )
        .await;
        wait_tcp_ready(bind).await;
        let mut s = TcpStream::connect(bind).await.unwrap();
        let payload = [i, i, i];
        s.write_all(&payload).await.unwrap();
        let mut buf = [0u8; 3];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, payload);
        client.kill_and_wait().await;
    }

    host.kill_and_wait().await;
}

// ---------------------------- Config / multiplexing ----------------------------

/// One lantun process running two host tunnels for two different backends.
#[tokio::test]
async fn one_process_multiple_tunnels() {
    let (backend_a, _a) = spawn_tcp_echo().await;
    let (backend_b, _b) = spawn_tcp_echo().await;

    let (host, keys) = spawn_host(
        "multi-tunnel-host",
        vec![HostSpec::tcp("a", backend_a), HostSpec::tcp("b", backend_b)],
    )
    .await;

    let bind_a: SocketAddr = format!("127.0.0.1:{}", pick_free_port_tcp().await)
        .parse()
        .unwrap();
    let bind_b: SocketAddr = format!("127.0.0.1:{}", pick_free_port_tcp().await)
        .parse()
        .unwrap();
    let client = spawn_client(
        "multi-tunnel-client",
        vec![
            ClientSpec::tcp("a", bind_a, &keys[0]),
            ClientSpec::tcp("b", bind_b, &keys[1]),
        ],
    )
    .await;

    wait_tcp_ready(bind_a).await;
    wait_tcp_ready(bind_b).await;

    // Both should echo. If tunnels crossed wires we'd get the wrong echo... but since
    // both backends are echo servers, we can only detect crossed wires by adding a
    // per-backend header. Simplest for now: just verify each channel roundtrips.
    for bind in [bind_a, bind_b] {
        let mut s = TcpStream::connect(bind).await.unwrap();
        s.write_all(b"xyz").await.unwrap();
        let mut buf = [0u8; 3];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"xyz");
    }

    client.kill_and_wait().await;
    host.kill_and_wait().await;
}

/// `enabled = false` tunnels must not be started — their local port stays unbound.
#[tokio::test]
async fn disabled_tunnel_is_not_started() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let (host, keys) = spawn_host("dis-host", vec![HostSpec::tcp("h", backend)]).await;

    let enabled_bind: SocketAddr = format!("127.0.0.1:{}", pick_free_port_tcp().await)
        .parse()
        .unwrap();
    let disabled_bind: SocketAddr = format!("127.0.0.1:{}", pick_free_port_tcp().await)
        .parse()
        .unwrap();

    let mut disabled = ClientSpec::tcp("dis", disabled_bind, &keys[0]);
    disabled.enabled = false;

    let client = spawn_client(
        "dis-client",
        vec![ClientSpec::tcp("en", enabled_bind, &keys[0]), disabled],
    )
    .await;
    wait_tcp_ready(enabled_bind).await;

    // Enabled tunnel works.
    let mut s = TcpStream::connect(enabled_bind).await.unwrap();
    s.write_all(b"ok").await.unwrap();
    let mut buf = [0u8; 2];
    s.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"ok");
    drop(s);

    // Disabled tunnel's local port should NOT be bound — connect fails fast.
    let result = timeout(Duration::from_secs(2), TcpStream::connect(disabled_bind)).await;
    match result {
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => { /* expected */ }
        other => panic!("expected connection refused, got {other:?}"),
    }

    client.kill_and_wait().await;
    host.kill_and_wait().await;
}

// ---------------------------- Shutdown / error cases ----------------------------

/// SIGINT causes both host and client to exit cleanly with status 0.
#[tokio::test]
async fn sigint_causes_clean_exit() {
    let (backend, _echo) = spawn_tcp_echo().await;
    let (host, keys) = spawn_host("sigint-host", vec![HostSpec::tcp("h", backend)]).await;

    let bind: SocketAddr = format!("127.0.0.1:{}", pick_free_port_tcp().await)
        .parse()
        .unwrap();
    let client = spawn_client("sigint-client", vec![ClientSpec::tcp("c", bind, &keys[0])]).await;
    wait_tcp_ready(bind).await;

    let host_status = host.sigint_and_wait().await;
    let client_status = client.sigint_and_wait().await;
    assert!(
        host_status.success(),
        "host exited with {host_status:?} (expected 0)"
    );
    assert!(
        client_status.success(),
        "client exited with {client_status:?} (expected 0)"
    );
}

/// Malformed config → process exits with nonzero status quickly.
#[tokio::test]
async fn invalid_config_errors_out() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = dir.path().join("lantun.toml");
    std::fs::write(
        &cfg,
        r#"
[[host_tunnels]]
name = "bad"
local = "127.0.0.1:12345"
protocol = "tcp"
secret_key = "not_valid_hex"
public_key = "also_bad"
enabled = true
"#,
    )
    .unwrap();

    let output = tokio::process::Command::new(LANTUN_BIN)
        .arg("--config")
        .arg(&cfg)
        .env("RUST_LOG", "warn")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();

    let output = timeout(Duration::from_secs(5), output)
        .await
        .expect("invalid-config lantun should exit quickly")
        .expect("spawn");
    assert!(
        !output.status.success(),
        "lantun should reject invalid config, got status {:?}",
        output.status
    );
}

/// Backend is down when a client tries to use the tunnel. The client's TCP connect
/// SUCCEEDS (local listener is up) but the tunneled forward fails when the host tries
/// to connect to the backend. The stream should close cleanly, not hang forever.
#[tokio::test]
async fn backend_unreachable_closes_stream() {
    // Grab a port and immediately release it — nothing is listening there.
    let dead_backend: SocketAddr = format!("127.0.0.1:{}", pick_free_port_tcp().await)
        .parse()
        .unwrap();
    let (host, keys) = spawn_host("dead-host", vec![HostSpec::tcp("h", dead_backend)]).await;

    let bind: SocketAddr = format!("127.0.0.1:{}", pick_free_port_tcp().await)
        .parse()
        .unwrap();
    let client = spawn_client("dead-client", vec![ClientSpec::tcp("c", bind, &keys[0])]).await;

    // We can't use wait_tcp_ready — the tunnel end-to-end won't work since the backend
    // is dead. Just wait a bit for the tunnel to establish the QUIC connection.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Client's local port is bound and the QUIC session should be up, so this connect
    // succeeds locally. The stream then fails when the host tries to reach the dead
    // backend — our forwarder should close it promptly.
    let s = TcpStream::connect(bind).await;
    if let Ok(mut s) = s {
        let _ = s.write_all(b"hello").await;
        let mut buf = [0u8; 1];
        // Should error (broken pipe / eof) within a reasonable time — not hang.
        let r = timeout(Duration::from_secs(5), s.read(&mut buf)).await;
        match r {
            Ok(Ok(0)) => { /* clean EOF — expected */ }
            Ok(Err(_)) => { /* connection reset — also fine */ }
            Ok(Ok(_)) => panic!("got unexpected data from dead backend"),
            Err(_) => panic!("read hung — forwarder didn't close on backend failure"),
        }
    }
    // If the local connect itself failed, that's also acceptable behavior.

    client.kill_and_wait().await;
    host.kill_and_wait().await;
}
