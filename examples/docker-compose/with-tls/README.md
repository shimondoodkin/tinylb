# TLS termination with self-signed certs

tinylb terminating HTTPS on :8443 with a self-signed cert, in front of two
plaintext backends.

## Run

```bash
./generate-cert.sh           # writes certs/fullchain.pem and certs/privkey.pem
docker compose up -d
```

Hit it (skip cert verification — it's self-signed):

```bash
curl -sk https://localhost:8443/ | grep '^Hostname'
```

The cert is hot-reloadable — replace `certs/fullchain.pem` and
`certs/privkey.pem`, then `docker kill -s HUP tinylb`. New TLS handshakes
use the new cert immediately; existing connections are untouched.

## Clean up

```bash
docker compose down
rm -rf certs
```
