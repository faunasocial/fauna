# Onboarding app target state

Owns: onboarding, admin-claim
Status: ratified — **partitioned by concept 2026-09-06**: provisioning a box (§§ 4-7, the reach hint and the "Almost ready" surface) moved to [`onboarding-provisioning.md`](onboarding-provisioning.md); what stays is joining a nest (§§ 1-3c), the wizard API and exit routing, app-launch routing, the store slots and the e2e bridge contract
Authority: the onboarding wizard — the seven-page + branch flow (identity, handle entry incl. local-target classification + SRV port discovery, invite request, the § 3a admin-claim transaction, the NAT-mode setup branch (and the retired encryption-mode / plaintext-consent branches, §§ 3b/3c), ), the wizard API + exit-outcome routing, app-launch routing incl. the silent-challenge fallback table, the long-term-store pending-invite + awaiting-manual-dns slots and the e2e machine-bridge contract. **NOT owned here either — split 2026-09-06:** the **DNS / VPS / provisioning and manual-DNS pages (§§ 4-7)**, **how a freshly-provisioned box is reached and claimed** (the reach address, provisioning = build + claim, the pending-provision slot), the **post-claim reach hint** and the **"Almost ready" surface** → [`onboarding-provisioning.md`](onboarding-provisioning.md). Defers: the auth ceremony → [`login.md`](login.md); the three-slot store + account registry → [`../architecture/long-term-store.md`](../architecture/long-term-store.md); registration modes/ceremony/handles → [`../architecture/nest/public-mode.md`](../architecture/nest/public-mode.md); storage-mode semantics → [`../architecture/nest/storage-modes.md`](../architecture/nest/storage-modes.md); VPS/cloud-init mechanics → [`../architecture/installers/vps.md`](../architecture/installers/vps.md); crash-recoverability → [`../architecture/nest/common.md`](../architecture/nest/common.md) § Client-state recoverability; element IDs + per-page scope → `tests/e2e-unified/ui.yaml`. (Provenance, not living authority: the 2026-04-27 UX plan + 2026-04-28 design spec — frozen work-product.)

---

Partitioned by concept on 2026-09-06, when this doc reached **255,816 B** — ~3 days from the 262,144 B whole-file read ceiling. The wizard already drew the line: pages 1-3c are *joining* a nest, pages 4-7 are *provisioning a box*, and every byte of the week's growth was on the provisioning side. Those pages, the reach hint, the "Almost ready" DNS-pending surface and their status-ledger entries went to [`onboarding-provisioning.md`](onboarding-provisioning.md), which owns the `reach-hint` concept from the same date. A routing stub sits at each original location, so every `§ 6`, `§ Reach hint` and `§ "Almost ready" surface` citation still resolves in one hop.

## Goal

The app is a thin observer-driven shell over `OnboardingMachine`. The machine owns all decision-making; the app renders snapshots and forwards user gestures. Persistent identity and pending-invite data live in the app's existing long-term store. The app does not hold per-stage view-model state, does not duplicate machine logic, does not carry platform-specific routing rules.

The wizard is **seven primary pages plus a claim-code branch and one setup branch (the NAT mode choice on admin claim)**. The app implements all of them. The same element IDs are used across every app. The claim-code branch is reached only when the wizard determines the target nest is unclaimed (no admin yet); it is mutually exclusive with `invite_request`.

---

## The pages

### 1. Identity

Three screens behind one step: `identity_choice`, `identity_created`, `identity_import`.

- `identity_choice` offers Create / Import.
- `identity_created` shows the generated secret and asks the user to copy it.
- `identity_import` accepts a pasted secret or a scanned QR. QR payload is `(identity, handle)`. When a handle is present in the payload, the app calls `set_current_handle(handle)` before navigating to step 2 so the input pre-fills; if absent, step 2 starts empty. *Implementation status: the import-field grammar (bare 64-hex secret · `fauna://identity?secret=&handle=` query form · colon form) is the shared `fauna_core::identity_qr` parser, exposed over UniFFI (`parse_identity_import`) + WASM (`parseIdentityImport`); the encoder `IdentityQr::to_uri(secret, handle)` writes the `(identity, handle)` payload so the parsers' handle branch is real end-to-end. **Adopted by all seven apps** (web, android, iOS, macOS, linux, windows, tui): a parse failure surfaces the localized `onboarding.identity_import.invalid_secret` without touching the machine. linux and tui call `fauna_core::identity_qr::parse_import_input` directly (native Rust, no FFI); the other five go through the `parse_identity_import` UniFFI / `parseIdentityImport` WASM face. `test_invalid_secret_shows_i18n_error` asserts the parse string for every native app.*

The app calls `confirm_generated_identity()` / `confirm_imported_identity(secret)` when the user commits. Both return the secret hex on success. The app writes the secret to its long-term store immediately on return — this is the durable commit point.

**Which identity the wizard is acting as — `identity_origin` decides, and it decides ONCE for everything (ratified 2026-08-28).** The machine holds two secret slots, and **neither screen clears the other's**: a user who taps Create, looks around, goes Back and then pastes the key they actually came with has both, and no screen may destroy a secret another screen may have just told them to write down. So precedence, not clearing, is what resolves it, and the rule is **the screen the user committed on wins** — `Created` → the generated secret, `Imported` → the imported one — with the *other* slot as the fallback for a screen entered but never confirmed (`identity_import` sets the origin on entry, before anything is pasted, so "origin says Imported, slot is empty" is an ordinary Back-out state, not an error), and the `seed_identity` path's deliberate `None` origin reading the imported slot it seeds. This is the fresh-box case and the returning-user case at once: claiming a new box with a brand-new identity keeps generated-wins because Create is what was committed on, while post-import the brought-in key is canonical.

**When the origin is written — entering a screen never commits (ratified 2026-08-30).** "The screen the user committed on" is only a rule once it says *when* each screen writes the origin, and the two write it at opposite moments because they fill their slots at opposite moments. `identity_import` writes `Imported` on **entry** and fills its slot at `confirm_imported_identity`; `identity_created` fills its slot on **entry** — it must, the screen's whole job is to display a key — and writes `Created` at `confirm_generated_identity`. Import can afford the early write because the empty slot it leaves *is* the tell that nothing was committed, and the fallback arm above reads it. Create has no such tell: its slot is full the moment the screen opens, so an entry-time origin is indistinguishable from a real commitment, and a user who pasted their real key and then tapped "create" to look around would have it displaced by the two-tap detour — every authenticating call signing as the throwaway (so a registered user is told they are not registered) and the terminal persisting the throwaway as the installation's long-term identity (§ Long-term store contract). The invariant both routes buy, and the one to preserve if either screen's shape changes: **entering a screen never changes which identity is canonical; only confirming on it does.** *Implementation status: BUILT 2026-08-30 in `libs/fauna-onboarding-machine` — `begin_create_identity` no longer writes the origin, `confirm_generated_identity` does. Pinned by `tests/identity_precedence.rs::abandoning_the_create_screen_keeps_the_imported_key`, the mirror of the import-side `abandoning_the_import_screen_falls_back_to_the_created_key`, red-verified against the pre-fix code. Shared Rust, so all seven apps inherit it; no app reads `identity_origin` directly.*

**The create screen mints once per wizard run.** `begin_create_identity` mints only into an empty slot, so re-entering `identity_created` after Back re-shows the **same** key: a user who was told to write the phrase down, went back, and returned is never silently looking at a different one — confirming the second key would make the written-down one worthless, and the identity secret has no escrow behind it at that point. This is the rule the recovery-kit root already follows one function away (`confirm_generated_identity` mints a `pending_recovery_secret` only when none exists). `reset()` — "start over", factory reset, the E2E reset between tests — clears the slot, so a genuinely new run does get a new key. *Implementation status: BUILT 2026-08-30, pinned by `tests/identity_precedence.rs::re_entering_the_create_screen_reshows_the_same_secret`.*

**One rule, one accessor, every caller.** `effective_secret()` is that rule's only face: it is what the app persists at the wizard terminal (§ Long-term store contract) **and** what every authenticating wire call signs with — the handle-check silent challenge (§ 2), the invite submit / recheck / redeem, the admin claim, the manual-DNS claim, the NAT-mode commit. The terminal's premise that "the wizard has just authenticated with the identity, so `effective_secret()` always has it" is only true while those are the same value, so a second precedence accessor is a defect by construction, not a style choice. *Implementation status: `State::canonical_secret` in `libs/fauna-onboarding-machine`, read by all eight authenticating call sites and by the public `effective_secret()` / `effectiveSecret` (UniFFI + WASM). Pinned by `tests/identity_precedence.rs`, which drives the real handle-check phases and asserts the secret that reaches the wire, not the accessor's return — a wrong-but-valid key is invisible in the outcome (the nest just answers `NotRegistered`). Before 2026-08-28 a second private accessor held the inverted precedence and every authenticating call read it, so a create-then-import user was silently challenged as the throwaway key and told "you're not registered" while holding a registered identity.*

The **export** counterpart — the Settings/Account affordance that *shows* the identity QR a second device scans here — is owned by [`../ui/settings.md`](../ui/settings.md) § Identity export, not by this doc; the wizard has no export page.

**Ratified 2026-07-23; screens + IDs ratified 2026-08-01; built on tui (2026-08-01/02), linux, web, macOS/iOS and windows (2026-09-26) and android (2026-10-06) — see the per-bullet status below.** Stage 1 gains two pages, both owned as *screens* here while the kit, the escrow, and every ceremony behind them stay owned by [`identity-succession.md`](identity-succession.md):

- **`recovery_kit`** — the follow-on to `identity_created`: strongly encouraged, skippable, showing the RecoveryKey secret once in the same hex+QR idiom as the identity secret (`recovery-kit-secret-display` / `-secret-copy-btn` / `-qr` / `-description`), plus `recovery-kit-escrow-status`. **The transition changed with it:** `identity-continue-button` now advances to `recovery_kit`, not straight to `handle_entry`; `recovery-kit-confirm-button` and `recovery-kit-skip-button` both land on `handle_entry`, so skipping costs one click and never blocks onboarding. Skipping leaves the standing Settings warning ([`../ui/settings.md`](../ui/settings.md) § Recovery kit).
  - **The screen only mints and displays — the ceremony is deferred (user-ratified 2026-08-01).** At this position no nest exists yet (the nest is chosen at `handle_entry` or later), so registration and the escrow `put` cannot run here; they run at the wizard's signed-in handoff, per the timing rule [`identity-succession.md`](identity-succession.md) § The RecoveryKey → *Creation UX* owns. `recovery-kit-escrow-status` therefore renders exactly one state on this screen — the deferred line ("your kit activates when your account comes online") — never an `EscrowOutcome`, and **must never imply the account is already protected**. A handoff whose `put` fails, or a wizard exit that never signs in, degrades to the Settings status truth (registered-no-escrow, or the standing never-created warning — [`../ui/settings.md`](../ui/settings.md) § Recovery kit), so a written-down phrase can be inert but the app never claims it is active.
