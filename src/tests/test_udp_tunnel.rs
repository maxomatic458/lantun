use crate::{client::ClientTunnel, common::Protocol, host::HostTunnel};
use iroh::SecretKey;
use rand::Rng;
use std::net::SocketAddr;
use tokio::{
    net::UdpSocket,
    time::{Duration, timeout},
};

/// Helper to create a test UDP echo server
async fn start_echo_server(addr: SocketAddr) -> tokio::task::JoinHandle<()> {
    let socket = UdpSocket::bind(addr).await.unwrap();

    tokio::spawn(async move {
        let mut buf = vec![0u8; 65536];
        loop {
            match socket.recv_from(&mut buf).await {
                Ok((n, peer_addr)) => {
                    if socket.send_to(&buf[..n], peer_addr).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    })
}

#[tokio::test]
async fn test_tunnel_creation() {
    let echo_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let socket = UdpSocket::bind(echo_addr).await.unwrap();
    let echo_addr = socket.local_addr().unwrap();
    drop(socket);

    let secret = SecretKey::generate(&mut rand::rng());

    let host_tunnel = HostTunnel::new(
        Some("test-host".to_string()),
        secret,
        echo_addr,
        Protocol::Udp,
    );

    // Start tunnel and connect to iroh network
    let host_tunnel = host_tunnel
        .start()
        .await
        .expect("Failed to start host tunnel");
    let _stopped_tunnel = host_tunnel.stop().await;
}

/// Test simple data exchange
#[tokio::test]
async fn test_data_exchange() {
    // Start a local echo server
    let echo_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let socket = UdpSocket::bind(echo_addr).await.unwrap();
    let echo_addr = socket.local_addr().unwrap();
    drop(socket);

    let _echo_server = start_echo_server(echo_addr).await;

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create and start host tunnel
    let secret = SecretKey::generate(&mut rand::rng());
    let public_key = secret.public();

    let host_tunnel = HostTunnel::new(
        Some("test-host".to_string()),
        secret,
        echo_addr,
        Protocol::Udp,
    );

    let mut host_tunnel = host_tunnel
        .start()
        .await
        .expect("Failed to start host tunnel");

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Create and start client tunnel
    let client_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let socket = UdpSocket::bind(client_addr).await.unwrap();
    let client_addr = socket.local_addr().unwrap();
    drop(socket);

    let client_tunnel = ClientTunnel::new(
        Some("test-client".to_string()),
        public_key,
        client_addr,
        Protocol::Udp,
    );

    let client_tunnel = client_tunnel
        .start()
        .await
        .expect("Failed to start client tunnel");

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Create UDP socket to send to client tunnel's internal socket
    let send_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Send data over tunnel
    let test_data = rand::rng()
        .sample_iter::<u8, _>(rand::distr::StandardUniform)
        .take(1024)
        .collect::<Vec<u8>>();

    send_socket
        .send_to(&test_data, client_addr)
        .await
        .expect("Failed to send data");

    // Read echo data
    let mut buf = vec![0u8; test_data.len()];
    let read_result = timeout(Duration::from_secs(5), send_socket.recv_from(&mut buf))
        .await
        .expect("Timeout reading response");

    let (n, _) = read_result.expect("Failed to read response");

    assert_eq!(n, test_data.len());
    assert_eq!(buf, test_data, "Echoed data doesn't match sent data");
    assert_eq!(host_tunnel.num_connections().await, 1);

    let _stopped_client = client_tunnel.stop().await;

    // Ensure client is disconnected
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(host_tunnel.num_connections().await, 0);

    let _stopped_host = host_tunnel.stop().await;
}

/// Test multiple concurrent connections through the tunnel
#[tokio::test]
async fn test_multiple_concurrent_clients() {
    let echo_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let socket = UdpSocket::bind(echo_addr).await.unwrap();
    let echo_addr = socket.local_addr().unwrap();
    drop(socket);

    let _echo_server = start_echo_server(echo_addr).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create and start host tunnel
    let secret = SecretKey::generate(&mut rand::rng());
    let public_key = secret.public();

    let host_tunnel = HostTunnel::new(
        Some("test-host-multi".to_string()),
        secret,
        echo_addr,
        Protocol::Udp,
    );

    let host_tunnel = host_tunnel
        .start()
        .await
        .expect("Failed to start host tunnel");

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Create and start client tunnel
    let client_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let socket = UdpSocket::bind(client_addr).await.unwrap();
    let client_addr = socket.local_addr().unwrap();
    drop(socket);

    let client_tunnel = ClientTunnel::new(
        Some("test-client-multi".to_string()),
        public_key,
        client_addr,
        Protocol::Udp,
    );

    let client_tunnel = client_tunnel
        .start()
        .await
        .expect("Failed to start client tunnel");

    tokio::time::sleep(Duration::from_millis(5000)).await;

    const NUM_CLIENTS: usize = 30;
    let mut handles = Vec::new();

    // Spawn multiple concurrent clients
    for i in 0..NUM_CLIENTS {
        let client_addr = client_addr.clone();
        let handle = tokio::spawn(async move {
            let socket = UdpSocket::bind("127.0.0.1:0")
                .await
                .expect(&format!("Client {}: Failed to bind socket", i));

            // Each client sends unique data
            let test_data = format!("Hello from client {}", i);
            socket
                .send_to(test_data.as_bytes(), client_addr)
                .await
                .expect(&format!("Client {}: Failed to send", i));

            // Read echoed data back
            let mut buf = vec![0u8; test_data.len()];
            let read_result = timeout(Duration::from_secs(5), socket.recv_from(&mut buf))
                .await
                .expect(&format!("Client {}: Timeout reading", i));

            let (n, _) = read_result.expect(&format!("Client {}: Failed to read", i));

            assert_eq!(
                n,
                test_data.len(),
                "Client {}: Received wrong amount of data",
                i
            );
            assert_eq!(
                String::from_utf8_lossy(&buf),
                test_data,
                "Client {}: Echoed data doesn't match",
                i
            );
        });

        handles.push(handle);
    }

    for (i, handle) in handles.into_iter().enumerate() {
        handle.await.expect(&format!("Client {} task panicked", i));
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let _stopped_client = client_tunnel.stop().await;
    let _stopped_host = host_tunnel.stop().await;
}

/// Test bidirectional data flow with larger payloads
#[tokio::test]
async fn test_large_data_transfer() {
    // Start a local echo server
    let echo_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let socket = UdpSocket::bind(echo_addr).await.unwrap();
    let echo_addr = socket.local_addr().unwrap();
    drop(socket);

    let _echo_server = start_echo_server(echo_addr).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create and start host tunnel
    let secret = SecretKey::generate(&mut rand::rng());
    let public_key = secret.public();

    let host_tunnel = HostTunnel::new(
        Some("test-host-large".to_string()),
        secret,
        echo_addr,
        Protocol::Udp,
    );

    let host_tunnel = host_tunnel
        .start()
        .await
        .expect("Failed to start host tunnel");

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Create and start client tunnel
    let client_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let socket = UdpSocket::bind(client_addr).await.unwrap();
    let client_addr = socket.local_addr().unwrap();
    drop(socket);

    let client_tunnel = ClientTunnel::new(
        Some("test-client-large".to_string()),
        public_key,
        client_addr,
        Protocol::Udp,
    );

    let client_tunnel = client_tunnel
        .start()
        .await
        .expect("Failed to start client tunnel");

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Create UDP socket
    let send_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Send large payload (9KB - safe size that works reliably on all systems)
    // for some reason packets above ~10KB are dropped even locally (linux)
    // TODO: figure out if this needs to be fixed
    let test_data: Vec<u8> = (0..9_000).map(|i| (i % 256) as u8).collect();
    send_socket
        .send_to(&test_data, client_addr)
        .await
        .expect("Failed to send large data");

    // Read echoed data back
    let mut buf = vec![0u8; test_data.len()];
    let read_result = timeout(Duration::from_secs(10), send_socket.recv_from(&mut buf))
        .await
        .expect("Timeout reading large response");

    let (n, _) = read_result.expect("Failed to read large response");

    assert_eq!(n, test_data.len(), "Received wrong amount of data");
    assert_eq!(buf, test_data, "Large echoed data doesn't match sent data");

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(host_tunnel.num_connections().await, 1);

    let _stopped_client = client_tunnel.stop().await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(host_tunnel.num_connections().await, 0);

    let _stopped_host = host_tunnel.stop().await;
}

/// Test sequential data exchanges on the same socket
#[tokio::test]
async fn test_sequential_exchanges() {
    // Start a local echo server
    let echo_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let socket = UdpSocket::bind(echo_addr).await.unwrap();
    let echo_addr = socket.local_addr().unwrap();
    drop(socket);

    let _echo_server = start_echo_server(echo_addr).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create and start host tunnel
    let secret = SecretKey::generate(&mut rand::rng());
    let public_key = secret.public();

    let host_tunnel = HostTunnel::new(
        Some("test-host-seq".to_string()),
        secret,
        echo_addr,
        Protocol::Udp,
    );

    let host_tunnel = host_tunnel
        .start()
        .await
        .expect("Failed to start host tunnel");

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Create and start client tunnel
    let client_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let socket = UdpSocket::bind(client_addr).await.unwrap();
    let client_addr = socket.local_addr().unwrap();
    drop(socket);

    let client_tunnel = ClientTunnel::new(
        Some("test-client-seq".to_string()),
        public_key,
        client_addr,
        Protocol::Udp,
    );

    let client_tunnel = client_tunnel
        .start()
        .await
        .expect("Failed to start client tunnel");

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Create UDP socket for sending
    let send_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Perform multiple sequential exchanges on the same socket
    for i in 0..10 {
        let test_data = format!("Message number {}", i);

        // Send
        send_socket
            .send_to(test_data.as_bytes(), client_addr)
            .await
            .expect(&format!("Failed to send message {}", i));

        // Read
        let mut buf = vec![0u8; test_data.len()];
        let read_result = timeout(Duration::from_secs(5), send_socket.recv_from(&mut buf))
            .await
            .expect(&format!("Timeout reading message {}", i));

        let (n, _) = read_result.expect(&format!("Failed to read message {}", i));

        assert_eq!(
            n,
            test_data.len(),
            "Message {}: received wrong amount of data",
            i
        );
        assert_eq!(
            String::from_utf8_lossy(&buf),
            test_data,
            "Message {} doesn't match",
            i
        );
    }

    assert_eq!(host_tunnel.num_connections().await, 1);
    let _stopped_client = client_tunnel.stop().await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(host_tunnel.num_connections().await, 0);

    let _stopped_host = host_tunnel.stop().await;
}

/// Test multiple different UDP tunnels running in parallel, each with multiple concurrent connections
#[tokio::test]
async fn test_multiple_tunnels_with_concurrent_connections() {
    const NUM_TUNNELS: usize = 10;
    const CONNECTIONS_PER_TUNNEL: usize = 10;

    let mut tunnel_handles = Vec::new();

    // Create multiple tunnels in parallel
    for tunnel_id in 0..NUM_TUNNELS {
        let handle = tokio::spawn(async move {
            // Start a local echo server for this tunnel
            let echo_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
            let socket = UdpSocket::bind(echo_addr).await.unwrap();
            let echo_addr = socket.local_addr().unwrap();
            drop(socket);

            let _echo_server = start_echo_server(echo_addr).await;

            tokio::time::sleep(Duration::from_millis(100)).await;

            // Create and start host tunnel
            let secret = SecretKey::generate(&mut rand::rng());
            let public_key = secret.public();

            let host_tunnel = HostTunnel::new(
                Some(format!("test-host-{}", tunnel_id)),
                secret,
                echo_addr,
                Protocol::Udp,
            );

            let host_tunnel = host_tunnel
                .start()
                .await
                .expect(&format!("Failed to start host tunnel {}", tunnel_id));

            tokio::time::sleep(Duration::from_millis(200)).await;

            // Create and start client tunnel
            let client_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
            let socket = UdpSocket::bind(client_addr).await.unwrap();
            let client_addr = socket.local_addr().unwrap();
            drop(socket);

            let client_tunnel = ClientTunnel::new(
                Some(format!("test-client-{}", tunnel_id)),
                public_key,
                client_addr,
                Protocol::Udp,
            );

            let client_tunnel = client_tunnel
                .start()
                .await
                .expect(&format!("Failed to start client tunnel {}", tunnel_id));

            tokio::time::sleep(Duration::from_millis(500)).await;

            // Spawn multiple concurrent connections for this tunnel
            let mut connection_handles = Vec::new();

            for conn_id in 0..CONNECTIONS_PER_TUNNEL {
                let client_addr = client_addr.clone();
                let handle = tokio::spawn(async move {
                    let socket = UdpSocket::bind("127.0.0.1:0").await.expect(&format!(
                        "Tunnel {}, Connection {}: Failed to bind socket",
                        tunnel_id, conn_id
                    ));

                    let test_data =
                        format!("Tunnel {} - Connection {} - Test Data", tunnel_id, conn_id);
                    socket
                        .send_to(test_data.as_bytes(), client_addr)
                        .await
                        .expect(&format!(
                            "Tunnel {}, Connection {}: Failed to send",
                            tunnel_id, conn_id
                        ));

                    // Read echoed data back
                    let mut buf = vec![0u8; test_data.len()];
                    let read_result = timeout(Duration::from_secs(5), socket.recv_from(&mut buf))
                        .await
                        .expect(&format!(
                            "Tunnel {}, Connection {}: Timeout reading",
                            tunnel_id, conn_id
                        ));

                    let (n, _) = read_result.expect(&format!(
                        "Tunnel {}, Connection {}: Failed to read",
                        tunnel_id, conn_id
                    ));

                    assert_eq!(
                        n,
                        test_data.len(),
                        "Tunnel {}, Connection {}: received wrong amount of data",
                        tunnel_id,
                        conn_id
                    );
                    assert_eq!(
                        String::from_utf8_lossy(&buf),
                        test_data,
                        "Tunnel {}, Connection {}: Echoed data doesn't match",
                        tunnel_id,
                        conn_id
                    );
                });

                connection_handles.push(handle);
            }

            // Wait for all connections in this tunnel to complete
            for (conn_id, handle) in connection_handles.into_iter().enumerate() {
                handle.await.expect(&format!(
                    "Tunnel {}, Connection {} task panicked",
                    tunnel_id, conn_id
                ));
            }

            // Cleanup this tunnel
            let _stopped_client = client_tunnel.stop().await;
            let _stopped_host = host_tunnel.stop().await;
        });

        tunnel_handles.push(handle);
    }

    // Wait for all tunnels to complete
    for (tunnel_id, handle) in tunnel_handles.into_iter().enumerate() {
        handle
            .await
            .expect(&format!("Tunnel {} task panicked", tunnel_id));
    }
}
