# fauna.social front door

Owns: front-door, web-app-trust
Status: ratified
Authority: the public front door of fauna.social — the door binary
(`services/fauna-front-door`), the public-vhost topology (apex, `www`, `app`,
`proxy`), the door's TLS acquisition policy, its security architecture
(headers, rate limits, tenancy rules), and the serving box's shape + deploy
contract (hardened systemd units, versioned-content swap, restricted deploy
path, push-triggered workflows); ACME order/renew mechanics →
architecture/nest/tls-certificates.md, the CORS proxy's forwarding behavior +
provider policy → architecture/provisioning/registry.md, the site and SPA
builds → architecture/build-system.md, release trust →
architecture/release-integrity.md.

Ratified 2026-08-21, executing the 2026-08-10 user direction: hosting target
is a Hetzner Cloud VPS; the front-door software choice was deferred to the
deciding session under the user's stated criterion — **simple code beats
complex but well-tested code in the current AI-driven-vulnerability climate**
(a small bespoke attack surface over widely-deployed shared software whose
exploits amortize across every deployment).

## Goal

fauna.social's public web surface is served by the smallest thing that can
serve it well, treated as **release infrastructure, not a marketing box**: the
apex serves the install guides and the official-apps page that tell users
which download is genuine, so whoever
controls the door influences what software users run. Everything below follows
from that framing.

What the door must do, and nothing more:

- serve static vhosts over HTTPS: the apex site, a `www`→apex redirect, and
  the `app` SPA;
- answer everything else on those hosts with the site's burrow 404 (SPA
  client routes excepted — § vhost table);
- reverse-proxy `proxy.fauna.social` to the CORS proxy on loopback;
- acquire and renew its certificates itself (HTTP-01), and always answer TLS;
- send hardened response headers and rate-limit its one cost-bearing route.

## The decision: an in-house door binary

**The front door is our own Rust binary — `services/fauna-front-door` — not
an off-the-shelf server.** A few hundred lines of glue over foundations
already in our supply chain and already through our own review pipeline:
axum/hyper/rustls/tower (the nest's serving stack), the ACME machinery that
already issues certificates in production (nest HTTP-01 loop and the
client-side order drivers — architecture/nest/tls-certificates.md), static
vhost serving prior art in the nest's web-content stack, reverse-proxy prior
art in `bins/fauna-router`. **Zero new dependencies.** The cost accepted with
it: we own our uptime bugs.

Rejected alternatives, and why (decision record):

- **Caddy** — memory-safe and a decent CVE history, but an entire new supply
  chain we can never read (the dep-source review rule), placed under the
  org's most public asset; it does far more than needed (admin API, dynamic
  config, on-demand TLS, module ecosystem), and its ubiquity makes it a prime
  shared-exploit target — the exact profile the deciding criterion scores
  down.
- **nginx + certbot** — memory-unsafe C plus a Python dependency tree.
- **static-web-server (Rust)** — still a new third-party tree, and no ACME,
  so it solves less than half the problem while costing the whole supply
  chain.

## Public-vhost topology

| Host | Serves | Miss behavior |
|---|---|---|
| `fauna.social` | the built site (`just site` output) — and, later, release artifacts alongside it | `404.html` (burrow page), status 404 |
| `www.fauna.social` | permanent redirect (308) to the same path on the apex | n/a |
| `app.fauna.social` | the built SPA (canonical hosted origin — the name is baked into shipped code as the default CORS origin; never rename) | `index.html`, status 200 (SPA client routing); real asset misses under content-hashed paths still 404 |
| `proxy.fauna.social` | reverse-proxy pass to `fauna-cors-proxy` on loopback (forwarding semantics owned by architecture/provisioning/registry.md) | upstream's answer; 502 with an empty body if the unit is down |

Port 80 serves exactly two things: `/.well-known/acme-challenge/*` and a
redirect to HTTPS — the same shape the nest's HTTP-01 listener has.

**Tenancy is closed.** The future `nest-broker` and `push` relay are **never
co-tenants** of this box — a compromise of the public web box must not touch
managed-mode brokering or push. New public names get their own placement
decision here first.

## Which origin a user loads the app from — the trust claim (ratified 2026-09-24)

