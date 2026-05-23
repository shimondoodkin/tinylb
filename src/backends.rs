use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::config::{BackendConfig, HealthConfig, RouteConfig};

/// A single backend instance.
pub struct Backend {
    pub url: String,
    pub max_connections: AtomicUsize,
    pub drain: AtomicBool,
    pub healthy: AtomicBool,
    pub active_connections: AtomicUsize,
    /// Errors since last stats sample. Incremented atomically, reset by stats loop.
    pub error_count: AtomicUsize,
    pub last_health_check: Mutex<Instant>,
}

impl Backend {
    pub fn new(config: &BackendConfig) -> Self {
        Self {
            url: config.url.clone(),
            max_connections: AtomicUsize::new(config.max_connections),
            drain: AtomicBool::new(config.drain),
            healthy: AtomicBool::new(true), // assume healthy until first check
            active_connections: AtomicUsize::new(0),
            error_count: AtomicUsize::new(0),
            last_health_check: Mutex::new(Instant::now()),
        }
    }

    /// HTTP health check URL. For WebSocket backends, converts ws:// to http://.
    /// For HTTP backends, returns the URL as-is.
    pub fn health_url(&self) -> String {
        if self.url.starts_with("ws://") || self.url.starts_with("wss://") {
            self.url
                .replace("ws://", "http://")
                .replace("wss://", "https://")
        } else {
            self.url.clone()
        }
    }
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend")
            .field("url", &self.url)
            .field("healthy", &self.healthy.load(Ordering::Relaxed))
            .field(
                "active_connections",
                &self.active_connections.load(Ordering::Relaxed),
            )
            .field("drain", &self.drain.load(Ordering::Relaxed))
            .finish()
    }
}

/// How an incoming Host header is matched against a route.
#[derive(Debug, Clone)]
pub enum HostMatch {
    /// Exact host match, stored lowercased.
    Exact(String),
    /// Wildcard suffix including the leading dot, stored lowercased.
    /// E.g. config host `*.example.com` → suffix `.example.com`.
    Wildcard { suffix: String },
}

impl HostMatch {
    /// Parse a `host` config value into a `HostMatch`.
    /// Leading `*.` becomes a wildcard; anything else is exact.
    /// Returns an error string for invalid wildcards.
    pub fn parse(host: &str) -> Result<Self, String> {
        if let Some(rest) = host.strip_prefix("*.") {
            if rest.is_empty() {
                return Err(format!("invalid wildcard host {:?}: suffix is empty", host));
            }
            if rest.contains('*') {
                return Err(format!(
                    "invalid wildcard host {:?}: only a single leading '*.' is allowed",
                    host
                ));
            }
            Ok(HostMatch::Wildcard {
                suffix: format!(".{}", rest).to_ascii_lowercase(),
            })
        } else if host.contains('*') {
            Err(format!(
                "invalid host {:?}: '*' is only allowed as a leading '*.' wildcard",
                host
            ))
        } else {
            Ok(HostMatch::Exact(host.to_ascii_lowercase()))
        }
    }
}

/// A group of backends for a specific host route.
pub struct BackendGroup {
    pub host: String,
    pub backends: RwLock<Vec<Arc<Backend>>>,
}

impl BackendGroup {
    pub fn new(route: &RouteConfig) -> Self {
        let backends = route
            .backends
            .iter()
            .map(|c| Arc::new(Backend::new(c)))
            .collect();
        Self {
            host: route.host.clone(),
            backends: RwLock::new(backends),
        }
    }

    /// Select the healthiest backend with the fewest active connections.
    pub fn select_backend(&self) -> Option<Arc<Backend>> {
        let backends = self.backends.read().unwrap();
        let mut best: Option<&Arc<Backend>> = None;
        let mut best_count = usize::MAX;

        for backend in backends.iter() {
            if backend.drain.load(Ordering::Relaxed) {
                continue;
            }
            if !backend.healthy.load(Ordering::Relaxed) {
                continue;
            }

            let active = backend.active_connections.load(Ordering::Relaxed);
            let max = backend.max_connections.load(Ordering::Relaxed);

            if active >= max {
                continue;
            }

            if active < best_count {
                best = Some(backend);
                best_count = active;
            }
        }

        best.cloned()
    }

    /// Apply new backend configs, preserving connection counts for existing backends.
    pub fn apply_backends(&self, configs: &[BackendConfig]) {
        let mut backends = self.backends.write().unwrap();

        let mut new_backends: Vec<Arc<Backend>> = Vec::with_capacity(configs.len());

        for config in configs {
            if let Some(existing) = backends.iter().find(|b| b.url == config.url) {
                existing
                    .max_connections
                    .store(config.max_connections, Ordering::Relaxed);
                existing.drain.store(config.drain, Ordering::Relaxed);
                new_backends.push(Arc::clone(existing));
                info!(
                    url = %config.url,
                    max_connections = config.max_connections,
                    drain = config.drain,
                    "Updated existing backend"
                );
            } else {
                let backend = Arc::new(Backend::new(config));
                info!(url = %config.url, "Added new backend");
                new_backends.push(backend);
            }
        }

        for old in backends.iter() {
            if !new_backends.iter().any(|b| b.url == old.url) {
                let active = old.active_connections.load(Ordering::Relaxed);
                warn!(
                    url = %old.url,
                    active_connections = active,
                    "Backend removed from config (existing connections will drain)"
                );
            }
        }

        *backends = new_backends;
    }
}

/// Registry holding all backend groups, keyed by host.
pub struct BackendRegistry {
    pub groups: RwLock<Vec<Arc<BackendGroup>>>,
}

