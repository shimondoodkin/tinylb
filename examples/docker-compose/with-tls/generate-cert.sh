#!/usr/bin/env bash
# Generates a self-signed cert valid for localhost.
# Output: certs/fullchain.pem, certs/privkey.pem.
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p certs
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout certs/privkey.pem \
  -out    certs/fullchain.pem \
  -days   365 \
  -subj   "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"
chmod 644 certs/*.pem
echo "Wrote certs/fullchain.pem and certs/privkey.pem (valid 365 days, CN=localhost)."
