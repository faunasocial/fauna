# Installer: Docker — target state

Owns: docker-packaging
Status: ratified
Authority: the published nest Docker image + compose bundles — the multi-stage build (cache-mount strategy, --locked, per-stage toolchains), the s6 supervision tree + per-role UID/sandbox wiring, the entrypoint first-run + every-boot reconcile contract, the FAUNA_* env catalog (bucket-classified), the supervisor sidekick socket wire shape (the wire-level detail `mail-bridge-lifecycle.md` § Wire shapes delegates here), scan sidecars + watchtower, ports + /data layout; defers image tags/release channels to architecture/build-system.md § Image tags & channels, UID-isolation/sandbox rationale to architecture/security.md § UID isolation, bridge lifecycle semantics to behavior/mail-bridge-lifecycle.md, SNI routing policy to behavior/caldav-server.md § Network exposure, the home host-networking bundle to installers/home-relay.md, VPS provisioning that consumes the image to installers/vps.md.

The recommended Docker deployment for the nest server — a single multi-arch container with s6-overlay process supervision, consumed directly by client-side provisioning via `fauna-provisioning`. Last verified: 2026-08-26 (docs-consistency sweep — re-checked the multi-stage build's binary list + `libfauna_ffi.so` feature flags, the s6 UID table, the supervisor allowlist, the retired-env-var claims, the golang exact-patch pin, and the `fauna-atproto-bridge` `contents.d` wiring against `Dockerfile`, `docker/entrypoint.sh`, `cmd/fauna-supervisor/supervisor.go`, `docker/s6/user/contents.d/`, and `bins/fauna-bridges/go.mod`; all held — no code change since the prior verify affected this doc's claims). | Source: `Dockerfile`, `docker/`, `docker-compose.yml`, `.github/workflows/build-nest-image.yml`.

## Implementation status today

Fully implemented and in production: the image is what the closed-alpha nests run (Watchtower tracks `:latest`; release channels + the internal-CI production gate are owned by `architecture/build-system.md` § Image tags & channels). CI builds each arch **separately on the self-hosted CI runner** (amd64 built `--load` + smoke-tested before push; arm64 native; **no QEMU** — the Go stage must never run emulated, § Multi-Stage Build) — `.github/workflows/build-nest-image.yml`.

Recorded gaps / migration targets:

