# Wildcard Host Matching Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add leading-`*.` wildcard support to `[[routes]].host` in `lb.toml` so one route can serve all subdomains of a domain (e.g. `*.example.doodkin.com`).

**Architecture:** A precomputed `HostMatch` enum (`Exact` or `Wildcard { suffix }`) hangs off each `BackendGroup`. Lookup tries exact first, then picks the longest-suffix wildcard. All changes are confined to `src/backends.rs`, with small wiring updates in `src/main.rs` and one-line doc updates.

**Tech Stack:** Rust 2021, existing tinylb dependencies (no new crates).

**Spec:** `docs/superpowers/specs/2026-05-23-wildcard-host-matching-design.md`

---

## File Structure

- **Modify** `src/backends.rs` — add `HostMatch` enum, change `BackendGroup` to hold `match_kind`, make `BackendGroup::new` fallible, propagate through `BackendRegistry::new` and `apply_config`, rewrite `find_group`. Add `#[cfg(test)]` module at end with all unit tests.
- **Modify** `src/main.rs:71` — handle `Result` from `BackendRegistry::new` at startup (fatal).
- **Modify** `src/main.rs:590` — handle `Result` from `apply_config` on reload (log + skip).
- **Modify** `lb.toml` — add commented wildcard route example.
- **Modify** `README.md` — one-line note in the `[[routes]]` table about `*.` prefix and TLS cert caveat.

No new files. Tests live inline next to the code they exercise (Rust convention; no `tests/` dir exists yet and adding one for ~10 small unit tests is overkill).

---

## Task 1: Add `HostMatch` enum and parser

**Files:**
- Modify: `src/backends.rs` (add enum + parser, add `#[cfg(test)]` module)

- [ ] **Step 1: Write failing tests for the parser**

