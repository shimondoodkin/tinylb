use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use std::sync::atomic::Ordering;

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt};
use hyper::body::{Body, Frame, Incoming};
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::Client;
use tracing::{debug, error};

use crate::backends::{Backend, ConnectionGuard};

/// Hop-by-hop headers that must not be forwarded (lowercase).
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// A response body wrapper that holds a ConnectionGuard alive for the lifetime
/// of the body stream. This is critical for SSE/long-lived streaming connections
/// where the response body outlives the proxy_http function call.
struct GuardedBody {
    inner: Incoming,
    _guard: ConnectionGuard,
}

impl Body for GuardedBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        // SAFETY: we only project to `inner` which is Unpin (Incoming is Unpin)
        let inner = unsafe { self.map_unchecked_mut(|s| &mut s.inner) };
        inner.poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

/// Proxy an HTTP request to a backend, streaming the response back.
/// The connection guard lives inside the response body, so long-lived
/// streams (SSE) keep the connection counted until the stream ends.
pub async fn proxy_http(
    req: Request<Incoming>,
    backend: Arc<Backend>,
    client: Client<hyper_util::client::legacy::connect::HttpConnector, Incoming>,
    peer_ip: &str,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    let guard = ConnectionGuard::new(Arc::clone(&backend));

    // Parse backend URL to extract scheme + authority
    let backend_uri: hyper::Uri = match backend.url.parse() {
        Ok(u) => u,
        Err(e) => {
            error!(backend = %backend.url, error = %e, "Invalid backend URL");
            backend.error_count.fetch_add(1, Ordering::Relaxed);
            let resp = Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(empty_body())
                .unwrap();
            return Ok(resp);
        }
    };

    let scheme = backend_uri.scheme_str().unwrap_or("http");
    let authority = match backend_uri.authority() {
        Some(a) => a.clone(),
        None => {
            error!(backend = %backend.url, "Backend URL has no authority");
            backend.error_count.fetch_add(1, Ordering::Relaxed);
            let resp = Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(empty_body())
                .unwrap();
            return Ok(resp);
        }
    };

    // Build the proxied URI: backend scheme+authority + original path+query
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let new_uri: hyper::Uri = format!("{}://{}{}", scheme, authority, path_and_query)
        .parse()
        .unwrap();

    // Build the forwarded request
    let (mut parts, body) = req.into_parts();
    parts.uri = new_uri;

    // Set forwarding headers
    let host_value = parts
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Build X-Forwarded-For
    let xff = match parts.headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        Some(existing) => format!("{}, {}", existing, peer_ip),
        None => peer_ip.to_string(),
    };

    // Remove hop-by-hop headers
    for header in HOP_BY_HOP {
        parts.headers.remove(*header);
    }

    parts
        .headers
        .insert("x-forwarded-for", xff.parse().unwrap());
    parts
        .headers
        .insert("x-forwarded-host", host_value.parse().unwrap_or_else(|_| "".parse().unwrap()));
    parts
        .headers
        .insert("x-forwarded-proto", "https".parse().unwrap());
    // Set Host to the backend authority
    parts
        .headers
        .insert("host", authority.as_str().parse().unwrap());

    let proxy_req = Request::from_parts(parts, body);

    debug!(
        backend = %backend.url,
        "Proxying HTTP request"
    );

    match client.request(proxy_req).await {
        Ok(resp) => {
            let (parts, body) = resp.into_parts();

            // Wrap body with the connection guard so it stays alive
            // for the entire stream duration (critical for SSE)
            let guarded = GuardedBody {
                inner: body,
                _guard: guard,
            };
            let boxed = guarded.boxed();

            let mut resp = Response::from_parts(parts, boxed);

            // Remove hop-by-hop headers from response
            for header in HOP_BY_HOP {
                resp.headers_mut().remove(*header);
            }

            Ok(resp)
        }
        Err(e) => {
            error!(
                backend = %backend.url,
                error = %e,
                "Failed to proxy request to backend"
            );
            backend.error_count.fetch_add(1, Ordering::Relaxed);
            let resp = Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(empty_body())
                .unwrap();
            Ok(resp)
        }
    }
}

fn empty_body() -> BoxBody<Bytes, hyper::Error> {
    http_body_util::Empty::new()
        .map_err(|never| match never {})
        .boxed()
}
