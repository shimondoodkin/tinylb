mod backends;
mod config;
mod http_proxy;
mod proxy;
mod stats;
mod tls;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error, info, warn};

use backends::BackendRegistry;
use config::LbConfig;
use stats::Stats;
use tls::ReloadableCertResolver;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tinylb=info".into()),
        )
        .init();

    // Install the default rustls crypto provider (ring)
    let _ = rustls::crypto::ring::default_provider().install_default();

    let config_path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("lb.toml"));

    let initial_config = match LbConfig::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            error!(path = %config_path.display(), error = %e, "Failed to load config");
            std::process::exit(1);
        }
    };

    info!(
        bind = %initial_config.server.bind,
        routes = initial_config.routes.len(),
        tls = initial_config.server.tls_cert.is_some(),
        "Starting tinylb"
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime");

    rt.block_on(run(initial_config, config_path));
}

async fn run(initial_config: LbConfig, config_path: PathBuf) {
    let registry = Arc::new(
        BackendRegistry::new(&initial_config.routes)
            .expect("Invalid route configuration at startup"),
    );

    let (health_tx, _health_rx) = watch::channel(initial_config.health.clone());

    // Spawn health check loop
    let health_registry = Arc::clone(&registry);
    let health_config = initial_config.health.clone();
    tokio::spawn(async move {
        backends::health_check_loop(health_registry, health_config).await;
    });

    // Spawn stats sampling loop
    let stats = Arc::new(Stats::new());
    let stats_registry = Arc::clone(&registry);
    let stats_clone = Arc::clone(&stats);
    tokio::spawn(async move {
        stats::sample_loop(stats_registry, stats_clone).await;
    });

    // Build shared HTTP client for proxying (connection pooling)
    let http_client: Client<hyper_util::client::legacy::connect::HttpConnector, Incoming> =
        Client::builder(TokioExecutor::new()).build_http();

    // Build TLS acceptor if configured
    let (tls_acceptor, tls_resolver) = match (
        &initial_config.server.tls_cert,
        &initial_config.server.tls_key,
    ) {
        (Some(cert_path), Some(key_path)) => {
            let cert = PathBuf::from(cert_path);
            let key = PathBuf::from(key_path);

            let certified_key = tls::load_certified_key(&cert, &key)
                .expect("Failed to load initial TLS certificates");

            let resolver = Arc::new(ReloadableCertResolver::new(certified_key));
            let server_config = tls::build_server_config(resolver.clone());
            let acceptor = TlsAcceptor::from(server_config);

            info!(cert = %cert_path, key = %key_path, "TLS enabled");
            (Some(acceptor), Some(resolver))
        }
        (None, None) => {
            info!("TLS disabled (no tls_cert/tls_key in config)");
            (None, None)
        }
        _ => {
            error!("Both tls_cert and tls_key must be set together");
            std::process::exit(1);
        }
    };

    // Spawn config reload handler (polls every 10s + SIGHUP)
    let reload_registry = Arc::clone(&registry);
    let reload_path = config_path.clone();
    let reload_resolver = tls_resolver.clone();
    tokio::spawn(async move {
        config_reload_loop(&reload_path, &reload_registry, &health_tx, reload_resolver.as_deref()).await;
    });

    // Bind TCP listener
    let addr: SocketAddr = initial_config
        .server
        .bind
        .parse()
        .expect("Invalid bind address");

    let listener = TcpListener::bind(addr)
        .await
        .expect("Failed to bind TCP listener");

    info!(addr = %addr, "Listening for connections");

    // Accept loop
    match tls_acceptor {
        Some(acceptor) => accept_loop_tls(listener, acceptor, registry, http_client, stats).await,
        None => accept_loop_plain(listener, registry, http_client, stats).await,
    }
}

/// Accept loop with TLS termination.
async fn accept_loop_tls(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    registry: Arc<BackendRegistry>,
    http_client: Client<hyper_util::client::legacy::connect::HttpConnector, Incoming>,
    stats: Arc<Stats>,
) {
    loop {
        let (tcp_stream, peer_addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                error!(error = %e, "Failed to accept TCP connection");
                continue;
            }
        };

        let acceptor = acceptor.clone();
        let registry = Arc::clone(&registry);
        let http_client = http_client.clone();
        let stats = Arc::clone(&stats);
        let peer = peer_addr.to_string();

        tokio::spawn(async move {
            let tls_stream = match acceptor.accept(tcp_stream).await {
                Ok(s) => s,
                Err(e) => {
                    debug!(peer = %peer, error = %e, "TLS handshake failed");
                    return;
                }
            };

            let io = TokioIo::new(tls_stream);
            let registry = Arc::clone(&registry);
            let http_client = http_client.clone();
            let stats = Arc::clone(&stats);
            let peer = peer.clone();

            let service = service_fn(move |req: Request<Incoming>| {
                let registry = Arc::clone(&registry);
                let http_client = http_client.clone();
                let stats = Arc::clone(&stats);
                let peer = peer.clone();
                async move { handle_request(req, &registry, &peer, http_client, &stats).await }
            });

            if let Err(e) = http1::Builder::new()
                .serve_connection(io, service)
                .with_upgrades()
                .await
            {
                let msg = e.to_string();
                if !msg.contains("early eof")
                    && !msg.contains("connection reset")
                    && !msg.contains("broken pipe")
                {
                    error!(error = %e, "HTTP connection error");
                }
            }
        });
    }
}

