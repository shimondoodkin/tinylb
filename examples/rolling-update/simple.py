#!/usr/bin/env python3
"""Rolling update for snapapi behind the Rust LB.

Builds the snapapi image once, then for each backend (snapapi-a, snapapi-b)
in turn:
  1. Flip its drain flag in lb/lb.toml + SIGHUP the LB (stops new traffic).
  2. Brief grace pause for any request the LB just sent before the SIGHUP.
  3. Poll the backend's /health.queue.active until it hits 0 (in-flight
     requests have finished). Bail after --drain-timeout if it doesn't.
  4. Recreate the container with the freshly-built image.
  5. Wait for the Docker healthcheck to flip to "healthy".
  6. Clear the drain flag + SIGHUP again.

Polling queue.active means the script proceeds *as soon as* requests are
actually done, instead of sleeping a fixed timeout — so a 2-second
screenshot doesn't make the next swap wait 15 seconds for nothing, and a
30-second screenshot doesn't get killed mid-flight by a too-short timeout.

Failure semantics: if an instance fails its healthcheck within
--startup-timeout, the script aborts WITH that instance still drained.
Traffic continues on the surviving instance. Fix the issue and re-run;
the script auto-detects drained instances and recovers them first.

Usage:
    python3 rolling-update.py                          # build + roll both
    python3 rolling-update.py --no-build               # roll without rebuild
    python3 rolling-update.py --drain-timeout 120      # raise max drain wait

Requires: pip install toml.
"""

import argparse
import re
import subprocess
import sys
import time

import toml

LB_TOML_PATH = "lb/lb.toml"
LB_CONTAINER = "snapapi"             # docker container_name of the LB
COMPOSE_PROJECT = "snapapi"           # folder name => compose project name
TARGET_SERVICES = ["snapapi-a", "snapapi-b"]


def log(msg: str) -> None:
    ts = time.strftime("%H:%M:%S")
    print(f"[{ts}] {msg}", flush=True)


def run(cmd: list[str], check: bool = True, capture: bool = True) -> subprocess.CompletedProcess:
    log(f"  $ {' '.join(cmd)}")
    return subprocess.run(cmd, capture_output=capture, text=True, check=check)


def read_config() -> dict:
    with open(LB_TOML_PATH, "r") as f:
        return toml.load(f)


def write_config(config: dict) -> None:
    with open(LB_TOML_PATH, "w") as f:
        toml.dump(config, f)


def reload_lb() -> None:
    run(["docker", "kill", "-s", "HUP", LB_CONTAINER])


def url_to_service(url: str) -> str:
    """http://snapapi-a:3000 -> snapapi-a"""
    m = re.match(r"https?://([^:/]+)", url)
    if not m:
        raise ValueError(f"Cannot parse service name from URL: {url}")
    return m.group(1)


def collect_backend_urls(config: dict) -> list[str]:
    """All unique backend URLs across every route, in insertion order."""
    seen = []
    for route in config.get("routes", []):
        for backend in route.get("backends", []):
            url = backend.get("url")
            if url and url not in seen and url_to_service(url) in TARGET_SERVICES:
                seen.append(url)
    return seen


def set_drain_everywhere(config: dict, url: str, drain: bool) -> None:
    """Toggle drain on every backend matching the given URL, across all routes."""
    touched = 0
    for route in config.get("routes", []):
        for backend in route.get("backends", []):
            if backend.get("url") == url:
                backend["drain"] = drain
                touched += 1
    write_config(config)
    reload_lb()
    state = "drained" if drain else "active"
    log(f"  {url} is now {state} ({touched} route(s) updated)")


def is_drained(config: dict, url: str) -> bool:
    """True iff every occurrence of this URL has drain=true."""
    occurrences = 0
    drained = 0
    for route in config.get("routes", []):
        for backend in route.get("backends", []):
            if backend.get("url") == url:
                occurrences += 1
                if backend.get("drain", False):
                    drained += 1
    return occurrences > 0 and drained == occurrences


def docker_health_status(container: str) -> str:
    """Return 'healthy' | 'unhealthy' | 'starting' | '' (no healthcheck)."""
    r = run(
        ["docker", "inspect", "--format", "{{.State.Health.Status}}", container],
        check=False,
    )
    return r.stdout.strip()


def get_queue_active(container: str) -> "int | None":
    """Query <container>:3000/health from inside the container and return
    queue.active. Returns None if the query fails (container down, JSON
    unparseable, etc) — caller treats that as "don't know, keep waiting"."""
    import json as _json
    r = run(
        ["docker", "exec", container, "node", "-e",
         "fetch('http://localhost:3000/health')"
         ".then(r=>r.text()).then(t=>{process.stdout.write(t)})"
         ".catch(e=>{console.error(e.message);process.exit(1)})"],
        check=False,
    )
    if r.returncode != 0:
        return None
    try:
        data = _json.loads(r.stdout)
    except Exception:
        return None
    return int(data.get("queue", {}).get("active", 0))


