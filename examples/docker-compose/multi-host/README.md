# Multi-host routing example

One tinylb routes two virtual hosts (`a.local`, `b.local`) to two separate
backend pools.

## Run

```bash
docker compose up -d
```

Test each host:

```bash
curl -s -H 'Host: a.local' http://localhost:8080/ | grep '^Hostname'
# → a1 or a2

curl -s -H 'Host: b.local' http://localhost:8080/ | grep '^Hostname'
# → b1 or b2
```

For real browser testing, add lines to `/etc/hosts`:

```
127.0.0.1   a.local
127.0.0.1   b.local
```

Then visit `http://a.local:8080/` and `http://b.local:8080/`.

## Clean up

```bash
docker compose down
```