/// Accept loop without TLS (plaintext).
async fn accept_loop_plain(
    listener: TcpListener,
    registry: Arc<BackendRegistry>,
    http_client: Client<hyper_util::client::legacy::connect::HttpConnector, Incoming>,
    stats: Arc<Stats>,
) {
    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                error!(error = %e, "Failed to accept TCP connection");
                continue;
            }
        };

        let registry = Arc::clone(&registry);
        let http_client = http_client.clone();
        let stats = Arc::clone(&stats);
        let peer = peer_addr.to_string();

        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let registry = Arc::clone(&registry);
            let http_client = http_client.clone();
            let stats = Arc::clone(&stats);
            let peer = peer.clone();

            let service = service_fn(move |req: Request<Incoming>| {
                let registry = Arc::clone(&registry);
                let http_client = http_client.clone();
                let stats = Arc::clone(&stats);
                let peer = peer.clone();
                async move { handle_request(req, &registry, &peer, http_client, &stats).await }
            });

            if let Err(e) = http1::Builder::new()
                .serve_connection(io, service)
                .with_upgrades()
                .await
            {
                let msg = e.to_string();
                if !msg.contains("early eof") && !msg.contains("connection reset") {
                    error!(error = %e, "HTTP connection error");
                }
            }
        });
    }
}

/// Handle an incoming HTTP request. Routes by Host header to the appropriate backend group.
async fn handle_request(
    mut req: Request<Incoming>,
    registry: &BackendRegistry,
    peer: &str,
    http_client: Client<hyper_util::client::legacy::connect::HttpConnector, Incoming>,
    stats: &Stats,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    // Stats endpoints
    let path = req.uri().path();
    if path == "/_lb/stats" {
        let json = stats.to_json();
        let resp = Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(http_body_util::Full::new(Bytes::from(json)).map_err(|never| match never {}).boxed())
            .unwrap();
        return Ok(resp);
    }
    if path == "/_lb/" || path == "/_lb" {
        let html = stats::dashboard_html();
        let resp = Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(http_body_util::Full::new(Bytes::from(html)).map_err(|never| match never {}).boxed())
            .unwrap();
        return Ok(resp);
    }

    // Extract host from Host header
    let host = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let group = match registry.find_group(&host) {
        Some(g) => g,
        None => {
            debug!(host = %host, "No route for host");
            let resp = Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(empty_body())
                .unwrap();
            return Ok(resp);
        }
    };

    let backend = match group.select_backend() {
        Some(b) => b,
        None => {
            warn!(client = %peer, host = %host, "No healthy backend available");
            let resp = Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(empty_body())
                .unwrap();
            return Ok(resp);
        }
    };

    // WebSocket upgrade or plain HTTP — every route handles both
    if is_websocket_upgrade(&req) {
        info!(
            client = %peer,
            backend = %backend.url,
            active = backend.active_connections.load(std::sync::atomic::Ordering::Relaxed),
            "Routing WebSocket connection"
        );

        let sec_websocket_key = req
            .headers()
            .get("Sec-WebSocket-Key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let accept_key = compute_accept_key(&sec_websocket_key);

        let existing_xff = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        // Preserve the request path+query for the backend WebSocket dial.
        let path_and_query = req
            .uri()
            .path_and_query()
            .map(|pq| pq.as_str().to_string())
            .unwrap_or_else(|| "/".to_string());

        let mut response_builder = Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header("Sec-WebSocket-Accept", accept_key);

        if let Some(protocol) = req.headers().get("Sec-WebSocket-Protocol") {
            response_builder =
                response_builder.header("Sec-WebSocket-Protocol", protocol.clone());
        }

        let response = response_builder.body(empty_body()).unwrap();

        let client_ip = peer.to_string();
        tokio::spawn(async move {
            match hyper::upgrade::on(&mut req).await {
                Ok(upgraded) => {
                    proxy::proxy_websocket(upgraded, backend, client_ip, existing_xff, path_and_query).await;
                }
                Err(e) => {
                    error!(error = %e, "WebSocket upgrade failed");
                }
            }
        });

        Ok(response)
    } else {
        http_proxy::proxy_http(req, backend, http_client, peer).await
    }
}

fn empty_body() -> BoxBody<Bytes, hyper::Error> {
    Empty::new().map_err(|never| match never {}).boxed()
}

fn is_websocket_upgrade(req: &Request<Incoming>) -> bool {
    let has_upgrade = req
        .headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);

    let has_connection_upgrade = req
        .headers()
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
        })
        .unwrap_or(false);

    has_upgrade && has_connection_upgrade
}

