# Changelog

All notable changes to tinylb are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.1] — 2026-06-04

### Fixed
- WebSocket proxying dropped the request path when dialing the backend: an
  incoming `/voice` upgrade was forwarded to `ws://backend:port` (root) instead
  of `ws://backend:port/voice`. Backends whose WS route wasn't at `/` rejected
  the handshake (returning a normal HTTP 200/4xx instead of 101), surfacing as
  `Failed to connect to backend WebSocket … error=HTTP error: 200 OK`. The
  proxy now preserves the original request path+query for the backend dial,
  mirroring the HTTP proxy path handling.

## [0.2.0] — 2026-05-23

### Added
- Wildcard subdomain routing: `[[routes]].host` may start with `*.` to match
  any subdomain (one or more sub-labels). E.g. `*.example.com` matches
  `foo.example.com` and `a.b.example.com` but not the bare apex `example.com`.
- Exact-host routes always win over wildcards; among wildcards, the longest
  literal suffix wins. Host matching is case-insensitive.
- Bad wildcard syntax in the config (e.g. `api.*.example.com`) now fails
  startup with a clear error; on reload, the bad config is rejected and the
  previous routing table stays live.

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

[Unreleased]: https://github.com/shimondoodkin/tinylb/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/shimondoodkin/tinylb/releases/tag/v0.2.0
[0.1.0]: https://github.com/shimondoodkin/tinylb/releases/tag/v0.1.0
