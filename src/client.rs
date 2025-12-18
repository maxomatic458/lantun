use crate::common::{FilteredUdpReader, FilteredUdpWriter};
use crate::{
    common::{ALPN, Protocol, TcpStreamOrUdpSocket},
    config::ClientTunnelConfig,
    forwarder::forwarder,
};
use iroh::{Endpoint, PublicKey, endpoint::Connection};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use thiserror::Error;
use tokio::{
    net::{TcpListener, UdpSocket},
    sync::RwLock,
};

#[derive(Error, Debug)]
pub enum ClientError {
    #[error("Failed to bind iroh endpoint: {0}")]
    EndpointBindError(#[from] iroh::endpoint::BindError),
    #[error("Failed to connect to host tunnel: {0}")]
    FailedToConnect(#[from] iroh::endpoint::ConnectError),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Connection error: {0}")]
    ConnectionError(#[from] iroh::endpoint::ConnectionError),
    #[error("Read exact error: {0}")]
    ReadExactError(#[from] iroh::endpoint::ReadExactError),
    #[error("Write error: {0}")]
    WriteError(#[from] iroh::endpoint::WriteError),
}

/// Marker type for a stopped client tunnel
pub struct Stopped;
/// Marker type for a running client tunnel
pub struct Running(Arc<RwLock<ActiveProps>>);

#[derive(Debug)]
pub struct ActiveProps {
    /// Maps local client addresses to their underlying connections.
    local_connections: Arc<RwLock<HashMap<SocketAddr, LocalConnection>>>,
    /// The endpoint of the connection.
    _endpoint: Endpoint,
    /// The handle to the local connection acceptor task
    /// (We want a separate data channel for each local connection,
    /// so we can have multiple local connections coming through the same tunnel)
    conn_acceptor: tokio::task::JoinHandle<()>,
}

#[derive(Debug)]
struct LocalConnection {
    /// The local socket address that exposes the tunnel
    local_addr: SocketAddr,
    /// The handle to the forwarder task
    forwarder: tokio::task::JoinHandle<()>,
}

impl LocalConnection {
    /// Creates a new local connection from an accepted TCP stream
    async fn new(
        connection: &Connection,
        stream: TcpStreamOrUdpSocket,
        local_addr: SocketAddr,
        all_connections: Arc<RwLock<HashMap<SocketAddr, LocalConnection>>>,
    ) -> Result<Self, ClientError> {
        tracing::info!(
            "Accepted local TCP connection for client tunnel at {}",
            local_addr
        );

        // let (stream_read, stream_write) = stream.into_split();
        tracing::debug!("Opening bi-directional stream to host");

        // Client opens a bi-directional stream to the host
        let (mut channel_write, channel_read) = connection.open_bi().await?;

        // Send opening byte to host
        channel_write.write_all(&[0u8]).await?;

        let conn_closer = move || {
            let all_connections = all_connections.clone();
            let local_addr = local_addr;
            async move {
                tracing::info!(
                    "Local connection at {} closed, removing from active connections",
                    local_addr
                );
                let mut connections = all_connections.write().await;
                connections.remove(&local_addr);
            }
        };

        match stream {
            TcpStreamOrUdpSocket::Tcp(stream) => {
                let (stream_read, stream_write) = stream.into_split();

                let forwarder = forwarder(
                    stream_read,
                    channel_write,
                    channel_read,
                    stream_write,
                    Default::default(),
                    conn_closer,
                );

                Ok(LocalConnection {
                    local_addr,
                    forwarder,
                })
            }
            TcpStreamOrUdpSocket::Udp(socket) => {
                println!(
                    "Creating forwarder for UDP local connection at {}",
                    local_addr
                );
                let stream_read = FilteredUdpReader {
                    socket: socket.clone(),
                    client_addr: local_addr,
                };

                let stream_write = FilteredUdpWriter {
                    socket: socket.clone(),
                    client_addr: local_addr,
                };

                let forwarder = forwarder(
                    stream_read,
                    channel_write,
                    channel_read,
                    stream_write,
                    Default::default(),
                    conn_closer,
                );

                Ok(LocalConnection {
                    local_addr,
                    forwarder,
                })
            }
        }

        // Ok(LocalConnection {
        //     local_addr,
        //     forwarder,
        // })
    }
}

pub struct ClientTunnel<State = Stopped> {
    pub name: Option<String>,
    /// The public key of the remote host tunnel.
    public: PublicKey,
    /// The local address the remote tunnel is exposed on.
    addr: SocketAddr,
    proto: Protocol,
    state: State,
}

impl ClientTunnel {
    /// Get the name of the tunnel
    pub fn name(&self) -> Option<&String> {
        self.name.as_ref()
    }

    /// Get the public key of the remote host tunnel
    pub fn host_public_key(&self) -> &PublicKey {
        &self.public
    }

    /// Get the local address the remote tunnel is exposed on
    pub fn address(&self) -> &SocketAddr {
        &self.addr
    }

    /// Get the protocol of the tunnel
    pub fn protocol(&self) -> &Protocol {
        &self.proto
    }
}

impl ClientTunnel<Stopped> {
    pub fn new(
        name: Option<String>,
        public: PublicKey,
        addr: SocketAddr,
        protocol: Protocol,
    ) -> Self {
        ClientTunnel {
            name,
            public,
            addr,
            proto: protocol,
            state: Stopped,
        }
    }

    /// Creates a client tunnel from config
    pub fn from_config(config: &ClientTunnelConfig) -> Self {
        Self::new(
            config.name.clone(),
            config.host_public,
            config.local_addr,
            config.proto,
        )
    }

    pub async fn start(self) -> Result<ClientTunnel<Running>, ClientError> {
        tracing::info!(
            "Starting client tunnel \"{:?}\" to {}",
            self.name,
            self.addr
        );

        let endpoint = Endpoint::builder()
            .alpns(vec![ALPN.to_vec()])
            .relay_mode(iroh::RelayMode::Default)
            .bind()
            .await
            .map_err(ClientError::EndpointBindError)?;

        let conn = endpoint
            .connect(self.public, ALPN)
            .await
            .map_err(ClientError::FailedToConnect)?;

        tracing::info!(
            "Client tunnel \"{:?}\" connected to host at {}",
            self.name,
            self.addr
        );

        let local_connections = Arc::new(RwLock::new(HashMap::new()));

        let conn_acceptor = {
            let local_connections = local_connections.clone();
            let proto = self.proto;
            let addr = self.addr;

            tokio::spawn(async move {
                // For TCP, create a single listener that accepts multiple connections
                match proto {
                    Protocol::Tcp => {
                        let Ok(listener) = TcpListener::bind(addr).await else {
                            tracing::error!(
                                "Failed to bind TCP listener for client tunnel at {}",
                                addr
                            );
                            return;
                        };

                        loop {
                            let Ok((stream, client_addr)) = listener.accept().await else {
                                continue;
                            };

                            let stream = TcpStreamOrUdpSocket::from_tcp(stream);

                            let Ok(local_conn) = LocalConnection::new(
                                &conn,
                                stream,
                                client_addr,
                                local_connections.clone(),
                            )
                            .await
                            else {
                                tracing::error!("Failed to create local connection");
                                continue;
                            };

                            {
                                let mut connections = local_connections.write().await;
                                connections.insert(local_conn.local_addr, local_conn);
                            }
                        }
                    }
                    Protocol::Udp => {
                        let Ok(socket) = UdpSocket::bind(addr).await else {
                            tracing::error!(
                                "Failed to bind UDP socket for client tunnel at {}",
                                addr
                            );
                            return;
                        };

                        let socket = Arc::new(socket);

                        loop {
                            let Ok(local_client_addr) = socket.peek_sender().await else {
                                continue;
                            };

                            // Check if we already have a local connection for this client
                            {
                                let connections = local_connections.read().await;
                                if connections.contains_key(&local_client_addr) {
                                    continue;
                                }
                            }

                            let socket = TcpStreamOrUdpSocket::from_udp(socket.clone());
                            println!(
                                "Bound socket at {} for client at {}",
                                addr, local_client_addr
                            );

                            println!(
                                "Creating new local UDP connection for client at {}",
                                local_client_addr
                            );

                            let Ok(local_conn) = LocalConnection::new(
                                &conn,
                                socket,
                                local_client_addr,
                                local_connections.clone(),
                            )
                            .await
                            else {
                                tracing::error!("Failed to create local UDP connection");
                                continue;
                            };

                            {
                                let mut connections = local_connections.write().await;
                                connections.insert(local_conn.local_addr, local_conn);
                            }
                        }
                    }
                }
            })
        };

        Ok(ClientTunnel {
            name: self.name,
            public: self.public,
            addr: self.addr,
            proto: self.proto,
            state: Running(Arc::new(RwLock::new(ActiveProps {
                local_connections,
                _endpoint: endpoint,
                conn_acceptor,
            }))),
        })
    }
}

impl ClientTunnel<Running> {
    /// Stops the client tunnel
    pub async fn stop(self) -> ClientTunnel<Stopped> {
        tracing::info!("Stopping client tunnel \"{:?}\"", self.name);

        let active_props = Arc::try_unwrap(self.state.0)
            .expect("No other references to active props exist")
            .into_inner();

        // Stop all local connections
        let local_conns = active_props.local_connections.read().await;
        for (_addr, local_conn) in local_conns.iter() {
            local_conn.forwarder.abort();
        }

        // Stop connection acceptor
        active_props.conn_acceptor.abort();

        ClientTunnel {
            name: self.name,
            public: self.public,
            addr: self.addr,
            proto: self.proto,
            state: Stopped,
        }
    }

    pub async fn num_local_connections(&self) -> usize {
        self.state
            .0
            .read()
            .await
            .local_connections
            .read()
            .await
            .len()
    }
}
