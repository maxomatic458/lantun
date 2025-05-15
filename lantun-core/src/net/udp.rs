use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use iroh::endpoint::{RecvStream, SendStream};
use tokio::{
    net::UdpSocket,
    sync::{RwLock, watch},
};

use crate::{
    client::{ClientError, ClientTunnel},
    common::{CONNECTION_TIMEOUT_SECONDS, LocalClientConnection, TUNNEL_FORWARDER_BUFFER_SIZE},
    host::{ConnectedClient, HostError, HostTunnel},
    utils::now_secs,
};

pub async fn bi_udp_host(
    // The virtual client socket
    socket: UdpSocket,
    connection: Arc<ConnectedClient>,
    // The current connection of the client to the host.
    local_client_connection: Arc<RwLock<LocalClientConnection>>,
    mut tunnel_send: SendStream,
    mut tunnel_recv: RecvStream,
    mut stop_rx: watch::Receiver<bool>,
) -> Result<(), HostError> {
    if socket.peer_addr().is_err() {
        panic!("Socket is not connected");
    }

    tracing::debug!("Starting bidirectional UDP forwarder for the host tunnel");

    let socket_arc = Arc::new(socket);

    let udp_read = socket_arc.clone();
    let udp_write = socket_arc.clone();

    let stop_rx_c = stop_rx.clone();
    let last_active = local_client_connection.read().await.last_active.clone();
    let last_active_c = last_active.clone();

    last_active.store(now_secs(), Ordering::SeqCst);

    let tunnel_to_udp_forwader = tokio::spawn(async move {
        let mut buf = vec![0; TUNNEL_FORWARDER_BUFFER_SIZE];
        let mut stop_rx = stop_rx_c;
        let last_active = last_active_c;

        loop {
            let now = now_secs();
            let last = last_active.load(Ordering::SeqCst);
            let elapsed = now.saturating_sub(last);
            let timeout = if elapsed >= CONNECTION_TIMEOUT_SECONDS {
                Duration::from_secs(0)
            } else {
                Duration::from_secs(CONNECTION_TIMEOUT_SECONDS - elapsed)
            };

            tokio::select! {
                biased;
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() {
                        tracing::debug!("Stopping UDP stream for host tunnel");
                        break;
                    }
                }
                Ok(n) = tunnel_recv.read(&mut buf) => {
                    let Some(n) = n else {
                        tracing::debug!("Tunnel stream closed");
                        break;
                    };

                    if n == 0 {
                        tracing::debug!("Tunnel stream closed");
                        break;
                    }

                    last_active.store(now_secs(), Ordering::SeqCst);
                    udp_write.send(&buf[..n]).await.unwrap();
                }
                _ = tokio::time::sleep(timeout) => {
                    let now = now_secs();
                    let last = last_active.load(Ordering::SeqCst);
                    let elapsed = now.saturating_sub(last);

                    if elapsed >= CONNECTION_TIMEOUT_SECONDS {
                        tracing::debug!("UDP stream timed out");
                        break;
                    }
                }
            }
        }
    });

    let udp_to_tunnel_forwader = tokio::spawn(async move {
        let mut buf = vec![0; TUNNEL_FORWARDER_BUFFER_SIZE];
        loop {
            let now = now_secs();
            let last = last_active.load(Ordering::SeqCst);
            let elapsed = now.saturating_sub(last);
            let timeout = if elapsed >= CONNECTION_TIMEOUT_SECONDS {
                Duration::from_secs(0)
            } else {
                Duration::from_secs(CONNECTION_TIMEOUT_SECONDS - elapsed)
            };

            tokio::select! {
                biased;
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() {
                        tracing::debug!("Stopping UDP stream for host tunnel");
                        break;
                    }
                }
                Ok(n) = udp_read.recv(&mut buf) => {
                    if n == 0 {
                        tracing::debug!("Tunnel stream closed");
                        break;
                    }

                    last_active.store(now_secs(), Ordering::SeqCst);
                    tunnel_send.write_all(&buf[..n]).await.unwrap();
                }
                _ = tokio::time::sleep(timeout) => {
                    let now = now_secs();
                    let last = last_active.load(Ordering::SeqCst);
                    let elapsed = now.saturating_sub(last);

                    if elapsed >= CONNECTION_TIMEOUT_SECONDS {
                        tracing::debug!("UDP stream timed out");
                        break;
                    }
                }
            }
        }
    });

    tracing::info!("Started tunnel UDP forwarder for host tunnel");

    let _ = tokio::try_join!(tunnel_to_udp_forwader, udp_to_tunnel_forwader);

    HostTunnel::disconnect_local_client_connection(connection, local_client_connection).await;

    tracing::info!("Stopping tunnel UDP forwarder for host tunnel");

    Ok(())
}