def wait_queue_idle(container: str, max_wait: int, post_drain_grace: int = 2) -> int:
    """Sleep `post_drain_grace` seconds (lets any-in-flight LB requests
    finish landing) then poll the backend's queue.active. Returns when
    queue is empty or `max_wait` is reached. Returns the last observed
    active count (0 if idle, >0 if we gave up waiting)."""
    log(f"  grace pause {post_drain_grace}s for late-arriving requests...")
    time.sleep(post_drain_grace)
    deadline = time.time() + max_wait
    last_active = None
    while time.time() < deadline:
        active = get_queue_active(container)
        if active is None:
            log(f"  could not query {container} health, retrying in 2s")
            time.sleep(2)
            continue
        last_active = active
        if active == 0:
            log(f"  queue.active=0, in-flight drained")
            return 0
        remaining = int(deadline - time.time())
        log(f"  queue.active={active}, waiting ({remaining}s left)")
        time.sleep(2)
    log(f"  drain wait expired with queue.active={last_active}, proceeding anyway")
    return last_active if last_active is not None else -1


def wait_healthy(container: str, timeout: int) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        status = docker_health_status(container)
        if status == "healthy":
            return True
        remaining = int(deadline - time.time())
        log(f"  {container} health: {status or 'no-healthcheck'} ({remaining}s left)")
        time.sleep(5)
    return False


def build_restart_order(config: dict, urls: list[str]) -> list[str]:
    """Drained backends first (recovery), then active ones."""
    drained, active = [], []
    for url in urls:
        (drained if is_drained(config, url) else active).append(url)
    return drained + active


def recreate_service(service: str) -> None:
    """`docker compose up -d --no-deps --force-recreate <svc>` swaps the
    container with the newly-built image; without --force-recreate compose
    can decide the image is fresh enough and skip the swap."""
    run(["docker", "compose", "up", "-d", "--no-deps", "--force-recreate", service])


def build_images(services: list[str]) -> None:
    run(["docker", "compose", "build", "--pull"] + services, capture=False)


def rolling_update(drain_timeout: int, startup_timeout: int, do_build: bool) -> None:
    config = read_config()
    urls = collect_backend_urls(config)
    if not urls:
        log("ERROR: No backends in lb.toml match TARGET_SERVICES")
        sys.exit(1)

    log(f"Found {len(urls)} target backend(s):")
    for url in urls:
        marker = " [DRAINED]" if is_drained(config, url) else ""
        log(f"  - {url}{marker}")

    if do_build:
        log("\nBuilding new image(s)...")
        build_images(TARGET_SERVICES)
        log("Build complete.")

    order = build_restart_order(config, urls)
    if any(is_drained(config, u) for u in order):
        log("Drained backends detected — restarting those first (recovery mode)")

    results = []
    for step, url in enumerate(order, 1):
        service = url_to_service(url)
        log(f"\n{'='*60}")
        log(f"Step {step}/{len(order)}: {service} ({url})")
        log(f"{'='*60}")

        config = read_config()
        already_drained = is_drained(config, url)
        if not already_drained:
            log(f"Draining {service} via LB...")
            set_drain_everywhere(config, url, True)
            log(f"Waiting for {service} queue to drain (max {drain_timeout}s)...")
            wait_queue_idle(service, max_wait=drain_timeout)
        else:
            log(f"{service} already drained (recovery mode)")

        log(f"Recreating {service} with the new image...")
        recreate_service(service)

        log(f"Waiting for {service} to become healthy (timeout {startup_timeout}s)...")
        if not wait_healthy(service, startup_timeout):
            log(f"FAILED: {service} did not become healthy in {startup_timeout}s.")
            log(f"Backend stays drained — traffic continues on the other instance.")
            log("Fix the issue and re-run this script; it will recover drained backends first.")
            results.append((service, "FAILED"))
            sys.exit(1)

        log(f"{service} is healthy.")
        config = read_config()
        log(f"Undraining {service}...")
        set_drain_everywhere(config, url, False)
        log("Sleeping 10s for the LB health-check loop to mark it healthy...")
        time.sleep(10)
        results.append((service, "OK"))

    log(f"\n{'='*60}")
    log("Rolling update complete.")
    log(f"{'='*60}")
    for service, status in results:
        log(f"  {service}: {status}")


def main():
    p = argparse.ArgumentParser(description="Rolling update for snapapi-a / snapapi-b")
    p.add_argument("--drain-timeout", type=int, default=60,
                   help="Max seconds to wait for in-flight requests to finish after "
                        "LB drain. Script polls queue.active and proceeds as soon "
                        "as it hits 0. (default 60)")
    p.add_argument("--startup-timeout", type=int, default=120,
                   help="Seconds to wait for the healthcheck after restart (default 120)")
    p.add_argument("--no-build", action="store_true",
                   help="Skip the docker compose build step (use existing image)")
    args = p.parse_args()

    log("Starting rolling update...")
    rolling_update(args.drain_timeout, args.startup_timeout, do_build=not args.no_build)


if __name__ == "__main__":
    main()
