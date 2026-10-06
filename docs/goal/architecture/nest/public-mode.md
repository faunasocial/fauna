# Nest: Public — target state

Owns: public-mode, registration
Status: ratified
Authority: the public (internet-facing) deployment mode — registration modes + the registration ceremony, handles (format, resolution, change delay/cooldown), tiers, first-admin bootstrap summary, the federation HTTP endpoint surface (ActivityPub/WebFinger residue, Nostr native-content routes, Bluesky OAuth metadata), the nest-level pairing policy, and the public-mode scope lines for bridge management and content delivery; defers the pairing/sync mechanism to `private-mode.md`, bridge architecture to `../apps/bridges.md`, the WS-RPC kind catalog to `../api-layers.md`, MTA-STS to `../../behavior/mail-multidomain.md`, CalDAV/CardDAV/WebDAV to `../../behavior/caldav-server.md` + siblings, DNS to `../../behavior/dns-management.md`, subscriptions to `../../behavior/monetization.md`, and the domainless claim/domain lifecycle to `domains-and-tls-bootstrap.md`.

## Implementation status today

- **Registration modes are app-set nest state** (the recorded CLI-flag violation is **CLOSED**, 2026-07-12). The posture is the `RegistrationMode` enum (`open` / `invite_required` / `closed`) plus the orthogonal `max_free_users` ceiling, held in the app-set `nest_registration_mode` DB singleton, flipped by the admin via `fauna.admin.set_registration_mode` (Admin-class) and boot-resolved into the live `AppState.registration_mode`; `[nest] registration_mode` is only the **pre-claim seed** (absent ⇒ `closed` — a nest that boots before its admin has picked a posture admits nobody). Read back by the admin client on `fauna.setup.status`. The `--registration-open` / `--registration-invite-required` / `--max-free-users` flags are **deleted**. Same shape as the subhandles flag (§ Handle resolution).
- **Auto-registration is removed** (2026-07-12). An actor with no `users` row is refused `fauna.auth.not_registered` in **every** mode: a valid self-signed token proves key possession, never admission. The old `fauna.auth.handshake` branch minted a handle-less `free` account on first contact, bypassing the invite gate, the free-tier cap and `closed` alike — an account § User Registration says cannot exist (registering *is* choosing a handle). Accounts now come into being through exactly three paths: the registration ceremony, the admin claim (`fauna.auth.claim_admin`, which creates the admin's row itself — a fresh box stays claimable), or an admin admitting a user (directly via `fauna.admin.users.create`, or by approving an invite *request*). **All three carry a handle** — see the § *A handle-less account* bullet below for the one shape that does not, and what it costs. `fauna.admin.set_require_registration` was retired in place 2026-07-12 and left the wire 2026-09-24 (the compat-remnant sweep, `version-compatibility.md` § Dim 2's fourth exception — it never had a caller), and the always-`true` `SetupStatusReply.require_registration` field followed it off the wire in the sweep's wire-payload program (2026-09-24), together with `nest.info`'s `registration.{open,invite_required}` boolean projection of the mode — the posture a client reads is `SetupStatusReply.registration_mode`.
- **Direct admission has app UI (2026-08-15, tui first).** The third account-creation path — an admin admitting a known actor id via `fauna.admin.users.create` — was RPC-only from its 2026-07-31 landing until the `admin-users` **Admit section** shipped (user-approved ids `admin-users-admit-{section,actor-input,handle-input,tier-select,button}`): the admin types the actor id, names the handle the actor is admitted under (the admit moment is where a handle is named — admins can only *clear* one later, and a registered actor cannot reach `fauna.account.register` to claim one themselves), and picks a tier. A deliberately blank handle admits the handle-less state below. The shared writer is `fauna_client_admin::AdminClient::users_create`; **tui renders the section** (the lead app; proven end-to-end by `tests/e2e-unified/tests/test_admin_users_admit.py` — the admitted actor authenticates and their profile indexes under the typed handle), **linux renders it too** (2026-08-19, second app; same test green `--app linux`), and **web renders it too** (2026-08-19, third app; the wasm face `libs/fauna-wasm`'s `adminUsersCreate`, same test green `--app web`), **android renders it too** (2026-08-21, fourth app; the UniFFI face `FfiAdminClient::users_create` — `libs/fauna-ffi/src/admin.rs`; `AdminUsersVM.admitUser` does the client-side 64-hex actor validation, mirroring tui/linux, and its e2e leg stays host-emulator-gated like every android tier_3 journey), **macOS + iOS render it too** (2026-08-25, fifth + sixth apps; shared FaunaKit `AdminUsersHubView.admitSection` + `AdminVM.admitUser`, reusing the same `FfiAdminClient::users_create` surface; same test green `--app macos`, iOS build-verified via the shared view), and **windows renders it too** (2026-08-26, seventh and last app; `AdminUsersViewModel.AdmitUserAsync` + `AdminUsersPage`'s Admit section, reusing the already-generated `FfiAdminClient.UsersCreate` binding; same test green `--app windows`) — **all 7 apps now render this section**, and the action layer no longer declares any `skip_unbuilt` gate for it. **This path carries the supervised designation too (nest half 2026-08-30):** additive `guardian_actor` + `age_band` on `AdminUserCreateRequest`, validated as the approve path validates its own and refused on a blank-handle admit — mechanism owner [`../../behavior/family-safety.md`](../../behavior/family-safety.md) § The guardianship link; the Admit-section guardian picker is a pending app leg behind net-new-ID approval (IDs proposed and asked 2026-10-01 — [`../../behavior/admin.md`](../../behavior/admin.md) § 2. Users).
- **A handle-less account is representable, and it cannot send email (2026-07-31).** Two paths reach the shape: `fauna.admin.users.create` **without** a `handle` (the admit form's deliberately blank handle — `../../behavior/admin.md` § 2 Users → Section 3; a product choice, not an older-client arm, which is why the compat-remnant sweep kept it), and `fauna.admin.users.clear_handle`, which strips a handle after the fact (the moderation affordance). Such an actor is fully registered — it authenticates, and `caller_class_for_actor` answers `User` — but owns no address on any of the nest's domains, so the From-handle verification on `fauna.email.send` (`behavior/mail-app-surface.md` § First-party client send) refuses every send from a deployment-domain `From:`. An off-domain `From:` is unaffected; the deployment is not authoritative for it. **The at-rest shape is the empty string, not NULL** — `create_user` writes `''`, so `db::get_handle` answers `Some("")` for a handle-less *registered* actor and reserves `None` for "no `users` row at all"; the send gate folds the empty case into its no-handle arm, and code reading `get_handle` must do the same or it will treat a handle-less actor as one whose handle merely fails to match. **Apps must not paper over it**: an app that cannot resolve the user's own `<handle>@<domain>` refuses the send locally with `error.email.no_handle` rather than substituting a session handle the nest does not back, or an empty `From:` (`ui/conversations.md` § Errors & edge cases).
- **The admin sets the posture from their app — all 7 apps render it (windows 2026-08-26, last).** The shared writer is `fauna_client_admin::AdminClient::set_registration_mode(mode, max_free_users)` — one call carrying both values — exposed at both client boundaries: `libs/fauna-ffi` (`FfiAdminClient::set_registration_mode`, taking the `FfiRegistrationMode` enum) and `libs/fauna-wasm` (`adminSetRegistrationMode`, validating the wire string at the boundary). **`apps/fauna-web`, `apps/fauna-tui`, `apps/fauna-linux`, `apps/fauna-android`, `apps/fauna-windows`, and the shared FaunaKit `AdminUsersHubView` (macos + ios, 2026-08-25) render the `admin-users` registration section** (the `admin-users-registration-*` IDs, tui native via `fauna-client-admin`; macos/ios via `AdminVM.loadRegistration`/`saveRegistration` + `registrationModeOptions()`; windows via `AdminUsersViewModel.LoadRegistrationAsync`/`SaveRegistrationAsync` + `FaunaFfiMethods.RegistrationModeOptions()`). The mode + ceiling are read back on `fauna.setup.status` (`SetupStatusReply.{registration_mode,max_free_users}`, mirrored onto `FfiSetupStatus`). Proven end-to-end through the UI by `tests/e2e-unified/tests/test_admin_registration_posture.py` (tier_3, all 7 apps): the admin flips the posture from the page and the next stranger's `fauna.account.register` is refused, live — no restart, no config file.
  - **The posture read-back is the raw wire string on purpose, at every boundary.** A client that cannot name the reported mode — `None` from a nest predating the field, or a *newer* nest's mode it predates — must render the section read-only rather than coerce the value to a known variant: saving that guess would overwrite the nest's real posture (`version-compatibility.md` — a client may be older than the nest). `registration_mode_from_wire` (FFI) / `asRegistrationMode` (web) return "unnamed" for both cases; neither is `closed`.
- **§ Registration Modes → *Age at registration* is BUILT nest-side (2026-08-24; band model owner: `behavior/family-safety.md` § The account age band — its implementation-status age-band row carries the full inventory).** The scope lines this doc owns, as landed: an `open`-mode self-registration mints **no band row** — the by-construction `18+`/`none` is represented by absence; the store-says-minor refusal is `fauna.account.guardian_admission_required` on every path that would create an *unsupervised* account (open registration, unsupervised code redemption — the code-redemption arm checks via a **non-consuming peek** so the refusal never burns a code use; a guardian-designated code stays the intended path for a minor claim); the require-knob is the app-set `nest_age_verification_required` DB singleton (default off, deliberately **no `[nest]` pre-claim seed** — there is no pre-claim moment where it matters), flipped via `fauna.admin.set_age_verification_required`, read back on `fauna.setup.status`, and it refuses every self-service `fauna.account.register` (codes included) carrying no **verified** attested claim with `fauna.account.age_verification_required` — a declared-only claim never satisfies it, and admin request-approval stays ungated with the claim recorded absence-as-signal on the request row. App surfaces (admin toggle beside the mode select, onboarding store-signal step) remain pending.
- The WS-RPC-everywhere cutover is complete for this doc's surfaces: registration (`fauna.account.register`), admin panel (`fauna.admin.*`), bridge management (`fauna.bridges.*`), feeds/search/inbox kinds. The in-nest CalDAV control plane is retired to a discovery redirect (§ CalDAV). The Nostr control plane rides the unified `fauna.bridges.*` kinds; only three native-content routes remain HTTP (§ Nostr).
- **Bridge management UX (2026-05-04 design):** the per-app bridge UI is migrating to a metadata-driven shared shape (a single bridges list + per-bridge detail page). **Nostr is the exception — it keeps its own dedicated page (like mail), NOT folded into the bridges list** (ratified 2026-06-13; see [`nostr.md`](../../ui/nostr.md) § Page structure + [`bridges.md`](../../behavior/bridges.md) § Scope). Nostr's control-plane rides the same `fauna.bridges.*` kinds — only its *page* is dedicated.

## Goal

The standard internet-facing nest: a public domain, real users registering and managing accounts, federation endpoints consumed by remote servers, bridges reaching out to other protocols (Bluesky, Nostr, Email), the admin surface, and the active side of pairing relationships with private nests. This is the deployment mode that other Fauna nests, fediverse peers, MUAs, and CalDAV clients see; private nests pair with one of these, workers replicate on behalf of one of these, but only a public nest holds the public identity of the deployment.

---

## What Makes a Nest "Public"

A public nest is internet-facing with a domain name. It is the standard deployment mode and is the only mode that:

- Handles user registration and account management
- Serves all client-facing API layers (owner: [`../api-layers.md`](../api-layers.md))
- Hosts federation endpoints consumed by external servers
- Manages bridge processes (Bluesky, Nostr, Email)
- Holds the user-authorized pairings that private nests sync through (subject to the admin's nest-level pairing policy)

A private nest pairs with a public nest; it does not handle registration or federation directly. A worker process supplements a public nest's storage capacity but is not itself a nest.

---

## Registration & Identity

### Registration Modes

Nest admins choose one of three registration modes, from their app (mechanism: § Implementation status today):

| Mode | Effect |
|---------|--------|
| `open` | Anyone can register |
| `invite_required` | Registration requires a valid invite code |
| `closed` | Registration is closed; no new accounts |

An optional `max_free_users` cap applies a ceiling to free-tier accounts regardless of mode.

The mode is a single enum, not a pair of booleans: `open` means "the register endpoint is enabled at all", so invite-only is a *mode*, not `open` + a second flag. That makes the incoherent fourth state (closed **and** invite-required) unrepresentable. `nest.info` still advertises the posture as the two booleans `open` / `invite_required` for older clients (`version-compatibility.md` § I4); the nest projects them from the enum.

**The mode gates registration, never authentication.** Closing registration must not lock out the users who already exist, and opening it must not admit anyone who has not registered: an actor with no account is refused in *every* mode, including `open` (§ Implementation status today → auto-registration removal). The only way into a nest is the ceremony below, the admin claim, or an admin admitting you.

**Age at registration (ratified 2026-08-22 — nest-side BUILT 2026-08-24, app surfaces pending; § Implementation status today).** Registration composes with the account age band ([`../../behavior/family-safety.md`](../../behavior/family-safety.md) § The account age band owns the band, its provenance values, and the attested platform claim); this doc owns only the registration-mode interplay:

- A self-registration under `open` mints an **`18+` band with provenance `none`** — no guardian is in the path, and supervision cannot be established here (it is set only at invite/request admission, family-safety.md § The guardianship link).
- A registering app carrying a store age signal that says **minor** is refused self-registration with a typed error pointing at guardian-mediated admission (invite code / request-approval). The signal corroborates; it is not an oracle — and the refusal binds only the self-service path, where no admitting human sees the applicant.
- The admin knob **"accept only signups carrying app age verification"** (default **off**; app UI + nest state, rendered in the registration section beside the mode select — [`../../behavior/admin.md`](../../behavior/admin.md) § 2 Users) refuses **self-service** admissions (open registration and invite-code redemption) that carry no attested age claim — an attestation the nest cannot check counts as none (family-safety.md § The account age band → *An attestation the nest cannot check*); admin-driven request-approval stays the admin's own judgment, with the claim's absence visible on the request row (absence-as-signal). Default off keeps works-out-of-the-box — web/desktop signups have no attestation path, and the platform age APIs are declinable outside mandated regions, so enabling the knob deliberately excludes capable-but-declining users too.

### User Registration

**Kind:** `fauna.account.register` (pre-identity WS-RPC; the `POST /api/v1/register` HTTP twin was retired in S4f).

Users register by submitting an Ed25519 public key (the `actor_id`) and a chosen handle. The client generates the keypair locally; the server never receives the secret key. The registration request includes a signature over the domain-tagged, length-prefixed `register_signed_message(actor_id, handle, domain, timestamp_be)` (within ±30 seconds of server time) to prove key possession; any of the nest's mail domains is accepted as the `domain` candidate.

On success the server, in this order (`account_core::register_core`):

1. Checks handle format (3–63 characters, lowercase alphanumeric + hyphens, no leading/trailing hyphens), then rejects a reserved handle. The reserved list is the hard-coded shared constant `fauna_protocol::handle::RESERVED_HANDLES` — a correctness constant nobody configures (no flag, no config key; the nest's old `--reserved-handle` flag was deleted 2026-07-17), kept a superset of the reserved web subdomain labels so no handle can derive a reserved web host, and enforced identically by `fauna-router`'s pre-flights
2. Parses the `actor_id` and validates the timestamp (within ±30 seconds of server time), then validates the signature
3. Checks the actor isn't already registered, then that the handle isn't already taken or in 72-hour cooldown for a different actor
4. Resolves the account's tier: a supplied invite code is checked against the invite-code table; on a miss it falls through to the payment-claim store instead — a paid membership claim code doubles as an invite code (`account_core::resolve_membership_claim`, redeem-first so a double-spend can't create two accounts; `monetization.md` § Pillar 4 Rail C step 1) and admits at the claim's linked quota tier. A redeemed invite code may also carry a guardian designation for a supervised admission — refused if the named guardian is the admitted actor itself (mechanism owner: [`../../behavior/family-safety.md`](../../behavior/family-safety.md) § The guardianship link). With no invite code supplied, `invite_required` mode is refused; otherwise `max_free_users` is enforced if a ceiling is set
5. Creates the user row with the resolved tier; a supervised admission also writes the guardianship link + default guardian-policy row in the same transaction, and a membership-claim admission also records a `subscribers` row for the claim's paid window
6. Indexes the actor in full-text search

### Handle resolution

Handles resolve **online, via the origin nest** — the authoritative source is the nest's own DB, served over its API: `fauna.nest.resolve` (HTTP-probes `https://{domain}` to find the domain's nest), then `fauna.actor.by_handle` for the DB lookup (`fauna.handle.available` + `fauna.nest.info` round out the discovery kinds), plus `/.well-known/webfinger` for ActivityPub/Fediverse interop. The nest writes **no** DNS for handles.

Per-user handle DNS records (`_fauna.{handle}.{domain} TXT "id={actor_hex}"`, which would enable resolution without the origin nest being online) are **deferred** — they had no implemented consumer, and under the DNS-write model only the client (which holds the DNS-provider keys) may publish DNS, so a robust DNS-based handle mechanism would be **client-published, O(1) not O(user)**, and is added only if a real origin-independent need appears. The nest never holds DNS-provider keys and never writes DNS ([`../../behavior/dns-management.md`](../../behavior/dns-management.md) § Where the credential lives). Handle changes carry a 6-hour delay and a 72-hour cooldown on the released name (original owner can reclaim during the cooldown).

`subhandles` (the `handle@domain` / `@handle.domain` alternative address forms) is an app-set nest-config flag: the nest reports it in node-info and includes the address strings in resolution responses — all nest-served from its DB, no DNS involved. The authoritative value is the **app-set `nest_subhandles` DB singleton**, flipped by the admin via `fauna.admin.set_subhandles` (Admin-class) and boot-resolved into the live `AppState.subhandles`; the `[nest].subhandles` config is only the **pre-claim seed** (used until an app sets the row). Read back by the admin client on `fauna.setup.status` (`SetupStatusReply.subhandles`). Per `principles.md` § One configuration surface: nest config a user/admin chooses comes from apps, not CLI/env/hand-edited files.

### Tiers

Tiers define per-user quotas and feature access. Admins define tiers; users are assigned one at registration and can upgrade. Quota fields: max inbox bytes, storage bytes, device count, blob size limit, and feed count.

These quota tiers are distinct from the creator *subscription* tiers ([`../../behavior/monetization.md`](../../behavior/monetization.md) § The unifying model). **Paid nest access** joins the two — an admin-owned subscription tier designated as a membership tier and linked to a quota tier, with paid self-service admission riding the registration mode exactly like invite codes — owned by `monetization.md` § Pillar 4 (nest-side admission + lapse reconcile built 2026-07-23; the app storefront remains — status: `monetization.md` § Implementation status today).

### First Admin Bootstrap

When a nest boots with no users, it enters bootstrap mode:

1. Nest writes a claim code to `/data/claim-code` (format + generation owner: [`../../behavior/onboarding.md`](../../behavior/onboarding.md) § 3a; entropy/throttle rationale: [`../federation.md`](../federation.md) § Security)
2. The `fauna.setup.status` WS-RPC kind reports wizard progress (domain, TLS, DNS, email, admin claimed)
3. The first user claims admin via the `fauna.auth.claim_admin` WS-RPC kind with the claim code + a handle + an Ed25519 signature (the `POST /api/v1/claim-admin` HTTP twin was removed in S4d). **The claim is self-contained — it does not register first.** It is an anonymous pre-identity kind that verifies the code and signature itself and creates the admin's `users` row (with the handle) directly, so it works on a `closed` nest and needs no prior account. That is what keeps a fresh box claimable now that an unregistered actor is refused everywhere else.
4. Server creates the superadmin account and deletes the claim code file (single-use)

A nest is claimed with a **handle alone** — no domain required. The default
deployment boots **domainless** (identified by its keypair, reached at any IP,
serving the self-signed floor) and gains domains later from an app. The
role-transition flow (domainless boot → claim-by-handle → app-added domains) is
owned by [`domains-and-tls-bootstrap.md`](domains-and-tls-bootstrap.md); the claim transaction and app
wizard by [`../../behavior/onboarding.md`](../../behavior/onboarding.md) § 3a.

The **NAT mode** (public/private — whether this nest is internet-facing) is likewise an app choice, not an env/CLI policy — owner: [`common.md`](common.md) § NAT mode (wizard step: [`../../behavior/onboarding.md`](../../behavior/onboarding.md) § 3b-bis).

---

## Federation

All federation endpoints are consumed by remote servers, not by apps. They are feature-gated and disabled on nests that do not enable the corresponding protocol.

### ActivityPub (`#[cfg(feature = "activitypub")]`)

Actor serving (WebFinger, NodeInfo, actor document, inbox, followers/following, outbox), the inbound/outbound pipelines, and per-account opt-in (via the unified Bridges page's `fauna.bridges.link` — the duplicate `/api/v1/activitypub/enable` HTTP twin was ripped 2026-07-16) — owner: [`../../behavior/activitypub.md`](../../behavior/activitypub.md). AP is not on by default for a user even when the nest has the feature enabled. HTTP-surface classification (the `/ap/*` + WebFinger/NodeInfo Layer-6 federation residue — the only AP HTTP left) — owner: [`../api-layers.md`](../api-layers.md).

### CalDAV / CardDAV / WebDAV

The in-nest DAV control plane is **retired**. The only in-nest DAV HTTP surface is service discovery: `GET /.well-known/{caldav,carddav,webdav}` each 301-redirect to `mail.<primary-domain>`, where the mail-bridge MDA serves the real store. Owners: [`../../behavior/caldav-server.md`](../../behavior/caldav-server.md), [`../../behavior/carddav-server.md`](../../behavior/carddav-server.md), [`../../behavior/webdav-server.md`](../../behavior/webdav-server.md).

### Email MTA-STS

The nest serves `GET /.well-known/mta-sts.txt` when mail is enabled. Policy semantics, mode storage, and cert coupling — owner: [`../../behavior/mail-multidomain.md`](../../behavior/mail-multidomain.md) § MTA-STS.

### Nostr (`#[cfg(feature = "nostr")]`)

The Nostr **control plane** (link/unlink/status/settings, follows) rides the unified `fauna.bridges.*` kinds with `bridge_id: "nostr"` (§ Bridge Management) — its dedicated HTTP routes were retired with the bridge control-plane sweep. **No `/api/v1/nostr/*` routes remain** (the native-content rip completed the surface's deletion, 2026-07-22): the three native-content routes formerly here (zap total, badge list, publish pre-signed event) now ride the prefix-less kinds `nostr.{zaps.total,badges.list,events.publish_signed}` (owner: [`../../ui/nostr.md`](../../ui/nostr.md); classification: [`../api-layers.md`](../api-layers.md) § Nostr).

The nest also serves the Nostr **relay protocol itself**, permanently, as third-party-facing residue (the far end is another Nostr client or relay, not a Fauna client, so this is the Nostr analogue of ActivityPub's `/ap/*` + WebFinger residue, never a WS-RPC migration candidate): `GET`/`WS` `/nostr` (NIP-01 relay over a persistent owner-scoped event store, NIP-42 auth), `GET /nostr/info` (NIP-11 relay info), and `GET /.well-known/nostr.json` (NIP-05 `you@<domain>` identity resolution). Detail owner: [`../../ui/nostr.md`](../../ui/nostr.md).

**NIP-65 relay list publishing:** when a user updates their relay list (Nostr bridge settings), the sync worker (`nostr/sync_worker.rs`) builds and signs a NIP-65 kind-10002 relay list metadata event and publishes it to well-known discovery relays. This tells other Nostr clients where to send events destined for the user's pubkey.

### Bluesky OAuth

```
GET /.well-known/atproto-oauth-client
```

AT Protocol OAuth client metadata so the linked Bluesky account's PDS can validate this nest as an OAuth client during the Bluesky bridge auth flow.

---

## Bridge Management

Bridges are external processes enrolled as **service-users** — the nest discovers each bridge's role from its enrollment row; there is no subprocess/daemon-socket architecture and no bridge proxy layer (the Layer-4 `ANY /api/v1/bridge/{bridge_name}/*` forwarder is deleted). Architecture + the bridge-kind catalogue — owner: [`../apps/bridges.md`](../apps/bridges.md); there is no admin bridge HTTP surface — enrollment is the WS-RPC `fauna.bridges.request_enrollment` plus the admin approval kinds.

### Generic Management API

Apps use these typed WS-RPC kinds to connect and manage bridges — the same calls regardless of protocol. The HTTP twins (`/api/v1/bridges/*`) were deleted in the T9+T10 sweep; [`../api-layers.md`](../api-layers.md) owns the authoritative catalog and per-kind replay/deadline semantics.

| WS-RPC kind | Purpose |
|-------------|---------|
| `fauna.bridges.list` | Installed bridges and their connection status |
| `fauna.bridges.link` | Start the OAuth or credential flow to connect a bridge |
| `fauna.bridges.unlink` | Disconnect a bridge and stop syncing |
| `fauna.bridges.set_settings` | Bridge-specific config: sync frequency, content filters, notification rules |
| `fauna.bridges.list_follows` | Accounts followed through this bridge |
| `fauna.bridges.add_follow` | Follow an external account so their content appears in the feed |
| `fauna.bridges.remove_follow` | Unfollow an external account |
| `fauna.bridges.list_follow_requests` | Follow requests waiting on the account through this bridge |
| `fauna.bridges.resolve_follow_request` | Approve or refuse one waiting follow request |

### Bridge Feed Subscriptions

External protocol feeds (e.g., a Bluesky algorithm feed) subscribe and flow content into the unified feed via the `fauna.bridges.feeds.{list,create,delete}` WS-RPC kinds (the `/api/v1/bridge-feeds/*` HTTP twins were deleted in the T9+T10 sweep). Once subscribed, the bridge ingests posts and submits them via `fauna.posts.create`, which appear in unified feed queries; **the nest classifies nothing at ingest** — the classify-at-ingest stage was retired at Phase 4, 2026-07-12 (`../data-flow.md` § Bridge post to unified feed; `../content-scoring.md` § The placement matrix).

---

## Admin Surface

The admin surface is **Admin-class WS-RPC on the same bearer transport** as everything else — the former `/admin/api/*` HTTP routes were deleted in the WS-RPC-everywhere cutover. The authoritative per-kind catalog is [`../api-layers.md`](../api-layers.md); the public-mode scope is the kind families:

| Kind family | Purpose |
|-------------|---------|
| `fauna.admin.users.*` | List, create, inspect, edit, delete accounts; force-release handles; evict / cancel-eviction / suspend (warn → suspend → delete timeline) |
| `fauna.admin.tiers.*` | Define and update storage/feature tiers (quota limits + feature flags) |
| `fauna.admin.membership_tiers.*` | Designate one of the admin's own subscription tiers as a nest-membership tier linked to a quota tier (`admin_tier`/`lapse_tier`) — the Pillar 4 paid-admission designation (§ Tiers, above); full semantics owned by [`../../behavior/monetization.md`](../../behavior/monetization.md) § Pillar 4 |
| `fauna.admin.invite_codes.*` | Generate, list, revoke invite codes for closed or invite-gated registration |
| `fauna.admin.audit.*` | Browsable audit log of admin actions + hash-chain integrity verification |
| `fauna.admin.gc` | Trigger garbage collection of orphaned blobs and expired data |
| `fauna.admin.stats` / `fauna.admin.status` | Nest-wide stats (user count, storage, blob count) + runtime status |
| `fauna.admin.worker.status` | Nest-link proxy worker: connection state, authorized key, capacity, replication count |

### Nest Pairing Policy

Pairing is **user-controlled** — a user links one of their own nests from their own app (see [`private-mode.md`](private-mode.md) § Pairing Flow and [`../../behavior/linked-nests.md`](../../behavior/linked-nests.md)). The admin does **not** approve individual pairings; there is no pending-approval queue.

The admin holds one nest-level knob — the `pairing` service toggle (surfaced on the app admin `admin-nest` page per [`../../behavior/admin.md`](../../behavior/admin.md) § Admin IA redesign — the former Services page was removed 2026-06-04) — to enable or disable user-initiated pairing for the whole nest (default **on**); an optional per-tier `allow_pairing` flag is a future refinement. The former `GET/POST /admin/api/pairings*` routes and the `fauna.admin.pairings.{list,approve}` WS-RPC kinds are **retired**.

### DNS

The nest does **not** manage DNS provider credentials or write DNS records. DNS is published **client-side** — the admin's client holds the DNS-provider keys and publishes via `libs/fauna-provisioning` ([`../../behavior/dns-management.md`](../../behavior/dns-management.md) § Where the credential lives); the nest only **reads/verifies** DNS (`fauna.dns.{list,verify}_records`).

---

## Nest Pairing (Public Side)

The public nest is the passive relay side: it stores the user-authorized pairing rows (subject to § Nest Pairing Policy), buffers `__conv` messages and sealed `__mail` records for paired private nests, serves namespace pull/push, and accepts forwarded posts. The whole mechanism — pairing flow, the sync kinds on the federation channel, capabilities, schemas — is owned by [`private-mode.md`](private-mode.md) (§ Pairing Flow, § Sync transport, § MLS Message Buffering, § Namespace Sync, § Post Forwarding).

---

## Content Delivery

The client-facing content surfaces are WS-RPC kinds (catalog owner: [`../api-layers.md`](../api-layers.md)); the public-mode scope lines:

- **Feeds** — feed CRUD + feed-post queries ride `fauna.feed.*`; the feed engine applies filter rules (tags, classifier labels, protocol source, media, time) over the unified content table. Feed UX — owner: [`../../ui/feed.md`](../../ui/feed.md); scoring mechanism — owner: [`../content-scoring.md`](../content-scoring.md).
- **Cross-nest scored candidates** — remote nests answer `fauna.federation.feed.query` with scored post candidates for merging into a local feed. (The old interest-profile read `GET /api/v1/context` was deleted with zero consumers; a future interest-profile read is a net-new kind, not a migration.)
- **Full-text search** — `fauna.search.query` (FTS5-backed, protocol + date filters). Inbox reads ride `fauna.inbox.{fetch,ack}`.

### Subscription / Paywalled Content

[`../../behavior/monetization.md`](../../behavior/monetization.md) owns the cross-cutting model (tier-as-entitlement, the E2E vs web-paywall delivery rails, the broadcast-`KeyBlob`/`KeyAccess::Mls` crypto, payment providers); this section is the public-nest scope line.

Creators define subscription **tiers**; gated posts unlock for entitled subscribers, who decrypt with their own keypair — non-subscribers cannot, even seeing the ciphertext in the feed. Subscription management rides the `fauna.subscriptions.*` **WS-RPC kinds** (`tiers.{list,create,update,delete}`, `offers.list`, `mine.list`, `{subscribe,unsubscribe}`, `status.get`, `requests.{list,approve,reject}`, `key_blob.get`, `subscribers.{list,remove}`, `delegate.upload`) — the mutating `/api/v1/subscriptions/*` HTTP routes were **removed** in the WS-RPC-everywhere cutover; only public, unauthenticated reads stay HTTP (`GET /api/v1/subscriptions/tiers/{author_id}`, `GET /api/v1/subscriptions/delegate/{author_id}`, `GET /api/v1/nest/info`); the full per-kind semantics are owned by [`../../behavior/monetization.md`](../../behavior/monetization.md).
