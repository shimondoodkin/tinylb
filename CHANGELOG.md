# Changelog

All notable changes to tinylb are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] — 2026-05-22

Initial public release. Source extracted from screenmcp-cloud/snapapi
where it has been running in production for several months.

### Added
- HTTP + WebSocket reverse proxy in one path (zero-copy WS frame forwarding).
- Host-header routing via `[[routes]]` blocks.
- Least-connections backend selection with per-backend `max_connections` cap.
- Active health checks (periodic HTTP GET) with cooldown on failure.
- Drain mode for zero-downtime rolling deploys.
- Hot config reload via SIGHUP (Unix) or 10s file-mtime poll (all platforms).
- TLS termination with rustls and hot certificate reload (lock-free via ArcSwap).
- `X-Forwarded-For` preservation on proxied HTTP requests.
- Stats JSON endpoint at `/_lb/stats` and HTML dashboard at `/_lb/`.
- Release binaries for Linux x86_64 (glibc and static-musl) and Windows x86_64.

[Unreleased]: https://github.com/shimondoodkin/tinylb/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/shimondoodkin/tinylb/releases/tag/v0.1.0
