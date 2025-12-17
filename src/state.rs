// use std::{collections::HashMap, net::SocketAddr, sync::Arc};
// use iroh::{Endpoint, PublicKey, SecretKey, endpoint::Connection};
// use tokio::{net::TcpListener, sync::RwLock};
// use crate::{common::{ALPN, Protocol}, config::ClientTunnelConfig, host::HostTunnel};
// use thiserror::Error;

// #[derive(Error, Debug)]
// pub enum ClientError {
//     #[error("Failed to bind iroh endpoint: {0}")]
//     EndpointBindError(#[from] iroh::endpoint::BindError),
//     #[error("Failed to connect to host tunnel: {0}")]
//     FailedToConnect(#[from] iroh::endpoint::ConnectError),
// }

// /// Marker type for a stopped client tunnel
// pub struct Stopped;
// /// Marker type for a running client tunnel
// pub struct Running(Arc<RwLock<ActiveProps>>);

// pub struct ActiveProps {
//     /// Maps local client addresses to their underlying connections.
//     local_connections: Arc<RwLock<HashMap<SocketAddr, LocalConnection>>>,
//     /// The endpoint of the connection.
//     endpoint: Endpoint,
//     /// The handle to the local connection acceptor task
//     /// (We want a seperate data channel for each local connection,
//     /// so we can have multiple local connections coming through the same tunnel)
//     conn_acceptor: tokio::task::JoinHandle<()>,
// }

// struct LocalConnection {
//     /// The local socket address that exposes the tunnel
//     local_addr: SocketAddr,
//     /// The handle to the forwarder task
//     forwarder: tokio::task::JoinHandle<()>,
// }

// impl LocalConnection {
//     /// Will block until an connection attempt is made to the local addr of the tunnel
//     pub fn new(connection: &Connection, local_addr: SocketAddr, proto: Protocol) -> Result<Self, ClientError> {
//         match proto {
//             Protocol::Tcp => {

//             },
//             Protocol::Udp => {
//                 todo!();
//             }
//         }
//     }
// }

// pub struct ClientTunnel<State = Stopped> {
//     pub name: Option<String>,
//     /// The public key of the remote host tunnel.
//     public: PublicKey,
//     /// The local address the remote tunnel is exposed on.
//     addr: SocketAddr,
//     proto: Protocol,
//     state: State,
// }

// impl ClientTunnel {
//     /// Get the name of the tunnel
//     pub fn name(&self) -> Option<&String> {
//         self.name.as_ref()
//     }

//     /// Get the public key of the remote host tunnel
//     pub fn public_key(&self) -> &PublicKey {
//         &self.public
//     }

//     /// Get the local address the remote tunnel is exposed on
//     pub fn address(&self) -> &SocketAddr {
//         &self.addr
//     }

//     /// Get the protocol of the tunnel
//     pub fn protocol(&self) -> &Protocol {
//         &self.proto
//     }
// }

// impl ClientTunnel<Stopped> {
//     pub fn new(
//         name: Option<String>,
//         public: PublicKey,
//         addr: SocketAddr,
//         protocol: Protocol,
//     ) -> Self {
//         ClientTunnel {
//             name,
//             public,
//             addr,
//             proto: protocol,
//             state: Stopped,
//         }
//     }

//     /// Creates a client tunnel from config
//     pub fn from_config(config: &ClientTunnelConfig) -> Self {
//         Self::new(
//             config.name.clone(),
//             config.host_public,
//             config.local_addr,
//             config.proto,
//         )
//     }

//     pub async fn start<T>(self) -> Result<ClientTunnel<Running>, ClientError> {
//         tracing::info!("Starting client tunnel \"{:?}\" to {}", self.name, self.addr);

//         let endpoint = Endpoint::builder()
//             .alpns(vec![ALPN.to_vec()])
//             .relay_mode(iroh::RelayMode::Default)
//             .bind()
//             .await
//             .map_err(ClientError::EndpointBindError)?;

//         let conn = endpoint
//             .connect(self.public, ALPN)
//             .await
//             .map_err(ClientError::FailedToConnect)?;

//         tracing::info!(
//             "Client tunnel \"{:?}\" connected to host at {}",
//             self.name,
//             self.addr
//         );

//         let local_connections = Arc::new(RwLock::new(HashMap::new()));

//         let conn_acceptor = {
//             let local_connections = local_connections.clone();
//             let proto = self.proto;
//             let addr = self.addr;

//             tokio::spawn(async move {
//                 loop {
//                     let Ok(local_conn) = LocalConnection::new(&conn, addr, proto) else {
//                         continue;
//                     };

//                     {
//                         let mut connections = local_connections.write().await;
//                         connections.insert(local_conn.local_addr, local_conn);
//                     }

//                 }

//                 // match proto {
//                 //     Protocol::Tcp => {
//                 //         let listener = TcpListener::bind(addr).await?;
//                 //         tracing::debug!("Local TCP listener for client tunnel bound at {}", addr);

//                 //         loop {
//                 //             let Ok((socket, local_addr)) = listener.accept().await else {
//                 //                 continue;
//                 //             };

//                 //             tracing::info!(
//                 //                 "Accepted local TCP connection for client tunnel at {}",
//                 //                 local_addr
//                 //             );

//                 //             let (socket_read, socket_write) = socket.into_split();
//                 //             let (channel_write, channel_read) = conn.open_bi().await?;
//                 //         }
//                 //     }
//                 //     Protocol::Udp => {
//                 //         todo!();
//                 //     }
//                 // }
//             })
//         };

//         todo!();
//     }
// }

// impl ClientTunnel<Running> {

// }
