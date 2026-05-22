# screenmcp-lb — WebSocket Load Balancer

Rust WebSocket load balancer for ScreenMCP workers. Accepts client WebSocket connections and proxies them to backend worker instances using least-connections routing.

## Build & Run

```bash
cargo build --release
./target/release/screenmcp-lb              # uses ./lb.toml
./target/release/screenmcp-lb /path/to/lb.toml   # custom config path
```

## Configuration

Config file (`lb.toml`):

```toml
[server]
bind = "0.0.0.0:8443"
tls_cert = "/etc/letsencrypt/live/example.com/fullchain.pem"   # optional
tls_key = "/etc/letsencrypt/live/example.com/privkey.pem"      # optional

[health]
interval_secs = 10          # how often to health-check backends
timeout_secs = 3            # HTTP timeout per health check
unhealthy_cooldown_secs = 15  # wait before retrying unhealthy backend

[[backends]]
url = "ws://127.0.0.1:8080"
max_connections = 5000       # default: 5000
drain = false

[[backends]]
url = "ws://127.0.0.1:8081"
max_connections = 5000
drain = false
```

### Server

| Field | Required | Description |
|---|---|---|
| `bind` | yes | Listen address (e.g. `0.0.0.0:8443`) |
| `tls_cert` | no | Path to PEM certificate chain. Omit for plaintext |
| `tls_key` | no | Path to PEM private key. Must be set with `tls_cert` |

### Health

| Field | Description |
|---|---|
| `interval_secs` | Seconds between health check rounds |
| `timeout_secs` | HTTP request timeout per backend check |
| `unhealthy_cooldown_secs` | Seconds to wait before retrying an unhealthy backend |

### Backends

| Field | Default | Description |
|---|---|---|
| `url` | required | Backend worker WebSocket URL |
| `max_connections` | `5000` | Max concurrent connections to this backend |
| `drain` | `false` | If true, no new connections routed here; existing ones finish naturally |

## Features

### Least-Connections Routing

New WebSocket connections are routed to the healthy, non-draining backend with the fewest active connections that hasn't reached its `max_connections` limit. Connection counts are tracked with atomic counters and decremented automatically when connections close (RAII guard).

### TLS Termination

When `tls_cert` and `tls_key` are configured, the load balancer terminates TLS using rustls.

**Multi-threaded handshakes**: Each incoming TLS handshake runs as an independent tokio task. Since tokio's multi-threaded runtime distributes tasks across all worker threads, multiple handshakes proceed in parallel across CPU cores with no shared locks.

**Lock-free cert access**: The certificate resolver uses `ArcSwap` for lock-free atomic reads. Concurrent TLS handshakes on different threads never block each other or a cert reload operation. Reading the current cert is a single atomic pointer load.

Without TLS config, the LB runs in plaintext mode (backward compatible).

### Hot Reload (SIGHUP)

Send `SIGHUP` to reload `lb.toml` and TLS certificates without dropping existing connections:

```bash
kill -HUP $(pidof screenmcp-lb)
```

What gets reloaded:
- **Backends**: new backends added, removed backends drain naturally, existing backends update `max_connections` and `drain` settings while preserving connection counts
- **TLS certificates**: new cert/key loaded from disk and swapped in atomically. New handshakes use updated certs immediately. Existing connections are unaffected. If the new cert fails to load, old cert remains active
- **Bind address**: changes require a restart

### Health Checks

Periodic HTTP `GET` to each backend's root URL (e.g. `http://127.0.0.1:8080/`). Workers already respond with HTTP 200 at `/`.

- Healthy backend that fails a check → marked unhealthy, logged
- Unhealthy backend → retried after `unhealthy_cooldown_secs`
- Backend recovers → marked healthy, logged

### Drain Mode

Set `drain = true` on a backend to stop routing new connections to it. Existing connections continue until they close naturally. Useful for rolling deploys:

1. Set `drain = true` on the backend being updated
2. `kill -HUP` the LB
3. Wait for existing connections to drain
4. Restart the backend worker
5. Set `drain = false`, `kill -HUP` again

### Transparent WebSocket Proxy

The LB performs zero-copy frame forwarding between client and backend. It does not inspect, modify, or buffer WebSocket messages. Both directions run concurrently via `tokio::select!`; when either side closes, the other is closed.

## Logging

Uses `tracing` with `RUST_LOG` env var:

```bash
RUST_LOG=screenmcp_lb=info ./target/release/screenmcp-lb    # default
RUST_LOG=screenmcp_lb=debug ./target/release/screenmcp-lb   # connection-level logs
RUST_LOG=screenmcp_lb=trace ./target/release/screenmcp-lb   # frame-level logs
```

Log events:
- `info`: startup, backend routing decisions, health status changes, config/cert reloads
- `debug`: individual connection open/close, TLS handshake failures, frame forwarding errors
- `warn`: no healthy backend available, backend removed from config
- `error`: TCP accept failures, WebSocket upgrade failures, config load errors

## Architecture

```
Client ──WSS──→ [TLS termination] ──→ HTTP upgrade ──→ select backend
                                                          │
                                          ┌───────────────┼───────────────┐
                                          ▼               ▼               ▼
                                     ws://worker:8080  ws://worker:8081  ...
                                          │
                                     proxy_websocket()
                                     ├── client → backend (tokio::select!)
                                     └── backend → client
```

## Files

| File | Description |
|---|---|
| `src/main.rs` | Server startup, TLS setup, accept loops, SIGHUP handler, WebSocket upgrade |
| `src/config.rs` | TOML config parsing and hot reload |
| `src/backends.rs` | Backend registry, least-connections selection, health checks, connection guard |
| `src/proxy.rs` | Bidirectional WebSocket frame forwarding |
| `src/tls.rs` | Reloadable TLS cert resolver (ArcSwap + rustls) |
| `lb.toml` | Default configuration file |
