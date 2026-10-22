# Installer: VPS Provisioning — target state

Owns: vps, ssh-door
Status: ratified
Authority: VPS-installer architecture — the client-side provisioning surface (fauna-provisioning crate + orchestrator, supported VPS/DNS providers, minimum VPS requirements, what gets deployed incl. Watchtower/sidecars/log-rotation), the host-OS-maintenance lifecycle (unattended-upgrades, nest-coordinated reboot, the host↔nest channel + admin visibility), and recovery/uninstall incl. the managed-by=fauna marker + delete_server teardown + the marker-filtered server listing primitive; defers the app view that retires a box to behavior/nest-retirement.md, the wizard UX + page flow to behavior/onboarding-provisioning.md §§ 4–6, the consumed image + env contract to installers/docker.md, the provider catalog to provisioning/registry.md, claim mechanics to behavior/onboarding.md § 3a, total-box-loss recovery to nest/box-recovery.md, and the fauna.setup.status wire-field contract to nest/common.md.

Last verified: 2026-09-09 (doc-consistency sweep — re-flow-traced `ProvisionResult`, `FAUNA_DEPLOYMENT_SEED`, the 5-named-VPS-provider `delete_server` matrix (now 6 with the bundled adapter, all routed through the shared `finish_delete` helper), and the conformance test files, all held. **Found+fixed:** § Implementation status today had recorded steps 7-8 (the 2026-08-29 build+claim/reach-hint/pending-provision-slot ratification) as "NOT BUILT yet" since sweep — stale since 2026-08-31, when the standard path's claim, the reach hint and the pending-provision slot all landed BUILT (per-app legs for windows/android still open, web's TLS wall pending re-measurement post the 2026-09-02 bridge cert); also repointed the stale `../../behavior/onboarding.md` § Implementation status today citation to `onboarding-provisioning.md`, which absorbed that content on 2026-09-06. Prior sweep's flagged-not-fixed stale "all five"/"five current implementations" code comments in `libs/fauna-provisioning/src/vps/mod.rs` have since been cleaned up by other commits — now reads "all six providers".) Source: `libs/fauna-provisioning/`, `libs/fauna-onboarding-machine/`, `libs/fauna-wasm-onboarding/`.

## Goal

One-click nest deployment via the setup wizard on all seven apps. The shared onboarding machine (`libs/fauna-onboarding-machine`) drives the `fauna-provisioning` Rust crate, which calls VPS and DNS provider APIs directly from the client (compiled to WASM for web via `libs/fauna-wasm-onboarding`; exposed via UniFFI for the native apps), provisioning a fresh VPS that runs the Docker image (see `docker.md`). Provider credentials are held only in the client for the duration of provisioning; no central registry is involved.

---

## User Experience

The wizard UX and page flow are owned by [`../../behavior/onboarding-provisioning.md`](../../behavior/onboarding-provisioning.md) §§ 4–6 (handle-first onboarding; the `vps_config` + `nest_provisioning` pages and the snapshot-driven `provisioning-*` progress family in `tests/e2e-unified/ui.yaml`). This doc owns what provisioning *does*, not the page flow. In brief: the user picks a VPS provider (and, for an own domain, a DNS provider), enters credentials that never leave the client, and the shared onboarding machine provisions the box and polls it healthy — a few minutes end-to-end, with typed per-step progress (Domain / Server / Dns / Online), pre-flight checks, idempotent retry, and soft cancel. There is no separate completion screen: the machine routes to `Done` once provisioning succeeds, and recovery after **total box loss** is wholly client-driven with no shell ([`../nest/box-recovery.md`](../nest/box-recovery.md)).

Per-provider credential requirements are listed under § Supported Providers below.

---

## What Happens Behind the Scenes

1. The client calls the VPS provider's API directly via the `fauna-provisioning` crate with the VPS provider, credentials, selected region, and domain configuration.
2. The `fauna-provisioning` crate creates a VPS instance at the chosen provider. The instance boots with cloud-init user-data that installs Docker (standard cloud images include Docker or cloud-init will install it).
3. The first-boot unit `fauna-nest.service` — cloud-init enables it and starts it without blocking — runs `docker compose up -d` and, when the pull or the start fails, retries every 60 s for as long as it takes (ruled 2026-10-19): a registry outage, a transient network error or a channel tag not yet published never strands a box whose sshd is already off, which [`../nest/common.md`](../nest/common.md) § Client-state recoverability would call a brick; the unit runs on every boot (an idempotent `up -d`), the interval is a Rust constant. The image is `ghcr.io/faunasocial/nest:<tag>`, the tag the wizard's channel choice maps to (`UpdateChannel::image_tag`; multi-arch, the correct amd64 or arm64 variant selected automatically). The wizard itself never builds a box for a tag the registry has no build for (`../../behavior/onboarding-provisioning.md` § 5 → *What exists decides the default*).
4. The container boots **domainless** — it receives **no** domain env var (`FAUNA_DOMAIN` is retired; `../nest/domains-and-tls-bootstrap.md` § Env contract). It serves the always-live self-signed floor at its IP and learns its domain at **claim**, from the admin's handle (`admin@example.com`), persisted as the primary `mail_domains` row (the nest's identity). (The formerly-documented managed-subdomain boot seed `FAUNA_NAME` is **retired** — read by nothing, not reserved; a future managed-subdomain offering is owned by [`../provisioning/registry.md`](../provisioning/registry.md) (the managed broker) and, per `docker.md` § Environment Variables, will not arrive as a container env var.) **The container never receives DNS-provider credentials** — DNS is published by the client (next step), and the nest never holds the keys (`../../behavior/dns-management.md` § Where the credential lives).
   - **`FAUNA_DEPLOYMENT_SEED`** (optional; 64-char hex of the raw 32-byte Ed25519 deployment seed) provisions the box with a **caller-chosen identity** instead of letting it mint its own — the *"provision a box with a caller-supplied deployment seed"* primitive used at **total-box-loss recovery** so the rebuilt box re-presents the same `nest_actor_id` ([`../nest/box-recovery.md`](../nest/box-recovery.md) § Mechanism). It is bucket-2 IPC artifact wiring (no human edits it), read at boot by the nest and adopted only on a fresh box (inert once an on-disk identity exists). **Both halves are built:** nest-side acceptance, and the client side — `fauna_provisioning::generate_deployment_seed` mints it, `CloudInitParams.deployment_seed` renders it into the compose env, and the onboarding machine's recovery branch re-installs a custodied seed at re-provision (per-app frontier: [`../nest/box-recovery.md`](../nest/box-recovery.md) § Implementation status today).
5. The client publishes the required DNS records itself via `fauna-provisioning` (A/AAAA for the nest + `mail.<domain>`, and MX/SPF/DMARC when email is enabled) — it holds the DNS-provider keys; the container does not. The **DKIM** TXT is published from the nest's auto-provisioned signing key (post-boot — see the DKIM note below). Then first-boot initialization runs, split across two owners:
   - **`entrypoint.sh`** (owner: `docker.md`) maps the `FAUNA_*` env into `nest.toml`, lays out `/data/`, and mints the service keypairs — pure env→config mapping and first-boot file layout.
   - **The nest binary** obtains the TLS certificate via ACME **HTTP-01** (the enable is *derived*, never configured; challenge served on the nest's own port, no DNS write — `../nest/tls-certificates.md` § B) and ensures the claim code at `/data/claim-code` (on a provisioned box the code is client-minted and cloud-init-injected — § Admin Claim).
   - The DKIM signing key is **not** generated by cloud-init — the nest mints and holds it when the mail domain is added (`mail_dkim_keys`; `../../behavior/mail-bridge-lifecycle.md` § DKIM provisioning (automatic)). The client publishes the DKIM TXT from the nest's `mail_dkim_keys.public_dns_value`, so the published record matches the key the nest actually signs with.
6. On the final successful step, the orchestrator returns a `ProvisionResult` to the client (`libs/fauna-provisioning/src/orchestrator.rs`):
   ```json
   {
     "server_id": "12345",
     "ipv4": "1.2.3.4",
     "domain": "myname.nest.fauna.social",
     "claim_code": "K7Q2-M9XJ"
   }
   ```
   `claim_code` is the same client-minted, cloud-init-injected code described in § Admin Claim below — the result carries it forward so the wizard can move straight to the claim page without retrieving anything from the box.
7. The orchestrator's final step (**Online**) polls the box's `/api/v1/health` **at its captured public IP** — the reach address, on every app; the domain is never waited on — until it returns HTTP 200, then the onboarding machine **claims the box** with the client-minted code as the step's last substep. The 480 × 5 s ceiling bounds box boot + image pull (+ the IP bridge cert's first issuance, which is what lets the *browser* reach the IP — `../nest/tls-certificates.md` § B-IP), not DNS, and is a ceiling, not a fixed wait (`libs/fauna-provisioning/src/progress.rs`). Owner of the reach address, the claim-in-run and the crash-safety slot: `onboarding.md` § 6 (ratified 2026-08-29).
8. On success the wizard continues to the NAT-mode choice exactly as after a typed claim — there is no separate completion screen and no claim code to type (`onboarding.md` §6, § 3b-bis); the account keeps the box's IP as a reach hint until the domain is live (`onboarding.md` § Reach hint). Recovery after total box loss is client-driven (no SSH) — see [`../nest/box-recovery.md`](../nest/box-recovery.md).

