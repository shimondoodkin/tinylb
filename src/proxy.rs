use std::sync::atomic::Ordering;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::{connect_async, WebSocketStream};
use tracing::{debug, error, info};

use crate::backends::{Backend, ConnectionGuard};

/// Proxy WebSocket frames between the client (already upgraded) and a backend.
///
/// This function:
/// 1. Connects to the backend WebSocket with X-Forwarded-For header
/// 2. Shovels frames in both directions using tokio::select!
/// 3. Uses a ConnectionGuard to track active connections via RAII
pub async fn proxy_websocket(
    client_upgraded: Upgraded,
    backend: Arc<Backend>,
    client_ip: String,
    existing_xff: Option<String>,
    path_and_query: String,
) {
    // RAII guard: increments on creation, decrements on drop
    let _guard = ConnectionGuard::new(Arc::clone(&backend));

    // Build the backend WebSocket URL from the backend's scheme+authority and the
    // ORIGINAL request path+query (mirrors http_proxy). Without this the request
    // path is lost and the backend is dialed at "/", which breaks any WS route
    // whose path isn't "/".
    let backend_uri: hyper::Uri = match backend.url.parse() {
        Ok(u) => u,
        Err(e) => {
            error!(backend = %backend.url, error = %e, "Invalid backend URL");
            backend.error_count.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    let ws_scheme = match backend_uri.scheme_str() {
        Some("https") | Some("wss") => "wss",
        _ => "ws",
    };
    let authority = match backend_uri.authority() {
        Some(a) => a.clone(),
        None => {
            error!(backend = %backend.url, "Backend URL has no authority");
            backend.error_count.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    let backend_url = format!("{}://{}{}", ws_scheme, authority, path_and_query);

    // Build the backend request with X-Forwarded-For
    let mut request = match backend_url.as_str().into_client_request() {
        Ok(r) => r,
        Err(e) => {
            error!(backend = %backend_url, error = %e, "Failed to build backend request");
            backend.error_count.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    // Append client IP to X-Forwarded-For chain
    let xff_value = match &existing_xff {
        Some(existing) => format!("{}, {}", existing, client_ip),
        None => client_ip.clone(),
    };
    request.headers_mut().insert(
        "X-Forwarded-For",
        xff_value.parse().unwrap_or_else(|_| "unknown".parse().unwrap()),
    );

    // Connect to backend with custom request
    let backend_ws = match connect_async(request).await {
        Ok((ws, _resp)) => ws,
        Err(e) => {
            error!(
                backend = %backend_url,
                error = %e,
                "Failed to connect to backend WebSocket"
            );
            backend.error_count.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    info!(
        client = %client_ip,
        backend = %backend_url,
        "WebSocket proxy session started"
    );

    // Wrap the client upgraded connection as a WebSocket
    let client_ws = WebSocketStream::from_raw_socket(
        TokioIo::new(client_upgraded),
        Role::Server,
        None,
    )
    .await;

    let (mut client_sink, mut client_stream) = client_ws.split();
    let (mut backend_sink, mut backend_stream) = backend_ws.split();

    // Shovel frames between client and backend
    let client_to_backend = async {
        while let Some(msg) = client_stream.next().await {
            match msg {
                Ok(frame) => {
                    if frame.is_close() {
                        debug!(client = %client_ip, "Client sent close frame");
                        let _ = backend_sink.close().await;
                        break;
                    }
                    if let Err(e) = backend_sink.send(frame).await {
                        debug!(
                            error = %e,
                            "Error forwarding frame to backend"
                        );
                        break;
                    }
                }
                Err(e) => {
                    debug!(error = %e, "Error reading from client");
                    break;
                }
            }
        }
    };

    let backend_to_client = async {
        while let Some(msg) = backend_stream.next().await {
            match msg {
                Ok(frame) => {
                    if frame.is_close() {
                        debug!(backend = %backend_url, "Backend sent close frame");
                        let _ = client_sink.close().await;
                        break;
                    }
                    if let Err(e) = client_sink.send(frame).await {
                        debug!(
                            error = %e,
                            "Error forwarding frame to client"
                        );
                        break;
                    }
                }
                Err(e) => {
                    debug!(error = %e, "Error reading from backend");
                    break;
                }
            }
        }
    };

    // Run both directions concurrently; when either finishes, the other is cancelled
    tokio::select! {
        _ = client_to_backend => {
            debug!(client = %client_ip, "Client-to-backend direction ended");
        }
        _ = backend_to_client => {
            debug!(client = %client_ip, "Backend-to-client direction ended");
        }
    }

    info!(
        client = %client_ip,
        backend = %backend_url,
        "WebSocket proxy session ended"
    );
}
