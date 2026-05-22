# Rolling update scripts

Production-tested Python scripts that perform zero-downtime rolling updates
of backends behind tinylb. The pattern is always:

1. **Drain** — flip `drain = true` on one backend in `lb.toml`, SIGHUP tinylb.
2. **Wait** — for in-flight requests to finish (either a fixed sleep or a
   queue-depth poll).
3. **Recreate** — `docker compose up -d --no-deps --force-recreate <svc>` (or
   `docker compose restart <svc>` if you don't need a new image).
4. **Wait healthy** — poll the Docker healthcheck until `healthy`.
5. **Undrain** — flip `drain = false`, SIGHUP again.
6. **Repeat** for the next backend.

If any backend fails its healthcheck, the script aborts with that backend
still drained — traffic continues on the surviving instances. Fix and
re-run; the scripts auto-detect drained backends and recover them first.

## Files

| File | Pattern |
|---|---|
| `rolling_update_lib.py` | Shared logic. Used by `restart_api.py`, `restart_worker.py`, `restart_web.py`. **Edit the constants at the top before using.** |
| `restart_api.py`, `restart_worker.py`, `restart_web.py` | Thin per-service entrypoints. One per backend group; specify the LB `host =` value and a startup timeout. |
| `simple.py` | A standalone script that rolls *one* set of services (configured via `TARGET_SERVICES` at the top). Polls a `/health` endpoint that returns JSON with `queue.active` so it proceeds *as soon as* in-flight work finishes, instead of using a fixed sleep. Adapt or simplify based on whether your backends expose queue depth. |

## Pick the right pattern

- **One service with N replicas, no queue-depth endpoint** → modify
  `restart_api.py` (or the others) — replace `host=` and `default_startup=`,
  set `--drain-timeout` to a fixed safe value. Each script is 4 lines.

- **Multiple service groups, all behind one tinylb** → keep one per-service
  script per group; each calls into the shared lib with a different
  `host=`.

- **Backend exposes queue depth via a health endpoint** → use `simple.py`
  as a starting point. It polls and proceeds the moment the queue empties.

## Prerequisites

- Python 3.10+
- `pip install toml`
- `docker compose` (v2) and `docker` on your PATH
- tinylb running in a container named `tinylb` (or adjust `LB_CONTAINER`)
- Backends in `docker-compose.yml` with `healthcheck:` blocks

## Quick start

Edit the constants at the top of `rolling_update_lib.py`:

```python
LB_TOML_PATH = "lb/lb.toml"
LB_CONTAINER = "tinylb"
COMPOSE_PROJECT = "myproject"
```

Then run:

```bash
python3 restart_api.py
# or with overrides:
python3 restart_api.py --drain-timeout 30 --startup-timeout 120
```
