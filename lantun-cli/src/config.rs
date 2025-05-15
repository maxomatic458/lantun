use std::{net::SocketAddr, path::Path};

use lantun_core::{LanTun, TunnelProtocol};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LanTunConfig {
    pub host_tunnels: Vec<HostTunnelEntry>,
    pub client_tunnels: Vec<ClientTunnelEntry>,
}

impl LanTunConfig {
    pub async fn create_state(&self) -> color_eyre::Result<LanTun> {
        let mut lantun = LanTun::default();

        for tunnel in &self.host_tunnels {
            let secret: [u8; 32] = hex::decode(&tunnel.private_secret)?
                .try_into()
                .map_err(|_| color_eyre::Report::msg("Invalid private secret length"))?;
            lantun
                .host
                .create_tunnel(tunnel.local, tunnel.name.clone(), secret, tunnel.protocol)
                .await;
        }

        for tunnel in &self.client_tunnels {
            let secret: [u8; 32] = hex::decode(&tunnel.secret)?
                .try_into()
                .map_err(|_| color_eyre::Report::msg("Invalid public secret length"))?;
            lantun
                .client
                .create_tunnel(tunnel.local, tunnel.name.clone(), secret, tunnel.protocol)
                .await?;
        }

        Ok(lantun)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostTunnelEntry {
    pub name: String,
    pub local: SocketAddr,
    pub protocol: TunnelProtocol,
    pub private_secret: String,
    /// This is the secret that the client will use to connect to the tunnel.
    pub public_secret: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientTunnelEntry {
    pub name: String,
    pub local: SocketAddr,
    pub protocol: TunnelProtocol,
    /// The public secret of the host tunnel
    pub secret: String,
    pub enabled: bool,
}

pub fn load_config(path: &Path) -> color_eyre::Result<LanTunConfig> {
    if !path.exists() {
        tracing::debug!("Config file not found, creating a new one at {:?}", path);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, toml::ser::to_string(&LanTunConfig::default())?)?;
        return Ok(LanTunConfig::default());
    }

    let config: LanTunConfig = toml::de::from_str(&std::fs::read_to_string(path)?)?;
    Ok(config)
}

pub fn save_config(path: &Path, config: &LanTunConfig) -> color_eyre::Result<()> {
    std::fs::write(path, toml::ser::to_string(config)?)?;
    tracing::debug!("Config file saved to {:?}", path);
    Ok(())
}
