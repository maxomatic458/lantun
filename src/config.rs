use std::{net::SocketAddr, path::Path};

use iroh::{PublicKey, SecretKey};
use serde::{Deserialize, Serialize};

use crate::common::Protocol;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LantunConfig {
    pub host_tunnels: Vec<HostTunnelConfig>,
    pub client_tunnels: Vec<ClientTunnelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostTunnelConfig {
    pub name: Option<String>,
    pub secret: SecretKey,
    /// The Address that will be opened to the internet
    pub local_addr: SocketAddr,
    pub proto: Protocol,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientTunnelConfig {
    pub name: Option<String>,
    /// The hosts public key
    pub host_public: PublicKey,
    /// The address to bind the remote service to
    pub local_addr: SocketAddr,
    pub proto: Protocol,
    pub enabled: bool,
}

pub fn load_config(path: &Path) -> color_eyre::Result<LantunConfig> {
    if !path.exists() {
        tracing::debug!("Config file not found, creating a new one at {:?}", path);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, toml::ser::to_string(&LantunConfig::default())?)?;
        return Ok(LantunConfig::default());
    }

    let config: LantunConfig = toml::de::from_str(&std::fs::read_to_string(path)?)?;
    Ok(config)
}

pub fn save_config(path: &Path, config: &LantunConfig) -> color_eyre::Result<()> {
    std::fs::write(path, toml::ser::to_string(config)?)?;
    tracing::debug!("Config file saved to {:?}", path);
    Ok(())
}
