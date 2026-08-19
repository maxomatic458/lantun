use std::net::SocketAddr;

use bincode::{Decode, Encode};
use iroh::endpoint::RecvStream;
use tokio::io::AsyncWriteExt;

use crate::error::{Error, Result};

/// ALPN protocol identifier.
pub const ALPN: &[u8] = b"lan-tun/0.2.0";

/// Wire-level protocol version.
pub const PROTOCOL_VERSION: u16 = 2;

/// Idle timeout for a forwarded stream.
pub const IDLE_TIMEOUT_SECS: u64 = 120;

/// Client -> Host: sent as the first frame on every bi-stream.
#[derive(Encode, Decode, Debug, Clone)]
pub struct ConnReq {
    pub client_local_addr: SocketAddr,
}

/// Client -> Host: sent once on a dedicated uni-stream immediately after connecting.
/// Host validates and either proceeds or closes the connection.
#[derive(Encode, Decode, Debug, Clone)]
pub struct Hello {
    pub version: u16,
    /// Reserved for future use
    pub capabilities: u32,
}

pub async fn write_frame<T, S>(stream: &mut S, msg: &T) -> Result<()>
where
    T: Encode,
    S: AsyncWriteExt + Unpin,
{
    let data = bincode::encode_to_vec(msg, bincode::config::standard())?;
    let len: u32 = data
        .len()
        .try_into()
        .map_err(|_| Error::Protocol("frame too large".into()))?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&data).await?;
    Ok(())
}

pub async fn read_frame<T>(stream: &mut RecvStream) -> Result<T>
where
    T: Decode<()>,
{
    let mut len_buf = [0u8; 4];
    read_exact(stream, &mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 1 << 20 {
        return Err(Error::Protocol(format!("frame too large: {len}")));
    }
    let mut buf = vec![0u8; len];
    read_exact(stream, &mut buf).await?;
    let (msg, _) = bincode::decode_from_slice(&buf, bincode::config::standard())?;
    Ok(msg)
}

/// iroh's RecvStream::read returns `Option<usize>` (None = clean EOF, Some(n) = n bytes).
/// This helper wraps it to fill a buffer fully or error on premature EOF.
async fn read_exact(stream: &mut RecvStream, buf: &mut [u8]) -> Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        match stream.read(&mut buf[filled..]).await? {
            Some(0) | None => {
                return Err(Error::Protocol("unexpected EOF while reading frame".into()));
            }
            Some(n) => filled += n,
        }
    }
    Ok(())
}
