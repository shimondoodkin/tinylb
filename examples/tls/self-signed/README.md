# Self-signed TLS for local dev

Run tinylb with HTTPS on `localhost:8443` against a self-signed cert and
a single plaintext backend on `127.0.0.1:9000`.

## Run

```bash
./generate-cert.sh
tinylb lb.toml
```

In another terminal, start any HTTP server on :9000:

```bash
python3 -m http.server 9000
```

Test:

```bash
curl -sk https://localhost:8443/
```

## Rotating the cert

`openssl req -x509 ...` to overwrite `fullchain.pem` and `privkey.pem`,
then `kill -HUP $(pidof tinylb)` (or simply save `lb.toml` and wait up to
10 seconds for the mtime poll to pick it up). Existing connections are
untouched; new handshakes use the new cert.