- **`recovery_entry`** — the phrase-only restore, reached from `identity_choice` via `restore-from-recovery-kit-button`: `recovery-entry-phrase-field` (accepting the `fauna://recovery` URI or a bare 64-hex secret through the shared `fauna_client_recovery::parse_kit` grammar — the deliberate twin of `identity_qr` on its own host, so a recovery secret is never taken for an identity secret), `recovery-entry-account-field` (the account's **handle**, `user@domain` — needed **only** when the payload does not carry one), `recovery-entry-submit-button`, `recovery-entry-back-button`, reusing `qr-camera-view`. On success it restores the identity seed and lands on `handle_entry`, uniform with `identity_import` — the seed is restored, so from there it behaves exactly like an import.
  - **The account field asks for a handle, not an actor id, because the handle is the only half that locates a nest.** The ceremony is pre-identity: there is no session to ask where the account lives, so the home nest is resolved from the handle's `@domain` through the same probe path `handle_entry` uses (SRV port discovery included), and the actor id is then taken from the payload's `actor=` when it carried one, else resolved on that nest via `fauna.actor.by_handle`. A kit whose payload carries `handle=` (the Settings-minted one — [`identity-succession.md`](identity-succession.md) § The RecoveryKey → *Kit payload*) therefore restores with nothing typed but the phrase; the onboarding-minted kit and a hand-typed 64-hex secret carry no handle and the field is what supplies it. A payload that names an actor but no handle is **not** self-sufficient — it says which account, never where — so the field is still required there.
  - **The locator of last resort is a direct nest address (ratified 2026-08-11; built — status below).** The account field also accepts `handle@<direct nest address>`, where the address part is any target the shared probe classification already recognizes (`fauna_provisioning::probe::resolve_handle_domain` — a public DNS name, an IP literal, `localhost`, a `.local` name, with an optional port). This exists for exactly one case, and it is the case recovery was built for: the handle's domain is **dead** — lapsed or seized ([`../architecture/nest/domains-and-tls-bootstrap.md`](../architecture/nest/domains-and-tls-bootstrap.md) § Domain loss) — so the ordinary domain-derived locator fails while the escrow blob, the actor, and the nest are all intact. The contract splits the field's two jobs: the **address part locates** (probed verbatim, exactly as `handle_entry` already probes a LAN target), and the **handle part names** — actor resolution passes the *bare* handle, which is the deployment-wide identifier (`mail-primary-domain-rename.md` bar 1), never the typed `handle@address` compound. A user who knows their phrase and where their box lives thereby recovers with the namespace gone. Nest-key trust on that first connection is the same TOFU the pre-identity ceremony already makes (no domain means no WebPKI anchor to prefer); the phrase, not the transport, is what authorizes.
  - Two refusals the screen must distinguish rather than collapse into one failure: `fauna.recovery.no_escrow` — no blob exists for this account, so phrase recovery is unavailable and the kit must be re-created from a signed-in device; and `fauna.auth.superseded` — the identity was succeeded, which routes to the identity-import flow naming the successor, uniform with every other superseded refusal ([`succession-propagation.md`](succession-propagation.md) § Propagation). *Naming the successor waits on verification: the refusal's claim is unverified, so until `resolve_successor` has checked it against the registration chain the routed-to page says what happened and what to do, and asserts nothing about who the successor is.*
  - *Implementation status: BUILT in the shared machine, 2026-08-02 — `OnboardingMachine::submit_recovery_entry` parses the phrase (`fauna_client_recovery::parse_kit`), picks the account (typed field over the payload's `handle=`), resolves the home nest from that handle's domain through the same local/loopback classification + SRV port discovery the handle-check probe uses, and drives `NestApi::restore_escrowed_seed` — `fauna.actor.by_handle` when the kit named no actor, then `restore_seed`, all on one pre-identity connection. **The direct-address fallback (the bullet above) is BUILT and pinned** — corrected 2026-08-12, having been declared UNBUILT here on 2026-08-11 when in fact both of its halves shipped with the ceremony on 2026-08-02: the `@`-suffix is passed verbatim to `fauna_provisioning::probe::resolve_handle_domain_with_local_port` (so an IP literal, an IPv6 literal bracketed for the URL authority, `localhost`, a `.local` name and an explicit port all locate), and `handle_local_part` is what reaches the wire, so actor resolution has always received the bare handle. The nest-side matching that was flagged unverified is verified: `fauna.actor.by_handle` resolves through `resolve_handle`'s exact match on `users.handle`, which stores the bare handle, and the app sends `domain: None` so the compound never reaches the active-domain check — **no nest-side change is owed**. Pins: `libs/fauna-onboarding-machine/tests/recovery_entry_ceremony.rs` (`a_direct_*_locates`, mutation-graded — the IPv6 and injected-loopback-port cases are the two that discriminate the classification from a naive `https://{typed}`) and, for the naming half against a live nest, the tier_3 `test_recovery_kit_restore.py`, whose account field already types the direct-address form. Success sets `imported_secret` and lands on `HandleEntry`, byte-for-byte the landing `confirm_imported_identity` produces. The outcome is a typed `RecoveryEntryOutcome`, and its `onboarding.recovery_entry.*` message is ONE shared table — `RecoveryEntryOutcome::message`, UniFFI `recovery_entry_outcome_message` — that every app resolves and none re-derives (`Superseded` alone routes rather than speaks); the machine writes no message of its own. The kit screen's QR/copy payload is likewise shared (`OnboardingMachine::recovery_kit_uri`), and so is the handoff's registration of the confirmed root (`fauna_client_recovery::ceremony::register_deferred_kit`, UniFFI `recovery_register_deferred_kit`, wasm `recoveryRegisterDeferredKit`). The account field is forwarded as shown, empty included, so a handle an earlier flow left on the machine never rides along unseen. **Rendered and reachable on tui (2026-08-02), linux, web, macOS/iOS and windows (2026-09-26)** — both screens, the entry CTA and the deferred handoff registration (apple: the shared FaunaKit `RecoveryKitOfferView` / `RecoveryEntryView`, the kit registered by the post-auth launch glue over UniFFI `recovery_register_deferred_kit`, restored predecessors through `FfiAccountRegistry.persistRestoredPredecessors`; windows: `RecoveryKitView` / `RecoveryEntryView` over the same exports, the kit registered and the predecessors persisted at `OnboardingViewModel`'s LoggedIn terminal) **and android (2026-10-06)** (`RecoveryKitScreen` / `RecoveryEntryScreen`; `OnboardingHost` declares the capability, persists restored predecessors at its LoggedIn terminal — or, adding an account, `AccountSettingsVM.completeAddAccount` between the add and the switch — and latches the confirmed root, which `PostAuthGlueVM.registerDeferredRecoveryKit` registers over UniFFI `recovery_register_deferred_kit` at the post-auth hook). Android's evidence is host-side Robolectric pins (`RecoveryKitScreenTest`, `RecoveryEntryScreenTest`, `RestoredPredecessorsPersistTest`); no android e2e run exists for any journey yet.*

⚠ `identity_choice` now carries **two** recovery entries that mean different things: `recover-lost-box-button` restores a lost *nest* ([`../architecture/nest/box-recovery.md`](../architecture/nest/box-recovery.md) § Restore) and `restore-from-recovery-kit-button` restores a lost *identity*. Their labels must differentiate sharply — the IDs already do. **A third, optional entry and two steps were ratified 2026-09-25 and are owned elsewhere:** `emergency-lockout-entry-button` → the step `emergency_lockout_entry` (the signed-out 24-hour lock door, persisting nothing), and the step `identity_stolen_entry` (where a locked launch surface sends the owner to run the stolen-identity ceremony) — both specified, IDs included, by [`../ui/sessions.md`](../ui/sessions.md) § Layout & flow, behavior by [`devices.md`](devices.md) § The locked state + § The signed-out door; unbuilt on every app.

### 2. Handle entry (`handle_entry`)

One row: text input + Check button. Below: an auto-sizing message panel. Below that: a hidden checkbox ("I control this domain"). Bottom row: Back, Continue.

The page is rendered from `handle_check_snapshot()`:
- `phase` ∈ `{Idle, Parsing, DnsLookup, NestProbe, ChallengeResponse, PriceLookup, Complete}` — drives the in-flight indicator.
- `outcome` ∈ `{None, FormatInvalid, TldInvalid, DomainAvailable{...}, RegisteredNoNest, AlreadyOnNest{...}, NestRunningUserUnregistered, UnregisteredUnclaimedNest, ProbeError{...}}` — drives Continue routing and message text. `UnregisteredUnclaimedNest` is the unclaimed-nest discriminator (the nest is reachable but `setup-status.claimed == false`); routes Continue to `claim_code` instead of `invite_request`.
- `message: LocalizedText` — `{key, args}` pair; the app looks up the i18n string for `key` and substitutes `args`.
- `continue_enabled: bool`
- `control_checkbox_visible: bool` — the hidden "I control this domain" checkbox.
- `control_checkbox_checked: bool`

Click handlers:
| Element | Action |
|---|---|
| `handle-check-button` | `start_handle_check(handle_input.text)`. Re-pressing while a probe is in flight cancels and restarts (the machine handles cancellation). |
| `handle-control-checkbox` | `set_control_checkbox(checked)` |
| `handle-entry-continue-button` | `submit_handle_check_continue()` returns the next `OnboardingStep`. Navigate to it. If the result is `Done`, query `wizard_outcome()` and follow the exit-routing table below. |
| `handle-entry-back-button` | Standard Back. |

Check stays disabled while the input is empty. Continue stays disabled while `continue_enabled == false`.

**Local / self-hosted targets ("trying out the app" / Pi on the LAN).** A handle whose domain is a *local target* — `localhost`, any `*.localhost` subdomain, an IPv4/IPv6 literal, a `.local` mDNS name (`pi.local`), or any of those with an explicit `:port` — denotes a nest the user is running themselves, not a registerable domain. For these the check **skips the DNS NS lookup, the TLD-registration check, and the price probe** (none apply) and goes straight to the nest health + challenge probes; the normal `AlreadyOnNest` / `NestRunningUserUnregistered` / `UnregisteredUnclaimedNest` / `RegisteredNoNest` outcomes follow. The domain resolves to a probe base URL via a shared helper (`fauna_provisioning::probe::resolve_handle_domain`). A nest is reached the **same way regardless of app origin** — `https` with the port hidden (`:443`) — whether the host is `localhost`, a LAN IP, a `.local` name, or a public domain (a self-signed cert is authenticated via the channel-binding trust model in `docs/goal/architecture/security.md` § Transport trust, **not** a public CA; this mirrors the network-reachable nest in `docs/goal/architecture/installers/windows.md` § Network-reachable nest, which binds `0.0.0.0:443` and is reached on `:443` from localhost / LAN / WAN alike). **The nest-health probe accepts a non-WebPKI cert for every target class, public registrable domains included** — it is a *reachability* check that carries no bearer and no secret, and the anonymous WS-RPC leg that follows it already accepts the cert provisionally and authenticates it by channel binding, so the probe matches that posture instead of applying a stricter one. This is not an edge case: an internet nest boots **domainless** and learns its name *from the claim* (`docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Boot / § Claim sets identity), so it is necessarily serving its self-signed floor at the moment the admin claims it — no ACME cert can exist yet for a name the nest does not yet know. Probing a public domain with a strict-WebPKI client therefore made the handshake fail, collapse to `ConnectionRefused`, and report `RegisteredNoNest`, whose control-checkbox UX ("I control this domain, set a nest up") is a dead end — i.e. **no native app could claim a fresh internet nest at all**; only the browser got through, because clicking past its cert interstitial grants that origin an exception. (Found on the first real VPS walkthrough, 2026-07-24.) The web app keeps the strict client regardless, since the browser owns TLS in wasm. An **explicit `:port`** is honored on `https` for **every** host-class (`localhost:3000`, `127.0.0.1:8080`, `[::1]:8443` → `https://…:port`); the scheme no longer depends on whether the host is loopback (Pillar C of the serving-ports plan — `resolve_handle_domain` stopped guessing `http` from host-class). A **bare loopback handle with no port** resolves to `https://host` (`:443`, port hidden) like any LAN IP — it does **not** assume `:3000`. A plain-HTTP dev / test nest (a standalone `fauna-nest` under `FAUNA_INSECURE_DISABLE_TLS`, default `--bind 127.0.0.1:3000`) is reached **not** by a host-class scheme guess but by the **same-box `nest_url` injection** the desktop app already uses in production: the onboarding machine's injectable nest override (`provider_base_urls["nest"]` for the full URL, or `local_nest_port` for a bare-loopback port) points the probe at the app's *actual* local nest scheme+port, while `state.nest_url` still records the resolved URL. The tier_3 e2e harness reuses that same override seam to reach an ephemeral plain-HTTP test nest. A `.local` name resolves to an address via OS-level multicast DNS at connect time (avahi / Bonjour) — `resolve_handle_domain` only classifies it as a direct-connect target so it skips the unicast-DNS registration probes that would otherwise fail (`.local` has no registrable TLD). **Non-local (registerable public) domains** resolve to `https://{domain}` with the port hidden, then the onboarding machine consults the **`_fauna._tcp.<domain>` SRV record** so a public nest serving its client-facing API on a **non-standard port** stays reachable by a clean, port-hidden `alice@domain` handle — the port is *discovered*, never typed (Pillar B of the serving-ports plan; `docs/goal/architecture/nest/common.md` § Serving ports). When the SRV record is absent (or advertises `:443`) resolution is unchanged (`https://{domain}`); when it advertises another port `state.nest_url` becomes `https://{domain}:{port}`. The lookup is **DoH-based** (`fauna_provisioning::probe::fauna_srv_port` over Cloudflare `dns-query`, so it works in the wasm web app and the native apps alike — unlike the native-only hickory `fauna_core::resolve::resolve_node_url`) and fires only for a handle with **no explicit port** (an explicit `:port` is the user's own override, honored as-is). It mirrors `fauna_core::resolve::resolve_full_url` (already used by federation + `fauna.nest.resolve`): SRV supplies only the *port*; the host stays the handle's domain. The resolved base URL is stored as `state.nest_url` so the continue-routing reuses it instead of re-deriving `https://{domain}`. The handle-entry hint (`onboarding.handle.localhost_hint`) advertises this path. *(Web caveat: when the SPA is served from a different origin than the nest — e.g. the `just web-dev` two-port split — the in-browser probe is cross-origin and the nest must allow it via `--cors-origin`; in the same-origin production deployment, where the nest serves the SPA, no CORS is needed.)*

**Nest hint (ratified 2026-09-24; built 2026-10-05 in the shared machine — `OnboardingMachine::set_nest_hint`, which also gates on `fauna_core::web::is_domain_authority_syntax` so a hint naming more than one authority never reaches the classifier — and on web, from the `nest` query parameter; no native deep link carries it yet).** An app may open with a *nest hint* — on web the `nest` query parameter that a nest's central-origin redirect carries ([`web-content-hosting.md`](web-content-hosting.md) § Same-origin security model → *The nest-served `/app/` and the central origin*), later a native deep link — which pre-fills this page's domain part with a target `resolve_handle_domain` classifies (a public domain, a direct nest address, with or without a port). It is a prefill and nothing more: the check, the probe and the trust ceremony run exactly as for a typed domain, it is ignored once an account is signed in (§ App-launch routing runs first and never consults it), and a hint that does not classify is dropped silently rather than surfaced as an error — the user just sees an empty field. Parsing and the drop rule live in the shared onboarding machine so every app applies the same rule; no app reads the raw hint into its own state.

### 3. Invite request (`invite_request`)

Two independent rows on one page.

**Top row** (admin-flow): submit button + status display + recheck button. Recheck is visible only during `PendingReview` state. During `PendingReview` the page **auto-polls** `recheck_invite_status()` on a client-owned cadence — see § The pending-invite surface below; the manual recheck button stays as the impatient user's affordance.

**Bottom row** (out-of-band code): code input + check button + status display. *(Forward-pointer: `register_core` already accepts a **payment claim code** minted for a nest membership tier as a fallback when the invite-code lookup misses (`monetization.md` § Pillar 4, built 2026-07-23), but the check this row calls first (`fauna.account.invite_code.verify`) only peeks the invite-code table, so today it rejects a payment claim code before that fallback is ever reached — a small remaining nest-side extension, not an app change. Rendering the nest's membership-tier offering on this page is a separate, still-unbuilt Pillar-4 app slice; behavior + data owned there.)*

Bottom: Back + Continue.

*(Forward-pointer, ratified 2026-08-22 — unbuilt: on ios/android the admission flows gain a store age-signal + platform-attestation step — corroboration carried into the admission call, never the enforcement point; owner [`family-safety.md`](family-safety.md) § The account age band. Its element ID is `invite-request-age-notice` — a `platform_elements` entry for android and ios (user-approved 2026-09-25) rendering the store round's outcome before submit/redeem, text derived in the shared onboarding machine (`invite_request_snapshot().age_notice`); the five other apps declare the absence.)*

The page is rendered from `invite_request_snapshot()`:
- `state: InviteRequestState` ∈ `{Idle, Submitting, Rechecking, Denied{reason, request_id}, PendingReview{request_id, last_checked_ms}, Error{transient, context, cause}}` — the former `Approved{quota, request_id}` variant is **retired** (2026-08-11): an admin approve *deletes* the request row after creating the account (`admin.md` § Section 1), so no live nest ever serves `status: "approved"` — approval is detected as *admission*, § The pending-invite surface below. (Deleting the variant also retires the snapshot-injected e2e arm that pretended to cover it; the tier_3 approve journey in § The pending-invite surface replaces it with a real drive.)
- `out_of_band_code_state: OobCodeState` ∈ `{Idle, Verifying, Valid{invite_id}, Invalid{reason}, Error{cause}}`
- `message: LocalizedText`
- `continue_enabled: bool`
- `recheck_visible: bool`

**A submit refused as already registered is terminal.** When the nest answers `fauna.account.actor_exists` — it already holds this key as an account, a suspended one included ([`login.md`](login.md) § Errors), which in practice is a suspended user who took the sign-in-refused surface's "Use a different nest" route (§ App-launch routing) — the machine lands `Error{transient: false, context: Submitting, cause: "invite.error.already_registered"}` (typed `InviteRequestError::AlreadyRegistered`), and `message` resolves that key-shaped sentinel to its own sentence `onboarding.invite.error.already_registered`, exactly as the recheck's `invite.error.not_found` is resolved. Never a "try again": only the admin's Restore lifts it.

Click handlers:
| Element | Action |
|---|---|
| `invite-request-submit-button` | `wizard_submit_invite_request()`. When the current state is `Denied`, the machine first sends the signed `fauna.account.invite_request.cancel` for its own row, then submits — the nest's one-row-per-actor invariant (`invite_core.rs` refuses a submit while *any* row exists, decided included) is preserved, and the denied requester stops being a dead end. Both kinds are pre-identity and Ed25519-signed by the same actor, so this is an app sequencing change, not a wire change. The submit carries the typed handle's **bare local part**, signed as such — the same split `redeem_invite`'s register makes, since the door refuses an `@` and its signature covers the handle alone. |
| `invite-request-recheck-button` | `recheck_invite_status()` — the same single-shot poll the auto-cadence drives. |
| `invite-code-check-button` | `verify_oob_invite_code(code_input.text)` |
| `invite-request-continue-button` | If `out_of_band_code_state == Valid` → `redeem_invite()` (returns an `OnboardingStep`; if `Done`, query `wizard_outcome()`). **Disabled during `PendingReview`** — the pending-review journey advances by polling (§ The pending-invite surface), never by a continue-exit; the former `submit_invite_request_continue()` exit is retired. |
| `invite-request-back-button` | `cancel_invite_op()` then standard Back. |

### The pending-invite surface ("no nests, 1 pending invite" — ratified 2026-08-11, the Spec-3 re-scope)

**The `invite_request` page in `PendingReview` state IS the surface.** There is no separate placeholder, no `Done` exit, and no dedicated page — § Pages that do not exist already rules `invite_request_pending` out, and the same-session wait and the relaunch hydration (§ App-launch routing's pending_invite row) render one code path, exactly the law the awaiting-DNS surface set. The wizard simply stays on this page after submit; a user who wants to leave closes the app (desktop) or backgrounds it (mobile), and the slot written at the submit return brings them back here.

**Poll is the channel — structurally, not provisionally.** Every shipped notification plane (Push frames, web-push/APNs subscriptions, notification rows, critical alerts) is keyed on a bearer-proven `actor_id` the requester does not have until approval creates it, and the anonymous connection deliberately receives no Push frames (`transport.md` § Pre-identity → No actor state). The one thing that survives for an anonymous caller is the durable `invite_requests` row plus the unauthenticated `fauna.account.invite_request.status` read — so the app polls it:

- **Cadence**: client-owned timers (the awaiting-DNS pattern), driving the same single-shot `recheck_invite_status()`. The interval is a shared-Rust constant exported by `fauna-onboarding-machine` (`INVITE_RECHECK_POLL_MS = 30_000` — admin review is human-latency; 3× lighter than the DNS surface's 10s, still snappy on approval), read by all 7 apps — never seven hand-copied numbers. (Bundle for the build track: lift the awaiting-DNS surface's four per-app 10_000 copies onto a sibling shared constant, priority #4.)
- **First poll fires immediately** when the page shows a hydrated `PendingReview` (the relaunch case), then on the interval while the page is visible.

**Approval is detected as admission.** The admin approve creates the account and deletes the request row (`admin.md` § Section 1), so the poll's `not_found` is the approval signal — ambiguous only with a cancelled/lost request, and the machine disambiguates by asking the one authority that knows: on `NotFound`, **before** rendering any error, it runs the registered-probe — the same challenge/verify ceremony the silent challenge uses (`fauna.auth.challenge` + `fauna.auth.verify`, pre-identity kinds, the identity it already holds). A successful verify **is** the login: the machine concludes exactly like a redeem success, the per-app glue saves identity + nest_url and deletes the slot, and the user lands in the app — no "Approved, press Continue" interstitial, mirroring the awaiting-DNS surface's auto-proceed on claim. **"Exactly like a redeem success" is load-bearing and now includes § 3b-ter's offer:** an approved request is a *join*, so on an app that declared `set_renders_trust_prompt(true)` — all 7 — admission routes to `OnboardingStep::TrustPrompt` with the outcome deliberately unset, and `wizard_outcome() = LoggedIn { nest_url, handle }` / `OnboardingStep::Done` follows when the user answers. That is not the interstitial this paragraph rules out: the ruled-out one *gates admission* behind a press, while the trust offer comes after the user is admitted and declining it costs nothing. The persistence callout below is unchanged in substance — its trigger is the `LoggedIn` outcome, which simply arrives at the offer's conclusion instead of at the recheck's return, the same deferral the claim path has had since the offer shipped (and the same reason a force-quit on the offer is recoverable: the slot outlives it, the next launch re-polls, and the registered-probe re-derives the admission). A verify that reports not-registered means the request is genuinely gone (cancelled elsewhere / admin-purged): terminal `invite.error.not_found`, slot deleted, as before. This is what makes `admin.md` § Architectural rules 5 ("approving must trigger the requester's onboarding flow to advance") true for the first time — the previous shape rendered a terminal error on approval, and the redeem it theoretically offered was doubly dead (`register_core` refuses the already-created actor as `ActorAlreadyRegistered`).

**Deny is read directly**: the denied row persists, the poll renders `Denied{reason}`, and re-submitting runs the cancel-then-submit sequence above. The nest also prunes denied rows on its own, independent of any app action: a background sweep deletes a denied row once its deny decision is 90 days old, which clears the one-row-per-actor block a requester who never re-submits would otherwise sit behind forever (`admin.md` § Section 1 — Pending requests owns the retention mechanism).

**Message copy** follows the mechanism: `onboarding.invite.pending_review` becomes "Submitted. The admin will review — you'll continue automatically once they respond." The `onboarding.done.invite_submitted_{sent,notify}` strings retire with the tui Done screen ("we'll notify you" promised a push channel that cannot exist for an unregistered actor).

**Persistence callouts** during this page:
- After `wizard_submit_invite_request()` returns and the snapshot transitions to `PendingReview`: read the snapshot, write `(nest_url, handle, request_id, status_json)` to the long-term store's pending-invite slot. **This is the only write moment** — the retired continue-exit's duplicate save is gone (rule 5, § Architectural rules: persistence happens on machine return values). In **append mode** the same return additionally triggers adoption — § Multi-account below.
- After `recheck_invite_status()` returns: if the snapshot's state is still actionable (`PendingReview`, `Denied`), update `status_json` in the slot. If the machine routed to `LoggedIn` (the registered-probe confirmed admission — § The pending-invite surface): save secret + nest_url to the identity store, delete the pending-invite slot — the same glue as any `LoggedIn` outcome. If it rendered the terminal `Error{transient: false, context: Rechecking, cause: invite.error.not_found}` (probe refuted admission): delete the slot.
- After `redeem_invite()` (OOB-code path) returns `WizardOutcome::LoggedIn`: save secret + nest_url to the identity store, delete the pending-invite slot if present.

### 3a. Claim code (`claim_code`)

Reached only when the wizard determines the target nest is unclaimed:
- Handle-check returned `HandleCheckOutcome::UnregisteredUnclaimedNest` (the probe verified the nest is running but the `fauna.setup.status` WS-RPC kind returned `claimed: false`), and the user clicked Continue on `handle_entry`.
- Silent-challenge's `fauna.auth.verify` WS-RPC kind reported `fauna.auth.not_registered` AND a follow-up `fauna.setup.status` query reported `claimed: false`. See § App-launch routing — silent-challenge fallback table.
- The admin triggered a **factory reset** from the app (`mail-bridge-lifecycle.md` § Factory reset): the affordance re-seeds the wizard here via `navigate_to_claim_code_for_known_nest_with_code(nest_url, handle, code)`, which additionally **pre-fills `claim-code-input`** with the code `fauna.admin.factory_reset` returned to the app. The human never sees that code, so the input is pre-loaded (read from `claim_code_prefill()` when the input is empty) rather than typed; otherwise this path is identical. The ordinary unclaimed-nest paths above carry no prefill (the human types the code the deployment printed/surfaced) — `navigate_to_claim_code_for_known_nest` clears it. Implemented per-app in the claim-code view's input init: linux `claim_code.rs`; **macOS `MacClaimCodeView` + iOS `ClaimCodeView` read `claim_code_prefill()` on appear** (added 2026-06-13 — both Apple views previously left the input empty after a factory reset, stranding the admin; `test_factory_reset_reonboard.py::test_factory_reset_navigate_prefills_claim_code` green `--app macos`, iOS typecheck-clean and harness-gated).

*(The client-provisioning path never shows this page: the wizard minted the code itself and its claim is automatic — § 6 *Provisioning = build + claim*.)*

The page is mutually exclusive with `invite_request`: there is no admin yet, so requesting an invite is impossible. The user's only path forward is to enter the one-time claim code printed by the nest server's bootstrap process (cloud-init, SSH banner, or server console) and atomically become the admin.

The claim code is a short single-use value minted by `fauna_core::claim_code::generate` (the single source of truth shared by the nest and the app provisioning flow): characters drawn from an ambiguity-free alphabet, displayed in hyphenated groups (`ABCD-EFGH`) — a deliberate 2026-07-24 reduction from a much longer code for hand-transcription comfort, safe at that length only because the anonymous-surface claim throttles are the primary bound (entropy + throttle rationale: `docs/goal/architecture/federation.md` § Security). The app passes `claim-code-input.text` to `wizard_submit_claim_code` **verbatim** — it does no length/charset validation, transformation, or trimming; the nest normalizes (uppercase + strip non-alphanumerics) on both sides at comparison, so the admin may type the code with or without the grouping hyphens. `claim-code-input` must therefore impose **no `maxlength` / charset filter** (web's prior `maxlength="6"` was lifted; the other six apps already pass it through).

**Claim URI (closes a security-review finding: a self-hosted public-domain claim had no axis-2 root).** The same `claim-code-input` text may instead be the console-printed `fauna://claim?code=<code>&nest=<64-hex-nest_actor_id>` URI (the deliberate sibling of `fauna://identity` — every unclaimed nest boot prints it beside the bare code, `bins/fauna-nest/src/claim.rs::print_claim_banner`; `docs/guides/nest-internet-setup.md` tells the admin to paste it). `wizard_submit_claim_code` parses the input first, via the shared `fauna_core::claim_code::parse_claim_input`, **before sending anything**: a bare code (or any string that isn't the `fauna://claim` scheme) passes through unchanged and pins nothing — today's pre-URI behavior, so old consoles and hand-typed codes keep working with **no root** (a documented residual, closed for the account's later life by the post-claim DNS `self=` TXT). A recognized URI's `nest=`, when present and 64-hex, is held as the first-contact identity root for the claim host (`hold_first_contact_identity`), so the very connection `claim_admin` opens graduates against the identity the console vouched for — no TOFU window. A `nest=` present but not 64-hex fails the **whole** parse (state → `Invalid`) rather than silently dropping the pin, so an input that looks protected never silently continues unpinned. This is a client-input-parsing behavior inside the shared machine, not a UI change — `claim-code-input` gets no new element or affordance. Mechanism + trust-model authority: [`../architecture/security.md`](../architecture/security.md) § Transport trust, the *Self-hosted public domain, pre-claim* row.

One row: code input + Claim button + status display. Bottom: Back.

The page is rendered from `claim_code_snapshot()`:
- `state: ClaimCodeState` ∈ `{Idle, Submitting, Claimed, Invalid{reason}, Error{transient, cause}}`
- `message: LocalizedText`
- `submit_enabled: bool` — true when `state ∈ {Idle, Invalid, Error}` AND the input is non-empty.

Click handlers:
| Element | Action |
|---|---|
| `claim-code-submit-button` | `wizard_submit_claim_code(claim-code-input.text)` → `OnboardingStep`. Async; submits the `fauna.auth.claim_admin` WS-RPC kind (over the anonymous connection; the `POST /api/v1/claim-admin` HTTP twin was removed in S4d) with the code, the chosen **handle** (its local part, **required** — a nest cannot be claimed without one), the handle's **`@domain` suffix as `mail_domain`** (`optional`), plus an Ed25519-signed auth payload (`actor_id`, `timestamp`, `signature`) using the secret from `confirm_*_identity`. The handle is **registered as the admin's handle in the same request, atomically** — `claim_admin_core` verifies the code, then requires + validates the handle and creates the admin *with* it (so the handle becomes the admin's address and the canonical mail-recipient alias is materialized from it on enable). A claim with **no handle is rejected outright** (`fauna.auth.invalid_request`): a handle-less admin is a degenerate, unusable state (mail/AUTH login resolves nobody), so claiming-without-a-handle is not a representable outcome. When the wizard handle carries a domain (`alice@fauna.test`), that domain is **auto-registered as the (primary) mail domain** at claim (`claim_admin_core` calls `mail_enable::ensure_mail_domain_registered`), so the handle is a routable email out of the box — no manual admin-dns add-domain step (the domain appears on admin-dns, where the admin can still remove it; config stays app-set). The claim reply also hands the claiming admin's app the nest's deployment signing seed for off-box recovery custody — mechanism + custody owned by [`../architecture/nest/box-recovery.md`](../architecture/nest/box-recovery.md) § Mechanism, not restated here. On success, state → `Claimed` and the wizard advances to the NAT-mode choice (`nat_mode_choice`, § 3b-bis) — the single, terminal admin-path setup step; the launched app then applies the claim-time serving enablement (§ 3b). On a rejection state → `Invalid{reason}` and the wizard stays on this page: a bad code or **missing or invalid handle** renders the generic `onboarding.claim_code.invalid` message with the raw `{reason}` substituted; a rejection because the nest already has an admin (the code itself was fine) is a distinct `ClaimAdminError::AlreadyClaimed` case that gets its own dedicated `onboarding.claim_code.error.already_claimed` message instead, so the raw wire code (`fauna.auth.already_claimed`) never leaks through `{reason}`. On a transient/network failure state → `Error{transient: true, ...}`. |
| `claim-code-back-button` | Standard Back to `handle_entry`. |

There is no separate Continue button: a successful claim transitions automatically to the NAT-mode choice (§ 3b-bis), the admin path's terminal action.

**Domainless / local-target claim.** The default deployment boots **domainless** (no domain, reached at its IP — `docs/goal/architecture/nest/domains-and-tls-bootstrap.md`), so the admin reaches it at a **local target** (`alice@<ip>` / `alice@host.local` / `localhost`). Wizard-visible behavior: a local-target claim registers **no** mail domain — the admin claims handle-only and adds a real domain later from an app; a claim on a real registerable domain (`alice@example.com`) auto-registers it as the primary. The nest-side enforcement (the `!is_public_dns_name` skip + its regression pins) is owned by `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Claim. *(The wizard still passes the local `@suffix` as `mail_domain`; the nest ignores it. Having the machine send `mail_domain = None` for local targets would be tidier but is not required.)*

**Persistence callouts:** After `wizard_submit_claim_code()` succeeds (state → `Claimed`), save `(nest_url, handle, secret)` to the long-term identity store. The user is logged in as the admin at this point but onboarding is not done — the wizard still shows the NAT-mode confirm (§ 3b-bis) before exiting to the authenticated UI. (The record is complete at claim; its legacy storage-`mode` slot is retired with the axis — Phase 4, [`../architecture/nest/storage-modes.md`](../architecture/nest/storage-modes.md).)

**Setup-call sequence.** The admin claim is the single **mandatory** server transaction (`fauna.auth.claim_admin`, done here in § 3a); the wizard then offers the **optional, mutable** NAT-mode set (`fauna.setup.nat_mode`, § 3b-bis — deferring keeps the working seed). Both ride as pre-identity WS-RPC kinds over the anonymous connection (`WsNestApi`; the HTTP twins are removed). There is **no post-claim unresolved state**: the retired second transaction — the write-once `fauna.setup.storage_mode` mode commit and its "claimed but mode-unresolved" window — is gone from the wire (it survived 2026-07-12 → 2026-09-24 as a validate-then-accept-and-discard shim, with a silent post-claim compat commit for an older still-moded nest; both left with the compat-remnant sweep — [`../architecture/nest/storage-modes.md`](../architecture/nest/storage-modes.md) § The transition contract records the window).

### 3b. Serving enablement at claim (the encryption-mode page is RETIRED)

> **Retired page (no-modes, ratified 2026-07-12).** The `encryption_mode_choice` page — the claim-time storage-mode question (Encrypted vs Plaintext), its snapshot (`EncryptionModeState`, `selected_mode`), `select`/`submit`/`defer_encryption_mode_choice`, the `AwaitingEncryptionMode` deferral loop, and the write-once `fauna.setup.storage_mode` commit it drove — is **deleted**: there is no storage mode. Every nest is sealed at rest from first boot, and a box's trust is its set of user-minted capability grants (owner: [`../architecture/nest/storage-modes.md`](../architecture/nest/storage-modes.md); grant primitive: [`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md) § Capability tiering). A successful claim advances directly to the NAT-mode choice (§ 3b-bis). The wire kind survived as a validate-then-accept-and-discard compat shim for older clients until the compat-remnant sweep removed it 2026-09-24 (storage-modes.md § The transition contract). The machine states, the six app views, and the ui.yaml entries are deleted (Phase-4 slice S8.7); the full pre-retirement spec of the page is preserved in git history (this section as of 2026-07-12) and nothing new may be built against it.

What **survives** from the retired page is the serving enablement it hosted — and it survives **without wizard UI** (user decision 2026-07-12): the four claim-time enablement intents (email / CalDAV / CardDAV / WebDAV) become **machine-derived defaults with no checkboxes**. The former `onboarding-enable-email-checkbox` / `-caldav-` / `-carddav-` / `-webdav-` checkboxes are retired with the page, **not relocated** — § 3b-bis stays confirm-only by design; the admin-mail / admin-calendar / admin-contacts / admin-files pages are the change surface after onboarding.

- **Gating rule (the claim axis; made explicit 2026-08-16):** the two defaulting axes below are evaluated **only on a wizard run that actually claimed the box** — they say how the intents *default* once a claim has happened, never whether one did. This is a **gate, not a default**: it is not overridable, and it is the first conjunct of all four intents (`claim_completed`, set at both claim-success sites — `wizard_submit_claim_code` and the deferred-DNS `recheck_manual_dns` → `complete_manual_dns_claim`, including that path's already-claimed resume arm, whose interrupted run never got its launch glue). The axis is load-bearing because **three** routes reach the single `WizardOutcome::LoggedIn` the launch glue reads these getters at — admin claim, `AlreadyOnNest` **sign-in**, and invite redemption — and only the claim routes may request enablement. A sign-in or invite redemption derives **all four OFF regardless of handle and NAT axis**: the box is already configured and its admin's change surface is the settings pages, so re-asserting deployment state from a client-side derivation would be an unrequested Admin-class write. *(Without this conjunct a returning admin's sign-in re-asserted deployment state — four Admin-class writes (`provision_mail_at_first_setup`, `set_caldav_enabled(true)`, `set_carddav_enabled(true)`, `set_webdav_enabled(true)`) against a box they had already configured; caught and closed 2026-08-16.)*
- **Default rule (two axes; NAT conjunct ratified 2026-07-13):** having claimed, each intent defaults **ON iff the admin's handle domain is a real public DNS name AND the box's effective NAT mode is not `private`**. The first axis is the shared handle-targets-real-domain predicate over `resolve_handle_domain` — OFF for `localhost` / `*.localhost` / IP-literal / `.local` targets: mail needs public DNS/MX/DKIM/public-TLS, and the DAV surfaces there are a deliberate manual opt-in rather than a default (`caldav-server.md` § Network exposure). The second axis is the § 3b-bis choice the admin has *just* answered: the **effective** mode is the committed `fauna.setup.nat_mode` value when the admin confirmed, else the nest's seed (`fauna.setup.status`.`node_mode`; a defer commits nothing, so the seed stays authoritative — absent seed ⇒ `public`, the nest's absent-row default). The NAT conjunct is what lets the two-box home-relay deployment ([`../architecture/nest/deployment-home-with-public-relay.md`](../architecture/nest/deployment-home-with-public-relay.md)) express "this box runs no mail" with **no extra UI**: both boxes are claimed with the same real-domain handle, but the home box is exactly the box on the private axis (typically pre-seeded/pre-selected private — § 3b-bis Defaulting note), and a claim-time enable there would mint a **fresh MSEK on the home box, diverging from the fleet MSEK** that the link action later re-seals onto it (`ProvisionRelayMailbox`; the one-MSEK-per-actor rule is `mail-credentials.md` § MSEK lifecycle rule 8). The conjunct covers **all four intents** — the DAV enables mint the same shared MSEK when they run first, so the divergence argument is not mail-specific; a private box's mailbox and serving surfaces are provisioned by the link/relay path and the post-onboarding settings pages instead.
- **Mechanism (unchanged):** the wizard runs pre-identity but the enables are Admin-class, so the machine records the derived intent (`email_enable_requested()` and siblings) and the **authed post-onboarding launch glue** applies it — by handing the four intents to ONE shared step, `fauna_client_mail_settings::serving_enablement` (every app's `LoggedIn` handoff calls it and no app fires the enables itself — built on tui, linux, web, apple and windows; the android handoff still fires the same steps by hand until its trickle-down). The step runs a fixed plan to the end: mail via the shared `enable_mail_with_generated_password` helper, which mints the admin's mailbox blobs + a generated-password credential (surfaced once) and fires `set_mail_enabled(true)` as its final step (on a non-admin first setup the same step is the policy-gated new-user auto-mint); each DAV enable as its own step (`set_caldav_enabled(true)`, …) with the CalDAV-/CardDAV-only mailbox mint only where no earlier protocol minted the shared MSEK; all idempotent with the corresponding settings-page enable paths. It publishes its completion (the decide-nothing run included) as the e2e observable `SERVING_ENABLEMENT_KEY` — [`../architecture/e2e-latency-independent-assertions.md`](../architecture/e2e-latency-independent-assertions.md) § Implementation status today. The box's own bridges self-enroll over loopback and are auto-approved (`mail-bridge-lifecycle.md` § Onboarding auto-approval); no manual approval card.
- **Deployment toggle vs. per-user auto-enable — don't conflate.** These are the **deployment-wide** toggles. A new non-admin user who later joins a mail-enabled deployment gets their `<handle>@<domain>` mailbox auto-provisioned on their first authenticated app setup, gated on the deployment policy `auto_enable_mail_for_new_users` (default-on; `mail-policy-config.md` § Tier-2) — that per-user mint is owned by `mail-credentials.md` § Auto-enable for new users. The admin's own mailbox is the special first case (auto-completes at claim via the launch glue above).

### 3b-bis. NAT mode choice (`nat_mode_choice`)

> **Implementation status today (verified 2026-07-19 — corrected a stale per-app tally).** The shared machine surface, ui.yaml IDs, and i18n are built, and the wizard page renders on **all 7 apps** (Phase-4 S8.7, 2026-07-12, through windows 2026-07-13). The separate **admin-panel NAT toggle** (`admin-nest-nat-mode-*` — the surface that makes the axis changeable *after* onboarding) is a distinct feature this section only points at, not restates — it is now also built on **all 7 apps**; see [`admin.md`](admin.md) § Nest → NAT-mode control, the owning doc, for current per-app status (this section previously said "windows/android/apple remaining," which was already wrong by the time it was read). Full recon (nest half, machine surface, per-app landing detail) + slicing owned by [`../architecture/nest/deployment-home-with-public-relay.md`](../architecture/nest/deployment-home-with-public-relay.md) § Implementation status ("Slice 2 — the app UI fan-out").

Reached directly from a successful admin claim (§ 3a) — the **single, terminal** admin-path setup step. The page resolves the nest's **NAT mode** (`public` / `private`), the network-reachability axis (`public-mode.md` / `private-mode.md`). This page does NOT exist for non-admin users (they inherit the nest's mode). Mechanism + persistence: [`../architecture/nest/deployment-home-with-public-relay.md`](../architecture/nest/deployment-home-with-public-relay.md) § Implementation status (design ratified 2026-06-15; tracked internally).

> **Why this is app-set (product invariant).** The NAT axis was previously env-driven (`FAUNA_MODE` baked into config.toml). Per the product invariant — nest configuration is set from Fauna apps, not env/CLI — it is now an admin choice in the wizard, persisted in nest DB state and changeable later from the admin panel. `FAUNA_MODE` survives only as the **pre-claim seed**: the nest boots in the seeded posture (default `public`; a home-relay installer seeds `private`) and this page pre-selects that seed so the common case is **confirm-only**.

Two-option choice; `selected_mode` defaults to the nest's **current resolved `node_mode`** (the seed — `public` unless the installer seeded `private`):

- **Public** — internet-facing nest with a public domain; serves federation/MX/MUA endpoints and is the active side of pairing. The default for a VPS / hosted box.
- **Private** — not internet-facing (home network / behind NAT); pairs with a public nest as its relay, binds IMAP/CalDAV LAN-only, runs no MTA, no ACME. The choice for a home box (the home-relay installer seeds this).

The page is rendered from `nat_mode_snapshot()`:
- `state: NatModeState` ∈ `{Choosing, Submitting, Done, Error{transient, cause}}`
- `selected_mode: NodeMode` ∈ `{Public, Private}` — defaults to the resolved seed (read from `fauna.setup.status` / the claim reply's `node_mode`).
- `message: LocalizedText`
- `submit_enabled: bool`

Click handlers:
| Element | Action |
|---|---|
| `public-nat-mode-radio` | `select_nat_mode(Public)`. Sets `selected_mode = Public`. |
| `private-nat-mode-radio` | `select_nat_mode(Private)`. Sets `selected_mode = Private`. |
| `nat-mode-confirm-button` | `submit_nat_mode_choice()` → `OnboardingStep`. Commits via the `fauna.setup.nat_mode` WS-RPC kind (pre-identity, Ed25519-signed by the committed admin over the domain-tagged `nat_mode_signed_message` — `SETUP_NAT_MODE_V1 ‖ mode_wire_str ‖ "\n" ‖ actor_id_hex ‖ "\n" ‖ timestamp_decimal`, its own tag so a NAT-mode commit can never replay as the byte-twin storage-mode one — and **mutable**: any valid admin-signed set upserts the `nest_nat_mode` row; there is no conflict reply). Server upserts the row, swaps `AppState.node_mode`, and re-evaluates the live MTA supervisor (`mta_should_run`) + the MDA bind (next `whoami`); ACME/STUN apply on the next restart (deployment-home § Implementation status, spec § 6). State → `Submitting`; on success state → `Done`, returns `OnboardingStep::Done`. On a 4xx-class reject (`not_claimed`, `signature_failed`, `invalid_request`) state → `Error { transient: false }`; on transport/internal failure state → `Error { transient: true }`. |
| `nat-mode-defer-button` | `defer_nat_mode_choice()` → `OnboardingStep::Done`. The mode keeps the seeded value (already a working default), so deferring is safe; the admin can set it later from the admin panel. Visible on every state. |

**Persistence callouts:** The long-term identity-store record is already complete at claim (`(nest_url, handle, secret)` — the NAT mode is nest-side state, not part of the identity-store record). The NAT mode needs no launch-routing re-seed because the seed always provides a working value; there is no unresolved state to detect.

> **Defaulting note.** The NAT mode is confirm-only for the common case — the seed is already correct for both a plain VPS (`public`) and a home-relay box (`private`). The page exists so the choice is *client-owned and changeable*, satisfying the invariant, without adding friction: one click on the pre-selected option. **Private-ward refinement (user-approved 2026-07-12):** when the seed is `public` — which is also just the generic-image default and may carry no installer knowledge — but the wizard's target classification says the nest address is a private-network target (a `localhost`/`*.localhost` or `.local` name, or a **non-global** IP literal per `fauna_core::resolve` — a *public* bare IP does not qualify), the page pre-selects `private` and `nat-mode-status` explains why ("this looks like a home-network address"). The refinement is **one-directional**: a `private` seed is never overridden (only the home-relay installer sets it, always deliberately), and no inference is authoritative — the human on this page is the decision.

### 3b-ter. One-tap "trust this box" default grant (`trust_prompt`) — IDs user-approved 2026-08-13; BUILT on tui 2026-08-14

> **Implementation status today:** BUILT on all 7 apps — **tui** (the lead app, 2026-08-14),
> **linux** (2026-08-14), **android** (2026-08-14), **web** (2026-08-14), **macOS + iOS**
> (2026-08-24, shared FaunaKit `TrustPromptView`), and **windows** (2026-08-26) — windows declares `set_renders_trust_prompt(true)` at
> `OnboardingMachine` construction (`OnboardingViewModel`'s constructor) and mints the default set
> at the signed-in handoff (`OnboardingViewModel.MintDefaultTrustSetAsync`), the same
> capture-before-adopt shape the DNS credential seal uses. **The joining user's first-login offer this section also promises is BUILT as of 2026-09-21, in shared Rust, and therefore on all 7 apps at once**: the machine's four hand-written exits to `WizardOutcome::LoggedIn` (`leave_nat_mode_choice` plus three inline assignments in the handle-check, recheck-poll and redeem arms) were collapsed into one gated exit, `leave_for_logged_in`, which every route reaches with an explicit `TrustOffer` verdict. **Which routes are offered, and why** — the ask-gate this section answers: an invite redemption and an approved join request are *joins* and are offered; an `AlreadyOnNest` sign-in is a returning user, not a joining one, and is not (this section promises the offer to a *joining* user; `docs/features/join-a-nest.md` outcome 8 says "your first sign-in to a nest you **joined**"; re-offering on every launch would train the user to dismiss a capability-grant prompt). Because every app already declares `set_renders_trust_prompt(true)` and routes off `step`, no per-app work was needed for the page to appear on the join routes — the per-app witness is owed, not the rendering. **The fifth and last route — the manual-DNS `already_claimed` resume (`complete_manual_dns_claim`), the recovery edge that short-circuits the wizard's setup tail on an already-set-up box — is offered too (ratified 2026-09-21; it had shipped offer-free pending this section's ruling).** The rule this section states below — *offer at the first conclusion of a run that put the user on this box; suppress only a returning sign-in* — decides it without a carve-out, and the arm's NAT-page skip does not carry over to the offer, for the reason the prose gives. It is one argument at one call site in shared Rust, pinned beside the other four routes in `trust_prompt_navigation.rs` (park on `TrustPrompt`, awaiting outcome cleared at the park so a late poll tick is a no-op, no second claim, no NAT page), and every app already routes the page off `step`, so no per-app work was needed. **The end-to-end witness exists (2026-09-23):** `test_trust_prompt.py::test_the_manual_dns_resume_offers_the_trust_and_survives_a_crash_on_it` takes the real deferred-DNS exit with a claimed nest's own admin identity loaded — the slot that exit writes carries no reach address, so the recheck dials the slot's `nest_url` and escapes the :443 wall a reach-armed slot hits — and asserts the offer, no NAT page, a kill on the offer relaunching back into the resume, and the answered offer's relaunch landing in the app; green on tui, linux and web, the other apps' columns landing through their trickle-down runs as the joiner routes' do.
> The shared halves are done and app-agnostic: `OnboardingStep::TrustPrompt` + the answer latch
> (`grant_default_trust` / `skip_trust_prompt` / `take_trust_prompt_granted`), and
> `LinkedNestsAction::MintDefaultSet`, which mints **every option the shared mint catalog derives**
> (`fauna_client_capabilities::view_model::mint_options` — the same list the Nests page's picker
> offers, so no second availability rule exists to drift), each to its own derived holder, skipping
> what a live grant to that holder already covers.
>
> **Two honest bounds this section's prose runs ahead of, both owned elsewhere:**
> **(1) renewal is promised only as far as every app keeps it.** The tap mints the standard ~90-day
> window and **blesses the home nest** (`ui/nests.md` § Expiry / renewal → *Duration and blessing*),
> so its grants renew themselves wherever the shared auto-renew loop runs — every app's Nests-page
> refresh, plus the app-level tick on tui, linux and web and the foreground pass on android. The consent copy therefore states **no fixed limit** (a limit the
> tap's own blessing removes would under-disclose the widening at the consent moment): it says the
> trust renews itself *while you use Fauna* — never in the background, which phones cannot keep — and
> names revocation in Settings → Nests as the stop, since windows, macOS and iOS do not yet render the un-bless toggle (tui, linux, web and android do since 2026-09-28)
> (ruled 2026-09-27).
> **(2) the set can be empty.** The catalog offers only what would actually mint, so on a box with
> mail not yet enabled or no content processor enrolled the tap mints nothing. That is an honest
> no-op, not an error — the offer is shown after a claim, when the derivable set is usually the
> mail + calendar pair, and the same trust is grantable any time from the Nests page.

An **optional** interstitial before LoggedIn — *not* a setup step (it configures nothing
nest-side, so § 3b-bis stays the terminal admin-path *setup* step): the app offers to mint the
default broad capability-grant set (scoped, ~90d, auto-renewed, revocable, logged) in one tap,
via the existing client mint path; declining changes nothing. Shown after a successful admin
claim, and offered at a joining user's first login. **The one rule that decides every route
(ratified 2026-09-21): the offer is due at the wizard's first conclusion of a run that put the
user on this box — an admin claim, an invite redemption, an approved join request — and is
suppressed only for a returning `AlreadyOnNest` sign-in, whose first login is long past.** The
manual-DNS `already_claimed` resume is on the offered side: it is the admin's own claim
concluding for the first time (the wizard never exited, so the admin was never asked). It
keeps skipping § 3b-bis, and that skip does not extend to the offer: the NAT page is a *setup*
step whose nest-held seed already holds without it, while this page is not a setup step and
has no seeded answer — skipping the setup tail is no reason to skip the ask. A force-quit on
the offer never wedges: every app clears the awaiting slot at `LoggedIn` and nowhere earlier
(§ Long-term store contract owns the moment, ratified 2026-09-21), so the relaunch comes back
into the resume and asks once more — the recovery a clear at the claim would have traded for
a lost offer (the claim-code path's own offer stays that kind of loss, its identity persisted
at claim; a lost offer is grantable any time from the Nests page).
Mint authority stays with the user's client
— the nest never self-mints (iron-clad). Behavior + grant shape owned by
[`../architecture/nest/storage-modes.md`](../architecture/nest/storage-modes.md) § What replaced
each piece of the axis (the "planned optional one-tap default-grant shortcut" row) +
[`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md) § Capability
tiering; this section owns only the page's place in the flow. Success surface: the Nests page
shows the grant + its log entry immediately after the tap.

### 3c. Plaintext mode consent (`plaintext_mode_consent`) — RETIRED

> **Retired page (no-modes, ratified 2026-07-12).** Deleted **without replacement**: there is no deployment storage posture for a joining user to consent to — every nest is sealed at rest, and what a specific box can read is visible per-user as its capability grants on the Nests page ([`../ui/nests.md`](../ui/nests.md)), not implied by joining. Invite redemption proceeds directly with no consent branch. A no-modes nest never reports `mode: "Plaintext"`, so even pre-retirement clients never route here against one ([`../architecture/nest/storage-modes.md`](../architecture/nest/storage-modes.md) § The transition contract). The machine states, the six app views, and the ui.yaml IDs (`plaintext-consent-acknowledge-button`, `plaintext-consent-decline-button`, `plaintext-consent-explanation`) are deleted (Phase-4 slice S8.7); the full pre-retirement spec is preserved in git history (this section as of 2026-07-12).

> **Pages 4-7 — *provisioning a box* — moved to [`onboarding-provisioning.md`](onboarding-provisioning.md) on 2026-09-06.** Pages 1-3c above are *joining* a nest and stay here. The page **numbers are unchanged**, because the wizard the user walks is still one wizard; each heading below is a routing stub so every `§ 4`-`§ 7` citation keeps resolving in one hop.

### 4. DNS configuration (`dns_config`)

**→ [`onboarding-provisioning.md`](onboarding-provisioning.md) § 4. DNS configuration** (2026-09-06 concept partition).

### 5. VPS configuration (`vps_config`)

**→ [`onboarding-provisioning.md`](onboarding-provisioning.md) § 5. VPS configuration** (2026-09-06 concept partition).

### 6. Nest provisioning (`nest_provisioning`)

**→ [`onboarding-provisioning.md`](onboarding-provisioning.md) § 6. Nest provisioning** (2026-09-06 concept partition) — including *Reaching the box*, *Provisioning = build + claim* and *The pending-provision slot*.

### 7. Manual DNS setup (`dns_post_instructions`)

**→ [`onboarding-provisioning.md`](onboarding-provisioning.md) § 7. Manual DNS setup** (2026-09-06 concept partition).

## Pages that do not exist

The following pages **must not exist** in the app:

- `nest_select` — routing absorbed into `handle_entry`.
- `nest_connect` — eliminated.
- `nest_login` — login folded into Continue actions on `handle_entry` and `invite_request`.
- `invite_request_pending` — pending state lives on `invite_request` itself.

If the app has views, view-models, navigation entries, persistence keys, or test helpers for any of these, delete them.

---

## Wizard API surface — what the app calls

All identifiers are the Rust source names. The app uses whatever spelling its FFI/WASM binding produces (camelCase for Swift, lowerCamelCase for Kotlin/TypeScript, PascalCase for C#, snake_case for Rust/Python).

**Construction.** `OnboardingMachine::new(observer, provider_base_urls)` returns an `Arc<OnboardingMachine>`. The observer is an app-supplied callback that fires on every state change; the app uses it to trigger view re-render. `provider_base_urls` is `None` in production; E2E tests pass `Some(map)` to redirect VPS / DNS / nest-health HTTP calls to the Python fake at `tests/e2e-unified/fakes/fake_cloud.py`. Web applies this at construction (reconstructs the wizard machine on a reload); native apps hold one page-bound machine, so they instead install the override at runtime via the `set_provider_base_urls(map)` test hook (driven through the E2E bridge — see below). It overrides only the HTTP provisioning surface (vps/dns/nest-health, all read via `provider_base_url(key)` at call time); the WS-RPC `nest_api` keeps its construction-time override, which `fake_cloud` (HTTP-only) doesn't exercise.

**App-launch hydration.**
- `seed_identity(secret)` — pre-loads a saved identity. Call before showing UI when the long-term store has a secret.
- `seed_pending_invite(nest_url, handle, request_id, status_json)` — pre-loads a saved pending-invite slot. Call after `seed_identity` when the long-term store has a pending-invite record. Lands the wizard at `InviteRequest` with the snapshot hydrated.
- `seed_awaiting_manual_dns(nest_url, handle, dns_records, claim_code)` — pre-loads a saved awaiting-manual-dns slot. Call after `seed_identity` when the long-term store has an awaiting-dns record. Sets `wizard_outcome()` to `AwaitingManualDns` (so the app renders the "Almost ready" surface identically to the same-session exit) and primes `awaiting_manual_dns_snapshot()`. See § "Almost ready" surface.

**Identity stage.**
- `begin_create_identity()` / `begin_import_identity()` — enter the create/import sub-flow.
- `confirm_generated_identity()` → `Result<String, OnboardingError>` — returns secret hex; persist immediately.
- `confirm_imported_identity(secret)` → `Result<String, OnboardingError>` — validates and echoes the secret; persist immediately.

Both `confirm_*_identity` transitions **reset the handle-check result** as they advance to `HandleEntry` — the rich `handle_check_snapshot()` returns to idle (`domain_status()` is a *derived view* over this snapshot's `outcome`, not stored state, so it resets along with it — the same reset point, `reset_handle_check`) and the probe-derived `State` fields (`nest_url`, `node_mode_seed`) clear. Without this, a prior probe's conclusion (e.g. "unregistered at <domain>") would survive a Back → re-import/re-create round-trip and the HandleEntry page would show a stale result until the user re-ran Check. The typed handle text itself is preserved; only the check *result* resets.

**Handle-entry stage.**
- `start_handle_check(handle)` — async; runs DNS → nest health → challenge → optional price probes. For a *local target* (localhost/`*.localhost`/IP/`host:port`) the DNS + price probes are skipped and the resolved `http(s)://host[:port]` base is probed directly (see §2 "Local / loopback targets"). Cancellation-safe; re-calling cancels the in-flight probe.
- `cancel_handle_check()` — explicit cancel without restart.
- `set_control_checkbox(checked)`
- `submit_handle_check_continue()` → `OnboardingStep` — async; routes by outcome. On `AlreadyOnNest` returns `OnboardingStep::Done` and `wizard_outcome()` becomes `Some(LoggedIn{...})`.
- `handle_check_snapshot()` → `HandleCheckSnapshot` — read for rendering.

**Invite-request stage.**
- `wizard_submit_invite_request()` → `OnboardingStep` — async. On a `Denied` current state, sends the signed `invite_request.cancel` first (§ 3 submit handler).
- `recheck_invite_status()` → `OnboardingStep` — async, single-shot (the app drives the cadence — § The pending-invite surface). On the nest's `not_found` it runs the registered-probe before rendering any error; a confirmed admission returns `Done` with `wizard_outcome()` = `LoggedIn{...}`.
- `verify_oob_invite_code(code)` — async.
- `redeem_invite()` → `OnboardingStep` — async, **OOB-code path only** (`out_of_band_code_state == Valid`); on success returns `Done` with `wizard_outcome()` = `LoggedIn{...}`. (Its former `Approved`-state arm is retired with the variant — it could never succeed: `register_core` refuses the admin-created actor as `ActorAlreadyRegistered`.)
- ~~`submit_invite_request_continue()`~~ — **retired 2026-08-11** with `WizardOutcome::InviteSubmitted` (§ Wizard exit handling): the pending-review journey never exits the wizard.
- `cancel_invite_op()` — call from Back.
- `invite_request_snapshot()` → `InviteRequestSnapshot`.
- `INVITE_RECHECK_POLL_MS` — the shared poll-cadence constant (§ The pending-invite surface).

**Claim-code stage.**
- `wizard_submit_claim_code(code)` → `OnboardingStep` — async; `code` is either the bare code or the console-printed `fauna://claim` URI (parsed + pinned before send — see § 3a *Claim URI*). Submits the `fauna.auth.claim_admin` WS-RPC kind (the `POST /api/v1/claim-admin` HTTP twin was removed in S4d) with the code, the machine's current handle (local part, **required** — registered as the admin's handle in the same request; the nest rejects a handle-less claim), the handle's `@domain` suffix as `mail_domain` (auto-registered as the primary mail domain at claim), and an Ed25519-signed auth payload. On success returns `Done` with `wizard_outcome()` = `LoggedIn{...}`. On a rejection / transient failure the snapshot moves to `Invalid` / `Error` and the wizard stays on `claim_code`.
- `claim_code_snapshot()` → `ClaimCodeSnapshot`.

**Encryption-mode-choice stage — RETIRED** (no-modes, ratified 2026-07-12; § 3b). `encryption_mode_snapshot()`, `select`/`submit`/`defer_encryption_mode_choice`, and `seed_pending_encryption_mode_choice` are deleted (Phase-4 S8.7). The serving-enablement intent getters it fed (`email_enable_requested()` and siblings) **survive** as machine-derived defaults (§ 3b).

**NAT-mode-choice stage** (admin claim only — the single, terminal admin-path setup step; § 3b-bis). The shared machine surface below is built and renders on all 7 apps; the admin-panel toggle's per-app status lives at [`admin.md`](admin.md) § Nest → NAT-mode control (the owning doc — see § 3b-bis Implementation status today for the pointer).
- `nat_mode_snapshot()` → `NatModeSnapshot` — read for rendering. `selected_mode` defaults to the nest's resolved `node_mode` (the seed), so the page is confirm-only in the common case.
- `select_nat_mode(mode)` — set the selected NAT mode (`Public` / `Private`).
- `submit_nat_mode_choice()` → `OnboardingStep` — async; commits via the mutable `fauna.setup.nat_mode` kind (upserts the `nest_nat_mode` DB row; re-evaluates the live MTA supervisor / MDA bind, ACME/STUN on next restart). On success returns `Done` with `wizard_outcome()` = `LoggedIn{...}`.
- `defer_nat_mode_choice()` → `OnboardingStep` — sync. Returns `OnboardingStep::Done`; the seeded mode stays in effect (a working default), settable later from the admin panel.

**Plaintext-mode-consent stage — RETIRED** (no-modes, ratified 2026-07-12; § 3c). `plaintext_mode_consent_snapshot()`, `acknowledge_plaintext_mode()`, and `decline_plaintext_mode()` are deleted (Phase-4 S8.7) with no replacement — invite redemption has no consent branch.

**DNS stage.** `dns_config()`, `toggle_buy_domain(on)`, `toggle_same_provider_for_vps(on)`, `select_dns_provider(id)`, `set_dns_cred(field, value)`, `dns_set_up_later()`, `confirm_price()`, `set_contact(contact)`, `verify_dns()` (async), `provider_status()`, `visible_dns_fields()`, `dns_provider_eligible(provider_id)`, `dns_provider_ineligible_reason(provider_id)`, `should_show_contact_form()`, `should_show_registrar_notes()`, `can_verify_dns()`, `can_continue_dns()`, `continue_from_dns()`, `dns_status_text_key()`, `handle_tld(handle)`.

**VPS stage.** `vps_config()`, `select_vps_provider(id)`, `select_vps_location(id)`, `set_vps_cred(field, value)`, `select_vps_server_type(id)`, `visible_vps_fields()`, `can_verify_vps()`, `can_continue_vps()`, `verify_vps()` (async), `continue_from_vps()` (async).

**Provisioning stage.**
- `start_provisioning()` — sync; spawns the orchestrator task on the appropriate runtime (`tokio::spawn` on native, `wasm_bindgen_futures::spawn_local` on web) and returns immediately. All inputs come from the wizard's existing state (handle, DNS provider/creds, VPS provider/creds, contact, set-up-later flag). Observer ticks drive re-render; the app reads `provisioning_snapshot()` on each tick.
- `continue_from_provisioning()` → `OnboardingStep` — sync. Refuses if `overall != Succeeded`. On the deferred path, transitions to `OnboardingStep::DnsPostInstructions`. On the standard path it returns `OnboardingStep::NatModeChoice` and emits **no** outcome — the § 3b-bis tail owns the exit (§ 6 *Provisioning = build + claim*); it additionally refuses while the box is unclaimed, since `Succeeded` is published before the claiming substep reopens the step.
- `continue_from_dns_post_instructions()` → `OnboardingStep` — sync. Sets `wizard_outcome()` to `AwaitingManualDns { nest_url, dns_records, claim_code }`, primes `awaiting_manual_dns_snapshot()`, and returns `OnboardingStep::Done`.
- `provisioning_snapshot()` → `ProvisioningSnapshot` — sync, cheap (clones a small struct).
- `provisioning_in_progress()` / `can_retry_provisioning()` / `can_continue_provisioning()` → `bool` — page-6 button affordances over `provisioning_snapshot().overall`; delegate to the `OverallStatus` predicates so the §6 rules live in one place (mirrors `can_continue_vps` / `can_continue_dns`).
- `cancel_provisioning()` — sets the cancel flag; the running task observes it at the next step boundary or retry iteration.
- `retry_provisioning()` — sync; resets the snapshot to idle, then `start_provisioning()`s. Same fire-and-forget shape.

**Awaiting-manual-DNS stage** (post-provisioning "Almost ready" surface — see § "Almost ready" surface).
- `awaiting_manual_dns_snapshot()` → `AwaitingManualDnsSnapshot` — read for rendering.
- `recheck_manual_dns()` → `OnboardingStep` — async; single-shot poll (the app drives the cadence). No-op unless `wizard_outcome()` is `AwaitingManualDns`. Pre-claim only, over the anonymous WS-RPC connection (`fauna.setup.status` then `fauna.auth.claim_admin`); on success routes to the post-claim setup step — `NatModeChoice` (§ 3b-bis). On the already-claimed resume (the recovery edge, `onboarding-provisioning.md` § *Provisioning = build + claim*) it skips that setup step and concludes through § 3b-ter's offer: `TrustPrompt` with the awaiting outcome cleared (so a late poll tick is a no-op), or straight to `Done` on an app that never declared the page.
- `seed_awaiting_manual_dns(nest_url, handle, dns_records, claim_code)` — app-launch hydration (listed above).

**Other.** `step()`, `current_handle()`, `set_current_handle(h)`, `nest_url()`, `set_nest_url(url)`, `error_message()`, `clear_error()`, `is_loading()`, `back()`, `reset()`, `wizard_outcome()`.

If the binding for any of these methods is missing on the app's platform, the central state-machine surface needs an export annotation — flag it, don't add a per-app workaround.

---

## Wizard exit handling

After every Continue / Redeem call returns an `OnboardingStep`, check it. If `Done`, query `wizard_outcome()` and route on the variant:

| Variant | App action |
|---|---|
| `LoggedIn { nest_url, handle }` | Save `(nest_url, handle)` — and, for a box this wizard just provisioned, its `reach_ipv4` (§ Reach hint) — to the long-term identity store. Delete the pending-invite slot and the awaiting slot if present. Navigate to the authenticated UI (feed). |
| `AwaitingManualDns { nest_url, dns_records, claim_code }` | Complete the awaiting-manual-dns long-term slot with `dns_records` (the slot already exists from the pending-provision write — § 6 — carrying `nest_url`, `handle`, `claim_code`, `reach_ipv4`). Navigate to the "Almost ready" surface (see § "Almost ready" surface) that renders `awaiting_manual_dns_snapshot()` and polls `recheck_manual_dns()` until the freshly-provisioned nest comes online and the claim completes. |

**`InviteSubmitted` is retired from this table (ratified 2026-08-11, the Spec-3 re-scope; DELETED FROM THE CODE 2026-08-12).** The pending-review journey never exits the wizard: the slot is written at the *submit* return (§ 3 Persistence callouts), the `invite_request` page itself is the "no nests, 1 pending invite" surface (§ The pending-invite surface), and the wizard stays there until the poll advances it to `LoggedIn` or the user leaves. The variant's per-app exit arms had quietly become five different behaviors (quit the app / blank page / fall back to identity-choice / a sessionless shell / a dead-end text screen) — deleting the exit is what makes the surface un-divergable: the state renders the page that already exists.

The seam that replaced it is **`OnboardingMachine::pending_invite_slot()`** (UniFFI + wasm `pendingInviteSlot`), returning the assembled `PendingInviteSlot { nest_url, handle, request_id, status_json }` or `None` outside `PendingReview`. It exists as one call rather than three getters because two of its rules are silent when wrong: `nest_url` must come from machine state and **not** `effective_nest_url()` (the `provider_base_urls` override retargets HTTP only — leaking it writes a test-cloud URL into a production identity record), and `status_json` is opaque to the app (§ Long-term store contract). Apps call it at the submit return; **in append mode the same return additionally adopts** (§ Multi-account).

`WizardOutcome` has no unresolved-mode variant (no-modes, ratified 2026-07-12): the retired `AwaitingEncryptionMode { nest_url, handle }` is deleted (Phase-4 S8.7) with no replacement — a successful claim always lands on `nat_mode_choice` (§ 3b-bis), never an unresolved outcome.

The app never inspects machine state to infer outcome. Always go through `wizard_outcome()`.

---

## "Almost ready" surface (post-provisioning DNS-pending)

**§ "Almost ready" surface → [`onboarding-provisioning.md`](onboarding-provisioning.md)** (2026-09-06 concept partition).

## Reach hint — the session reaches a freshly-provisioned box before its domain is live

**§ Reach hint → [`onboarding-provisioning.md`](onboarding-provisioning.md)** (2026-09-06 concept partition) — that doc owns the `reach-hint` concept as of the same date.

## Long-term store contract

The app's existing identity store grows a single pending-invite slot. The slot is one logical record — **not a list, and that is the ratified end-state** (2026-08-11, the Spec-3 re-scope): multi-account already gives every actor its own per-actor slot (`long-term-store.md` § Multi-account evolution), and "one user on several nests" is owned by linked nests / per-user multi-homing ([`linked-nests.md`](linked-nests.md)) plus federation's guest access — never by a pending-invite list.

Fields, in whatever shape matches the platform's existing identity-store layout:

| Field | Type | Source |
|---|---|---|
| `pending_invite_nest_url` | string | the machine's `nest_url()` at the `PendingReview` transition |
| `pending_invite_handle` | string | the machine's `current_handle()` at the `PendingReview` transition |
| `pending_invite_request_id` | string | `invite_request_snapshot().state`'s `PendingReview.request_id` |
| `pending_invite_status_json` | string | `serde_json::to_string(&invite_request_snapshot().state)` — opaque to the app |

Naming is platform-conventional (snake_case for Apple Keychain accounts and Linux libsecret items, PascalCase for Windows ISecretStore, `fauna_*`-prefixed for Web localStorage, snake_case for Android CredentialStore). Layout matches the existing identity-slot shape on that platform.

The app treats `status_json` as opaque. The wizard parses it back via `seed_pending_invite`; on parse failure the wizard falls back to `PendingReview`. Don't validate the JSON.

Store API surface the app exposes internally:
- `save_pending_invite(nest_url, handle, request_id, status_json)`
- `load_pending_invite() → Option<PendingInviteRecord>`
- `delete_pending_invite()`

The store also grows a single **awaiting-manual-dns slot** — the deferred-DNS
analogue of the pending-invite slot. It is written **before `create_server`**
on both provisioning paths (§ 6 *The pending-provision slot*), completed at the
deferred-DNS exit with `WizardOutcome::AwaitingManualDns`'s records, and
cleared at **`LoggedIn`** — the wizard's terminal, never the claim itself
(*Mechanism* below owns the moment; the claim routes to the post-claim
`NatModeChoice` step, § 3b-bis, or to the trust offer, § 3b-ter, and only from
there does the wizard exit to the main app like any claimed nest). The `handle` is required
because the eventual `LoggedIn` outcome carries it and it isn't derivable from
`nest_url` alone.

| Field | Type | Source |
|---|---|---|
| `awaiting_dns_nest_url` | string | `WizardOutcome::AwaitingManualDns.nest_url` |
| `awaiting_dns_handle` | string | `current_handle()` at the deferred-DNS exit |
| `awaiting_dns_records_json` | string | `serde_json::to_string(&AwaitingManualDns.dns_records)` |
| `awaiting_dns_claim_code` | string | `WizardOutcome::AwaitingManualDns.claim_code` |
| `awaiting_dns_reach_ipv4` | string, **optional** (added 2026-08-29; `#[serde(default)]`, so a pre-existing record reads as absent) | the box's public IPv4 from `create_server`, written the moment it returns (§ 6 *The pending-provision slot*); absent before the box exists |
| `awaiting_dns_nest_actor_id` | string, **optional** (added 2026-08-29; `#[serde(default)]`) | the identity the box boots with — the injected deployment seed's derived public key (64 hex), never the seed — written with the claim code before `create_server` and rewritten by the Server step iff it *created* a box (§ 6 *The pending-provision slot*); absent on a record from before the field existed, on the legacy mirror, and for a same-name box this client never built |

*Mechanism (ratified 2026-07-11).* Like the pending-invite slot, this is **one
per-actor slot in the account registry, carried by the shared launch machine** —
not per-app glue. `AccountRegistry` holds it at `fauna/{actor_id}/awaiting_dns`
as opaque JSON (the fields above, as `fauna_launch_machine::AwaitingDnsRecord`),
and `LaunchPersistence::load_awaiting_dns()` is what `LaunchMachine::start()` reads
to route `WizardAt{AwaitingManualDns}` — **checked before the silent-challenge row**
(below). Deletion stays client-side (the machine never writes the store), so the
trait carries no `delete_awaiting_dns`. **The one clearing moment is `LoggedIn`
(ratified 2026-09-21): the slot is spent inside the shared `persist_logged_in`
moment (`fauna-client-accounts`), which every app's `LoggedIn` terminal runs —
never earlier: not at claim completion, not when the outcome leaves
`AwaitingManualDns`, not at the `NatModeChoice` routing.** A `LoggedIn` terminal
that does not reach the helper — the append-mode arms that register through their
own add + switch — clears the slot explicitly at that same terminal. The only other clear is the
"Almost ready" surface's explicit exit
([`onboarding-provisioning.md`](onboarding-provisioning.md) § 6 → *The
pending-provision slot*). Why `LoggedIn` and not the claim: it is the richer
recovery — a force-quit on the NAT page or on the trust offer relaunches back
into "Almost ready", whose first poll takes the already-claimed resume (§ 3b-ter)
and asks once more, whereas a clear at the claim would relaunch straight into the
app with the offer lost; it is also where the pending-invite slot is already
spent, and where five of the seven apps already were (web and apple cleared at
the claim until the ruling — the split this sentence settles). Being
per-actor, it is swept with its identity by `remove()` / `clear_all()`, so a factory
reset cannot strand the next launch on a nest whose secret is gone. The store-side
API is therefore the registry's `awaiting_dns_json` / `set_awaiting_dns_json` /
`clear_awaiting_dns`, not a bespoke `save_awaiting_dns` per app. Authority for
the per-actor key layout: [`../architecture/long-term-store.md`](../architecture/long-term-store.md)
§ Multi-account evolution.

On relaunch the app calls `seed_identity(secret)` then
`seed_awaiting_manual_dns(nest_url, handle, dns_records, claim_code)`, reading the
record back through the **same** `LaunchPersistence` the machine branched on — so
the record seeded is the record routed on, with no second source of truth.

**The `LoggedIn` terminal records the home nest PER-ACTOR, and the legacy single
slot is never enough (ratified 2026-08-22).** Reaching `WizardOutcome::LoggedIn`
means this identity now has a home nest, and that is what the next launch's
silent-challenge row branches on. The write goes through the registry — the
shared `persist_logged_in(registry, secret_hex, nest_url)` moment: `add_account`
with the URL (and the reach hint, when this wizard provisioned the box —
§ Reach hint), activate, and spend the pending-invite slot (its row is evaluated
*before* the silent-challenge row, so a survivor would pin every later launch on
the invite-request surface for an invite already redeemed). Idempotent, so a
re-entered wizard or a resumed claim is safe.

⚠ **The per-actor row is the ONLY place the home nest can be recorded.** There
is no single-slot key beside the registry any more (2026-09-24, the retired
mirror: long-term-store.md § Downgrade mirror + abandoned-append recovery); a
terminal that skipped `persist_logged_in` would leave `nest_url` absent, and
the routing tuple `(identity present, nest_url absent, no slot)` sends the next
launch to `HandleEntry` — a completed onboarding silently re-rendering the
handle-entry page with the user's identity intact and nothing logged. (Before
the retirement the failure was subtler: a URL written only to the legacy slot
was *deleted by the next launch's boot re-mirror* before the launch machine
read it — apple's shape until the shared moment existed, and the deferred-DNS
slot's sibling bug one wizard moment earlier.) Per-actor key layout authority:
[`../architecture/long-term-store.md`](../architecture/long-term-store.md)
§ Multi-account evolution.

**Append mode is exempt.** The "Add account" wizard's own terminal registers and
switches (§ Multi-account below), so activating at this moment would move
`active` off the live account before that runs — the same exemption moment 1's
confirm-identity write carries.

**The terminal reads the secret from the MACHINE, never from the store
(ratified 2026-08-27).** At this moment the wizard has just authenticated with
the identity, so `effective_secret()` always has it (which identity that is:
§ 1 Identity → *Which identity the wizard is acting as*); the long-term store has it
only if **moment 1's write landed**, and that write can silently not land.
`SecretStore::set` is infallible *by signature* — which is the entire reason
`persist_confirmed_identity` does a read-back — and every app's confirm-identity
write is log-only on failure (rule 5 below). So "live authenticated session,
empty secret slot" is a state a real box reaches: a keyring that kept nothing, a
row removed out from under the app, or any drive path that seeds the machine
without going through a confirm arm. **An app that gates its terminal on the
store read therefore has a failure branch on the success path**, and what that
branch does is unconstrained — on linux it tore the wizard down, which for a
non-append run quits the process through the window's close handler, so the user
watched the app vanish at the exact moment onboarding succeeded, with no window
and no message. That is the same divergence class § Wizard exit handling deleted
for `InviteSubmitted` ("quit the app / blank page / …"), arriving on the one
outcome that survived it. The store read is still correct for the *companion*
rows (`device_id`, and empty is the ordinary case there — moment 1 passes `None`
for it); it is only the secret that must come from the machine.

*Implementation status: the shared moment is `fauna_client_accounts::persist_logged_in`,
exported over UniFFI (`fauna-ffi`) and WASM (`persistLoggedIn`), pinned by
`the_wizard_terminal_survives_the_next_boot_re_mirror` (red-verified against the
legacy-only shape). Consumed by **macOS + iOS** (2026-08-22, `OnboardingVM`'s
`if !append` shape; since 2026-09-25 the legacy `node_url` / `cached_handle`
writes beside it are gone — the handle lands in the registry cache), **linux** (`views/onboarding/mod.rs`), and **web**
(2026-08-22, `onboarding/+page.svelte`'s non-append terminal — awaited before the
in-SPA `goto`, since that mounts the feed whose `identity.init()` runs the very
re-mirror the write must survive; pinned by the tier_3
`test_onboarding_logged_in_terminal_web.py`, which RELOADS, because web's exit
does not relaunch and every arrival-asserting test is blind to the loss), and
**android** (2026-08-23, `OnboardingHost.handleWizardExit`'s `if (!isAppend)`
shape, gated on the `appendMode` flag `AccountSettingsVM.beginAddAccount` sets;
pinned at the FFI boundary by `OnboardingWizardExitPersistenceTest.
loggedInSlotSurvivesOnAMultiAccountInstall`), and **windows** (2026-09-25,
`OnboardingViewModel.HandleWizardOutcome`'s non-append `LoggedIn` arm, the secret
from `_m.EffectiveSecret()` and the reach hint from `ProvisionReachIpv4()`; pinned
at the FFI boundary by `AccountRegistryStoreTests.PersistLoggedInRecordsTheHomeNestPerActor`).
tui reaches the same helper through `session.rs`. Before 2026-09-25 this note
read "windows and tui were audited and already correct": windows was not — it
wrote the legacy single slot and cleared the resume slots by hand, with no call
site for the shared moment. All 7 apps now call it.*

*Implementation status (secret source): **tui** (`wizard/mod.rs`'s
`effective_secret()`), **android** (`OnboardingHost.handleWizardExit`),
**web** (`+page.svelte`'s `handleWizardExit`, `m.effectiveSecret()`,
2026-08-27), **apple** (`OnboardingVM.performLoggedInHandoff`,
`machine.effectiveSecret()`, 2026-08-28) and **windows**
(`OnboardingViewModel.SealCapturedDnsCredentialAsync` /
`MintDefaultTrustSetAsync`, `_m.EffectiveSecret()`, 2026-09-06) read the
machine. **linux** read the store AND quit the app on absence; it now reads
`effective_secret()` (2026-08-27). All 7 apps now read the machine at this
terminal. Pinned by the
tier_3 `test_onboarding_logged_in_terminal_empty_store.py`, which drives the
`seed_identity` path (machine only, no moment 1) to the terminal and requires
the app to still be running — the drive path the paid live provisioning e2e
(`tests/live/test_hetzner_provision.py`) takes, and the reason this went unseen:
every other local claim flow seeds via `import_key`, whose UI path runs a
confirm arm and populates the slot.*

*Correction (2026-09-08): the 2026-09-06 windows citation above covered only
`SealCapturedDnsCredentialAsync`/`MintDefaultTrustSetAsync`, two DOWNSTREAM
hand-offs — the terminal itself, `App.xaml.cs`'s `onOnboardingCompleted`
(wired from `HandleWizardOutcome`'s `OnLoggedIn?.Invoke()`), still gated on
`secretStore.LoadSecret()`/`LoadNestUrl()` and, on the machine-only
`seed_identity` drive, silently stalled on the onboarding page instead of
reaching the authenticated shell — the "all 7 apps" claim was true for the
downstream hand-offs but not yet for the terminal itself. Fixed via the new
`OnboardingViewModel.EffectiveSecret` property (`_m.EffectiveSecret()`),
captured in `onOnboardingCompleted`'s synchronous prefix before `OnLoggedIn`
tears the machine down, same shape as the two hand-offs above; the store read
survives only as a fallback for a `Current`-already-gone edge. Windows is now
covered by `test_onboarding_logged_in_terminal_empty_store.py` too.*

### Multi-account (add-account = append-mode onboarding)

One app install may hold several identities and switch between them; the
long-term store is per-actor plus a non-secret index. **Authority:
[`../architecture/long-term-store.md`](../architecture/long-term-store.md)
§ Multi-account evolution** — the identity slots + pending-invite slot become
per-actor there, and switching is an app teardown/rebuild of the launch machine.

"Add account" **reuses this same wizard in append mode** (create-or-import →
handle → connect): launched from the running authenticated session, so (a) the
close-request handler must **not** quit the app (it only dismisses the wizard),
and (b) on the `LoggedIn` outcome the app registers the new identity in the
account registry (`add_account`) and **switches to it** (the switch teardown/
rebuild) rather than booting a fresh single-identity app. Reference implementation:
linux (`build_onboarding_window_append` + `register_switch_account_handler`),
2026-07-02; the other apps adopt the same shape (see long-term-store.md
§ Implementation status today).

**Append-mode deferred/incomplete states + abandonment.** The append wizard can
also reach the deferred/incomplete states — the `AwaitingManualDns` outcome (e.g.
the motivating admin identity claims a nest and exits mid-setup), and the
pending-invite `PendingReview` state, which since the Spec-3 re-scope is not an
exit at all: **the append glue adopts on the submit return** — the same
registration-and-switch it used to run at the retired `InviteSubmitted` terminal
(register the append identity in the account registry, write its per-actor
pending-invite slot, switch to it), triggered by the snapshot's transition to
`PendingReview` instead of a wizard exit. The wizard stays on `invite_request`,
which is now simply the newly-active account's launch surface (what a relaunch
would show); the way back to the previous account is the account switcher, exactly
as after today's terminal adoption. Escape *before* submit abandons back to the
live session unchanged. **An append that is abandoned before any of those
adoptions leaves the registry's routing untouched (ratified 2026-09-24; the
provisioning-custody carve-out ruled 2026-09-25):**
the append wizard's confirm-identity step writes nothing —
`persist_confirmed_identity`'s append arm is a pure derivation, the appended
identity living only in the wizard machine until its terminal adopts it — so
there is no slot pointing at the not-yet-registered identity for the next
launch to mis-route on, and the App-launch routing below reads the registry's
active account exactly as before the append began (long-term-store.md
§ Multi-account evolution → *Downgrade mirror + abandoned-append recovery*,
which records the retired single-slot heal this replaced). **The one mid-run
write is custody, never routing.** A provisioning run started inside the
append mints its claim code before `create_server`
([`onboarding-provisioning.md`](onboarding-provisioning.md) § 6 *The
pending-provision slot* — custody precedes dispatch), and that write registers
the appended identity **inactive** beside its pending-provision slot: the
secret a box is being built for and the code only this app holds must survive
a quit, but the active pointer does not move — every write on the
pending-provision store path (the mint, the reach completion, the retry
re-persist) leaves it where it was, and it moves only at the terminal, exactly
as every other append write. So an append abandoned or interrupted *after* a
provisioning run leaves the live account active and its rows intact (no
hijack), plus one inactive row in the account switcher: the resumable custody
of a box that may be billing. Selecting that row is the foreground resume —
the switch relaunches over its slot onto the "Almost ready" surface, whose
exit ("Use a different nest") or the switcher's remove retires the row.
Actually **resuming** an abandoned append's incomplete provisioning *in the
background* (as a task on the inactive append identity) is **Stage-3 /
background-cross-identity** and is PARKED — until then, an abandoned append is
re-doable via "Add account" or picked up from the switcher.

---

## App-launch routing

At every app launch, before showing UI, read the long-term store and branch on what's present:

```
identity     nest_url     slot present       →  action
─────────────────────────────────────────────────────────────────────────────
(the account index is present and unreadable →  terminal refusal surface;
 — checked before every row below)               never the wizard
present      present      —                  →  silent challenge → main app
present      —            awaiting-manual-dns →  seed_identity(secret)
                                                seed_awaiting_manual_dns(...)
                                                show "Almost ready" surface
present      absent       pending_invite     →  seed_identity(secret)
                                                seed_pending_invite(...)
                                                show wizard at InviteRequest
present      absent       —                  →  seed_identity(secret)
                                                show wizard at HandleEntry
absent       —            —                  →  show wizard at IdentityChoice
```

The awaiting-manual-dns row is checked before the silent-challenge row: while
DNS is still pending the nest is unreachable, so a silent challenge would just
fail through to the `launch_retry` surface. Once the claim completes the slot
is deleted and subsequent launches take the silent-challenge → main-app row.

**The account-index row is checked before every other one, and it is the only
row that never reaches the wizard.** When `fauna/index` is present but this
build cannot use it, the registry answers no session account — so *every* row
in the table reads the install as identity-less and the last one would offer
to make a **new identity** to someone whose accounts are sitting intact behind
a blob this build merely cannot parse. `LaunchPersistence::account_index_refusal`
carries the verdict (`version-compatibility.md` § 5 item 9 owns the two
verdicts); the machine parks in `Offline { transient: false }` — terminal, no
retry, since no amount of retrying reparses a blob — and reports which verdict
in the additive `LaunchSnapshot::account_index_refusal`, on the
`superseded_successor` pattern: an app that does not read the field still stops
and shows `last_error`, and reading it is what picks the right offer. The two
differ **only** in the action they may offer:

| Verdict | What the user is told | Action offered |
|---|---|---|
| A newer build wrote the index (a stamp this build could read says so) | the accounts are intact and updating brings them back (`onboarding.launch.index_newer_build`) | **update the app** — nothing else, because nothing else helps |
| Unparseable, and no readable stamp either | updating cannot help, and nothing has been changed or deleted (`onboarding.launch.index_malformed`) | the documented floor (`long-term-store.md` § Cleanup contract) — and **only behind a confirm that states its residual** (`onboarding.launch.index_malformed_reset_residual`): the two routing pointers go, per-actor slots cannot be reached |

Neither ever offers to **rebuild** the index. A blob nothing here can parse is
not thereby known to name no accounts — only that this build cannot read
them — so the no-overwrite guarantee applies to both
(`version-compatibility.md` § 5 item 9).

The "silent challenge" path is the existing single-flight refresh pattern from the persistence-cleanup work. It runs the challenge handshake against `nest_url` — falling back to the account's reach hint when the domain does not yet connect (§ Reach hint) — refreshes cached handle / domain / tier, and on success enters the authenticated UI. On failure, fall back per the architecture doc's handle-check flow:

| Failure mode | Fallback |
|---|---|
| Challenge endpoint reports the secret is not registered on this nest **AND** the `fauna.setup.status` WS-RPC kind returns `claimed: false` (the nest is running but no admin has claimed it) | `seed_identity(secret)`, navigate the wizard to `claim_code` (the user's only path forward is to claim, since there is no admin to issue invites). On the protocol level, the `fauna.auth.verify` WS-RPC kind returns `fauna.auth.not_registered` AND `fauna.setup.status` returns `{"claimed": false, ...}`. Any error on the `fauna.setup.status` probe falls back to the next row (the claimed-nest sign-in-refused surface) — the safer default; mirrors the handle-check arm. |
| Challenge endpoint reports the secret is not registered on this nest **AND** the nest is claimed (or the `fauna.setup.status` probe fails — the safe default, since verify itself answered) — **the previously-signed-in row, designed 2026-09-24** | Show the **`launch_sign_in_refused`** surface: the localized sentence `onboarding.launch.sign_in_refused` ("This nest no longer signs you in…") in the surface's own **`launch-sign-in-refused-notice`** (not the generic `error-message`, so an e2e cannot be satisfied by any old error text), **Retry** (`launch-retry-button`, re-runs the silent challenge) and **"Use a different nest"** (`launch-fallthrough-button` → `seed_identity(secret)` + wizard at `handle_entry`). **Never the invite wizard.** Rationale: this row runs only on a stored identity + `nest_url`, and the store writes `nest_url` only after a real sign-in or claim here (§ Long-term store contract) — so the nest is one this app *was signed in to*, and an opaque `fauna.auth.not_registered` from it means the nest no longer signs this identity in: **suspended** ([`admin.md`](admin.md) § 2 Users → *Cutting a user off*), or **removed** (the eviction ladder's end, or a private nest that dropped the actor). The app cannot tell which — [`login.md`](login.md) § Errors rules the wire code opaque (no suspended-vs-unregistered oracle) — so the copy names both and asserts neither, and the surface distinguishes on **client-local knowledge only** (`nest_url` present), never on anything that crosses the wire. Retry is the way back in: a suspended account's restore is a button on the *admin's* app (`admin.md` § *Cutting a user off* → Restore: no client-side recovery step, no re-onboarding), so this is the **one terminal launch outcome whose retry the machine honours**. The wizard was wrong on both readings: it offered to re-join a nest that already holds the account, and a suspended actor's invite submit is refused outright as already registered (`fauna.account.actor_exists`). A removed-then-relaunched user reaches the invite path through "Use a different nest" → `handle_entry`, where the wizard's own handle check routes them (§ 2). **Machine shape (all 7 apps, no per-app routing):** `fauna-launch-machine` parks in `Offline { transient: false }` with the sentence in `last_error` and reports the verdict on the additive `LaunchSnapshot::sign_in_refused` field — the `account_index_refusal` / `superseded_successor` pattern (above): an app that never reads the field still stops routing into the wizard and shows the sentence; reading it is what adds Retry and the pane title `onboarding.launch.sign_in_refused_title`. **The same verdict mid-session lands the same surface** ([`../architecture/security.md`](../architecture/security.md) § Post-auth surfacing): the machine's token-refresh arm carries the field too; a `4401` teardown followed by a refused re-mint on the WS-RPC client is the same signal. The page and its notice element are user-approved (rule A, 2026-09-25); which apps render them today: § Implementation status today. The unclaimed-nest sub-branch stays the `claim_code` row above: an unclaimed nest at a saved `nest_url` is the factory-reset-from-another-device case, and the claim the user owes is not a refusal. |
| Network failure or nest unreachable (DNS doesn't resolve, decommissioned, transient outage, 5xx, captive portal — client-side reachability cannot reliably distinguish these) | Show the `launch_retry` surface (`launch-transient-error` / `launch-retry-button` / `launch-fallthrough-button` per `ui.yaml`) with two CTAs: **Retry** re-runs the silent challenge against the same `nest_url`; **"Use a different nest"** calls `seed_identity(secret)` and lands the wizard at `handle_entry`. Rationale: a client-side classifier that distinguishes transient from terminal is fundamentally unreliable (DNS-fail can be airplane mode; connection-refused can be transient ISP filtering), and a false-terminal misclassification — alarming the user and dropping them to handle_entry when retry would have worked — is strictly worse than the mild friction of always offering Retry. The user has context the app lacks; the two CTAs surface that choice. |
| Nest **authoritatively** reports it is outdated — `fauna.nest.outdated` (a schema/version mismatch the nest detected at boot, so it serves a degraded "needs-update" mode; `version-compatibility.md` Dim 4 / § 2.2). | Show a **NON-retry** "update required" surface: render the localized actionable message (`error.nest.outdated`) in the page's `error-message` element, **omit `launch-retry-button`**, and keep `launch-fallthrough-button` ("Use a different nest" → `seed_identity(secret)` + wizard at `handle_entry`). Rationale: unlike the reachability row above, this is **not** an unreliable client-side guess — the nest *told us authoritatively* it cannot serve this client version, so retrying the same nest is futile and a Retry CTA would just spin a doomed loop. This is the "update prompt vs. retry" distinction `version-compatibility.md` Dim 4 requires. Where the signal arrives depends on the path: the connect / launch-machine path lands `Offline { transient: false }` with the localized `last_error` (apps route `transient: false` to this non-retry surface, never the retry CTA); the direct silent-challenge path raises `FfiError::NestOutdated` (native) / a wasm `"outdated:"`-prefixed rejection (web). |
| The nest's **pinned deployment identity changed** — or a pinned nest can no longer prove any identity (the withdrawn/downgrade case). The trust layer's verdict, not a reachability guess: natively the challenge/verify (and handshake) connection's channel-binding graduation caught a `PinChanged` (or a missing/failed binding while a pin exists); on web the verify reply's possession-proof differs from the localStorage pin. Semantics owner: [`../architecture/security.md`](../architecture/security.md) § Transport trust. | Show the **BLOCKING** `launch_identity_changed` surface (`nest-identity-changed-warning`; the SSH `known_hosts` model) — **no retry CTA** (a retry cannot change the verdict and must never silently re-pin) and **no auto-entry**; the minted bearer is dropped. Two explicit ways out: **"Trust this nest"** (`nest-identity-changed-trust-button`) → forget the pin and re-TOFU — on the shared `LaunchMachine`, `trust_nest_identity()` (forget via the connector's trust seam + re-run the silent challenge), which is what **web** calls since it adopted the machine (2026-07-13); native shells off the machine use `fauna_ffi::forget_nest_identity_pin`. And **"Use a different nest"** (`launch-fallthrough-button`) → `seed_identity(secret)` + wizard at `handle_entry`. Any app that has not yet built the re-trust button still renders the warning **without a retry CTA** — never a Retry, which cannot change the verdict — and offers only the fallthrough; see [`../architecture/security.md`](../architecture/security.md) § Implementation status for the per-app state and the ui.yaml widening gate (all seven render the full surface today, so that fallback is a contract for future shells, not a description of any current one). On the machine this is `LaunchPhase::IdentityChanged { pinned_hex, seen_hex }` (`seen_hex` absent = withdrawn); the same verdict during a **runtime token refresh** lands the same phase — the token is dropped and the session blocks on the warning (a mid-session identity change is the same MITM signal). |

**What a completed wizard hands the launch path — and the harness trap in it.** On
`AlreadyOnNest` Continue, the machine derives the session's nest URL from the handle
the **user typed**, not from the nest's reply (which carries a bare localpart), and
records it in `WizardOutcome::LoggedIn` (`libs/fauna-onboarding-machine/src/machine.rs`
§ `submit_handle_check_continue`). That derivation is `resolve_handle_domain`, which is
**uniform https for every host-class with an explicit port, loopback included** — the
resolver owns that rule (`libs/fauna-provisioning/src/probe.rs`), and a nest serves TLS
on every entry path. Two consequences worth stating here, because both have been read as
onboarding bugs:

- The arm is deliberately **independent of whether the probe phases ran**, so a
  snapshot-injected `AlreadyOnNest` completion reaches `Done → LoggedIn` exactly like a
  real check. "The injection seam cannot complete the wizard" is not true.
- But the recorded URL is `https://` **regardless of the override seam the probe used**
  to reach a plain-HTTP test nest. So a completion driven against a plain-HTTP nest
  hands the app a URL it cannot dial: the wizard completed correctly and the
  *authenticated* connection then fails, landing the retry surface. That reads exactly
  like "onboarding never completed" — and did, for a day (where a probe found the app on none of feed/handle_entry/identity_choice with
  `handle: None`, and the retry surface was the one place it never looked). **A test
  that drives a completion needs a nest with `handle_domain` + `serve_tls=True`**;
  `test_smoke_k_real_onboarding_completion_reaches_the_main_app`'s `handled_nest` is
  the reference fixture.

### A verdict renders only while its own launch is still the current one

**Ratified 2026-09-21**, alongside the apple implementation that measured it. A launch
verdict is decided from a store read taken when the launch *started* and rendered one
network round trip later. Anything that becomes a **newer session** in between — another
launch, a teardown, a completed sign-in, an e2e session patch — leaves a verdict in
flight that was already wrong when it was computed. **Rendering it walks the app
backwards**, and not only cosmetically: three of the arms are session-destroying
(`WizardAt`, `IdentityChanged`, and the account-index refusal), so a superseded verdict
tears down a session it never examined.

- **The rule, one sentence:** a launch renders its verdict only while it is still the
  launch the app currently holds. An app that keeps a handle on the in-flight launch
  already has the mechanism — it checks that the verdict it is about to render belongs
  to *that* launch, and every superseding path retires an in-flight launch by replacing
  or clearing the handle rather than by reaching into its task.
- **It is an ownership test, never an "is a session present?" test.** A verdict from the
  *current* launch still routes in full even though a session is live — which is the
  whole point of `IdentityChanged`, the MITM verdict that must tear the session down
  (`../architecture/security.md` § Transport trust → *Connection-teardown rule*). A
  guard keyed on mere presence would silently retire that.
- **A superseded verdict is dropped whole**, not arm by arm: half-applying one leaves
  the incoherent session § Long-term store contract already names. Whatever superseded
  it owns the routing from there — and an app whose shell reads a *launch-gate* state
  before it reads its session must resolve that gate when it retires the verdict that
  would otherwise have resolved it, or the shell sits on a launch spinner with a live
  session behind it.

⚠ **Any app can reach this, not only one that enters optimistically.** The gating apps
re-enter their launch from the account switcher, the remove-account promote and the
factory-reset re-onboard; optimistic entry (iOS — § Implementation status) only raises
the cost, because there the superseded verdict lands on a shell the app has already
authenticated into.

**iOS is the only app that enters optimistically — the other six GATE, and the split is
citable.** iOS renders the home tabs off the cached identity and lets the machine's
verdict correct course (§ Implementation status today, the iOS row: *"That is the one
deliberate macOS/iOS launch divergence"*). web, linux, tui, macOS, windows and android
each hold their shell on a launch-gate or wizard state and authenticate **no** session
until the verdict lands. The consequence that matters beyond routing: the three
session-destroying arms (`WizardAt`, `IdentityChanged`, the account-index refusal) end a
**real, already-entered session on iOS alone**. On the gating six the same verdict routes
a wizard and destroys nothing — so there is no teardown there to observe, to count, or to
assert, and a cross-app test measuring one declares their absence rather than expecting a
number (`../architecture/e2e-latency-independent-assertions.md` § convention 14 — iOS's
fifth teardown arm, `leaveAuthenticatedSession`, exists for exactly this and for nothing
else). This is a statement about *entry posture*, not about who re-enters a launch: the
paragraph above still holds for all seven.

**Admin-claimed, mode-unresolved — RETIRED as a native wizard route** (no-modes, ratified 2026-07-12; supersedes the 2026-07-11 `WizardAt{EncryptionModeChoice}` mechanism). Target state has **no unresolved-mode window**: the `VerifyReply.storage_mode_pending` flag a no-modes nest reported as a constant `Some(false)` — and that a new client read off an older still-moded nest to route it to the browser path ([`../architecture/nest/storage-modes.md`](../architecture/nest/storage-modes.md) § The transition contract, rule 4) — left the wire 2026-09-24 with the compat-remnant sweep; a successful verify lands `Online`. There is **no client-local `pending_encryption_mode` slot** in any shape — the Linux/Web slot + pre-machine branch was pre-ratification drift and is deleted with the route (Phase-4 S8.7): `SilentChallengeOutcome::Success` never branched on `storage_mode_pending` after S8.7, and since 2026-09-24 the field does not exist — a successful verify lands `Online`.

---

## Element IDs

Every element has the same ID across all 7 apps. Source of truth: `tests/e2e-unified/ui.yaml`. The IDs the wizard pages use:

**`identity_choice`** — `create-identity-button`, `import-identity-button`.

**`identity_created`** — `secret-key-display`, `secret-key-copy-btn`, `identity-continue-button`, `identity-created-back-button`.

**`identity_import`** — `paste-secret-field`, `import-submit-button`, `identity-import-back-button`. Optional: `qr-camera-view` (camera preview on platforms with a scanner).

**`handle_entry`** — `handle-input`, `handle-check-button`, `handle-message-area`, `handle-entry-continue-button`, `handle-entry-back-button`. Optional: `handle-control-checkbox` (visible only on `RegisteredNoNest` outcome).

**`invite_request`** — `invite-request-submit-button`, `invite-request-status`, `invite-code-input`, `invite-code-check-button`, `invite-code-status`, `invite-request-continue-button`, `invite-request-back-button`. Optional: `invite-request-recheck-button` (visible during `PendingReview`).

**`claim_code`** — `claim-code-input`, `claim-code-submit-button`, `claim-code-status`, `claim-code-back-button`. No Continue button — the Submit is terminal.

**`launch_sign_in_refused`** (the launch surface of § App-launch routing's previously-signed-in row, shown before the wizard mounts) — `launch-sign-in-refused-notice`, `launch-retry-button`, `launch-fallthrough-button`. **IDs user-approved 2026-09-25** (rule A) and allocated in `ui.yaml`.

**`encryption_mode_choice` — RETIRED** (§ 3b; ui.yaml page + all its IDs — `encrypt-storage-radio`, `plaintext-storage-radio`, `encryption-confirm-button`, `plaintext-trust-confirm-button`, `encryption-mode-defer-button`, `encryption-mode-status`, and the four `onboarding-enable-*-checkbox` serving toggles — are deleted in Phase-4 S8.7; the serving toggles are **not relocated**, per the § 3b ruling).

**`nat_mode_choice`** — `public-nat-mode-radio`, `private-nat-mode-radio`, `nat-mode-confirm-button`, `nat-mode-defer-button`, `nat-mode-status` (the single, terminal admin-path setup step; `selected_mode` defaults to the resolved seed — with the § 3b-bis private-ward refinement — so it is confirm-only in the common case). No Back button — the admin is server-committed. **IDs user-approved 2026-07-12** (rule A satisfied) and allocated in `ui.yaml`.

**`trust_prompt`** — `trust-box-summary`, `trust-box-grant-button`, `trust-box-skip-button` (+ `error-message`); the optional one-tap default-grant interstitial (§ 3b-ter — built on all 7 apps). **IDs user-approved 2026-08-13** (§ 3b-ter's status block) — allocated in `ui.yaml`.

**`plaintext_mode_consent` — RETIRED** (§ 3c; the page and its IDs — `plaintext-consent-acknowledge-button`, `plaintext-consent-decline-button`, `plaintext-consent-explanation` — are deleted in Phase-4 S8.7).

**`dns_config`** — `dns-buy-domain-checkbox`, `dns-same-provider-checkbox`, `dns-provider-row` (indexed), `dns-set-up-later-button`, `dns-provider-link`, `dns-provider-open-browser-button`, `dns-provider-help-text`, `dns-credentials-form`, `dns-verify-button`, `dns-status-text`, `dns-tld-price-display`, `dns-config-back-button`, `dns-config-continue-button`. Optional: `dns-price-confirm-checkbox`, `dns-no-provider-message`, `dns-registrar-notes-text` (per-provider notes from `i18n/providers.yaml`'s `registrar_notes_key` field, visible when `buy_domain && provider has registrar_notes_key`), `dns-contact-form` plus 9 `dns-contact-{first-name,last-name,email,phone,address1,city,state,postal-code,country}-input` fields (visible only when `buy_domain && provider_status == UnregisteredBuyable && requires_contact`).

**`vps_config`** — `vps-provider-row` (indexed), `vps-provider-link`, `vps-provider-open-browser-button`, `vps-provider-help-text`, `vps-credentials-form`, `vps-verify-button`, `vps-location-picker` (indexed), `vps-server-type-radio` (indexed), `vps-config-back-button`, `vps-config-continue-button`.

**`nest_provisioning`** — `provisioning-step-row` (indexed), `provisioning-step-checkbox`, `provisioning-step-label`, `provisioning-substep`, `provisioning-step-error`, `provisioning-retry-button` (visible when `overall == Failed` or `Cancelled` — see §6's `can_retry()`), `provisioning-cancel-button` (visible when `overall == Running`), `provisioning-elapsed`. Plus the architecture-owned top-region price/CTA elements (not enumerated here — defined alongside step 6 in the architecture doc). The legacy `provision-progress` and `provision-stage` IDs are migrated to the per-step IDs above.

**`dns_post_instructions`** — `dns-post-instructions-text`, `dns-post-instructions-copy-button`, `dns-post-instructions-continue-button`.

Every page exposes `error-message`. Per-platform implementation: `data-testid` (Web), `AutomationProperties.AutomationId` (Windows XAML), `Modifier.testTag` (Android Compose), `accessibilityIdentifier` (iOS/macOS), `set_widget_name` (Linux GTK).

---

## E2E bridge contract

The app's automation driver (Playwright bridge for Web, FlaUI for Windows, the in-process automation server for iOS/macOS, AT-SPI for Linux, UiAutomator for Android) implements:

```
driver.call_machine_method(name: str, json_arg: str)
```

The bridge serialises the call across to the running app, which then invokes the named method on its `OnboardingMachine` instance with the JSON-decoded argument. Today this is used by `tests/e2e-unified/drivers/machine_test_setter.py` to fixture wizard snapshots:

- `set_handle_check_snapshot_for_test(snap_json)` — drops a `HandleCheckSnapshot` directly into the machine.
- `set_invite_request_snapshot_for_test(snap_json)` — same for `InviteRequestSnapshot`.
- `set_nest_identity_pin_for_test({nest_url, actor_id})` — seed a TOFU nest-identity pin, so the next launch's silent challenge meets a pin the nest cannot prove and must route to `LaunchPhase::IdentityChanged` (`security.md` § Transport trust) instead of auto-entering. Paired reader: `nest_identity_pin_for_test({nest_url})` → the pinned id as a JSON hex string (`null` when none), so a test can assert the re-trust button *forgot* the pin.

  These two are **not** per-app: the pin store is process-global and each app installs its own backend at startup (`DiskPinStore` native, `LocalStoragePinStore` web), so the one dispatcher arm writes the right store everywhere and the harness never learns either backend's on-disk shape. The arg carries the **nest URL** and each side derives the pin key exactly as its production path does (native: the URL's authority; web: the origin verbatim), so a test never has to know origin-vs-host. Native reaches them through the shared dispatcher; web through thin `fauna-wasm-onboarding` exports that delegate to that same dispatcher, because its bridge reflects over the wasm-bindgen surface rather than calling `call_machine_method` by name.

- `set_account_reach_for_test({nest_url?, reach_ipv4?, clear_reach_ipv4?})` — point the **active account's** identity URL and/or reach hint (§ Reach hint) wherever a test needs them. Paired reader: `account_reach_for_test()` → `{actor_id, nest_url, reach_ipv4}` (the account secret the same slot read carries is dropped at the seam and never crosses the bridge). Every field is optional and **absence means "leave it alone"**, so a test moves one slot without restating the other; clearing the hint is its own flag, because a JSON `null` and an absent key deserialize alike and "do not touch" must stay distinguishable from "delete". Both are no-ops when no account is active — a bridge call made a moment too early must never take the app under test down.

  These live in a **second dispatcher**, `fauna_client_accounts::call_registry_method_for_test(registry, name, json_arg)`, tried before the machine's. The pin arms above can be machine-free because their store is process-global; the account registry is not — each app builds one from its own `SecretStore` — so shared Rust holds the name table and the semantics while the app contributes only its registry, exactly the split `persist_logged_in` already makes for the production `LoggedIn` moment. An app's agent therefore adds **one** delegation and every later registry-level seam costs it nothing. The UniFFI apps reach it through the `test-helpers` export `FfiAccountRegistry::call_registry_method_for_test`, compiled out of the production flavors like every other seam. Wired on **tui** (the lead app), **macOS** and **iOS** (one FaunaKit helper, `RegistryTestBridge`); the other four inherit the logic the moment their agent adds the same arm.

  **Why a bridge seam and not a UI gesture.** The reach hint's behaviour is observable only while the domain is unreachable *and* the hint reaches the box, and the hint is a bucket-1 fact captured automatically at the wizard's `LoggedIn` terminal — never a knob — so no user gesture produces that disagreement on demand. This is fixture setup, which `../architecture/e2e-conventions.md` § point 8 exempts by name; the behaviour the journey then asserts is entirely the app's own (`tests/e2e-unified/tests/test_reach_hint_dial_journey.py`).

- `refuse_secret_writes_for_test({refuse})` — make the app's own credential store take every identity-secret write and keep none, until a second call with `refuse: false` lifts it: the locked-keyring shape `SecretStore::set` cannot report, which `add_account`'s read-back then refuses (`../architecture/long-term-store.md` § *Adding refuses a secret the store did not keep*). The fault lives **in the store's backing**, as a reserved row outside every per-actor key's shape — not on the registry value (apps build a fresh registry view per operation), not keyed by the store object (apple and the UniFFI seam build a fresh store bridge per `FfiAccountRegistry`, web a fresh `LocalStorageSecretStore` per registry in two wasm modules with separate statics), and not process-wide (it would also fail the auxiliary namespaces and every parallel unit test). Every view over the same keychain or origin therefore sees it, and nothing else does. Sticky rather than one-shot because an app writes the successor seed more than once on the way to the stolen-identity ceremony's persist-failure message (`../ui/settings.md` § Recovery kit) — the only surface it exists to reach, since no user gesture makes a keystore refuse on demand. Fault injection, not a stand-in for the user: the journey drives the ceremony through the UI (`tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_key_this_device_cannot_store_stays_on_screen_until_you_leave`). Same dispatcher as the reach arms above, so an app that has added the reach delegation has this one too.

**Return values.** `call_machine_method` returns the method's result: **setter/command** names return nothing; **reader** names (`provisioning_snapshot`, `provider_base_url`) return their JSON-serialised value so a test can poll live machine state (e.g. the fake-cloud orchestrator e2e polling "is the Online step Running"). Web returns values by reflecting over the wasm getter surface (`__fauna_callMachineMethod`); native apps route the read through the shared `OnboardingMachine::call_machine_method_with_result(name, json_arg) -> Option<String>` and surface the JSON back through their automation bridge (Linux/Android/macOS/iOS/Windows: stashed in the test agent's pushed state as `machine_method_result`, decoded — not the raw JSON string, which the state dict's own re-encoding at the wire boundary would double-encode). The setter-only `call_machine_method(name, json_arg)` (no return) is retained for native apps that haven't yet wired the value-returning path.

**Apple's `machine_method_result` stash had two independent bugs (both fixed 2026-07-18) — a genuine SwiftUI/JSON-shape gotcha worth knowing before touching this bridge again.** (1) **`@State` mutated from an `init()`-captured `self` silently drops the write.** `FaunaMacApp`/`FaunaApp` wire the in-process bridge's `commandHandler`/`stateProvider` closures from `startInProcessAgentIfNeeded()`, called in `init()` — per Apple's own guidance, `@State` must never be read/written from `init()`, because SwiftUI installs the "live" storage for the instance that actually renders separately from whatever `init()` saw. A bare `@State private var machineMethodResult: Any?` written through that init-time closure therefore always read back `nil` on the *next* poll, even though the write itself did not crash — every reader call (`provider_base_url`, `provisioning_snapshot`) silently reported nothing, only ever exercising the always-`nil` setter-only path in practice before this fix. **Fixed** by wrapping the stash in a reference-type box (FaunaKit's `MachineMethodResultBox` — a plain class held via `@State`) and mutating its `.value` property in place, mirroring how `conversationsVM`/`feedVM` already dodge this same trap (a class instance's own property mutation is visible through ANY struct copy holding the reference, regardless of `@State`'s own live-binding bookkeeping — only *reassigning* a `@State` value needs that bookkeeping). (2) **macOS additionally nested `machine_method_result` one level too deep.** `serializeState()`'s `data["machine_method_result"] = …` placed it under `state.data.machine_method_result`, but the bridge contract above — and linux's reference `state_json` (`apps/fauna-linux/src/main.rs`) — puts it top-level, a sibling of `nav`/`session`/`messages` (`state.machine_method_result`); the Python driver reads exactly that shape (`resp["state"]["machine_method_result"]`). iOS already had the correct top-level placement — only macOS had drifted. Both bugs together meant `provider_base_url`'s read-back always reported `None` even once `set_provider_base_urls` had applied correctly, which is what made `test_cancel_mid_online_then_retry_succeeds` regress. Neither bug is specific to this one reader — any future native reader name would have hit the identical silent-`nil` trap.

**Async methods block until complete.** When a method is async (e.g. `verify_dns`, `verify_vps`), the driver's `call_machine_method` must not return until the method has finished and its effect is in the snapshot — so a test can drive `verify_dns` then immediately `continue_from_dns` without a poll. Web satisfies this because its `__fauna_callMachineMethod` is `await`ed by the web driver.

The two sync dispatchers (`call_machine_method`, `call_machine_method_with_result`) **cannot `.await`**, so every async name falls into their forward-compatible `_` arm and is *silently ignored* — the bridge would ack green having done nothing. Native apps therefore route through the shared **`OnboardingMachine::call_machine_method_async(name, json_arg) -> Option<String>`**, which runs the await-to-completion methods (`verify_dns`, `verify_vps`, `continue_from_vps`, `start_handle_check`, `verify_oob_invite_code`, `submit_handle_check_continue`, `wizard_submit_claim_code`, `wizard_submit_invite_request`, `recheck_invite_status`, `redeem_invite`, `recheck_manual_dns`, `submit_nat_mode_choice`) to completion before returning, and delegates every other name to the sync dispatcher — one name table, not one per app. The app blocks its bridge ack on that call (tui: `await` on its tokio main loop; Linux: `async_helper::block_on_tokio`, blocking the GTK thread for the one local round-trip), so the ack — and thus the driver's return — fires only afterward.

**Fire-and-forget orchestration is deliberately excluded from that dispatcher.** `start_provisioning` / `retry_provisioning` spawn a task that outlives the call (provisioning runs for minutes; the bridge must return at once and let the driver poll `provisioning_snapshot`), and *which runtime owns that task* is genuinely platform-divergent — `tokio::spawn` on an ambient runtime (tui), a dedicated worker thread whose runtime must outlive the task (Linux, whose `block_on_tokio` builds a current-thread runtime and **drops it on return**, cancelling anything merely spawned inside), `spawn_local` (web), or UniFFI's own managed tokio runtime via the `runProvisioning()` suspend export awaited from a coroutine scope (Android — `NestProvisioningVM.start()`/`retry()` call the suspend twin instead of the sync `startProvisioning()`/`retryProvisioning()`, which do a bare `tokio::spawn` with no ambient runtime guaranteed on the UI thread; `run_provisioning_inner` resets the snapshot + cancel flag at entry, so the same suspend call serves both; macOS/iOS share this mechanism — the provisioning views' start/retry buttons wrap `Task { await vm.machine.runProvisioning() }` instead of calling the sync `startProvisioning()`/`retryProvisioning()`, same hazard, same fix; Windows shares it too — `OnboardingViewModel`'s `ProvisioningStartAsync`/`ProvisioningRetryAsync` `[RelayCommand]`s `await _m.RunProvisioning()` instead of the sync `StartProvisioning()`/`RetryProvisioning()`, same hazard, same fix). Each app keeps that one arm; awaiting them in the shared dispatcher would break the return-immediately contract.

**Credential-form field IDs are `{kind}-credentials-form-{field.id}`** (e.g. `vps-credentials-form-api-token`) on every app, so a helper that types into the form resolves the field globally; the cred *key* the app passes to `set_dns_cred`/`set_vps_cred` stays the raw `field.id`.

The 8 tests in `tests/e2e-unified/tests/test_handle_entry_outcomes.py` exercise this bridge to verify the app renders each handle-check outcome correctly. They are red until the app implements the bridge; turning them green for `--app <yours>` is part of done.

The bridge uses the test-helpers feature gate on the Rust side (`#[cfg(any(test, feature = "test-helpers"))]`). The app's debug/test build links the machine with `--features test-helpers,test-observer`.

---

## ui-actual refresh

`tests/e2e-unified/ui-actual-<app>.yaml` reflects the app's actual implementation. When the app's onboarding migration lands, refresh:
- Bump `status.updated`, `status.source_commit`.
- Remove blocks for `nest_select`, `nest_connect`, `nest_login`, `invite_request_pending`.
- Update `handle_entry` and `invite_request` blocks.
- Update `diffs_with_ui_yaml` to reflect remaining gaps (most apps will have none for these pages once migration completes).
- `ui-actual-lint` must exit 0.

---

## Architectural rules

1. **Observer-driven rendering.** Subscribe to `OnboardingObserver`; on every notification re-read the relevant snapshot getter and re-render. No client-side caching of machine state.
2. **No per-stage view-model.** A single thin proxy object exposes machine snapshots to the view layer. Old per-stage VMs (e.g. legacy `ProvisioningVM`, per-page state classes) are deleted.
3. **Snapshots are read-only on the client side.** Mutations go through machine methods. The app never constructs a snapshot to mutate state.
4. **Localized strings come from `LocalizedText`.** The wizard returns `{key, args}`; the app looks up the key in its generated i18n table and substitutes args. Don't hard-code English in the view; don't recompute message text in the app.
5. **Persistence happens on machine return values, not on observer ticks.** The app persists when a method *returns* (the `PendingReview` transition after `wizard_submit_invite_request`, `LoggedIn` after `redeem_invite` or a recheck-confirmed admission, secret after `confirm_*_identity`). It does not poll the machine for state to persist.
6. **No platform-specific element IDs.** Use the IDs from `ui.yaml` verbatim. If your platform can't render an element with that ID, raise a ui.yaml change rather than inventing a variant.
7. **No platform-skip in tests.** A test that doesn't apply on this app gets fixed in the action layer, not skipped at the test layer. The only valid skip is structural impossibility, and `--app <name>` already handles app deselection.
8. **No hidden-shim elements.** If the platform's natural UI doesn't match a ui.yaml element, propose a ui.yaml change. Don't ship zero-size accessibility elements as filler.
9. **Provider field rendering uses `visible_*_fields()`.** Don't filter providers or fields client-side.
10. **`error-message` element on every page.** Read it before any failing assertion in tests.
11. **No client-causable unrecoverable nest state (absolute).** Every nest state a client can reach — including a client crash *mid-claim* or *mid-factory-reset* — must be recoverable by a client, with no SSH / shell / manual DB surgery. Claim and factory-reset are the canonical transitions; they are crash-atomic (claim sets a required handle so a handle-less admin is unrepresentable; factory-reset wipes via a boot-time marker so a crash leaves either "completes on next boot" or "unchanged", both re-claimable). The authoritative statement + the verification question for any new client-driven transition lives in [`../architecture/nest/common.md`](../architecture/nest/common.md) § Client-state recoverability (absolute invariant).

---

## Don't do these

- Don't store wizard state in a per-stage view-model. The wizard owns state.
- Don't reach into machine internals. Use the public API.
- Don't add a `StateStore` callback or any wizard-side persistence trait. The wizard is in-memory.
- Don't validate `pending_invite_status_json`. The wizard handles parse failures.
- Don't design a multi-pending-invite list. Single slot per actor — the **ratified end-state**, not a placeholder (Spec 3 retired 2026-08-11: its list premise is superseded by the per-actor registry slot + multi-account, and multi-homing belongs to [`linked-nests.md`](linked-nests.md)).
- Don't carry the OLD wizard surface alongside the new on this app. Replace, don't shim.
- Don't mark tests `pytest.skip` for platform differences.
- Don't depend on field order in JSON snapshots. Use the binding's deserialiser.
- Don't bypass the test-helpers bridge to fixture state via real probes in tests.
- Don't introduce per-app element IDs.

---

## Done definition

An app is done when all of the following hold:

- [ ] All seven pages render against the snapshot getters.
- [ ] No view, view-model, navigation entry, or persistence key references `nest_select`, `nest_connect`, `nest_login`, or `invite_request_pending`.
- [ ] `confirm_*_identity` callers persist the returned secret immediately.
- [ ] App-launch glue branches on the four cases above; calls `seed_identity` and/or `seed_pending_invite` as appropriate.
- [ ] Long-term store has a pending-invite slot with the four fields above.
- [ ] Continue/Redeem call sites read `wizard_outcome()` and route on all variants (`LoggedIn`, `AwaitingManualDns`; `InviteSubmitted` is retired — § Wizard exit handling).
- [ ] `driver.call_machine_method` works through the app's automation bridge.
- [ ] `tests/e2e-unified/tests/test_handle_entry_outcomes.py` — all 8 tests pass for `--app <yours>`.
- [ ] `tests/e2e-unified/tests/test_invite_request_states.py` (and siblings, when authored) — all pass for `--app <yours>`.
- [ ] `tests/e2e-unified/tests/test_claim_code_unclaimed_nest.py` — passes for `--app <yours>`. Covers handle-check routing the unclaimed-nest outcome to `claim_code`, page idle/back/URI-paste behavior, and the invalid-code error path. (Corrected — a stale "successful claim → `LoggedIn`" line stood here; this file has no successful-claim test at all, and a successful claim has landed on `nat_mode_choice`, never `LoggedIn`, since the no-modes retirement — § 3a. That leg is covered instead by `tests/e2e-unified/tests/test_mail_enable_at_admin_claim.py`, which submits `claim-code-submit-button` and asserts the `nat_mode_choice` landing.)
- [ ] `tests/e2e-unified/tests/test_provisioning_progress.py` — passes for `--app <yours>`. Covers step-row rendering, sub-step transitions, the skipped-on-rerun idempotency path, and cancel-mid-Online → retry-succeeds.
- [ ] `tests/e2e-unified/ui-actual-<app>.yaml` refreshed; `ui-actual-lint` introduces no new errors compared to the session's starting SHA. (The lint baseline is currently dirty for android/ios reasons unrelated to onboarding migration; "exits 0" overall is not the right bar — "no regression in your app's section" is. Fix-ups for the unrelated dirt belong to those apps' sessions.)
- [x] Automated (linux, tier_3): submit invite request, force-quit, relaunch — wizard lands at `invite_request` with `PendingReview` (`test_onboarding_launch_routing_smoke.py::test_smoke_a_pending_invite_survives_force_quit`).
- [x] Automated (linux, tier_3): a registered identity relaunch skips the wizard → main app (`test_smoke_b_registered_identity_relaunches_to_main_app`).
- [x] Automated (tier_3, native): a **real onboarding completion** — the wizard's own handle check run against a live nest, no injected snapshot — reaches the authenticated main app (`test_smoke_k_real_onboarding_completion_reaches_the_main_app`). Distinct from smoke B, which *seeds* a stored identity and asserts the launch path: K is the only automated proof that the **post-wizard hand-off** works, and it is the reason to distrust a green suite as evidence of it. Verified on windows 2026-08-10, where nothing had proved it before — every other windows test either logs in via `set_state` or force-navigates, so all of them stayed green while this was an open question. **Verified on linux and tui 2026-08-21** — both green on the first real run. **macos/ios run 2026-08-21** (same row): **ios green** (a single run — the race below is structurally present on ios too, so don't treat this as proof ios is clean); **macos REDS deterministically** (reproduced twice) — `session.authenticated` flips `true` while `handle-input` is still rendered, because `OnboardingVM.performLoggedInHandoff` assigns the flag synchronously before the async post-onboarding launch (`completeAuthenticatedLaunch`) has actually mounted the authenticated app. Root-caused and tracked (the same "assigned, not derived" flag fragility already flagged from code inspection, now with a hard regression test). **The fix landed 2026-08-22**: `SessionState.isAuthenticated` is now DERIVED from the mounted shell on both apple targets and has no setter — full shape in `architecture/e2e-conventions.md` § convention 11's implementation-status bullet. Apple's arm of this line still owes its green run: do not mark it `[x]` until the case passes on both apple apps under repeated runs. **The 2026-08-22 post-fix macOS run makes the remaining gap precise, and it is a DIFFERENT defect:** with the flag honest (`authenticated: false` while `handle-input` is up — the contract), the case no longer fails at the flag-vs-surface assertion but at the arrival wait — `performLoggedInHandoff` completes (every session field written) while the async `completeAuthenticatedLaunch` never sets `isOnboarded`, so the app sits on the handle-entry page with `app error: ''` and no launch surface shown. **Root-caused and fixed 2026-08-22**, and the defect was one layer below the launch glue: apple's wizard terminal recorded the home nest in the **legacy single slot only**, while identity-confirm had already materialized the account index — so the next launch's boot re-mirror, which rewrites the legacy view from the per-actor rows, **deleted the URL before the launch machine read it**. `load_nest_url()` answered `None`, the routing tuple degraded to `(identity, no nest_url, no slot)`, and § App-launch routing's table sent it to `HandleEntry` — the wizard's own handle-entry page, re-rendered after a perfectly successful onboarding, with nothing logged because that is a legitimate routing outcome everywhere else. The terminal is now the shared `persist_logged_in` moment (§ Long-term store contract), pinned by `fauna-client-accounts`' `the_wizard_terminal_survives_the_next_boot_re_mirror` (red-verified against the legacy-only shape). **Apple is GREEN on both targets under repeated runs (2026-08-22): macOS 2/2, iOS 3/3** — the same case that had failed deterministically on macOS, and the iOS leg run three times because its earlier single green was timing luck rather than proof. Apple's arm of this line is therefore discharged. ⚠ **The audit that fix produced found the same defect on two more apps**, each captured with its own row: android (latent — no android e2e has ever run on a device) and **web**, where the shape is *invisible to this very test*: web's wizard exit is an in-SPA `goto` with no relaunch, so arrival succeeds and the user is stranded on their NEXT page load — a green smoke K on web is not evidence either way, and any test for it must reload. ⚠ **The case needs a nest with `handle_domain` + `serve_tls=True`** (its own `handled_nest` fixture, not the session `nest_instance`): since Pillar C the typed `…@127.0.0.1:<port>` handle resolves to **https**, so a plain-HTTP nest fails the handshake *after* a perfectly correct wizard completion — see § App-launch routing's note on injected completions.
- [x] Automated: the redeem **wire shape** is pinned by the onboarding-machine unit test `redeem_invite_registers_the_bare_local_part_signed_over_the_handle_domain`; an OOB code is redeemed through the real UI to `LoggedIn` by `tests/e2e-unified/tests/test_bearer_cache_web.py` (tier_3, web). ⚠ **This line used to claim the approve→redeem drive was "covered by `test_invite_request_*` + onboarding-machine unit tests". It was not, and the false claim was load-bearing** (2026-07-17): `test_invite_request_states.py` only asserts `is_enabled` on `invite-request-continue-button` and never clicks it, so it covers the *enablement gate*, not the redeem; the unit tests covered `verify_oob_invite_code` for the same reason. Nothing exercised `redeem_invite`'s register call, and it was **broken end-to-end on all six apps** the whole time — it sent the whole `alice@nest.example` as the handle, which the nest's `validate_handle` rejects before it even looks at the signature. Fixed in the same commit as this line. When a coverage claim names a test, check that the test *drives* the mechanism rather than its precondition.
- [ ] No regressions in adjacent test suites.

---

## Implementation status today

**The provisioned-box reach and provisioning status entries — the standard path's claim, the `Online` ceiling, retry/start-over/relaunch resume, the persisted reach address and the pending-provision slot, with the options struck at ratification → [`onboarding-provisioning.md`](onboarding-provisioning.md) § Implementation status today** (2026-09-06 concept partition).


**The pending-invite surface (§ 3, ratified 2026-08-11) is BUILT (2026-08-12) — shared machine, the three retirements, the per-app persistence move, and the poll timer, on all 7 apps.** Per-app build legs: windows' leg landed 2026-08-15; macOS/iOS's leg landed 2026-08-23.

**Landed 2026-08-12 (shared Rust, `libs/fauna-onboarding-machine`):**
- **Approval is detected as admission.** `recheck_invite_status`'s `NotFound` arm runs the registered-probe (`NestApi::silent_challenge` — the same `fauna.auth.{challenge,verify}` ceremony the silent sign-in uses) *before* rendering any error: registered → `WizardOutcome::LoggedIn` + `OnboardingStep::Done`, not-registered → the terminal `invite.error.not_found` as before. **This is what makes `admin.md` § Architectural rules 5 true for the first time.** A probe that cannot reach the nest deliberately leaves the state `PendingReview` (never an `Error`) so the poll continues — an error there would wedge the poll, since `recheck_invite_status` only proceeds from `PendingReview`.
- **A denied requester can resubmit.** `wizard_submit_invite_request` sends the signed `invite_request.cancel` first when the current state is `Denied` (new `NestApi::cancel_invite_request` seam; the payload builder is `machine::build_invite_request_cancel`, pinned to verify against the bytes `cancel_invite_request_core` reconstructs). App sequencing only — no wire change.
- **One poll cadence for all 7 apps.** `poll::{INVITE_RECHECK_POLL_MS, AWAITING_DNS_POLL_MS}` (+ UniFFI accessors), replacing what would have been seven hand-copied numbers.
- **tui polls** on that cadence (`main.rs::run`'s `invite_poll` arm, the awaiting-DNS arm's twin — both drive the shared `periodic_tick`), so on tui an approval now lands the user in the app with no user action. linux, web and android joined it 2026-08-12.
- **The status row says what is happening** (2026-08-12, all 7 apps). `InviteRequestSnapshot.message` was a *write-once* field — set to the idle copy by `InviteRequestSnapshot::idle()` and never written again by any transition — so `invite-request-status` read "Ask for an invite above…" through `Submitting`, `PendingReview`, `Denied` and every error, and a **denied requester was never shown the reason**. The whole `onboarding.invite.*` string family was unreachable. It is now derived from the state on every `invite_request_snapshot()` read (`snapshots::invite_request::message_for`), the rule `oob_message` already followed for this page's other row — Architectural rule 4. The one arm that derives nothing is the dead `Approved` variant, whose string takes pre-formatted byte sizes shared Rust cannot produce without hard-coding English; it disappears with the variant. **Why no test caught it:** `test_invite_request_states.py` *injects* `state` and `message` together, which production never does — the gap needed the real journey (`test_pending_invite_journey.py`) to surface.

**The three retirements LANDED 2026-08-12, atomically with the per-app persistence move.** `WizardOutcome::InviteSubmitted`, `submit_invite_request_continue()` and `InviteRequestState::Approved` are all deleted, along with `onboarding.invite.approved` + `onboarding.done.invite_submitted_{sent,notify}` and the two `invite-request-continue-button` transitions in ui.yaml (`state.approved` and `state.pending-review`). They had to move together because **five apps wrote their pending-invite resume slot *exclusively* in the `InviteSubmitted` arm**, so deleting the exit alone would have compiled cleanly and silently stopped them persisting. All seven now write at the submit return from `pending_invite_slot()` (§ Wizard exit handling), and the five divergent exit behaviors are gone with the exit: linux no longer quits the app, web no longer renders a blank page (`step: 'done'` had no template arm — its "stay on the page" comment described what never happened), windows no longer falls back to identity-choice, android no longer exits into a sessionless shell, tui's dead-end text screen and macOS's undeclared `invite-submitted-placeholder` (also a rule-A deviation) and iOS's `Color.clear` are all deleted.

Two consequences worth knowing. `message_for` is now **total** (`LocalizedText`, not `Option`) — its one `None` arm existed only for `Approved`, so the read path no longer has an "unreachable" branch to get wrong; the totality is pinned by `every_state_derives_a_non_empty_message`, whose match carries no wildcard so a new variant fails the build rather than silently losing its string. And `InviteQuota` **survives deliberately** as a wire type (`InviteRequestResponse.quota`): a client must keep decoding a field it has merely stopped using (`version-compatibility.md`), so it is not dead code to clean up.

**At-rest:** `status_json` is a serialized `InviteRequestState`, so retiring `Approved` is an at-rest change. No migration is owed — `seed_pending_invite` degrades an unparseable slot to `PendingReview`, which resumes polling and lets the registered-probe re-derive the truth; that strictly improves on the old dead end (a rendered `Approved` whose redeem hit `ActorAlreadyRegistered`). Pinned by `a_legacy_approved_slot_resumes_as_pending_review_and_polls`.

**Polling is live on all seven: tui, linux, web, android, windows, macOS, iOS.** All seven read the cadence from the shared `poll::{INVITE_RECHECK_POLL_MS, AWAITING_DNS_POLL_MS}` (native via the UniFFI accessors, web via the wasm `inviteRecheckPollMs`/`awaitingDnsPollMs` — ⚠ exported as `f64`, because wasm-bindgen maps a 64-bit integer to a JS `BigInt` that `setInterval` rejects), and the awaiting-DNS surface's hand-copied `10_000`s were lifted onto the shared constant on every platform (linux, web, android, windows in the pass; macOS/iOS's `AwaitingManualDnsView.swift` in the pass). `continue_enabled` is now false throughout `PendingReview` on all 7.

**windows landed 2026-08-15**: `InviteRequestView.xaml.cs`'s `DispatcherTimer`, mirroring `AwaitingManualDnsView`'s pre-existing cadence timer — both now read `InviteRecheckPollMs()`/`AwaitingDnsPollMs()` rather than a literal. Building it surfaced a genuine, unrelated windows registry bug that blocked the approve→advance journey specifically (an orphaned per-actor `nest_url` slot from the import flow's early secret persist); fixed in the same change — see `long-term-store.md` § Eager vs. lazy migration at native boot for the mechanism.

**macOS/iOS landed 2026-08-23**: the shared FaunaKit `OnboardingVM.pollPendingInviteWhileNeeded()`, attached by both `MacInviteRequestView` and `InviteRequestView` (iOS) as `.task(id: vm.isInvitePendingReview)` so SwiftUI restarts the loop the instant the page enters `PendingReview` (submit or relaunch hydration) and tears it down when the page leaves that state or the view disappears — the first poll fires immediately, matching § The pending-invite surface's cadence rule.

**The `onboarding.invite.pending_review` reword has SHIPPED (2026-08-23).** § The pending-invite surface's "…you'll continue automatically once they respond" is now live on all 7 apps, the last of which (macOS/iOS) can finally keep the promise.

**Test coverage.** The machine half is unit-covered (the probe's three arms, cancel-then-submit ordering, the cancel signature's domain separator, and a resume→approve drive that replaced the old injected-`Approved` fixture). **Both owed journeys landed 2026-08-12** in `tests/e2e-unified/tests/test_pending_invite_journey.py` (tier_3), green on all 7 apps as of 2026-08-23 (windows 2026-08-15; macOS/iOS row 73):

- `test_admin_approval_advances_the_requester_with_no_user_action` — the real drive: submit through the app UI, an out-of-band admin approve, then **no user action at all** until the app reaches the authenticated shell on its own. This is the first green e2e for `admin.md` § Architectural rules 5.
- `test_a_denied_requester_reads_the_reason_and_can_resubmit` — deny → the reason renders on `invite-request-status` → a resubmit succeeds, proven nest-side by a *new* pending row id (a UI-only assert would also pass if the page merely re-rendered its old `PendingReview`). It drives the denial-observation click when the button is still there, tolerating the app's own automatic poll winning the race first — both call the identical `recheck_invite_status()`, so either is valid evidence. (Superseded framing: this test used to claim the manual click was what let it "run on all 7 apps rather than tui alone" — true only while tui was the sole poller; all 7 have polled since 2026-08-23 above, so that rationale no longer holds and is not the reason this test runs everywhere.)

**tui's poll timers keep their cadence under continuous activity (fixed 2026-08-12).** `main.rs`'s `tokio::select!` used to call `sleep(INTERVAL).await` fresh inside each of `pending_invite_tick`/`awaiting_dns_tick`/`screen_lock_tick`'s own arm; `select!` drops the losing futures every spin, so any other arm winning — a keypress, a mouse click, an agent request — restarted the sleep at zero. The real cadence was "INTERVAL of UI inactivity", not the interval the constants above promise — invisible to a real user (the invite page is otherwise idle) but a genuine e2e trap: an agent driving the app via `dispatch` → `recv_agent` was itself "activity" and could starve its own poll forever. Fixed by building each as a `tokio::time::Interval` **once**, outside the select loop (`periodic_tick`, `run`'s `dns_poll`/`invite_poll`/`lock_tick`) — the deadline lives inside the `Interval`, not the per-poll future, so racing it against a faster arm no longer restarts it. This is why the approve case above waits on `session.authenticated` via `GET /app/state` rather than an element: that choice used to be **load-bearing** (an element read would have starved `pending_invite_tick`), and is now merely the cheaper option — pinned by `apps/fauna-tui/src/main.rs::periodic_tick_tests`, mutation-graded (both an inverted `active` gate and a reversion to the old rebuilt-`sleep()` shape are caught).

**§ 3b's claim axis is BUILT, unit-pinned, and e2e-pinned on tui (2026-08-16).** The
gate is one conjunct (`OnboardingMachine::claim_completed`, over `State::claim_completed`) on all four
getters, so every app inherits it from the shared machine — linux, tui and apple read the getters
directly and web through `fauna-wasm-onboarding`'s delegating wrappers; no app re-derives. Pinned by
`serving_enablement_derivation.rs::plain_sign_in_on_a_real_domain_public_box_derives_all_four_off`
(a sign-in derives all four OFF) and `::a_manual_dns_claim_still_derives_all_four_on` (the second
claim path keeps its defaults — it never touches the claim-code snapshot, which is why the axis is an
explicit state fact and not a read of that snapshot).

The **journey** layer — the one the 2026-08-16 live-box observation actually lived at — is pinned by
`test_mail_enable_at_admin_claim.py::test_a_returning_admin_sign_in_issues_no_deployment_enable`
(tier_3, tui). It drives BOTH journeys against ONE nest, because the gating rule asserts a
*difference*: the same identity first CLAIMS with a domain-shaped handle (deployment mail enables —
the control, so a glue that enables nothing reds there first), the toggles then go back OFF and the
client store is wiped, and the SAME identity types the SAME handle at the SAME nest — signing in this
time. Nothing may be enabled. Its negative half is anchored per
[`../architecture/e2e-latency-independent-assertions.md`](../architecture/e2e-latency-independent-assertions.md) point 14 rather than on a
settle-sleep: the authed shell renders, `driver.barrier()` establishes that the `LoggedIn` handler
(where the spawn decision is made *synchronously*) has run, and the residual network leg is bounded by
a ceiling **measured from the control leg in the same run**. Red-verified against the reverted
conjunct; **the witness was `caldav_enabled`, not `mail_enabled`** (the control leg has already minted
that admin's mailbox, so the mail arm no longer reaches its `set_mail_enabled(true)`), which is why
the pin reads all four toggles in one `get_mail_config` — a mail-only assertion false-passes here.

**Remaining gap:** that pin runs on **tui, macOS and iOS** — apple's leg of the same
post-`LoggedIn` dial-seam (`resolved_dial_url`) swap landed 2026-09-24 at `FaunaMacApp`/`FaunaApp`'s
`completeAuthenticatedLaunch`/`enterOptimistically` + `applySessionPatch` call
sites, and the journey is now e2e-green on both apple targets. web resolves the
dial too (see below) but cannot run this particular journey for an unrelated
reason; windows and android landed their own leg of the swap (windows
2026-08-25, android 2026-08-21) but lack the admin-claim onboarding UI drive
this journey also needs, so on those two the sign-in journey stays pinned by
the shared derivation, not yet by an app-level e2e.

**tui's glue fires all four enables (fixed 2026-08-17; previously fired only `email`/`caldav`, a
parity gap against linux).** `apps/fauna-tui/src/wizard/mod.rs`'s `WizardOutcome::LoggedIn` arm now
captures `carddav_enable_requested()`/`webdav_enable_requested()` alongside the original two and
fires `mail_glue::set_carddav_enabled`/`set_webdav_enabled` (CardDAV's companion MSEK mint gated
`!enable_email && !enable_caldav`, mirroring `carddav-server.md` § Independent enablement; WebDAV
has no companion mint — `webdav-server.md` § Independent enablement). Both
`test_admin_claim_with_real_domain_handle_auto_enables_mail` and
`test_a_returning_admin_sign_in_issues_no_deployment_enable` now read all four axes via
`_read_deployment_enables` on the positive claim leg, not just `mail_enabled`.

**§ 3b's derived-ON branch is e2e-proven locally — on tui and linux (2026-08-13).** The derivation
itself was always unit-covered (`libs/fauna-onboarding-machine/tests/serving_enablement_derivation.rs`),
but the *glue-fires-through-a-real-app* half had never run anywhere except the env-gated live-remote
test: a domain-shaped typed handle is the only input that derives ON, and the authenticated session
each app establishes at `LoggedIn` dialed the **literal** `https://{domain}`, which no local DNS
resolves — so a harness claim reached `LoggedIn` and then simply never connected, and
`enable_mail_with_generated_password` could not run. Every *pre-identity* call had already resolved
through the `provider_base_urls["nest"]` override (the handle-check `probe_base`, and the claim /
NAT commit via `effective_nest_url`); the authed dial was the one leg with no seam.

- **The seam lives once, in `fauna_launch_machine::dial::resolved_dial_url`** — the crate every
  app's launch runs through. It returns the installed `provider_base_urls["nest"]` override, else
  `nest_url` unchanged, and it is a `cfg`-split pair: the production twin is the identity function
  and the override state and its setter do not exist in a release artifact at all
  (`../architecture/e2e-automation-surface-gating.md` point 15). The override is process-global **because the
  consuming path has no object to hang it off**: a relaunch and an "Add account" switch both begin
  at the store, with no onboarding machine in scope. `OnboardingMachine::set_provider_base_urls`
  mirrors its `"nest"` entry there, so the one harness gesture still installs and clears both
  halves, and `OnboardingMachine::resolved_nest_dial_url` is now a *face* that delegates rather than
  a second reader — the two answers cannot disagree.
- **It redirects only the socket.** The stored `nest_url` — what apps persist, render and hand to
  `MuaInstructions::for_node_url` — stays the literal typed string. Inside the machine that is
  enforced by resolving in `WsAuthConnector` alone, so `State::Online` and the
  `save_authenticated` write never see a resolved URL; a persisted override would make an app dial
  the harness forever after.
- **tui** consults it at its `LoggedIn` glue (`wizard/mod.rs::nest_dial_url` → `session::adopt`,
  which persists `nest_url` and dials `dial_url`) **and** at its store read
  (`launch.rs::route`'s `Online` arm), which is what an append or an ordinary relaunch takes.
- **linux** resolves at its two client-construction sites — `views/onboarding/mod.rs`'s
  `launch_main_app_after_signin` (post-claim) and `main.rs`'s `launch_authenticated` (relaunch) —
  while the registry's per-actor `nest_url` (`persist_logged_in`) and the status row keep the
  literal. Its `LaunchMachine` needs no call site: the connector resolves internally.
- Both are proven by `test_mail_enable_at_admin_claim.py::test_admin_claim_with_real_domain_
  handle_auto_enables_mail` (tier_3): a real-domain claim auto-mints the admin mailbox and flips
  deployment mail with **no UI action after the claim**.
- **web already resolves it too (landed 2026-08-17, row 21 — independently of this row, so found
  only by inspection here, not built for it).** `apps/fauna-web/src/lib/api.ts`'s `nodeUrl()` is
  web's TS twin of `resolved_dial_url` (same production-identity-function property: it collapses to
  `storedNestUrl()` whenever `__FAUNA_E2E_AUTOMATION__` is false), consumed at every connection site
  (`rpc.ts`'s `getClient`, `resolve.ts`, the mail-settings/spam machine builders) — the ~25 nest-facing
  call sites the module doc names. The override installs end-to-end: the driver's
  `set_provider_base_urls` reloads with the `fauna_e2e_provider_base_urls` query param,
  `onboarding/machine.svelte.ts`'s init reads it and calls `setNestDialOverride` alongside the
  onboarding-provider override, and `nodeUrl()` reads the installed value back out of
  `sessionStorage`. **The launch route's own silent challenge resolves it too since 2026-10-01:** web's `LaunchMachine` runs in the launch wasm chunk, and
  each chunk is its own linear memory, so the copy of the Rust seam's `static` that
  `OnboardingMachine::set_provider_base_urls` installs (the onboarding chunk's) never reached the
  one `WsAuthConnector` reads. The launch chunk's `LaunchMachine` constructor now seeds its copy
  from the same `sessionStorage` key (`libs/fauna-wasm-launch`, `test-helpers` only, beside the
  launch-clock seed). Until then a tab whose stored `nest_url` was not itself dialable — a typed
  loopback handle resolves to `https://`, a harness nest serves plain HTTP — failed every silent
  challenge on the launch route; it stayed unseen because a signed-in web reload does not run the
  launch machine, and no web journey had claimed a box through the override and then re-entered
  that route. Proven by `test_box_recovery_two_nest.py`'s linked-box journey on `--app web`.
  **Not yet provable by `test_admin_claim_with_real_domain_handle_auto_enables_
  mail` regardless** — that file's own module-level guard already skips it for any `--app` outside
  {linux, macOS, iOS, tui}, because every test in it also needs to *drive the admin-claim onboarding
  UI itself*, which web (and windows, android) do not yet automate — a separate, larger, already-
  tracked gap (this file's `skip_unbuilt("the believable admin-claim onboarding UI drive")`), not
  this seam.
- **Still owed on the other two** (macOS, iOS): each owes the same one-call
  swap at its own store read, and neither needs a new seam — macOS/iOS re-dial through
  `FaunaMacApp.runLaunch()` / `FaunaApp.runLaunch()` over `RegistryLaunchPersistence`, the same
  shape linux has. **android LANDED 2026-08-21:** `AppLaunchVM.connectActiveSession()`
  — the one site that turns `credentialStore.nodeUrl` into `apiClient.nodeUrl`, covering both cold
  boot and account-switch reconnect — now wraps it in `resolvedDialUrl()`, a new UniFFI free-function
  export of `fauna_launch_machine::dial::resolved_dial_url` (`OnboardingMachine::resolved_nest_dial_
  url` was already a method twin for the onboarding machine's own callers; this is the same seam
  reached from a plain store-read site). Compile-verified (`:app:compileDebugKotlin`); no e2e proof is
  possible on a Linux dev machine with no Android emulator attached — android e2e needs one, the
  standing gap every android e2e-adjacent row carries. **windows LANDED 2026-08-25:** `App.xaml.cs`
  wraps its two store-read connection sites (the cold-boot connect and `StartMainAppAsync`'s
  post-`Online` rebuild — this app has two, not one, since a returning-user relaunch reconnects a
  second time once the launch machine confirms the session) in
  `FaunaLaunchMachineMethods.ResolvedDialUrl(baseUrl)`, leaving `baseUrl` itself — every other
  reader, including the stored/serialized value — untouched. Build- and unit-test-verified
  (`windows-debug`, `windows-cs-test`); e2e proof is blocked the same way web's is (windows doesn't
  yet automate the admin-claim onboarding UI this test also needs to drive). Until each remaining
  app lands, the test is `skip_unbuilt` off tui + linux — a declared, tallied absence, not a silent
  one.

**§ 3b's four intents are consumed from the SHARED machine getters on all 7 apps, and windows'
enable-isolation defect is fixed (2026-08-14) and e2e-proven (2026-08-16).** No app sources these
from a checkbox — the retired page's checkboxes were not relocated (§ 3b above), and every app
reads `caldav_enable_requested()` and its three siblings off the machine (windows via
`OnboardingViewModel.CaldavEnableRequested`, itself `_m.CaldavEnableRequested()`). What windows
alone got wrong was not the *source* but the *application*: it wrapped all four enables in ONE
`try` with the mail branch awaited **first**, so a throw from the mail mint skipped every DAV
enable, and — since mail is the only branch that *mints* (mailbox blobs + MSEK + credential)
rather than flipping a flag — even a successful mint delayed all three DAV flips behind it. That
contradicted § 3b's "each DAV enable fires separately" (line 154). The other six were already
per-subsystem-isolated (linux/tui independently spawned, web per-`void (async () => …)()`,
apple/android per-subsystem `do/catch`). `App.xaml.cs` now gives each subsystem its own
try/catch and runs the three cheap DAV flips **ahead** of the mint. Proven by
`test_caldav_onboarding_derived_enablement.py --app windows` — **3/3 green 2026-08-16** (1:07:49),
all three address-type arms, having been `1 failed, 3 passed` on 2026-08-14. The run also settles
which failure mode had been biting: the agent log shows the capture reading
`mail=True caldav=True carddav=True webdav=True` and mail logging `OK`, not `FAILED`, so
suppression-by-throw is refuted and the coupling was the **ordering delay** — mail measured
~2.81 s behind the DAV flips, which under the old shape was inserted ahead of all three. ⚠ On a
quiet box 2.81 s fits inside the test's 10 s settle window, so that latency *alone* does not
account for the original red; the honest reading is that the old ordering made the DAV enables
hostage to a mint whose duration is unbounded under load, and the fix removes the coupling rather
than widening a budget.

Snapshot of where each app stands against the seven-page wizard, the
shared `LaunchMachine` (all 7 apps today — see each row), and the third
long-term-store slot (`AwaitingManualDns`):

| App  | Wizard pages | LaunchMachine | AwaitingManualDns slot + "Almost ready" surface |
|---------|---|---|---|
| Web     | All 7 implemented (`apps/fauna-web/src/routes/onboarding/+page.svelte`) | **Done** (2026-07-13) — **every** launch row is machine-carried (`lib/wasm-launch.ts` over `libs/fauna-wasm-launch`). The silent-challenge row joined the other two when the `AuthConnector` grew its nest-identity-pin (TOFU) seam: `LaunchPhase::IdentityChanged` renders `launch_identity_changed` and its trust button calls `trust_nest_identity()`. Web's own `runSilentChallenge` classifier — and its re-derivation of the `fauna.setup.status` claimed/unclaimed fallback table — are **deleted**. The three separate per-row gates that each built their own machine (`hasPendingFactoryResetSlot()`, `hasAwaitingDnsSlot()`, `nodeUrlStored`) are collapsed (2026-07-15) into one `createLaunchMachine()` + one `.start()`, routed through a single `applyLaunchPhase()` switch; the two `has*Slot()` helpers are no longer called from the page (kept in `launch-persistence.ts` for an existing unit test) | **DONE** (2026-07-12). 4-field localStorage slot (`fauna_awaiting_dns_*`) → `WebLaunchPersistence.loadAwaitingDns()` → the machine's `WizardAt{AwaitingManualDns}` row; surface rendered + polling in `+page.svelte`. The legacy write-only 3-key slot is **deleted**, and the deferred-DNS exit no longer writes `fauna_node_url` |
| Linux   | All 7 implemented (`apps/fauna-linux/src/views/onboarding/`) | Done — `main.rs:run_silent_challenge_async` constructs `LaunchMachine` per call | **DONE** (2026-07-12). Registry slot (`fauna/{actor}/awaiting_dns`), machine-routed, surface rendered + polling (`views/onboarding/awaiting_manual_dns.rs`). The legacy 3-field libsecret slot is **deleted**. See the caveat below on linux's pre-machine branch |
| macOS   | All 7 implemented (`apps/fauna-apple/Fauna-macOS/Views/Onboarding*`) | Done (2026-07-13) — `FaunaMacApp.runLaunch()` constructs `LaunchMachine` over the shared `RegistryLaunchPersistence` (via `FaunaAccounts.bootLaunchPersistence()`; was the hand-rolled `KeychainLaunchPersistence` until the 2026-07-14 registry cutover) and routes on `LaunchSnapshot.phase`. The hand-rolled four-case Swift table (`checkKeychainOnLaunch`) and the bespoke silent-challenge classifier (`runSilentChallengeGate`) are **deleted**, so CR-2's boot reconcile and `LaunchPhase::IdentityChanged` are inherited rather than re-derived. `launch_identity_changed` renders (shared FaunaKit `LaunchIdentityChangedView`), its trust button calling `trustNestIdentity()` on the held machine | **DONE** (2026-07-19). The Keychain slot grew to the shared 4-field shape: `KeychainStore.saveAwaitingDns` writes `awaiting_dns_{nest_url,handle,records_json,claim_code}` and `KeychainSecretStore.legacyKeyMap` now maps all four (was only `records_json`/`claim_code`), so `FaunaAccounts.launchPersistence().loadAwaitingDns()` composes a present record (was `None`) and the machine's `AwaitingManualDns` row fires; `FaunaMacApp.seedWizard`'s `.awaitingManualDns` arm seeds identity + `seedAwaitingManualDnsJson(...)`; surface rendered + polling in the **shared FaunaKit `AwaitingManualDnsView`** (10s cadence, consumed by both apple apps). The writer dropped its `.nodeUrl` write (trap a) and its hand-rolled records JSON for `awaitingDnsRecordsJson()` (trap c). e2e GREEN `--app macos` (`test_awaiting_manual_dns.py`); the round-trip is pinned by the `fullAwaitingDnsSlotComposesAsPresent` FaunaKit unit test |
| iOS     | All 7 implemented (`apps/fauna-apple/Fauna-iOS/Views/Onboarding*`) | Done (2026-07-13) — same cutover, same shared FaunaKit surface (`FaunaApp.runLaunch()`). This is also what gave iOS its silent-challenge fallbacks **at all**: pre-cutover a not-registered result fell out of an `if let` and left the user sitting in the app on a nest that had never heard of them. iOS **keeps its optimistic entry** (it renders the home tabs off the cached identity and lets the machine's verdict correct course, backing the session out on a `WizardAt`/`IdentityChanged` verdict) — `isOnboarding` defaults to `true`, so awaiting the machine before the first render would flash the wizard on every launch. That is the one deliberate macOS/iOS launch divergence. **Backing out is bounded by § App-launch routing → *A verdict renders only while its own launch is still the current one*** (2026-09-21): both apple targets check the verdict against the launch `LaunchMachineBox` still holds, so a verdict in flight when the switcher / remove-account promote / factory-reset re-onboard / e2e session patch establishes a newer session is dropped instead of tearing that session down. iOS needs it most — it has already entered optimistically, so a superseded verdict there un-authenticates a live shell rather than merely misrouting a wizard | **DONE** (2026-07-19) — the identical fix as macOS: the store adapter (`KeychainStore`/`KeychainSecretStore`), the writer + slot-clear (`OnboardingVM`), the seedWizard arm (`FaunaApp`), and the surface (**shared FaunaKit `AwaitingManualDnsView`**) are all shared or line-for-line mirrored, and `WelcomeView`'s `.done` router renders the surface off `wizardOutcome() == AwaitingManualDns`. Build-verified (the `FaunaiOS` target compiles under `swift-test`); the `--app ios` e2e leg is verified separately (a cold ios run needs the full multi-slice apple-ffi build) |
| Windows | All 7 implemented (`apps/fauna-windows/FaunaApp/FaunaApp/Views/`) | Done — `App.xaml.cs:OnLaunched` constructs `LaunchMachine` over `SecretStoreLaunchPersistence` | **DONE** (2026-07-13). `ISecretStore.AwaitingDnsRecord` grew from 3 fields to the shared 4-field shape (added `Handle`) → `SecretStoreLaunchPersistence.LoadAwaitingDns()` now returns a real record (ctor pre-load, same pattern as `LoadPendingInvite`) instead of the hardcoded `null` it had carried since the trait grew the method; a new `LaunchWizardEntry.AwaitingManualDns` case in `App.xaml.cs` seeds the wizard; surface rendered + polling in `Views/Onboarding/AwaitingManualDnsView.xaml(.cs)` (a 10s `DispatcherTimer`, mirroring linux/tui/android's client-owned cadence). Fixed two live bugs in the pre-existing same-session save path found en route: it hand-rolled the records JSON via `JsonSerializer` over the UniFFI-bound `DnsRecordPlain[]` (camelCase) instead of `awaiting_dns_records_json()` (silently round-tripped to an empty list), and it called `SaveNestUrl` at the deferred-DNS exit (would have raced this row's own not-yet-reachable nest against the next launch's silent-challenge fast path) |
| Android | All 7 implemented (`apps/fauna-android/app/src/main/java/com/fauna/app/ui/screen/onboarding/`) | Done — `ui/viewmodel/AppLaunchVM.kt` owns `LaunchMachine` via `core/LaunchPersistenceImpl.kt` (Hilt-provided). The identity-pin row joined the others 2026-07-13: `LaunchPhase::IdentityChanged` renders `launch_identity_changed` (`LaunchIdentityChangedScreen.kt`) with its trust button calling `trustIdentity()` → `machine.trustNestIdentity()` — the full uniform surface web has (security.md § Implementation status) | **DONE** (2026-07-12). `CredentialStore` awaiting-DNS slot (all 4 fields) → `LaunchPersistenceImpl.loadAwaitingDns()`; `AppLaunchVM.navTargetFor` routes `onboarding/almost-ready`; surface in `ui/screen/onboarding/AwaitingManualDnsScreen.kt`. Compile-verified only — android has no e2e (see § below) |
| tui     | All 7 implemented (`apps/fauna-tui/src/wizard/`) — the §§4-7 DNS/VPS/provisioning/manual-DNS pages landed 2026-07-12, closing the page set. Migrated off the retired storage-mode pages with the six shipped apps (Phase-4 S8.7): `encryption_mode_choice` / `plaintext_mode_consent` deleted, `nat_mode_choice` built. | Done — `launch.rs:start` constructs `LaunchMachine` over `RegistryLaunchPersistence`; `launch_retry` + the non-retry `NeedsUpdate` surface implemented | **DONE — the reference implementation.** Registry slot (`fauna/{actor}/awaiting_dns`), machine-routed, surface rendered + polling |

**§6 top-region price summary — DONE on all 7 apps** (windows last, 2026-09-03). `OnboardingMachine::bill_of_materials()` (shared Rust, UniFFI + WASM exported) and the 3 ui.yaml IDs (`provisioning-price-bom`, `provisioning-bom-domain-line`, `provisioning-bom-vps-line`) exist; Linux's `nest_provisioning` view (`apps/fauna-linux/src/views/onboarding/nest_provisioning.rs`), tui's (`apps/fauna-tui/src/wizard/nest_provisioning.rs::elements`), web's (`apps/fauna-web/src/routes/onboarding/+page.svelte`, via `machine.svelte.ts`'s `billOfMaterialsValue`), Apple's shared `FaunaKit.ProvisioningPriceBom` (both macOS + iOS targets), and Android's `NestProvisioningScreen.kt` (`ProvisioningPriceBom` composable, landed 2026-07-22, compile+Robolectric-verified only — android e2e stays host-emulator-gated) all read the same shared data via the single canonical i18n template pair `onboarding.nest_provisioning.bom_line`/`bom_line_recurring` (consolidated 2026-07-22 — web had independently duplicated these as `onboarding.provision.bom_line_one_time`/`bom_line_recurring` before converging onto the majority shape). The cross-app e2e test hook is `OnboardingMachine::call_machine_method("set_dns_availability_for_test", ...)` (consolidated 2026-07-22 from a since-removed narrower `set_dns_buyable_for_test` duplicate), seeding `provider_status() == UnregisteredBuyable` without a live registrar-quote probe (no wiremock fixture for that exists — same gap `test_dns_config.py` notes). `test_provisioning_progress.py` still covers this in two overlapping test pairs (`test_bom_*`, `@pytest.mark.linux`/`@pytest.mark.tui`-restricted; `test_price_bom_*`, unrestricted) — consolidating them into one marker-widened pair is a small follow-on, not yet done. Windows landed 2026-09-03 (`Views/Onboarding/NestProvisioningView.xaml`'s `provisioning-price-bom` StackPanel over `OnboardingViewModel.ProvisioningBom{Domain,Vps}LineText`, resolved with `Strings.ResolveNested` since `{label}` is itself a key) — closing the page's 7-app set.

**What `LaunchMachine::start()` covers.** The shared machine implements **all five**
**The previously-signed-in row (§ App-launch routing, designed 2026-09-24) — machine + tui + android + linux + web + apple built; the dedicated surface's IDs approved 2026-09-25 and built on tui, android and linux, then web and apple (macOS and iOS) (2026-10-02).** `fauna-launch-machine` lands both arms (launch: `run_silent_challenge_phase`'s `NotRegistered` on a claimed/unreachable probe; mid-session: `refresh_internal`'s `NotRegistered`) in `State::SignInRefused` → `Offline { transient: false }` + `LaunchSnapshot::sign_in_refused` + the localized sentence, and `retry_silent_challenge()` honours that state (pins: `libs/fauna-launch-machine/tests/silent_challenge.rs`, `token_refresh.rs`; the real suspend → refused → restore → retry → online chain over WS: `bins/fauna-nest/tests/launch_machine_auth_roundtrip.rs`). **tui, android, linux and apple** route the field to the full `launch_sign_in_refused` page (tui `LaunchSurface::SignInRefused`, linux `LaunchPhase::SignInRefused` in `apps/fauna-linux/src/views/launch.rs` — checked ahead of the generic `Offline` arm, android `AppLaunchVM.NavTarget.SignInRefused` → `LaunchSignInRefusedScreen`, checked after the account-index refusal and only on an `Offline` phase, apple the one shared FaunaKit `LaunchSignInRefusedView` — macOS `LaunchGate.signInRefused`, iOS `AppState.signInRefusedMessage` — checked after the account-index and superseded rows and ahead of the transient/terminal split, with Retry calling `retrySilentChallenge()` on the held machine and re-dispatching its snapshot): the sentence in `launch-sign-in-refused-notice`, `launch-retry-button`, `launch-fallthrough-button`, title `sign_in_refused_title`. **windows alone still renders the machine's fallback** through its existing `Offline { transient: false }` arm: the sentence in its terminal-offline copy (painted in `error-message` on its needs-update page) with the fallthrough and **no Retry** — honest but under an "update needed" title, and relaunching the app is its retry until the trickle-down lifts the field. **The mid-session leg's client side — shared Rust + tui + linux built (2026-09-25):** the WS-RPC client's post-`4401` re-mint refusal ends the reconnect supervisor as the typed `SupervisorStop::Refused(fauna.auth.not_registered)` whichever bearer minted (`LaunchMachineBearer` carries the machine's verdict as `fauna_nest_http::ApiError::SignInRefused`; `WsChallengeBearer`'s is read off its last-refusal channel), classified by the shared `NestClientError::session_ending_verdict`; tui and linux read `NestClient::supervisor_stop()` on the connection-state pump's `Disconnected` and escalate it — and the background silent sign-in's `SilentSignInVerdict::NotRegistered`, which they no longer swallow — through their existing launch-surface re-entry (the `IdentitySuperseded` path). **macOS and iOS built (2026-09-26):** the same read reaches the UniFFI apps as `FfiNestClient::session_ending_verdict`; FaunaKit's connection-state observer posts it on `Disconnected`, and each target's root routes it — like the post-auth silent sign-in's `not_registered` (a `nil` result), now escalated too — through the one shared `escalateSessionEnding` door (`PostAuthAccountFlows.swift`). **windows built (2026-09-27):** its connection-state pump reads the same `session_ending_verdict` on `Disconnected` and routes every verdict through one `SessionEndingRoute` door (`FaunaApp.Core`), which re-enters launch over the same account; its TTL refresh loop escalates a refused refresh (`Offline` + `sign_in_refused`) through the same door, and smoke E2 runs on windows. **web built (2026-09-28):** both of its channels escalate through `post-auth-escalation.ts`'s `escalateSignInRefused` — the same launch-route re-entry as its supersession verdict. The background silent sign-in's `not_registered` (a `null` result from `silentSignIn`) takes it on a signed-in reload; the WS client's post-`4401` re-mint takes it from `getAuthToken`, which throws the typed `SignInRefusedError` (`auth-errors.ts`) for the home nest's refusal, and whose wire `code` the wasm token provider reads to stop the reconnect loop as `SupervisorStop::Refused` instead of backing off — read typed by `WsRpcClient.sessionEndingVerdict()`, which `rpc.ts`'s connect wait routes through the same door. **android built (2026-09-28):** `ApiClient`'s connection-state pump reads `FfiNestClient.sessionEndingVerdict()` on `Disconnected` and hands the verdict to the authenticated shell, which routes every verdict through `AppLaunchVM.escalateSessionEnding` (a drop, credentials kept) and re-enters launch over the same account; its post-auth silent sign-in escalates a `null` (`not_registered`) result the same way. E2E: smoke E2 (`test_smoke_e2_suspended_while_signed_in_lands_refused_surface_without_relaunch`, tui + linux + web + android + macos + ios + windows) — suspend a signed-in user through the admin API, the refused surface with no relaunch, restore, Retry, back in the app (android and windows check the interim `error-message` shape, then record the notice unbuilt; tui, linux, web, macos and ios read the notice element). Launch-time E2E: `test_onboarding_launch_routing_smoke.py` smoke E (tui, linux, web) — on tui, linux and web it reads the sentence from `launch-sign-in-refused-notice`; on an app still painting it in `error-message` it checks that interim shape, then records the notice element unbuilt.

§ App-launch routing rows: the three hydration branches (`IdentityChoice` /
`HandleEntry` / `InviteRequest`), the `identity + nest_url` silent-challenge row and
its fallback table, and the **awaiting-manual-dns row**: `LaunchPersistence::load_awaiting_dns()`
→ `WizardAt{AwaitingManualDns}`, evaluated **before** the silent-challenge row (§
App-launch routing; § Long-term store contract *Mechanism*). The silent-challenge
row's former mode-unresolved branch (`VerifyReply.storage_mode_pending` →
`WizardAt{EncryptionModeChoice}`) is retired with no replacement (no-modes,
ratified 2026-07-12; § App-launch routing *Admin-claimed, mode-unresolved*):
`storage_mode_pending` was read but never routed on until it left the wire
2026-09-24 (a successful verify lands `Online`), so this row carries no per-app
adoption status any more.

*(This supersedes the position that the awaiting-manual-dns row was "client-side glue
by design". The trait already carried `PendingInviteRecord` — an opaque wizard payload
of exactly the same class — so there was no structural reason to exclude it, and the
per-app alternative meant seven bespoke pre-machine branches. The nest genuinely
cannot report this state, because a DNS-pending nest is unreachable; that argues
against nest-authority, not against the machine carrying the row off a store slot.)*

The **`InviteRequest` hydration row** is listed above as machine-carried, and it
now genuinely is on **tui**: the `InviteSubmitted` exit writes the account
registry's per-actor pending-invite slot (`AccountRegistry::set_pending_invite_json`
— the same slot `RegistryLaunchPersistence::load_pending_invite()` reads), so the
shared `LaunchMachine` routes the relaunch to `InviteRequest` on its own with no
pre-machine branch, and the `nest_url.is_some()` split in `launch::route_wizard_entry`
disambiguates it from the silent-challenge 404-on-a-claimed-nest fallback. **Linux**
joined tui on this seam 2026-07-12 (`apps/fauna-linux/src/views/onboarding/mod.rs`
now writes via `registry.set_pending_invite_json`; `apps/fauna-linux/src/main.rs`'s
launch-routing reads via `fauna_client_accounts::RegistryLaunchPersistence::
load_pending_invite()` — its old bespoke libsecret-only wrapper and pre-machine
branch are deleted). **Web** joined the registry slot 2026-07-13 (CR-3): `pending-invite-store.ts` is
now a thin camelCase wrapper over the shared `registryLoadPendingInvite`/
`registrySavePendingInvite` (`fauna-client-accounts`, the same per-actor registry
slot linux/tui write) — the old bespoke `fauna_pending_invite_*` localStorage keys
are gone. Web still restores the record through its own pre-machine branch
(`machine.svelte.ts::tryRestorePendingInvite`) rather than the `LaunchMachine`'s
`InviteRequest` hydration row, but the record itself is now the shared,
multi-account-aware registry slot — no app is left on client-local-only
storage.

**The "Almost ready" surface's implementation status → [`onboarding-provisioning.md`](onboarding-provisioning.md) § Implementation status today** (2026-09-06 concept partition).


**The UniFFI apps' legacy-slot gap (found + fixed 2026-07-23).** The *Mechanism
(ratified 2026-07-11)* above — one per-actor registry slot — held on tui, linux and web
but **not on apple, windows or android**: no UniFFI export existed for the per-actor
write, so all three wrote the four **legacy global** keys instead
(apple: `KeychainStore.saveAwaitingDns`). Those compose back only while
`may_read_legacy_globals()` holds (no account index yet) **and** every required field is
non-empty (`legacy_slots::req`) — so the handle-less deferred-DNS exit round-tripped to
*nothing* and the relaunch fell through to `handle_entry`, losing a half-provisioned
nest; and even with a handle the slot was invisible to any user who had ever added a
second account, whose boot mirror actively clears the legacy keys. **apple is fixed**:
`FfiAccountRegistry` gained `persist_awaiting_dns` / `clear_awaiting_dns` /
`persist_pending_invite`, wrapping one shared
`fauna_client_accounts::persist_awaiting_dns` helper (the lift of tui's and linux's
byte-similar copies), and `OnboardingVM` writes both wizard-exit slots through it.
**android is fixed too** (2026-07-23, same day): `OnboardingHost.handleWizardExit`
now writes through `FfiAccountRegistry.persistAwaitingDns` / `persistPendingInvite`
(previously `CredentialStore.saveAwaitingDns` / `savePendingInvite`, the legacy
four-field write), and clears both real slots at the `LoggedIn` claim terminal via
`registry.clearAwaitingDns()` / `LaunchPersistence.deletePendingInvite()` (the legacy
deletes are kept too, sweeping any row an older build left behind). The relaunch
read side had the same bug in reverse: `AppLaunchVM.navTargetFor`'s
`AWAITING_MANUAL_DNS` / `INVITE_REQUEST` arms read `CredentialStore.loadAwaitingDns()`
/ `loadPendingInvite()` — the legacy slot the fixed write path no longer
populates — so both arms now read through the same registry-backed
`LaunchPersistence` the machine branched on to pick that entry, mirroring apple's
`FaunaApp.swift` launch-routing shape. Pinned by
`OnboardingWizardExitPersistenceTest` (FFI-real, `just android-host-test`) — the
handle-less deferred-DNS case and the multi-account case, mirroring apple's
`KeychainSecretStoreTests.swift` — plus `AppLaunchVMTest`'s new
`wizardAtAwaitingManualDns_*` cases. **windows is fixed too** (2026-07-23,
same day): `OnboardingViewModel.HandleWizardOutcome`'s
`AwaitingManualDns`/`InviteSubmitted` arms now also call
`IFfiAccountRegistry.PersistAwaitingDns`/`PersistPendingInvite`, and its
`LoggedIn` claim terminal clears both via `ClearAwaitingDns()` /
`LaunchPersistence().DeletePendingInvite()` — the legacy `ISecretStore`
Save\*/Delete\* calls stay alongside these unconditionally, both because
windows' own launch-routing pre-check in `App.xaml.cs` still reads the legacy
pending-invite slot directly (a distinct C# fast path ahead of the
`LaunchMachine`, unrelated to this gap — a fresh write now populates both) and
to keep sweeping any row an older build left behind. (Superseded 2026-09-25: the
legacy `ISecretStore` writes and that C# pre-check are gone — windows writes
both slots per-actor only, hydrates the invite resume from the machine's
`WizardAt(InviteRequest)`, and spends both at `LoggedIn` through the shared
`PersistLoggedIn`.) Unlike apple/android,
windows had no read-side counterpart bug to fix: its launch routing already
went through `LaunchMachine` over the registry-backed `LaunchPersistence`
everywhere else. Pinned by `OnboardingWizardExitPersistenceTests` (FFI-real,
`dotnet test FaunaApp.Tests`) — the handle-less deferred-DNS case and the
multi-account case for both slots, mirroring apple's
`KeychainSecretStoreTests.swift` / android's `OnboardingWizardExitPersistenceTest.kt`.
Android has not yet joined `test_onboarding_launch_routing_smoke.py`'s
cross-app `NATIVE_ONLY` e2e module (case **I**) — unlike apple's
`AppleFileCredStore`, android's e2e driver seeds credentials over the bridge's
`seed_credentials` HTTP param rather than a `credential_dir` the app reads
directly, so its adapter is a genuinely different shape; captured as a residual rather than built speculatively here. Windows is in the
same boat — no `cred_store` adapter either.

**Web's launch chunk.** `libs/fauna-wasm-launch` — the `LaunchMachine`'s JS bindings,
including a *reflective* `loadAwaitingDns` lookup so a persistence object written
before the row existed reads as an empty slot rather than throwing — shipped its code
but had **no `wasm-pack` recipe and no consumer**: nothing ever built
it for wasm32 and no `static/` artifact existed for the SPA to import. It went live
2026-07-12 with the web leg (`just wasm-launch`, in both `just wasm` and `just
web-test`).

**Three shared getters exist so no app hand-rolls the slot's JSON** (added
2026-07-12 with the linux/android legs):
`awaiting_dns_records_json()` (write side), `seed_awaiting_manual_dns_json(...)`
(read side — the seeder taking the slot's `dns_records_json` verbatim), and
`awaiting_dns_records_text()` (the one formatter every app renders *and*
copies). The first two matter because serde emits `record_type` while the
UniFFI/WASM bindings expose `recordType`: an app that serialized or parsed the
*bound* type by hand would silently round-trip the records to an **empty list**,
leaving the user an "Almost ready" page with nothing to add — a failure invisible
until a relaunch. Pinned by
`libs/fauna-onboarding-machine/tests/continue_from_dns_post_instructions.rs`.

Per-app adoption is **small, because the row is machine-carried**: an app that
already constructs `LaunchMachine` needs only (a) to persist `AwaitingDnsRecord` —
all four fields, `handle` included — at its `AwaitingManualDns` wizard exit, (b) a
`WizardAt{AwaitingManualDns}` entry-handling leg that seeds the wizard, and (c) the
surface itself. Until an app adopts it, its deferred-DNS exit leaves the user on
`dns_post_instructions`, and its legacy 3-field slot (above) stays dead storage — it
cannot satisfy the seeder, which requires the `handle`.

> **⚠ "Already constructs `LaunchMachine`" is not the same as "reaches the
> awaiting-DNS row" — check the *guard*, not just the constructor.** Linux
> constructs the machine only on the `nest_url`-present path
> (`main.rs`'s five-case launch match), but the deferred-DNS exit deliberately
> persists **no** `nest_url` — so the row was structurally unreachable there and
> the relaunch fell through the legacy branch to `handle_entry`, discarding the
> half-provisioned nest. Linux now tests the registry slot *before* the
> silent-challenge case and hands that case to the machine (`has_awaiting_dns_slot()`),
> which is why its Case-I test passes. **Web** adopted the *same* gate 2026-07-12
> (`hasAwaitingDnsSlot()` checked ahead of a separate silent-challenge branch; the
> separate per-row branches were collapsed 2026-07-15 into the single
> `LaunchMachine.start()` routing, § Implementation status today) — note web's
> trap was the mirror image: its deferred-DNS exit *wrote* `fauna_node_url`, so the
> next launch silent-challenged a nest whose DNS had not propagated, fell through to
> `handle_entry`, and discarded the provisioned nest. It no longer writes it.
> **Apple** (like Windows) constructs it unconditionally and inherits the row —
> Apple's `LaunchMachine` cutover landed 2026-07-13, and its awaiting-dns-slot
> completion is the 2026-07-19 row in the table above. Any app whose launch
> routing is gated on a stored `nest_url` has this same trap.
>
> **Corollary, now resolved.** This paragraph used to flag two remaining
> migrations — web's other launch rows and Apple's `LaunchMachine` adoption —
> as blocked on a missing seam: the shared `AuthConnector` (`connector.rs`)
> modeled only success / not-registered / transient / outdated, with **no
> nest-identity-pin (TOFU) seam**, while web shipped a security-relevant
> `launch_identity_changed` surface off `NestIdentityChangedError` that a
> wholesale swap would have silently dropped. The seam was grown in shared
> Rust first (`LaunchPhase::IdentityChanged`), so both web (2026-07-13) and
> Apple (2026-07-13, `trustNestIdentity()`) inherited the warning when they
> migrated rather than losing it — see their rows in § Implementation status
> today, both now fully on `LaunchMachine`.

The cross-app `LaunchMachine` migration is tracked internally.
The legacy 2-step
device-authorization flow (`/auth/device-code`, `/auth/device-token`,
`/auth/device-verify`) is gone — retired from the nest; every app's `device_auth.*` module was deleted in the
2026-04-22 app sweep.

## Manual smoke tests

These launch / persistence paths were historically not exercisable
through the default e2e drivers, which isolate each launch with a
*fresh per-launch* `FAUNA_KEYRING_APP` namespace **and** a *fresh
per-launch* `FAUNA_E2E_CREDENTIAL_DIR` file store, so neither the
identity trio nor the pending-invite slot survives a force-quit +
relaunch. **As of 2026-06-28 they are AUTOMATED** by
`tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py`
(tier_3), now **cross-app** (2026-07-15) over the shared
`tests/common/launch_harness.py`: **linux** rides the driver's
`use_real_keyring` mode (a STABLE namespace + STABLE XDG dirs on a real
Secret Service the test's `LibsecretCredStore` runs privately for itself —
since 2026-09-15 never the desktop keyring, so it skips only where
`gnome-keyring-daemon` is not installed; `e2e-launch-isolation.md` convention
10); **tui** pins the shared file backend (headless-safe, no
skip); and **web** joins the seed-and-launch routing cases **B/C/D/E/F**
(its `LaunchMachine` dials the injected `node_url` cross-origin, so a
browser reaches the same routing landings a native binary does). Cases
**G/A/I** stay native (G is a structural web dead-end — `reset()` sweeps
localStorage as the test agent's own cleanup; A/I are already covered on
web by `tests/web/test_pending_invite_persistence.py` +
`tests/web/test_awaiting_dns_persistence.py`). The internal dev-setup
notes document the equivalent manual procedure as the truest-to-production
cross-check:

- **A** — a submitted pending invite survives force-quit → relaunch
  rehydrates `invite_request` at PendingReview (drives the real submit;
  `test_smoke_a_pending_invite_survives_force_quit`). Since the pending-invite
  slot moved onto the shared, file-backable `AccountRegistry` (2026-07-12),
  this case alone no longer needs `use_real_keyring` even on linux: it
  runs against a file-backed `LinuxFileCredStore` fixture
  (`headless_credential_store` marker), so its linux arm needs no keyring
  daemon at all — unlike the B–F *linux* arm below (which runs its private
  `gnome-keyring-daemon`; the B–F tui + web arms need nothing).
- **B** — a registered identity relaunches straight to the main app
  (Online), no wizard — the launch-routing half of "redemption +
  identity-skip"; `test_smoke_b_registered_identity_relaunches_to_main_app`.
  (The redeem *drive* — an OOB code redeemed through the real UI to
  `LoggedIn` — is `tests/e2e-unified/tests/test_bearer_cache_web.py`; its
  wire shape is pinned by the onboarding-machine unit test
  `redeem_invite_registers_the_bare_local_part_signed_over_the_handle_domain`.
  ⚠ This used to read "covered by `test_invite_request_*` + the
  `fauna-onboarding-machine` unit tests" — false, and it hid a redeem that
  was broken on every app; see the § Done-definition bullet above.)
- **C** — silent-challenge transient (connection-refused) → retry
  surface; `test_smoke_c_unreachable_refused_shows_retry_surface`.
- **D** — silent-challenge unreachable, **including a DNS that does not
  resolve**, → the **retry surface** — the authoritative fallback-table
  outcome (row in § App-launch routing): a reachability fault the app
  can't reliably classify always offers Retry, it is **not** an
  automatic handle_entry. (handle_entry is reached only via the retry
  surface's "Use a different nest" fallthrough = **F**.)
  `test_smoke_d_unreachable_dns_fail_shows_retry_surface`.
- **E** — silent-challenge 404 on a claimed nest → invite_request;
  `test_smoke_e_unregistered_on_claimed_nest_routes_to_invite_request`.
- **F** — "Use a different nest" fallthrough on the retry surface →
  handle_entry; `test_smoke_f_retry_surface_fallthrough_goes_to_handle_entry`.

The persistence layer the smokes ultimately exercise is *also* covered
by the Rust round-trip test
`libs/fauna-client-accounts/src/launch_persistence.rs::pending_invite_round_trips_for_active_account`
(the shared `AccountRegistry` slot Linux and tui both now write through —
`apps/fauna-linux/src/client.rs`'s old bespoke libsecret-only
`pending_invite_persistence_tests` module was deleted 2026-07-12 when Linux
moved onto this seam) and the
launch-routing logic by `libs/fauna-launch-machine`'s 46+ unit tests —
the new e2e module closes the loop end-to-end through the real binary +
real libsecret + a real force-quit. All seven apps have since adopted
`LaunchMachine` (§ Implementation status today), but this shared
force-quit/relaunch smoke harness still covers only linux/tui/web (`WEB_FIT`/
`NATIVE_ONLY` in `test_onboarding_launch_routing_smoke.py`) — windows, android,
and apple are pinned instead by their own per-app unit/e2e tests (see their
rows above). Extending the cross-app harness to those three remains
tracked internally ("Definition of
done" for the per-app launch-state branches).

## FAQ

**Q: How does the wizard know whether to show invite_request or skip
to dns_config?**
A: `submit_handle_check_continue()` reads the handle-check outcome
and routes — `RegisteredNoNest` (with control checkbox) or
`DomainAvailable` → `dns_config`; `NestRunningUserUnregistered` →
`invite_request`; `AlreadyOnNest` → exits with `LoggedIn`. The
app doesn't make this decision.

**Q: What happens if the user closes the app mid-provisioning?**
A: Already-created VPS / DNS resources stay (the orchestrator
doesn't tear them down). The pending-provisioning state IS
persisted (built 2026-08-29, wired on all seven apps): the machine
writes the pending-provision slot before `create_server`
(`mint_and_persist_pending_provision`, `fauna-launch-machine`), so a
relaunch resumes the run instead of orphaning the box (this answer
previously still read *not persisted*; corrected 2026-09-19). Owner:
[`onboarding-provisioning.md`](onboarding-provisioning.md)
§ Implementation status today → *The pending-provision slot*.

**Q: Why is `buy_domain` machine-derived rather than user-toggled?**
A: The `handle_check` outcome already determines whether the domain
is available to buy or already owned. Letting the user toggle
buy-domain manually would let them pick "buy" for a domain they
already own (which would fail at the registrar) or "don't buy" for
an unowned domain (which would fail at provisioning). The machine
sets the right value; the user can override via
`toggle_buy_domain(on)` only after they've seen the snapshot.

**Q: How does the wizard handle handle conflicts (taken handle on
the user's chosen domain)?**
A: The handle-check probe surfaces `AlreadyOnNest{handle_differs}`
when the domain is registered with a different handle owner; the
message panel shows the conflict, Continue is disabled, and the user
edits the handle to retry.

**Q: What happens after the user submits an invite request —
where's the `InviteSubmitted` outcome?**
A: Retired (2026-08-11, § Wizard exit handling). Submitting is not
an exit: the app persists the pending-invite slot when
`wizard_submit_invite_request()` returns with the snapshot in
`PendingReview`, and the wizard stays on `invite_request` — the
page auto-polls and advances to `LoggedIn` on its own once the
admin admits the requester (§ The pending-invite surface). On next
launch, Case 2 reads the pending-invite slot and rehydrates the
wizard at `invite_request` with the snapshot in `PendingReview` —
the same surface, one code path.

**Q: Where do I add a new DNS or VPS provider?**
A: Edit `i18n/providers.yaml`, run `just providers-generate`,
implement the relevant trait (`DnsProvider` / `VpsProvider` /
`Registrar`) in `libs/fauna-provisioning/src/{dns,vps,registrar}/`,
add wiremock stubs (`Mock`/`MockServer` from the `wiremock` crate,
inline in the Rust test — there is no separate fixtures directory)
to the matching `libs/fauna-provisioning/tests/{dns,registrar}_conformance.rs`
harness. The wizard,
ui.yaml, and the cross-app parity test pick up the new provider
automatically — no per-app UI changes.

---

## Reading list (in priority order)

1. `principles.md` — product invariants + engineering principles.
2. The canonical user-facing flow (ratified 2026-04-27; tracked internally).
3. The implementation design (ratified 2026-04-28; tracked internally).
4. The provisioning-progress design (ratified 2026-04-27; tracked internally) — page 6 snapshot shape, four-step model, idempotency, retry/cancel.
5. The onboarding-persistence-cleanup design (ratified 2026-04-28; tracked internally) — wizard-as-pure-decision-machine model.
6. This document.
7. `tests/e2e-unified/ui.yaml` — onboarding section (search `# === Onboarding ===`).
8. `libs/fauna-onboarding-machine/src/snapshots/handle_check.rs` and `invite_request.rs` — snapshot types.
9. `libs/fauna-onboarding-machine/src/outcome.rs` — `WizardOutcome`.
10. `libs/fauna-onboarding-machine/src/machine.rs` — public methods.
11. `tests/e2e-unified/tests/test_handle_entry_outcomes.py` — the gating test file.
12. `tests/e2e-unified/ui-actual-<app>.yaml` — current shape; you will refresh.
