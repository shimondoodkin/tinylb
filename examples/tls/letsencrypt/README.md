# Let's Encrypt with certbot sidecar

Production TLS pattern: tinylb terminates HTTPS using certs issued by
certbot. A certbot sidecar runs in the background, renewing every 12
hours; after each successful renewal it `kill -HUP`s tinylb so the new
cert is loaded atomically.

## Prerequisites

- A domain pointing at the host running this compose stack
- Port 80 reachable from the public internet (for the http-01 challenge)
- Replace every occurrence of `example.com` in `lb.toml` and below with
  your real domain
- Replace the e-mail address in the initial issuance command

## One-time setup: issue the first cert

The renewal loop only renews already-issued certs — it doesn't issue the
first one. Run the issuance command once:

```bash
docker compose up -d backend          # backend must be reachable first
docker compose run --rm \
  -p 80:80 \
  certbot certonly \
  --standalone \
  --preferred-challenges http \
  --agree-tos --no-eff-email \
  -m you@example.com \
  -d example.com
```

If that succeeds, certs land at `/etc/letsencrypt/live/example.com/` in
the `letsencrypt` volume.

## Run

```bash
docker compose up -d
```

Test:

```bash
curl -s https://example.com/
```

## How the renewal hook works

certbot runs every 12 hours. When a cert is renewed (≤ 30 days before
expiry), certbot invokes `/renew-hook.sh`, which calls
`docker kill -s HUP tinylb`. tinylb re-reads its cert files via
`ReloadableCertResolver` and swaps in the new cert atomically.

Existing TLS connections are not interrupted. New handshakes use the new
cert immediately.

## Clean up

```bash
docker compose down -v       # -v also removes the letsencrypt volume
```
