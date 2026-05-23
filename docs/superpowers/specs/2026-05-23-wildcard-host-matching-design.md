# Wildcard Host Matching — Design

**Date:** 2026-05-23
**Status:** Approved (pending written-spec review)

## Goal

Allow `[[routes]]` entries in `lb.toml` to use a leading-`*.` wildcard in the
`host` field so that one route can serve all subdomains of a domain
(e.g. `*.example.doodkin.com` matches `foo.example.doodkin.com`,
`api.v2.example.doodkin.com`, etc.).

Existing exact-host routes continue to work unchanged.

## Non-Goals

- Mid-label or trailing wildcards (`api.*.example.com`, `example.*`). Leading
  `*.` only.
- Automatic apex inclusion: `*.example.com` does NOT match the bare apex
  `example.com`. Define a separate route for the apex if needed.
- Regex / glob host matching.
- TLS certificate provisioning or validation — the operator is responsible for
  supplying a cert (e.g. Let's Encrypt `*.example.com`) that covers the
  wildcard. tinylb's existing `tls_cert` / `tls_key` config is unchanged.

## User-Visible Behavior

### Config

```toml
[[routes]]
host = "*.example.doodkin.com"
[[routes.backends]]
url = "http://backend-a:8080"
```

Any `host` value not beginning with `*.` is treated as an exact match
(unchanged behavior). The `*.` prefix is the only wildcard syntax accepted.

### Matching rules

For an incoming request with `Host: <h>` (port stripped, lowercased):

1. **Exact match wins.** If any route has `host == h` (case-insensitive), use it.
2. **Otherwise, wildcard match.** A route with `host = "*.SUFFIX"` matches `h`
   iff `h` ends with `.SUFFIX` (case-insensitive). The wildcard consumes one
   or more sub-labels (multi-label), so `*.example.com` matches both
   `foo.example.com` and `a.b.example.com`.
3. **Longest suffix wins among wildcards.** If multiple wildcard routes match,
   pick the one whose literal suffix is longest. Example: for
   `x.api.example.com`, `*.api.example.com` beats `*.example.com`.
4. **No match → 404.** Unchanged from current behavior.
5. **Apex is not implicit.** `*.example.com` does not match `example.com`.

### Invalid wildcards

A `host` value of literally `"*"`, `"*."`, or `"*.<empty>"` is rejected at
config load with a clear error. (Any `*.X` where `X` is non-empty is accepted.)
Embedded `*` anywhere other than the leading `*.` is also rejected — e.g.
`api.*.example.com` or `foo*.example.com`.

## Implementation

All changes are confined to `src/backends.rs` and its tests; `src/config.rs`
and the config schema do not change.

### Data model

Add a precomputed match descriptor to `BackendGroup`:

```rust
pub enum HostMatch {
    /// Exact host match, lowercased.
    Exact(String),
    /// Wildcard suffix including the leading dot, lowercased.
    /// E.g. host = "*.example.com" → suffix = ".example.com".
    Wildcard { suffix: String },
}

pub struct BackendGroup {
    pub host: String,           // original config value, kept for display/logs
    pub match_kind: HostMatch,  // new
    pub backends: RwLock<Vec<Arc<Backend>>>,
}
```

`BackendGroup::new()` parses `route.host` into a `HostMatch`:

- If `route.host` starts with `*.` and the remainder is non-empty and contains
  no further `*`, build `Wildcard { suffix: format!(".{}", &route.host[2..]).to_ascii_lowercase() }`.
- Else if `route.host` contains a `*`, return an error (propagated up from
  config load).
- Else `Exact(route.host.to_ascii_lowercase())`.

To surface parse errors cleanly, `BackendGroup::new` becomes
`fn new(route: &RouteConfig) -> Result<Self, String>` and
`BackendRegistry::new` / `apply_config` propagate the error so a bad config
fails the reload with a logged error (consistent with existing
`reload_config` behavior in `src/config.rs:54-69`).

### Lookup

Rewrite `BackendRegistry::find_group`:

```rust
pub fn find_group(&self, host: &str) -> Option<Arc<BackendGroup>> {
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    let groups = self.groups.read().unwrap();

    // 1. Exact match wins.
    if let Some(g) = groups.iter().find(|g| matches!(&g.match_kind,
        HostMatch::Exact(h) if h == &host)) {
        return Some(g.clone());
    }

    // 2. Longest-suffix wildcard match.
    groups.iter()
        .filter_map(|g| match &g.match_kind {
            HostMatch::Wildcard { suffix } if host.ends_with(suffix.as_str())
                => Some((suffix.len(), g)),
            _ => None,
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, g)| g.clone())
}
```

### Reload semantics

`apply_config` already keys group identity off `route.host` (the raw config
string). We keep that: a route is "the same" iff its `host` string is byte-
identical to a previously-loaded one. This means renaming `example.com` to
`*.example.com` in the config is treated as remove + add (active connections
drain, new group starts fresh) — consistent with how other host changes work
today.

## Testing

Unit tests in `src/backends.rs` (alongside existing code):

1. Exact match still works (regression).
2. `*.example.com` matches `foo.example.com`.
3. `*.example.com` matches `a.b.example.com` (multi-label).
4. `*.example.com` does NOT match `example.com` (apex excluded).
5. `*.example.com` does NOT match `notexample.com` (suffix must follow a dot).
6. Exact route `foo.example.com` wins over `*.example.com` for
   `foo.example.com`.
7. `*.api.example.com` wins over `*.example.com` for `x.api.example.com`
   (longest-suffix tiebreak).
8. Case-insensitive: `Foo.Example.COM` matches `*.example.com`.
9. Port is stripped: `foo.example.com:8443` matches `*.example.com`.
10. Invalid `host = "*"` / `"*."` / `"api.*.example.com"` / `"foo*.example.com"`
    rejected by `BackendGroup::new` with a clear error.

No new integration tests required; the live proxy path already exercises
`find_group` via `src/main.rs:302`.

## Docs

Update `lb.toml` (the bundled example config) with a short commented example
of a wildcard route, and add a one-line note to the README's routing section
pointing to the wildcard syntax and TLS-cert caveat.

## Risks & Open Questions

- **TLS SNI mismatch.** If a user configures `*.example.com` but supplies a
  cert that only covers the apex, TLS handshakes for `foo.example.com` will
  fail at the TLS layer before `find_group` is ever called. This is operator
  error and not something the LB can fix; the README note is the mitigation.
- **Performance.** The longest-suffix scan is O(n) over routes per request.
  For tinylb's expected route counts (tens, maybe low hundreds) this is fine
  and matches the existing exact-match scan cost. If we ever need to scale to
  thousands of wildcards, a reverse-domain trie would be the next step — out
  of scope here.
