# Nest: domainless-default boot & domain lifecycle — target state

Owns: domains, tls-bootstrap, host-address
Status: ratified
Authority: the nest role-transition story — domainless-default boot (loopback floor SAN shape + floor-first serving), claim-sets-identity (the handle's @domain as the primary mail_domains row = the deployment identity, with the local-target carve-out), client-added domains post-claim, host-address acquisition (the client-reports decision tree + the LAN standing deferral), and the FAUNA_DOMAIN/FAUNA_SELF_SIGNED retirement glue; defers cert mechanics to [`tls-certificates.md`](tls-certificates.md) § A/B, the claim transaction to [`../../behavior/onboarding.md`](../../behavior/onboarding.md) § 3a, registration to [`public-mode.md`](public-mode.md), multi-domain add/remove to [`../../behavior/mail-multidomain.md`](../../behavior/mail-multidomain.md), the DNS record surface to [`../../behavior/dns-management.md`](../../behavior/dns-management.md), and the container env contract to [`../installers/docker.md`](../installers/docker.md) § Environment Variables.

## Goal

A nest is identified by its **Ed25519 keypair**, not a domain. The default
deployment has **no domain at all**: it boots, serves TLS, is reached at whatever
address the admin points at it (an IP, a LAN hostname, `localhost`), is claimed
with a bare handle, and only *then* — from a Fauna app — gains one or more
domains. This is the existing product invariant ("nest configuration is set from
Fauna apps, not env vars / CLI / hand-edited files"; "works out-of-the-box"),
stated end-to-end. `FAUNA_DOMAIN` / `FAUNA_SELF_SIGNED` and the hosts-file trick
were interim scaffolding; this is the target they collapse onto.

## The five requirements (ratified 2026-06-14)

1. **Boots with NO domain by default** — identified by its keypair, reachable at
   any IP / any address the admin points at it, with no hosts-file editing and
   no `FAUNA_DOMAIN`.
2. **Serves self-signed HTTPS from boot** — the always-live floor (§ Boot). Native
   clients trust it via channel-binding (identity pin); browsers / MUAs accept the
   one self-signed prompt.
3. **Claimed with a handle ALONE** — no domain required, and **no domain
   auto-registered from the access address** (§ Claim).
4. **Domains (any number) are added by an app AFTER install** — Admin→DNS /
   `fauna.bridges.add_local_domain`; adding a domain is what triggers a trusted
   (ACME) cert for it. The floor stays the fallback (§ Add domains).
5. **Nothing on the server disk is ever edited** by an operator. `FAUNA_DOMAIN` is
   **retired** and `FAUNA_SELF_SIGNED` is removed (§ Env contract).

## Boot: the always-live self-signed floor (domainless-capable)

Every nest writes and serves a self-signed **floor** cert from boot,
**unconditionally** — with or without a configured domain, and regardless of
whether ACME is enabled. This is [`tls-certificates.md`](tls-certificates.md) § A
("Every nest maintains a self-signed floor at all times — always live … guarantees
a listener never has *no* cert"); see that doc for the cert mechanics (stable
floor key, per-SNI valid-else-floor, auto-renew). The glue claims this doc owns:

- A **domainless** nest's floor is **loopback-only** (SANs `localhost` + `127.0.0.1`,
  CN `fauna-nest`) — `write_self_signed_bootstrap(None)`. A browser reaching it by
  IP sees the one expected self-signed prompt; native apps pin the identity and
  ignore the name (channel binding, `security.md` § Transport trust). A box with a
  **directly-attached public IP** additionally obtains a short-lived trusted cert
  for that IP and serves it to IP dials until its domain cert is live — the IP
  bridge cert, owner [`tls-certificates.md`](tls-certificates.md) § B-IP (ratified
  2026-08-29); the loopback-only floor beneath it is unchanged.
- Serving the floor **first** means an ACME deploy whose order can't complete (a
  LAN box on a real hostname, no public DNS / inbound `:80`) **no longer strands
  the listener** on a pending resolver — HTTPS is up immediately and ACME
  self-heals the floor to a trusted cert if/when it can. This is what retires the
  `FAUNA_SELF_SIGNED` knob (the unconditional floor subsumes it).

## Claim: the handle domain IS the deployment's identity domain

The onboarding/claim flow is owned by [`onboarding.md`](../../behavior/onboarding.md)
§ 3a (`fauna.auth.claim_admin`) and the registration shape by
[`public-mode.md`](public-mode.md) § Registration. The glue claim this doc owns —
**the handle's `@domain` is the *sole* determinant of the deployment's domain, set
at claim**; the box boots domainless and learns its domain when the admin claims it
(there is no `FAUNA_DOMAIN` — § Env contract):

- A claim **requires a handle**; its `@domain` suffix travels as an **optional**
  `mail_domain` (wire shape + rejection rules — a handle-less admin is
  unrepresentable: [`onboarding.md`](../../behavior/onboarding.md) § 3a).
- **Identity = the primary domain, set at claim (single source of truth).** The
  primary `mail_domains` row **is** the nest's identity (`../../behavior/dns-management.md`);
  there is no separate identity store. For a real domain, `claim_admin_core` →
  `mail_enable::ensure_mail_domain_registered` writes that primary row and, in the same
  call, `identity_domain_core::apply_primary_identity` swaps the sync
  `AppState.identity_domain` **cache** (a projection of the row — it cannot drift) and
  self-heals TLS. `AppState::handle_domain()` / `web_serving_domain()` read that cache
  at top precedence over the `config.nest.domain` seed, so identity/discovery/web/
  TLS-apex and mail all follow the claimed domain from the same instant (no split-brain
  where a domainless-booted box thinks it is `localhost` while mail knows the real
  domain). The cache is boot-loaded from the primary by
  `identity_domain_core::resolve_identity_domain`.
