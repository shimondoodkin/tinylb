#!/usr/bin/env bash
# Generate a self-signed cert for localhost, valid 365 days.
set -euo pipefail
cd "$(dirname "$0")"
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout privkey.pem \
  -out    fullchain.pem \
  -days   365 \
  -subj   "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"
echo "Wrote fullchain.pem and privkey.pem (valid 365 days, CN=localhost)."
