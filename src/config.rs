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
    #[serde(default)]
    pub log: LogConfig,
}

/// `[log]` section. Everything is optional.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    /// Log file path. Unset = log to stdout. Restart required to change.
    pub file: Option<String>,
    /// Rotate the log file when it reaches this size (MB).
    pub max_size_mb: u64,
    /// Number of rotated files to keep (tinylb.log.1 .. .N).
    pub max_files: u32,
    /// Repeated per-request errors (backend down, connect failures) are
    /// logged once per this many seconds with a suppressed count.
    /// 0 = log every error. Hot-reloadable.
    pub error_summary_secs: u64,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            file: None,
            max_size_mb: 50,
            max_files: 3,
            error_summary_secs: crate::throttle::DEFAULT_WINDOW_SECS,
        }
    }
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