pub async fn bi_udp_client(
    // The socket of the client application to the tunnel socket.
    socket: Arc<UdpSocket>,
    // All connections on this tunnel.
    local_connections: Arc<RwLock<HashMap<SocketAddr, Arc<RwLock<LocalClientConnection>>>>>,
    // The current connection of the client to the host (on this tunnel).
    local_client_connection: Arc<RwLock<LocalClientConnection>>,
    mut tunnel_send: SendStream,
    mut tunnel_recv: RecvStream,
    mut stop_rx: watch::Receiver<bool>,
) -> Result<(), ClientError> {
    tracing::debug!("Starting bidirectional UDP forwarder for the client tunnel");

    // The address of the client application connecting to the tunnel.
    let client_local_addr = { local_client_connection.read().await.client_local_addr };

    let udp_read = socket.clone();
    let udp_write = socket;

    let stop_rx_c = stop_rx.clone();
    let last_active = local_client_connection.read().await.last_active.clone();
    let last_active_c = last_active.clone();

    last_active.store(now_secs(), Ordering::SeqCst);

    let tunnel_to_udp_forwader = tokio::spawn(async move {
        let mut buf = vec![0; TUNNEL_FORWARDER_BUFFER_SIZE];
        let mut stop_rx = stop_rx_c;
        let last_active = last_active_c;

        loop {
            let now = now_secs();
            let last = last_active.load(Ordering::SeqCst);
            let elapsed = now.saturating_sub(last);
            let timeout = if elapsed >= CONNECTION_TIMEOUT_SECONDS {
                Duration::from_secs(0)
            } else {
                Duration::from_secs(CONNECTION_TIMEOUT_SECONDS - elapsed)
            };

            tokio::select! {
                biased;
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() {
                        tracing::debug!("Stopping UDP stream for host tunnel");
                        break;
                    }
                }
                Ok(n) = tunnel_recv.read(&mut buf) => {
                    let Some(n) = n else {
                        tracing::debug!("Tunnel stream closed");
                        break;
                    };

                    if n == 0 {
                        tracing::debug!("Tunnel stream closed");
                        break;
                    }

                    last_active.store(now_secs(), Ordering::SeqCst);
                    udp_write.send_to(&buf[..n], client_local_addr).await.unwrap();
                }
                _ = tokio::time::sleep(timeout) => {
                    let now = now_secs();
                    let last = last_active.load(Ordering::SeqCst);
                    let elapsed = now.saturating_sub(last);

                    if elapsed >= CONNECTION_TIMEOUT_SECONDS {
                        tracing::debug!("UDP stream timed out");
                        break;
                    }
                }
            }
        }
    });

    let udp_to_tunnel_forwader = tokio::spawn(async move {
        let mut buf = vec![0; TUNNEL_FORWARDER_BUFFER_SIZE];
        loop {
            let now = now_secs();
            let last = last_active.load(Ordering::SeqCst);
            let elapsed = now.saturating_sub(last);
            let timeout = if elapsed >= CONNECTION_TIMEOUT_SECONDS {
                Duration::from_secs(0)
            } else {
                Duration::from_secs(CONNECTION_TIMEOUT_SECONDS - elapsed)
            };

            tokio::select! {
                biased;
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() {
                        tracing::debug!("Stopping UDP stream for host tunnel");
                        break;
                    }
                }
                Ok((n, addr)) = udp_read.peek_from(&mut buf) => {
                    if addr != client_local_addr {
                        continue
                    };

                    if udp_read.recv(&mut buf[..n]).await.is_err() {
                        // We can safely ignore this case,
                        // the thread corresponding to the unknown IP will read the data.
                        continue;
                    };

                    if n == 0 {
                        tracing::debug!("Tunnel stream closed");
                        break;
                    }

                    last_active.store(now_secs(), Ordering::SeqCst);
                    tunnel_send.write_all(&buf[..n]).await.unwrap();
                }
                _ = tokio::time::sleep(timeout) => {
                    let now = now_secs();
                    let last = last_active.load(Ordering::SeqCst);
                    let elapsed = now.saturating_sub(last);

                    if elapsed >= CONNECTION_TIMEOUT_SECONDS {
                        tracing::debug!("UDP stream timed out");
                        break;
                    }
                }
            }
        }
    });

    tracing::info!("Started tunnel UDP forwarder for host tunnel");

    let _ = tokio::try_join!(tunnel_to_udp_forwader, udp_to_tunnel_forwader);

    ClientTunnel::disconnect_local_connection(local_connections, local_client_connection).await;

    Ok(())
}