The web app is the one client whose code the **nest** chooses: every nest
serves the bundled SPA at `/app/` (what that path answers, and the admin's
choice over it, is owned by
[`../behavior/web-content-hosting.md`](../behavior/web-content-hosting.md)
§ Same-origin security model → *The nest-served `/app/` and the central
origin*), and that code holds the user's master secret (same doc, § The
decisive fact). The six native apps arrive through stores, releases and
source builds — never from the nest. So a malicious or compromised nest can
serve a malicious web app to every browser that loads the app *from it*, and
no licence, signature or nest-side control touches that
([`release-integrity.md`](release-integrity.md) § Defense priority; the
2026-09-24 licence round examined it as its first threat).

**The claim: loading the app from `app.fauna.social` moves the code-trust
from N nest operators to the one origin the association runs.** It closes
"a malicious nest swaps the app" for every user whose *entry point* is the
central origin — a bookmark, a typed address, a PWA install — because the
code then comes from the association's box and the nest is only ever spoken
to over the API. It does **nothing** for a user who follows a link the nest
hands out: a malicious nest can redirect to a lookalike, and no origin can
protect a user from where they were sent. Three consequences:

- **The user's own typed origin always wins.** Nothing on any nest can
  override where a user chooses to load the app from; the per-nest redirect
  (web-content-hosting.md, above) only changes what that nest's own `/app/`
  answers.
- **Self-hosters keep the nest-served app, and it is exactly right for
  them.** A user who runs their own nest already trusts that box with
  everything; the central origin would only add a second party to trust.
  The nest-served `/app/` is also the out-of-the-box and the offline/LAN
  path (a fresh nest with no internet still needs its app), so it stays.
- **The guide says it in the user's voice** (`docs/guides/install.md`): for
  a nest you do not run, use a native app or the central origin, not the
  nest's own `/app/`.

The SPA's base path is `/app` on every origin it is served from
(`apps/fauna-web/svelte.config.js` `paths.base`): the nest mounts the build
there, and the `app` vhost serves the same tree under the same prefix, so
one URL shape — `<origin>/app/` — names the app everywhere and the redirect
has one target to build.

The central origin is only as trustworthy as its box and deploy key — the
same dev-tier compromise class release-integrity.md § Defense priority
accepts — which is why it must be *checkable* and not merely central:
[`release-integrity.md`](release-integrity.md) § Release signing →
*Web-app verifiability* owns that (a reproducible SPA build, a published
manifest of the served asset hashes, an entry in the transparency log).

**GitHub Pages is not the origin — recorded, not to be rebuilt.** The
earlier design proposed removing app serving from the nest entirely and
hosting the app on GitHub Pages behind `app.fauna.social`. Its
hash-manifest idea (§ 1.7 there) survives in the verifiability decision;
both hosting halves are superseded — the nest keeps serving `/app/`
(web-content-hosting.md invariant 3 reserves the path for the SPA), and the
origin is the association's own hardened box (§ Public-vhost topology) —
because Pages puts a third party in the code path of every web user
(coercion, geo-blocking, IP logging at load) and its deploy credential is
the same GitHub account whose compromise is the in-source threat, so it is
no trust improvement over the ratified box. A Pages mirror as an
availability fallback stays possible, but each extra origin is one more to
compromise and one more a user can be redirected to; none is planned.

## TLS policy

- **HTTP-01, never DNS-01 — no DNS API token exists on the box.** A
  zone-write token on a public web box is domain takeover: site, mail
  interception, download-link redirect. The box's only secret is its cert keys
  (plus the host SSH keys). This is the door's one non-negotiable.
- One ACME account, one order covering the four public names as SANs;
  renewal with the same ~30-day lead the nest uses.
- **The order/renew driver is shared with the nest, not forked.** The nest's
  HTTP-01 flow (challenge state + well-known route + renew loop) moves to a
  shared seam both binaries consume; mechanics stay owned by
  architecture/nest/tls-certificates.md.
- **The door always answers TLS**: a self-signed floor certificate at first
  boot, hot-swapped the moment a trusted certificate is issued or renewed —
  the nest's valid-else-floor principle, minus its SNI machinery (four fixed
  names).

## Security architecture