- **Images built 2026-09-02 → 2026-10-04 carry NONE of § Feature flags' three bridge planes — fixed in the Dockerfile 2026-10-04; a box keeps the gap until it pulls an image built after the fix.** The Dockerfile spelled the flags `--features fauna-nest/bluesky,fauna-nest/nostr,fauna-nest/activitypub`; once `bins/fauna-nest` listed itself as a dev-dependency (2026-09-02), cargo bound `fauna-nest/<feat>` to that dev-dep edge, which a build never activates, and dropped all three with exit 0 — the staging box answered `/.well-known/nodeinfo`, `/.well-known/nostr.json` and `/.well-known/atproto-oauth-client` with the landing page while its health probe said ok. The flags are now bare (`build-system.md` § the Dockerfile's build-speed levers, *One merged native cargo invocation*), pinned twice: tier_1 `test_cli_features_never_name_a_self_dev_dep.py` refuses the `<pkg>/<feat>` form for any crate with a self dev-dep, and the image build's smoke step probes one route per plane on the built image before anything is pushed.
- **The plugin supervisor + catalog-driven `allowedServices` (§ Supervisor sidekick socket, ratified 2026-09-05): unbuilt** — `allowedServices` is the three-entry compiled list today.
- **The ACME contact is a constant (none) and the CA a constant (Let's Encrypt) — ruled 2026-10-01, the knob removed 2026-10-02.** The entrypoint reads no contact variable and takes no `--acme-email`. Owner: `../nest/tls-certificates.md` § ACME settings — constants, not choices. `FAUNA_ACME_DIRECTORY_URL` is test IPC for the tier_4 pebble harness and stays. This doc's env rows classify, never normalize.
- **`FAUNA_PAIR_WITH` is removed, with no seed form in its place (ruled 2026-10-01, built 2026-10-02).** Which nest to relay from is the user's choice, made in the app: the workers read the app-set pairing row, and the variable, the entrypoint's `--pair-with` (with its now-dead `--mode` twin) and the `[pairing]` / `[forwarding]` tables are gone. Owner: `../nest/private-mode.md` § Implementation status today.
- **`FAUNA_IMAP` is gone (2026-10-01).** Nothing read it after the `entrypoint.sh` seeding of the legacy `services.json.bridge` flag was deleted (2026-07-10); cloud-init's emission of it for a mail box — the last trace — is removed, and a unit test pins that a mail box's compose names no such variable.
- DKIM has no DNS auto-reconcile (a re-mint can go stale — caught by deploy-verify gate 6); tracked nest-side.
- **✅ CLOSED 2026-07-23 — the `fauna-atproto-bridge` bundle-wiring packaging bug.** The service directory (`docker/s6/fauna-atproto-bridge/{run,down,type,dependencies.d}`), its dedicated `fauna-atproto` UID, the Dockerfile build/COPY step, and the supervisor allowlist entry were all present (§ s6-overlay Services, § Supervisor sidekick socket), but `docker/s6/user/contents.d/` — unlike every other flag-gated service (`fauna-mail-bridge-mta`/`-mda`, `fauna-iroh-relay`) — had no `fauna-atproto-bridge` entry. s6-rc only instantiates services reachable from a bundle's `contents.d`, so the built image never brought the service under live supervision at all: `s6-svc -u /run/service/fauna-atproto-bridge` (what `set_atproto_enabled` sends over the supervisor socket) had no target to act on, and enabling ATProto hosting silently did nothing. The marker file now exists. **Pinned headlessly by tier_1 `tests/e2e-unified/tests/test_s6_services_are_supervised.py`** — "is every longrun reachable from a bundle" is a static property of the checked-in tree, so it needs no image build (plus the inverse check: no `contents.d` marker naming a service that does not exist, which would fail bundle compilation at image build). tier_3 could never catch this class — it drives the binaries directly, bypassing `docker/s6/*` (the documented tier_3 blind spot); a tier_4 test would additionally prove the service actually comes up, which the static check does not claim. Owner: `../../behavior/atproto-pds-bridge.md` § Implementation status today (S1 packaging).

## Goal

Provide the recommended deployment shape for the nest server: a single multi-arch (amd64 + arm64) container image, built via a multi-stage Dockerfile, with all nest processes supervised by s6-overlay under a non-root runtime user. Apps provision instances directly using the `fauna-provisioning` crate; no central registry is required.

> **Feature flags:** `fauna-nest` is built with `--features bluesky,nostr,activitypub` (mail/IMAP/CalDAV handlers are always compiled — there is no `email` cargo feature). Each bridge shipped on its own user-approved release decision, dark until then: `nostr` on 2026-07-16, `activitypub` on 2026-07-16 (gated on the handle-derived username mint landing and the F1 SSRF fix). **Shipping `activitypub` federates nothing by itself** — the routes mount with the feature, but only per-actor enablement (`ap_accounts.enabled`, client-only) exposes any content, unlike the server-wide `nostr` relay → [`../../behavior/activitypub.md`](../../behavior/activitypub.md) § Implementation status today. `test-hooks` is never enabled in the published image.

## Image

**Registry:** `ghcr.io/faunasocial/nest:latest`

| Property | Value |
|----------|-------|
| Architectures | amd64 (x86_64), arm64 (aarch64) |
| Base image | `debian:bookworm-slim` |
| Process supervisor | s6-overlay v3.2.0.2 |
| Runtime user | `fauna` (UID 1000) |

Runs on any Linux host with Docker or Podman. Also works on Docker Desktop (macOS, Windows) for development.

## Multi-Stage Build

The image is built in layered stages (BuildKit). Dependency caching is **BuildKit
cache mounts**, not stub stages: the rust stages mount the cargo registry + target
dir as `type=cache` mounts (with `CARGO_UNSTABLE_CHECKSUM_FRESHNESS` so cached
artifacts stay fresh across COPY mtime churn), so source-only changes don't
recompile external crates. There is no `rust-deps` stub stage. **Every cargo
invocation passes `--locked`** — the build must fail rather than silently update
`Cargo.lock` (release-integrity contract; `architecture/release-integrity.md`).

**Rust** (`rust:bookworm`): `rust-native` builds `fauna-nest`,
`fauna-sandbox`, `fauna-sni-router`, `fauna-iroh-relay`, **and `libfauna_ffi.so`**
(built `--no-default-features --features labeler` to match `just mail-bridge-ffi`
— the ABI the checked-in `libs/fauna-mail-go/` bindings were generated against;
the `labeler` half is ABI-load-bearing — dropping it is a linker failure, recorded
in the Dockerfile). A parallel `rust-wasm` stage builds the WASM modules for the
web SPA.

**Go** (an exact-patch-pinned `golang:1.26.8-bookworm` — never a floating
`golang:1.26-bookworm` tag, so the shipped toolchain is reproducible and
`govulncheck` scans exactly what ships; kept in sync with
`bins/fauna-bridges/go.mod`'s `toolchain` directive by a dedicated
merge gate — `build-system.md`
§ Go toolchain pin), `--platform=$BUILDPLATFORM` **cross-compile** — Go must
never run emulated; the Rosetta/QEMU SIGCHLD deadlock is recorded in the
Dockerfile: the `bridges-builder` stage builds three binaries from the
same `bins/fauna-bridges` module — the **cgo** `fauna-mail-bridge` (MTA/MDA
roles) and the **cgo** `fauna-atproto-bridge` (out-of-process ATProto PDS
bridge, role `atproto.pds` — it also links `libfauna_ffi.so`, for the
sealed-blob unseal), both copying `libfauna_ffi.so` from `rust-native` plus the
checked-in Go bindings and linking against them (`CGO_CFLAGS`/`CGO_LDFLAGS`
mirror `just mail-bridge-build`) — and the **pure-Go** (`CGO_ENABLED=0`)
`fauna-supervisor` sidekick.

**Web** (`denoland/deno`): `web-builder` builds the Svelte SPA against the WASM
artifacts.

**Final** (`debian:bookworm-slim`): installs s6-overlay, `jq`, `ffmpeg`,
`ca-certificates`, and `libcap2-bin`, then copies all binaries. `libfauna_ffi.so`
lands in `/usr/local/lib` (matched by the bridge binary's rpath; `ldconfig`
refreshes the linker cache), and `setcap cap_net_bind_service=+ep` lets the
bridge / SNI router bind privileged ports (25/465/587/993/443) under their
non-root per-role UIDs — the file capability is granted at `execve` regardless of
UID (Docker's default capability bounding set includes `CAP_NET_BIND_SERVICE`).

> **Workspace-member gotcha:** both rust stages COPY the workspace member trees
> (`libs/`, `bins/`, `apps/`, `services/`, `tools/`) before building. A new cargo
> workspace member whose tree isn't COPYed into **both** rust stages fails the
> build under `--locked` (lock-file canonicality — the regression).
> When adding a member outside the already-copied trees, extend the COPY lists in
> both stages.

## s6-overlay Services

**Per-role UID isolation (co-resident process trust boundary — `security.md`
§ UID isolation).** Each network-facing service runs under its OWN non-root UID
via `s6-setuidgid`, so a compromised process cannot read another role's key
material or nest's sealed store:

| Service | UID | runs as |
|---|---|---|
| `fauna-nest` | 1000 | `fauna` — owns the sealed store (`/data/nest.db`, `/data/blobs`, `/data/acme`) + per-deployment secrets |
| `fauna-mail-bridge-mta` | 1001 | `fauna-mta` |
| `fauna-mail-bridge-mda` | 1002 | `fauna-mda` |
| `fauna-sni-router` | 1003 | `fauna-router` (distinct UID; every backend it fronts — nest, the MDA's DAV listener, the PDS bridge's XRPC listener — recognises the legitimate PROXY-header writer via a shared-secret TLV, NOT `SO_PEERCRED` — those hops are TCP loopback, where peer-credential checks are unavailable — `security.md` § Co-resident process trust boundary) |
| `fauna-iroh-relay` | 1004 | `fauna-relay` (P2P relay sidecar) |
| `fauna-atproto-bridge` | 1005 | `fauna-atproto` (out-of-process ATProto PDS bridge, role `atproto.pds`) |
| `fauna-supervisor` | 0 | root (it calls `s6-svc`) |

`/data` is `fauna`(nest)-owned `0711` (others traverse, can't list); the
sensitive files under it are individually `0600`/`0700` nest-owned. Each role's
keyfile lives in its OWN `0700` subdir owned by that role's UID
(`/data/keys/{mta,mda,relay}/`); `/data/keys` itself is `root:root 0711`, and the
root-owned `/data/keys/blessed/` + `/data/keys/router/proxy-secret` are minted by
the **entrypoint as root** with the bridges LOAD-ONLY. `proxy-secret`
is `root:root 0600` in a `0700` dir, so **no bridge UID can ever read it** — that
unreadability is precisely what makes it proof of router authorship. The
run-scripts of every process that needs it (nest, router, MDA, PDS bridge) `cat`
it **as root**, before their `s6-setuidgid` drop, and export it as an env the
dropped process inherits; env, never argv, since `/proc/<pid>/cmdline` is
world-readable while `/proc/<pid>/environ` is own-UID-only. The shared
`/data/operator-hatch.toml` (deployment topology — bind addresses, scanner
endpoints, MX overrides; no secret) is `0644` so both bridges read it. A redeploy
onto a pre-split `/data` volume migrates a legacy flat `/data/keys/{role}.key`
into the per-role subdir + re-owns it (entrypoint), preserving the enrolled
service-user identity.

**Both mail bridges run under `fauna-sandbox`** (the `bridge` profile for the MTA,
`bridge-imap` for the MDA), which grants only `/data/keys/{role}` +
`/data/operator-hatch.toml` and carries `cap_net_bind_service` raised into the
ambient set so the sandboxed bridge binds its privileged ports under
`no_new_privs`. The wrapper additionally exports its Landlock enforcement status
to the binary it execs (`FAUNA_SANDBOX_LANDLOCK`), which the bridge relays to
nest as a provisioning diagnostic — artifact-set IPC, the same bucket as the
env wiring in § Environment below. The threat model and profile rationale (why
kernel-denial, not just DAC), and what that diagnostic is and is not, are owned
by `security.md` § UID isolation + § Confinement self-probe — this table is the
wiring only.

| Service | Condition | Description |
|---------|-----------|-------------|
| `fauna-nest` | Always | Main server |
| `fauna-supervisor` | Always | Nest→s6 sidekick: reads up/down commands on `/run/fauna-supervisor.sock` and dispatches `s6-svc` (runs as root; § Supervisor sidekick socket) |
| `fauna-sni-router` | Always | L4 SNI-passthrough front for `:443`; routes `mail.<domain>` → MDA CalDAV (`127.0.0.1:8444`), the relay SNI → `fauna-iroh-relay` loopback, `pds.<domain>` → `fauna-atproto-bridge` loopback (`127.0.0.1:8447`), everything else → nest (`127.0.0.1:3000`). Sends PROXY-v2 to nest, the MDA CalDAV route, and the PDS bridge route (not the relay, which doesn't parse it — `caldav-server.md` § Network exposure) |
| `fauna-iroh-relay` | Always (it stands by until the nest has a public name of its own — [`../../behavior/p2p.md`](../../behavior/p2p.md) § The relay) | P2P relay sidecar — loopback `:8445`/`:8446`, SNI-fronted at `:443`; keydir `/data/keys/relay` |
| `fauna-mail-bridge-mta` | `down` by default; up via socket when `/data/imap-enabled` exists; **never starts when `FAUNA_MODE=private`** (run-script gate — a private home box relays outbound via its public peer) | Go mail bridge, MTA role — SMTP MX (25) + submission (465/587). One process, one keypair (`/data/keys/mta/mta.key`, fauna-mta-owned) |
| `fauna-mail-bridge-mda` | `down` by default; up via socket when any of the `/data/{imap,caldav,carddav,webdav}-enabled` flags exists | Go mail bridge, MDA role — IMAP (993/143) + CalDAV. Distinct keypair (`/data/keys/mda/mda.key`, fauna-mda-owned) |
| `fauna-atproto-bridge` | `down` by default; up via socket when `/data/atproto-enabled` exists — raised by `mail_enable::set_atproto_enabled` the first time any user reaches a hosted `IntegrationLevel` via `fauna.bridges.atproto.set_integration_level` | Out-of-process ATProto PDS bridge — XRPC/OAuth on loopback `127.0.0.1:8447`, SNI-fronted at `pds.<domain>` (`fauna-sni-router` row above); terminates `pds.<domain>` TLS itself via its own sealed cert blob. Own keypair (`/data/keys/atproto/atproto.pds.key`, fauna-atproto-owned). `atproto-pds-full.md` § Wire & process topology |

**Two enable mechanisms coexist, by design:**

- **Mail bridge → event-driven supervisor socket (target; `mail-bridge-lifecycle.md` § Default-off on first claim).** The `fauna-mail-bridge-{mta,mda}` services are `down` by default. When the admin toggles `mail.enabled` in their Fauna app, nest (`fauna.bridges.set_mail_enabled`) writes the `/data/imap-enabled` flag **and** sends an `up` command on the supervisor socket; `fauna-supervisor` runs `s6-svc -u`, the run-script's flag guard passes, and the bridge starts in milliseconds — no polling, no restart. Toggling off sends `down` (SIGTERM → graceful drain) and unlinks the flag. The MDA's gate is the **OR of four sibling flags** (`imap`/`caldav`/`carddav`/`webdav`-enabled — `mail_enable.rs` maintains all four; enable semantics owned by `mail-bridge-lifecycle.md`). The flag files are **nest's output, never the admin's input** — hand-editing on a running deployment is forbidden.
- **Relay sidecar → always up, no gate.** `fauna-iroh-relay` has no enable state in the image: s6 starts it at boot and nothing switches it on or off. Whether it *serves* is the nest's answer over the relay's own sidecar channel — it is handed its certificate only once the nest has a public name of its own, and stands by until then (owner [`../../behavior/p2p.md`](../../behavior/p2p.md) § The relay). The `services.json`-polling `fauna-service-watcher` that used to gate it, the last user of that mechanism, was removed 2026-10-03.

## Supervisor sidekick socket

The `fauna-supervisor` service is the event-driven channel that lets nest bring
the flag-gated mail-bridge services up/down without filesystem polling. This is
the wire-level shape that `mail-bridge-lifecycle.md` § Wire shapes delegates here;
**it is the contract between the deploy side (this image, the reader — `fauna-supervisor`)
and the nest side (`fauna.bridges.set_mail_enabled`, the writer — `bins/fauna-nest/src/mail_enable.rs`,
Stage 1).** Both halves are now built and converged on this exact wire (the nest
side's `supervisor_command` emits byte-for-byte what `fauna-supervisor` parses).

| Property | Value |
|----------|-------|
| Path | `/run/fauna-supervisor.sock` (Unix-domain, on tmpfs) |
| Owner / mode | `fauna` : `fauna`, `0600` — only nest (which runs as `fauna`) can write; the supervisor runs as **root** so `s6-svc` can signal the service control FIFOs and so it can chown the socket |
| Framing | one JSON object per line (newline-delimited), request and reply |
| Request | `{"action": "up" \| "down", "service": "<name>"}` |
| Reply | `{"ok": true}` or `{"ok": false, "error": "<reason>"}`, one per request |
| Service allowlist | `fauna-mail-bridge-mta`, `fauna-mail-bridge-mda`, `fauna-atproto-bridge` **only** (`allowedServices`, `cmd/fauna-supervisor/supervisor.go`) — any other name is rejected, so the socket is a narrow control surface, not a general `s6-svc` proxy |
| Dispatch | `up` → `s6-svc -u /run/service/<service>`; `down` → `s6-svc -d /run/service/<service>` (SIGTERM → the bridge's graceful drain) |

The connection is long-lived-capable: nest may send both `mta` and `mda` commands
back-to-back on a single connection (the per-claim flow enables both roles at
once). A malformed line yields an error ack but does not close the connection.

**Enable flow (the two halves):** admin toggles `mail.enabled` →
`fauna.bridges.set_mail_enabled(true)` → nest **writes `/data/imap-enabled`**
(0600, owner `fauna`) and **writes `{"action":"up","service":"fauna-mail-bridge-mta"}`
then `…-mda` to the socket** → `fauna-supervisor` validates + runs `s6-svc -u`
for each → the run-scripts see the flag present and exec the bridge. Disable
reverses it: `down` for each service (graceful drain), then unlink the flag.

**Target (ratified 2026-09-05, the third-party integration chain; unbuilt): `allowedServices` becomes catalog-driven data under a compiled ceiling.** A **plugin supervisor** in the same trust class as this socket's owner starts curated-catalog plugin containers unprivileged, network-restricted to their declared hosts, with the sidecar bearer as environment; the set of startable services is the installed catalog entries (app-UI-installed nest state) beneath a compiled list of what a plugin container may ever be given — the § Capability-allowlist pattern of `../apps/bridges.md`, applied to supervision. Runner contract + catalog admission: [`../third-party.md`](../third-party.md) § Execution forms; sandbox profile: [`../security.md`](../security.md) § Co-resident process trust boundary.

ATProto's service name (`fauna-atproto-bridge`) has already been added to the
supervisor's allowlist and reuses this socket unchanged (nest's
`set_atproto_enabled` writes `/data/atproto-enabled` + sends the `up` command,
mirroring `set_mail_enabled`) — **but see § Implementation status today: the
service is not yet wired into the `user` bundle, so `s6-svc -u` on it has no
live target today.** A future standalone Nostr relay bridge that inherits the
lifecycle shape would add its service name to the allowlist the same way.
ActivityPub needs no entry here: it ships in-process inside `fauna-nest`, not
as a separately supervised service (`../../behavior/activitypub.md` §
Architecture; `../apps/bridges.md` § Future bridges).

## First-Run Initialization

`entrypoint.sh` runs on every container start. On first run (before `nest.toml` exists), it detects configuration and runs one of the setup modes below. **All TLS is ACME HTTP-01 (no DNS write); all DNS records are published by the admin's client (`fauna-provisioning`), never by the container** — the nest holds no DNS-provider keys (`../../behavior/dns-management.md`).

**Not every `nest.toml` write is first-run.** The artifact's own writes go through
`docker/nest-toml-overlay.sh`, which splits them by lifetime: the `cors_origins`
**seed** is written once at first run (a app-set choice supersedes it — § Environment
Variables), while `static_dir` — the bundled web SPA's path, a hard-coded artifact
constant no human chooses — is **reconciled on every boot**, so a box first-booted from
an image whose write was broken repairs itself on pull. Every write verifies itself and
fails the boot rather than leaving the nest half-configured. Why that shape, and the
2026-07-13 regression that forced it: [`../../behavior/web-content-hosting.md`](../../behavior/web-content-hosting.md)
§ Implementation status today → *Serving the SPA is artifact wiring*.

### Default: Domainless

**Trigger:** always — there is no domain input to the container.

The recommended default. The container writes no domain overlay and no `[acme]`
table → ACME stays off and the nest serves the **always-live self-signed floor**
(§ Environment Variables note; [`../nest/tls-certificates.md`](../nest/tls-certificates.md)
§ A). Reach it at its IP, claim it with a **handle alone**, and add a domain later
from a Fauna app. Full flow: [`../nest/domains-and-tls-bootstrap.md`](../nest/domains-and-tls-bootstrap.md).

### Mode 1: Managed Subdomain — **not implemented**

No trigger exists — the formerly-documented `FAUNA_NAME` is read by nothing
(entrypoint, compose, install scripts: zero consumers). A managed-subdomain
offering is future work owned by `../provisioning/registry.md` (the managed
broker); it will not arrive as a container env var.

### Mode 2: Own Domain — **retired**

**Former trigger:** `FAUNA_DOMAIN=example.com` — **removed.** The entrypoint no
longer reads `FAUNA_DOMAIN`; there is no "own domain" boot mode. A box always boots
**domainless** (§ Default above) and learns its domain at **claim**, from the
admin's handle (`admin@example.com`), persisted as the primary `mail_domains` row
(which IS the nest's identity). Full flow / authority:
[`../nest/domains-and-tls-bootstrap.md`](../nest/domains-and-tls-bootstrap.md)
§ Env contract.

Once a domain is claimed: TLS is ACME **HTTP-01** (challenge served on the nest's
own port; no DNS write); all DNS records are published by the admin's **client**
— the container holds no DNS-provider credentials; and DKIM is auto-provisioned
nest-side, never entrypoint-generated (owner:
`../../behavior/mail-bridge-lifecycle.md` § DKIM provisioning (automatic)).

**Home deployments have no boot mode of their own.** A home box is `FAUNA_MODE=private`
plus the host-networking compose bundle and its installer, owned by [`home-relay.md`](home-relay.md). (A
`FAUNA_MODE=home` value was once documented here; the nest accepts only
`public|private` — anything else is ignored and the box boots the default
**public** posture, so `home` was a silent misconfiguration, never a mode.)

## Environment Variables

Every value here sits in one of the two invariant buckets: **artifact-set IPC**
(the compose bundle / installer / cloud-init writes it — no human edits it to
express a preference) or a **seed for a app-set choice** (read before the
admin's client-persisted state exists, overridden by it after).

| Variable | Purpose | Bucket |
|----------|---------|--------|
| `FAUNA_DOMAIN` | **Retired / removed** — the box boots **domainless** and learns its domain from the admin's **claim handle** (persisted as the primary `mail_domains` row, which IS the nest's identity). No longer read by `entrypoint.sh`, `docker-compose*.yml`, the install scripts, or `cloud_init.rs`. Authority: [`../nest/domains-and-tls-bootstrap.md`](../nest/domains-and-tls-bootstrap.md) § Env contract. | — (removed) |
| `FAUNA_MODE` | `public` (default) \| `private` — **pre-claim NAT-mode seed only** (any other value is ignored → public). Overridden by the app-set `nest_nat_mode` row once the admin chooses in the wizard/admin panel. Owner: [`../nest/common.md`](../nest/common.md) § NAT mode. Also gates the MTA run-script (never starts private-side). | Seed |
| `FAUNA_LAN_BIND_IP` | Home-relay private box: LAN IP to bind the MDA IMAP 993/143 + CalDAV (`<LAN-IP>:8443`, the `caldav_bind_host` hatch) listeners on. Deployment topology. See [`home-relay.md`](home-relay.md). | Artifact-set IPC |
| `FAUNA_BIND_ADDR` | Nest listener bind address (the home bundle sets `127.0.0.1` under host networking) | Artifact-set IPC |
| `FAUNA_PORT` | Nest's internal listen port behind the `:443` SNI router (default: `3000`; kept off 8443 so 8443 means CalDAV) | Artifact-set IPC |
| `FAUNA_CLAIM_CODE` | Non-interactive claim-code injection (provisioning writes it; see § Admin Claim Code) | Artifact-set IPC |
| `FAUNA_DEPLOYMENT_SEED` | Box-recovery identity seed, read by the nest binary (inert once an identity exists). Custody story: [`../nest/box-recovery.md`](../nest/box-recovery.md). Passed through compose (`docker-compose*.yml`) + both install scripts. | Artifact-set IPC (recovery input) |
| `FAUNA_ACME_DIRECTORY_URL` | ACME directory override → `[acme] directory_url`. Set only by the tier_4 pebble harness; the CA is otherwise the constant Let's Encrypt (same owner). | Artifact-set IPC (test) |
| `FAUNA_CORS_ORIGINS` | Comma-separated CORS origins boot seed; the live surface is the app-set `fauna.admin.set_cors_origins` state | Seed |
| `FAUNA_CLAMD_ADDR` / `FAUNA_RSPAMD_URL` | Content-scan sidecar addresses → `operator-hatch.toml` (compose defaults `clamd:3310` / `http://rspamd:11333`) | Artifact-set IPC |
| `FAUNA_MTA_MX_OVERRIDE` | Test-harness MX override → `operator-hatch.toml` | Artifact-set IPC (test) |
| `FAUNA_LOG_LEVEL` | Bridge log level (s6 run-scripts, default `info`) | Artifact-set IPC |
| `FAUNA_IMAGE_TAG` | Image tag the compose bundle follows (default `latest`; channels owned by `architecture/build-system.md`) | Artifact-set IPC |

(`FAUNA_NAME`, `FAUNA_PUBLIC_IP` and `FAUNA_IMAP`, formerly listed here, are read by
nothing and emitted by nothing — removed. `FAUNA_ACME_EMAIL` and the entrypoint's
`--acme-email`, formerly listed here, are removed too: the ACME contact is a
constant (none) — [`../nest/tls-certificates.md`](../nest/tls-certificates.md)
§ ACME settings — constants, not choices. `FAUNA_PAIR_WITH`, formerly listed here
(a private box's pull target), is removed too: the pull target is the user's
in-app pairing row — [`../nest/private-mode.md`](../nest/private-mode.md)
§ Implementation status today. `FAUNA_INBOUND_DELIVER_KEY`/`FAUNA_BRIDGE_DELIVER_KEY`/`FAUNA_ROUTER_PROXY_SECRET`/
`FAUNA_BLESSED_KEYS_DIR`/`FAUNA_FRONTED_BY_ROUTER`/`FAUNA_SIDECAR_TOKEN` are
entrypoint-/run-script-internal wiring the s6 services export to each other or
to nest, not deployment inputs — no human ever sets them. **The unset branch of
`FAUNA_ROUTER_PROXY_SECRET` and `FAUNA_BLESSED_KEYS_DIR` is a live state, not a
remnant (ruled 2026-10-01):** a nest that runs without this image's router and
blessed-key registry — the native Linux install, a desktop nest, a test nest —
never has either variable, and there the binaries trust a loopback PROXY
header and enroll a loopback bridge leniently by design. In this image the
entrypoint mints both before any service starts, so the run-scripts' tolerance
of a missing secret is reached only by a provisioning fault, which nest logs;
why it stays permissive there instead of failing closed is owned by
[`../security.md`](../security.md) § Co-resident process trust boundary.
`FAUNA_SIDECAR_TOKEN`
is additionally a **start-time snapshot** of a per-nest-session value, which is
why a sidecar whose credential nest refuses exits for s6 to restart rather than
retrying — the run-script re-reads the token file on each start; the rule and its
rationale are owned by [`../transport.md`](../transport.md) § Future directions →
the sidecar channel's credential lifetime. `FAUNA_INSECURE_DISABLE_TLS`
is a test/diagnostic-only escape (nest's API listener serves plain HTTP instead
of the self-signed floor) that no deployment path ever sets — see § Health Check.)

> **`FAUNA_SELF_SIGNED` is removed** (was: `1` = turn ACME off and self-sign on
> boot). The nest now writes an **always-live self-signed floor unconditionally**
> on boot (`self_signed_cert::ensure_floor_present`, called unconditionally
> by `prepare_listener_tls`/`start_server`; [`../nest/tls-certificates.md`](../nest/tls-certificates.md)
> § A), so it serves HTTPS immediately whether or not ACME is enabled and a
> non-completable ACME order no longer hangs the listener — the knob is subsumed. A
> fully-local (LAN-only) deployment runs **domainless** like every box:
> reach it by IP, claim handle-only, add a domain from a
> app ([`../nest/domains-and-tls-bootstrap.md`](../nest/domains-and-tls-bootstrap.md)).
> `FAUNA_DOMAIN` itself is **retired** — the entrypoint no longer reads it, so there
> is no Mode-2 overlay; the domain comes solely from the claim handle (§ Env
> contract, cross-referenced above).

## Ports

| Port | Protocol | Purpose | Required |
|------|----------|---------|----------|
| 443 | TCP | HTTPS front door — the `fauna-sni-router` (routes to nest / MDA CalDAV / relay by SNI); compose maps host `443:443` | Yes |
| 8080 | TCP | HTTP — ACME HTTP-01 challenges + HTTPS redirect (host `80→8080`) | Yes (if using HTTP-01) |
| 3000 | TCP | Nest's internal listener behind the router (`FAUNA_PORT` default) — loopback IPC, not published | Internal |
| 8443 | TCP | MDA CalDAV — the bare-IP/LAN direct listener (admin-set CalDAV port default; `caldav-server.md` § Network exposure) | If CalDAV enabled |
| 8444 | TCP | MDA CalDAV loopback (domain box — SNI-router backend) | Internal |
| 25 | TCP | SMTP inbound (MX) | If email enabled |
| 465 | TCP | SMTP submission (implicit TLS) | If email enabled |
| 587 | TCP | SMTP submission (STARTTLS) | If email enabled |
| 993 / 143 | TCP | IMAPS / IMAP | If email enabled |
| 8445 / 8446 | TCP | `fauna-iroh-relay` P2P relay sidecar, HTTPS/HTTP (SNI-router backend, `relay.<domain>`) | Internal |
| 8447 | TCP | `fauna-atproto-bridge` XRPC/OAuth loopback (SNI-router backend, `pds.<domain>`) | Internal |
| 3478 | UDP | STUN endpoint discovery (P2P) | Optional |

The bridges/router bind the privileged ports inside the container under their
non-root per-role UIDs via `CAP_NET_BIND_SERVICE` (see § Multi-Stage Build,
`setcap`).

## Persistent Volume

A single named volume is mounted at `/data`.

```
/data                     (fauna-owned 0711)
├── nest.toml             (config, generated on first run by the entrypoint)
├── nest.db               (SQLite database — the sealed store)
├── claim-code            (admin claim code, single-use; § Admin Claim Code)
├── blobs/                (file storage)
├── acme/                 (TLS certificate cache + acme-retry-state.json)
├── keys/                 (root:root 0711 — per-role service-user keypairs, MINTED BY THE ENTRYPOINT as root, roles load-only)
│   ├── mta/ mda/ relay/  (each 0700, owned by its role UID — mta.key / mda.key / relay keydir)
│   ├── atproto/          (0700, fauna-atproto-owned — atproto.pds.key; mint-if-absent so the enrolled identity is stable across reboots; no blessed-pubkey entry yet — S1 enrolls loopback-gated only)
│   ├── blessed/          (root-owned blessed-pubkey registry — strict enrollment)
│   └── router/proxy-secret  (SNI-router PROXY-v2 secret)
├── atproto/              (0700, fauna-atproto-owned — the ATProto PDS bridge's WAL SQLite repo store; re-derivable from nest state, a cache not precious)
├── inbound-deliver-key   (the inbound deliver secret, generated by the entrypoint)
├── operator-hatch.toml   (bridge deployment-topology overrides — bind addresses, clamd_addr / rspamd_url, MX override; written by entrypoint.sh, 0644)
├── imap-enabled, caldav-enabled, carddav-enabled, webdav-enabled, atproto-enabled
│                         (flag files — written by nest on the admin's app toggles; the MDA gates on their OR, the MTA on imap-enabled, the ATProto bridge on atproto-enabled; brought up via the supervisor socket)
├── maintenance/ maintenance-host/  (host-OS-maintenance channel — vps.md § Host OS Maintenance)
└── services.json         (nest's own flags: bridge, pairing — nothing in the image reads it)
```

Everything the nest needs to persist lives here. The volume is preserved across upgrades and container recreations.

## docker-compose.yml

The repository's `docker-compose.yml` is the canonical, runnable bundle — copy
it to the host (`just dev-deploy user@host` does this) and `docker compose up -d`.
Its shape:

- **`fauna-nest`** — the image above. Maps host `80→8080`, `443→443` (the
  `fauna-sni-router`, which fronts the nest's internal `:3000` + the MDA CalDAV),
  and the mail ports `25`, `465`, `587`, `993`. Environment:
  `FAUNA_MODE`, `FAUNA_DEPLOYMENT_SEED` (box-recovery seed,
  empty on a fresh box), `FAUNA_PORT` (default `3000`),
  and the content-scan addresses `FAUNA_CLAMD_ADDR` (default `clamd:3310`) +
  `FAUNA_RSPAMD_URL` (default `http://rspamd:11333`). **No `FAUNA_DNS_*` env** —
  the container never receives DNS-provider credentials (§ First-Run Initialization
  above, and `vps.md` § container env); DNS is published by the admin's client.
- **`clamd`** (`clamav/clamav:latest-debian`) and **`rspamd`** (`rspamd/rspamd:latest`)
  — content-scan sidecars the mail bridge dials over the compose network. Both
  are **required whenever mail is enabled**: the bridge's scan gate is default-on
  and fail-closed, so an unreachable scanner 451s every inbound message. rspamd's
  normal worker is bound to `*:11333` via an inline compose `config` (the stock
  image binds localhost only). **clamd holds the signature DB in RAM (~1.5 GB),
  raising the practical RAM floor for a mail-enabled deployment well above the
  1 GB VPS minimum.** Orchestrator-provisioned (cloud-init) boxes therefore emit
  these sidecars **only for a mail box** — gated on the `vps_config` mail-vs-social
  intent (`vps.md` § Provisioned services; `behavior/onboarding.md` §5) — so a
  social-only box stays viable on the 1 GB tier. Both refs are **digest-pinned**
  in every shipping copy (since 2026-08-27) — the pin, the vetting evidence, and
  the currency cadence are owned by [`../release-integrity.md`](../release-integrity.md)
  § Third-party container images; this page names the topology only.
- **`watchtower`** (`nickfedor/watchtower:latest` — maintained fork; the original
  `containrrr/watchtower` is unmaintained and incompatible with Docker 24+) —
  label-gated daily poll for image updates on the followed tag, recreating the
  container on a new digest. Its `127.0.0.1:8080` HTTP API is **opt-in**
  (`WATCHTOWER_HTTP_API_UPDATE`, default off) — when enabled it is what
  `just deploy-dev` triggers over SSH for an instant update. Remove the service
  for manual-only upgrades. Digest-pinned like the scanners (same owner). Because
  only `fauna-nest` carries the enable label, watchtower updates the nest image
  **only**: the two scanners and watchtower itself change only with a new bundle
  copy — § Upgrades' `docker compose pull` re-pulls the same pinned bytes until
  then — until the Fauna-namespace mirror that owner's part 4 targets lands.
- Compose-level hardening: every service carries the `x-logging` 50 MB × 5
  rotation caps (the log-growth incident backstop) and the nest service a
  `stop_grace_period: 15s` (graceful WS close-1001 drain on recreate).

A single named volume `fauna-data` is mounted at `/data`.

## Health Check

The image's own HEALTHCHECK probes the nest's internal listener, trying plain
HTTP first and falling back to HTTPS (`-k`, self-signed) — nest serves HTTPS
from boot via its always-live self-signed floor cert
(`self_signed_cert::ensure_floor_present`, § Environment Variables note), so a
default deployment answers on the HTTPS leg:

```bash
curl -sf http://localhost:${FAUNA_PORT:-3000}/api/v1/health 2>/dev/null \
  || curl -sfk https://localhost:${FAUNA_PORT:-3000}/api/v1/health
```

Returns HTTP 200 when the nest is ready. (Port 8443 is the MDA CalDAV listener,
not the API — probing it checks the wrong service.)

## Admin Claim Code

On first run with no registered users, the nest ensures a claim code at
`/data/claim-code` (format + generation owned by `../../behavior/onboarding.md`
§ 3a; entropy/throttle rationale owned by `../federation.md` § Security). The
first client to send `fauna.auth.claim_admin`
(the sole claim transport — pre-identity WS-RPC; there is no HTTP claim endpoint)
with the code, a handle, and a valid Ed25519 signature becomes the admin. The
file is deleted after use and cannot be reused.

Two non-interactive injection paths exist for provisioned boxes (both
artifact-set — the code is minted client-side before the box exists):
`FAUNA_CLAIM_CODE` env, or the `/run/fauna/claim-code-seed` cloud-init mount.

The nest prints the code banner to **stderr**, so log retrieval works (run from
the compose bundle's directory — compose does not name the container literally
`fauna-nest`, so `docker compose`'s service-name resolution is what both install
scripts use, not a raw `docker logs`/`docker exec` against a guessed container name):

```bash
docker compose logs fauna-nest 2>&1 | grep claim
```

Or read it directly from the volume:

```bash
docker compose exec fauna-nest cat /data/claim-code
```

## Upgrades

**Automatic (Watchtower):** The `watchtower` service in the compose file checks for a new `latest` image daily and restarts the container if one is found. The data volume is preserved automatically.

**Manual:**

```bash
docker compose pull
docker compose up -d
```

## Uninstall

Stop containers and remove them:

```bash
docker compose down
```

To also remove all data (irreversible):

```bash
docker volume rm fauna-data
```

## Platform Support

| Architecture | Supported |
|-------------|-----------|
| amd64 (x86_64) | Yes |
| arm64 (aarch64) | Yes |

The image is a native multi-arch manifest. Docker pulls the correct variant automatically.
**An architecture is only supported if every compose sidecar is published for it too** — the
scan gate is default-on and fail-closed, so a sidecar missing on one architecture makes that
architecture's mail-enabled deployment 451 every inbound message at shipped defaults. This is
not hypothetical: `clamav/clamav`'s unsuffixed tags are amd64-only, and the bundle named one
until 2026-08-26. A dedicated dev-fleet test now pins each shipping copy's sidecar images
against the platforms `build-nest-image.yml` publishes, so adding an architecture here without
re-vetting the sidecars fails the merge. Works on Docker Desktop (macOS, Windows) for development but production deployments should run on Linux.

## History (one-liners; details in git)

- 2026-05-24: Go mail bridge shipped in the image (cgo stage, s6 services, supervisor sidekick, scan sidecars); nest-side enable-handshake writer converged on the identical wire; live VPS bring-up milestone since met (production).
- 2026-06-22: dead entrypoint DKIM generation removed (no `/data/dkim.pem`, no `generate-dkim` subcommand) — DKIM is nest-side provision-on-read (owner: `../../behavior/mail-bridge-lifecycle.md` § DKIM provisioning).
- 2026-07-19/20: ATProto PDS bridge (`fauna-atproto-bridge`) added to the image — dedicated `fauna-atproto` UID 1005, cgo Go binary linking `libfauna_ffi.so`, supervisor-allowlist entry, `pds.<domain>` SNI route; shipped with a packaging gap (never wired into the `user` bundle, so s6-rc never supervised it).
- 2026-07-23: that gap closed — `docker/s6/user/contents.d/fauna-atproto-bridge` added, and the whole class pinned headlessly by tier_1 `test_s6_services_are_supervised.py` (every longrun reachable from a bundle; no marker naming a missing service).