---

## Client-Side Provisioning

Apps call VPS and DNS provider APIs directly via the `fauna-provisioning` Rust crate. Credentials are held only by the client and never sent to a central registry.

### fauna-provisioning Crate

Located at `libs/fauna-provisioning/`, this Rust crate provides:

- **VPS provider integrations:** Hetzner, DigitalOcean, Vultr, OVH, Linode
- **DNS provider integrations:** Cloudflare, Namecheap, Porkbun, Gandi
- **Cloud-init builder:** Generates user-data scripts for VPS initialization (`cloud_init.rs` — including the deployment-seed and claim-code injection)
- **Provisioning orchestrator:** Coordinates VPS creation and DNS setup with snapshot-driven progress

### App Implementations

All seven apps drive the same shared **onboarding machine** (`libs/fauna-onboarding-machine`) — the wizard state machine that owns provisioning. `fauna-provisioning` is the machine's internal engine, not a per-app API surface.

| Platform | Machine face |
|----------|---------------|
| Web | WASM via `libs/fauna-wasm-onboarding` → `apps/fauna-web/src/lib/onboarding/machine.svelte.ts` |
| iOS/macOS | UniFFI machine via FaunaKit `OnboardingVM.swift` |
| Android | UniFFI machine via `core/OnboardingHost.kt` |
| Linux | Direct Rust dependency — `apps/fauna-linux/src/views/onboarding/mod.rs` |
| Windows | UniFFI machine via `FaunaApp.Core/ViewModels/OnboardingViewModel.cs` (+ `Views/Onboarding/VpsConfigView.xaml.cs`) |
| tui | Direct Rust dependency — `apps/fauna-tui/src/launch.rs` |

### Crate surface (machine-internal)

The orchestrator entry points are `provision_with_snapshot`, `provision_with_registration_snapshot`, and `provision_nest_no_dns`, all funneling through the single `run_server_step` create chokepoint (`libs/fauna-provisioning/src/orchestrator.rs`), plus the keypair / claim-code / deployment-seed helpers (`lib.rs`). Progress is snapshot-driven — four user-visible steps (Domain / Server / Dns / Online) over a shared `ProvisioningSnapshot` with pre-flight checks, idempotent retry, and soft cancel — pulled by every app through the machine. Credential verification and the wizard's page-support reads ride the provider-registry surface ([`../provisioning/registry.md`](../provisioning/registry.md) owns the registry schema, dispatch, and the wasm page-support shims).

---

## Supported Providers

The tables below are the curated, named providers wired directly into the wizard. A generic **bundled provider** — a BYO `base-url` adapter through which a third party offers VPS + DNS + domain registration as one combined checkout (`VpsProvider`/`DnsProvider`/`Registrar` all implemented by `libs/fauna-provisioning/src/{vps,dns,registrar}/bundled.rs`) — also exists alongside these; it is not a named row here because no implementer is admitted yet (§ Neutrality). Owner: [`../provisioning/registry.md`](../provisioning/registry.md) § Bundled provider; wire contract: [`../provisioning/bundled-provider-api.md`](../provisioning/bundled-provider-api.md).

### VPS Providers

| Provider | Credentials | Approx. regions | Starting price | Notes |
|----------|------------|----------------|----------------|-------|
| Hetzner | Single API token | 5 | ~$4.50/mo | Recommended — cheapest, EU/US. ⚠ Blocks outbound SMTP by default — see *Outbound mail ports* below. |
| DigitalOcean | Single API token | 15 | ~$6/mo | |
| Vultr | Single API key | 32 | ~$5/mo | Most region choices |
| OVH | Application key + application secret + consumer key | 10+ | ~€7/mo (€5.71 before VAT) | Three credentials; the picker lists each project × region as one entry; offers priced from OVH's public catalogue on monthly billing and shown with tax, as Hetzner's gross price is ([`../provisioning/registry.md`](../provisioning/registry.md) § Server offers follow the location → *OVH*) |
| Linode | Single personal access token | 11 | ~$5/mo | |

Prices are static estimates from the provider catalog's curated offers (`i18n/providers.yaml` — [`../provisioning/registry.md`](../provisioning/registry.md)); they drift with real provider pricing, and the wizard presents them as estimates. There is no live pricing endpoint (the central registry that once served one is decommissioned).

> **⚠ Outbound mail ports (25 / 465 / 587) must be reachable from the box for external delivery.**
> The nest delivers mail **direct-to-MX** — the MTA dials each recipient domain's MX on port 25;
> there is **no authenticated smarthost/relay** in the product (by design — no third-party mail
> dependency; the only `mta_mx_override` transport hook is an *unauthenticated* split-horizon/test
> route, not a public relay). **Most cloud providers — including Hetzner, the recommended one —
> block outbound TCP 25 (and often 465/587) by default** as an anti-spam measure. The failure mode is
> asymmetric and easy to misread: **inbound mail keeps working**, the client's SMTP *submission* to
> the box succeeds (message lands in "Sent"), but every *outbound* delivery silently times out
> (`dial <mx>:25: i/o timeout`), retries, and eventually bounces. This is a provider-network block,
> **not** a nest/bridge fault — the recipient's server logs nothing because the SYN never leaves the
> provider's network.
>
> **Remediation:** request that the provider lift the outbound-SMTP block (Hetzner grants this for
> established accounts via a support request / their "unblock port 25" form). Confirm from the box:
> `timeout 8 bash -c 'exec 3<>/dev/tcp/gmail-smtp-in.l.google.com/25'` — a timeout means still
> blocked, a clean connect means delivery will work. If a provider refuses, the only options are to
> move to one that permits outbound 25 or to add authenticated-smarthost support to the MTA (not
> built today — a deliberate feature decision given the no-third-party-relay stance). `smtp-server.md`
> § Outbound delivery owns the retry/bounce machinery that surfaces this; `mail-deliverability.md`
> § Symptom diagnostics' "Outbound TLS to gmail.com" probe is the in-product detector.

### Evaluated but Not Added

These providers were evaluated (2026-03-31, tracked internally) and not added:

| Provider | Why not | Reconsider if... |
|----------|---------|-------------------|
| Scaleway | EU-only (3 regions), separate IPv4 charge | User demand for EU-focused provider. Easiest to add — same API pattern as existing providers. |
| AWS (EC2) | SigV4 auth, security group prerequisite, ~$10.45/mo | Strong demand from users with existing AWS accounts |
| Oracle Cloud | 4 credentials including RSA PEM key, VCN prerequisites, free-tier capacity issues | Oracle introduces bearer-token auth |
| Azure | 3 credentials, 7+ API calls to create a VM, ~$12.78/mo | Never — fundamentally incompatible with one-click provisioning |