- **Headers (static vhosts):** `X-Content-Type-Options: nosniff`,
  `X-Frame-Options: DENY`, `Referrer-Policy: strict-origin-when-cross-origin`,
  a minimal `Permissions-Policy`, HSTS once certificates are trusted, and a
  strict `Content-Security-Policy` sized to what the site actually ships
  (the site ships zero JS; the SPA's CSP is sized to the SPA build — exact
  values are implementation, pinned by tests). Content-hashed assets get
  long-lived immutable caching; HTML short.
- **Rate limiting is minimal and targeted:** the proxy vhost — the only
  route that spends someone else's resources — gets a per-peer-IP token
  bucket (in-house, same shape as the nest's anonymous-surface throttling);
  static vhosts get only a global concurrency cap. Nothing else — the
  Hetzner Cloud firewall (80/443 only) is the outer wall.
- The CORS proxy binds **loopback only** on this box; the door is the only
  process on 80/443.

## The box: shape and deploy contract

Minimal Debian + unattended-upgrades. **No Docker, no Watchtower** — a
Docker socket is root-equivalent held by third-party images; the door and the
CORS proxy run as **two static Rust binaries under hardened systemd units**:

- Hardening set (both units): `DynamicUser=yes`, `NoNewPrivileges=yes`,
  `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes`,
  `PrivateDevices=yes`, `ProtectKernelTunables/Modules/Logs=yes`,
  `ProtectClock=yes`, `ProtectControlGroups=yes`, `RestrictNamespaces=yes`,
  `LockPersonality=yes`, `MemoryDenyWriteExecute=yes`,
  `RestrictAddressFamilies=AF_INET AF_INET6`,
  `SystemCallFilter=@system-service`, `CapabilityBoundingSet=` (empty; the
  door alone keeps `CAP_NET_BIND_SERVICE` in both the bounding and ambient
  sets — ambient capabilities must be inside the bounding set — for 80/443).
  Both units carry `MemoryMax` + `CPUQuota` so neither can starve the box.
- The door's writable state is exactly `StateDirectory=fauna-front-door`
  (ACME account + certificates); content docroots are read-only to it.
- **Content lands in versioned dirs with an atomic symlink flip:**
  `/srv/fauna/{site,app}/releases/<stamp>/` rsync'd complete, then `current`
  renamed onto it; the door serves through `current` and tolerates the swap;
  old releases pruned (keep 3). Binaries arrive the same way +
  `systemctl restart`.
- The deploy identity is a dedicated user whose SSH key is restricted
  (`command=`-forced rsync target, no pty, no forwarding), owning only the
  release dirs — a leaked deploy key can replace content, never touch
  certificates, units, or the system, and **read nothing**. The forced
  command never runs the client's command line: it accepts the one exact
  `rsync --server` shape a deploy sends (receive-only, into
  `{site,app}/releases/<stamp>/`, `<stamp>` from an alphabet with no `/` or
  `.`) plus `flip`, and starts rsync from its own literals. "Owning only the
  release dirs" is load-bearing: the deploy user's `authorized_keys` is
  root-owned and outside its reach, or a write primitive becomes a shell.
- **The forced command confines WHERE the key writes, not WHAT — and the door
  is the second reader of what it writes.** The one option word accepted is
  the client's, and `rsync -az` keeps symlinks (`-l`) and specials (`-D`), so
  a release can carry a link aimed anywhere the door can read — its own
  `StateDirectory`, holding the issued certificate keys — or a FIFO that parks
  a request. **"Read nothing" is a claim about the key's own reach only if a
  pushed shape cannot borrow the door's**, so both ends refuse those shapes:
  the receiver is started with `--munge-links --no-specials --no-devices`, and
  the door serves only **regular files whose canonical path lies inside the
  canonical docroot** — on the main static path and on the burrow-404 and SPA
  fallbacks alike. Neither layer alone covers both arms, and the release path
  and its `releases` parent must each resolve to themselves, so nothing is
  created through a planted parent.
- **Deploys are push-triggered, path-filtered workflows** for both the site
  and the SPA (user-ratified 2026-08-10, deliberately reversing the old
  manual-only stance): `origin/main` is leak-checked pre-merge by the cheap
  gates, and the site build is the async `just site` gate, so a push that
  reaches main is already vetted.

## Relationship to neighbouring docs

- **architecture/nest/tls-certificates.md** owns ACME mechanics (order,
  renew, retry budgets); this doc owns only the door's *policy* (HTTP-01,
  SAN set, floor).
