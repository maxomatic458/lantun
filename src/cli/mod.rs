use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::{TunnelProtocol, VERSION};

pub mod config;
pub mod run;

pub use run::run;

fn default_config_file() -> PathBuf {
    dirs::config_dir()
        .expect("failed to determine platform config directory")
        .join("lantun")
        .join("lantun.toml")
}

#[derive(Parser, Debug)]
#[command(version = VERSION, author = env!("CARGO_PKG_AUTHORS"), about = "peer-to-peer port forwarding over QUIC")]
pub struct Args {
    /// Log level (overrides RUST_LOG when set).
    #[arg(long, short = 'l')]
    pub log_level: Option<tracing::Level>,

    /// Config file path.
    #[arg(long, short, default_value = default_config_file().into_os_string())]
    pub config: PathBuf,

    /// Inline host tunnels as a JSON array. When set, the config file is not read or
    /// created. Each element: `{ "name": "...", "local": "127.0.0.1:PORT",
    /// "protocol": "tcp"|"udp", "secret_key": "hex..." }`. Cannot be combined with
    /// subcommands.
    #[arg(long, value_name = "JSON")]
    pub host_tunnels: Option<String>,

    /// Inline client tunnels as a JSON array. When set, the config file is not read or
    /// created. Each element: `{ "name": "...", "local": "127.0.0.1:PORT",
    /// "protocol": "tcp"|"udp", "host_key": "hex..." }` where `host_key` is the public
    /// key of the host tunnel this client connects to. Cannot be combined with
    /// subcommands.
    #[arg(long, value_name = "JSON")]
    pub client_tunnels: Option<String>,

    #[command(subcommand)]
    pub action: Option<Action>,
}

#[derive(Subcommand, Debug)]
pub enum Action {
    /// Create a new host tunnel.
    #[command(name = "add-host")]
    AddHost {
        /// Local backend address that this tunnel will forward traffic to.
        local: std::net::SocketAddr,
        /// Protocol (tcp or udp).
        protocol: TunnelProtocol,
        /// Human-readable name for this tunnel.
        #[arg(default_value = "host-tunnel")]
        name: String,
    },
    /// Create a new client tunnel.
    #[command(name = "add-client")]
    AddClient {
        /// Public key (hex) of the host tunnel this client will connect to.
        host_key: String,
        /// Local address to bind for incoming client connections.
        local: std::net::SocketAddr,
        /// Protocol (tcp or udp).
        protocol: TunnelProtocol,
        /// Human-readable name for this tunnel.
        #[arg(default_value = "client-tunnel")]
        name: String,
    },
    /// List all configured tunnels.
    List,
    /// Remove a tunnel by name.
    Remove {
        /// The name of the tunnel to remove.
        name: String,
    },
    /// Enable a tunnel (only enabled tunnels run when `lantun` is invoked with no subcommand).
    Enable {
        /// The name of the tunnel to enable.
        name: String,
    },
    /// Disable a tunnel.
    Disable {
        /// The name of the tunnel to disable.
        name: String,
    },
}