impl BackendRegistry {
    pub fn new(routes: &[RouteConfig]) -> Self {
        let groups = routes
            .iter()
            .map(|r| Arc::new(BackendGroup::new(r)))
            .collect();
        Self {
            groups: RwLock::new(groups),
        }
    }

    /// Find a backend group by Host header value (strips port if present).
    pub fn find_group(&self, host: &str) -> Option<Arc<BackendGroup>> {
        let host = host.split(':').next().unwrap_or(host);
        let groups = self.groups.read().unwrap();
        groups.iter().find(|g| g.host == host).cloned()
    }

    /// Apply a new configuration with routes.
    pub fn apply_config(&self, routes: &[RouteConfig]) {
        let mut groups = self.groups.write().unwrap();

        let mut new_groups: Vec<Arc<BackendGroup>> = Vec::with_capacity(routes.len());

        for route in routes {
            if let Some(existing) = groups.iter().find(|g| g.host == route.host) {
                // Update backends within the existing group
                existing.apply_backends(&route.backends);
                new_groups.push(Arc::clone(existing));
            } else {
                let group = Arc::new(BackendGroup::new(route));
                info!(host = %route.host, "Added new route");
                new_groups.push(group);
            }
        }

        for old in groups.iter() {
            if !new_groups.iter().any(|g| g.host == old.host) {
                warn!(host = %old.host, "Route removed from config");
            }
        }

        *groups = new_groups;
    }
}

/// RAII guard that decrements the backend's active connection count on drop.
pub struct ConnectionGuard {
    backend: Arc<Backend>,
}

impl ConnectionGuard {
    pub fn new(backend: Arc<Backend>) -> Self {
        backend.active_connections.fetch_add(1, Ordering::Relaxed);
        debug!(
            url = %backend.url,
            active = backend.active_connections.load(Ordering::Relaxed),
            "Connection opened"
        );
        Self { backend }
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.backend
            .active_connections
            .fetch_sub(1, Ordering::Relaxed);
        debug!(
            url = %self.backend.url,
            active = self.backend.active_connections.load(Ordering::Relaxed),
            "Connection closed"
        );
    }
}

/// Run periodic health checks against all backends in all groups.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_exact_host() {
        let m = HostMatch::parse("example.com").unwrap();
        assert!(matches!(m, HostMatch::Exact(ref s) if s == "example.com"));
    }

    #[test]
    fn parse_exact_lowercases() {
        let m = HostMatch::parse("Example.COM").unwrap();
        assert!(matches!(m, HostMatch::Exact(ref s) if s == "example.com"));
    }

    #[test]
    fn parse_wildcard_stores_suffix_with_leading_dot() {
        let m = HostMatch::parse("*.example.com").unwrap();
        assert!(matches!(m, HostMatch::Wildcard { ref suffix } if suffix == ".example.com"));
    }

    #[test]
    fn parse_wildcard_lowercases_suffix() {
        let m = HostMatch::parse("*.Example.COM").unwrap();
        assert!(matches!(m, HostMatch::Wildcard { ref suffix } if suffix == ".example.com"));
    }

    #[test]
    fn parse_rejects_bare_star() {
        assert!(HostMatch::parse("*").is_err());
    }

    #[test]
    fn parse_rejects_star_dot_empty() {
        assert!(HostMatch::parse("*.").is_err());
    }

    #[test]
    fn parse_rejects_mid_label_wildcard() {
        assert!(HostMatch::parse("api.*.example.com").is_err());
    }

    #[test]
    fn parse_rejects_partial_label_wildcard() {
        assert!(HostMatch::parse("foo*.example.com").is_err());
    }
}

pub async fn health_check_loop(registry: Arc<BackendRegistry>, health_config: HealthConfig) {
    let interval = Duration::from_secs(health_config.interval_secs);
    let timeout = Duration::from_secs(health_config.timeout_secs);
    let cooldown = Duration::from_secs(health_config.unhealthy_cooldown_secs);

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .expect("Failed to build HTTP client for health checks");

    loop {
        tokio::time::sleep(interval).await;

        let groups = {
            let lock = registry.groups.read().unwrap();
            lock.clone()
        };

        for group in &groups {
            let backends = {
                let lock = group.backends.read().unwrap();
                lock.clone()
            };

            for backend in &backends {
                // If unhealthy, check cooldown before retrying
                if !backend.healthy.load(Ordering::Relaxed) {
                    let last_check = *backend.last_health_check.lock().unwrap();
                    if last_check.elapsed() < cooldown {
                        continue;
                    }
                }

                let url = backend.health_url();
                let result = client.get(&url).send().await;

                // Update last check time
                *backend.last_health_check.lock().unwrap() = Instant::now();

                match result {
                    Ok(resp) if resp.status().is_success() => {
                        if !backend.healthy.load(Ordering::Relaxed) {
                            info!(url = %backend.url, "Backend became healthy");
                        }
                        backend.healthy.store(true, Ordering::Relaxed);
                    }
                    Ok(resp) => {
                        let was_healthy = backend.healthy.swap(false, Ordering::Relaxed);
                        if was_healthy {
                            warn!(
                                url = %backend.url,
                                status = %resp.status(),
                                "Backend became unhealthy (bad status)"
                            );
                        }
                    }
                    Err(e) => {
                        let was_healthy = backend.healthy.swap(false, Ordering::Relaxed);
                        if was_healthy {
                            warn!(
                                url = %backend.url,
                                error = %e,
                                "Backend became unhealthy (request failed)"
                            );
                        }
                    }
                }
            }
        }
    }
}