- **Every runtime surface that names the deployment follows the claim — the rule is
  general, and the list is not an enumeration to keep in sync but a consequence.** A
  request-path read of the `config.nest.domain` *seed* is frozen at boot, so on a
  provisioned box (which boots domainless by design) it serves `localhost` — or worse,
  a hard-coded placeholder — forever. This binds **ActivityPub** (WebFinger, the actor
  document, followers/following/outbox, note dereference, nodeinfo, the instance actor,
  and outbound `Create` push), **nostr** (NIP-42 AUTH host matching and the gift-wrap
  serving gated behind it, the NIP-11 relay document, the bunker connect-string, NIP-05
  relay hints, the NIP-65 relay self-advertisement), **video** HLS playlist segment base
  URLs, the **federated Welcome** `origin_nest_url`, the **setup-status** report, and
  **`fauna.nest.info`** (its `domain`, and the whole `registration` block — both its
  presence and its `handle_domain`) — each reading `handle_domain()` /
  `handle_domain_if_set()`, never the seed.

  ⚠ `nest.info` was the **last surface still reading a seed**, and it was found only
  by routing a test off the `--handle-domain` scaffolding that had been hiding it
  (2026-09-02). Its gate and its `handle_domain` field both read
  `auth.registration.handle_domain` — written by nothing but the `--handle-domain` CLI
  arg, which no deployment artifact passes — so on **every** real box it stayed `None`
  for life and this surface, the PRE-IDENTITY one a stranger reads to learn that
  self-service registration is open and at which domain, answered `domain: "unknown"`
  and `registration: null` however the admin had claimed and configured the box. That
  contradicted this section's own "discovery/registration … follow the claimed domain"
  claim below, and `AppState::handle_domain()`'s docstring, which already named
  `discovery_core` as a caller. The seed's *other* reader, the ActivityPub NodeInfo
  document's `openRegistrations`, had the same defect and the same fix. The rule's
  generality is the point: two sites read the seed, both were wrong, and neither was a
  judgement call once the accessor existed. **One
  deliberate exception:** the iroh-relay cert's seal AAD label (`sidecar_channel.rs`)
  binds the *seed*, because it is a sealing binding — following the live domain would
  make a pre-claim-sealed relay cert undecryptable. Enforced as a class, not per site,
  by `state::tests::no_runtime_handler_reads_a_domain_boot_seed`: any new read of
  **either** seed off an `AppState` — `config.nest.domain` or
  `auth.registration.handle_domain` — fails the build unless a
  `// seed-read-ok(<class>): <reason>` marker sits within the 8 lines above it
  (`accessor` / `boot-binding` / `test`). The guard covers **both** seeds, and that
  generality is itself the 2026-09-02 lesson: the 2026-07-23 version was keyed to
  `config.nest.domain` alone, so the `handle_domain` seed's two readers above were
  invisible to it for six weeks. A third seed joins by adding a table row, never by
  hand-copying the test; the marker is line-level rather than file-level because the
  old file-level allowlist exempted the whole of `state.rs`, which holds four seed
  reads of its own. TLS self-heal:
  `apply_primary_identity` force-re-synthesizes the floor to cover `<domain>` +
  `mail.<domain>` + `relay.<domain>` + `pds.<domain>` (SPKI-stable) and wakes the ACME
  lifecycle task to issue a trusted cert (§ Implementation status).
- **A local target registers no domain (identity stays the `localhost` fallback).**
  When the admin reaches a domainless nest at a **local target** — an IP literal,
  `localhost`/`*.localhost`, or a `.local` mDNS name (the targets
  `fauna_provisioning::probe::resolve_handle_domain` classifies as NOT
  `is_public_dns_name`) — nothing is
  registered: `mail.<IP>` / `mail.<localhost>` is nonsense (it breaks mail + CalDAV host
  routing), and the whole point of the domainless default is that a real domain is added
  *later* from an app. So `handle_domain()` stays the `localhost` fallback (as before;
  the access address is not persisted — a LAN box is reached by IP and TOFU-pinned by the
  nest key, not its domain). A handle on a genuinely registerable domain —
  `alice@example.com` — registers the primary mail domain and sets the identity; the
  carve-out is **local targets only**. Adding a real domain later from an app (the
  domainless-then-add-domain flow, `fauna.bridges.add_local_domain`) sets the primary +
  identity the **same way** — the handler applies the primary identity via the shared
  `identity_domain_core::apply_primary_identity` step (the same step
  `ensure_mail_domain_registered` runs at claim / boot), guarded on the added row being
  the first (primary) domain and a real (non-local) target. It writes the `mail_domains`
  row directly rather than routing through `ensure_mail_domain_registered` so the
  client's MTA-STS / catch-all / DKIM-selector picks are preserved.

## Add domains after install (client-driven → ACME)

A claimed, domainless nest adds domains from an app: Admin→DNS, the
`fauna.bridges.add_local_domain` WS-RPC kind (and the multi-domain "+ Add domain"
wizard, [`mail-multidomain.md`](../../behavior/mail-multidomain.md) § Adding a new
local domain). Adding a domain is what triggers trusted-cert acquisition for it
([`tls-certificates.md`](tls-certificates.md) § B — HTTP-01 / DNS-01); until a
trusted cert exists the per-SNI floor covers the name. The DNS-provider credential
lives on the client, never the nest ([`../../behavior/dns-management.md`](../../behavior/dns-management.md)).

**The first such add is irreversible.** It becomes the **primary**, and the primary row is the
deployment identity; the primary cannot be removed, and the only other exit is the primary-rename
ceremony onto another real domain — there is no revert-to-domainless. Rules owned by
[`../../behavior/mail-multidomain.md`](../../behavior/mail-multidomain.md) § Removing a local domain
and [`../../behavior/mail-primary-domain-rename.md`](../../behavior/mail-primary-domain-rename.md).
What this means for a LAN-only box that has no business carrying a public apex at all:
[`deployment-home-with-public-relay.md`](deployment-home-with-public-relay.md) § MUA reach.

## Host-address acquisition: the client reports the nest's public IP (ratified 2026-07-06)

`nest_host_address` (`nest_ipv4`/`nest_ipv6` for the apex, `mail_ipv4`/`mail_ipv6`
for the `mail.<primary>` MX host — [`fauna.dns.set_host_address`](../../behavior/dns-management.md))
is the deployment's **public IP**. It is a deployment **fact**, not a user/admin
*choice* — under the one-config-surface invariant it is **bucket-1 auto-detected /
reported IPC, never a app-UI knob**. It gates the **strong** ACME HTTP-01
resolve-gate ([`tls-certificates.md`](tls-certificates.md) § HTTP-01 — the
`*_resolves_to_*` checks that defer a domain whose DNS does not point *here* rather
than dropping it into the all-or-nothing order) and it is the source of the
assembled apex/`mail.` `A`/`AAAA` (+ advisory `PTR`) rows. When it is **not**
persisted the resolve-gate falls back to a weak "any record" admit — availability-
only, but the origin of the open weak-fallback residual on the secondary-apex gate.

**Who acquires it — the admin client, at claim/onboarding (not nest self-detection).**
The client holds the DNS-provider credential, already sets the nest's public DNS,
and can reliably determine the public IP; the nest behind NAT cannot self-detect a
reliable address (and cannot know a `mail_ipv4 ≠ nest_ipv4` split MX host). So the
admin client calls `fauna.dns.set_host_address` as part of onboarding, and again
**idempotently on later admin-client connects** so an existing box (or an IP change)
converges without re-onboarding. This is an **IPC call the client makes, not a UI
element** — so it is uniform across all 7 apps (priority #1: identical behavior)
without being a priority-#1 *UI* deviation (see § Implementation status for the
current per-app wiring — **all 7 landed**).

**The LAN case — the client MUST report the nest's PUBLIC address, never its
observed connect-address.** An admin frequently sets up a box from the **same LAN**
(reaching the nest at a private IP or an mDNS `.local` name). The observed connect-
address is then a LAN address, which is **useless and harmful** as a host-address —
publishing it as the apex/`mail` `A` record poisons public DNS and guarantees an
HTTP-01 failure (a CA cannot reach a private IP). The client determines the address
to report as follows:

- **Reached the nest at a publicly-routable address** (dialed a public IP, or a
  hostname that resolves to one) → report that address.
