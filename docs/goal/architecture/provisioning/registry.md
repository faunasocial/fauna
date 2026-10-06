# Provisioning Provider Registry — target state

Owns: provisioning
Status: ratified
Authority: provisioning-registry architecture — the declarative provider registry (`i18n/providers.yaml` schema: capabilities, fields + kinds, cors_policy, post_verify_select, curated_offers, tld_pricing_endpoint, modes), the providers-generate codegen pipeline and its consumers, `fauna-provisioning` crate layering (traits, enum dispatch, snapshot orchestrator, cloud-init build, nest-side DKIM keygen), the CORS-proxy boundary, the registrar trait contract, and the registry-side shape of the bundled provider (the generic `bundled` entry, the `hosted-auth` field type, the `curated_offers` wildcard — the wire contract itself is [bundled-provider-api.md](bundled-provider-api.md)'s); defers the VPS deploy flow + host lifecycle to [../installers/vps.md](../installers/vps.md), cloud-init/container runtime detail to [../installers/docker.md](../installers/docker.md), wizard UX + the per-app adoption matrix to [../../behavior/onboarding.md](../../behavior/onboarding.md), and deployment-seed custody to [../nest/box-recovery.md](../nest/box-recovery.md).

A single declarative provider registry (`i18n/providers.yaml`) drives codegen for all 7 apps, the backend provisioning crate, and the e2e test harness — no per-provider code lives in any app. Source of truth: `i18n/providers.yaml` + `libs/fauna-provisioning/` + `libs/fauna-onboarding-machine/`. Last verified: 2026-08-25.

## Goal

A user opens the wizard in any app (web, Linux, Windows, macOS, iOS, Android, tui), enters credentials for a VPS provider, picks a DNS provider they control or buys a new domain inside the wizard, and ends up with a running nest on their own domain. The only outbound dependency from the wizard is the provider APIs themselves — no `*.fauna.social` call path for the reference flow (Porkbun registrar + Porkbun DNS + Hetzner VPS), which all have permissive CORS.

The architectural property: one YAML file (`i18n/providers.yaml`) is the declarative registry of VPS, DNS, and registrar providers. Codegen drives every consumer (7 apps, the backend provisioning crate, the e2e test harness); no app carries per-provider code.

## Originating specs

The current registry shape is the result of several layered design passes; preserved here for provenance:

- **Originating plan:** ratified 2026-04-24 (tracked internally).
- **Foundation extension:** ratified 2026-04-25 (tracked internally); added `curated_offers`, Hetzner DNS, and `tld_pricing_endpoint`.
- **Capability survey:** ratified 2026-04-25 (tracked internally); formalized the per-provider capability matrix (DNS edit, VPS create, VPS PTR, …).
- **UniFFI surface:** ratified 2026-04-27 (tracked internally); collapsed the 5 per-language `Providers.*` mirrors into a single Rust source served via `OnboardingMachine` snapshot methods (UniFFI for native, wasm-bindgen for Web).
- **VPS PTR capability:** ratified 2026-04-27 (tracked internally); added per-provider PTR-record-setting as a registry capability used by the wizard after `create_server` succeeds.

## Data flow

```
i18n/providers.yaml          single source of truth
        │
        ▼
scripts/providers-generate.py    codegen
        │
   ┌────┴────┬──────────┬──────────┬──────────┬──────────┐
   ▼         ▼          ▼          ▼          ▼          ▼
  Rust    TypeScript   Swift    Kotlin      C#        Python
  enum     const      struct    data       record     dict
            array      array    class                 (runtime
                                                       YAML)
   │         │          │          │          │          │
   ▼         ▼          ▼          ▼          ▼          ▼
 fauna-    fauna-    FaunaKit   fauna-     fauna-       e2e
 provis-   web       (iOS,      android    windows      tests
 ioning    (Svelte   macOS)     (Compose)  (WinUI)
 crate     generic
           form)
   │
   ▼
runtime  →  dispatch::<kind>_provider(id, creds) → trait impl → API call
                        ▲
                        └── the variant this yields is declared back in
                            providers.yaml (`dispatch` / `dns_dispatch` /
                            `registrar_dispatch`) and pinned against the code
                            by dispatch_registry_bijection.rs
```

## Why YAML + codegen, not runtime parsing

- Native apps shouldn't ship YAML parsers for build-time constants.
- Strong types: Swift, Kotlin, C#, and TypeScript all get enum types per provider.
- Matches the existing `i18n/strings/en.yaml` → `just i18n-generate` pattern the codebase already runs.
- WASM bundle stays small (no `yaml-rs` dependency on the critical path).

## Crate layering

| Layer | File | Role |
|---|---|---|
| Declarative spec | `i18n/providers.yaml` | Which providers exist, what fields, what capabilities, what CORS policy. |
| Codegen | `scripts/providers-generate.py` | Reads the YAML, writes per-app output files. |
| Generated metadata | `libs/fauna-provisioning/src/providers_generated.rs` + 5 siblings | `ProviderId` enum + `PROVIDERS` const array. |
| Traits | `libs/fauna-provisioning/src/{dns,vps,registrar}/mod.rs` | `verify`, `create_record`, `create_server`, `set_ptr`, `check`, `register`. |
| Impls | `libs/fauna-provisioning/src/{dns,vps,registrar}/<provider>.rs` | One file per provider × capability. |
| Shared per-provider transport | `libs/fauna-provisioning/src/namecheap_api.rs` | Where one provider serves several capabilities from a *single* authenticated endpoint, the transport is a crate-root module both adapters hold rather than logic each re-derives. Namecheap is the only such provider today (§ Proxy boundary → *Namecheap's IP allowlist* explains why re-deriving it is dangerous). Gandi and Porkbun need no equivalent — their capability adapters are independent REST clients. |
| Dispatch | `libs/fauna-provisioning/src/dispatch.rs` | `ProviderId` + `Credentials` → enum-wrapped trait impl. |
| Orchestrator | `libs/fauna-provisioning/src/orchestrator.rs` | `provision_with_snapshot`, `provision_with_registration_snapshot`, `provision_nest_no_dns` — all over the `run_server_step` chokepoint. |
| Progress | `libs/fauna-provisioning/src/progress.rs` | `ProvisioningSnapshot` + the four user-visible steps (Domain/Server/Dns/Online), `run_step`, `CancelFlag`. |
| Cloud-init build | `libs/fauna-provisioning/src/cloud_init.rs` | `CloudInitParams` → compose/env render, incl. the deployment seed (custody: `../nest/box-recovery.md`); carries **no** DKIM material — embedding a client-generated key would publish one the bridge never signs with, so cloud-init omits it entirely (regression-tested). |
| DKIM keygen | `libs/fauna-provisioning/src/dkim.rs` | **Nest-side** keygen (`mint_signing_key`), invoked by the nest binary when a mail domain is added, at boot and at rotation; the nest keeps the key — not part of the client's cloud-init provisioning flow (see § Provisioning progress below). |
| Probe / proxy | `libs/fauna-provisioning/src/{probe,proxy}.rs` | Domain-status probe helpers; CORS-proxy routing. |
| WASM shims | `libs/fauna-wasm-onboarding/src/lib.rs` | Page-support `#[wasm_bindgen]` fns for web (§ WASM surface); provisioning itself rides `OnboardingMachine`. |

## Dispatch uses enum wrappers, not trait objects

The provider traits use native `#[allow(async_fn_in_trait)]` (not the `async_trait` macro), which makes them NOT object-safe. `Box<dyn DnsProvider>` doesn't compile.

Instead, dispatch returns concrete enum wrappers (`DnsDispatch`, `VpsDispatch`, `RegistrarDispatch`) that implement the trait by dispatching each method through their variants. This means no per-call heap allocation and no crate-wide macro dependency.

Exhaustive `match ProviderId { ... }` in dispatch (no `_ => None` wildcards) forces compile errors when new providers are added, catching integration gaps at the type level.

**What exhaustiveness does not catch, and what does.** A new `ProviderId` forces an arm in all three constructors — but the arm may legitimately be `None` (a capability boundary), and every credential lookup inside it is a *string* lookup against the registry's field ids (`creds.get("api-token")?`). So renaming a field id in `providers.yaml`, or declaring a capability whose adapter was never wired, degrades to a silent runtime `None` that the wizard reads as "this provider has no DNS" — indistinguishable from a genuine boundary, with no compile error.

The registry closes that gap by naming the expected adapter per capability. `providers.yaml`'s three dispatch keys — `dispatch`, `dns_dispatch`, `registrar_dispatch` — are normalized by the generator into one variant per declared capability (`dispatch` is the provider's DNS variant unless it also does VPS, in which case `dispatch` is the VPS one and DNS moves to `dns_dispatch`; `registrar_dispatch` is always explicit) and emitted as `ProviderMeta::{dns,vps,registrar}_dispatch`, read through `ProviderMeta::dispatch_variant(Capability)`. The code's matching half is `DnsDispatch::variant_name()` and its two siblings.

`libs/fauna-provisioning/tests/dispatch_registry_bijection.rs` asserts the two halves agree for every (provider, declared capability): the constructor builds `Some` from a credentials bag assembled *from the registry's own `FieldMeta`* — which is what makes a field-id rename visible — and the variant it returns is the one the registry names. The converse is pinned too: an undeclared capability builds nothing, and an empty bag builds nothing anywhere (so a constructor that ignored its credentials could not satisfy the first property vacuously). Malformed declarations fail earlier still, in the generator's `_validate_dispatch`.

## The owner-name contract: the caller relativizes, adapters consume verbatim

DNS providers disagree on how a record's owner name is spelled: most want it **zone-relative** (`@`, `mail`, `_dmarc`), Cloudflare's v4 API consumes and returns **fully-qualified** names. `DnsProvider::record_names_relative_to_zone()` declares which — `true` for every adapter, overridden to `false` by Cloudflare alone.

**The caller owns the conversion.** By the time a `name` reaches an adapter (`create_record`'s `record.name`, `find_records`/`delete_record`'s `name`) it is already in the form that provider's API wants. Adapters use it verbatim. The **only** sanctioned per-adapter transform is translating the shared `@` apex sentinel into a provider's own spelling — Porkbun's is a blank string, and it is the sole instance. An adapter must not re-derive relativity by stripping a `.<zone>` suffix.

**This is the only contract the seam can express, not a preference.** Adapters receive `zone_id`, never the zone *name*. Hetzner's `zone_id` is an opaque integer, so a Hetzner adapter physically cannot strip a zone suffix; Cloudflare has opted out. Gandi, Namecheap and Porkbun could only ever appear to defend themselves because those three happen to use the zone name as their id — an accident of those APIs, not a property of the seam.

So a defensive strip in the three adapters that *can* buys nothing and costs detection: it makes a caller that forgot to relativize look correct on three providers while silently corrupting the two that cannot defend. That asymmetry caused a **production outage on 2026-07-24** — `fauna-client-dns` passed fully-qualified owners, and Hetzner's RRset API stored every record at a doubled name (`_acme-challenge.example.com.example.com.`), visible to an API GET but never resolvable at the name the CA queried, failing ACME twice (history: [../nest/tls-certificates.md](../nest/tls-certificates.md) § the 2026-07-24 dual defect). Removing the strips makes a future violation fail uniformly and visibly on all five adapters instead of silently on two.

Both callers hold up their end, through one shared function: the provisioning orchestrator via `orchestrator::dns_record_name` in `to_provider_record`, and `fauna-client-dns`'s managed-DNS seam via its own `owner_name`, which calls it. A new caller must do likewise; a new adapter may assume it. Pinned per-adapter by `libs/fauna-provisioning/tests/dns_conformance.rs` (`*_does_not_re_relativize_a_relative_owner_ending_in_the_zone`, plus the apex wire-shape tests) and caller-side by `fauna-client-dns`'s `owner_name_tests`.

## Proxy boundary

`libs/fauna-provisioning` compiled to native Rust hits provider APIs directly — no intermediary.

On WASM (web app only), browser CORS blocks the same calls for providers that don't serve permissive CORS headers. This is entirely a browser restriction; the provider's endpoint is unchanged.

Solution: `services/fauna-cors-proxy/` — a stateless HTTP byte-forwarder that adds permissive CORS headers. Deployed to `proxy.fauna.social` (the fallback constant in `libs/fauna-provisioning/src/proxy.rs` and `i18n/providers.yaml`; self-hostable). It does not parse request bodies, does not log credentials, and sees nothing the provider's own server wouldn't see (TLS terminates at the provider).

**This is the only `*.fauna.social` dependency any app has for provisioning, and it's entirely self-hostable.** Native apps ignore `cors_policy` and always call providers directly.

The `cors_policy` field in `providers.yaml` controls routing:

- `open` — browser fetch works directly; no proxy.
- `proxy` — route through `proxy.fauna.social` or a self-hosted instance.
- `native` — provider is native-only; web hides it from the UI.

**How `proxy` is honoured, and how that is enforced.** A `proxy` provider's adapter must build its default base with `proxy::default_api_base(DIRECT_API, PROXY_PREFIX)` rather than hard-coding the direct URL; `default_api_base` returns the direct URL on native and a `{proxy_root}/{PROXY_PREFIX}` URL on wasm32. `PROXY_PREFIX` is the proxy's provider key followed by the direct URL's path (`gandi/v5`, `cloudflare/client/v4`, `vultr/v2`, `namecheap/xml.response`), because the forwarder resolves `/{provider}/{*path}` against its base table and appends the remaining path. The registry declaration and the adapters are held in bijection by `libs/fauna-provisioning/tests/cors_policy_bijection.rs`, which constructs each `proxy` adapter for `BuildEnv::Web` and asserts the resulting base is proxy-rooted (and, for `BuildEnv::Native`, still direct) — a native test can only do this because `proxy::default_api_base_for_env` takes the build env explicitly. Declaring `cors_policy: proxy` without wiring the adapter fails that test.

**Namecheap's IP allowlist: the provider's own rejection is the reflector.** Namecheap enforces API access against the request's **source** address, which differs by build env — the CORS proxy's on web, the user's own on native — and requires a syntactically valid `ClientIp` parameter on every call (a missing one is rejected outright). Neither source address is knowable up-front: determining a public address needs an external STUN/echo reflector, and picking one is a deferred self-hosted-invariant decision (`../nest/domains-and-tls-bootstrap.md` § Host-address acquisition). The resolution needs no reflector and no per-build-env branching: the adapter seeds `ClientIp` with a placeholder, and Namecheap's rejection **echoes the source address it observed**, so the adapter retries once carrying that value and — if it is still refused — surfaces an error naming the exact address the user must allowlist. The proxy stays a generic byte-forwarder with no provider-specific knowledge, and web and native run the identical code path. ⚠ This is only reachable because Namecheap reports API-level failures with **HTTP 200** and a `Status="ERROR"` body: an adapter that checks only the HTTP status reads every failure as success, including a `setHosts` that wrote nothing. Namecheap responses are therefore checked at the API level, not the transport level.

**The forwarder preserves the query string.** `/{provider}/{*path}?{query}` forwards to `{base}/{path}?{query}`. This is load-bearing, not incidental: Namecheap passes every parameter — credentials, `ClientIp`, and the `Command` selecting the operation — in the query string, so a path-only forward would reach Namecheap as a different request than the client made; Cloudflare and Vultr carry pagination and filters the same way. Pinned by unit tests in `services/fauna-cors-proxy/src/main.rs`.

### Current CORS policies (as of 2026-04-24 Phase 0 audit)

| Provider | CORS policy | Basis |
|---|---|---|
| Hetzner | `open` | API returns `Access-Control-Allow-Origin: *` |
| Porkbun | `open` | API returns `Access-Control-Allow-Origin: *` |
| OVH | `open` | API returns `Access-Control-Allow-Origin: *` |
| DigitalOcean | `open` | API returns `Access-Control-Allow-Origin: *` |
| Linode | `open` | API returns `Access-Control-Allow-Origin: *` |
| Cloudflare | `proxy` | OPTIONS returns 400; no ACAO header |
| Vultr | `proxy` | OPTIONS returns 405; no ACAO header |
| Gandi | `proxy` | OPTIONS returns 401; no ACAO header |
| Namecheap | `proxy` | No ACAO header; also requires IP allowlist |

### Capability matrix (handle-first foundation)

| Provider | Capabilities | VPS PTR | Curated offers (`curated_offers`) | TLD pricing |
|---|---|---|---|---|
| Hetzner | `dns, vps` | yes (per-IP `rdns`) | `cx23, cx33, cx43, ccx13, ccx23` | — |
| DigitalOcean | `vps` | yes (PTR managed via reverse-DNS) | `s-1vcpu-1gb, s-1vcpu-2gb, s-2vcpu-2gb, s-2vcpu-4gb, s-4vcpu-8gb` | — |
| Linode | `vps` | yes (per-IP rDNS) | `g6-nanode-1, g6-standard-1, g6-standard-2, g6-standard-4, g6-dedicated-2` | — |
| Vultr | `vps` | yes (per-IP reverse) | `vc2-1c-1gb, vc2-1c-2gb, vc2-2c-4gb, vhf-1c-2gb, vhf-2c-4gb` | — |
| OVH | `vps` | yes (Reverse DNS) | — (deferred, see § Implementation status today) | — |
| Porkbun | `dns, registrar` | n/a | n/a | `https://api.porkbun.com/api/json/v3/pricing/get` |
| Cloudflare | `dns, registrar` | n/a | n/a | n/a |
| Gandi | `dns, registrar` | n/a | n/a | n/a |
| Namecheap | `dns, registrar` | n/a | n/a | — (`getPricing` needs auth) |
| Bundled (generic, BYO base URL — § Bundled provider) | `dns, vps, registrar` | yes (`PUT /v1/servers/{id}/ptr`) | `*` — the intermediary's own catalog, in its order | `{base-url}/v1/pricing/tlds` via the adapter (the static field stays `null`) |

The PTR column reflects `VpsProvider::set_ptr` (a **trait method** — `libs/fauna-provisioning/src/vps/mod.rs`; the YAML schema carries no PTR field), invoked by the wizard after `create_server` succeeds and the public IPv4 is known, so outbound mail from a freshly-provisioned nest doesn't drop into spam folders. Design: `2026-04-27-vps-set-ptr-design.md`.

Hetzner is the only *named* provider with both `dns` and `vps` capabilities — the "Buy VPS with same provider as DNS" checkbox in step 5.3 of the handle-first flow lights up for it and for the generic `bundled` entry, which is the only entry carrying all three capabilities (§ Bundled provider). Hetzner DNS migrated onto the Hetzner **Cloud** API (`api.hetzner.cloud/v1`, Bearer) when the standalone `dns.hetzner.com` DNS API went read-only (2026-05-20), so a single project Read&Write Cloud API token now covers **both** VPS and DNS. The provider therefore exposes **one** `api-token` field with `kinds: [vps, dns]` — shown on both the dns_config and vps_config forms, and mirrored to the VPS flow by `same_provider_for_vps`. (Dispatch still reads a legacy separate `dns-api-token` as a fallback so a config written before the single-token collapse keeps reconciling — additive, no-data-loss evolution.)

The reference flow — Porkbun (registrar + DNS) plus Hetzner (VPS) — runs entirely over providers with `cors_policy: open`, so the web wizard needs **zero** fauna.social proxy hops for that path.

## Provisioning progress — the snapshot model

The orchestrator is snapshot-driven: four user-visible steps — **Domain / Server / Dns / Online** — update a shared `ProvisioningSnapshot` (`progress.rs`) with per-step pre-flight, idempotent retry, and soft cancel (`CancelFlag`); every app pulls the snapshot through `OnboardingMachine.start_provisioning`. Step 4 (**Online**) polls the box's `/api/v1/health` **at its reach address** — the captured public IP, never the not-yet-propagated domain — until 200, after which the machine claims the box as the step's final substep (ratified 2026-08-29; owner of the reach address, the claim-in-run and the crash-safety slot: `../../behavior/onboarding.md` § 6). DKIM keys are **not** injected via cloud-init: a client-generated key embedded at provision would publish one the deployment never signs with (`dkim=fail` from day one), so the nest mints its own **when the mail domain is added** and the admin publishes the TXT from the `admin-dns` page (owner: `../../behavior/mail-bridge-lifecycle.md` § DKIM provisioning; a `test_cloud_init_embeds_no_dkim_key` regression asserts the payload carries no key, and `fauna_provisioning::dkim` backs only that nest-side mint, via `mint_signing_key`). The former `/api/v1/setup-status` poll was retired in S4c2 for the anonymous WS-RPC `fauna.setup.status` kind. Design ratified 2026-04-27 (tracked internally); e2e: `tests/e2e-unified/tests/test_provisioning_progress.py` (per-step outcomes).

## Health-poll CORS on the newly-provisioned nest

The Online health poll is cross-origin from the web wizard's origin. `fauna-nest` serves a `tower_http::cors::CorsLayer` globally with a live-swappable origin predicate: the app-set `nest_cors_origins` state — written by the Admin-class kind `fauna.admin.set_cors_origins`, read through an `ArcSwap` so a change takes effect without restart (`bins/fauna-nest/src/lib.rs`), and surfaced on `SetupStatusReply.cors_origins` (`discovery_core.rs`). The default allows `https://app.fauna.social`; the `--cors-origin` CLI flag / TOML value is only a boot seed (artifact wiring), never the choice surface. The health and claim endpoints (and the anonymous `GET /api/v1/ws` the setup-status kind rides) are unauthenticated and covered by the layer.

## Modes

| Mode | User state | Steps | Status |
|---|---|---|---|
| `own` | Has a domain | DNS config → VPS config → provision | Available |
| `purchase` | Wants to buy a domain in-wizard | Registrar → (implicit DNS) → VPS config → provision | Available (Porkbun reference) |
| `byo` | Has a server | Show Docker command | Available |
| `managed` | Wants a `*.nest.fauna.social` subdomain | Broker-handled | Placeholder in UI; broker plan to be authored |

### Why `purchase` doesn't show a separate DNS step

All four live registrars — **Porkbun**, **Gandi** (`registrar/gandi.rs`, incl. sandbox support), **Namecheap** (`registrar/namecheap.rs`) and **Cloudflare** (`registrar/cloudflare.rs`) — are also DNS providers with the same credentials (Cloudflare's registrar adapter reuses DNS's `api-token` plus one registrar-only `account-id` field DNS doesn't need). After successful registration, the wizard runs `dns_verify(<registrar>, <same creds>)` to discover the new zone's ID and seeds the DNS keys as if the user had picked that DNS provider manually. From the VPS/provision page's perspective, the flow is indistinguishable from `own`.

A future registrar-only provider (or one with different DNS creds) would need a follow-up DNS step in `purchase` mode — target-state, lands with that provider.

## Registrar trait shape and why

Porkbun's API shaped the final trait:

```rust
pub trait Registrar: Send + Sync {
    async fn verify(&self, client: &reqwest::Client) -> Result<(), ProvisionError>;
    async fn check(&self, client: &reqwest::Client, domain: &str)
        -> Result<DomainAvailability, ProvisionError>;
    async fn register(
        &self, client: &reqwest::Client,
        domain: &str, years: u32, agreed_price_cents: u64,
        contact: Option<&ContactInfo>,
    ) -> Result<RegistrationResult, ProvisionError>;
    fn requires_contact(&self) -> bool { true }
}
```

Key decisions:

- **`agreed_price_cents` in `register()`.** Porkbun's `domain/create` endpoint requires the caller to acknowledge a specific price in the request body, matching a `DomainAvailability.price_first_year_cents` returned from `check()`. This prevents price-change races between "user confirmed price X in UI" and "backend billed price Y." UIs MUST show the price and get user confirmation before calling `register`.
- **`contact: Option<&ContactInfo>` (not required).** Porkbun and Cloudflare don't accept per-domain contact fields — both use the account-level contact configured on the provider's own dashboard and override `requires_contact() -> false`. Gandi and Namecheap accept contact info per registration with the default `requires_contact() -> true` (`registrar_requires_contact: true` in the registry), forcing the UI to collect the form. Namecheap needs the *same* contact under four WHOIS roles (Registrant / Tech / Admin / AuxBilling) and rejects a create that omits any of them, so its adapter submits the wizard's single `ContactInfo` four times; it also requires phone numbers as `+CountryCode.Number` and refuses — with the expected format in the message — a number whose country code would have to be guessed, since a wrong guess attaches an unreachable contact to a real registration.
- **WHOIS privacy is on where the registrar offers it free.** Namecheap's is opt-in per registration (`AddFreeWhoisguard` / `WGEnabled`), so its adapter always asks for it: a hard-coded constant, not a knob — there is no deployment in which publishing the user's home address is the wanted behaviour, and any genuine user choice would have to be app UI (§ *Product invariants*), never a flag.
- **`DomainAvailability.price_first_year_cents: Option<u64>`** (integer cents, not a string). Avoids floating-point price bugs.

## Cross-language surface

The provider registry is exposed to apps through two mechanisms:

1. **Generated static `PROVIDERS` list + `OnboardingMachine` eligibility queries** (the primary path; supersedes the `2026-04-27 provider-registry-uniffi-design`'s original snapshot-method shape, which was never built that way — verified current 2026-08-25). Native and web apps hold the generated provider list directly — Rust `providers_generated::PROVIDERS` (linked in-process by tui and other native crates) and its four per-language mirrors (`Providers.swift`, `Providers.kt`, `Providers.cs`, `providers.ts`) — and filter it client-side by capability (e.g. web's `PROVIDERS.filter((p) => p.capabilities.includes('dns'))`). There is no `OnboardingMachine` method that returns the list itself. Per-provider eligibility (greying out a registrar-less provider when `buy_domain` is on, etc.) is queried per item from `OnboardingMachine::dns_provider_eligible`/`dns_provider_ineligible_reason`, computed once inside the Rust state machine and not duplicated per app — so only the enabled/disabled state is stateful, not the list. (VPS provider rows carry no equivalent eligibility gate today — every VPS provider is always selectable.) All 7 apps drive the machine today (adoption matrix: `../../behavior/onboarding.md`). The Rust `providers_generated.rs` and the Python test reference (`tests/e2e-unified/generated/providers.py`) stay. The dispatch-variant fields are Rust-only on purpose: dispatch happens in shared Rust for every app (web through WASM), so no mirror carries them — `providers.ts` did carry an unconsumed `dnsDispatch` until 2026-08-26.
2. **Direct WASM page-support shims** (web-only, § WASM surface) — registrar/VPS/DNS verification and pricing calls the wizard pages make outside the machine. Provisioning itself is NOT on this surface anymore.

## WASM surface

Provisioning runs through **`OnboardingMachine.start_provisioning`** (snapshot-pull progress, idempotent retry, soft cancel — § Provisioning progress). The standalone provisioning shims (`provision_nest`, `provision_nest_no_dns`, `provision_with_registration`, `fetch_dkim`, `provision_build_cloud_init`) were retired by the provisioning-progress design, and `probe_domain_status` was removed at S4c2 with the HTTP setup-status probe.

Seven page-support `#[wasm_bindgen]` shims survive in `libs/fauna-wasm-onboarding/src/lib.rs`:

1. `verify_vps_provider(provider_json)` → location discovery for the VPS config page. `provider_json` is a single combined blob (`{"provider": "<id>", ...credential fields}`).
2. `verify_dns_provider(provider_json)` → zone discovery for the DNS config page. Same single-combined-blob shape as `verify_vps_provider`, unlike the other five shims' separate `provider_id`/`creds_json` params.
3. `registrar_check(provider_id, creds_json, domain)` → `DomainAvailability`.
4. `registrar_register(provider_id, creds_json, domain, years, agreed_price_cents, contact_json)` → `RegistrationResult`. `contact_json` is `"null"` for account-level-contact registrars.
5. `registrar_availability(provider_id, creds_json, domain)` → `RegistrarAvailability` (the lighter availability probe).
6. `registrar_list_tld_pricing(provider_id, creds_json)` → `Option<Vec<TldPriceQuote>>`. Backs the "buy domain for $X" hint.
7. `vps_list_server_types(provider_id, creds_json, curated_ids_json)` → `Vec<ServerTypeInfo>`. Backs the VPS radio options.

## Adding a new provider

1. Implement the trait(s) in `libs/fauna-provisioning/src/<kind>/<provider>.rs`.
2. Add `pub fn new(...)` constructors matching the dispatch layer's expected credential field ids.
3. Add an entry to `i18n/providers.yaml` — fields, capabilities, `cors_policy`, optional `post_verify_select`, optional `registrar_*` flags.
4. Declare the dispatch variant per capability in the same YAML entry — `dispatch` plus `dns_dispatch` / `registrar_dispatch` as the capability set requires (§ Dispatch uses enum wrappers). The generator refuses an entry whose declared capabilities do not each resolve to a variant.
5. Add the provider variant to the exhaustive matches in `libs/fauna-provisioning/src/dispatch.rs`, and its arm to the matching `variant_name()`.
6. Add i18n strings under `provisioning.<provider>.*` to `i18n/strings/en.yaml`.
7. Run `just i18n-generate` then `just providers-generate`.
8. Run `cargo test -p fauna-provisioning` — `dispatch_registry_bijection` is the test that fails if step 2's credential field ids and step 3's YAML `fields` disagree, or if step 4 and step 5 name different variants.
9. Verify the e2e parity test (`tests/e2e-unified/tests/test_provider_registry_parity.py`) extends automatically and passes for every `--client`.

No per-app UI changes are required unless the new provider introduces a field type that the generic renderer doesn't already handle. The `text` / `secret` / `select` field types cover the nine named providers; the fourth, `hosted-auth` (§ Bundled provider), is the one that cost per-app glue — landed tui-first, with a `secret`-input fallback on every app that has not built it yet.

## Managed subdomain (future)

`mode_select = managed` is wire-compatible but disabled today. A future broker service at `nest-broker.fauna.social` will accept an identity and a desired subdomain, pre-provision a nest, and return its URL + claim code. This is the one planned `fauna.social` single-point-of-failure for users who want the fastest possible onboarding. Users who want independence stay on `own`, `purchase`, or `byo`.

Broker design + implementation: a managed-subdomain-broker plan, to be authored separately (no file exists yet).

## Bundled provider — one intermediary as registrar + DNS + VPS

**Owner of the concept and the wire contract: [bundled-provider-api.md](bundled-provider-api.md)** (ratified 2026-08-26). This section owns only the registry-side shape:

- **The generic `bundled` entry** — `capabilities: [registrar, dns, vps]`, `cors_policy: open` (a spec requirement, not an audit observation), `registrar_requires_contact: true` (the registrant is the user), `curated_offers: ["*"]`, `tld_pricing_endpoint: null`, and two fields: `base-url` (`text` — the address a conformant company gave the user) and `api-token` (`hosted-auth`). One entry, one adapter (`libs/fauna-provisioning/src/{registrar,dns,vps}/bundled.rs`, dispatch variants `Bundled*`): a company that implements the spec needs no Rust change. A named intermediary, should one ever be admitted (spec § Neutrality), is a second YAML entry on the same dispatch variants with a fixed base URL — still zero Rust.
- **`hosted-auth` — the fourth field type** (`field_types` in `providers.yaml`): the value is a Bearer token obtained through the spec's device-authorization flow rather than typed. The machine owns the flow (`OnboardingMachine::hosted_auth_begin` / `hosted_auth_wait` / `hosted_auth_state`); an app renders the field as a button carrying the field's derived id (rendering rule: `../../behavior/onboarding.md` § 4) or, until its glue lands, as a `secret` input. This is the one per-app cost the concept has (§ Adding a new provider); any future provider offering hosted sign-in may declare the same type.
- **`curated_offers: ["*"]`** — the wildcard means *the provider curates server-side*: its `GET /v1/server_types` is already the ordered offer list, so the adapter ignores `curated_ids` and shows the first five. Only the bundled adapter honours it; the app-side "offered for VPS purchase iff `curated_offers` is non-empty" filter is unchanged.
- **`signup_url`** for the generic entry points at the published spec — the entry's "website" *is* the open API, and the hosted-auth button is the real "go there" affordance. `signup_url` stays a required key (codegen rejects an entry without one); making it optional would touch every app's link rendering for the sake of one entry.
- **Not the managed broker.** `managed` mode's `disabled_reason_key` and the `nest-broker` name are never reused here (§ Managed subdomain): that is a `*.nest.fauna.social` subdomain broker run by the association; this is a paid own-domain service from an independent company.

## Implementation status today

The registry, codegen, dispatch, snapshot orchestrator, and machine-driven wizard are **built and consumed by all 7 apps** (per-app adoption matrix: `../../behavior/onboarding.md` — do not restate it here). Gaps, verified 2026-08-25:

- **The Online step's reach-address poll + in-run claim (§ Provisioning progress, ratified 2026-08-29) are largely BUILT** (corrected 2026-09-19; this bullet had kept the 2026-08-29 "NOT BUILT" wording after `../installers/vps.md` § Implementation status today was corrected on 2026-09-09): the standard path claims the box as `Online`'s final substep (`libs/fauna-onboarding-machine/src/machine.rs`, `claim_completed`) and `continue_from_provisioning` refuses an unclaimed box. What is still open — the per-app reach-hint legs and the live re-measure of the web Online poll — is owned, with the measured detail and the capture rows, by `../../behavior/onboarding-provisioning.md` § Implementation status today.
- **OVH curated offers + project → region flow.** OVH's catalog API is project- and region-scoped with per-region currency, which doesn't fit the unified `ServerTypeInfo` shape — OVH returns `Ok(vec![])` from `list_server_types`, hiding it from the VPS picker; and OVH's `verify()` returns cloud projects, not regions, so full support needs a second `post_verify_select` stage in the providers.yaml schema. Deferred.
- **Managed broker.** `mode_select = managed` is wire-compatible but disabled (`disabled: true` in the registry); the broker service does not exist.
- **CORS-proxy deployment.** `services/fauna-cors-proxy/` exists in-repo with a container image recipe (`services/fauna-cors-proxy/Dockerfile`), but **`proxy.fauna.social` is NOT live — verified 2026-07-13, the name does not resolve**. Until it deploys, the web app's 4 proxy-routed providers (§ Current CORS policies) and the browser ACME DNS-01 order path are degraded in production (native apps unaffected — they call providers directly). Deployment is user-gated (host placement + the `fauna.social` DNS record); the placement itself is ratified 2026-08-21 — `proxy.fauna.social` is a loopback unit behind the fauna.social front door (`architecture/front-door.md`), going live at the door's cutover; tracked internally.
- **Namecheap `ClientIp` — resolved 2026-07-22** (§ Proxy boundary → *Namecheap's IP allowlist*). The adapter seeds the parameter, checks Namecheap's API-level error envelope, retries once with the address Namecheap echoes, and otherwise surfaces an error naming the address to allowlist. Pinned by unit tests in `libs/fauna-provisioning/src/dns/namecheap.rs` and wiremock conformance tests in `libs/fauna-provisioning/tests/dns_conformance.rs`. The user must still allowlist that address at Namecheap by hand — an irreducible manual step on the provider's side. **Not yet exercised against the live API with real credentials**, since that needs a Namecheap account; the error shapes it relies on were verified against the live endpoint unauthenticated.
- **Namecheap registrar — built 2026-07-22.** `registrar/namecheap.rs` implements `verify` (`users.getBalances` — proves the credentials *and* that the account can be billed), `check` / `availability` (`domains.check` plus a `users.getPricing` quote), and `register` (`domains.create` with the four WHOIS roles). Availability distinguishes `TldNotSupported` from `Unavailable` **structurally** — Namecheap quoting no 1-year REGISTER price for a TLD is taken as "does not sell it", because Namecheap has no verified unsupported-TLD error code to key on. Premium names quote their inline `PremiumRegistrationPrice`, not the standard table, and every quote includes the ICANN fee so it matches the charge. `list_tld_pricing` stays `Ok(None)`: `getPricing` requires authentication, and that method exists to quote *before* credentials are entered. Pinned by `libs/fauna-provisioning/tests/namecheap_registrar_conformance.rs` (19 tests), whose error-path cases matter most — an HTTP-200 `Status="ERROR"` on `domains.create` read as success would tell a user they own a domain they do not. **Not yet exercised against the live API with real credentials** (needs a funded Namecheap account), the same caveat as the DNS half above.
- **Cloudflare registrar — built 2026-07-23, against a beta API (risk accepted, not resolved).** `registrar/cloudflare.rs` implements `verify` (an inert `domain-check` probe on `cloudflare.com`, since the beta API exposes no dedicated account/credential-check endpoint), `check` / `availability` (`POST .../accounts/{account_id}/registrar/domain-check`), and `register` (`POST .../accounts/{account_id}/registrar/registrations`, account-level contact only). Availability distinguishes `TldNotSupported` from `Unavailable` **structurally and more directly than Namecheap/Gandi's inference**: the API itself returns `reason: "extension_not_supported_via_api"` on an unregistrable domain when the TLD is outside the beta's supported subset, vs. `"domain_unavailable"` for an ordinary already-taken name — no static TLD list needed. Adds one registrar-only `account-id` field (`kinds: [registrar]`) alongside the DNS-shared `api-token`. **Explicitly out of scope, per the beta's own stated gaps (confirmed 2026-07-23):** renewals, transfers, and contact updates are not available through this API at all, and premium-domain fee acknowledgement is unhandled (the `tier` field the API returns for that case wasn't observed in any documented example). Pinned by `libs/fauna-provisioning/tests/cloudflare_registrar_conformance.rs` (17 tests). **Not yet exercised against the live API with real credentials** (needs a Cloudflare account with Registrar API access), the same caveat as Namecheap's above — and unlike Namecheap's, the *documented* response shapes here are themselves unverified against a live call, since the beta API predates this session's ability to test against it.
- **Owner-name contract — implemented uniformly 2026-08-01** (§ The owner-name contract). No gap: all five DNS adapters now consume the caller-supplied owner name verbatim. Gandi, Namecheap and Porkbun previously carried a defensive `.<zone>` suffix strip on their read halves (`find_records`/`delete_record`), unreachable on every production path — both callers pre-relativize — but contradicting the contract by implying an FQDN was acceptable, and corrupting any legitimate relative owner ending in the zone name. Removed, with a per-adapter regression test each. Porkbun's apex-sentinel translation (`@` → blank) is the one surviving per-adapter transform and is now shared by both its halves. The write halves already complied.
- **Bundled provider — ratified AND built 2026-08-26 (§ Bundled provider; spec `bundled-provider-api.md`).** `providers.yaml` carries the `hosted-auth` field type (codegen maps it per language: Rust `HostedAuth`, Swift `.hostedAuth = "hosted-auth"`, Kotlin `HOSTED_AUTH`, C# `HostedAuth`, TS `'hosted-auth'`) and the generic `bundled` entry; `libs/fauna-provisioning/src/bundled_api.rs` is the shared transport (base-URL normalization, Bearer, the spec error envelope, the RFC 8628 `device_authorize`/`device_token` calls) under `{registrar,dns,vps}/bundled.rs` on all three traits, dispatched as `Bundled*` off the `base-url` + `api-token` credential pair (`dispatch::bundled_creds`; no canonical host, so a blank `base-url` is a missing credential); `tests/bundled_conformance.rs` is the compliance definition (wire pins + the env-gated live runner). `RegistrarAvailability::Buyable` gained `renewal_cents` for the BoM renewal line (every other adapter reports `None`). The machine owns the hosted sign-in (`OnboardingMachine::hosted_auth_{begin,wait,state,can_begin}` over `CredentialForm`, UniFFI + WASM); **tui** renders the field's button (`wizard::hosted_auth_button`) and **linux**/**web**/**android** now mirror it (`generic_provider_form.rs`'s `HostedAuth` arm; `routes/onboarding/+page.svelte`'s two field loops over the WASM `hostedAuth*` wrappers in `machine.svelte.ts`; the shared `CredentialsForm` composable's `HOSTED_AUTH` arm in `DnsConfigScreen.kt`, android's leg compile/Robolectric-verified only since android e2e is emulator-gated on the emulator host), **macOS**/**iOS** landed 2026-08-27 (one shared FaunaKit lift). **windows' button landed 2026-09-03** (`GenericProviderForm.xaml.cs`'s `HostedAuth` arm, collapsed 2026-09-04 to one `OnboardingViewModel.RunHostedAuth` call the view cannot mis-sequence). Both prerequisites this note used to name are now **closed** — windows renders the §6 BoM (e2e-green) and shares the unscoped composed `{dns,vps}-credentials-form-{field.id}` convention — but the journey is still red there. Its 2026-09-04 defect — an awaited machine call begun on the WinUI UI thread failing to issue its request under the journey's UIA load — was **fixed 2026-09-06** in the generated C# FFI async layer (`../apps/native-async-execution.md` § C#: the FFI layer never needs the caller's thread back), and the two windows defects behind it (a real browser launch wedging the fake's socket pool; a guardless TwoWay binding driving the page at ~300 full-page re-evaluations/second) were fixed 2026-09-06/07. **The journey's last blocker turned out not to be windows' at all** (measured 2026-09-10): since `onboarding.md` § 6 *Provisioning = build + claim* (2026-08-29) a standard-path run ends by claiming the box over WS-RPC, and this journey still pointed its nest leg at `fake_cloud`, which serves `/api/v1/health` over HTTP and no WS-RPC at all — so the claim took a 500, `Online` landed `Failed` with *"the nest did not complete the claim"*, and `provisioning-continue-button` (gated on `claim_completed`, not on `Succeeded` alone) never enabled. **No app could pass it**, on the same snapshot byte-for-byte — reproduced on tui, which had been recorded green on 2026-08-26, three days before the claim step existed. It was the one test the 2026-08-29 sweep missed; its three sibling provisioning tests all took the `provision_target_nest` fixture that week. The **tier_3** journey `tests/e2e-unified/tests/test_bundled_provider.py` now points only its nest leg at a real never-claimed nest (VPS/DNS/registrar still reach the address the user types) and buys the domain and the server through the one row against the reference server `tests/e2e-unified/fakes/fake_bundled.py`. No implementer exists; the generic row ships visible (the spec's § Implementation status today records why no provider-level `disabled` flag was added).
- **Per-language mirrors — audited 2026-08-25, not deletable: no gap.** Each of the four generated app mirrors is the live provider-list source for its app (web's onboarding/admin-dns pages import `PROVIDERS` from `providers.ts`; apple's `DnsConfigFields.swift`/`MacVpsConfigView.swift`/`VpsConfigView.swift`/`AdminDnsView.swift` from `Providers.swift`; android's `DnsConfigScreen.kt`/`VpsConfigScreen.kt`/`AdminDnsScreen.kt` from `Providers.kt`; windows's `DnsConfigView.xaml.cs`/`VpsConfigView.xaml.cs`/`AdminDnsPage.xaml.cs` from `Providers.cs` — see § Cross-language surface). `OnboardingMachine` supplies only per-item eligibility, never the list. No further audit needed.

## Related docs

- [bundled-provider-api.md](bundled-provider-api.md) — the open REST contract a bundled provider implements (§ Bundled provider is the registry side of it).
- `../installers/vps.md` — VPS deploy flow + host lifecycle (its provider list defers to this registry).
- `../installers/docker.md` — cloud-init and container runtime.
- (design ratified 2026-03-29; tracked internally) — original architecture decision: no central broker for provider API calls.
- (design ratified 2026-04-24; tracked internally) — implementation plan for this registry.
- (tracked internally) — Phase 0 CORS audit.
- (tracked internally) — historical task trackers (archived).
