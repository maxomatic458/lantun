//! # lantun
//!
//! Peer-to-peer port forwarding over QUIC (built on [iroh]) — no router configuration
//! needed. A host exposes a local service (TCP or UDP) behind a public key.
//! Clients that know the public key can bind a local port that transparently forwards
//! traffic through an encrypted P2P tunnel to the host.
//!
//! ## Host side
//!
//! ```no_run
//! # async fn demo() -> lantun::Result<()> {
//! use lantun::{HostTunnel, TunnelProtocol, gen_secret};
//! use std::net::SocketAddr;
//!
//! let secret = gen_secret();
//! println!("share this public key: {}", secret.public());
//!
//! let host = HostTunnel::builder()
//!     .secret(secret)
//!     .forward_to("127.0.0.1:25565".parse::<SocketAddr>().unwrap())
//!     .protocol(TunnelProtocol::Tcp)
//!     .name("my-minecraft")
//!     .start()
//!     .await?;
//!
//! # let _ = host;
//! # Ok(()) }
//! ```
//!
//! ## Client side
//!
//! ```no_run
//! # async fn demo(host_key: iroh::PublicKey) -> lantun::Result<()> {
//! use lantun::{ClientTunnel, TunnelProtocol};
//! use std::net::SocketAddr;
//!
//! let client = ClientTunnel::builder()
//!     .host_key(host_key)
//!     .bind("127.0.0.1:25565".parse::<SocketAddr>().unwrap())
//!     .protocol(TunnelProtocol::Tcp)
//!     .start()
//!     .await?;
//! # let _ = client;
//! # Ok(()) }
//! ```

mod error;
mod forward;
mod protocol;
mod tunnel;

pub mod cli;

pub use error::{Error, Result};
pub use iroh::{NodeAddr, PublicKey, RelayMode, SecretKey};
pub use protocol::{ALPN, PROTOCOL_VERSION};
pub use tunnel::{
    ClientTunnel, ClientTunnelBuilder, HostTunnel, HostTunnelBuilder, ReconnectPolicy,
    TunnelProtocol,
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Generate a secret key.
pub fn gen_secret() -> SecretKey {
    SecretKey::generate(&mut rand::rngs::OsRng)
}
