use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct ForwarderConfig {
    buffer_size: usize,
    timeout: Option<std::time::Duration>,
}

impl Default for ForwarderConfig {
    fn default() -> Self {
        Self {
            buffer_size: 65536,
            timeout: None,
        }
    }
}

/// Starts the forwarding between a local socket and a quic message stream
pub fn forwarder<RS, WS, RC, WC, F, Fut>(
    mut from1: RS,
    mut to1: WC,
    mut from2: RC,
    mut to2: WS,
    config: ForwarderConfig,
    mut on_connection_closed: F,
) -> tokio::task::JoinHandle<()>
where
    RS: AsyncReadExt + Unpin + Send + 'static,
    WS: AsyncWriteExt + Unpin + Send + 'static,
    RC: AsyncReadExt + Unpin + Send + 'static,
    WC: AsyncWriteExt + Unpin + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
    F: FnMut() -> Fut + Send + 'static,
{
    tokio::spawn(async move {
        let mut socket_buf = vec![0u8; config.buffer_size];
        let mut stream_buf = vec![0u8; config.buffer_size];

        loop {
            let timeout_future = match config.timeout {
                Some(duration) => tokio::time::sleep(duration),
                None => tokio::time::sleep(std::time::Duration::MAX),
            };

            tokio::select! {
                // Read from socket and write to stream
                res = from1.read(&mut socket_buf) => {
                    println!("read: {:?}", res);
                    match res {
                        Ok(0) => {
                            tracing::debug!("Socket closed");
                            break;
                        }
                        Ok(n) => {
                            if let Err(e) = to1.write_all(&socket_buf[..n]).await {
                                tracing::error!("Error writing to stream: {}", e);
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::error!("Error reading from socket: {}", e);
                            break;
                        }
                    }
                }
                // Read from stream and write to socket
                res = from2.read(&mut stream_buf) => {
                    match res {
                        Ok(0) => {
                            tracing::debug!("Stream closed");
                            break;
                        }
                        Ok(n) => {
                            if let Err(e) = to2.write_all(&stream_buf[..n]).await {
                                tracing::error!("Error writing to socket: {}", e);
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::error!("Error reading from stream: {}", e);
                            break;
                        }
                    }
                }
                // Timeout if configured
                _ = timeout_future, if config.timeout.is_some() => {
                    tracing::warn!("Forwarding timeout reached");
                    break;
                }
            }
        }

        on_connection_closed().await;
    })
}