- **architecture/provisioning/registry.md** owns the CORS proxy's behavior
  and its deployment-gap claim; this doc owns where and how it is placed.
- **architecture/build-system.md** owns building the site (§ Public website
  build) and the SPA; this doc owns serving what those builds produce.
- **architecture/release-integrity.md** owns release trust; this doc's
  release-infrastructure framing is placement only.

## Implementation status today

Built 2026-08-21 (the deciding session), NOT yet deployed:

- **The shared HTTP-01 seam exists:** `libs/fauna-acme-http01` — lifted from
  the nest, which now re-exports it — carrying the challenge state + port-80
  router/listener, the order flow (with a `test-helpers` client-injection
  seam), the certificate inspectors, the persisted retry budget, and the
  hot-swap `ReloadableCertResolver`. Real-wire proof: `tests/pebble_http01.rs`
  (`just e2e-pebble-http01` — #[ignore], needs Docker; demonstrated green
  2026-08-21: a certificate issued end-to-end through the production
  challenge listener against pebble).
- **The door binary exists:** `services/fauna-front-door` — vhost dispatch,
  hardened headers, burrow 404 + SPA fallback, loopback proxy pass, per-IP
  token bucket, TLS floor + hot-swap, renewal task; each behavior unit-tested.
- **The docroots are contained at the door** (2026-09-20): `src/vhost.rs`
  resolves every static request itself and opens only what it resolved — the
  docroot is canonicalized per request (`current` flips underneath a running
  door), each path segment is percent-decoded and refused if it is `.`, `..`
  or a decoded separator, and what remains must canonicalize to a regular file
  inside the canonical docroot. A link out of the tree, a FIFO, a socket and a
  device are all misses rather than opens; a link that stays inside the
  docroot is ordinary content. Directory pages keep working: a directory is
  served through its own `index.html`, and one named without a trailing slash
  still answers `307` to the slashed form. Pinned by the door's containment
  tests and, receiver-side, by a real-rsync push of a link and a FIFO in
  `tests/scripts/test_door_deploy_receive.py`.
- **Deploy artifacts staged** in `services/fauna-front-door/deploy/`: the two
  hardened units, the forced-command deploy wrapper, and DRAFT push-triggered
  workflows — the migration track swaps them in at the cutover. The wrapper's
  confinement is pinned by `tests/scripts/test_door_deploy_receive.py`
  (crafted commands + a real rsync round trip; hardened 2026-09-19 — as first
  staged it accepted `..` traversal and the read direction).
- **How binaries arrive is NOT settled.** § The box says "the same way", but
  the wrapper has — deliberately — no binaries arm: a CI-held key that can
  replace the door binary touches "the system" this section says it never
  can. Until that is ruled on, binaries arrive by the admin's own SSH at
  provisioning, not by the deploy key.
- **The CORS proxy has the loopback bind control** (`BIND_ADDR`,
  artifact-set; container default unchanged).
- **`release.yml` ships both box binaries** for x86_64 + aarch64 Linux
  (`fauna-front-door`, `fauna-cors-proxy` in both crate loops) — since
  2026-08-31 from the public repository's copy at
  `.github/workflows/release.yml`
  ([`release-integrity.md`](release-integrity.md) § Release signing → *When a
  release workflow publishes* owns the port).

NOT built/done: the box itself (user's VPS order), DNS staging and the
nameserver switch, workflow activation, and the Azure teardown — all owned by
the migration track. Until the cutover the site serves from Azure Static Web
Apps (apex + `www` attached 2026-08-06), and `app.fauna.social` /
`proxy.fauna.social` do not resolve.

**§ Which origin a user loads the app from — RATIFIED 2026-09-24, the origin
not live.** Until the Track H cutover every web user loads the app from a
nest, so the claim is ratified and unrealized: `docs/guides/install.md`
carries the honest form today (native app for a nest you do not run; the
central origin named as not yet live) and flips to the bookmark instruction
at cutover; the `app` vhost's `/app` prefix is a cutover detail. The per-nest
redirect's nest half is built (the state row, the admin kinds, the `302` and
its `setup.status` projection — web-content-hosting.md § Implementation status
today); its app half is built on tui (the lead app) and the `nest` hint on web
(the same doc's status and `onboarding.md` § 2 → *Nest hint*), while the
six-app trickle-down and the verifiability pieces are captured on their
owners' rows.
