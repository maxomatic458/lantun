use std::{
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpStream, UdpSocket},
};

pub const ALPN: &[u8] = b"lan-tun/0.2.0";

#[derive(Clone, Copy, Debug)]
pub enum Protocol {
    Tcp,
    Udp,
}

impl serde::Serialize for Protocol {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Protocol::Tcp => serializer.serialize_str("tcp"),
            Protocol::Udp => serializer.serialize_str("udp"),
        }
    }
}

impl<'de> serde::Deserialize<'de> for Protocol {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "tcp" => Ok(Protocol::Tcp),
            "udp" => Ok(Protocol::Udp),
            _ => Err(serde::de::Error::custom(format!("Unknown protocol: {}", s))),
        }
    }
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::Tcp => write!(f, "tcp"),
            Protocol::Udp => write!(f, "udp"),
        }
    }
}

impl std::str::FromStr for Protocol {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tcp" => Ok(Protocol::Tcp),
            "udp" => Ok(Protocol::Udp),
            _ => Err(format!("Unknown protocol: {}", s)),
        }
    }
}

pub enum TcpStreamOrUdpSocket {
    Tcp(TcpStream),
    Udp(Arc<UdpSocket>),
}

impl TcpStreamOrUdpSocket {
    pub fn from_tcp(stream: TcpStream) -> Self {
        TcpStreamOrUdpSocket::Tcp(stream)
    }

    pub fn from_udp(socket: Arc<UdpSocket>) -> Self {
        TcpStreamOrUdpSocket::Udp(socket)
    }
}

pub struct FilteredUdpReader {
    pub socket: Arc<UdpSocket>,
    pub client_addr: SocketAddr,
}

pub struct FilteredUdpWriter {
    pub socket: Arc<UdpSocket>,
    pub client_addr: SocketAddr,
}

impl AsyncRead for FilteredUdpReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let socket = self.socket.clone();

        // Try to peek at the next packet's sender without consuming it
        match socket.try_peek_sender() {
            Ok(addr) => {
                if addr == self.client_addr {
                    // This is from our client, consume and read it
                    let mut temp_buf = vec![0u8; buf.remaining()];
                    match socket.try_recv_from(&mut temp_buf) {
                        Ok((n, _)) => {
                            buf.put_slice(&temp_buf[..n]);
                            Poll::Ready(Ok(()))
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            // Socket became empty between peek and recv
                            // Register waker and wait
                            let mut read_buf = tokio::io::ReadBuf::new(&mut temp_buf);
                            socket.poll_recv_from(cx, &mut read_buf).map(|_| Ok(()))
                        }
                        Err(e) => Poll::Ready(Err(e)),
                    }
                } else {
                    // This packet is from a different client
                    // Wake ourselves up immediately to check again soon,
                    // but return Pending to let other forwarders run
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No packets available, register waker and wait
                let mut temp_buf = vec![0u8; buf.remaining()];
                let mut read_buf = tokio::io::ReadBuf::new(&mut temp_buf);
                socket.poll_recv_from(cx, &mut read_buf).map(|_| Ok(()))
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl AsyncWrite for FilteredUdpWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let socket = self.socket.clone();
        match socket.try_send_to(buf, self.client_addr) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Register waker and wait for socket to be writable
                socket.poll_send_to(cx, buf, self.client_addr)
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

pub struct UdpWriter {
    pub socket: Arc<UdpSocket>,
}

pub struct UdpReader {
    pub socket: Arc<UdpSocket>,
}

impl AsyncWrite for UdpWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let socket = self.socket.clone();
        match socket.try_send(buf) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Register waker and wait for socket to be writable
                socket.poll_send(cx, buf)
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncRead for UdpReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let socket = self.socket.clone();
        let mut temp_buf = vec![0u8; buf.remaining()];
        match socket.try_recv_from(&mut temp_buf) {
            Ok((n, _)) => {
                buf.put_slice(&temp_buf[..n]);
                Poll::Ready(Ok(()))
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Register waker and wait for data
                let mut read_buf = tokio::io::ReadBuf::new(&mut temp_buf);
                socket.poll_recv_from(cx, &mut read_buf).map(|_| Ok(()))
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl AsyncWrite for UdpReader {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(Err(std::io::Error::other(
            "UdpReader does not support write",
        )))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
