# Simple two-backend example

tinylb in front of two `traefik/whoami` containers. Demonstrates the
absolute minimum: one route, two backends, plaintext HTTP, least-connections
between them.

## Run

```bash
docker compose up -d
```

Hit it a few times and watch each response come from a different backend:

```bash
for i in 1 2 3 4 5 6; do curl -s http://localhost:8080/ | grep '^Hostname'; done
```

Open `http://localhost:8080/_lb/` for the live stats dashboard.

## If the published Docker image doesn't exist yet

Replace the `image:` line for the `lb` service with a build context pointing
at your local tinylb checkout (and remove the `command:` since the binary
path is then up to your Dockerfile):

```yaml
  lb:
    build:
      context: ../../..      # path to the tinylb repo root
      dockerfile: Dockerfile  # you'll need to add one — minimal example below
```

Minimal `Dockerfile`:

```dockerfile
FROM rust:1.78 AS build
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/tinylb /usr/local/bin/tinylb
ENTRYPOINT ["/usr/local/bin/tinylb"]
```

## Clean up

```bash
docker compose down
```
