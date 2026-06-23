# xiaomiproxy

Key-injecting reverse proxy for the **opencrabs × Xiaomi MiMo** collab.

The OpenCrabs client points `[providers.xiaomi] base_url` at this proxy and sends
**no real key**. The proxy holds the real Xiaomi key (baked in from `.env` at
build time), injects it server-side, and streams the response back. So the key
never ships inside the open-source OpenCrabs binary.

```
OpenCrabs client ──(no key)──▶ nginx (xiaomi-collab.opencrabs.com)
                                   │
                                   ▼
                          xiaomiproxy :8787 ──(real key)──▶ api.xiaomimimo.com
```

## Endpoints

- `POST /v1/chat/completions` — streaming pass-through (SSE), key injected.
- `GET  /v1/models` — pass-through, key injected.
- `GET  /health` — `ok`.
- `GET  /stats` — usage stats as JSON (protected by `APP_TOKEN` if set).

## Vision model routing (built-in)

If a request includes `image_url` content parts and targets a non-vision model
(like `mimo-v2.5-pro` or `mimo-v2-flash`), the proxy silently swaps the model
to `mimo-v2-omni` — the vision-capable default. No client-side changes needed.

| Model | Text | Vision |
|-------|------|--------|
| `mimo-v2.5-pro` | ✅ | ✅ (auto-routed to `mimo-v2-omni`) |
| `mimo-v2-flash` | ✅ | ✅ (auto-routed to `mimo-v2-omni`) |
| `mimo-v2-omni` | ✅ | ✅ (native, no swap) |
| `mimo-v2.5` | ✅ | ✅ (native, no swap) |

Text-only requests are never touched — only requests containing `image_url`
blocks in the messages array trigger the swap.

## Usage tracking

Every request is recorded to a local SQLite database (`STATS_DB`, default
`./xiaomiproxy.db`). The `/stats` endpoint returns aggregated usage as JSON.

**Tracked per request:** IP, endpoint, model (requested + used), status code,
latency (ms), vision swap flag, timestamp.

**`/stats` response:**

```json
{
  "total_requests": 1234,
  "requests_by_model": {"mimo-v2-omni": 800, "mimo-v2.5-pro": 300, "mimo-v2-flash": 134},
  "requests_by_endpoint": {"/v1/chat/completions": 1200, "/v1/models": 34},
  "unique_ips": 45,
  "vision_swaps": 50,
  "errors": 10,
  "avg_latency_ms": 234.5,
  "hourly": [{"hour": "2026-06-22T10:00", "count": 42}]
}
```

Protected by `APP_TOKEN` if set (same auth as the proxy endpoints). Set
`STATS_DB=/var/lib/xiaomiproxy/stats.db` in production so the DB survives
binary restarts.

## Access control (time-boxed free window)

Pragmatic, not cryptographic — an open-source client can't hold an unextractable
secret. The Xiaomi key stays safe regardless; the worst case is bounded
free-riding on the proxy.

1. **Cutoff date** — after `CUTOFF_DATE` (default `2026-06-27`, UTC) every call gets `410 Gone`.
2. **App token** — if `APP_TOKEN` is set, clients must send it (`Authorization: Bearer <token>` or `X-OpenCrabs-Token: <token>`). Rotatable.
3. **User-Agent allowlist** — `REQUIRE_OPENCRABS_UA=true` requires a UA containing `opencrabs`.
4. **Per-IP rate limit** — `RATE_MAX_PER_MIN` (default 60), keyed off `X-Forwarded-For`.

## Config

`XIAOMI_MIMO_API_KEY` and `APP_TOKEN` are **baked in at build time** from `.env`
(see `.env.example`); a runtime env var of the same name overrides. Everything
else is runtime-only. See `.env.example` for the full list.

## Build

```bash
cp .env.example .env      # set XIAOMI_MIMO_API_KEY (+ APP_TOKEN) — already done
cargo build --release     # bakes the secrets in; binary at target/release/xiaomiproxy
```

The binary is self-contained — no key file needed on the server.

## Deploy to neuraprod (ssh alias `prods`)

```bash
# 1. Build (locally or on the box). If cross-building for Linux from macOS,
#    build on the server instead, or use a linux target/musl.
cargo build --release

# 2. Ship the binary
scp target/release/xiaomiproxy prods:/tmp/xiaomiproxy
ssh prods 'sudo mv /tmp/xiaomiproxy /usr/local/bin/ && sudo chmod 755 /usr/local/bin/xiaomiproxy'

# 3. systemd unit
scp deploy/xiaomiproxy.service prods:/tmp/
ssh prods 'sudo mv /tmp/xiaomiproxy.service /etc/systemd/system/ && sudo systemctl daemon-reload && sudo systemctl enable --now xiaomiproxy'
ssh prods 'curl -s localhost:8787/health'   # -> ok

# 4. nginx + TLS
scp deploy/nginx-xiaomi-collab.conf prods:/tmp/
ssh prods 'sudo mv /tmp/nginx-xiaomi-collab.conf /etc/nginx/sites-available/xiaomi-collab.opencrabs.com \
  && sudo ln -sf /etc/nginx/sites-available/xiaomi-collab.opencrabs.com /etc/nginx/sites-enabled/ \
  && sudo certbot --nginx -d xiaomi-collab.opencrabs.com \
  && sudo nginx -t && sudo systemctl reload nginx'
```

Runtime overrides (optional) go in `/etc/xiaomiproxy.env` (the unit reads it if present).

## Smoke test

```bash
# Through the proxy (no client key needed during the free window):
curl -N https://xiaomi-collab.opencrabs.com/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"mimo-v2.5-pro","stream":true,"messages":[{"role":"user","content":"hi"}]}'
```

## Remaining client-side step (optional, for the app token)

If you set `APP_TOKEN` on the proxy, the OpenCrabs client must send it. Today the
client sends an empty Bearer (the proxy injects the real key regardless). To wire
the token, have `try_create_xiaomi` send `Authorization: Bearer <APP_TOKEN>` (or
the `X-OpenCrabs-Token` header) during the keyless window. Until then, run the
proxy with `APP_TOKEN` unset (rate-limit + cutoff still apply).
