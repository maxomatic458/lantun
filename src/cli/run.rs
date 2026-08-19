use std::time::Duration;

use color_eyre::eyre::{Context, bail};
use iroh::{PublicKey, SecretKey};

use crate::{
    ClientTunnel, HostTunnel, ReconnectPolicy, VERSION,
    cli::{Action, Args, config},
    gen_secret,
};

pub async fn run(args: Args) -> color_eyre::Result<()> {
    init_tracing(&args);

    tracing::debug!("lantun v{VERSION}");
    tracing::debug!("config: {}", args.config.display());
    let mut cfg = config::load(&args.config)?;

    match args.action {
        None => run_all(cfg).await,
        Some(Action::AddHost {
            local,
            protocol,
            name,
        }) => {
            if cfg.name_taken(&name) {
                bail!("tunnel name \"{name}\" is already taken");
            }
            if cfg
                .host_tunnels
                .iter()
                .any(|t| t.local == local && t.protocol == protocol)
            {
                bail!("host tunnel {local}/{protocol} already exists");
            }
            let secret = gen_secret();
            let public_key = hex::encode(secret.public());
            cfg.host_tunnels.push(config::HostEntry {
                name: name.clone(),
                local,
                protocol,
                secret_key: hex::encode(secret.to_bytes()),
                enabled: true,
            });
            cfg.host_tunnels.sort_by(|a, b| a.name.cmp(&b.name));
            config::save(&args.config, &cfg)?;
            println!("Created host tunnel \"{name}\" on {local}/{protocol}");
            println!("Public key: {public_key}");
            println!();
            println!("Peers can connect with:");
            println!("  lantun add-client {public_key} <local> {protocol} <name>");
            Ok(())
        }
        Some(Action::AddClient {
            host_key,
            local,
            protocol,
            name,
        }) => {
            if cfg.name_taken(&name) {
                bail!("tunnel name \"{name}\" is already taken");
            }
            let bytes = hex::decode(&host_key).context("host key is not valid hex")?;
            if bytes.len() != 32 {
                bail!(
                    "host key must be 32 bytes (64 hex chars), got {}",
                    bytes.len()
                );
            }
            if cfg
                .client_tunnels
                .iter()
                .any(|t| t.local == local && t.protocol == protocol)
            {
                bail!("client tunnel {local}/{protocol} already exists");
            }
            cfg.client_tunnels.push(config::ClientEntry {
                name: name.clone(),
                local,
                protocol,
                host_key,
                enabled: true,
            });
            cfg.client_tunnels.sort_by(|a, b| a.name.cmp(&b.name));
            config::save(&args.config, &cfg)?;
            println!("Created client tunnel \"{name}\" on {local}/{protocol}");
            Ok(())
        }
        Some(Action::List) => {
            list(&cfg);
            Ok(())
        }
        Some(Action::Remove { name }) => {
            let before = cfg.host_tunnels.len() + cfg.client_tunnels.len();
            cfg.host_tunnels.retain(|t| t.name != name);
            cfg.client_tunnels.retain(|t| t.name != name);
            let after = cfg.host_tunnels.len() + cfg.client_tunnels.len();
            if before == after {
                bail!("no tunnel named \"{name}\"");
            }
            config::save(&args.config, &cfg)?;
            println!("Removed \"{name}\"");
            Ok(())
        }
        Some(Action::Enable { name }) => set_enabled(&args.config, &mut cfg, &name, true),
        Some(Action::Disable { name }) => set_enabled(&args.config, &mut cfg, &name, false),
    }
}

