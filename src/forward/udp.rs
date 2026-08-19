use std::{net::SocketAddr, sync::Arc, time::Duration};

use iroh::endpoint::{RecvStream, SendStream};
use tokio::{net::UdpSocket, sync::mpsc, time::timeout};
use tokio_util::sync::CancellationToken;

use crate::{
    error::{Error, Result},
    protocol::IDLE_TIMEOUT_SECS,
};

const MAX_DGRAM: usize = 65_535;

/// Write a length-prefixed datagram to the tunnel bi-stream.
async fn write_dgram(send: &mut SendStream, data: &[u8]) -> Result<()> {
    if data.len() > MAX_DGRAM {
        return Err(Error::Protocol(format!("dgram too large: {}", data.len())));
    }
    let len = (data.len() as u16).to_be_bytes();
    send.write_all(&len).await?;
    send.write_all(data).await?;
    Ok(())
}

/// Read a length-prefixed datagram from the tunnel bi-stream.
/// Returns `None` on clean EOF.
async fn read_dgram(recv: &mut RecvStream) -> Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 2];
    let mut filled = 0;
    while filled < 2 {
        match recv.read(&mut len_buf[filled..]).await? {
            Some(0) | None if filled == 0 => return Ok(None),
            Some(0) | None => return Err(Error::Protocol("EOF mid-length".into())),
            Some(n) => filled += n,
        }
    }
    let len = u16::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    let mut filled = 0;
    while filled < len {
        match recv.read(&mut buf[filled..]).await? {
            Some(0) | None => return Err(Error::Protocol("EOF mid-payload".into())),
            Some(n) => filled += n,
        }
    }
    Ok(Some(buf))
}

/// Host-side UDP forwarder. The `socket` is already connected to the backend service.
pub async fn bi_udp_host(
    socket: UdpSocket,
    mut send: SendStream,
    mut recv: RecvStream,
    cancel: CancellationToken,
) -> Result<()> {
    let socket = Arc::new(socket);
    let idle = Duration::from_secs(IDLE_TIMEOUT_SECS);

    let sock_r = socket.clone();
    let cancel_r = cancel.clone();
    let socket_to_tunnel = async move {
        let mut buf = vec![0u8; MAX_DGRAM];
        loop {
            let recv_fut = sock_r.recv(&mut buf);
            match timeout(idle, recv_fut).await {
                Err(_) => {
                    tracing::debug!("udp host: idle timeout, closing");
                    return Ok::<(), Error>(());
                }
                Ok(Err(e)) => return Err(e.into()),
                Ok(Ok(n)) => {
                    if cancel_r.is_cancelled() {
                        return Ok(());
                    }
                    write_dgram(&mut send, &buf[..n]).await?;
                }
            }
        }
    };

    let sock_w = socket.clone();
    let tunnel_to_socket = async move {
        loop {
            match read_dgram(&mut recv).await? {
                None => return Ok::<(), Error>(()),
                Some(data) => {
                    sock_w.send(&data).await?;
                }
            }
        }
    };

    tokio::select! {
        _ = cancel.cancelled() => {
            tracing::debug!("udp host forwarder cancelled");
        }
        r = socket_to_tunnel => {
            if let Err(e) = r { tracing::debug!("udp host s->t: {e}"); }
        }
        r = tunnel_to_socket => {
            if let Err(e) = r { tracing::debug!("udp host t->s: {e}"); }
        }
    }
    Ok(())
}

/// Client-side per-source UDP session.
pub async fn bi_udp_client_session(
    socket: Arc<UdpSocket>,
    source: SocketAddr,
    mut incoming: mpsc::Receiver<Vec<u8>>,
    mut send: SendStream,
    mut recv: RecvStream,
    cancel: CancellationToken,
) -> Result<()> {
    let idle = Duration::from_secs(IDLE_TIMEOUT_SECS);

    let cancel_up = cancel.clone();
    let up = async move {
        loop {
            match timeout(idle, incoming.recv()).await {
                Err(_) => {
                    tracing::debug!("udp client session {source}: idle timeout");
                    return Ok::<(), Error>(());
                }
                Ok(None) => return Ok(()),
                Ok(Some(data)) => {
                    if cancel_up.is_cancelled() {
                        return Ok(());
                    }
                    write_dgram(&mut send, &data).await?;
                }
            }
        }
    };

    let sock = socket.clone();
    let down = async move {
        loop {
            match read_dgram(&mut recv).await? {
                None => return Ok::<(), Error>(()),
                Some(data) => {
                    sock.send_to(&data, source).await?;
                }
            }
        }
    };

    tokio::select! {
        _ = cancel.cancelled() => {}
        r = up => { if let Err(e) = r { tracing::debug!("udp client {source} up: {e}"); } }
        r = down => { if let Err(e) = r { tracing::debug!("udp client {source} down: {e}"); } }
    }
    Ok(())
}
