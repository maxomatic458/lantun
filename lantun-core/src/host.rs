use iroh::{
    Endpoint, PublicKey, RelayMode, SecretKey,
    endpoint::{Connection, ReadError, WriteError},
};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use thiserror::Error;
use tokio::{
    net::{TcpSocket, UdpSocket},
    sync::{RwLock, watch},
    task::JoinHandle,
};

use crate::{
    ALPN, TunnelProtocol,
    common::{Client2HostControlMsg, LocalClientConnection, TunnelCommon, receive_packet},
    get_unspecified,
    net::{tcp::bi_tcp_stream_host, udp::bi_udp_host},
};

#[derive(Error, Debug)]
pub enum HostError {
    #[error("Tunnel with local addr {0} already exists")]
    TunnelAlreadyExists(SocketAddr),
    #[error("Failed to create endpoint: {0}")]
    FailedToCreateEndpoint(String),
    #[error("Failed to accept connection: {0}")]
    FailedToAcceptConnection(String),
    #[error("Failed to create stream: {0}")]
    FailedToCreateStream(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Write error: {0}")]
    WriteError(#[from] WriteError),
    #[error("Read error: {0}")]
    ReadError(#[from] ReadError),
    #[error("Connection error: {0}")]
    ConnectionError(#[from] iroh::endpoint::ConnectionError),
    #[error("Tunnel is not running")]
    TunnelNotRunning,
    #[error("Receive packet error: {0}")]
    ReceivePacketError(#[from] crate::common::ReceivePacketError),
    #[error("Unexpected control message: {0:?}")]
    UnexpectedControlMessage(Client2HostControlMsg),
    #[error("Tunnel is already open")]
    TunnelAlreadyOpen,
}

/// The host state.
pub struct HostState {
    tunnels: HashMap<[u8; 32], HostTunnel>,
}

impl Default for HostState {
    fn default() -> Self {
        Self::new()
    }
}

impl HostState {
    /// Create a new host state.
    pub fn new() -> Self {
        Self {
            tunnels: HashMap::new(),
        }
    }

    /// Get the tunnels.
    pub fn tunnels(&self) -> &HashMap<[u8; 32], HostTunnel> {
        &self.tunnels
    }

    /// Get the tunnels mutable.
    pub fn tunnels_mut(&mut self) -> &mut HashMap<[u8; 32], HostTunnel> {
        &mut self.tunnels
    }

    /// Create a new tunnel.
    pub async fn create_tunnel(
        &mut self,
        local_addr: SocketAddr,
        name: String,
        secret: [u8; 32],
        protocol: TunnelProtocol,
    ) -> &mut HostTunnel {
        let tunnel = HostTunnel {
            name: name.clone(),
            secret: secret.into(),
            host_addr: local_addr,
            protocol,
            is_open: Arc::new(AtomicBool::new(false)),
            running_props: None,
        };

        self.tunnels.insert(secret, tunnel);

        self.tunnels.get_mut(&secret).unwrap()
    }

    pub fn get_tunnel(&self, secret: &[u8; 32]) -> Option<&HostTunnel> {
        self.tunnels.get(secret)
    }

    pub fn get_tunnel_mut(&mut self, secret: &[u8; 32]) -> Option<&mut HostTunnel> {
        self.tunnels.get_mut(secret)
    }
}

/// A [`HostTunnel`] will relay traffic to and from the `host_addr` to the connected clients
/// (all clients that know this tunnel's `secret`).
pub struct HostTunnel {
    /// The name of the tunnel.
    pub name: String,
    /// The secret allows others to connect to this tunnel.
    /// This value is unique for every existing HostTunnel.
    secret: SecretKey,
    /// The local socket address this tunnel is bound to.
    host_addr: SocketAddr,
    /// The protocol of the tunnel.
    protocol: TunnelProtocol,
    /// If the tunnel is open.
    is_open: Arc<AtomicBool>,
    /// Running tunnel values.
    // TODO: type state pattern.
    running_props: Option<RunningHostTunnelProps>,
}

/// Clients connected to the tunnel.
pub struct ConnectedClients {
    /// A tunnel can have multiple connected clients.
    tunnel_clients: HashMap<PublicKey, ConnectedClient>,
}

/// A single connected client.
#[derive(Clone)]
pub struct ConnectedClient {
    /// A client can have multiple local connections.
    /// e.g 2 clients running on the same machine connecting to the same tunnel.
    ///
    /// The key is the virtual address of the client (which is used on the host).
    local_connections: Arc<RwLock<HashMap<SocketAddr, Arc<RwLock<LocalClientConnection>>>>>,
    /// The connection to the client.
    #[allow(dead_code)]
    connection: Connection,
}

/// The properties of the running host tunnel.
struct RunningHostTunnelProps {
    /// The active connections of the tunnel.
    connections: Arc<RwLock<ConnectedClients>>,
    /// Quinn endpoint.
    endpoint: Endpoint,
    /// Channel sender to stop the entire tunnel.
    stop_tx: watch::Sender<bool>,
}

impl HostTunnel {
    /// Start the tunnel loop.
    pub async fn start(&mut self) -> Result<JoinHandle<Result<(), HostError>>, HostError> {
        tracing::debug!("Starting host tunnel \"{}\"", self.name);
        if self.is_running() {
            tracing::warn!(
                "Host tunnel {} is already open, not starting again",
                self.name
            );
            return Err(HostError::TunnelAlreadyOpen);
        }

        let endpoint = Endpoint::builder()
            .alpns(vec![ALPN.to_vec()])
            .secret_key(self.secret.clone())
            .relay_mode(RelayMode::Default)
            .discovery_n0()
            .bind()
            .await
            .map_err(|e| HostError::FailedToCreateEndpoint(e.to_string()))?;

        let connections = Arc::new(RwLock::new(ConnectedClients {
            tunnel_clients: HashMap::new(),
        }));

        let tunnel_name = self.name.clone();
        let (stop_tx, stop_rx) = watch::channel(false);

        self.running_props = Some(RunningHostTunnelProps {
            connections: connections.clone(),
            endpoint: endpoint.clone(),
            stop_tx,
        });
        self.is_open.store(true, Ordering::SeqCst);

        let local_addr = self.host_addr;
        let protocol = self.protocol;

        // Thread to handle incoming connections.
        let conn_handler = tokio::spawn(async move {
            loop {
                let conn = endpoint.accept().await;
                let Some(conn) = conn else {
                    tracing::debug!("Failed to accept connection");
                    continue;
                };

                let conn = match conn.await {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::debug!("Failed to accept connection: {}", e);
                        continue;
                    }
                };

                let peer_public_key = conn
                    .remote_node_id()
                    .expect("Failed to get peer public key");

                {
                    let mut connections = connections.write().await;
                    if connections.tunnel_clients.contains_key(&peer_public_key) {
                        tracing::warn!(
                            "Host tunnel \"{}\" already has a connection from {}, ignoring",
                            tunnel_name,
                            peer_public_key
                        );
                        continue;
                    }

                    connections.tunnel_clients.insert(
                        peer_public_key,
                        ConnectedClient {
                            local_connections: Arc::new(RwLock::new(HashMap::new())),
                            connection: conn.clone(),
                        },
                    );
                } // drop lock

                tracing::debug!(
                    "Host tunnel \"{}\" accepted a new connection from {}",
                    tunnel_name,
                    peer_public_key
                );

                match HostTunnel::handle_connection(
                    conn,
                    local_addr,
                    protocol,
                    connections.clone(),
                    stop_rx.clone(),
                )
                .await
                {
                    Ok(_) => {
                        tracing::debug!("Host tunnel \"{}\" connection closed", tunnel_name);
                    }
                    Err(e) => {
                        tracing::error!("Host tunnel \"{}\" connection error: {}", tunnel_name, e);
                    }
                }
            }
        });

        tracing::info!("Host tunnel \"{}\" started", self.name);

        Ok(conn_handler)
    }

    async fn handle_connection(
        conn: Connection,
        tunnel_local_addr: SocketAddr,
        protocol: TunnelProtocol,
        connections: Arc<RwLock<ConnectedClients>>,
        stop_rx: watch::Receiver<bool>,
    ) -> Result<(), HostError> {
        loop {
            let (tunnel_send, tunnel_recv) = conn.accept_bi().await?;
            // Get the local address of the local client connection.
            let c2h_conn_req: Client2HostControlMsg = receive_packet(&conn).await?;
            let local_addr = match c2h_conn_req {
                Client2HostControlMsg::ConnReq { local_addr } => local_addr,
            };

            let client_virtual_addr = get_unspecified(local_addr.is_ipv4());

            let local_client_connection = Arc::new(RwLock::new(LocalClientConnection {
                client_local_addr: local_addr,
                client_virtual_addr,
                last_active: Arc::new(AtomicU64::new(0)),
            }));

            let connected_client: Arc<ConnectedClient>;

            {
                let mut connections = connections.write().await;
                let client = connections
                    .tunnel_clients
                    .get_mut(&conn.remote_node_id().unwrap())
                    .unwrap();
                client
                    .local_connections
                    .write()
                    .await
                    .insert(client_virtual_addr, local_client_connection.clone());
                connected_client = Arc::new(client.clone());
            } // drop lock

            match protocol {
                TunnelProtocol::Tcp => {
                    let socket = if tunnel_local_addr.is_ipv4() {
                        TcpSocket::new_v4()?
                    } else {
                        TcpSocket::new_v6()?
                    };

                    socket.bind(client_virtual_addr)?;
                    let stream = socket.connect(tunnel_local_addr).await?;

                    bi_tcp_stream_host(
                        stream,
                        connected_client,
                        local_client_connection.clone(),
                        tunnel_send,
                        tunnel_recv,
                        stop_rx.clone(),
                    )
                    .await?;
                }
                TunnelProtocol::Udp => {
                    let socket = UdpSocket::bind(client_virtual_addr).await?;
                    socket.connect(tunnel_local_addr).await?;

                    bi_udp_host(
                        socket,
                        connected_client,
                        local_client_connection.clone(),
                        tunnel_send,
                        tunnel_recv,
                        stop_rx.clone(),
                    )
                    .await?;
                }
            }
        }
    }

    /// Static method to disconnect a remote client connection, which closes all related
    /// streams and sockets.
    /// This is static so it can be called from other threads given the connections.
    ///
    /// # Returns
    /// * `true` if the connection was successfully disconnected
    /// * `false` otherwise.
    pub async fn disconnect_local_client_connection(
        connection: Arc<ConnectedClient>,
        local_connection: Arc<RwLock<LocalClientConnection>>,
    ) -> bool {
        let virtual_addr = local_connection.read().await.client_virtual_addr;
        let mut connections = connection.local_connections.write().await;

        if connections.remove(&virtual_addr).is_some() {
            tracing::debug!(
                "Disconnected local connection {}. Connections left: {}",
                virtual_addr,
                connections.len()
            );
            true
        } else {
            tracing::warn!("Tried disconnecting a local connection that does not exist");
            false
        }
    }

    /// Stop the tunnel.
    /// This will disconnect all clients and stop the tunnel.
    pub async fn stop(&mut self) -> Result<(), HostError> {
        if !self.is_running() {
            tracing::warn!("Tunnel \"{}\" is already stopped", self.name);
            return Ok(());
        }

        if let Some(running_props) = self.running_props.take() {
            running_props.stop_tx.send(true).unwrap();
            running_props.endpoint.close().await;
        }

        self.is_open.store(false, Ordering::SeqCst);
        tracing::info!("Tunnel \"{}\" stopped", self.name);
        Ok(())
    }

    /// Get the numbre of connected clients.
    pub async fn num_clients(&self) -> usize {
        if let Some(running_props) = &self.running_props {
            let connections = running_props.connections.read().await;
            connections.tunnel_clients.len()
        } else {
            0
        }
    }
}

impl TunnelCommon for HostTunnel {
    fn secret(&self) -> [u8; 32] {
        self.secret.to_bytes()
    }

    fn protocol(&self) -> TunnelProtocol {
        self.protocol
    }

    fn is_running(&self) -> bool {
        self.is_open.load(Ordering::SeqCst)
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    async fn num_active_connections(&self) -> usize {
        if let Some(running_props) = &self.running_props {
            let mut local_connections = 0;
            for client in running_props
                .connections
                .read()
                .await
                .tunnel_clients
                .values()
            {
                local_connections += client.local_connections.read().await.len();
            }

            local_connections
        } else {
            0
        }
    }

    fn addr(&self) -> SocketAddr {
        self.host_addr
    }
}
