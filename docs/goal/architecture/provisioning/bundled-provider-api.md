# Fauna Bundled Provider API v1 — target state

Owns: bundled-provider
Status: ratified
Authority: the open REST contract an independent company implements to appear in the onboarding wizard as **one combined registrar + DNS + VPS provider** (a *bundled provider*): the endpoint set (the union of the three `fauna-provisioning` traits), the hosted sign-up + checkout credential type (`hosted-auth`, an RFC 8628 device-authorization grant), the error envelope, versioning, the CORS requirement, the mandatory exit guarantees, the neutrality rule, the trust model, and the conformance suite as the compliance definition. Defers the registry schema + codegen + the generic `bundled` entry to [registry.md](registry.md) § Bundled provider, the wizard pages to [../../behavior/onboarding.md](../../behavior/onboarding.md) §§ 4–6, credential persistence to [../../behavior/dns-management.md](../../behavior/dns-management.md) § Where the credential lives, and deployment-seed custody to [../nest/box-recovery.md](../nest/box-recovery.md).

Ratified 2026-08-26 (user idea 2026-08-25). Source of truth for the client side: `libs/fauna-provisioning/src/{registrar,dns,vps}/bundled.rs` + `libs/fauna-provisioning/tests/bundled_conformance.rs`; the e2e reference server is `tests/e2e-unified/fakes/fake_bundled.py`.

## Goal

Onboarding today makes the user open an account — and paste an API token — at a VPS provider *and* at a domain registrar. An independent company that takes the user's payment details **once**, registers the domain and buys the server through its own supplier relationships, can collapse that to *sign up, pay, done*. The wizard is already shaped for a provider with all three capabilities (one row on `dns_config`, eligible with both checkboxes on, pre-selected on `vps_config`, BoM + provisioning unchanged); no live provider happens to have all three. This doc is the door such a company walks through.

The architectural property: **one open spec, one adapter.** Any company implementing this API plugs in with **zero Rust changes** — N intermediaries ≠ N adapters. The client speaks the spec against a user-supplied `base-url`; a company that wants a named row in the registry gets one under the same adapter (§ Neutrality).

## What a bundled provider is — and is not

- It is a **paid, own-domain** service from a **third party**: the user's domain is registered in the user's name, the server is a real VPS the user can leave with. Every guarantee in § Exit is mandatory.
- It is **not** the disabled `managed` mode (`registry.md` § Managed subdomain): that is a future `*.nest.fauna.social` subdomain broker at `nest-broker.fauna.social`, run by the association, and the one planned `fauna.social` single point of failure. The two concepts never share a name, a host, or a registry entry.
- It is **not** a blessed partner. The association ships the door, not a company.

## Neutrality — a hard constraint

The association may not charge users, may not enter agreements obliging it to provide commercial consideration to third parties, holds its domains and accounts in stewardship, and admits no privileged actor (the Fauna Social bylaws § 2, § 4, § 5, published at fauna.social/organization/bylaws — the authority; this doc only draws the provisioning-side consequences):

1. **BYO `base-url` is the primitive.** The generic `bundled` registry entry has a `base-url` text field; the user types the address a conformant company gave them. No company is named in the app.
2. **A curated list, if one ever exists, is open to every implementer on equal terms** under a published inclusion policy (the footing Hetzner and Porkbun have today) — a named row is the same `bundled` adapter with a fixed base URL, nothing more. Whether and when to curate anyone is a user decision, moot until an implementer exists.
3. **No referral cut, no revenue share, nothing under `fauna.social`.** The intermediary's API, hosted pages, and support are its own.

## The plug-in shape

Wizard flow, unchanged from `onboarding.md` §§ 4–6 — only the provider row is new:

1. **`dns_config`** — the user turns *buy this domain* and *same provider for the server* on; the bundled row is eligible (all three capabilities; the eligibility rule and its on-screen reason are `OnboardingMachine`'s, never re-derived per app). The credentials form shows two fields: `base-url` (text) and the account token (`hosted-auth`, § Authentication).
2. **Verify** → `GET /v1/me` returns the account's zones and locations; the wizard then asks `GET /v1/domains/check` for the handle's domain and shows the first-year price with the price-confirm acknowledgement, and — since `requires_contact` is `true` — the WHOIS contact form (**registrant = the user**, never the intermediary; `GET /v1/contact` pre-fills it when the account has a default).
3. **`vps_config`** — pre-selected; server types come from `GET /v1/server_types` (the intermediary's own curated catalog, § Endpoints); the mail-mode RAM gate applies to whatever it returns.
4. **`nest_provisioning`** — the BoM recaps the domain's first-year *and renewal* price and the server's monthly price; *Buy and set up* runs the shared orchestrator: Domain (`POST /v1/domains`) → Server (`POST /v1/servers` with the verbatim cloud-init) → Dns (record writes) → Online (health poll), then `PUT /v1/servers/{id}/ptr`.
5. **Afterwards** the token persists client-side exactly like any other provider credential (`dns-management.md` § Where the credential lives): *Fauna controls DNS* and the add-domain wizard work through it; the nest never holds it.

`cors_policy` is `open` **by requirement, not observation** (§ CORS) — the web app calls the intermediary directly, with no `proxy.fauna.social` hop.

## Authentication — `hosted-auth` (RFC 8628 device authorization)

The user creates the account and enters payment details on the **intermediary's hosted pages**; card data never touches Fauna code. The credential the app ends up holding is an ordinary scoped Bearer token, obtained through the **OAuth 2.0 Device Authorization Grant** ([RFC 8628](https://www.rfc-editor.org/rfc/rfc8628)):

| Step | Who | What |
|---|---|---|
| 1 | app → intermediary | `POST {base}/v1/auth/device` (form-encoded, no auth) `client_id=fauna&scope=provisioning` → `200 {"device_code","user_code","verification_uri","verification_uri_complete","expires_in","interval"}` |
| 2 | app | opens `verification_uri_complete` in the system browser through the app's existing open-URL affordance (the same one behind `dns-provider-open-browser-button`) and shows `user_code` on screen as the fallback the user can type at `verification_uri` |
| 3 | user, on the hosted page | signs up (or in), enters payment details, confirms the code |
| 4 | app → intermediary | polls `POST {base}/v1/auth/token` (form-encoded) `grant_type=urn:ietf:params:oauth:grant-type:device_code&device_code=…&client_id=fauna` every `interval` seconds; `400 {"error":"authorization_pending"}` / `"slow_down"` keep polling, `"expired_token"` / `"access_denied"` end the attempt; `200 {"access_token","token_type":"bearer","scope":"provisioning"}` lands the token in the credentials bag under the `hosted-auth` field's id |

`client_id` is the fixed public string `fauna` — a public client with no secret (RFC 8252 § 8.5); the intermediary must accept it without pre-registration. Token lifetime is the intermediary's choice; there is no refresh grant in v1 — a revoked or expired token surfaces as `401` on the next call, which the DNS page renders as *re-enter credentials* (`dns-management.md` § The two modes), and re-entering is the same device flow again.

**Why a device flow and not a redirect callback.** A redirect flow needs a registered `redirect_uri` per client — a loopback listener or a custom URL scheme on each native app (an OS-level integration none of the six GUI apps has today) and a pre-registered origin for web, which a BYO `base-url` cannot register in advance. The device flow needs none of that: the same two HTTP calls and the same *open a URL* affordance on all 7 apps, terminal included, so tui (the lead app) proves the exact mechanism the others inherit — no per-platform divergence (priority #1) and the whole flow lives in shared Rust (priority #2).

**Rendering rule (owner of the field type's UX: `onboarding-provisioning.md` § 4).** A `hosted-auth` field renders as a button that carries the derived form-field id (`dns-credentials-form-{field.id}` — the same id every field has, so no new element is minted) whose label is machine-derived: *Sign in at the provider…* → *Finish in your browser — code {user_code}* → *Connected*. An app that has not yet built the button renders the field as a `secret` input instead — a token from the intermediary's own dashboard pasted there is exactly as valid, so the six trickle-down apps degrade gracefully rather than break.

## Endpoints

All paths are relative to the user's `base-url`. The adapter **refuses a non-`https` base** (`fauna_provisioning::bundled_api::checked_base_url`, called from every entry point — the device-authorization calls, and the dispatch layer that builds the authenticated client) rather than merely normalizing one: this is the widest-scope credential the product holds (registrar + DNS + VPS on one bearer token over one base), so a silently-accepted `http://` would be a domain/DNS/box takeover from one on-path capture. The one carve-out is an explicit loopback host (`127.0.0.1`, `::1`, `localhost`) on `http://`, for a self-hosted development intermediary and the E2E fakes (wiremock, pytest-httpserver) that exercise this same user-typed field — never a config knob, never widened to "any `http://`". Trailing slashes and surrounding whitespace are also normalized. Bodies are `application/json` unless stated; the two auth endpoints are form-encoded per RFC 6749. Every authenticated call carries `Authorization: Bearer <token>`.

| Method + path | Auth | Serves | Request | Response (2xx) |
|---|---|---|---|---|
| `POST /v1/auth/device` | — | `hosted-auth` step 1 | form: `client_id`, `scope` | RFC 8628 § 3.2 device-authorization response |
| `POST /v1/auth/token` | — | `hosted-auth` step 4 | form: `grant_type`, `device_code`, `client_id` | RFC 6749 § 5.1 token response; RFC 8628 § 3.5 errors |
| `GET /v1/me` | Bearer | `Registrar::verify`, `DnsProvider::verify` (zones), `VpsProvider::verify` (locations) | — | `{"api_version":1,"account":{"id"},"zones":[Zone],"locations":[Location]}` |
| `GET /v1/pricing/tlds` | — | `Registrar::list_tld_pricing` | — | `{"currency","tlds":[{"tld","registration_cents","renewal_cents"}]}` |
| `GET /v1/domains/check?name=` | Bearer | `Registrar::check` / `availability` | query `name` | `Availability` |
| `POST /v1/domains` | Bearer | `Registrar::register` | `{"name","years","agreed_price_cents","contact":Contact,"whois_privacy":true}` — `agreed_price_cents` is in the currency the check quoted | `201 {"name","nameservers":[…]}` |
| `GET /v1/contact` | Bearer | `Registrar::fetch_default_contact` | — | `Contact` (`404` = none) |
| `GET /v1/domains/{name}/auth-code` | Bearer | § Exit | — | `{"auth_code"}`, or `202 {"available_after"}` while a registry transfer lock applies |
| `GET /v1/zones/{zone_id}/records?name=&type=` | Bearer | `DnsProvider::find_records` | query filters, both optional | `{"records":[Record]}` |
| `POST /v1/zones/{zone_id}/records` | Bearer | `DnsProvider::create_record` | `Record` without `id` | `201 Record` |
| `DELETE /v1/zones/{zone_id}/records/{record_id}` | Bearer | `DnsProvider::delete_record` (adapter resolves the id by name + type + value) | — | `204`; `404` is success |
| `GET /v1/server_types` | Bearer | `VpsProvider::list_server_types` | — | `{"server_types":[ServerType]}` — **the intermediary's curated catalog, in display order**; the adapter ignores the registry's `curated_offers` and shows the first five |
| `GET /v1/servers?name=` / `?label=k=v` | Bearer | `VpsProvider::find_server_by_name`; the future decommission view (`../installers/vps.md` § Uninstall) | query filters | `{"servers":[Server]}` |
| `POST /v1/servers` | Bearer | `VpsProvider::create_server` | `{"name","location","server_type","user_data","labels":{…}}` — `user_data` is the cloud-init document **verbatim** | `201 Server` |
| `DELETE /v1/servers/{id}` | Bearer | `VpsProvider::delete_server` | — | `204`; `404` is success (idempotent decommission) |
| `GET /v1/servers/{id}/ptr` | Bearer | `VpsProvider::get_ptr` | — | `{"ptr": "mail.example.com" \| null}` |
| `PUT /v1/servers/{id}/ptr` | Bearer | `VpsProvider::set_ptr` | `{"ptr"}` | `204` (overwrites — idempotent) |

### Shapes

| Shape | Fields |
|---|---|
| `Zone` | `id` (opaque string), `name` (the zone's apex, e.g. `example.com`) |
| `Location` | `id`, `name`, `city`, `country` (ISO 3166-1 alpha-2) |
| `ServerType` | `id`, `vcpu` (int), `mem_gb` (number), `disk_gb` (int), `price_monthly_cents` (int), `currency` |
| `Server` | `id`, `name`, `ipv4`, `ipv6` (optional; absent or `null` when the box has none — what scopes the retire view's `AAAA` cleanup, `behavior/nest-retirement.md` § DNS cleanup), `status` (`"provisioning"` \| `"running"` \| `"stopped"`), `labels` (object) |
| `Record` | `id`, `type` (`A`/`AAAA`/`MX`/`TXT`/`SRV`/`TLSA`/…), `name`, `value`, `ttl` (int seconds), `priority` (int, MX/SRV only, else absent) |
| `Availability` | `name`, `status` (`"available"` \| `"unavailable"` \| `"tld_not_supported"`), `currency`, `registration_cents`, `renewal_cents` (both present iff `available`) |
| `Contact` | `first_name`, `last_name`, `email`, `phone` (E.164), `address1`, `city`, `state`, `postal_code`, `country` (ISO 3166-1 alpha-2) — `fauna_provisioning::registrar::ContactInfo` verbatim |

Rules the adapter relies on, pinned by the conformance suite:

- **Owner names are zone-relative** with `@` at the apex (`registry.md` § The owner-name contract — the adapter's `record_names_relative_to_zone()` is `true`, and it passes names verbatim). `find_records` filters are exact matches on `name` and `type`.
- **`agreed_price_cents` is binding.** `POST /v1/domains` must fail with `409 price_changed` (carrying the current quote in `details`) rather than charge a different amount — the same race guard every registrar adapter has (`registry.md` § Registrar trait shape).
- **`years` is 1** in the wizard; the intermediary quotes and bills exactly one year plus the renewal price it disclosed.
- **`whois_privacy` is always requested** where the registry offers it (a hard-coded constant, mirroring the Namecheap rule — there is no deployment in which publishing the user's home address is wanted).
- **Integer cents, never floats.**
- **Labels are a flat string map**; the orchestrator always sends `managed-by: fauna` (`vps/mod.rs::MANAGED_BY_LABEL`), and the `label=` filter must match it.

## Error envelope

Every non-2xx response is `{"error":{"code":"<snake_case>","message":"<human-readable>","details":{…}?}}` with a matching HTTP status. Codes the adapter understands (anything else maps to the status alone):

| HTTP | `code` | Meaning |
|---|---|---|
| 400 | `invalid_request` | malformed body / query |
| 401 | `unauthorized` | missing, expired or revoked token → the app's *re-enter credentials* path |
| 402 | `payment_required` | the account has no usable payment method — the hosted page fixes it |
| 403 | `forbidden` | the token's scope does not cover the call |
| 404 | `not_found` | zone / record / server / contact absent (a success for the two idempotent deletes) |
| 409 | `price_changed` | `POST /v1/domains` with a stale `agreed_price_cents`; `details.registration_cents` carries the current quote |
| 422 | `tld_not_supported` / `domain_unavailable` | registration refused at check or create time |
| 429 | `rate_limited` | honour `Retry-After` |
| 5xx | `unavailable` | transient |

The adapter surfaces these as `ProvisionError::Provider { status, body }`; the orchestrator's existing transient set (5xx, 408, 429) retries with backoff, everything else is terminal for the step (`onboarding.md` § 6).

The two OAuth endpoints use the RFC 6749/8628 error shape (`{"error":"…"}`) instead — implementers can stand up an off-the-shelf authorization server unchanged.

## Versioning

- The path prefix **is** the major version: `/v1/…`. Within v1 evolution is **additive only** — new optional fields, new endpoints, new error codes; a field never changes meaning or type. A breaking change is a new prefix, and the adapter that speaks it is a new file.
- `GET /v1/me` returns `api_version` so the client can name the mismatch when a `/v1` implementation answers with a later shape.
- The Fauna side pins the same rule at the wire: `bundled.rs` deserializes with unknown fields ignored, never denies them.

## CORS

Mandatory, verified by the conformance suite, and the reason the registry entry can say `cors_policy: open` for a provider that does not exist yet: every endpoint (auth included) answers `OPTIONS` with `204` and sends `Access-Control-Allow-Origin: *`, `Access-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS`, `Access-Control-Allow-Headers: Authorization, Content-Type`. Without this, the web app could reach the intermediary only through `proxy.fauna.social`, which would make a `fauna.social` host a dependency of a third party's commercial service — exactly what § Neutrality forbids.

## Exit — mandatory guarantees

The intermediary owns the supplier accounts (console, reinstall, snapshots) and sees what a VPS provider sees today. That is acceptable **only** because leaving is designed in, and the spec makes each door mandatory:

1. **The domain is the user's.** The registrant contact posted at `POST /v1/domains` is registered as the domain's registrant (reseller model — the accredited registrar stays responsible, ICANN 2013 RAA § 3.12), and `GET /v1/domains/{name}/auth-code` hands out the transfer authorization code on demand (a registry-imposed 60-day lock is reported as `202 available_after`, never as a refusal).
2. **The server is enumerable and deletable** by the user's own token — `GET /v1/servers?label=managed-by=fauna` and the idempotent `DELETE /v1/servers/{id}` — so the future *list / decommission my nests* view (`../installers/vps.md` § Uninstall) works here like on any provider.
3. **Re-provision anywhere with the same identity.** The deployment seed is client-custodied (`../nest/box-recovery.md` § Mechanism); a user who leaves points DNS at a new box provisioned by any other provider, restores data per `../../behavior/backup-restore.md`, and transfers the domain with the auth code.
4. **No proprietary lock**: no extra required client-side state, no intermediary-specific identifiers the client must keep beyond the token, and nothing in the cloud-init the intermediary may alter (`user_data` is verbatim).

## Trust model

- **What the intermediary sees.** The cloud-init document — claim code and deployment seed included — exactly as any VPS provider does today; `box-recovery.md` already treats a cloud provider's disk as untrusted, so no *new* exposure arises from a third party in that seat. It additionally holds the supplier accounts, which is why § Exit is mandatory rather than advisory.
- **What it never sees.** The user's identity secret, any nest content (sealed at rest), the DNS/VPS token of any *other* provider the user holds, and payment data in transit through Fauna (the hosted page collects it).
- **What the nest never sees.** The bundled token — client-held in the account-state plane kind `fauna.state.dns` ([`../config-dissolution.md`](../config-dissolution.md) § The `__config` dissolution schedule → *The kinds*), nest-opaque (`dns-management.md` § Where the credential lives). The nest is out of the DNS-write path for this provider as for every other.

## Notes for implementers (not legal advice)

The intermediary's own counsel owns everything below. What the wizard itself already does for every registrar — bundled or not — is owned by `onboarding.md` § 4 (the price-confirm acknowledgement) and § 6 (the renewal line on the BoM):

- **Domains.** ccTLD eligibility rules map to `tld_not_supported`.
- **Servers.** Partner / reseller terms with the supplier; the intermediary must be able to act on abuse notices (suspend), like any provider today.

## Conformance suite — the compliance definition

`libs/fauna-provisioning/tests/bundled_conformance.rs` **is** what "implements the Fauna Bundled Provider API v1" means; prose here never outranks it.

- **Wire pins (always run, wiremock):** for every endpoint, the exact request the adapter sends (path, method, headers, body) and the response it accepts — including the `409 price_changed` refusal, `404`-as-success on both deletes, the RFC 8628 pending/slow-down/denied arms, the `202 available_after` auth-code arm, and the CORS preflight the web build depends on.
- **Live runner (env-gated, self-skipping like `vps_set_ptr_live.rs`):** `FAUNA_BUNDLED_BASE_URL` + `FAUNA_BUNDLED_TOKEN` point the same suite's read-only half at a real implementation (`/v1/me`, `/v1/pricing/tlds`, `/v1/domains/check`, `/v1/server_types`); `FAUNA_BUNDLED_LIVE_MUTATE=1` adds a create-and-delete round trip for a record and a server. This is how an implementer proves conformance before asking for a curated row.
- **Reference server:** `tests/e2e-unified/fakes/fake_bundled.py` implements the whole spec (auto-approving device flow included) for the tier_2 onboarding journey, so the wizard is exercised end to end against the contract, not against a hand-picked subset.

## Implementation status today

Ratified 2026-08-26; nothing below the spec existed before that date. Status per deliverable:

- **Spec** — this doc (ratified).
- **Registry + codegen, shared Rust, conformance suite, the machine's hosted sign-in — BUILT 2026-08-26** (`registry.md` § Implementation status today owns the code-side detail). The reference server `tests/e2e-unified/fakes/fake_bundled.py` implements this whole spec, and the tier_2 journey `tests/e2e-unified/tests/test_bundled_provider.py` completes onboarding through the one row against it (tui, linux, web, android).
- **`hosted-auth` glue** — tui, linux, web, android, macOS and iOS render the button; windows' button landed 2026-09-03 but isn't yet provable through the shared journey test (two separate, tracked prerequisites — no §6 BoM UI, and an id-convention divergence) (§ Authentication, rendering rule; per-app detail and the open row are `registry.md` § Implementation status today's).
- **Curated list / inclusion policy** — none; BYO `base-url` only. A user decision once an implementer exists (§ Neutrality).
- **No implementer exists.** The `bundled` row is visible and functional for a user who holds a conformant base URL; a provider-level `disabled` flag was considered and not added — the row is the invitation, and it costs nothing to a user who ignores it.

## Related docs

- [registry.md](registry.md) — the registry schema, codegen, dispatch, the generic `bundled` entry.
- [../../behavior/onboarding.md](../../behavior/onboarding.md) §§ 4–6 — the wizard pages the flow rides.
- [../../behavior/dns-management.md](../../behavior/dns-management.md) — where the token lives afterwards.
- [../nest/box-recovery.md](../nest/box-recovery.md), [../../behavior/backup-restore.md](../../behavior/backup-restore.md), [../installers/vps.md](../installers/vps.md) § Uninstall — the exit hatch this spec makes mandatory.
- [../front-door.md](../front-door.md) — why nothing here lives under `fauna.social`.