fn set_enabled(
    path: &std::path::Path,
    cfg: &mut config::Config,
    name: &str,
    enabled: bool,
) -> color_eyre::Result<()> {
    let mut changed = false;
    for h in cfg.host_tunnels.iter_mut() {
        if h.name == name {
            h.enabled = enabled;
            changed = true;
        }
    }
    for c in cfg.client_tunnels.iter_mut() {
        if c.name == name {
            c.enabled = enabled;
            changed = true;
        }
    }
    if !changed {
        bail!("no tunnel named \"{name}\"");
    }
    config::save(path, cfg)?;
    println!("{name}: {}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}

fn list(cfg: &config::Config) {
    println!("Host tunnels:");
    if cfg.host_tunnels.is_empty() {
        println!("  (none)");
    }
    for t in &cfg.host_tunnels {
        let flag = if t.enabled { " " } else { "d" };
        println!(
            "  [{flag}] {}: {} ({}) key={}",
            t.name,
            t.local,
            t.protocol,
            t.public_key_hex()
        );
    }
    println!("\nClient tunnels:");
    if cfg.client_tunnels.is_empty() {
        println!("  (none)");
    }
    for t in &cfg.client_tunnels {
        let flag = if t.enabled { " " } else { "d" };
        println!(
            "  [{flag}] {}: {} ({}) -> {}",
            t.name, t.local, t.protocol, t.host_key
        );
    }
}

async fn run_all(cfg: config::Config) -> color_eyre::Result<()> {
    let host_entries: Vec<_> = cfg.host_tunnels.into_iter().filter(|t| t.enabled).collect();
    let client_entries: Vec<_> = cfg
        .client_tunnels
        .into_iter()
        .filter(|t| t.enabled)
        .collect();

    if host_entries.is_empty() && client_entries.is_empty() {
        println!("No enabled tunnels. Add one with `lantun add-host` or `lantun add-client`.");
        return Ok(());
    }

    let mut hosts: Vec<HostTunnel> = Vec::new();
    let mut clients: Vec<ClientTunnel> = Vec::new();

    for entry in host_entries {
        let secret = decode_secret_key(&entry.secret_key)?;
        let public_key = hex::encode(secret.public());
        let host = HostTunnel::builder()
            .secret(secret)
            .forward_to(entry.local)
            .protocol(entry.protocol)
            .name(entry.name.clone())
            .start()
            .await
            .with_context(|| format!("failed to start host tunnel {}", entry.name))?;
        println!(
            "host \"{}\" listening for peers on {}/{} (public key {})",
            entry.name, entry.local, entry.protocol, public_key
        );
        hosts.push(host);
    }

    let reconnect = reconnect_policy_from_env();
    for entry in client_entries {
        let key = decode_public_key(&entry.host_key)?;
        let mut builder = ClientTunnel::builder()
            .host_key(key)
            .bind(entry.local)
            .protocol(entry.protocol)
            .name(entry.name.clone());
        if let Some(ref policy) = reconnect {
            builder = builder.reconnect(policy.clone());
        }
        let client = builder
            .start()
            .await
            .with_context(|| format!("failed to start client tunnel {}", entry.name))?;
        println!(
            "client \"{}\" bound to {}/{}",
            entry.name, entry.local, entry.protocol
        );
        clients.push(client);
    }

    println!("Press Ctrl-C to stop.");

    let ctrl_c = tokio::signal::ctrl_c();
    let host_dead = async {
        let names: Vec<_> = hosts.iter().map(|h| h.name().to_string()).collect();
        let deaths = hosts.iter().map(|h| Box::pin(h.wait_dead()));
        if deaths.len() == 0 {
            // No hosts — wait.
            std::future::pending::<()>().await;
            unreachable!()
        } else {
            let (_, idx, _) = futures::future::select_all(deaths).await;
            names[idx].clone()
        }
    };

    let fatal = tokio::select! {
        r = ctrl_c => {
            r?;
            println!("\nShutting down...");
            None
        }
        name = host_dead => Some(name),
    };

    for h in hosts {
        if let Err(e) = h.shutdown().await {
            tracing::warn!("host shutdown: {e}");
        }
    }
    for c in clients {
        if let Err(e) = c.shutdown().await {
            tracing::warn!("client shutdown: {e}");
        }
    }

    if let Some(name) = fatal {
        bail!("host tunnel \"{name}\" died unexpectedly — exiting...");
    }
    Ok(())
}

fn decode_secret_key(hex_str: &str) -> color_eyre::Result<SecretKey> {
    let bytes = hex::decode(hex_str).context("secret_key is not valid hex")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| color_eyre::eyre::eyre!("secret_key must be 32 bytes"))?;
    Ok(SecretKey::from_bytes(&arr))
}

fn decode_public_key(hex_str: &str) -> color_eyre::Result<PublicKey> {
    let bytes = hex::decode(hex_str).context("public_key is not valid hex")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| color_eyre::eyre::eyre!("public_key must be 32 bytes"))?;
    PublicKey::from_bytes(&arr).map_err(|e| color_eyre::eyre::eyre!("invalid public key: {e}"))
}

fn init_tracing(args: &Args) {
    match args.log_level {
        Some(level) => {
            tracing_subscriber::fmt()
                .with_max_level(level)
                .with_target(true)
                .init();
        }
        None => {
            let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "lantun=info".into());
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .without_time()
                .init();
        }
    }

    let _ = color_eyre::install();
}

/// Read `LANTUN_RECONNECT_INITIAL_MS` and `LANTUN_RECONNECT_MAX_MS` env vars, if either is
/// set. Primarily useful for tests that need quicker retry cycles than the production
/// default (1s → 60s exponential).
fn reconnect_policy_from_env() -> Option<ReconnectPolicy> {
    let init_ms = std::env::var("LANTUN_RECONNECT_INITIAL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok());
    let max_ms = std::env::var("LANTUN_RECONNECT_MAX_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok());
    if init_ms.is_none() && max_ms.is_none() {
        return None;
    }
    let init = init_ms.unwrap_or(1000);
    let max = max_ms.unwrap_or(init.max(60_000));
    Some(ReconnectPolicy {
        max_attempts: None,
        initial_backoff: Duration::from_millis(init),
        max_backoff: Duration::from_millis(max),
    })
}
