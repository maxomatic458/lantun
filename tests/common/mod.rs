//! Test helpers: echo servers, tunnel builders that skip discovery/relay.

use std::{net::SocketAddr, time::Duration};

use iroh::RelayMode;
use lantun::{ClientTunnel, HostTunnel, ReconnectPolicy, SecretKey, TunnelProtocol, gen_secret};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UdpSocket},
    task::JoinHandle,
};

pub const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Spawn a TCP echo server on 127.0.0.1:0. Returns the bound address and the join handle.
/// The handle is left detached; test tearing down the tunnel is sufficient to reclaim it
/// (dropping the tunnel drops the last client connection; when the listener is dropped
/// after test end the accept loop exits).
pub async fn spawn_tcp_echo() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                loop {
                    let n = match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    if sock.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (addr, handle)
}

/// Spawn a UDP echo server on 127.0.0.1:0. Datagrams received are sent back to sender.
pub async fn spawn_udp_echo() -> (SocketAddr, JoinHandle<()>) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let mut buf = vec![0u8; 65_535];
        loop {
            let (n, src) = match socket.recv_from(&mut buf).await {
                Ok(x) => x,
                Err(_) => return,
            };
            let _ = socket.send_to(&buf[..n], src).await;
        }
    });
    (addr, handle)
}

/// Start a host tunnel bound to loopback that forwards to `backend`.
/// Discovery + relay disabled so tests don't touch the network.
pub async fn start_host(backend: SocketAddr, protocol: TunnelProtocol) -> HostTunnel {
    start_host_with_secret(gen_secret(), backend, protocol).await
}

pub async fn start_host_with_secret(
    secret: SecretKey,
    backend: SocketAddr,
    protocol: TunnelProtocol,
) -> HostTunnel {
    HostTunnel::builder()
        .secret(secret)
        .forward_to(backend)
        .protocol(protocol)
        .relay(RelayMode::Disabled)
        .discovery(false)
        .name("test-host")
        .start()
        .await
        .expect("host start")
}

/// Start a client tunnel connecting to `host` by explicit NodeAddr (skipping discovery).
/// Binds to `127.0.0.1:0` — retrieve the actual port via `client.bind()`.
pub async fn start_client(host: &HostTunnel, protocol: TunnelProtocol) -> ClientTunnel {
    let addr = host.node_addr().await.expect("host node_addr");
    ClientTunnel::builder()
        .host_addr(addr)
        .bind("127.0.0.1:0".parse().unwrap())
        .protocol(protocol)
        .relay(RelayMode::Disabled)
        .discovery(false)
        .reconnect(ReconnectPolicy::none())
        .name("test-client")
        .start()
        .await
        .expect("client start")
}

/// Wait until `f()` returns true, polling every 20ms. Panics after `deadline`.
pub async fn wait_until<F: FnMut() -> bool>(mut f: F, deadline: Duration) {
    let start = std::time::Instant::now();
    while !f() {
        if start.elapsed() > deadline {
            panic!("wait_until: condition not met within {deadline:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Async variant: awaits `f()` each iteration.
pub async fn wait_until_async<F, Fut>(mut f: F, deadline: Duration)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = std::time::Instant::now();
    loop {
        if f().await {
            return;
        }
        if start.elapsed() > deadline {
            panic!("wait_until_async: condition not met within {deadline:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
