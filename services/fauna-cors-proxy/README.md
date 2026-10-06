# fauna-cors-proxy

A stateless CORS forwarder for provisioning provider APIs. The only fauna.social
dependency for web users; native clients never talk to it.

## What it does and does not do

- Forwards `POST /<provider>/<path>` to `https://api.<provider>.com/<path>`.
- Preserves the request body byte-for-byte and forwards all headers except `Host`.
- Adds permissive CORS headers so browser WASM can call it.
- **Does not** parse bodies, log credentials, store state, or see anything
  the provider's own origin server wouldn't see (TLS terminates at the provider).

### ACME (Let's Encrypt) routes

Two extra prefixes carry the browser (wasm) DNS-01 **certificate order**
(`fauna-client-dns::acme_pure`), which can't reach Let's Encrypt cross-origin:

- `acme-le` → `https://acme-v02.api.letsencrypt.org` (production)
- `acme-le-staging` → `https://acme-staging-v02.api.letsencrypt.org` (staging)

ACME requests are JWS-signed end-to-end, so the proxy stays credential-blind — the
same invariant as the provider routes. The CA returns **absolute** follow-up URLs
(newOrder/authz/challenge/finalize/cert) pointing at the LE host; the client
re-points each at `<proxy>/acme-le/<path>` before fetching. The proxy preserves
all response headers, so the `Replay-Nonce` / `Location` headers ACME relies on
pass through intact. See `docs/goal/architecture/nest/tls-certificates.md` § C.

## Self-hosting

If you don't trust `proxy.fauna.social`, run your own:

```bash
cargo run -p fauna-cors-proxy --release
```

Point your web client at it by setting `FAUNA_PROXY_URL=https://your-host:8080`
at build time. The `fauna-provisioning` crate reads it via `option_env!` in
`libs/fauna-provisioning/src/proxy.rs`. Setting it before `just wasm-onboarding`
(or any `wasm-pack` invocation) bakes it into the WASM binary; native builds
ignore it (they bypass the proxy entirely).

## Supported providers (per Phase 0 CORS audit)

Per the provisioning CORS audit, only 4 of the 9 listed
providers actually need the proxy (Cloudflare, Vultr, Gandi, Namecheap). The
other 5 (Hetzner, Porkbun, OVH, DigitalOcean, Linode) allow direct browser
calls via `Access-Control-Allow-Origin: *`. All 9 are listed in `BASE_URLS`
anyway for future-proofing — provider CORS policies can tighten over time.

## Deployment

The reference deployment target is `proxy.fauna.social` (the baked-in default
in `libs/fauna-provisioning/src/proxy.rs`; not yet live — tracked internally).

A container image builds from the repo root (the crate is a workspace member,
so the whole workspace is the build context):

```bash
docker build -f services/fauna-cors-proxy/Dockerfile -t fauna-cors-proxy .
docker run -d --restart unless-stopped -p 8080:8080 fauna-cors-proxy
```

The service is plain HTTP on `$PORT` (default 8080) with a `/healthz` probe;
terminate TLS in front of it (any reverse proxy / the host's ACME tooling).
It is stateless — no volume, no config file.

## Not to be confused with `bins/fauna-router`

`bins/fauna-router` (renamed from `fauna-proxy` 2026-07-05 to resolve this
naming collision) is an unrelated service — a multi-nest *internal* routing
proxy (sits in front of many nests, routes by `actor_id`, uses WireGuard
tunnel IPs to reach backends). This service (`fauna-cors-proxy`) is a thin
*external* CORS forwarder used only by the web client.
