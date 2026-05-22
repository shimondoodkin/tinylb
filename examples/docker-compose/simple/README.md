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

## Building the image from source

The compose file pulls `doodkin/tinylb:0.1.0` from Docker Hub. If you want
to build the image from the current checkout (e.g. for a development
version), replace the `image:` line with a build context pointing at the
repo root:

```yaml
  lb:
    build:
      context: ../../..
      # uses the Dockerfile at the repo root
```

The repo's `Dockerfile` is a multi-stage Alpine build that produces a
~16 MB static-musl image.

## Clean up

```bash
docker compose down
```