- **Reached the nest at a private address** — RFC 1918 (`10/8`, `172.16/12`,
  `192.168/16`), CGNAT `100.64/10`, link-local `169.254/16`, IPv6 ULA `fc00::/7` or
  link-local `fe80::/10`, or an mDNS `.local` name → the client is on the **nest's
  LAN**. Determining the WAN IP here would require an **external IP-echo / STUN**
  reflector (a LAN-connected client shares the nest's NAT, so the reflector's returned
  WAN IP **is** the nest's public IP) — but fauna wires **no** such reflector: iroh's
  relay is `RelayMode::Custom` at the nest's *own* `relay.<domain>` (inside the LAN,
  useless for this) or `Disabled`, never a public STUN, per the self-hosted invariant.
  So active LAN WAN-discovery is a **standing deferral** (§ Implementation status), and
  a private-address dial falls through to **report nothing** (next bullet), keeping the
  self-signed floor.
- **Never** send a private/LAN/`.local` address as `nest_ipv4`/`mail_ipv4`.
- **No public IP determinable** (a pure-LAN nest with no port-forward / not publicly
  reachable) → the client reports **nothing**. Such a box is not a public mail/ACME
  deployment; it keeps the self-signed floor and is reached by IP + key-TOFU exactly
  as the § Claim *local-target* carve-out already describes. (A LAN-only box has no
  public host-address to report — consistent, not contradictory.)
- **Single-box (the common case):** `mail_ipv4 == nest_ipv4` (one public IP for both
  roles). A split MX host (`mail.<primary>` on a separate box) is a distinct advanced
  admin action that sets `mail_*` independently; the client defaults them equal.

**Implementation status:** the nest side is **built** — `fauna.dns.set_host_address`
persists the address and the strong resolve-gate reads it when present. The
**client-reports flow is now built for the public-address + safety half, on the
shared layer + web + linux + windows** (2026-07-07, `clients-host-address-onboarding`):

- **Shared Rust** — the public/private classifier (`fauna_core::resolve::is_global_ip`,
  lifted from the nest SSRF guard so both share it), the decision fn
  (`fauna_client_dns::host_address::report_host_address` + `classify_dial_host`) that
  reports a **public** dial-address (IP literal, or a public name it resolves) and
  **never** publishes a private/LAN/`.local` one, and the `DnsAdminClient::set_host_address`
  caller. STUN is an **injected capability** (`HostAddressProbe`) so a reflector can be
  dropped in later without touching the decision tree.
- **Exposed** via the `fauna-ffi` free fn `report_host_address` (native
  windows/apple/android) and the `fauna-wasm` `reportHostAddress` binding (web).
- **Wired** at the universal post-auth hook, admin-gated + idempotent, on **web**
  (`+layout.svelte`), **linux** (`AdminStatusLoaded`), **windows**
  (`StartMainAppAsync` → `HostAddressReporter`, the am_i_admin-gated on-connect
  report), **apple** (2026-07-12, `MailEnableGlue.reportHostAddress` called
  from the shared FaunaKit universal post-auth `Task` on both macOS and iOS —
  `apps/fauna-apple/Fauna-macOS/App/FaunaMacApp.swift` /
  `apps/fauna-apple/Fauna-iOS/App/FaunaApp.swift`), and **android** (2026-07-17,
  `MailEnableGlueVM.reportHostAddress` — `api.checkIsAdmin()`-gated, called from
  the universal post-auth `LaunchedEffect` in `FaunaNavHost.kt` alongside the
  mail/caldav/deployment-seed glue). So a box **any of these 6 apps'** admin
  claims/reconnects-to at a **public** address now gets the strong gate.
  **`tui` landed 2026-07-29** (`apps/fauna-tui/src/admin/mod.rs`
  `spawn_host_address_report`, fired from the `Outcome::GateLoaded { is_admin:
  true }` fold — the same `am_i_admin` observation linux hooks, so it is
  admin-gated by the caller). **All 7 apps now report**, and the
  admin-only-uses-tui hole below is closed.
- **LAN active WAN-discovery is a STANDING DEFERRAL** (user decision 2026-07-07,
  reaffirmed at close): a home-LAN nest behind NAT would need an **external**
  STUN/echo reflector to learn its WAN IP, and fauna wires none — the self-hosted
  invariant blocks a public-STUN default, and the nest's own iroh relay
  (`RelayMode::Custom` at `relay.<domain>`, or `Disabled`) sits inside the LAN, useless
  here. So the injected probe (`HostAddressProbe::stun_public_ipv4`) returns `None`, a
  pure-LAN box reports **nothing** and keeps the self-signed floor — exactly the § "No
  public IP determinable" safe fallback — and the never-publish-a-private-address
  **safety** invariant holds regardless. **This is intentional, not a stub awaiting
  completion.** The only case it fails to serve — a box that is publicly reachable yet
  *only ever dialed via its LAN address* — self-resolves the moment an admin dials it by
  its public name/IP (the already-shipped resolve path), which happens naturally when
  remote access is set up. **If a real need for zero-config LAN onboarding ever surfaces,
  the invariant-clean upgrade is an admin-configurable, opt-in reflector endpoint**
  exposed in the app UI (a net-new ui.yaml element across all 7 apps → priority-#1
  approval) and dropped in behind the existing `HostAddressProbe::stun_public_ipv4` seam —
  the decision tree and its `lan_dial_with_reflector_reports_stun_wan_ip` test already
  accommodate it. A baked-in **public-STUN default is rejected**: external dependency plus
  it leaks the box's existence to a third party, both barred by the self-hosted invariant.

**All 7** app call-sites now report the host address — android landed
2026-07-17, `tui` last on 2026-07-29 (above). The
weak-fallback residual now persists **only** for the
separately-deferred LAN-discovery case (above): a box behind NAT stays on the
weak gate until an opt-in reflector is built. The "admin only ever uses tui"
arm of this residual is **closed**. This § is the durable record of that
residual — and its scope is now the **secondary-apex** gate alone: the two
primary-domain-rename mail-host gates dropped their weak arm on 2026-08-13
(strong-check-or-DROP, joining the old-apex gate), ruled and owned by
[`../../behavior/mail-primary-domain-rename.md`](../../behavior/mail-primary-domain-rename.md)
§ Renaming away from a dead domain.

## Domain loss: lapse and seizure (ratified 2026-08-11)

The handle domain rents the world's namespace, and the rent can stop — expiry, seizure, a registry dispute. This section owns the end-to-end story of losing the deployment's primary domain: what dies, what survives, the clock, the re-home path, and the recovery-locator consequence. It states the story honestly rather than solving what DNS reality does not permit solving.

**What dies with the name; what survives; where the pin boundary runs.** What dies is every world-facing surface that *resolves* through the name: inbound mail delivery (MX), WebFinger/ActivityPub addressing, NIP-05, the web apex, CalDAV/CardDAV host routing, and ACME issuance for the name's SANs. What survives is everything bound to **keys** rather than the name: every resident's actor identity, data, contacts, and groups; the nest identity (`nest_actor_id`); and — decisively — the apps' trust in the box, because **enrolled apps pin `nest_actor_id`, not the domain** (§ Claim; a LAN box is already reached by IP and TOFU-pinned by the nest key). The next registrant of a lapsed name can obtain real TLS for it trivially, so the pin is the exact boundary of impersonation: **a re-registered domain cannot impersonate the nest to the user's own enrolled apps** (their dial refuses the wrong nest key on any cert), while **unpinned third parties — correspondent MTAs, browsers, fediverse peers — follow DNS** and are the capture surface.

