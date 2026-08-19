use std::{net::SocketAddr, path::Path};

use iroh::SecretKey;
use serde::{Deserialize, Serialize};

use crate::TunnelProtocol;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub host_tunnels: Vec<HostEntry>,
    #[serde(default)]
    pub client_tunnels: Vec<ClientEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostEntry {
    pub name: String,
    pub local: SocketAddr,
    pub protocol: TunnelProtocol,
    /// Hex-encoded 32-byte secret key. Kept private on the host machine.
    pub secret_key: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl HostEntry {
    pub fn public_key_hex(&self) -> String {
        match hex::decode(&self.secret_key)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
        {
            Some(arr) => hex::encode(SecretKey::from_bytes(&arr).public()),
            None => "<invalid>".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientEntry {
    pub name: String,
    pub local: SocketAddr,
    pub protocol: TunnelProtocol,
    /// Hex-encoded public key of the host tunnel this client connects to.
    #[serde(alias = "public_key")]
    pub host_key: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

const fn yes() -> bool {
    true
}

pub fn load(path: &Path) -> color_eyre::Result<Config> {
    if !path.exists() {
        tracing::debug!("config file not found, creating at {}", path.display());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let empty = Config::default();
        std::fs::write(path, toml::ser::to_string(&empty)?)?;
        return Ok(empty);
    }
    let text = std::fs::read_to_string(path)?;
    let cfg: Config = toml::de::from_str(&text)?;
    Ok(cfg)
}

pub fn save(path: &Path, cfg: &Config) -> color_eyre::Result<()> {
    let text = toml::ser::to_string(cfg)?;
    std::fs::write(path, text)?;
    tracing::debug!("config written to {}", path.display());
    Ok(())
}

impl Config {
    pub fn name_taken(&self, name: &str) -> bool {
        self.host_tunnels.iter().any(|t| t.name == name)
            || self.client_tunnels.iter().any(|t| t.name == name)
    }
}
