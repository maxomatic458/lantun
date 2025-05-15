use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use iroh::{
    Endpoint, PublicKey, RelayMode,
    endpoint::{Connection, ReadError, VarInt, WriteError},
};
use thiserror::Error;
use tokio::{
    net::{TcpListener, UdpSocket},
    sync::{RwLock, watch},
    task::JoinHandle,
};

use crate::{
    ALPN, TunnelProtocol,
    common::{Client2HostControlMsg, LocalClientConnection, TunnelCommon, send_packet},
    net::{tcp::bi_tcp_stream_client, udp::bi_udp_client},
};

#[derive(Error, Debug)]
pub enum ClientError {
    #[error("Failed to create endpoint: {0}")]
    FailedToCreateEndpoint(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Write error: {0}")]
    WriteError(#[from] WriteError),
    #[error("Read error: {0}")]
    ReadError(#[from] ReadError),
    #[error("Tunnel with local address {0}/{1} already exists")]
    TunnelAlreadyExists(SocketAddr, TunnelProtocol),
    #[error("Failed to connect: {0}")]
    FailedToConnect(String),
    #[error("Failed to accept stream: {0}")]
    FailedToAcceptStream(String),
    #[error("Connection error: {0}")]
    ConnectionError(#[from] iroh::endpoint::ConnectionError),
    #[error("Already connected to tunnel")]
    AlreadyConnected,
    #[error("Not connected to tunnel")]
    NotConnected,
}

/// The client state.
pub struct ClientState {
    /// There should only be one tunnel per local SocketAddr and proto.
    tunnels: HashMap<(SocketAddr, TunnelProtocol), ClientTunnel>,
}

impl Default for ClientState {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientState {
    /// Create a new client state.
    pub fn new() -> Self {
        Self {
            tunnels: HashMap::new(),
        }
    }

    /// Get the tunnels.
    pub fn tunnels(&self) -> &HashMap<(SocketAddr, TunnelProtocol), ClientTunnel> {
        &self.tunnels
    }

    /// Get the tunnels mutable.
    pub fn tunnels_mut(&mut self) -> &mut HashMap<(SocketAddr, TunnelProtocol), ClientTunnel> {
        &mut self.tunnels
    }

    /// Create a new tunnel.
    pub async fn create_tunnel(
        &mut self,
        local_addr: SocketAddr,
        name: String,
        secret: [u8; 32],
        protocol: TunnelProtocol,
    ) -> Result<&mut ClientTunnel, ClientError> {
        if self.tunnels.contains_key(&(local_addr, protocol)) {
            return Err(ClientError::TunnelAlreadyExists(local_addr, protocol));
        }

        let tunnel = ClientTunnel {
            name,
            secret: PublicKey::from_bytes(&secret).unwrap(),
            tunnel_addr: local_addr,
            protocol,
            is_connected: Arc::new(AtomicBool::new(false)),
            running_props: None,
        };

        self.tunnels.insert((local_addr, protocol), tunnel);

        Ok(self.tunnels.get_mut(&(local_addr, protocol)).unwrap())
    }

    pub fn get_tunnel(
        &self,
        local_addr: SocketAddr,
        protocol: TunnelProtocol,
    ) -> Option<&ClientTunnel> {
        self.tunnels.get(&(local_addr, protocol))
    }

    pub fn get_tunnel_mut(
        &mut self,
        local_addr: SocketAddr,
        protocol: TunnelProtocol,
    ) -> Option<&mut ClientTunnel> {
        self.tunnels.get_mut(&(local_addr, protocol))
    }
}

/// A [`ClientTunnel`] represents the other end of the [`HostTunnel`].
pub struct ClientTunnel {
    /// The name of the tunnel.
    pub name: String,
    /// The secret of the tunnel this client is connected to.
    /// This value is unique for every existing HostTunnel.
    secret: PublicKey,
    /// The local address which the traffic of the host's tunnel will be forwarded to.
    /// e.g if the Host runs a server, this is the address that the client needs to connect to.
    tunnel_addr: SocketAddr,
    /// The protocol of the tunnel.
    /// TODO: make sure this is the same as the host tunnel.
    protocol: TunnelProtocol,
    /// If the client tunnel is connected to the host tunnel.
    is_connected: Arc<AtomicBool>,
    /// Running tunnel values.
    running_props: Option<RunningClientTunnelProps>,
}

/// The properties of the running client tunnel.
struct RunningClientTunnelProps {
    /// Local connections of this client. (e.g a game client)
    ///
    /// The key is the local address of the application connecting to the tunnel socket.
    local_connections: Arc<RwLock<HashMap<SocketAddr, Arc<RwLock<LocalClientConnection>>>>>,
    /// This tunnels endpoint on the iroh network.
    endpoint: Endpoint,
    /// The connection to the host tunnel.
    connection: Connection,
    /// Channel sender to stop the connection.
    stop_tx: watch::Sender<bool>,
}

impl ClientTunnel {
    /// Get the client address.
    pub fn client_addr(&self) -> SocketAddr {
        self.tunnel_addr
    }

    /// Start the tunnel loop.
    pub async fn connect(&mut self) -> Result<JoinHandle<Result<(), ClientError>>, ClientError> {
        if self.is_running() {
            tracing::warn!(
                "Client tunnel \"{}\" is already connected, not starting again",
                self.name
            );
            return Err(ClientError::AlreadyConnected);
        }

        let endpoint = Endpoint::builder()
            // .alpns(vec![ALPN.to_vec()])
            .relay_mode(RelayMode::Default)
            .discovery_n0()
            .bind()
            .await
            .map_err(|e| ClientError::FailedToCreateEndpoint(e.to_string()))?;

        let conn = endpoint
            .connect(self.secret, ALPN)
            .await
            .map_err(|e| ClientError::FailedToConnect(e.to_string()))?;

        let local_connections = Arc::new(RwLock::new(HashMap::new()));

        let (stop_tx, mut stop_rx) = watch::channel(false);

        self.running_props = Some(RunningClientTunnelProps {
            local_connections: local_connections.clone(),
            connection: conn.clone(),
            stop_tx,
            endpoint,
        });
        self.is_connected.store(true, Ordering::SeqCst);

        tracing::debug!("Client tunnel \"{}\" connected to host", self.name);

        let protocol = self.protocol;
        let tunnel_addr = self.tunnel_addr;

        // TODO: create helper function for this

        let connection_listener = tokio::spawn(async move {
            match protocol {
                TunnelProtocol::Tcp => {
                    let sock = TcpListener::bind(tunnel_addr).await?;
                    tracing::debug!("Client TCP socket listening on {}", tunnel_addr);

                    loop {
                        tokio::select! {
                            biased;
                            _ = stop_rx.changed() => {
                                if *stop_rx.borrow() {
                                    tracing::debug!("Stopping TCP stream for client tunnel");
                                    break;
                                }
                            }
                            Ok((stream, client_local_addr)) = sock.accept() => {
                                let (tunnel_send, tunnel_recv) = conn.open_bi().await?;
                                send_packet(&conn, Client2HostControlMsg::ConnReq { local_addr: client_local_addr  }).await?;

                                let local_client_connection = LocalClientConnection {
                                    client_local_addr,
                                    // We dont need to know this on the client side.
                                    // Maybe create a separate struct for this? or make it an option?
                                    client_virtual_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)),
                                    last_active: Arc::new(AtomicU64::new(0)),
                                };

                                let local_client_connection = Arc::new(RwLock::new(local_client_connection));

                                {
                                    local_connections.write().await.insert(client_local_addr, local_client_connection.clone());
                                } // drop lock

                                // Start TCP forwarder for this connection
                                bi_tcp_stream_client(stream, local_connections.clone(), local_client_connection.clone(), tunnel_send, tunnel_recv, stop_rx.clone()).await?;
                            }
                        }
                    }
                }
                TunnelProtocol::Udp => {
                    // This socket is used to establish UDP sessions.

                    // We use multiple sockets on the same addr so we can use the
                    // connected api for ease of use.
                    // not sure if there are any downsides to this.
                    let socket = UdpSocket::bind(tunnel_addr).await?;
                    let socket = Arc::new(socket);

                    tracing::debug!("Client UDP socket listening on {}", tunnel_addr);

                    loop {
                        let mut buf = vec![0; 1024];
                        let socket = socket.clone();
                        tokio::select! {
                            biased;
                            _ = stop_rx.changed() => {
                                if *stop_rx.borrow() {
                                    tracing::debug!("Stopping UDP stream for client tunnel");
                                    break;
                                }
                            }
                            Ok((_, client_local_addr)) = socket.peek_from(&mut buf) => {
                                println!("Received UDP packet from {}", client_local_addr);
                                // If we already have a connection for this local address, skip it
                                {
                                    let local_connections = local_connections.read().await;
                                    if local_connections.contains_key(&client_local_addr) {
                                        // The connection is already being tunneled
                                        continue;
                                    }
                                } // drop lock

                                let (tunnel_send, tunnel_recv) = conn.open_bi().await?;
                                send_packet(&conn, Client2HostControlMsg::ConnReq { local_addr: client_local_addr  }).await?;

                                let local_client_connection = LocalClientConnection {
                                    client_local_addr,
                                    // We dont need to know this on the client side.
                                    // Maybe create a separate struct for this? or make it an option?
                                    client_virtual_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)),
                                    last_active: Arc::new(AtomicU64::new(0)),
                                };

                                let local_client_connection = Arc::new(RwLock::new(local_client_connection));
                                {
                                    local_connections.write().await.insert(client_local_addr, local_client_connection.clone());
                                } // drop lock

                                bi_udp_client(socket, local_connections.clone(), local_client_connection.clone(), tunnel_send, tunnel_recv, stop_rx.clone()).await?;

                            }
                        }
                    }
                }
            };
            Ok::<(), ClientError>(())
        });

        tracing::info!("Client tunnel \"{}\" is connected", self.name);

        Ok(connection_listener)
    }

    /// Static method to disconnect a local connection, which closes all related
    /// streams and sockets.
    /// This is static so it can be called from other threads given the connections.
    ///
    /// # Returns
    /// * `true` if the connection was successfully disconnected
    /// * `false` otherwise.
    pub async fn disconnect_local_connection(
        local_connections: Arc<RwLock<HashMap<SocketAddr, Arc<RwLock<LocalClientConnection>>>>>,
        connection_to_disconnect: Arc<RwLock<LocalClientConnection>>,
    ) -> bool {
        let mut local_connections = local_connections.write().await;
        let connection_to_disconnect = connection_to_disconnect.read().await;

        let client_local_addr = connection_to_disconnect.client_local_addr;

        if local_connections.remove(&client_local_addr).is_some() {
            tracing::debug!(
                "Disconnected local connection {}. Connections left: {}",
                client_local_addr,
                local_connections.len()
            );
            true
        } else {
            tracing::warn!("Tried disconnecting a local connection that does not exist");
            false
        }
    }

    /// Terminates all connections and stops the client tunnel.
    pub async fn disconnect(&mut self) -> Result<(), ClientError> {
        if !self.is_running() {
            tracing::warn!(
                "Client tunnel \"{}\" is not connected, not stopping",
                self.name
            );
            return Err(ClientError::NotConnected);
        }

        if let Some(running_props) = self.running_props.take() {
            running_props.stop_tx.send(true).unwrap();
            running_props.connection.close(VarInt::from_u32(0), &[]);
            running_props.endpoint.close().await;
        }

        self.is_connected.store(false, Ordering::SeqCst);

        tracing::info!("Client tunnel \"{}\" disconnected", self.name);

        Ok(())
    }
}

impl TunnelCommon for ClientTunnel {
    fn secret(&self) -> [u8; 32] {
        *self.secret.as_bytes()
    }

    fn protocol(&self) -> TunnelProtocol {
        self.protocol
    }

    fn is_running(&self) -> bool {
        self.is_connected.load(Ordering::SeqCst)
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    async fn num_active_connections(&self) -> usize {
        if let Some(running_props) = &self.running_props {
            let local_connections = running_props.local_connections.read().await;
            local_connections.len()
        } else {
            0
        }
    }

    fn addr(&self) -> SocketAddr {
        self.tunnel_addr
    }
}
