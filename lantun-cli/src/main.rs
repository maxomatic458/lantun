use clap::{Parser, Subcommand};
use config::load_config;
use futures::FutureExt;
use lantun_core::{LANTUN_VERSION, TunnelProtocol, gen_secret};
use std::{net::SocketAddr, path::PathBuf, pin::Pin};

mod config;

fn default_config_file() -> PathBuf {
    dirs::config_dir()
        .expect("Failed to get config directory")
        .join("lantun")
        .join("lantun.toml")
}

#[derive(Parser, Debug)]
#[clap(version = LANTUN_VERSION, author = env!("CARGO_PKG_AUTHORS"))]
struct Args {
    #[clap(long, short)]
    /// Log Level
    log_level: Option<tracing::Level>,
    /// Config file
    #[clap(long, short, default_value = default_config_file().to_string_lossy().to_string())]
    config: PathBuf,
    /// Action to perform
    #[clap(subcommand)]
    action: Option<Action>,
}

#[derive(Subcommand, Debug)]
enum Action {
    /// Create a new host tunnel
    #[clap(name = "add-host")]
    CreateHostTunnel {
        /// The local address of the tunnel
        local: SocketAddr,
        /// the protocol of the tunnel
        protocol: TunnelProtocol,
        /// The name of the tunnel
        #[clap(default_value = "host tunnel")]
        name: String,
    },
    #[clap(name = "add-client")]
    AddClientTunnel {
        /// The secret of the tunnel
        secret: String,
        /// The local address this tunnel will bind to
        local: SocketAddr,
        /// The protocol of the tunnel
        protocol: TunnelProtocol,
        /// The name of the tunnel
        #[clap(default_value = "client tunnel")]
        name: String,
    },
    /// List all tunnels
    #[clap(name = "list")]
    ListTunnels,
    /// Remove a tunnel
    #[clap(name = "remove")]
    RemoveTunnel {
        /// The secret of the tunnel
        name: String,
    },
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let args = Args::parse();

    match args.log_level {
        None => {
            let env_filter = std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "lantun_core=info,lantun_cli=info".to_string());
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_target(false)
                .without_time()
                .init();
        }
        Some(level) => {
            tracing_subscriber::fmt()
                .with_max_level(level)
                .with_target(true)
                .init();
        }
    }

    tracing::debug!("LanTun version: {}", LANTUN_VERSION);
    tracing::debug!("Config file: {}", args.config.display());

    let mut config = load_config(&args.config)?;

    match args.action {
        None => {
            if config.host_tunnels.is_empty() && config.client_tunnels.is_empty() {
                println!(
                    "No tunnels found. Use `lantun add-host` or `lantun add-client` to create a tunnel."
                );
                return Ok(());
            }

            let mut lantun_state = config.create_state().await?;

            #[allow(clippy::type_complexity)]
            let mut handles: Vec<
                Pin<Box<dyn Future<Output = Result<(), color_eyre::Report>> + Send>>,
            > = Vec::with_capacity(
                lantun_state.host.tunnels().len() + lantun_state.client.tunnels().len(),
            );

            for host_tunnel in lantun_state.host.tunnels_mut().values_mut() {
                let handle = host_tunnel.start().await?;
                let fut = handle.map(|res| {
                    res.map_err(|e| color_eyre::Report::msg(e.to_string()))
                        .and_then(|inner| inner.map_err(|e| color_eyre::Report::msg(e.to_string())))
                });
                handles.push(Box::pin(fut));
            }

            for client_tunnel in lantun_state.client.tunnels_mut().values_mut() {
                let handle = client_tunnel.connect().await?;
                let fut = handle.map(|res| {
                    res.map_err(|e| color_eyre::Report::msg(e.to_string()))
                        .and_then(|inner| inner.map_err(|e| color_eyre::Report::msg(e.to_string())))
                });
                handles.push(Box::pin(fut));
            }

            futures::future::try_join_all(handles).await?;
        }
        Some(action) => match action {
            Action::CreateHostTunnel {
                local,
                protocol,
                name,
            } => {
                let secret = gen_secret();
                let private_secret = hex::encode(secret.to_bytes());
                let public_secret = hex::encode(secret.public());

                let new_tunnel = config::HostTunnelEntry {
                    name: name.clone(),
                    local,
                    protocol,
                    private_secret,
                    public_secret: public_secret.clone(),
                    enabled: true,
                };

                if config
                    .host_tunnels
                    .iter()
                    .any(|t| t.local == local && t.protocol == protocol)
                {
                    tracing::error!("Tunnel with this local address and protocol already exists");
                    return Ok(());
                }

                config.host_tunnels.push(new_tunnel);
                config.host_tunnels.sort_by(|a, b| a.name.cmp(&b.name));

                config::save_config(&args.config, &config)?;

                println!(
                    "Tunnel \"{}\" with local addr {}/{} created.",
                    name, local, protocol
                );
                println!("The public secret is \"{}\"\n", public_secret);
                println!("To add a client tunnel on the client side, use:");
                println!(
                    "  lantun add-client {} {} {} <name>",
                    public_secret, local, protocol
                );
            }
            Action::AddClientTunnel {
                secret,
                local,
                protocol,
                name,
            } => {
                let secret = hex::decode(secret).map_err(|_| {
                    color_eyre::Report::msg("Invalid secret format. It should be a hex string.")
                })?;

                let new_tunnel = config::ClientTunnelEntry {
                    name: name.clone(),
                    local,
                    protocol,
                    secret: hex::encode(secret),
                    enabled: true,
                };

                if config
                    .client_tunnels
                    .iter()
                    .any(|t| t.local == local && t.protocol == protocol)
                {
                    tracing::error!("Tunnel with this local address and protocol already exists");
                    return Ok(());
                }

                config.client_tunnels.push(new_tunnel);
                config.client_tunnels.sort_by(|a, b| a.name.cmp(&b.name));

                config::save_config(&args.config, &config)?;

                println!(
                    "Client tunnel \"{}\" with local addr {}/{} created.",
                    name, local, protocol
                );
            }
            Action::ListTunnels => {
                println!("Host Tunnels:");
                for tunnel in &config.host_tunnels {
                    println!(
                        "  {}: {}/{}: {}",
                        tunnel.name, tunnel.local, tunnel.protocol, tunnel.public_secret
                    );
                }

                println!("\nClient Tunnels:");
                for tunnel in &config.client_tunnels {
                    println!("  {}: {} -> {}", tunnel.name, tunnel.local, tunnel.secret);
                }
            }
            Action::RemoveTunnel { name } => {
                if let Some(pos) = config.host_tunnels.iter().position(|t| t.name == name) {
                    config.host_tunnels.remove(pos);
                    println!("Removed host tunnel \"{}\"", name);
                    config::save_config(&args.config, &config)?;
                } else if let Some(pos) = config.client_tunnels.iter().position(|t| t.name == name)
                {
                    config.client_tunnels.remove(pos);
                    println!("Removed client tunnel \"{}\"", name);
                    config::save_config(&args.config, &config)?;
                } else {
                    println!("Tunnel \"{}\" not found", name);
                }
            }
        },
    }

    Ok(())
}