### DNS Providers

| Provider | Credentials | Zone selection |
|----------|------------|----------------|
| Cloudflare | API token (zone-scoped, "Edit zone DNS" template) | Dropdown of zones returned by verification |
| Namecheap | API user + API key | Dropdown of domains |
| Porkbun | API key + secret API key | Dropdown of domains |
| Gandi | Personal access token ("Manage domain name technical configurations" permission) | Dropdown of domains |

> **⚠ Web-only degradation: Cloudflare, Namecheap, Gandi (DNS) and Vultr (VPS) route through a CORS proxy on the web app, and `proxy.fauna.social` is not yet deployed** (verified 2026-07-13). Provisioning through these four providers is degraded on **web** only — native apps (linux/windows/macOS/iOS/android/tui) call every provider directly and are unaffected. Detail: [`../provisioning/registry.md`](../provisioning/registry.md) § Implementation status today.

Additional DNS providers were evaluated (2026-03-31, tracked internally). The highest-value additions are DNS services from existing VPS providers (DigitalOcean, Vultr, Linode) — they would reuse the VPS token, potentially eliminating the DNS credentials step entirely. Hetzner, the fourth VPS provider in that evaluation set, already gained this: Hetzner is the only *named* provider with both `dns` and `vps` capabilities (the generic bundled adapter also carries both, plus registrar — § Supported Providers), one `api-token` covers both, and the "Buy VPS with same provider as DNS" checkbox (`same_provider_for_vps`) reuses it — see [`../provisioning/registry.md`](../provisioning/registry.md) § Capability matrix.

---

## Minimum VPS Requirements

| Resource | Minimum |
|----------|---------|
| vCPU | 1 |
| RAM | 1 GB (social-only); **≥ 2 GB with mail enabled** — see note |
| Disk | 20 GB (raised from 10 GB 2026-10-17 — see note) |
| Network | Public IPv4 address |
| Operating system | **Ubuntu 26.04 LTS, 64-bit — for every server Fauna creates and for a hand-built box alike** — the internet server and the home machine ([`home-relay.md`](home-relay.md) § The two boxes → *Sizing*) — the release the dev fleet runs (owner-ruled 2026-10-20 for the servers Fauna creates, for consistency; widened to the hand-built box 2026-10-21 in public-texts round 3, so one release is tested) — every named adapter in `libs/fauna-provisioning/src/vps/` creates it, the bundled provider contract requires it of whatever server a bundled provider creates (`../provisioning/bundled-provider-api.md` § Endpoints), and the hand-built guides use Ubuntu's own packages |
| Runtime | Docker (or Podman) |
| Open ports | 443 (HTTPS); 25/465/587/993 if email enabled |

Any entry-level plan from the supported providers meets the social-only floor. **Mail
raises the floor:** the deployment runs the `clamd` content-scan sidecar, which holds its
full signature DB in RAM (~1.5 GB), so a mail-enabled box needs comfortably more than the
1 GB minimum (`docker.md` § content-scan sidecars). The scan gate is default-on and
fail-closed, so an under-provisioned box where clamd cannot stay up **451s every inbound
message** — pick a ≥ 2 GB plan when mail will be enabled. **Disk: 20 GB is the one number the guides
carry (ruled 2026-10-17; the table said 10 GB while two guides said 20).** 10 GB
boots a social-only box, but the OS, the image set, the scanner's signature database and the
capped container logs take several GB before any user content, and a household's photos and
mail outgrow the rest within a year; every supported provider's entry-level plan with the mail
floor's RAM already carries 20 GB or more, so the number costs nothing on a mail box.

---

## What Gets Deployed

- **Docker container:** `ghcr.io/faunasocial/nest:latest` — see `docker.md` for full service list, environment variables, and `/data/` layout.
- **The Fauna updater:** `fauna-updater`, a second container of the nest image holding only the Docker socket — follows the box's update channel and re-creates the nest on a newer build, never an older one ([`../nest/update-channel.md`](../nest/update-channel.md) § The updater).
- **Content-scan sidecars:** `clamd` (`clamav/clamav:latest-debian`) + `rspamd` (`rspamd/rspamd:latest`) — the mail bridge dials these over the compose network. The cloud-init compose ships them **only for a mail box**: provisioning gates them (and the mail ports / mail `ufw` rules) on the `vps_config` mail-vs-social intent (`behavior/onboarding.md` §5), so a **social-only box omits them entirely** and stays viable on the 1 GB tier (matching the RAM note above), while a mail box matches the canonical `docker-compose.yml`. The scan gate is default-on and fail-closed, so a mail box without them 451s every inbound message once mail is enabled (`docker.md` § content-scan sidecars). Their absence is also what tells the box it cannot enable mail: the entrypoint declares it on the topology hatch and the nest refuses the enable ([`../../behavior/mail-bridge-lifecycle.md`](../../behavior/mail-bridge-lifecycle.md) § Default-off on first claim → *A box with no content scanners cannot enable mail*).
- **Persistent volume:** mounted at `/data/` — survives container restarts, upgrades, and recreation.
- **Container log rotation:** every service in the compose bundle caps its `json-file` logs at 50 MB × 5 files (`x-logging` anchor in `docker-compose.yml`). This is a hard backstop against a chatty or runaway service filling the host disk — without it an unbounded log can wedge the whole node. Retry/refresh loops are themselves bounded (the mail-bridge cert refresh loop backs off through `refreshsched.NextDelay`); rotation is the deployment-side second line of defense.

Architectures: amd64 and arm64. Docker pulls the correct variant from the multi-arch manifest automatically. What "supported" requires of the compose sidecars — every one of them published for that architecture too, or its mail deployments 451 — is `docker.md` § Platform Support.

---

## Host OS Maintenance

> **Authority:** this section owns the host-Ubuntu patch + reboot lifecycle and its surfacing policy — what the maintenance fields mean, when they change, and how the admin sees them. The `fauna.setup.status` **wire-field contract** (field names, types, admin gating) is owned by [`../nest/common.md`](../nest/common.md) § `fauna.setup.status`, which points back here for the lifecycle.

The onboarded box is **Ubuntu host → Docker → nest container**. That is **two independent patch streams**:

- **Container** — the nest image (`debian:bookworm-slim` base) is kept current by **the Fauna updater** (§ What Gets Deployed); base-image CVEs are patched when the image is rebuilt and republished. Nothing host-side touches it.
- **Host Ubuntu** (kernel, `openssl`, `systemd`, `docker.io`, `ufw`) — kept current by the design below. **No human ever logs in** (product invariant: there is no operator), so the entire policy is **artifact-set IPC + hard-coded constants — no human-editable config, no app knob.** The test ("would a user or admin ever want to *choose* this?") answers *no* for "patch the OS / reboot for security": it is always-on, so it is constant + cloud-init wiring, never a setting.