Append to `src/backends.rs` (create the `#[cfg(test)]` module if it doesn't exist yet):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_exact_host() {
        let m = HostMatch::parse("example.com").unwrap();
        assert!(matches!(m, HostMatch::Exact(ref s) if s == "example.com"));
    }

    #[test]
    fn parse_exact_lowercases() {
        let m = HostMatch::parse("Example.COM").unwrap();
        assert!(matches!(m, HostMatch::Exact(ref s) if s == "example.com"));
    }

    #[test]
    fn parse_wildcard_stores_suffix_with_leading_dot() {
        let m = HostMatch::parse("*.example.com").unwrap();
        assert!(matches!(m, HostMatch::Wildcard { ref suffix } if suffix == ".example.com"));
    }

    #[test]
    fn parse_wildcard_lowercases_suffix() {
        let m = HostMatch::parse("*.Example.COM").unwrap();
        assert!(matches!(m, HostMatch::Wildcard { ref suffix } if suffix == ".example.com"));
    }

    #[test]
    fn parse_rejects_bare_star() {
        assert!(HostMatch::parse("*").is_err());
    }

    #[test]
    fn parse_rejects_star_dot_empty() {
        assert!(HostMatch::parse("*.").is_err());
    }

    #[test]
    fn parse_rejects_mid_label_wildcard() {
        assert!(HostMatch::parse("api.*.example.com").is_err());
    }

    #[test]
    fn parse_rejects_partial_label_wildcard() {
        assert!(HostMatch::parse("foo*.example.com").is_err());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib backends::tests`
Expected: compilation error (`HostMatch` not defined).

- [ ] **Step 3: Add the `HostMatch` enum and parser**

Add this block to `src/backends.rs` just above `pub struct BackendGroup` (around line 61):

```rust
/// How an incoming Host header is matched against a route.
#[derive(Debug, Clone)]
pub enum HostMatch {
    /// Exact host match, stored lowercased.
    Exact(String),
    /// Wildcard suffix including the leading dot, stored lowercased.
    /// E.g. config host `*.example.com` → suffix `.example.com`.
    Wildcard { suffix: String },
}

impl HostMatch {
    /// Parse a `host` config value into a `HostMatch`.
    /// Leading `*.` becomes a wildcard; anything else is exact.
    /// Returns an error string for invalid wildcards.
    pub fn parse(host: &str) -> Result<Self, String> {
        if let Some(rest) = host.strip_prefix("*.") {
            if rest.is_empty() {
                return Err(format!("invalid wildcard host {:?}: suffix is empty", host));
            }
            if rest.contains('*') {
                return Err(format!(
                    "invalid wildcard host {:?}: only a single leading '*.' is allowed",
                    host
                ));
            }
            Ok(HostMatch::Wildcard {
                suffix: format!(".{}", rest).to_ascii_lowercase(),
            })
        } else if host.contains('*') {
            Err(format!(
                "invalid host {:?}: '*' is only allowed as a leading '*.' wildcard",
                host
            ))
        } else {
            Ok(HostMatch::Exact(host.to_ascii_lowercase()))
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib backends::tests`
Expected: all 8 parser tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/backends.rs
git commit -m "feat(backends): add HostMatch enum and parser for wildcard hosts"
```

---

## Task 2: Wire `HostMatch` into `BackendGroup` (fallible construction)

**Files:**
- Modify: `src/backends.rs` — `BackendGroup` struct, `BackendGroup::new`, `BackendRegistry::new`, `BackendRegistry::apply_config`
- Modify: `src/main.rs:71` — handle `Result` at startup
- Modify: `src/main.rs:590` — handle `Result` on reload

- [ ] **Step 1: Write failing test for fallible group construction**

Add to the `#[cfg(test)]` mod in `src/backends.rs`:

```rust
    #[test]
    fn backend_group_new_propagates_invalid_wildcard() {
        let route = RouteConfig {
            host: "api.*.example.com".to_string(),
            backends: vec![],
        };
        assert!(BackendGroup::new(&route).is_err());
    }

    #[test]
    fn backend_group_new_accepts_wildcard() {
        let route = RouteConfig {
            host: "*.example.com".to_string(),
            backends: vec![],
        };
        let g = BackendGroup::new(&route).unwrap();
        assert!(matches!(g.match_kind, HostMatch::Wildcard { ref suffix } if suffix == ".example.com"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib backends::tests`
Expected: compile errors (`BackendGroup::new` returns `Self`, not `Result`; `match_kind` field doesn't exist).

- [ ] **Step 3: Update `BackendGroup` struct and constructor**

In `src/backends.rs`, change `BackendGroup` (around line 62) and its `impl` block. Replace this:

```rust
pub struct BackendGroup {
    pub host: String,
    pub backends: RwLock<Vec<Arc<Backend>>>,
}

impl BackendGroup {
    pub fn new(route: &RouteConfig) -> Self {
        let backends = route
            .backends
            .iter()
            .map(|c| Arc::new(Backend::new(c)))
            .collect();
        Self {
            host: route.host.clone(),
            backends: RwLock::new(backends),
        }
    }
```

With:

```rust
pub struct BackendGroup {
    pub host: String,
    pub match_kind: HostMatch,
    pub backends: RwLock<Vec<Arc<Backend>>>,
}

impl BackendGroup {
    pub fn new(route: &RouteConfig) -> Result<Self, String> {
        let match_kind = HostMatch::parse(&route.host)?;
        let backends = route
            .backends
            .iter()
            .map(|c| Arc::new(Backend::new(c)))
            .collect();
        Ok(Self {
            host: route.host.clone(),
            match_kind,
            backends: RwLock::new(backends),
        })
    }
```

- [ ] **Step 4: Update `BackendRegistry::new` to propagate errors**

In `src/backends.rs`, replace `BackendRegistry::new` (around line 157):

```rust
    pub fn new(routes: &[RouteConfig]) -> Result<Self, String> {
        let mut groups = Vec::with_capacity(routes.len());
        for r in routes {
            groups.push(Arc::new(BackendGroup::new(r)?));
        }
        Ok(Self {
            groups: RwLock::new(groups),
        })
    }
```

- [ ] **Step 5: Update `BackendRegistry::apply_config` to propagate errors**

In `src/backends.rs`, change `apply_config` (around line 175). Validate all routes first, then commit — so a bad reload doesn't half-apply.

Replace the existing body with:

```rust
    pub fn apply_config(&self, routes: &[RouteConfig]) -> Result<(), String> {
        // Validate all routes up-front so a bad config doesn't half-apply.
        for r in routes {
            HostMatch::parse(&r.host)?;
        }

        let mut groups = self.groups.write().unwrap();
        let mut new_groups: Vec<Arc<BackendGroup>> = Vec::with_capacity(routes.len());

        for route in routes {
            if let Some(existing) = groups.iter().find(|g| g.host == route.host) {
                existing.apply_backends(&route.backends);
                new_groups.push(Arc::clone(existing));
            } else {
                // Safe to unwrap: we validated above.
                let group = Arc::new(BackendGroup::new(route).unwrap());
                info!(host = %route.host, "Added new route");
                new_groups.push(group);
            }
        }

        for old in groups.iter() {
            if !new_groups.iter().any(|g| g.host == old.host) {
                warn!(host = %old.host, "Route removed from config");
            }
        }

        *groups = new_groups;
        Ok(())
    }
```

- [ ] **Step 6: Update startup site `src/main.rs:71`**

Change:

```rust
    let registry = Arc::new(BackendRegistry::new(&initial_config.routes));
```

To:

```rust
    let registry = Arc::new(
        BackendRegistry::new(&initial_config.routes)
            .expect("Invalid route configuration at startup"),
    );
```

- [ ] **Step 7: Update reload site `src/main.rs:590`**

Change:

```rust
        if let Some(new_config) = config::reload_config(config_path) {
            registry.apply_config(&new_config.routes);
```

To:

```rust
        if let Some(new_config) = config::reload_config(config_path) {
            if let Err(e) = registry.apply_config(&new_config.routes) {
                error!(error = %e, "Failed to apply new route configuration; keeping previous");
            }
```

Then verify the `error` macro is in scope at the top of `main.rs`. Run:

```bash
grep -n "use tracing" src/main.rs
```

If `error` is not in the existing `use tracing::{...}` import, add it.

- [ ] **Step 8: Run the full test suite + build**

Run: `cargo test --lib`
Expected: all tests pass (8 from Task 1 + 2 new = 10).

Run: `cargo build`
Expected: clean build, no warnings about unused imports or dead code.

- [ ] **Step 9: Commit**

```bash
git add src/backends.rs src/main.rs
git commit -m "feat(backends): make BackendGroup construction fallible and store HostMatch"
```

---

## Task 3: Implement wildcard lookup in `find_group`

**Files:**
- Modify: `src/backends.rs` — `BackendRegistry::find_group`, add lookup tests

- [ ] **Step 1: Write failing tests for lookup behavior**

Add to the `#[cfg(test)]` mod in `src/backends.rs`. Define a small helper at the top of the module to keep tests terse:

```rust
    fn route(host: &str) -> RouteConfig {
        RouteConfig { host: host.to_string(), backends: vec![] }
    }

    fn registry(hosts: &[&str]) -> BackendRegistry {
        let routes: Vec<RouteConfig> = hosts.iter().map(|h| route(h)).collect();
        BackendRegistry::new(&routes).unwrap()
    }

    #[test]
    fn find_exact_match() {
        let r = registry(&["example.com"]);
        assert_eq!(r.find_group("example.com").unwrap().host, "example.com");
    }

    #[test]
    fn find_exact_match_strips_port() {
        let r = registry(&["example.com"]);
        assert_eq!(r.find_group("example.com:8443").unwrap().host, "example.com");
    }

    #[test]
    fn find_exact_match_is_case_insensitive() {
        let r = registry(&["example.com"]);
        assert_eq!(r.find_group("Example.COM").unwrap().host, "example.com");
    }

    #[test]
    fn find_no_match_returns_none() {
        let r = registry(&["example.com"]);
        assert!(r.find_group("other.com").is_none());
    }

    #[test]
    fn wildcard_matches_single_label_subdomain() {
        let r = registry(&["*.example.com"]);
        assert_eq!(r.find_group("foo.example.com").unwrap().host, "*.example.com");
    }

    #[test]
    fn wildcard_matches_multi_label_subdomain() {
        let r = registry(&["*.example.com"]);
        assert_eq!(r.find_group("a.b.example.com").unwrap().host, "*.example.com");
    }

    #[test]
    fn wildcard_does_not_match_apex() {
        let r = registry(&["*.example.com"]);
        assert!(r.find_group("example.com").is_none());
    }

    #[test]
    fn wildcard_does_not_match_suffix_without_dot_boundary() {
        let r = registry(&["*.example.com"]);
        assert!(r.find_group("notexample.com").is_none());
    }

    #[test]
    fn exact_wins_over_wildcard() {
        let r = registry(&["*.example.com", "foo.example.com"]);
        assert_eq!(r.find_group("foo.example.com").unwrap().host, "foo.example.com");
    }

    #[test]
    fn longest_suffix_wildcard_wins() {
        let r = registry(&["*.example.com", "*.api.example.com"]);
        assert_eq!(
            r.find_group("x.api.example.com").unwrap().host,
            "*.api.example.com"
        );
    }

    #[test]
    fn wildcard_match_is_case_insensitive() {
        let r = registry(&["*.example.com"]);
        assert_eq!(
            r.find_group("Foo.Example.COM").unwrap().host,
            "*.example.com"
        );
    }

    #[test]
    fn wildcard_match_strips_port() {
        let r = registry(&["*.example.com"]);
        assert_eq!(
            r.find_group("foo.example.com:8443").unwrap().host,
            "*.example.com"
        );
    }
```

- [ ] **Step 2: Run tests to verify the new ones fail**

Run: `cargo test --lib backends::tests`
Expected: the 12 new lookup tests fail (most because `find_group` still does exact `g.host == host` matching, so wildcards don't match anything; some pass coincidentally). The Task 1 parser tests and Task 2 construction tests still pass.

- [ ] **Step 3: Rewrite `find_group`**

In `src/backends.rs`, replace `find_group` (around line 168):

```rust
    /// Find a backend group by Host header value (strips port if present).
    /// Tries exact match first, then falls back to the longest-suffix wildcard.
    pub fn find_group(&self, host: &str) -> Option<Arc<BackendGroup>> {
        let host = host
            .split(':')
            .next()
            .unwrap_or(host)
            .to_ascii_lowercase();
        let groups = self.groups.read().unwrap();

        // 1. Exact match wins.
        if let Some(g) = groups.iter().find(|g| match &g.match_kind {
            HostMatch::Exact(h) => h == &host,
            _ => false,
        }) {
            return Some(g.clone());
        }

        // 2. Longest-suffix wildcard match.
        groups
            .iter()
            .filter_map(|g| match &g.match_kind {
                HostMatch::Wildcard { suffix } if host.ends_with(suffix.as_str()) => {
                    Some((suffix.len(), g))
                }
                _ => None,
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, g)| Arc::clone(g))
    }
```

- [ ] **Step 4: Run all tests to verify they pass**

Run: `cargo test --lib`
Expected: all 22 tests pass (8 parser + 2 construction + 12 lookup).

Run: `cargo build --release`
Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add src/backends.rs
git commit -m "feat(backends): wildcard host matching with longest-suffix precedence"
```

---

## Task 4: Documentation updates

**Files:**
- Modify: `lb.toml` — add commented wildcard example
- Modify: `README.md` — update `[[routes]]` table description for `host`

- [ ] **Step 1: Update `lb.toml`**

Add this commented block to `lb.toml`, immediately after the existing `[[routes]]` example (after the `# [[routes.backends]]` block for `api.example.com`):

```toml

# Wildcard subdomain route. A leading "*." matches one or more sub-labels:
# "*.example.com" matches "foo.example.com" and "a.b.example.com" but NOT
# the bare apex "example.com" (define a separate route for that if needed).
# Exact routes win over wildcards; among wildcards, the longest suffix wins.
# Your TLS cert must cover the wildcard (e.g. Let's Encrypt *.example.com).
#
# [[routes]]
# host = "*.example.com"
# [[routes.backends]]
# url = "http://wildcard-backend:8080"
```

- [ ] **Step 2: Update `README.md`**

In `README.md`, find the `[[routes]]` table row for `host` (line 103) and replace it:

```markdown
| `host` | yes | The HTTP `Host:` header value to match. Port is stripped before comparison. May start with `*.` for wildcard subdomain matching (e.g. `*.example.com` matches `foo.example.com` and `a.b.example.com`, but not the bare apex). Exact matches win over wildcards; longest wildcard suffix wins among wildcards. |
```

- [ ] **Step 3: Verify build still passes**

Run: `cargo build`
Expected: clean build (sanity check that no doc-test or include broke).

- [ ] **Step 4: Commit**

```bash
git add lb.toml README.md
git commit -m "docs: document *. wildcard host matching in lb.toml and README"
```

---

## Task 5: Manual smoke test

**Files:** none (verification only)

- [ ] **Step 1: Start a couple of throwaway backends**

In two separate terminals:

```bash
python3 -m http.server 9001 --bind 127.0.0.1
```

```bash
python3 -m http.server 9002 --bind 127.0.0.1
```

- [ ] **Step 2: Write a temporary test config**

Create `/tmp/lb-wildcard-test.toml`:

```toml
[server]
bind = "127.0.0.1:8080"

[health]
interval_secs = 30
timeout_secs = 3
unhealthy_cooldown_secs = 15

[[routes]]
host = "exact.test"
[[routes.backends]]
url = "http://127.0.0.1:9001"

[[routes]]
host = "*.wild.test"
[[routes.backends]]
url = "http://127.0.0.1:9002"
```

- [ ] **Step 3: Start tinylb**

```bash
cargo run --release -- /tmp/lb-wildcard-test.toml
```

- [ ] **Step 4: Verify routing with curl**

In another terminal:

```bash
# Exact host → 9001 (look for a 200 with a directory listing from the 9001 process).
curl -s -H 'Host: exact.test' http://127.0.0.1:8080/ -o /dev/null -w 'exact: %{http_code}\n'

# Wildcard single-label subdomain → 9002.
curl -s -H 'Host: foo.wild.test' http://127.0.0.1:8080/ -o /dev/null -w 'foo.wild: %{http_code}\n'

# Wildcard multi-label subdomain → 9002.
curl -s -H 'Host: a.b.wild.test' http://127.0.0.1:8080/ -o /dev/null -w 'a.b.wild: %{http_code}\n'

# Wildcard apex → 404 (apex is NOT matched by *.wild.test).
curl -s -H 'Host: wild.test' http://127.0.0.1:8080/ -o /dev/null -w 'apex: %{http_code}\n'

# Unknown host → 404.
curl -s -H 'Host: nope.example' http://127.0.0.1:8080/ -o /dev/null -w 'unknown: %{http_code}\n'
```

Expected output:
```
exact: 200
foo.wild: 200
a.b.wild: 200
apex: 404
unknown: 404
```

Also confirm in the two backend terminals that the GETs landed on the right ports (9001 logs `exact.test`, 9002 logs the `foo.wild.test` and `a.b.wild.test` ones).

- [ ] **Step 5: Test hot-reload validation**

While tinylb is still running, edit `/tmp/lb-wildcard-test.toml` and change `*.wild.test` to `api.*.wild.test` (invalid). Save.

Within ~10 seconds, the tinylb log should show:
```
Failed to apply new route configuration; keeping previous error=invalid wildcard host "api.*.wild.test": only a single leading '*.' is allowed
```

Re-run the wildcard `curl` from Step 4 — it should still return 200, proving the old config is still active. Fix the file (restore `*.wild.test`) and verify the next reload succeeds.

- [ ] **Step 6: Tear down**

Ctrl-C all three processes. No commit needed for this task.

---

## Done criteria

- [ ] All 22 unit tests in `cargo test --lib` pass
- [ ] `cargo build --release` produces no warnings
- [ ] Manual smoke test (Task 5) returns the expected status codes
- [ ] Bad wildcard at startup aborts cleanly; bad wildcard on reload logs and is rejected (existing config stays live)
- [ ] `lb.toml` and `README.md` document the new syntax
