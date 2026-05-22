use serde::Deserialize;
use std::path::Path;
use tracing::{error, info};

#[derive(Debug, Clone, Deserialize)]
pub struct RouteConfig {
    pub host: String,
    pub backends: Vec<BackendConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LbConfig {
    pub server: ServerConfig,
    pub health: HealthConfig,
    pub routes: Vec<RouteConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub bind: String,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HealthConfig {
    pub interval_secs: u64,
    pub timeout_secs: u64,
    pub unhealthy_cooldown_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BackendConfig {
    pub url: String,
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    #[serde(default)]
    pub drain: bool,
}

fn default_max_connections() -> usize {
    5000
}

impl LbConfig {
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: LbConfig = toml::from_str(&content)?;
        Ok(config)
    }
}

/// Reload configuration from disk. Returns the new config or logs an error and returns None.
pub fn reload_config(path: &Path) -> Option<LbConfig> {
    match LbConfig::load(path) {
        Ok(config) => {
            info!(
                routes = config.routes.len(),
                bind = %config.server.bind,
                "Configuration reloaded successfully"
            );
            Some(config)
        }
        Err(e) => {
            error!(error = %e, "Failed to reload configuration");
            None
        }
    }
}
