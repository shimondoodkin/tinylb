use std::io::{self, BufReader};
use std::path::Path;
use std::sync::Arc;

use arc_swap::ArcSwap;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::ServerConfig;
use tracing::{error, info};

/// Hot-reloadable TLS certificate resolver.
///
/// Uses `ArcSwap` for lock-free reads — TLS handshakes on any thread
/// read the current cert without blocking. Cert reloads are a single
/// atomic pointer swap.
pub struct ReloadableCertResolver {
    certified_key: ArcSwap<Option<Arc<CertifiedKey>>>,
}

impl std::fmt::Debug for ReloadableCertResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReloadableCertResolver")
            .field("has_cert", &self.certified_key.load().is_some())
            .finish()
    }
}

impl ReloadableCertResolver {
    /// Create a new resolver with the given initial cert/key.
    pub fn new(key: Arc<CertifiedKey>) -> Self {
        Self {
            certified_key: ArcSwap::new(Arc::new(Some(key))),
        }
    }

    /// Atomically swap in a new cert/key. Existing connections are unaffected;
    /// new TLS handshakes will use the updated cert immediately.
    pub fn reload(&self, key: Arc<CertifiedKey>) {
        self.certified_key.store(Arc::new(Some(key)));
        info!("TLS certificates reloaded");
    }
}

impl ResolvesServerCert for ReloadableCertResolver {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        // Lock-free load via ArcSwap — safe to call from any thread concurrently
        let guard = self.certified_key.load();
        (**guard).clone()
    }
}

/// Load a certificate chain and private key from PEM files.
pub fn load_certified_key(
    cert_path: &Path,
    key_path: &Path,
) -> io::Result<Arc<CertifiedKey>> {
    // Read certificate chain
    let cert_file = std::fs::File::open(cert_path).map_err(|e| {
        io::Error::new(e.kind(), format!("failed to open cert {}: {}", cert_path.display(), e))
    })?;
    let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(cert_file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad cert PEM: {}", e)))?;

    if certs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "no certificates found in PEM file",
        ));
    }

    // Read private key
    let key_file = std::fs::File::open(key_path).map_err(|e| {
        io::Error::new(e.kind(), format!("failed to open key {}: {}", key_path.display(), e))
    })?;
    let key = rustls_pemfile::private_key(&mut BufReader::new(key_file))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad key PEM: {}", e)))?
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "no private key found in PEM file")
        })?;

    let signing_key = rustls::crypto::ring::sign::any_supported_type(&key).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("unsupported key type: {}", e))
    })?;

    Ok(Arc::new(CertifiedKey::new(certs, signing_key)))
}

/// Build a rustls ServerConfig using the reloadable resolver.
///
/// The resolver is shared — call `resolver.reload()` to swap certs
/// without rebuilding the ServerConfig or dropping connections.
pub fn build_server_config(resolver: Arc<ReloadableCertResolver>) -> Arc<ServerConfig> {
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(resolver);

    // Enable ALPN for HTTP/1.1 (WebSocket runs over HTTP/1.1)
    config.alpn_protocols = vec![b"http/1.1".to_vec()];

    Arc::new(config)
}

/// Try to reload certs from the given paths. Logs and returns Ok/Err.
pub fn try_reload_certs(
    resolver: &ReloadableCertResolver,
    cert_path: &Path,
    key_path: &Path,
) -> io::Result<()> {
    match load_certified_key(cert_path, key_path) {
        Ok(key) => {
            resolver.reload(key);
            Ok(())
        }
        Err(e) => {
            error!(
                cert = %cert_path.display(),
                key = %key_path.display(),
                error = %e,
                "Failed to reload TLS certificates, keeping old ones"
            );
            Err(e)
        }
    }
}
