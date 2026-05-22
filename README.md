# tinylb

**Tiny, hot-reloadable HTTP + WebSocket reverse proxy in Rust** — TLS termination, host-header routing, least-connections selection, and drain mode for zero-downtime rolling deploys. One static binary, one TOML config.

[![release](https://img.shields.io/github/v/release/shimondoodkin/tinylb)](https://github.com/shimondoodkin/tinylb/releases)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

## Features

- **HTTP and WebSocket in one path** — the same route handles both; WS frames are forwarded zero-copy.
- **Host-header routing** — multiple `[[routes]]` blocks let one tinylb fan out several public hostnames to different backend pools.
- **Least-connections selection** — every new connection goes to the healthy, non-draining backend with the fewest active connections.
- **Hot config reload** — `SIGHUP` or just save `lb.toml` (10-second mtime poll fallback works on Windows too). Backends can be added, removed, or marked draining without dropping a single connection.
- **TLS termination with hot cert reload** — rustls + `ArcSwap` for lock-free cert atomics. Let's Encrypt renewals just work.
- **Drain mode** — flip `drain = true` on a backend, reload; new connections stop, existing ones finish naturally. Perfect for rolling deploys.
- **Active health checks** — periodic HTTP GET against each backend's root; unhealthy backends are removed from rotation and re-checked on a cooldown.
- **Live stats** — JSON at `/_lb/stats`, HTML dashboard at `/_lb/` showing per-backend active connections and errors over the last 24h.
- **No external runtime deps** — single binary; no Redis, no etcd, no control plane.

## Install

### Download a release binary

Grab the latest from [Releases](https://github.com/shimondoodkin/tinylb/releases):

| File | When to use |
|---|---|
| `tinylb-linux-x86_64-gnu.tar.gz` | Debian/Ubuntu/RHEL — smaller, slightly faster (links glibc dynamically) |
| `tinylb-linux-x86_64-musl.tar.gz` | Alpine, scratch containers, or "I don't know the distro" — fully static |
| `tinylb-windows-x86_64.zip` | Windows |

### Build from source

```bash
git clone https://github.com/shimondoodkin/tinylb.git
cd tinylb
cargo build --release
./target/release/tinylb lb.toml
```

Requires Rust 1.74 or newer.

## Quickstart

Save this as `lb.toml`:

```toml
[server]
bind = "0.0.0.0:8080"

[health]
interval_secs = 10
timeout_secs = 3
unhealthy_cooldown_secs = 15

[[routes]]
host = "localhost"
[[routes.backends]]
url = "http://127.0.0.1:9001"
[[routes.backends]]
url = "http://127.0.0.1:9002"
```

Run:

```bash
./tinylb lb.toml
```

Open `http://localhost:8080/_lb/` for the stats dashboard.

## Configuration reference

### `[server]`

| Field | Required | Default | Description |
|---|---|---|---|
| `bind` | yes | — | TCP listen address, e.g. `0.0.0.0:8443` |
| `tls_cert` | no | — | Path to PEM certificate chain. Must be set together with `tls_key`. |
| `tls_key` | no | — | Path to PEM private key. |

### `[health]`

| Field | Default | Description |
|---|---|---|
| `interval_secs` | — | Seconds between health-check rounds |
| `timeout_secs` | — | HTTP timeout per health check |
| `unhealthy_cooldown_secs` | — | Seconds before re-checking an unhealthy backend |

### `[[routes]]`

| Field | Required | Description |
|---|---|---|
| `host` | yes | The HTTP `Host:` header value to match. Port is stripped before comparison. |
| `backends` | yes | One or more `[[routes.backends]]` blocks |

### `[[routes.backends]]`

| Field | Default | Description |
|---|---|---|
| `url` | required | Backend URL, e.g. `http://10.0.0.5:3000` (use `http://`; tinylb upgrades to WS when needed) |
| `max_connections` | `5000` | Hard cap on concurrent connections per backend |
| `drain` | `false` | If true, no new connections are routed here; existing connections finish naturally |

## Hot reload

tinylb reloads on either trigger:

- `SIGHUP` (Unix only): `kill -HUP $(pidof tinylb)` or `docker kill -s HUP <container>`
- File save: every 10 seconds tinylb checks the config file's mtime; if it changed, reload (works on Windows too)

**Reloads live (no connection drops):**
- Backend list (add/remove)
- `max_connections`
- `drain` flag
- TLS certificates (re-read from disk; new handshakes use the new cert immediately, existing TLS connections continue)

**Requires restart:**
- `server.bind`
- `server.tls_cert` / `tls_key` *paths* (the file contents reload; only changing the path itself needs a restart)

## Drain mode and rolling deploys

To replace a backend with zero downtime:

1. Set `drain = true` on the backend in `lb.toml`, send SIGHUP. New connections stop going there.
2. Wait for the backend's existing connections to finish. (Watch `/_lb/stats`, or poll a `/health` endpoint on the backend that reports queue depth.)
3. Restart/replace the backend process or container.
4. Wait for it to be healthy (tinylb's health check will flip it to healthy within `interval_secs`).
5. Set `drain = false`, send SIGHUP. Backend rejoins rotation.

`examples/rolling-update/` ships production-tested Python scripts that automate this.

## TLS

Set `tls_cert` and `tls_key` to PEM-encoded cert chain and private key paths:

```toml
[server]
bind = "0.0.0.0:443"
tls_cert = "/etc/letsencrypt/live/example.com/fullchain.pem"
tls_key  = "/etc/letsencrypt/live/example.com/privkey.pem"
```

Certs are reloaded atomically on every config reload — point certbot's `--deploy-hook` at `kill -HUP $(pidof tinylb)` and renewals never drop a connection.

See `examples/tls/self-signed/` for local dev and `examples/tls/letsencrypt/` for the production setup.

## Stats

- `GET /_lb/stats` — JSON snapshot: per-backend active connections and errors-per-minute over the last 24h.
- `GET /_lb/` — HTML dashboard with two live charts (Chart.js). Auto-refreshes once a minute.

## Architecture

```
Client ──HTTP/HTTPS/WSS──► tinylb ──HTTP/WS──► backends
                              │
                              ├─ rustls TLS termination (hot-reload via ArcSwap)
                              ├─ Host header → backend group lookup
                              ├─ Least-connections selection (atomic counters)
                              ├─ Active health checks (reqwest)
                              ├─ Drain mode (per-backend flag)
                              ├─ Config reload (SIGHUP + 10s mtime poll)
                              └─ Stats sampler → /_lb/stats and /_lb/
```

Source layout:

| File | Responsibility |
|---|---|
| `src/main.rs` | Server startup, TLS setup, accept loops, signal/reload handler, WS upgrade |
| `src/config.rs` | TOML parsing |
| `src/backends.rs` | Backend registry, health checks, least-conn selection, RAII connection counting |
| `src/proxy.rs` | Bidirectional WebSocket frame forwarding |
| `src/http_proxy.rs` | HTTP request forwarding with `X-Forwarded-For` preservation |
| `src/tls.rs` | Reloadable cert resolver (rustls + arc-swap) |
| `src/stats.rs` | Stats sampling loop + JSON endpoint + HTML dashboard |

## Examples

- `examples/docker-compose/simple/` — two backends + tinylb, plaintext.
- `examples/docker-compose/multi-host/` — one tinylb routing two virtual hosts to two backend pools.
- `examples/docker-compose/with-tls/` — TLS termination with mounted self-signed certs.
- `examples/rolling-update/` — Python scripts that drain → recreate → wait healthy → undrain. Real production patterns.
- `examples/tls/self-signed/` — generate a dev cert and run tinylb against it.
- `examples/tls/letsencrypt/` — certbot sidecar + SIGHUP renew-hook for production.

## License

MIT — see [LICENSE](LICENSE).
