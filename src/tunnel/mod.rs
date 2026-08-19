pub mod client;
pub mod host;

use std::{
    fmt,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    str::FromStr,
};

pub use client::{ClientTunnel, ClientTunnelBuilder, ReconnectPolicy};
pub use host::{HostTunnel, HostTunnelBuilder};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TunnelProtocol {
    Tcp,
    Udp,
}

impl fmt::Display for TunnelProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TunnelProtocol::Tcp => write!(f, "tcp"),
            TunnelProtocol::Udp => write!(f, "udp"),
        }
    }
}

impl FromStr for TunnelProtocol {
    type Err = std::io::Error;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "tcp" => Ok(Self::Tcp),
            "udp" => Ok(Self::Udp),
            other => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid protocol: {other}"),
            )),
        }
    }
}

impl serde::Serialize for TunnelProtocol {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        })
    }
}

impl<'de> serde::Deserialize<'de> for TunnelProtocol {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// A local "bind any" address.
pub(crate) fn any_addr(ipv4: bool) -> SocketAddr {
    if ipv4 {
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0))
    }
}
