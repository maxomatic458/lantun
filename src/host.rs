use crate::{
    common::{ALPN, Protocol, UdpReader, UdpWriter},
    config::HostTunnelConfig,
    forwarder::forwarder,
};
use iroh::{Endpoint, PublicKey, SecretKey, endpoint::Connection};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    sync::Arc,
};
use thiserror::Error;
use tokio::{
    net::{TcpSocket, UdpSocket},
    sync::RwLock,
};

#[derive(Error, Debug)]
pub enum HostError {
    #[error("Failed to bind iroh endpoint: {0}")]
    EndpointBind(#[from] iroh::endpoint::BindError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Connection error: {0}")]
    Connection(#[from] iroh::endpoint::ConnectionError),
    #[error("Write error: {0}")]
    Write(#[from] iroh::endpoint::WriteError),
}

/// Marker type for a stopped tunnel
pub struct Stopped;

/// Marker type for a running tunnel
pub struct Running(Arc<RwLock<ActiveProps>>);

/// Data available on an active host tunnel
pub struct ActiveProps {
    /// Maps the local/virtual address of the client to the underlying connection.
    active_connections: Arc<RwLock<HashMap<SocketAddr, ClientConnection>>>,
    /// Endpoint created by this tunnel.
    _endpoint: Endpoint,
    /// The handle to the connection acceptor task
    conn_acceptor: tokio::task::JoinHandle<()>,
}

struct ClientConnection {
    /// The iroh connection.
    connection: Connection,
    /// The virtual address assigned to this client.
    virtual_addr: SocketAddr,
    /// The handle to the stream acceptor task that handles multiple bi-directional streams
    stream_acceptor: tokio::task::JoinHandle<()>,
}

impl ClientConnection {
    pub async fn new(
        connection: Connection,
        local: SocketAddr,
        proto: Protocol,
        all_connections: Arc<RwLock<HashMap<SocketAddr, ClientConnection>>>,
    ) -> Result<Self, HostError> {
        match proto {
            Protocol::Tcp => {
                // Spawn a task that accepts multiple bi-directional streams
                // Each stream represents a separate TCP connection
                let stream_acceptor = {
                    let connection = connection.clone();
                    tokio::spawn(async move {
                        loop {
                            // Accept incoming bi-directional stream from client
                            let Ok((channel_write, mut channel_read)) =
                                connection.accept_bi().await
                            else {
                                tracing::debug!("Connection closed or failed to accept bi stream");
                                break;
                            };

                            tracing::debug!("Accepted bi-directional stream for TCP connection");

                            let all_connections_clone = all_connections.clone();
                            tokio::spawn(async move {
                                // Consume opening byte from client
                                if let Err(e) = channel_read.read_exact(&mut [0u8]).await {
                                    tracing::error!("Failed to read opening byte: {}", e);
                                    return;
                                }

                                let client_addr = if local.is_ipv4() {
                                    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
                                } else {
                                    SocketAddr::V6(SocketAddrV6::new(
                                        Ipv6Addr::UNSPECIFIED,
                                        0,
                                        0,
                                        0,
                                    ))
                                };

                                let socket = if local.is_ipv4() {
                                    TcpSocket::new_v4().unwrap()
                                } else {
                                    TcpSocket::new_v6().unwrap()
                                };

                                socket.bind(client_addr).unwrap();
                                tracing::debug!("Connecting to local service at {}", local);

                                let Ok(stream) = socket.connect(local).await else {
                                    tracing::error!(
                                        "Failed to connect to local service at {}",
                                        local
                                    );
                                    return;
                                };

                                let (stream_read, stream_write) = stream.into_split();

                                // Start forwarding data between the stream and the channel
                                let _forwarder = forwarder(
                                    channel_read,
                                    stream_write,
                                    stream_read,
                                    channel_write,
                                    Default::default(),
                                    move || {
                                        let all_connections = all_connections_clone.clone();
                                        let local_addr = local;
                                        async move {
                                            tracing::info!(
                                                "Local connection at {} closed, removing from active connections",
                                                local_addr
                                            );
                                            let mut connections = all_connections.write().await;
                                            connections.remove(&local_addr);
                                        }
                                    },
                                );

                                // Wait for forwarder to complete
                                _forwarder.await.ok();
                            });
                        }
                    })
                };

                Ok(Self {
                    connection,
                    virtual_addr: local,
                    stream_acceptor,
                })
            }
            Protocol::Udp => {
                let stream_acceptor = {
                    let connection = connection.clone();
                    tokio::spawn(async move {
                        loop {
                            let Ok((channel_write, mut channel_read)) =
                                connection.accept_bi().await
                            else {
                                tracing::debug!("Connection closed or failed to accept bi stream");
                                break;
                            };

                            tracing::debug!("Accepted bi-directional stream for UDP connection");

                            let all_connections_clone = all_connections.clone();
                            tokio::spawn(async move {
                                // Consume opening byte from client
                                if let Err(e) = channel_read.read_exact(&mut [0u8]).await {
                                    tracing::error!("Failed to read opening byte: {}", e);
                                    return;
                                }

                                let client_addr = if local.is_ipv4() {
                                    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
                                } else {
                                    SocketAddr::V6(SocketAddrV6::new(
                                        Ipv6Addr::UNSPECIFIED,
                                        0,
                                        0,
                                        0,
                                    ))
                                };

                                let Ok(socket) = UdpSocket::bind(client_addr).await else {
                                    tracing::error!(
                                        "Failed to bind UDP socket for local service at {}",
                                        local
                                    );
                                    return;
                                };

                                tracing::debug!("Connecting to local UDP service at {}", local);

                                if let Err(e) = socket.connect(local).await {
                                    tracing::error!(
                                        "Failed to connect UDP socket to local service at {}: {}",
                                        local,
                                        e
                                    );
                                    return;
                                }

                                let sock_arc = Arc::new(socket);

                                let stream_read = UdpReader {
                                    socket: sock_arc.clone(),
                                };

                                let stream_write = UdpWriter {
                                    socket: sock_arc.clone(),
                                };

                                let _forwarder = forwarder(
                                    channel_read,
                                    stream_write,
                                    stream_read,
                                    channel_write,
                                    Default::default(),
                                    move || {
                                        let all_connections = all_connections_clone.clone();
                                        let local_addr = local;
                                        async move {
                                            tracing::info!(
                                                "Local UDP connection at {} closed, removing from active connections",
                                                local_addr
                                            );
                                            let mut connections = all_connections.write().await;
                                            connections.remove(&local_addr);
                                        }
                                    },
                                );
                                // Wait for forwarder to complete
                                _forwarder.await.ok();
                            });
                        }
                    })
                };
                Ok(Self {
                    connection,
                    virtual_addr: local,
                    stream_acceptor,
                })
            }
        }
    }
}

/// A tunnel that exposes connections from our LAN to other peers.
pub struct HostTunnel<State = Stopped> {
    pub name: Option<String>,
    secret: SecretKey,
    /// The local address this tunnel exposes.
    addr: SocketAddr,
    proto: Protocol,
    state: State,
}

impl HostTunnel {
    /// Get the name of the tunnel
    pub fn name(&self) -> Option<&String> {
        self.name.as_ref()
    }

    /// Get the secret key of the tunnel
    pub fn secret(&self) -> &SecretKey {
        &self.secret
    }

    /// Get the public key of the tunnel
    pub fn public_key(&self) -> PublicKey {
        self.secret.public()
    }

    /// Get the address of the tunnel
    pub fn address(&self) -> SocketAddr {
        self.addr
    }

    /// Get the protocol of the tunnel
    pub fn protocol(&self) -> Protocol {
        self.proto
    }
}

impl HostTunnel<Stopped> {
    /// Create a new stopped tunnel
    pub fn new(name: Option<String>, secret: SecretKey, addr: SocketAddr, proto: Protocol) -> Self {
        Self {
            name,
            secret,
            addr,
            proto,
            state: Stopped,
        }
    }

    /// Create a host tunnel from config
    pub fn from_config(config: &HostTunnelConfig) -> Self {
        Self::new(
            config.name.clone(),
            config.secret.clone(),
            config.local_addr,
            config.proto,
        )
    }

    /// Start the tunnel, transitioning to the Running state
    pub async fn start(self) -> Result<HostTunnel<Running>, HostError> {
        tracing::info!("Starting host tunnel \"{:?}\" at {}", self.name, self.addr);

        let endpoint = Endpoint::builder()
            .alpns(vec![ALPN.to_vec()])
            .secret_key(self.secret.clone())
            .relay_mode(iroh::RelayMode::Default)
            .bind()
            .await
            .map_err(HostError::EndpointBind)?;

        tracing::info!("Host tunnel \"{:?}\" started", self.name);

        let active_connections = Arc::new(RwLock::new(HashMap::new()));

        let conn_acceptor = {
            let endpoint = endpoint.clone();
            let active_connections = active_connections.clone();
            let proto = self.proto;
            let addr = self.addr;

            tokio::spawn(async move {
                loop {
                    let Some(incoming) = endpoint.accept().await else {
                        continue;
                    };

                    match incoming.accept() {
                        Ok(accepting) => {
                            let Ok(connection) = accepting.await else {
                                tracing::error!("Failed to establish connection");
                                continue;
                            };

                            let conn = match ClientConnection::new(
                                connection,
                                addr,
                                proto,
                                active_connections.clone(),
                            )
                            .await
                            {
                                Ok(c) => c,
                                Err(e) => {
                                    tracing::error!("Failed to create client connection: {}", e);
                                    continue;
                                }
                            };

                            tracing::info!(
                                "Client connected with virtual address {}",
                                conn.virtual_addr
                            );

                            active_connections
                                .write()
                                .await
                                .insert(conn.virtual_addr, conn);
                        }
                        Err(e) => {
                            tracing::error!("Failed to accept connection: {}", e);
                        }
                    }
                }
            })
        };

        let active_props = Arc::new(RwLock::new(ActiveProps {
            active_connections: active_connections.clone(),
            _endpoint: endpoint,
            conn_acceptor,
        }));

        Ok(HostTunnel {
            name: self.name,
            secret: self.secret,
            addr: self.addr,
            proto: self.proto,
            state: Running(active_props),
        })
    }
}

impl HostTunnel<Running> {
    /// Stop the tunnel
    pub async fn stop(self) -> HostTunnel<Stopped> {
        tracing::info!("Stopping host tunnel \"{:?}\"", self.name);

        // Close all active connections
        let active_props = self.state.0.read().await;
        let active_connections = active_props.active_connections.read().await;
        for (_addr, conn) in active_connections.iter() {
            conn.connection.close(0u32.into(), b"");
            conn.stream_acceptor.abort();
        }

        active_props.conn_acceptor.abort();

        tracing::info!("Host tunnel \"{:?}\" stopped", self.name);

        HostTunnel {
            name: self.name,
            secret: self.secret,
            addr: self.addr,
            proto: self.proto,
            state: Stopped,
        }
    }

    pub async fn num_connections(&self) -> usize {
        self.state
            .0
            .read()
            .await
            .active_connections
            .read()
            .await
            .len()
    }
}
