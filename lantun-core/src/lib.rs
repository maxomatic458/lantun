mod client;
mod common;
mod host;
mod net;
mod utils;

use client::ClientState;
use host::HostState;
use iroh::SecretKey;
use rand::rngs::OsRng;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

pub use common::TunnelCommon;

pub const ALPN: &[u8] = b"lan-tun/0.1.0";

pub fn get_unspecified(ip4: bool) -> SocketAddr {
    if ip4 {
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TunnelProtocol {
    Tcp,
    Udp,
}

impl std::fmt::Display for TunnelProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TunnelProtocol::Tcp => write!(f, "tcp"),
            TunnelProtocol::Udp => write!(f, "udp"),
        }
    }
}

/// The state of the LanTun application.
/// This tracks both the host and client state,
/// so a user can use other tunnels as well as create their own.
pub struct LanTun {
    /// The state of the tunnels hosted by the user.
    pub host: HostState,
    /// The state of the tunnels this user is connected to.
    pub client: ClientState,
}

impl Default for LanTun {
    fn default() -> Self {
        Self::new()
    }
}

impl LanTun {
    pub fn new() -> Self {
        let host_state = HostState::new();
        let client_state = ClientState::new();

        LanTun {
            host: host_state,
            client: client_state,
        }
    }
}

pub fn gen_secret() -> SecretKey {
    let mut rng = OsRng;
    SecretKey::generate(&mut rng)
}
