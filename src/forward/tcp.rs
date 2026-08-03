use iroh::endpoint::{RecvStream, SendStream};
use tokio::{io::copy_bidirectional, net::TcpStream};
use tokio_util::sync::CancellationToken;

use crate::error::Result;

/// Bi-directionally forward bytes between a TCP connection and a QUIC bi-stream.
pub async fn bi_tcp_forward(
    mut tcp: TcpStream,
    mut send: SendStream,
    mut recv: RecvStream,
    cancel: CancellationToken,
) -> Result<()> {
    // Combine the QUIC recv + send streams into a single AsyncRead + AsyncWrite duplex so
    // `copy_bidirectional` can drive both directions itself.
    let mut tunnel = tokio::io::join(&mut recv, &mut send);

    tokio::select! {
        _ = cancel.cancelled() => {
            tracing::debug!("tcp forwarder cancelled");
        }
        r = copy_bidirectional(&mut tcp, &mut tunnel) => {
            match r {
                Ok((up, down)) => tracing::debug!("tcp forwarder finished: {up} up / {down} down"),
                Err(e) => tracing::debug!("tcp forwarder error: {e}"),
            }
        }
    }
    Ok(())
}
