mod client;
mod common;
mod config;
mod forwarder;
mod host;
mod state;

use clap::Parser;
use clap::Subcommand;
use iroh::PublicKey;
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::client::ClientTunnel;
use crate::common::Protocol;
use crate::config::HostTunnelConfig;
use crate::config::LantunConfig;
use crate::config::load_config;
use crate::config::save_config;
use crate::host::HostTunnel;

fn default_config_file() -> PathBuf {
    dirs::config_dir()
        .expect("Failed to get config directory")
        .join("lantun")
        .join("lantun.toml")
}

#[derive(Parser, Debug)]
#[clap(version = env!("CARGO_PKG_VERSION"), author = env!("CARGO_PKG_AUTHORS"))]
struct Args {
    #[clap(long, short, default_value = "warn")]
    /// Log Level
    log_level: tracing::Level,
    /// Config file
    #[clap(long, short, default_value = default_config_file().to_string_lossy().to_string())]
    config: PathBuf,
    /// Action to perform
    #[clap(subcommand)]
    action: Option<Action>,
}

#[derive(Debug, Subcommand)]
enum Action {
    #[clap(name = "create-tunnel")]
    CreateHostTunnel {
        /// The local address this tunnel exposes.
        local: SocketAddr,
        /// The protocol to use `tcp` or `udp`
        protocol: Protocol,
        name: Option<String>,
    },
    #[clap(name = "add-client-tunnel")]
    AddClientTunnel {
        /// The local address the remote tunnel is exposed on.
        local: SocketAddr,
        /// The protocol to use `tcp` or `udp`
        protocol: Protocol,
        /// The public key of the remote host tunnel.
        public_key: PublicKey,
        name: Option<String>,
    },
}

pub struct State {
    pub active_host_tunnels: Vec<HostTunnel<host::Running>>,
    pub inactive_host_tunnels: Vec<HostTunnel>,

    pub active_client_tunnels: Vec<ClientTunnel<client::Running>>,
    pub inactive_client_tunnels: Vec<ClientTunnel>,
}

impl State {
    pub async fn from_config(config: &LantunConfig) -> color_eyre::Result<Self> {
        let mut active_host_tunnels = Vec::new();
        let mut inactive_host_tunnels = Vec::new();
        let mut active_client_tunnels = Vec::new();
        let mut inactive_client_tunnels = Vec::new();

        for host_tunnel_config in &config.host_tunnels {
            let host_tunnel = HostTunnel::from_config(host_tunnel_config);

            if host_tunnel_config.enabled {
                println!("Enabling host tunnel {:?}", host_tunnel.public_key());
                active_host_tunnels.push(host_tunnel.start().await?);
            } else {
                inactive_host_tunnels.push(host_tunnel);
            }
        }

        for client_tunnel_config in &config.client_tunnels {
            let client_tunnel = ClientTunnel::from_config(client_tunnel_config);

            if client_tunnel_config.enabled {
                active_client_tunnels.push(client_tunnel.start().await?);
            } else {
                inactive_client_tunnels.push(client_tunnel);
            }
        }

        Ok(Self {
            active_host_tunnels,
            inactive_host_tunnels,
            active_client_tunnels,
            inactive_client_tunnels,
        })
    }
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .without_time()
        .init();

    let args = Args::parse();

    tracing::debug!("Lantun {}", env!("CARGO_PKG_VERSION"));

    let mut config = load_config(&args.config)?;

    match args.action {
        None => {
            tracing::info!("Running lantun with config: {:?}", args.config);

            let state = State::from_config(&config).await?;
        }
        Some(action) => match action {
            Action::CreateHostTunnel {
                local,
                protocol,
                name,
            } => {
                let mut rng = rand::rng();
                tracing::info!(
                    "Creating host tunnel at {} with protocol {:?}",
                    local,
                    protocol
                );
                let host_tunnel =
                    HostTunnel::new(name, iroh::SecretKey::generate(&mut rng), local, protocol);
                config.host_tunnels.push(HostTunnelConfig {
                    name: host_tunnel.name().cloned(),
                    secret: host_tunnel.secret().clone(),
                    local_addr: host_tunnel.address(),
                    proto: host_tunnel.protocol(),
                    enabled: true,
                });
                save_config(&args.config, &config)?;
                tracing::info!("Host tunnel created and saved to config.");
            }
            Action::AddClientTunnel {
                local,
                protocol,
                public_key,
                name,
            } => {
                tracing::info!(
                    "Adding client tunnel to {} with protocol {:?}",
                    local,
                    protocol
                );
                let client_tunnel = ClientTunnel::new(name, public_key, local, protocol);
                config
                    .client_tunnels
                    .push(crate::config::ClientTunnelConfig {
                        name: client_tunnel.name().cloned(),
                        host_public: *client_tunnel.host_public_key(),
                        local_addr: *client_tunnel.address(),
                        proto: *client_tunnel.protocol(),
                        enabled: true,
                    });
                save_config(&args.config, &config)?;
                tracing::info!("Client tunnel added and saved to config.");
            }
        },
    }

    // wait for ctrl-c
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutting down...");

    Ok(())
}
