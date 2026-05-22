# tinylb — Open-Source Release Design

**Date:** 2026-05-22
**Author:** Shimon Doodkin
**Status:** Approved for implementation

## Goal

Extract the Rust load balancer currently embedded in `snapapi/lb` and `screenmcp-cloud/lb` (and soon `gemini-live-server`) into a standalone open-source project published on GitHub as **tinylb**.

The source has been running in production for months in three projects. The work is *packaging*, not new development: rename, document, add examples, add a tagged-release CI workflow.

## Non-Goals (v0.1.0)

- No new load balancer features
- No tests (source is battle-tested in prod; tests are a v0.2 concern)
- No CONTRIBUTING / issue templates / GitHub Pages docs (add when there's real interest)
- No Cargo workspace, no library crate — single binary crate only
- No package-manager distribution (crates.io publish, Homebrew tap, AUR) — out of scope for v0.1.0; can follow once releases stabilize

## Project Identity

| Field | Value |
|---|---|
| Project name | `tinylb` |
| GitHub repo | `github.com/shimondoodkin/tinylb` |
| Cargo crate name | `tinylb` |
| Binary name | `tinylb` |
| License | MIT |
| Dev location | `/home/user/lb` |

## Architecture (unchanged from source)

```
Client ──HTTP/HTTPS/WSS──► tinylb ──HTTP/WS──► backends
                              │
                              ├─ TLS termination (rustls, hot-reloadable certs)
                              ├─ Host-header routing → backend group
                              ├─ Least-connections selection
                              ├─ Health checks (HTTP GET per backend)
                              ├─ Drain mode (zero-downtime rolling deploys)
                              ├─ Hot config reload (SIGHUP + 10s mtime poll)
                              └─ Stats endpoint /_lb/stats + dashboard /_lb/
```

Existing modules in `src/`:

| File | Responsibility |
|---|---|
| `main.rs` | Server startup, TLS setup, accept loops, signal/reload handler, WS upgrade, SHA-1/base64 inline impl |
| `config.rs` | TOML config parsing |
| `backends.rs` | Backend registry, health-check loop, least-connections selection, RAII connection guard |
| `proxy.rs` | Bidirectional WebSocket frame forwarding |
| `http_proxy.rs` | HTTP request forwarding with XFF header preservation |
| `tls.rs` | Reloadable cert resolver (rustls + ArcSwap) |
| `stats.rs` | Stats sampling loop, JSON endpoint, HTML dashboard |

## Repo Layout

```
tinylb/
├── Cargo.toml                        # renamed package
├── Cargo.lock
├── LICENSE                           # MIT
├── README.md                         # rewritten from scratch
├── CHANGELOG.md                      # v0.1.0 entry
├── .gitignore                        # /target, *.swp, .DS_Store
├── lb.toml                           # commented example exposing every option
├── src/                              # patched (see Source Patches)
│   ├── main.rs
│   ├── config.rs
│   ├── backends.rs
│   ├── proxy.rs
│   ├── http_proxy.rs
│   ├── stats.rs
│   └── tls.rs
├── examples/
│   ├── docker-compose/
│   │   ├── simple/                   # 2 backends + LB, plaintext
│   │   ├── multi-host/               # LB routing 2 hosts to 2 backend pools
│   │   └── with-tls/                 # LB with mounted certs
│   ├── rolling-update/
│   │   ├── README.md                 # when to use which pattern
│   │   ├── rolling_update_lib.py
│   │   ├── simple.py                 # single-service rolling deploy
│   │   ├── restart_api.py            # production example from screenmcp-cloud
│   │   ├── restart_worker.py
│   │   └── restart_web.py
│   └── tls/
│       ├── self-signed/              # dev cert via openssl one-liner + lb.toml + README
│       └── letsencrypt/              # certbot sidecar + SIGHUP renew hook + compose + README
├── docs/
│   └── superpowers/specs/            # this file
└── .github/
    └── workflows/
        └── release.yml               # triggers on v* tag
```

## Source Patches

1. **`Cargo.toml`** — rename package `screenmcp-lb` → `tinylb`; add `description`, `license = "MIT"`, `repository`, `keywords`, `categories = ["network-programming", "web-programming::http-server"]`; set initial `version = "0.1.0"`.

2. **`src/main.rs`** — default `RUST_LOG` filter `screenmcp_lb=info` → `tinylb=info`; startup log message `"Starting screenmcp load balancer"` → `"Starting tinylb"`.

3. **`src/main.rs`** — gate the SIGHUP handler so the Windows target compiles. On Windows, hot-reload happens via the existing 10s file-mtime poll only. Implementation approach: extract a small helper `sighup_signal()` that returns a future. On Unix it wraps `tokio::signal::unix::signal(SignalKind::hangup())` and resolves when SIGHUP arrives. On Windows it returns `std::future::pending()` (never resolves). The `tokio::select!` in `config_reload_loop` calls this helper, so the source compiles unchanged on both platforms. The exact code shape is for the implementation plan to decide — the requirement here is "Windows compiles; on Unix SIGHUP still triggers reload; on Windows reload still works via file-mtime poll."

4. **`src/stats.rs`** — scan `dashboard_html()` for any ScreenMCP-specific branding (title, headings); replace with tinylb-neutral copy.

5. **Imports** — remove `tokio::signal::unix::{signal, SignalKind}` from `main.rs` top-level imports; reference fully-qualified inside `cfg(unix)` block instead, so Windows build doesn't break on the unused import.

## Examples

### `examples/docker-compose/simple/`

Two `whoami`-style backends behind one tinylb. Plaintext. Used to verify basic routing.

Files: `docker-compose.yml`, `lb.toml`, `README.md`.

### `examples/docker-compose/multi-host/`

Two host routes (`a.local`, `b.local`), each backed by two backend containers. Demonstrates `[[routes]]` with multiple hosts.

Files: `docker-compose.yml`, `lb.toml`, `README.md` (includes `/etc/hosts` snippet for testing).

### `examples/docker-compose/with-tls/`

Single host, TLS termination via mounted cert + key. Uses a self-signed cert generated by a helper script.

Files: `docker-compose.yml`, `lb.toml`, `generate-cert.sh`, `README.md`.

### `examples/rolling-update/`

Real-world Python scripts from production. Each script is generalized (paths, container names parameterized via constants at top), but the structure is preserved:

| File | Source | Use case |
|---|---|---|
| `rolling_update_lib.py` | screenmcp-cloud `rolling_restart_lib.py` | Shared functions: drain, wait healthy, undrain, SIGHUP LB |
| `simple.py` | snapapi `rolling-update.py` | Single service with 2 replicas |
| `restart_api.py` | screenmcp-cloud | API service example |
| `restart_worker.py` | screenmcp-cloud | Background worker example |
| `restart_web.py` | screenmcp-cloud | Web/nginx frontend example |

`README.md` explains: prerequisites, what each script does, how to adapt to your stack, "the rolling-update playbook" (drain → recreate → wait healthy → undrain).

### `examples/tls/self-signed/`

For local dev. `generate-cert.sh` runs `openssl req -x509 ...` to produce a cert. `lb.toml` points at the resulting files.

### `examples/tls/letsencrypt/`

Production TLS pattern. `docker-compose.yml` runs tinylb + a certbot sidecar that shares a volume containing `/etc/letsencrypt/`. Certbot renews; the renewal hook (`renew-hook.sh`) does `kill -HUP $(pidof tinylb)` (or container equivalent: `docker kill -s HUP tinylb`). README explains DNS prerequisites, initial cert issuance, and renewal cron.

## README Outline

```
# tinylb

One-line tagline: hot-reloadable HTTP+WebSocket reverse proxy with TLS,
least-connections routing, and drain mode for zero-downtime deploys.

[Badges: build status, license, crate version (when on crates.io)]

## Features
- Bullet list (8–10 items): HTTP+WS in one path, host-routing, least-conn,
  drain mode, hot config reload, TLS hot-reload, health checks, stats dashboard,
  zero-copy WS frame forwarding, RAII connection counting

## Install
- Download a release binary (Linux glibc/musl, Windows)
- Build from source: cargo build --release

## Quickstart
- 10-line lb.toml
- One command to run

## Configuration reference
- Tables: [server], [health], [[routes]], [[routes.backends]]

## Hot reload
- SIGHUP, or just save the file (10s mtime poll)
- What reloads (backends, certs), what requires restart (bind addr)

## Drain mode and rolling deploys
- Walkthrough with rolling-update example

## TLS
- Self-signed for dev
- Let's Encrypt for prod (link to example)

## Stats
- /_lb/stats JSON
- /_lb/ HTML dashboard screenshot

## Architecture
- ASCII diagram (same as in source README)

## Examples
- Link to examples/ folder

## License
- MIT
```

## Release Workflow

`.github/workflows/release.yml`:

- **Trigger:** `push` on tags matching `v*`.
- **Permissions:** `contents: write` on the release job (required to upload assets).
- **Jobs:**
  1. `build` matrix — three combinations:

     | Runner | Target | Asset filename |
     |---|---|---|
     | `ubuntu-latest` | `x86_64-unknown-linux-gnu` | `tinylb-linux-x86_64-gnu.tar.gz` |
     | `ubuntu-latest` (with `musl-tools` installed) | `x86_64-unknown-linux-musl` | `tinylb-linux-x86_64-musl.tar.gz` |
     | `windows-latest` | `x86_64-pc-windows-msvc` | `tinylb-windows-x86_64.zip` |

     Each job: checkout → set up Rust (`dtolnay/rust-toolchain@stable` with the target added) → `cargo build --release --target <target>` → strip binary (Linux only) → tar/zip with binary + LICENSE + README + lb.toml → upload as artifact.

  2. `release` job (needs: `build`) — downloads all artifacts → publishes a GitHub Release on the tag via `softprops/action-gh-release@v2` with all three archives attached. Release body is auto-generated from commit log between previous tag and this one (`generate_release_notes: true`).

- **No tests in CI** (none exist yet; v0.2 concern).
- **No publish to crates.io** in v0.1.0 (manual when desired).

## Tagging and First Release Flow

1. Implement everything per implementation plan.
2. Commit, push to `main`.
3. `git tag v0.1.0 && git push --tags`.
4. GitHub Action fires, produces three release assets attached to the v0.1.0 release.
5. Verify each binary downloads, extracts, and runs `tinylb --help` (or `tinylb path/to/lb.toml`).

## Adoption Path Inside Existing Projects

After v0.1.0 ships, the three downstream projects can switch over incrementally:

- `snapapi/lb/`, `screenmcp-cloud/lb/`, and the future `gemini-live-server/lb/` can drop their vendored sources and either (a) reference the release binary directly in their docker-compose, or (b) keep a thin local `Dockerfile` that `wget`s the release binary. Decision per-project; out of scope here.

## Open Questions

None blocking. Things to decide post-v0.1.0:

- crates.io publish? (yes, probably v0.1.1 once the name is verified-available)
- Tests? (integration tests with a small Python WS server + httpx client)
- Additional architectures (aarch64 Linux/macOS)? (depends on demand)
