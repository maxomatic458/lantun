use std::{
    fmt::Debug,
    net::SocketAddr,
    sync::{Arc, atomic::AtomicU64},
};

use bincode::{Decode, Encode};
use iroh::endpoint::{Connection, ConnectionError, ReadError};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

use crate::TunnelProtocol;

pub const TUNNEL_FORWARDER_BUFFER_SIZE: usize = 1024;
pub const CONNECTION_TIMEOUT_SECONDS: u64 = 20;

/// A single local connection of that client. (e.g game client)
pub struct LocalClientConnection {
    /// The local address of the client (inside the client network).
    pub client_local_addr: SocketAddr,
    /// The address of the "virtual client socket" that will be bound on the host machine.
    pub client_virtual_addr: SocketAddr,
    pub last_active: Arc<AtomicU64>,
}

pub async fn send_packet<P: Encode + Debug>(conn: &Connection, packet: P) -> std::io::Result<()> {
    tracing::debug!("Sending packet: {:?}", packet);
    let mut send = conn.open_uni().await?;

    let data = bincode::encode_to_vec(&packet, bincode::config::standard()).unwrap();
    send.write_all(&data).await?;

    send.flush().await?;
    send.finish()?;

    Ok(())
}

#[derive(Error, Debug)]
pub enum ReceivePacketError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode error: {0}")]
    Decode(#[from] bincode::error::DecodeError),
    #[error("connection error: {0}")]
    Connection(#[from] ConnectionError),
    #[error("read error: {0}")]
    Read(#[from] ReadError),
}

pub async fn receive_packet<P: Decode<()> + Debug>(
    conn: &Connection,
) -> Result<P, ReceivePacketError> {
    let mut recv = conn.accept_uni().await?;

    let mut buf = Vec::new();

    loop {
        let mut data = vec![0; 1024];
        if let Some(n) = recv.read(&mut data).await? {
            buf.extend_from_slice(&data[..n]);
            continue;
        }

        break;
    }

    let packet = bincode::decode_from_slice(&buf, bincode::config::standard())?.0;
    tracing::debug!("Received packet: {:?}", packet);

    Ok(packet)
}

#[derive(Encode, Decode, Debug)]
pub enum Client2HostControlMsg {
    /// The initial connection request from the client.
    ConnReq {
        /// The local address of the connection (on the client side).
        local_addr: SocketAddr,
    },
}

pub trait TunnelCommon {
    /// Get the tunnel secret.
    fn secret(&self) -> [u8; 32];
    /// Get the tunnel protocol.
    fn protocol(&self) -> TunnelProtocol;
    /// If the tunnel is currently running and connected.
    /// For Host tunnels this means clients can connect.
    /// For Client tunnels this means the tunnel is connected to a host.
    fn is_running(&self) -> bool;
    /// Get the tunnel name.
    fn name(&self) -> String;
    /// Get the amount of active connections.
    #[allow(async_fn_in_trait)]
    async fn num_active_connections(&self) -> usize;
    /// Get the tunnel address.
    fn addr(&self) -> SocketAddr;
}
