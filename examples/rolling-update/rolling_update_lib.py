"""Shared rolling-restart logic for services behind the LB.

Used by rolling-restart-web.py, rolling-restart-api.py, rolling-restart-worker.py.
Not intended to be run directly.

Drains, restarts, and undrains one backend at a time for zero-downtime
deployments. Reads lb.toml to discover backends and their drain state.

Requires: pip install toml
"""

import argparse
import re
import subprocess
import sys
import time

import toml

# ─── ADJUST THESE FOR YOUR PROJECT ────────────────────────────────────────
# Path to your lb.toml (relative to the cwd you run this script from).
LB_TOML_PATH = "lb/lb.toml"
# docker container_name of the tinylb container. `docker kill -s HUP <name>`
# is what triggers a config reload.
LB_CONTAINER = "tinylb"
# docker compose project name — used to construct per-service container
# names as f"{COMPOSE_PROJECT}-{service}-1" for healthcheck inspection.
# Typically the folder name compose is run from.
COMPOSE_PROJECT = "myproject"
# ──────────────────────────────────────────────────────────────────────────


def log(msg: str) -> None:
    ts = time.strftime("%H:%M:%S")
    print(f"[{ts}] {msg}", flush=True)


def run(cmd: list[str], check: bool = True) -> subprocess.CompletedProcess:
    log(f"  $ {' '.join(cmd)}")
    return subprocess.run(cmd, capture_output=True, text=True, check=check)


def read_config() -> dict:
    with open(LB_TOML_PATH, "r") as f:
        return toml.load(f)


def write_config(config: dict) -> None:
    with open(LB_TOML_PATH, "w") as f:
        toml.dump(config, f)


def reload_lb() -> None:
    """Send SIGHUP to the LB container to hot-reload config."""
    run(["docker", "kill", "-s", "HUP", LB_CONTAINER])


def url_to_service(url: str) -> str:
    """Extract docker compose service name from backend URL.

    http://web1:3000 -> web1
    """
    match = re.match(r"https?://([^:]+):\d+", url)
    if not match:
        raise ValueError(f"Cannot parse service name from URL: {url}")
    return match.group(1)


def get_backends(config: dict, host: str) -> tuple[int, list[dict]]:
    """Find the route for `host` and return (route_index, backends list)."""
    for i, route in enumerate(config.get("routes", [])):
        if route.get("host") == host:
            return i, route.get("backends", [])
    raise RuntimeError(f"No route found for host '{host}' in {LB_TOML_PATH}")


def set_drain(config: dict, route_idx: int, backend_idx: int, drain: bool) -> None:
    """Set drain flag on a specific backend and write config."""
    config["routes"][route_idx]["backends"][backend_idx]["drain"] = drain
    write_config(config)
    reload_lb()
    state = "drained" if drain else "active"
    url = config["routes"][route_idx]["backends"][backend_idx]["url"]
    log(f"  Backend {url} is now {state}")


def docker_health_status(service: str) -> str:
    """Get Docker healthcheck status for a compose service."""
    container = f"{COMPOSE_PROJECT}-{service}-1"
    result = run(
        ["docker", "inspect", "--format", "{{.State.Health.Status}}", container],
        check=False,
    )
    return result.stdout.strip()


def wait_healthy(service: str, timeout: int) -> bool:
    """Poll Docker healthcheck until healthy or timeout."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        status = docker_health_status(service)
        if status == "healthy":
            return True
        remaining = int(deadline - time.time())
        log(f"  {service} health: {status} ({remaining}s remaining)")
        time.sleep(5)
    return False


def restart_service(service: str) -> None:
    """Restart a docker compose service."""
    run(["docker", "compose", "restart", service])


def build_restart_order(backends: list[dict]) -> list[int]:
    """Return backend indices ordered: drained first, then non-drained.

    Drained backends are assumed to be from a previous failed restart and
    should be recovered first (they're already not receiving traffic).
    """
    drained = []
    active = []
    for i, b in enumerate(backends):
        if b.get("drain", False):
            drained.append(i)
        else:
            active.append(i)
    return drained + active


def rolling_restart(host: str, drain_timeout: int, startup_timeout: int) -> None:
    config = read_config()
    route_idx, backends = get_backends(config, host)

    if not backends:
        log(f"ERROR: No backends found for route '{host}'")
        sys.exit(1)
    if len(backends) < 2:
        log(f"WARNING: Only {len(backends)} backend(s) for '{host}' — restart will cause downtime")

    log(f"Found {len(backends)} backend(s) for '{host}':")
    for i, b in enumerate(backends):
        drain_state = " [DRAINED]" if b.get("drain", False) else ""
        log(f"  [{i}] {b['url']}{drain_state}")

    order = build_restart_order(backends)
    if any(backends[i].get("drain", False) for i in order):
        log("Drained backends detected — restarting those first (recovery mode)")

    results = []

    for step, backend_idx in enumerate(order, 1):
        backend = backends[backend_idx]
        url = backend["url"]
        service = url_to_service(url)

        log(f"\n{'='*60}")
        log(f"Step {step}/{len(order)}: Restarting {service} ({url})")
        log(f"{'='*60}")

        # 1. Drain
        already_drained = backend.get("drain", False)
        if not already_drained:
            log(f"Draining {service}...")
            # Re-read config in case it changed
            config = read_config()
            set_drain(config, route_idx, backend_idx, True)
            log(f"Waiting {drain_timeout}s for existing connections to finish...")
            time.sleep(drain_timeout)
        else:
            log(f"{service} already drained (recovering from previous failure)")

        # 2. Restart
        log(f"Restarting {service}...")
        restart_service(service)

        # 3. Wait for healthy
        log(f"Waiting for {service} to become healthy (timeout: {startup_timeout}s)...")
        if not wait_healthy(service, startup_timeout):
            log(f"RESTART FAILED: {service} did not become healthy within {startup_timeout}s")
            log(f"Backend remains drained — no traffic will be sent to {service}")
            log("Fix the issue and re-run this script to recover.")
            results.append((service, "FAILED"))
            sys.exit(1)

        log(f"{service} is healthy!")

        # 4. Undrain
        log(f"Undraining {service}...")
        config = read_config()
        set_drain(config, route_idx, backend_idx, False)

        # 5. Wait for LB health check to pick it up
        log("Waiting 10s for LB health check to mark backend healthy...")
        time.sleep(10)

        results.append((service, "OK"))
        log(f"{service} restart complete!")

    # Summary
    log(f"\n{'='*60}")
    log("Rolling restart complete!")
    log(f"{'='*60}")
    for service, status in results:
        log(f"  {service}: {status}")


def cli(target: str, host: str, default_startup: int = 180) -> None:
    """Standard CLI entrypoint for per-target rolling-restart scripts."""
    parser = argparse.ArgumentParser(
        description=f"Rolling restart for '{target}' ({host})"
    )
    parser.add_argument(
        "--drain-timeout",
        type=int,
        default=15,
        help="Seconds to wait for connections to drain (default: 15)",
    )
    parser.add_argument(
        "--startup-timeout",
        type=int,
        default=default_startup,
        help=f"Seconds to wait for service to become healthy (default: {default_startup})",
    )
    args = parser.parse_args()

    log(f"Starting rolling restart for target='{target}' (host='{host}')...")
    rolling_restart(host, args.drain_timeout, args.startup_timeout)
