#!/usr/bin/env bash
# Build the tinylb Docker image and push it to Docker Hub.
#
# Usage:
#   ./docker-publish.sh             # uses version from Cargo.toml, tags :latest too
#   ./docker-publish.sh 0.1.0       # explicit version
#   IMAGE=ghcr.io/owner/tinylb ./docker-publish.sh   # override registry/name
#
# Requires:
#   - docker logged into the target registry (`docker login`)
#   - run from the repo root

set -euo pipefail
cd "$(dirname "$0")"

IMAGE="${IMAGE:-doodkin/tinylb}"
VERSION="${1:-$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)}"

if [[ -z "$VERSION" ]]; then
  echo "ERROR: could not determine version. Pass it explicitly: ./docker-publish.sh <version>" >&2
  exit 1
fi

echo "==> Building $IMAGE:$VERSION (and :latest)..."
docker build -t "$IMAGE:$VERSION" -t "$IMAGE:latest" .

echo "==> Pushing $IMAGE:$VERSION..."
docker push "$IMAGE:$VERSION"

echo "==> Pushing $IMAGE:latest..."
docker push "$IMAGE:latest"

echo ""
echo "Done. Verify with:"
echo "  docker pull $IMAGE:$VERSION"