**Scope: the box the app provisions.** Everything below is installed by the app's cloud-init, so it binds a provisioned box only. A box an admin builds by hand — from the internet-server guide, or a home box from the home-setup guide ([`home-relay.md`](home-relay.md)), whose Step 3 installs the same packages — gets Ubuntu's own default `unattended-upgrades` (security patches, no automatic reboot) and no reboot coordinator: its host-status fields stay absent, and a restart for a kernel patch is the admin's — from the provider's control panel once SSH is closed (`../deployment-threat-model.md` § 1a, the hand-built box after its claim), or on a home machine the admin's own restart (stated 2026-10-20 for the home guide's restart sentence). Each guide's compose still mounts the two maintenance directories, so the nest's half runs unchanged and finds no host data (stated 2026-10-19 for the internet guide's restart sentence), and the § 4 status line says so ("OS status not reported by this host"), never "OS up to date".

### 1 — Non-reboot patches: `unattended-upgrades` (always-on, baked into cloud-init)

The vast majority of host updates (`openssl`, `docker.io`, most libraries) apply live with no interruption. Cloud-init (`libs/fauna-provisioning/src/cloud_init.rs`) installs and configures `unattended-upgrades`:

- `unattended-upgrades` added to `packages`.
- `/etc/apt/apt.conf.d/20auto-upgrades` — enable periodic package-list update + unattended upgrade.
- `/etc/apt/apt.conf.d/50unattended-upgrades` — security origin (`${distro_id}:${distro_codename}-security`, plus `-updates`), and **`Unattended-Upgrade::Automatic-Reboot "false"`** — the nest-coordinated reboot below owns reboots, not `unattended-upgrades`.
- `needrestart` configured **non-interactive + defer** (`$nrconf{restart} = 'l'` / list-only) so a library update never auto-bounces `docker.service` (which would drop the container uncoordinated); anything genuinely needing a restart is rolled into the next strategic reboot.

This layer needs no coordination — it is a pure addition to the generated cloud-init.

### 2 — Reboot: nest-coordinated idle, with a hard ceiling

Only kernel / `glibc` / `systemd` / `dbus` updates need a reboot, signalled by `/run/reboot-required`. These are infrequent. **A reboot is never a *safety* problem — only an *availability* one:** the nest is crash-safe by invariant ([`../nest/common.md`](../nest/common.md) § Client-state recoverability) and a clean stop already drains gracefully (SIGTERM → broadcast WS `1001 (Going Away, Retry)` → drain in-flight, `ws::GRACEFUL_SHUTDOWN_TIMEOUT = 7 s` → `db.flush()` WAL checkpoint; [`../transport.md`](../transport.md) § Graceful shutdown). So even a worst-case hard reboot loses nothing and clients just reconnect; "strategic" is purely about **not interrupting an active user**, and the only component that knows whether a user is active is the **nest** (`ws_state.connection_count()`).

A small **reboot-coordinator** — a `systemd` timer + script, baked into cloud-init — runs on the host every ~15 min:

1. If `/run/reboot-required` does **not** exist, do nothing.
2. Read the nest's `nest-readiness` file (§ 3). The nest is **idle** when `connection_count == 0` — a request can only be in flight over a live connection (there is no separate in-flight counter, `ws.rs`), so no connections ⇒ nothing in flight.
3. Reboot (`systemctl reboot`) when the nest is idle **OR** the reboot has been pending past a **hard ceiling of 24 h** — security must not be deferrable forever. The ceiling clock is the **mtime of the host-owned `/run/reboot-required`** (`stat -c %Y`), an *un-forgeable* source outside any container mount — so a compromised nest cannot push it into the future to defeat the ceiling (the ceiling-suppression fix; see § 3 trust model). `systemctl reboot` stops `docker.service`, which sends the container SIGTERM → the **existing** graceful-shutdown path (no second drain to build, priority #2/#4). `restart: unless-stopped` brings the container back after boot.

The cheaper alternative considered and **rejected** — `unattended-upgrades`' own `Automatic-Reboot-Time "HH:MM"` — is timezone-naive (the box runs UTC) and blind to live sessions, so it is a blind clock, not "strategic" (user decision, 2026-06-28).

**Kernel CVEs are handled by reboot, not livepatch (user decision, 2026-06-28).** Canonical Livepatch would apply kernel patches without a reboot but requires an Ubuntu Pro token provisioned per box — a new external-account dependency that fights works-out-of-the-box. Rejected; kernel CVEs apply on the next strategic reboot.

### 3 — Host ↔ nest channel (makes OS maintenance visible to the admin)

The nest runs in a container and cannot read host apt state; the host cannot read `connection_count()`. The two sides exchange small **flat `key=value`** files (not JSON: the dependency-free base-Ubuntu coordinator `grep`s them, so the box needs no `jq`; no network, no auth).

**Trust model (the channel is split by trust direction).** The host `fauna-reboot-coordinator` runs as **root**; the realistic adversary this whole layer defends against is a **compromised nest container** (it limits container-escape blast radius — see the threat framing at the top of § Host OS Maintenance). The nest runs as host **uid 1000** (`Dockerfile` `useradd -u 1000 fauna`), so a root process must **never read or write its own trusted state in a directory uid 1000 owns** (that was the gap in the first cut). Provisioning therefore adds **two** bind mounts, one per trust direction:

- **`/opt/fauna/maintenance` ↔ `/data/maintenance` (rw, `chown 1000:1000`)** — the **nest→host** direction. The nest *writes* it; the root coordinator only *reads* it, and reads it **defensively** (rejects a symlink with `[ ! -L ]`, value-validates every field) — never writing trusted state there. A compromised nest can at most forge these container-owned inputs, and both forgeable directions are benign: forging "idle" only *accelerates* a crash-safe reboot, and forging "busy" is overridden by the un-forgeable ceiling (below). Because the container **owns** this dir, the defensive read is also **hang-proof** by construction — no container-owned input can block the coordinator's read or suppress a future reboot. Two combined defenses give that invariant: the connection-count read is **`timeout`-bounded** (a read that cannot complete is killed and reads as idle `0`, the benign direction), and the coordinator service carries an explicit **`TimeoutStartSec=`** (a `Type=oneshot` *disables* the start timeout by default), so any residual wedge self-clears (unit fails, timer re-arms) instead of becoming permanent.
- **`/opt/fauna/maintenance-host` ↔ `/data/maintenance-host` (`:ro`, root-owned `0755`)** — the **host→nest** direction. The root coordinator *writes* `host-status` here; the nest only *reads* it (read-only mount). Because the dir is root-owned and the container's view is `:ro`, a compromised nest can neither **symlink-plant** a path the root coordinator would follow (closed) nor **forge** the host status (closed). Defense-in-depth: `fauna-reboot-coordinator.service` is systemd-sandboxed (`ProtectSystem=strict` + `ReadWritePaths=` the two maintenance dirs + `PrivateTmp=true` + `NoNewPrivileges=true`), confining any stray write to those two dirs even though it runs as root.

The files exchanged:

- **nest → host (rw mount): `nest-readiness`** — `connection_count`, plus (while idle) `idle_since` (unix seconds). The nest rewrites it on a ~30 s tick (`bins/fauna-nest/src/host_maintenance.rs`). The coordinator (§ 2) reads `connection_count` (symlink-rejecting + digit-validated) to decide "idle." A missing, symlinked, or stale-because-crashed file reads as idle (`connection_count` treated as 0), which is correct — rebooting a dead nest is desirable. There is **no `in_flight` field**: a request can only be in flight over a live connection and there is no separate in-flight counter (`ws.rs`), so idle ⟺ `connection_count == 0`.
- **nest → host (rw mount): `restart-requested`** — the admin "restart now" flag (§ 4). The nest writes it; the coordinator reads it symlink-safely (a stat-only `[ -f ]`/`[ ! -L ]` test that never `open()`s the file, so no FIFO-hang vector) and consumes it with an `O_NOFOLLOW`-safe `rm -f` so it fires exactly once. The coordinator **ignores a restart-now within the first ~10 min of host uptime** (a min-reboot-interval guard keyed on the un-forgeable `/proc/uptime`), so a compromised container re-planting the flag after each boot cannot drive a tight reboot loop at the `OnBootSec` cadence; a genuine admin restart-now (on a box up longer) is immediate, and one issued just after boot is merely deferred to the next run (INFO-1).
- **host → nest (`:ro` root-owned mount): `host-status`** — `security_updates_pending`, `reboot_pending`, `reboot_deferred_since`, `last_patched_at` (flat `key=value`), written by the coordinator each run (atomic tmp-write + rename into the root-owned dir). `reboot_deferred_since` is the un-forgeable `/run/reboot-required` mtime (§ 2). The nest reads it and surfaces it on `fauna.setup.status` (§ 4).
- **host → nest (`:ro` root-owned mount): `host-addresses`** (ratified 2026-10-09; built 2026-10-10) — one `address=<ip>` line per global-unicast address the host holds on its own interfaces (IPv4 and IPv6, read from the host's interface table; loopback, link-local and private ranges excluded — the nest applies its own classification again, so the shell filter may stay coarse), written by the coordinator each run with the same atomic tmp-write + rename, and **once more on first boot, before `docker compose up`, by a dedicated publish entry point of the same script** — never a full coordinator run there, which on a freshly patched box could reboot before the compose is up — so the nest's first lifecycle tick already sees it. The file persists on `/opt/fauna` across reboots, so a reboot serves the last set until the coordinator's first post-boot run refreshes it. It is the second source of the IP bridge cert's derived enable and has that one consumer; it never feeds `nest_host_address` ([`../nest/domains-and-tls-bootstrap.md`](../nest/domains-and-tls-bootstrap.md) § Host-address acquisition stays client-reported). What the nest does with it, and why this is the source: [`../nest/tls-certificates.md`](../nest/tls-certificates.md) § B-IP *When*.

### 4 — Admin visibility (app UI, user decision 2026-06-28)

OS maintenance is **surfaced to the admin in all 7 apps** (chosen over fully-invisible). The nest exposes the `host-status` fields on **`fauna.setup.status`** — additive, version-skew-safe, **landed *with* their app consumer** (the `fronted_by_router` precedent in [`../nest/common.md`](../nest/common.md) — `serving_port_bind_failed` follows the same rule but has not landed; that doc's § Implementation status — serving ports says so): `os_host_status_present: bool`, `os_security_updates_pending: u32`, `os_reboot_pending: bool`, `os_reboot_deferred_since: Option<i64>`, `os_last_patched_at: Option<i64>` (all `#[serde(default)]`). `os_host_status_present` is `true` only when the nest read the `host-status` file; without it (a box with no host channel — hand-built, dev, desktop — or a host that has not written the file yet) the other four are their zero/`None` defaults, not a report, and the line says the host reports nothing rather than vouching for it. These five fields are **admin-gated** (OS-LEAK): `setup.status` is reachable **anonymously** (a pre-identity discovery kind), and host patch/reboot posture is not something an unauthenticated caller may learn — so the nest populates `os_*` only for an authenticated **admin** caller and returns the default "nothing pending" values to anonymous/non-admin callers (the wire fields stay additive — only their *population* is gated; the contract lives in [`../nest/common.md`](../nest/common.md) § `fauna.setup.status`). The admin/nest page renders a passive status indicator ("OS status not reported by this host" / "OS up to date" / "N security updates pending" / "Restart pending — will restart automatically when idle"), the first whenever `os_host_status_present` is `false` — the state→line decision is the shared `fauna_core::format::os_maintenance_status_label`. A manual **"restart now"** affordance (admin → nest writes a request flag into `/data/maintenance`, coordinator picks it up) **ships in v1 alongside the indicator** (user decision, 2026-06-28). **Surface approved** (user, 2026-06-28): it lives on the existing **admin/nest page**, and the Slice-2 author proposes the exact `ui.yaml` element IDs (working set: `nest-os-maintenance-status`, `nest-os-updates-count`, `nest-os-restart-now-button`) for ratification at review. Slice 2 is tracked internally.

### Implementation status (host OS maintenance)

**All four slices are built.** Slices 1–3 security-reviewed; the symlink-plant and ceiling-suppression gaps confirmed closed, the three residuals (hang-proof reads, `os_*` leak, min-uptime guard) fixed.

| Piece | Status |
|---|---|
| Slice 1 — cloud-init foundation: `unattended-upgrades` + `needrestart` list-only, reboot-coordinator script + timer, the two trust-split bind mounts, nest `nest-readiness` writer | Built (`libs/fauna-provisioning/src/cloud_init.rs`; `bins/fauna-nest/src/host_maintenance.rs` — None-gated no-op without the mount; witness `test_host_reboot_coordinator.py`, which runs the rendered coordinator end-to-end in a container) |
| Slice 3 — channel hardening: symlink-plant, ceiling suppression, `timeout`-bounded reads + `TimeoutStartSec=`, `os_*` admin-gated, min-uptime restart guard | Built |
| Slice 2 — nest + shared Rust: `os_*` on `fauna.setup.status` (`read_host_status` → `discovery_core`), `fauna.admin.request_host_restart` (Admin-class), shared `fauna_core::format::os_maintenance_status_label` (wasm + UniFFI), `AdminClient::request_host_restart` | Built |
| Slice 2 — app indicator + restart-now: `nest-os-maintenance-status` + `nest-os-updates-count` + `nest-os-restart-now-button` (ui.yaml-ratified) on the admin/nest page | Built on **all 7 apps** (web, linux, android, macOS + iOS, windows — 2026-06-28; tui via `admin/mod.rs`) |
| Slice 4 — `host-addresses` publication (§ 3): the publish function + first-boot entry point in the coordinator script, the runcmd call before `docker compose up`, the nest's root-owned-dir reader feeding the IP bridge derive | Built 2026-10-10 (`cloud_init.rs` `fauna_publish_host_addresses` + `publish-addresses`; `host_maintenance::read_host_addresses`; witness `test_acme_ip_bridge_host_published.py`) |
| No host data distinguished from up to date: `os_host_status_present` on `fauna.setup.status` (`HostStatus::present`, set when `read_host_status` reads the file), and `os_maintenance_status_label` answering `os_no_host_data` ("OS status not reported by this host") when it is `false` — so a hand-built box (internet or home), dev and desktop never read "OS up to date" | Built 2026-10-20 on all 7 apps (tests: `fauna-core` `os_maintenance_status_label_priority`, `host_maintenance` reader tests, `tests/api/test_host_maintenance.py`, `tests/test_host_maintenance.py`) |

History: tracked internally; per-app landing narratives live in git history.

---

## Managed Subdomain vs Own Domain

### Managed (`{name}.fauna.social`)

Note: Managed subdomains are not part of the current client-side provisioning flow and are reserved for future implementation.

### Own Domain

- The user provides DNS API credentials for their provider via the setup wizard.
- The client verifies credentials directly via the DNS provider's API and **publishes the DNS records itself** (`fauna-provisioning`) — the credentials stay client-side and are never passed to the container.
- The client creates the VPS instance; the container boots **domainless** (no domain env var, no DNS credentials — `FAUNA_DOMAIN` is retired) and learns the domain from the admin's claim handle.
- ACME uses **HTTP-01**: the nest serves the challenge on its own port — no DNS write, so the nest needs no DNS-provider key.
- The email DNS records MX, SPF, DMARC are published **client-side** by `fauna-provisioning`; the **DKIM** TXT is published from the nest's auto-provisioned `mail_dkim_keys.public_dns_value` (the public half of the key the nest signs with — `../../behavior/mail-bridge-lifecycle.md` § DKIM provisioning (automatic) — **not** a client-generated key). All are then verified read-only by the nest (`fauna.dns.verify_records`).
- The user retains full ownership and portability of the domain.

Trigger in Docker: none — the box boots domainless and the domain comes from the admin's claim handle (`FAUNA_DOMAIN` is retired; `../nest/domains-and-tls-bootstrap.md` § Env contract).

---

## Admin Claim

The claim transaction (code format, handle requirement, signature, atomicity, rejection) is owned by [`../../behavior/onboarding.md`](../../behavior/onboarding.md) § 3a — the sole transport is the pre-identity `fauna.auth.claim_admin` WS-RPC kind. The VPS-specific facts: the single-use code lives at `/data/claim-code` (deleted after claiming), and on a **provisioned** box it is minted **client-side** at provisioning (`fauna_provisioning::generate_claim_code` — a short, ambiguity-free code) and injected via cloud-init, so the wizard already holds it and there is nothing to retrieve from the box.

---

## The SSH door (admin option, owner-ruled 2026-10-20)

**What it is.** A default-off choice on the wizard's VPS-configuration page — *keep an SSH door to this server* ([`../../behavior/onboarding-provisioning.md`](../../behavior/onboarding-provisioning.md) § 5 owns the page; this section owns what the choice does to the box). **Off** — the default, and the posture of every box whose admin does not choose otherwise — the box is the keyless, sshd-less box [`../deployment-threat-model.md`](../deployment-threat-model.md) § Concern 1a describes: cloud-init disables `ssh.service`/`ssh.socket` and denies 22/tcp. **On**, the admin's app mints an Ed25519 keypair **for this box alone** on the admin's device, and the cloud-init the app already renders ([`behavior/onboarding.md`] § 5's `vps_config` intent reaches `build_cloud_init` the same way the mail intent does) installs the public key for `root`, a **key-only** sshd — the drop-in `/etc/ssh/sshd_config.d/0-fauna-door.conf` (`PasswordAuthentication no`, `KbdInteractiveAuthentication no`, `PermitRootLogin prohibit-password`, `AuthenticationMethods publickey`), named to sort **before** the provider's `50-cloud-init.conf` because sshd keeps the first value it reads per keyword and the provider's own cloud-init writes `PasswordAuthentication yes` for a keyless create, plus the cloud-config directive `ssh_pwauth: false`, and — because the provider creates a keyless server with root's generated password already expired, which makes PAM's account check end every key login that has no TTY (measured on the first live door box, 2026-10-20) — resets root's last-change date to the boot's day in `runcmd` (`chage -d`), the password itself staying unknown and refused by sshd — and leaves `ssh.socket` enabled; the cloud-init's `ufw deny 22/tcp` still stands, so **the port admits nobody until the claim**. No provider key is ever passed at create: the key travels only inside the cloud-init, uniformly across every VPS adapter, and the provider's generated root password (returned in the create response, never read) is neutralised by `PasswordAuthentication no` exactly as it is on a door-less box by the closed port.

**One address — the one the admin claims from (owner's word 2026-10-20).** The door's firewall whitelists exactly the public address the admin's app claims the box from, and nothing else. The nest learns the claimant's real source address on every connection (the in-container SNI router hands it over by PROXY protocol v2, `bins/fauna-sni-router`, resolved into the nest's `ConnectInfo`), so at the claim transaction ([`../../behavior/onboarding.md`](../../behavior/onboarding.md) § 3a) the nest writes it **once** to the nest→host maintenance file `ssh-door-source` (`address=<ip>`, the flat `key=value` form of § Host OS Maintenance → *3 — Host ↔ nest channel*) and keeps it in the box's admin-visible state; the claim reply echoes the address it wrote (`ClaimAdminReply.ssh_door_source`, absent when it wrote none) — the claimant's own address, so the echo crosses no boundary — and the wizard names it on the door's command at once ([`../../behavior/onboarding-provisioning.md`](../../behavior/onboarding-provisioning.md) § 5). The root coordinator's door pass — `/usr/local/sbin/fauna-ssh-door`, written with its own unit and a one-minute timer **only on a door box**, so a door-less box carries no door code and renders byte-for-byte as it would with no door at all, and a unit of its own because `ufw` writes `/etc/ufw` and `/run`, which the reboot coordinator's `ProtectSystem=strict` keeps read-only — reads the file defensively, as the coordinator reads every nest-written file (symlink-rejecting, timeout-bounded, the value a single IPv4 or IPv6 literal, and only when the door's drop-in exists on the host), runs `ufw prepend allow proto tcp from <ip> to any port 22` — **above** the cloud-init deny in that address family's chain, since ufw stops at the first matching rule (`prepend`, not `insert 1`, because ufw numbers IPv6 rules after every IPv4 one) — and records `ssh-door-admitted` (`address=<ip>`, `at=<unix>`; `refused=malformed` when the value was not one literal, which shuts the door for good) in the root-owned host dir, so it never acts again whatever the nest writes later, and switches its timer off. A failing `ufw` records nothing, so the next minute retries. The admin-visible state is `fauna.setup.status`'s `ssh_door_admitted_from`, read from the host's `ssh-door-admitted` (never the nest's own file, which a door-less host ignores) and admin-gated like the `os_*` fields. **Widening the door to another address is the admin's own act over the door** (`ufw insert 1 allow proto tcp from <next-ip> to any port 22`), made **before** their address changes; an admin whose address changed first is locked out of the door — not of the box, which the app still administers — and re-creates the box if they need the door back (accepted for now, owner 2026-10-20; a later cut may let the app re-admit from its current address through the same one-shot channel, signed by the admin key pinned at claim). The nest never holds a private key, and the health endpoint does not advertise the door. What a compromised nest gains: in the **claim window only**, the choice of the one admitted address — the reachability of a key-only sshd from one address of the attacker's choosing, which is the ordinary posture of most servers on the internet and nothing like a shell; after the claim the channel is inert.

**The key (owner-ruled 2026-10-20: the standard place, no sealed copy).** On a desktop app (linux, macos, windows, tui) the private key is written **where SSH keys live**: `~/.ssh/fauna-<domain>` (mode 0600, the directory 0700), with a `Host <domain>` entry appended to `~/.ssh/config` (`User root`, `IdentityFile ~/.ssh/fauna-<domain>`, `IdentitiesOnly yes`), so `ssh <domain>` works at once and the admin's own tooling sees the key; the app keeps no copy afterwards and holds no secret. The key is **this device's**, as SSH keys are: a second admin device gets no door, and a lost device means re-creating the box if the door is wanted again — the same answer as a changed address, accepted. On a phone (ios, android) and on web, which have no `~/.ssh` and no ssh client, the app offers the key file **once**, at the end of provisioning — a share sheet on the phones, a download on web — and the admin puts it where their ssh client reads it. Hygiene: the app never overwrites an existing file at that path (it suffixes the name and names the file it wrote); a re-created box mints a fresh key and removes the old `Host` entry; a sandboxed macOS app that cannot write `~/.ssh` silently writes it through the save panel defaulting to that path (the implementing row checks the app's distribution). The app shows the exact command (`ssh <domain>`, or `ssh -i <file> root@<domain>` where no config entry was written) with the admitted address.

**What the door is not.** Fauna **never needs it**: no install, update, recovery or configuration path uses a shell — the apps stay the one configuration surface ([`../../principles.md`](../../principles.md) § One configuration surface), and a change made over the door to the compose, the nest's files or the host is **unsupported** and may be overwritten by the updater or a re-creation. It **cannot be opened after the create** — the admin-settings toggle is deferred: enabling sshd on a running box means a root-side request from the unprivileged nest, a security design of its own (a request signed by the admin key pinned at claim), to be built on demand; the admin app shows the door's state read-only. It is a **deliberate reduction of the box's security**, and the wizard says so in a warning the admin reads before the toggle takes (a public text, § 5 of the wizard doc). **The organisation's nests:** the admin of `dev.fauna.build` opens the door — the kept-box drive does it through the app, as any admin would, on the box's next re-creation, retiring the hand-applied shell of 2026-10-14 (`deployment-threat-model.md` § Concern 1a) — and `test.fauna.build` and `fauna.build` keep it shut; `deploy-verify` keeps asserting `:22` closed on them and reads the dev nest's answering `:22` as a NOTE.

**Implementation status today: box side BUILT, wizard BUILT in shared Rust and on tui, linux, web and android (2026-10-20).** `CloudInitParams.ssh_door` renders the drop-in, `ssh_pwauth: false`, root's key (written `defer: true`, after cloud-init's own `ssh` module) and the `fauna-ssh-door` script, unit and timer, and keeps sshd enabled, with `ufw deny 22/tcp` standing (`libs/fauna-provisioning/src/cloud_init.rs`, unit-tested: the door-less render unchanged, the drop-in's sort order, the door pass's one-shot admission, its refusals and its retry run against a stub `ufw`); the claim writes `ssh-door-source` from the anonymous connection's peer and echoes it in the claim reply (`bins/fauna-nest/src/claim_handlers.rs`, `host_maintenance.rs`) and `fauna.setup.status` carries `ssh_door_admitted_from`. The wizard half — the toggle, the key minted on the device and written to `~/.ssh` with its `Host` entry, `CloudInitParams.ssh_door` filled — is built in shared Rust and on tui; its state and gaps are `behavior/onboarding-provisioning.md`'s *Implementation status today (SSH door)*. **Not built:** the macos, ios and windows wizard elements (linux, web and android followed tui on 2026-10-20). **First real box, 2026-10-20:** `dev.fauna.build` (Hetzner, Ubuntu 26.04), its door opened by the kept-box drive through tui; the run found two defects no fake cloud shows, both fixed in `cloud_init.rs` — the largest render over Hetzner's 32,768-byte `user_data` limit (now `USER_DATA_MAX_BYTES`, the box scripts rendered without their comment lines) and the provider-expired root password that refused every key session (the `chage` line above).

---

## Recovery

- The claim code is at `/data/claim-code` on the VPS (single-use — deleted after claiming).
- The nest's **deployment signing seed** (its `nest_actor_id` identity) is what a rebuilt box must re-present after **total box loss**; recovery custodies it off-box in the admin's client-synced config and re-installs it at re-provision — see [`../nest/box-recovery.md`](../nest/box-recovery.md). It is never printed or retrieved by hand; custody is wholly client-side.
- The VPS is accessible via the provider's dashboard (console or VNC; the box runs no sshd and allows no 22/tcp) regardless of nest health.
- All persistent state lives in the Docker volume at `/data/`. Recreating the container from the same volume restores the nest fully.

---

## Uninstall

**From the provider's dashboard:** delete the VPS instance. This removes the server, the data volume, and the running container.

**From the VPS directly:**

```bash
# Stop and remove containers (data preserved)
docker-compose down

# Also delete all nest data (irreversible)
docker volume rm fauna-data
```

DNS records created during provisioning must be removed manually from the DNS provider's dashboard if the domain is to be reused.

**Programmatic teardown — the crate primitive.** `fauna-provisioning`'s `VpsProvider` trait now has a first-class **`delete_server`** across all five named providers (`libs/fauna-provisioning/src/vps/mod.rs`; Hetzner `DELETE /v1/servers/{id}`, DigitalOcean `/droplets/{id}`, Linode `/linode/instances/{id}`, Vultr `/instances/{id}`, OVH signed `/cloud/project/{id}/instance/{id}`) — the generic bundled adapter (§ Supported Providers) implements it too, over the same `finish_delete` 404-is-success helper — closing the asymmetry with the DNS side's `delete_record`. It is **idempotent** — a `404` (server already gone) is a no-op success, so a double-decommission never raises. This is the crate primitive that backs the client-side retire flow ([`../../behavior/nest-retirement.md`](../../behavior/nest-retirement.md)); the listing primitive that finds the retirable boxes (`list_managed_servers`) is built too — see § Implementation status today.

**The `managed-by=fauna` marker (ratified 2026-07-08).** Every fauna-provisioned box carries the stable provider-side label **`managed-by=fauna`** (`fauna_provisioning::vps::MANAGED_BY_LABEL`, a hard-coded constant mirroring the `app.kubernetes.io/managed-by` convention — never a config surface). The orchestrator unions it with caller labels at its single create chokepoint (`run_server_step`), so no caller can forget it; the live-provision e2e's `fauna-e2e=1` sweep label rides alongside it. Provider mapping: Hetzner and any future label-map provider carry it natively; DigitalOcean/Linode/Vultr encode labels as flat **`key:value`** tag strings (colon, not `=` — DigitalOcean's tag charset allows only letters, numbers, colons, dashes, and underscores); OVH instance-create has no label field, so OVH boxes carry no marker.

**Listing primitive (ratified 2026-09-19, BUILT 2026-09-21).** The retire view needs a list the trait does not have: `find_server_by_name` is the only lookup, and `VpsInstance` (`server_id`, `ipv4`) carries nothing a list can show. The `VpsProvider` trait gains **`list_managed_servers(&self, client) -> Result<Vec<ManagedServer>, ProvisionError>`**, where `ManagedServer { server_id, name, ipv4: Option<String>, ipv6: Option<String>, labels: Vec<(String, String)>, created_at: Option<String>, marked: bool }` (convertible to the `VpsInstance` `delete_server`/`get_ptr` take). The list is **filtered to fauna boxes by the marker** — Hetzner via `label_selector=managed-by=fauna` server-side; DigitalOcean/Linode/Vultr by the `managed-by:fauna` tag string (server-side tag filter where the API offers one, client-side match otherwise); the bundled adapter via `GET /v1/servers?label=managed-by=fauna` (`../provisioning/bundled-provider-api.md`; the adapter sends only `?name=` today); OVH (no labels) returns the **unfiltered** project list with every row `marked: false` — across **every project the credentials see**, each row's `server_id` `<project>/<instance>` so `delete_server` reaches the project that holds it (built 2026-10-14; since 2026-10-21 the adapter holds no chosen project at all, every OVH server id carrying its project). Every adapter follows pagination to the end. Filtering to the marker is what keeps a user from ever being shown — let alone deleting — a non-fauna server that happens to live in the same cloud account; an unmarked row is deletable only behind the view's typed per-box confirm. Boxes provisioned before 2026-07-08 predate the marker and won't appear in the filtered list; the provider dashboard remains their teardown path (acceptable: pre-alpha boxes are few and disposable). Each adapter additionally **re-checks the marker client-side** after sending its filter — a provider that silently ignores an unknown filter parameter must not be able to hand the view the user's whole account — and the walk is bounded by `MAX_LIST_PAGES`. Conformance: `libs/fauna-provisioning/tests/vps_list_managed_servers_conformance.rs`, beside `vps_delete_server_conformance.rs`, pinning per adapter the filter sent, pagination across two pages, and a non-fauna server's exclusion, plus OVH's unfiltered/`marked: false` contract and its account-wide walk; the bundled `?label=` pin lives in `tests/bundled_conformance.rs`.

**The app view** — entries, page flow, credential stance, box→domain attribution, the typed confirm, and the DNS-cleanup scope and order (**DNS records first, then `delete_server`**: the reverse order strands `A` records on a released address if the run dies between the two) — is owned by [`../../behavior/nest-retirement.md`](../../behavior/nest-retirement.md).

The live provisioning e2e (`tests/e2e-unified/tests/live/test_hetzner_provision.py`, `testing.md` § Gap 3) decommissions its throwaway box via the Hetzner API directly (a Python `DELETE` in the always-runs teardown fixture, which fires when the app process may already be down — so it deliberately does not route through the crate/bridge; the crate path is proven separately by the `vps_delete_server_conformance.rs` wiremock harness). Its orphan sweep selects boxes by the `fauna-e2e=1` provider label (`create_server` labels, set via the machine's `set_provision_labels` bucket-1 IPC) rather than a name-prefix heuristic.

---

## Implementation status today

**Server release 26.04 (ruled 2026-10-20) — BUILT for every named adapter — the four the wizard showed on 2026-10-20 and OVH since its project → region flow was built (2026-10-21) — and stated in the bundled contract (2026-10-20).** Hetzner (`"image": "ubuntu-26.04"`), DigitalOcean (`ubuntu-26-04-x64`), Linode (`linode/ubuntu26.04`) and Vultr (`os_id` 2760, "Ubuntu 26.04 LTS x64") create 26.04, each identifier read off the provider's own image listing that day and pinned per adapter by `libs/fauna-provisioning/tests/vps_create_image_conformance.rs`; the live throwaway-box run (`tests/e2e-unified/tests/live/test_hetzner_provision.py`) asserts Hetzner's record of the booted image is Ubuntu 26.04 (the box has no SSH to read `/etc/os-release` over) — not yet witnessed green: its 2026-10-20 run stopped at the wizard's no-build preflight, no Stable (`:latest`) build being published yet. **OVH** (the owner's 2026-10-21 exception, ended when its flow was built) resolves the image at create time instead of naming it: OVH's instance-create takes the flavor and image *ids*, so the adapter reads the region's linux image list and takes the image named `OVH_UBUNTU_IMAGE_NAME` ("Ubuntu 26.04"), and a region without one is an error naming the region, never a fall-back to an older release (`../provisioning/registry.md` § Server offers follow the location → *OVH*); pinned by the same test, both the resolved create and the no-image error. The image name is read off OVH's published API shape, not a signed listing: the live witness confirms it. The generic bundled adapter's create body carries no image field — the intermediary picks the image from its own supplier — so the release is a rule of its wire contract instead: `../provisioning/bundled-provider-api.md` § Endpoints requires the created server to boot Ubuntu 26.04 LTS, because the verbatim cloud-init is written for it (settled 2026-10-20; the contract, not the adapter, is where a bundled provider learns it, and nothing on the wire checks it). `cloud_init.rs` needed no change: every package it installs (docker.io, docker-compose-v2, ufw, unattended-upgrades, needrestart) is in the 26.04 archive, the `ssh.socket`/`ssh.service` units it disables still exist, and the unattended-upgrades origin syntax is unchanged. Existing boxes move only when re-created (`dev.fauna.build`, `test.fauna.build`, `fauna.build` are still 24.04 until then).

**First-boot retry unit (step 3, ruled 2026-10-19) — BUILT 2026-10-19.** `libs/fauna-provisioning/src/cloud_init.rs` writes `/usr/local/sbin/fauna-nest-up` (a POSIX `sh` loop on `docker compose -f /opt/fauna/docker-compose.yml up -d`, logging each failure and sleeping `NEST_UP_RETRY_INTERVAL_SECS` = 60 s) and `fauna-nest.service` (`Requires=docker.service`, after `network-online.target`, `WantedBy=multi-user.target`); `runcmd` no longer runs the compose itself — it ends with `systemctl enable fauna-nest.service` and `systemctl start --no-block fauna-nest.service`, after the maintenance dirs and the `publish-addresses` call. The unit is `Type=exec` with `RemainAfterExit=yes`, not `oneshot`: a target is ordered after every unit it wants, so a oneshot still retrying would hold `multi-user.target` for the whole outage. Pinned by the crate's unit tests (the render, the runcmd order, and the loop against a stub `docker`) and by `tests/e2e-unified/tests/platform/docker/test_first_boot_nest_up.py` (tier_4: the shipped script at its real path in a Debian container, a stub `docker` failing three times then succeeding).

**Steps 7–8 of § What Happens Behind the Scenes describe the 2026-08-29 ratification — now largely BUILT** (corrected here 2026-09-09; this section had stood as "NOT BUILT yet" since sweep, which was already stale by 2026-08-31): the standard path claims the box as `Online`'s final substep and `continue_from_provisioning` refuses an unclaimed box outright; the account's reach hint is persisted and dialed on relaunch (wired on tui + web at the `LoggedIn` terminal, with linux/android/apple/windows legs still open); and the pending-provision slot is wired on all seven apps. The web Online poll's TLS wall is believed removed by the 2026-09-02 IP bridge cert but not yet re-measured on a live web run. Owner, full measured detail and capture rows: `../../behavior/onboarding-provisioning.md` § Implementation status today (this content moved there from `onboarding.md` on 2026-09-06); the nest half (the IP bridge cert) is `../nest/tls-certificates.md` § Implementation status today.

**§ Uninstall:** the crate primitives are implemented and tested (`delete_server` on all five named providers — `vps_create_labels_conformance.rs` / `vps_delete_server_conformance.rs` — and on the generic bundled adapter — `tests/bundled_conformance.rs`; the `managed-by=fauna` marker unioned at the orchestrator create chokepoint, covered for both). The **listing primitive is now built too** (2026-09-21): `list_managed_servers` + `ManagedServer` on the trait and all six adapters, marker-filtered and re-checked client-side, pagination followed to the end, with `vps_list_managed_servers_conformance.rs` and the bundled `?label=` pin. **The app view** — its design and ID package are [`../../behavior/nest-retirement.md`](../../behavior/nest-retirement.md)'s (the IDs approved by the user 2026-09-25, in `ui.yaml` since 2026-09-26) — **is built on tui, the lead app, and on linux, web and android** (this said "on no other app yet" until 2026-10-19, behind its owner's status); that doc's § Implementation status today carries the per-app state, the shared-Rust half's own state and whatever arm is still unbuilt.

The setup wizard and client-side provisioning via the `fauna-provisioning` crate are fully implemented. The 5 named DNS providers (Cloudflare, Porkbun, Hetzner, Namecheap, Gandi — `i18n/providers.yaml`) and all 5 named VPS providers are usable from the wizard (plus the generic bundled adapter — § Supported Providers): OVH, the fifth, since 2026-10-21, when the tui wizard e2e `tests/e2e-unified/tests/test_vps_config_ovh.py` drove its project → region picker, priced offers, re-list and resolved create end to end against the fake cloud; its live witness against a real OVH account is still owed (`../provisioning/registry.md` § Implementation status today); and **web-only provisioning through the 4 `cors_policy: proxy` providers (Cloudflare, Namecheap, Gandi, Vultr) is degraded** until `proxy.fauna.social` deploys — see the warning under § Supported Providers → DNS Providers. Not all VPS providers expose region selection at the same level — OVH in particular requires three separate credentials (app key, app secret, consumer key) and scopes its regions under projects; its picker entries are `<project>/<region>` pairs (`../provisioning/registry.md` § Server offers follow the location → *OVH*, designed and built 2026-10-21). Automated provisioning is most polished for Hetzner; other providers follow the same code path via the shared `fauna-provisioning` library.

The dispatch model for the `fauna-provisioning` crate has been confirmed and refined by recent specs:

- **2026-04-25 provider-vps-dns capability survey** and **2026-04-27 provider-registry-uniffi**: VPS/DNS providers and capabilities (DNS edit, VPS create, VPS PTR, …) are declared in `i18n/providers.yaml` and surfaced through the `fauna-provisioning-registry` UniFFI surface (see [`../provisioning/registry.md`](../provisioning/registry.md)).
- **2026-04-27 vps-set-ptr**: the registry exposes a per-provider PTR-setting capability used by the wizard once the VPS is provisioned.
- **2026-04-28 dns-config-state-machine**: DNS configuration progresses through an explicit state machine; the wizard reflects the current state and surfaces any manual fallback (`AwaitingManualDns` slot) per [`../../behavior/onboarding.md`](../../behavior/onboarding.md).
- **2026-04-25 nest-wizard back-buttons + VPS/DNS swap** and **2026-04-27 provisioning-progress**: in-wizard back navigation and the VPS↔DNS step-swap behavior are specified; provisioning progress UI surfaces typed states.

Provider list refreshes follow `i18n/providers.yaml` — re-read the registry doc when adding or removing providers rather than tracking the list in this installer doc.

**DKIM key plumbing (tracked internally, item D) — dead client-side generation REMOVED 2026-06-22.** The prose above is now the implemented state: the DKIM signing key is minted and held nest-side (per `../../behavior/mail-bridge-lifecycle.md` § DKIM provisioning (automatic)), and the DKIM TXT is published from `mail_dkim_keys.public_dns_value`. The former dead client-side path — the `entrypoint.sh generate-dkim`→`/data/dkim.pem` step, the `config.email.dkim_key`/`dkim_selector` fields, the `FAUNA_DKIM_*` env vars, and the `fauna-provisioning` cloud-init DKIM keygen+embedding — has been **removed**: cloud-init no longer embeds a DKIM key, and the provisioning orchestrator no longer publishes a (mismatched) client-generated DKIM TXT during step 3. This closes the divergence that would publish a key the deployment never signs with. **DNS-side status (stale as of 2026-06-22, corrected):** a managed-mode domain auto-publishes the DKIM TXT (part of the generic zone-publish set since 2026-06-07) and auto-reconciles it — the withdraw-aware re-mint/rotation-cleanup converge pass (`reconcile_dkim_txt`, tracked internally as item E) landed 2026-06-23/07-17; only a **manual-mode** domain (no held credential; the managed opt-in publishes on every app since 2026-09-25, `../../behavior/dns-management.md` § Implementation status today — this said "every domain today" until 2026-10-19) still needs the admin to paste the DKIM TXT from the `admin-dns` page, with a stale published key caught by deploy-verify gate 6. Detail + design: [`../../behavior/dns-management.md`](../../behavior/dns-management.md) § Fauna-managed → Withdraw-aware convergence.
