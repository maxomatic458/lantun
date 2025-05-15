use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use iroh::endpoint::{RecvStream, SendStream};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{RwLock, watch},
};

use crate::{
    client::{ClientError, ClientTunnel},
    common::{CONNECTION_TIMEOUT_SECONDS, LocalClientConnection, TUNNEL_FORWARDER_BUFFER_SIZE},
    host::{ConnectedClient, HostError, HostTunnel},
    utils::now_secs,
};

pub async fn bi_tcp_stream_host(
    // The stream of the virtual client to the tunneled socket.
    tcp_stream: TcpStream,
    connection: Arc<ConnectedClient>,
    // The current connection of the client to the host.
    local_client_connection: Arc<RwLock<LocalClientConnection>>,
    mut tunnel_send: SendStream,
    mut tunnel_recv: RecvStream,
    mut stop_rx: watch::Receiver<bool>,
) -> Result<(), HostError> {
    tracing::debug!("Starting bidirectional TCP forwarder for the host tunnel");

    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();
    let stop_rx_c = stop_rx.clone();
    let last_active = local_client_connection.read().await.last_active.clone();
    let last_active_c = last_active.clone();

    last_active.store(now_secs(), Ordering::SeqCst);

    let tunnel_to_tcp_forwader = tokio::spawn(async move {
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
                        tracing::debug!("Stopping TCP stream for host tunnel");
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
                    tcp_write.write_all(&buf[..n]).await.unwrap();
                }
                _ = tokio::time::sleep(timeout) => {
                    let now = now_secs();
                    let last = last_active.load(Ordering::SeqCst);
                    let elapsed = now.saturating_sub(last);

                    if elapsed >= CONNECTION_TIMEOUT_SECONDS {
                        tracing::debug!("TCP stream timed out");
                        break;
                    }
                }
            }
        }
    });

    let tcp_to_tunnel_forwader = tokio::spawn(async move {
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
                        tracing::debug!("Stopping TCP stream for host tunnel");
                        break;
                    }
                }
                Ok(n) = tcp_read.read(&mut buf) => {
                    if n == 0 {
                        tracing::debug!("TCP stream closed");
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
                        tracing::debug!("TCP stream timed out");
                        break;
                    }
                }
            }
        }
    });

    tracing::info!("Started Tunnel TCP forwarder for host tunnel");

    let _ = tokio::try_join!(tunnel_to_tcp_forwader, tcp_to_tunnel_forwader);

    HostTunnel::disconnect_local_client_connection(connection, local_client_connection).await;

    tracing::info!("Tunnel TCP forwarder for host tunnel stopped");

    Ok(())
}

pub async fn bi_tcp_stream_client(
    // The stream of the client application to the tunnel socket.
    tcp_stream: TcpStream,
    // All connections on this tunnel.
    local_connections: Arc<RwLock<HashMap<SocketAddr, Arc<RwLock<LocalClientConnection>>>>>,
    // The current connection of the client to the host (on this tunnel).
    local_client_connection: Arc<RwLock<LocalClientConnection>>,
    mut tunnel_send: SendStream,
    mut tunnel_recv: RecvStream,
    mut stop_rx: watch::Receiver<bool>,
) -> Result<(), ClientError> {
    tracing::debug!("Starting bidirectional TCP forwarder for the client tunnel");

    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();
    let stop_rx_c = stop_rx.clone();
    let last_active = local_client_connection.read().await.last_active.clone();
    let last_active_c = last_active.clone();

    last_active.store(now_secs(), Ordering::SeqCst);

    let tunnel_to_tcp_forwader = tokio::spawn(async move {
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
                        tracing::debug!("Stopping TCP stream for client tunnel");
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
                    tcp_write.write_all(&buf[..n]).await.unwrap();
                }
                _ = tokio::time::sleep(timeout) => {
                    let now = now_secs();
                    let last = last_active.load(Ordering::SeqCst);
                    let elapsed = now.saturating_sub(last);

                    if elapsed >= CONNECTION_TIMEOUT_SECONDS {
                        tracing::debug!("TCP stream timed out");
                        break;
                    }
                }
            }
        }
    });

    let tcp_to_tunnel_forwader = tokio::spawn(async move {
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
                        tracing::debug!("Stopping TCP stream for client tunnel");
                        break;
                    }
                }
                Ok(n) = tcp_read.read(&mut buf) => {
                    if n == 0 {
                        tracing::debug!("TCP stream closed");
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
                        tracing::debug!("TCP stream timed out");
                        break;
                    }
                }
            }
        }
    });

    tracing::info!("Started Tunnel TCP forwarder for client tunnel");

    let _ = tokio::try_join!(tunnel_to_tcp_forwader, tcp_to_tunnel_forwader);

    ClientTunnel::disconnect_local_connection(local_connections, local_client_connection).await;

    tracing::info!("Tunnel TCP forwarder for client tunnel stopped");

    Ok(())
}