/// Compute the Sec-WebSocket-Accept header value per RFC 6455.
fn compute_accept_key(key: &str) -> String {
    let magic = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(magic.as_bytes());
    let hash = hasher.finalize();
    base64_encode(&hash)
}

// Minimal SHA-1 implementation (RFC 3174)
struct Sha1 {
    state: [u32; 5],
    buffer: Vec<u8>,
    total_len: u64,
}

impl Sha1 {
    fn new() -> Self {
        Self {
            state: [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0],
            buffer: Vec::new(),
            total_len: 0,
        }
    }

    fn update(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
        self.total_len += data.len() as u64;
    }

    fn finalize(mut self) -> [u8; 20] {
        let bit_len = self.total_len * 8;
        self.buffer.push(0x80);
        while self.buffer.len() % 64 != 56 {
            self.buffer.push(0);
        }
        self.buffer.extend_from_slice(&bit_len.to_be_bytes());

        let buffer = std::mem::take(&mut self.buffer);
        for block in buffer.chunks_exact(64) {
            self.process_block(block);
        }

        let mut result = [0u8; 20];
        for (i, &s) in self.state.iter().enumerate() {
            result[i * 4..i * 4 + 4].copy_from_slice(&s.to_be_bytes());
        }
        result
    }

    fn process_block(&mut self, block: &[u8]) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = self.state;

        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };

            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
    }
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    let chunks = data.chunks(3);
    for chunk in chunks {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

/// Future that resolves whenever a SIGHUP arrives on Unix.
/// On non-Unix targets this returns a future that never resolves, so the
/// `tokio::select!` in the reload loop effectively only watches the file
/// mtime poll on those platforms.
#[cfg(unix)]
async fn wait_sighup() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut s = match signal(SignalKind::hangup()) {
        Ok(s) => s,
        Err(_) => {
            std::future::pending::<()>().await;
            return;
        }
    };
    s.recv().await;
}

#[cfg(not(unix))]
async fn wait_sighup() {
    std::future::pending::<()>().await;
}

/// Reload config when triggered by SIGHUP or when the config file changes (polled every 10s).
async fn config_reload_loop(
    config_path: &Path,
    registry: &BackendRegistry,
    _health_tx: &watch::Sender<config::HealthConfig>,
    tls_resolver: Option<&ReloadableCertResolver>,
) {
    // Track last known modification time
    let mut last_modified = std::fs::metadata(config_path)
        .and_then(|m| m.modified())
        .ok();

    let mut poll_interval = tokio::time::interval(std::time::Duration::from_secs(10));
    poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let reason = tokio::select! {
            _ = poll_interval.tick() => {
                // Check if file modification time changed
                let current_modified = std::fs::metadata(config_path)
                    .and_then(|m| m.modified())
                    .ok();
                if current_modified == last_modified {
                    continue;
                }
                last_modified = current_modified;
                "config file changed"
            }
            _ = wait_sighup() => {
                // Update last_modified so we don't double-reload on next poll tick
                last_modified = std::fs::metadata(config_path)
                    .and_then(|m| m.modified())
                    .ok();
                "SIGHUP"
            }
        };

        info!("Reloading configuration ({})", reason);

        if let Some(new_config) = config::reload_config(config_path) {
            if let Err(e) = registry.apply_config(&new_config.routes) {
                error!(error = %e, "Failed to apply new route configuration; keeping previous");
            }

            // Reload TLS certs if configured
            if let Some(resolver) = tls_resolver {
                if let (Some(cert_path), Some(key_path)) = (
                    &new_config.server.tls_cert,
                    &new_config.server.tls_key,
                ) {
                    let _ = tls::try_reload_certs(
                        resolver,
                        Path::new(cert_path),
                        Path::new(key_path),
                    );
                }
            }
        }
    }
}