**Mail capture, stated plainly.** Whoever registers the name next receives what the world still sends there, mail included — exactly as with any lapsed mail domain today. Nothing box-side can prevent it; the mitigations are timely re-home and correspondents adopting the new addresses. On a *seizure* the same is true from day one, with the sharper corollary that the old name must be treated as hostile immediately: stop presenting `@old` addresses as live (the admin may soft-delete the old domain rather than keep the demote-not-delete default — [`../../behavior/mail-primary-domain-rename.md`](../../behavior/mail-primary-domain-rename.md) bar 4 names both options).

**The clock is the managed cert's remaining life.** The deployment's ACME order is all-or-nothing and — outside a rename — always contains the primary apex, so once the primary zone is dead **every renewal fails** and the box coasts on the current managed cert — at most the issuer's ~90-day term. Past its expiry, world-facing TLS degrades to the always-live self-signed floor (§ Boot): enrolled apps keep working through the nest-key pin, and everything the floor cannot satisfy (browsers on the web apex, MTA-STS fetches, AP delivery) breaks. Re-home before the cert expires and the degradation window never opens — and starting the rename is itself what lets an order succeed again, since a pre-flip rename resolve-gates the dead apex out (owner: [`../../behavior/mail-primary-domain-rename.md`](../../behavior/mail-primary-domain-rename.md) § Renaming away from a dead domain).

**The re-home path is the ordinary primary-domain rename** — no second machinery exists or should. The surviving admin (or the heir via the succession instrument — [`../../behavior/admin.md`](../../behavior/admin.md) § Admin continuity and succession) registers a new domain, adds it (`fauna.bridges.add_local_domain`), and runs `start_primary_domain_rename(<new>)`. The rename's behavior against a *dead* old primary — the grace window's irrelevance, the dead-apex ACME deadlock and its resolve-gate remedy, the post-completion disposition of the old domain — is owned by [`../../behavior/mail-primary-domain-rename.md`](../../behavior/mail-primary-domain-rename.md) § Renaming away from a dead domain. Residents' handles are deployment-wide identifiers, so every `bob@old` is reserved-for-bob at `bob@new` by the existing handle-uniqueness rule; users opt into the new address through the standard add-address flow.

**The recovery-locator consequence.** Phrase-only identity recovery resolves the home nest **from the handle's `@domain`** ([`../../behavior/onboarding.md`](../../behavior/onboarding.md) § recovery_entry), so a dead domain breaks the locator even though the escrow blob, the actor, and the nest are all intact. Post-re-home, a user who knows their new handle simply types it (the account field exists for exactly this). The remaining case — a user holding only an old-domain kit and no knowledge of the new name — is served by the **direct-address fallback** ratified in onboarding.md § recovery_entry (the account field accepts `handle@<direct nest address>`), which turns "I know my phrase and where my box lives" into a complete recovery even with the namespace gone.

### Detection — the domain-expiry watch (ratified 2026-08-11; BUILT 2026-08-12)

Nothing watched the registration before 2026-08-12 — a lapse announced itself as failures. The remedy is a **domain-expiry watch**: the nest periodically RDAP-queries its primary domain's registration and a critical-alerts feeder surfaces danger on every authenticated page (the feeder row + why it clears the severity bar: [`../../behavior/critical-alerts.md`](../../behavior/critical-alerts.md) § Feeders; this section owns the detection mechanism, per that doc's authority split).

**Two-plane shape.** The **nest** fetches RDAP over its existing outbound HTTPS stack (`reqwest` — no new dependency; resolver order: the IANA RDAP bootstrap for the TLD, with `rdap.org` as the aggregating fallback), on its own slow cadence (daily-class, a bucket-1 constant — registration state moves slowly and RDAP operators deserve politeness; **not** the 5-minute cert tick), and persists `(expiry, statuses, fetched_at, outcome)`. A **User-class read kind** serves that record to any authenticated session (non-admins are in the audience, below). The **feeder** — `libs/fauna-client-alert-sweep`, joining by the seam alone like feeder #3, no app change — reads it in the 6 h sweep and posts/clears. Why this feeder may consume a nest-computed value when the directory feeders may not: the audited party here is the **registry**, not the nest — the condition is not adversarial-to-the-nest (a nest lying about its own domain's expiry defeats only its own users' warning, the same trust class as the nest serving their mail at all), so the audit-floor rule's premise does not apply.

**The signal set (two arms, and why the pre-arm is deliberately short).** The watch alarms on **either**:

1. **Pre-expiry:** the registration's expiry is within `DOMAIN_EXPIRY_ALERT_THRESHOLD` (**7 days**, bucket-1 hard constant) or already past. The window is deliberately short because **RDAP cannot see auto-renew intent** — an at-date auto-renewing registrar shows an approaching expiry every year, so a 30-day window would put a month-long false banner on every healthy auto-renew deployment annually, which is the § Severity bar's crying-wolf failure verbatim. Seven days bounds that false arm to ≤ a week on at-date renewers (zero on renew-early ones) while the remedy — a registrar payment, minutes of work — needs days, not weeks.
2. **Status-driven (the reliable arm):** the registration carries any of the EPP statuses `redemptionPeriod`, `pendingDelete`, `serverHold`, `clientHold` — alarm **regardless of the date**. This arm exists because the date alone is unreliable in exactly the lapse case: gTLD registry auto-renewal can bump the RDAP expiry a year forward *at* expiry even while the registrant has not paid (the registrar later deletes for credit), so a lapsing domain can present a healthy-looking future date. The hold/redemption statuses are what actually announce "this name is out of DNS / dying", and they persist through the grace and redemption windows — the phase where renewal is still possible and the warning is most valuable.

