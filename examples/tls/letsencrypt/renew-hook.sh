#!/usr/bin/env sh
# certbot deploy-hook: runs once after each successful renewal.
# Tells tinylb to re-read its cert files atomically (no connection drops).
set -e
docker kill -s HUP tinylb
echo "[renew-hook] SIGHUP sent to tinylb"