**Audience — every authenticated user, text differing by role (ratified).** This is the first *deployment-scoped* feeder, and the banner surface has no admin scoping; both facts are correct here. Only an admin can renew, but a resident's stake is irrecoverable in its own right — their `@domain` addresses die and their phrase-only recovery **locator** breaks (§ The recovery-locator consequence above) — and their remedy is real but different: reach the admin, and know their box's direct address while the kit fallback remains unbuilt. So the feeder composes role-appropriate `LocalizedText` lines (admin: renew at the registrar, now; non-admin: the deployment's name is lapsing — contact your admin), rather than scoping the alert to admins and leaving residents to learn at the failure.

**The three outcomes (per the feeder contract).** A TLD the RDAP bootstrap does not serve (many ccTLDs) is a **skip** with a stable reason token (`rdap-unserved-tld`) — never a failure, never an alert: absence of data must not alarm. A domainless deployment skips (`no-primary-domain`). An RDAP server that is present but unreachable/erroring is a **failure** — retried next sweep, and a standing alert is left standing (unreachable is not resolved). No config surface anywhere: the thresholds and cadence are hard constants, and there is nothing here a user or admin would choose (the only human act is renewing the domain, which happens at the registrar).

**Two build-time facts a cold read needs, neither of them re-derivable from the prose above.** **(1) The RDAP status vocabulary is spelled two ways in the wild, and matching one loses the reliable arm on half the world's registries.** RFC 9083 § 10.2.2 defines the RDAP names with spaces and lowercase (`redemption period`, `pending delete`, `client hold`, `server hold`), while plenty of registries emit the raw camelCase EPP token instead; the comparison therefore normalizes (lowercase, strip spaces/hyphens/underscores) before matching — `fauna_protocol::domain_expiry::normalize_status`, pinned over both spellings. **(2) Every RDAP fetch goes through the nest's SSRF guard**, because the base URL of a TLD's RDAP service is read out of the IANA bootstrap file — a *remote document naming remote hosts* — which is a caller-supplied URL in the sense `ssrf.rs` means. Redirects are followed explicitly, at most four hops, with the guard re-run at each: RDAP services redirect constantly (the `rdap.org` fallback is essentially a redirector), so following none would break the fallback path entirely and following them inside the HTTP client would defeat the DNS pinning.

### Implementation status — domain loss

The story above is ratified over existing machinery; the rename's dead-old-apex resolve-gate is **built** (2026-08-11 — owned by mail-primary-domain-rename.md § Renaming away from a dead domain, which states the gate and its drop-on-uncertainty fail direction).

**The expiry watch is BUILT.** All four pieces § Detection names exist: the nest's daily-class RDAP fetch + persistence (`bins/fauna-nest/src/domain_expiry.rs`, the single-row `domain_expiry` table), the User-class read kind `fauna.domain.expiry.get` (`domain_expiry_handlers.rs`), the shared two-arm decision + constants (`libs/fauna-protocol/src/domain_expiry.rs`, `DOMAIN_EXPIRY_ALERT_THRESHOLD_SECS`), and feeder #4 in the sweep (`libs/fauna-client-alert-sweep/src/domain_expiry.rs`) — which joined by the seam alone, with **no app change on any of the 7**, exactly as § Detection predicted. The role-differentiated lines are six i18n keys (`critical_alerts.domain_{expiring,expired,lapsing}_{admin,resident}`); the caller's role rides the read's own reply rather than a second `fauna.account.am_i_admin` round trip, so a feeder can never know the domain is lapsing but not which sentence to render.

One piece of the section is only partially reachable: the recovery-entry direct-address fallback itself is **built** — both the client-side classification and the nest-side bare-handle matching (owned + declared in onboarding.md § recovery_entry) — but it is **rendered on tui only**; the other six apps don't yet expose the entry-point CTA, so most residents still can't reach it from their own app. That gap, not an unbuilt mechanism, is why the resident-facing alert lines tell the user to *make sure they know their nest's direct address* rather than pointing at the affordance directly. Everything else the section states — the pin boundary, the floor degradation, the rename path, handle reservation — is shipped behavior, cited at its owner.

## Env contract (deprecations)

Owned in full by [`../installers/docker.md`](../installers/docker.md) § Environment
Variables; the deprecations this track lands:

- **`FAUNA_SELF_SIGNED` — removed.** It meant "turn ACME off + self-sign"; the
  unconditional floor makes the self-sign half automatic and the floor-serves-first
  behavior makes the ACME-off half unnecessary (a non-completable order no longer
  hangs). A fully-local box is best run **domainless** (omit `FAUNA_DOMAIN`).
- **`FAUNA_DOMAIN` — retired.** A domain is a user-chosen policy, and the *only*
  configuration surface is the apps (`principles.md` § One configuration surface — there is no
  operator hand-editing a file/env), so the deployment's domain now comes **solely
  from the admin's claim handle** (§ Claim), persisted as the primary `mail_domains`
  row (which IS the nest's identity — `../../behavior/dns-management.md`). The `docker/entrypoint.sh` `$DOMAIN` block (the `[nest].domain` / `[email]`
  / domain-gated `[acme]` writes) is removed and `docker-compose*.yml` / the install
  scripts / `cloud_init.rs` no longer pass or require it; the box boots **domainless**
  and learns its domain at claim. **Ruled 2026-10-01: the `[nest].domain` config field (`config.nest.domain`) is
  test-harness wiring, written by no shipped artifact.** The Docker entrypoint
  never wrote it after `FAUNA_DOMAIN` went, and the Linux installer's `--domain`
  flag — the last artifact-facing writer — was removed with the ruling, together
  with its `--email` / `--email-domain` / `--smtp-bind` flags (they wrote an
  `[email]` table nothing reads) and `--acme` / `--acme-email`
  ([`tls-certificates.md`](tls-certificates.md) § ACME settings). So on every
  real box the field is absent and the domain comes from the claim. It stays in
  the binary for the conformance and e2e fixtures, which boot nests that must
  hold a domain before any claim runs, and what reads it is: at boot, before an
  identity row can exist, the ACME configuration, the self-signed floor's SAN
  and the plain-HTTP refusal; at every boot by design, the relay seal's binding
  label (`sidecar_channel.rs::relay_seal_domain`, § Claim); and at lowest
  precedence under the identity row,
  `identity_domain_core::resolve_identity_domain`. The nest has no `--domain`
  flag (`--acme-domain`, a flag nothing passed, was removed with the ACME knobs), and
  `docker/entrypoint.sh` forwards no domain-shaped flag.
  A second, differently-named seed exists at the raw-binary level —
  `registration.handle_domain`, set only by the `--handle-domain` CLI arg
  (`ServeArgs::handle_domain`, `bins/fauna-nest/src/main.rs`) and read by
  `AppState::handle_domain_if_set()` between the identity cache and
  `config.nest.domain` — but it is **test/conformance-harness-only scaffolding**
  (`scripts/start-test-nest.sh`, the e2e/conformance test fixtures under
  `tests/`), on the same footing as the `FAUNA_INSECURE_DISABLE_TLS` escape
  (§ Test posture): no deployment artifact (Docker entrypoint, compose file,
  install script, `cloud_init.rs`) ever passes it, so it never reaches a real
  deployment. The DB identity row overrides both seeds once a claim lands;
  with no artifact writing either, a deployment's domain comes only from
  claim/add-domain, never a hand-edited file. This lands the
  deploy-migration that was formerly deferred ("move the public boxes to
  boot-domainless-then-add-domain"). Authority for the full var list:
  [`../installers/docker.md`](../installers/docker.md) § Environment Variables.

## Test posture

The unconditional floor makes **every real nest** serve self-signed HTTPS. The
tier_3 binary-e2e suite, however, connects to nests over plain `http://` from dozens
of scattered call sites across all seven app platforms (many self-spawn their own
nest), so flipping the whole suite onto https would be a large cross-platform sweep.
Instead the suite opts out: the binary honors a **test/diagnostic-only**
`FAUNA_INSECURE_DISABLE_TLS` env var (set process-wide in
`tests/e2e-unified/conftest.py`'s `pytest_sessionstart`) that serves plain HTTP on
**the nest's own API listener**. The escape is scoped to that listener only — the
self-signed floor is **still written to disk**, because the in-process mail bridges
terminate their *own* TLS for CalDAV/IMAP regardless of the nest API's mode and
fetch the floor (sealed) via `fetch_tls_cert_blob`; skipping the floor would leave a
plain-HTTP nest's bridges binding CalDAV/IMAP with no cert to serve (the tier_3
any-locator matrix blocker — `caldav-imap-any-locator`). **No deployment path ever
sets the escape** — the Docker entrypoint never emits it; a real nest serves the
floor on its own listener too. The TLS floor itself is covered by the nest unit
tests (`prepare_listener_tls_*`, `self_signed_cert` tests) and the **tier_4 Docker
suite**, which runs real https end-to-end.

**The `bins/fauna-nest` standalone binary's read of the escape is `test-hooks`-gated
(landed 2026-08-24)**, so a release build of that binary — the
one the Docker image ships — has no `FAUNA_INSECURE_DISABLE_TLS` knob at all: the
env read, the branch that skips TLS, and its `tracing::warn!` are all compiled out
under `default-features` (verified by a `strings`-grep of the built binary). The
tier_3 e2e harness is unaffected — it always builds `fauna-nest` with `test-hooks`
on (`tests/common/nest.py::build_node`'s default `features`). Convention 15's
compile-time exclusion is therefore the actual security boundary here, not merely
the runtime env check. **This did not require migrating tier_3 onto real TLS** —
the earlier framing assumed removal meant flipping the whole suite onto https; the
gate instead removes the knob from the artifact while leaving tier_3's plain-HTTP
behavior unchanged.

**`desktop_serve.rs`'s read of the same escape is `test-hooks`-gated too (landed
2026-08-25), via a flavor split in
the two desktop service shells.** `desktop_serve::run_serve_loop` is the shared
cross-OS body of **both** shipped shells — `fauna-nest-service` (the Windows SCM
service, `fauna-nest-svc.exe`) and `fauna-nest-daemon` (the macOS
`social.fauna.nest` LaunchDaemon) — so one gate covers two release artifacts.
Cargo features are not inherited from a dependency, so each shell now declares its
own `test-hooks` feature forwarding onto `fauna-nest/test-hooks`; it is forwarded as
a **feature**, never named on the `fauna-nest` dep line, so the release paths
(`release.yml` builds `fauna-nest-service` featureless; `fauna-nest-daemon`'s own
release path, `installer/macos/build.sh`, is also featureless — `release.yml` does
not build the daemon at all) carry no escape.

**The artifact witness for this half is per-OS, and is OWED rather than done.** The
standalone `fauna-nest` binary could be strings-grepped anywhere, but each desktop
shell only links `run_serve_loop` on its own platform, so only its own machine can
produce a meaningful grep:

- `fauna-nest-svc.exe` must be grepped on **Windows**. It cannot even be compiled
  elsewhere: `apps/fauna-windows/fauna-nest-service/src/config.rs:14` calls the
  `#[cfg(windows)]` `fauna_ipc::device::programdata_base()`, which is why the justfile
  excludes the crate from every workspace build. Its `[features]` block is
  declaration-verified only.
- `fauna-nest-daemon` must be grepped on **macOS**. `bins/fauna-nest-daemon/src/main.rs:453`
  is a `#[cfg(not(target_os = "macos"))]` stub that never calls the loop, so off macOS
  the linker dead-strips it and **both** flavors grep to zero — measured on Linux
  2026-08-25. A zero from a build that never linked the loop is vacuous, not a pass.

Hence the witness must always be run as a **pair** — shipped flavor grepping to 0 and
`test-hooks` flavor grepping to ≥1. The first number alone cannot distinguish "the
gate works" from "this platform never linked the code".

What the Linux-side verification did establish: both clippy flavors of
`-p fauna-nest -p fauna-nest-daemon --lib --bins -- -D warnings` are green, so the cfg
is well-formed in both directions, and the four `required-features` targets below pass
under `just nest-testhooks-check`.

Two consequences worth knowing before touching this:

- **The Windows tier_3 CalDAV/IMAP leg now needs the flavored exe.**
  `windows_caldav_nest.py` *locates* a pre-built `fauna-nest-svc.exe` rather than
  building one, so the build is a hand step on Windows, and it must now be
  `cargo-win.cmd build -p fauna-nest-service --features test-hooks`. A
  release-flavor exe would otherwise serve HTTPS while the leg dials `http://`,
  failing on a transport error naming nothing about features — so `nest_svc_exe()`
  checks the exe for the escape's string and **skips with the right build command**
  instead. That flavor-staleness check is convention 15's own artifact property
  used from the other side.
- **The installed-deployment (tier_4) path is unchanged by the gate.** No installer
  or service definition ever emitted `FAUNA_INSECURE_DISABLE_TLS`, so the MSI's
  `FaunaNest` SCM service already resolved `force_plain_http = false` by the var's
  absence; it now resolves so by construction. Same serving behavior, one fewer
  knob in the artifact.

Four in-tree Rust targets depend on the escape at **runtime** — `desktop_serve_loop`,
`desktop_serve_loopback`, `desktop_serve_port_change_loopback`,
`index_survives_nest_restart` each set the var and dial the loop they spawn over
`http://`. They now carry `required-features = ["test-hooks"]` and are named in
`just nest-test-hooks`, the `nest-testhooks-check` merge gate, and both CI
workflows. This is the population `merge-gate-catalog.md` § The heavy gate catalog called
unmeasured when it recorded the route-shaped runtime dependency: a target that needs
a feature for *anything* needs both halves, the `required-features` block and a
runner line.

## Implementation status today

**The IP bridge cert pointer in § Boot (ratified 2026-08-29) is BUILT and CA-proven as of 2026-09-02** (owner and status, including the remaining live-deployment witness: [`tls-certificates.md`](tls-certificates.md) § Implementation status today); a domainless box on a **public** address now serves a publicly-trusted IP cert to a no-SNI dial, while a NAT/LAN box still serves only its loopback-SAN floor, as it always has.

Verified against `main` after the unconditional-floor change and the
claim-authoritative-identity-domain / `FAUNA_DOMAIN`-retirement change.

**Built (build on these):**

- **Claim sets the deployment's identity domain (the primary domain IS the identity —
  single source of truth).** The handle's `@domain` is the sole determinant of the
  deployment domain. `claim_admin_core` (`bins/fauna-nest/src/claim_core.rs`) registers it
  as the primary via `mail_enable::ensure_mail_domain_registered`, which — when the row is
  the primary — calls `identity_domain_core::apply_primary_identity`
  (`bins/fauna-nest/src/identity_domain_core.rs`) to `.store()` the sync
  `AppState.identity_domain` **cache** (an `ArcSwapOption<String>` projection of the row,
  never independently authored → cannot drift). `AppState::handle_domain()` /
  `web_serving_domain()` read that cache at **top precedence** over
  `registration.handle_domain` and `config.nest.domain`, so discovery/registration, web
  apex, mail, and the TLS apex all follow the claimed domain with no restart. Boot-loaded
  by `identity_domain_core::resolve_identity_domain` in `start_server` (the primary row
  wins, else the `config.nest.domain` seed, else `None`/domainless). There is **no**
  separate identity store — the primary row is the single source of truth; a local/IP
  box registers no domain and falls back to `localhost` (below). e2e:
  `tests/e2e-unified/tests/api/test_claim_primary_domain.py::test_claim_real_domain_registers_primary_and_dns`
  (domained → primary mail domain + `handle_domain()` follows + DNS matrix) +
  `::test_claim_local_target_registers_no_mail_domain` (local → no domain, `localhost`).
- **TLS self-heals a claimed-post-boot domain (no restart).** On a domainless-booted box
  the boot floor is the loopback cert and the ACME runtime is spawned without a domain;
  the claim then (a) force-re-synthesizes the floor to cover `<domain>` + `mail.<domain>`
  + `relay.<domain>` + `pds.<domain>` via `self_signed_cert::resynthesize_floor_for_domain` (SPKI-stable —
  guards `pem_is_self_signed` so it never clobbers a trusted cert — hot-reloaded by the
  cert watcher) and (b) wakes the ACME lifecycle task (`acme_retry_notify`). The task now
  **always spawns** when ACME-capable (public axis + HTTP-01 mode + not the plain-HTTP
  escape), dropping the boot-time domain requirement (`main.rs`), and reads the apex
  **per-iteration** from `handle_domain()` with an empty/`!is_public_dns_name` guard (`acme_http01.rs`
  `cert_lifecycle_loop`), so a claim issues a trusted cert immediately. ACME account
  creation omits the contact when no email is configured (RFC 8555 — there is no
  `FAUNA_DOMAIN` to default `acme@<domain>`).
- **Unconditional, domainless-capable floor — written on EVERY entry path.** The
  always-live floor is written by the shared `self_signed_cert::ensure_floor_present`
  helper (idempotent: writes `write_self_signed_bootstrap` only when no cert exists,
  with or without a configured domain — dropping the prior `!acme_enabled && a
  domain is configured` gate). It is called from BOTH `prepare_listener_tls`
  (`bins/fauna-nest/src/main.rs`, before that path builds its own TLS listener) AND
  `start_server` (`bins/fauna-nest/src/lib.rs`, the shared init every caller hits).
  The `start_server` call is what covers nests that never go through `main.rs`'s
  `prepare_listener_tls` — the Windows `fauna-nest-service` (loopback plain-HTTP) and
  the plain-HTTP e2e harness — so their in-process mail bridges still get a floor to
  fetch (`fetch_tls_cert_blob`) and serve over CalDAV/IMAP (caldav-imap-any-locator).
  The `floor_renew_task` is spawned on every nest (it backs off without clobbering a
  CA cert). Unit-pinned: `prepare_listener_tls_serves_floor_when_domainless`,
  `prepare_listener_tls_serves_floor_first_under_acme`,
  `domainless_bootstrap_writes_a_self_signed_cert`. tier_4:
  `test_real_hostname_serves_floor_https_under_acme`.
- **`FAUNA_SELF_SIGNED` removed; no `[acme].enabled` field either.**
  `docker/entrypoint.sh` no longer reads the var or the `--self-signed` flag, and
  there is no `[acme].enabled` config field. Whether a nest *orders* is *derived*
  (`acme::build_acme_config` + `main.rs`): the **public NAT axis** AND (now, per the
  claim-domain change above) a real orderable apex known **at issuance time** rather
  than boot — the challenge listener + lifecycle task spawn whenever ACME-capable
  (public axis + HTTP-01 mode + not `force_plain_http`) and the loop skips issuance
  until the apex is real. Unit-pinned: `build_acme_config_disabled_for_localhost_domain`,
  `build_acme_config_defaults_when_unset`.
- **`FAUNA_DOMAIN` retired (domainless boot).** `docker/entrypoint.sh` no longer has
  the `$DOMAIN` block (no `[nest].domain` / `[email]` / domain-gated `[acme]` writes);
  `docker-compose.yml` / `docker-compose.home.yml` / `scripts/install-fauna-{public,home}.sh`
  no longer pass or require it; `libs/fauna-provisioning/src/cloud_init.rs` no longer
  injects it into the rendered compose. The box boots domainless and learns its domain
  at claim (above). The `[nest].domain` config field survives as test-harness
  wiring only: no shipped artifact writes it (the Linux installer's `--domain` was
  removed 2026-10-01), and the DB identity row overrides it (§ Env contract).
- **Test plain-HTTP escape (nest API listener only).** `FAUNA_INSECURE_DISABLE_TLS`
  (`prepare_listener_tls` early-return) + `conftest.py` `pytest_sessionstart` makes
  the **nest's own API listener** serve plain HTTP. It does NOT skip the floor: the
  `start_server` `ensure_floor_present` call still writes it so the mail bridges
  serve CalDAV/IMAP TLS off it. Unit-pinned:
  `prepare_listener_tls_force_plain_http_skips_nest_tls`.
- **Claim accepts a bare handle / `mail_domain: None`.** `fauna.auth.claim_admin`
  and `claim_admin_core` register a domain only when one is supplied.
- **Local-target access addresses are NOT registered and do NOT set the identity.**
  `claim_admin_core` (`bins/fauna-nest/src/claim_core.rs`) registers a mail domain —
  and, through that registration, sets the identity via
  `identity_domain_core::apply_primary_identity` — **only** when the claim's
  `mail_domain` is a real, `is_public_dns_name` domain; it skips `ensure_mail_domain_registered`
  entirely when the target is a local one (IP literal / `localhost`/`*.localhost` /
  `.local` / `host:port`), using the same
  `fauna_provisioning::probe::resolve_handle_domain(d).is_public_dns_name` classifier the cert
  self-heal + nets share (the gated `if let Some(d) = mail_domain && …is_public_dns_name` at
  `claim_core.rs`). So reaching a domainless nest at `alice@<IP>` claims handle-only,
  registers **nothing**, and leaves `handle_domain()` at the `localhost` fallback
  (`identity_domain` stays `None` — the access address is **not** persisted; a LAN box
  is reached by IP and TOFU-pinned by the nest key, not its domain), fixing the home2
  symptom (no nonsense `mail.<IP>`); a real domain (`alice@example.com`) both registers
  the primary mail domain and sets the identity. This matches § Claim above and is
  regression-pinned: `storage_mode_api.rs`
  `claim_at_local_target_does_not_register_a_mail_domain` (asserts nothing is registered)
  + `..._with_real_domain_registers_it`.
- **Adding the first domain from an app sets the identity too (the
  domainless-then-add-domain flow).** `add_local_domain_handler`
  (`bins/fauna-nest/src/bridge_routing_handlers.rs`), the
  `fauna.bridges.add_local_domain` handler, calls
  `identity_domain_core::apply_primary_identity` when the added row is the first
  (primary) domain **and** a real (`is_public_dns_name`) target — so a box reached by IP,
  claimed with a **bare handle** (identity `localhost`), then given its first domain via
  Admin→DNS gains the identity apex + a trusted ACME cert with no restart, the same way
  the claim path does. It writes the `mail_domains` row **directly** (not via
  `ensure_mail_domain_registered`) so the client's MTA-STS / catch-all / DKIM-selector
  picks survive. Unit-pinned: `add_local_domain_creates_primary_then_lists` (asserts
  `handle_domain()` flips `localhost`→added domain on the primary add, and stays put on a
  2nd, non-primary add). tier_4:
  `test_domainless_add_domain_acquires_acme_cert_and_serves_mail`.
- **A cert that lands under a *running* mail bridge is re-served promptly (no 12 h wait).**
  A post-claim `add_local_domain` (or a claim on an already-serving box) makes ACME issue a
  cert covering the new `mail.<domain>`, but a *running* MDA/MTA otherwise re-fetches its TLS
  blob only on a 12 h timer — so the trusted cert would sit on the nest, unserved, until that
  timer / a restart. `cert_lifecycle_loop` now fires a `config_changed`/`"tls"` push after a
  successful ACME issue, and `provision_self_signed_cert` fires one too (and wakes the ACME
  lifecycle so an ACME-on box's interim self-signed re-heals to trusted at once); both bridge
  roles register a `configReloader` applier that re-runs `fetch_tls_cert_blob` on it
  (seal-on-read hands back the cert on disk). So the IMAPS/993 + submission listeners swap to
  the trusted leaf within seconds. Mechanism + coverage:
  [`../../behavior/mail-bridge-lifecycle.md`](../../behavior/mail-bridge-lifecycle.md)
  § *`fauna.bridges.config_changed` push* / § TLS provisioning; Go contract test
  `internal/tls` `TestConfigReloaderTLSApplierRefetchesCert`. **Root-caused + FIXED
  (2026-07-02 — the ACME reused-authorization fix):** the deployer's
  re-run pinned assert 4 red, and instrumenting the tier_4 test (nest's *own*
  HTTPS leaf + `cert_status`) showed the failure was **not** a listener-rebind gap at all —
  the nest's own HTTPS *also* stayed self-signed (`is_floor == true`), i.e. the self-heal
  silently failed. `bring_bridges_to_serving`'s `provision_self_signed_cert` clobbers the
  trusted cert to self-signed, and the re-heal's ACME re-issue — running inside the CA's
  **authorization-reuse window** — was rejected: `obtain_certificate` blindly re-POSTed
  `set_challenge_ready` on an authorization the CA already held as `valid`
  (`Cannot update challenge with status valid, only status pending`), so no trusted cert
  re-landed and *both* the nest HTTPS and the MDA IMAPS/993 kept serving the self-signed
  `CN=added.test` leaf. The MDA seal-on-read + `config_changed`/`"tls"` push mechanism
  was sound — this was the missing half. **Fix:** `obtain_certificate` now
  skips `set_challenge_ready` for already-`valid` authorizations and finalizes on them
  (aligning the nest HTTP-01 path with the already-proven client DNS-01 path
  `fauna_client_dns::acme_order`, which handled reuse since D6).
  **CONFIRMED LIVE (deployer, 2026-07-10):** assert 4 **XPASSED** on the first
  image ≥ the fix (`sha-dd9dbd60f…`), so
  `test_domainless_add_domain_acquires_acme_cert_and_serves_mail` (in
  `test_domainless_add_domain_acme_serves_mail.py`) now runs as a plain live
  tier_4 test — the `xfail(strict=False)` marker is deleted.
- **Web-content / subdomain routing AND per-subdomain cert issuance follow a post-boot
  claim (no restart) — the web apex is live end-to-end.** `HostResolver.nest_domain` is
  an `ArcSwap<String>` (was a read-once `String`);
  `identity_domain_core::apply_primary_identity` calls `HostResolver::set_nest_domain`
  right after the `identity_domain` swap, so a domainless-booted box claimed / given its
  first domain **post-boot** resolves its `<handle>.<domain>` subdomains and evaluates
  the reserved-host guard against the new apex immediately, no restart — the same
  live-swap the `identity_domain` cache already did for `handle_domain()`. The cert half
  reads the SAME live handle: `web_cert_lifecycle_task` takes the router's
  `Arc<HostResolver>` as a task param (not a `WebCertConfig` field — the resolver's
  `RwLock`s would break the config derives) and re-reads `nest_domain()` each tick, so
  per-subdomain HTTP-01 issuance for `<handle>.<domain>` starts on the first tick after
  the claim — routing and issuance can never disagree on the apex (this also retired
  `main.rs`'s boot-time capture, which missed the resolved identity domain the
  `HostResolver` seed includes). Unit-pinned:
  `web_content::serve::tests::host_resolver_nest_domain_updates_live`,
  `identity_domain_core::tests::apply_primary_identity_updates_web_host_resolver_domain`,
  `web_content::cert::tests::subdomain_issuance_follows_post_boot_domain_claim`.
- **Port-80 redirect echoes the request host.** The HTTP-01 challenge listener's
  fallback redirect (`http01_router`) targets the request's own `Host` (port-stripped),
  falling back to the boot-time domain only for a `Host`-less request and answering 400
  when both are absent — so a domainless box's pre-claim window redirects correctly, and
  a custom web domain / subdomain bounces to *its* https origin instead of the apex. The
  ACME challenge route itself is token-based and unaffected.

**Unbuilt (remaining scope):**
- ~~**`tui` host-address reporting.**~~ **Done (2026-07-29).** All 7 apps now
  call `report_host_address` from a post-auth admin-gated hook; tui's is
  `admin::spawn_host_address_report`, fired from the `GateLoaded { is_admin:
  true }` fold, mirroring linux's `App::report_host_address` at
  `AdminStatusLoaded`. No new logic — the shared decision fn is unchanged.
- ~~**Domainless deploy guide.**~~ **Done (2026-07-07).** The internal dev-setup notes
  lead with the domainless-boot → claim-by-handle flow, and
  `docs/guides/nest-home-setup.md` now leads IP-first — run it → reach it at
  `https://<ip>/app/` → claim `you@<ip>` (handle-only; local target registers no
  domain) → CalDAV direct at `<ip>:8443` (host networking + `FAUNA_LAN_BIND_IP`, per
  `../../behavior/caldav-server.md` § Network exposure any-locator serving) — with the
  owned-domain / hosts-file flow demoted to an appendix (use-a-name-from-day-one) plus
  the add-a-domain-later path via `admin-dns`.
- **Phase 2 (separate track): multi-domain handle parity + deletable primary — nest
  side landed; per-app rollout owned by the two features docs.** Nest side:
  registration accepts a signature over any active local domain, `by_handle` echoes a
  `domain` qualifier, and secondary apexes get client-reachability DNS
  ([`../../behavior/mail-multidomain.md`](../../behavior/mail-multidomain.md)
  § Implementation status owns the current per-app find-user/compose-display rollout).
  The promote-then-demote primary-domain-rename state machine (SLICE 1–4) landed
  2026-07-06
  ([`../../behavior/mail-primary-domain-rename.md`](../../behavior/mail-primary-domain-rename.md)
  § Implementation status owns the current per-app rollout). **The `tui` gap this
  bullet used to track is closed:** `tui` gained its `admin-dns` page — including the
  rename sheet + banner — 2026-07-29 (`apps/fauna-tui/src/admin/dns.rs`), landing the
  rename wizard UX on **all 7 apps**; only the audit-log rows remain (per
  mail-primary-domain-rename.md § Implementation status).

## Relationship to neighbouring docs

- [`tls-certificates.md`](tls-certificates.md) — cert mechanics (floor key, per-SNI,
  ACME tiers, DANE/MTA-STS). This doc owns *when* the floor is written (always) and
  the domainless SAN shape; that doc owns *how*.
- [`public-mode.md`](public-mode.md) — registration, handle resolution, first-admin
  bootstrap.
- [`onboarding.md`](../../behavior/onboarding.md) — the app wizard, local-target
  handling (§ 2), the claim page + NAT-mode choice (§ 3a/§ 3b-bis; the
  storage-mode page is retired — § 3b/§ 3c).
- [`mail-multidomain.md`](../../behavior/mail-multidomain.md) — adding/removing local
  domains post-claim.
- [`../installers/docker.md`](../installers/docker.md) — the container env contract.
