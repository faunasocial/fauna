# Mail credentials — target state

Owns: mail-credentials, msek
Status: ratified
Authority: client-side per-credential lifecycle — trigger taxonomy, MSEK client-side lifecycle (generation, persistence in the `fauna.state.mail` custody, rotation/recovery), KDF choice per credential type, MUA-username convention, auto-enable-for-new-users, cross-deployment uniformity. Defers wire crypto to the wrapped-blob crypto design (ratified 2026-05-08; tracked internally); key taxonomy/audience to `architecture/owner-key-material.md` § Path B; bridge AUTH to `behavior/imap-server.md` § Authentication; page UX + element IDs to `ui/mail-settings.md` + `tests/e2e-unified/ui.yaml`; admin-side DKIM/TLS to `behavior/mail-bridge-lifecycle.md` § DKIM provisioning (automatic) + § TLS provisioning (three paths).

> **Audience:** every per-app implementation of the `mail-settings` page; anyone touching the shared `libs/fauna-client-mail-settings/` state machine; future onboarding-machine work that may want to bundle "Enable mail" into the wizard.
> **Purpose:** how a user (or admin) sets up, manages, and rotates the per-credential wrapped-blob ecosystem that the MDA bridge consumes at MUA-AUTH per `docs/goal/behavior/imap-server.md` § Authentication. Covers the trigger taxonomy, the MSEK lifecycle on the client side, KDF choice per credential type, the MUA-username convention, rotation/recovery flows, and the cross-deployment uniformity property (§ Mode-uniform behavior — the retired storage-mode axis never forked this flow; `nest/storage-modes.md`). (The nest-side `provision_*` / `fetch_*` / `revoke_*` surface, the shared state machine, and the per-app UI are all shipped — see the per-section implementation-status blocks.)

---

## Goal

A user with a Fauna account can enable mail (IMAP + SMTP) for their account from any of their Fauna apps in one short flow, configure additional MUA credentials over time, rotate keys after a suspected compromise, and revoke credentials they no longer need — without any CLI involvement, and without re-doing the flow when they switch to a different Fauna app. The user provisions; nest stores opaque ciphertext; the MDA bridge unwraps at MUA-AUTH per `docs/goal/behavior/imap-server.md` § Authentication; the property is end-to-end.

**Bar: one click in account settings → one credential dialog → working IMAP/SMTP.** No "you have to enable encryption first," no "make sure your nest admin has approved your client," no copy-pasting hex blobs between devices. The mail-settings page is the single touchpoint for everything in this doc; everything else happens silently in shared Rust.

---

## Trigger taxonomy

Every user (or admin) action that mints a wrapped-blob on nest. Background tasks that mint silently are listed too — they're the same shape minus the UI touchpoint.

| Trigger | Surface | Blobs minted | Where the wrap happens |
|---|---|---|---|
| **Admin claim** (the pre-identity WS-RPC kind `fauna.auth.claim_admin` — its HTTP twin was deleted in S4d) | Onboarding wizard | **None.** Admin claim establishes the actor; it does not provision any wrapped-blob (there is no storage-mode step to establish either — `nest/storage-modes.md` § What replaced each piece of the axis; the wizard advances straight to `nat_mode_choice`). The first wrapped-MLS-blob on a fresh deployment is minted later, when whoever-first-enables-mail (typically the admin) goes through the flow. The onboarding final screen offers a non-blocking link to `mail-settings` for the admin who wants mail immediately. | n/a |
| **Enable mail** for an actor (first credential) | `mail-settings` page → "Enable mail" toggle → add-credential dialog | `WrappedMsekBlob` (per credential_id), `MlsSnapshotBlob` (per actor), `WrappedSubmissionTokenBlob` (per credential_id), + `provision_recipient_mls_pubkey` (per actor, user-self-registered) | `libs/fauna-client-mail-settings/` on the client the user clicked the button on; the user's identity seed (held by the client) signs the SubmissionToken. |
| **Add a credential** to an already-mail-enabled actor | `mail-settings` page → "Add credential" button | `WrappedMsekBlob` (new credential_id, same MSEK), `WrappedSubmissionTokenBlob` (new credential_id) | Same crate; MSEK is read from the account's mail custody (`fauna.state.mail`). |
| **Enable CalDAV** (calendar-only, no email) | `mail-settings` page CalDAV section (`caldav-server.md` § Independent enablement) | The **read recipe only** — `WrappedMsekBlob` (default credential), `MlsSnapshotBlob`, `provision_recipient_mls_pubkey`; **no** `WrappedSubmissionTokenBlob` (no SMTP submission). Sets `caldav_enabled = true` (and `mail_enabled = Some(false)` when email isn't on); reuses the shared `msek` if one exists. | `MailSettingsMachine::enable_caldav_mailbox` (`libs/fauna-client-mail-settings/src/machine.rs`). |
| **Enable CardDAV** (contacts-only) | `mail-settings` page — the contacts twin (`carddav-server.md` § Independent enablement) | Same read-recipe shape as Enable CalDAV; sets `carddav_enabled = true`. | `MailSettingsMachine::enable_carddav_mailbox` — delegates to the same read-recipe mint. |
| **Disable mail** | `mail-settings` page → disable | **None minted.** Sets `mail_enabled = Some(false)`; **preserves** the shared `msek` (and the read-recipe blobs) when `caldav_enabled`/`carddav_enabled` still need it — one MSEK serves IMAP + SMTP + CalDAV + CardDAV. **Email-only (no CalDAV/CardDAV): a teardown** — every credential is revoked (the soft-revoke marker on each row, both blob kinds deleted), email is turned off and `pending_rotation` cleared. **The MSEK and its grace window stay, dormant**: the mail custody's state row keeps `msek` present-wins, because the MSEK is irrecoverable key material (`config-dissolution.md` § Phases and gates → *Bounded rows* → *The mail plane*), so no write can clear it. A later re-enable — email or a DAV protocol — mints a fresh credential under the **same** MSEK, under an id no revoked row has held, so mail sealed before the disable stays openable (a teardown is not a rotation; a user who wants the old key gone runs "Rotate mail keys"). | `MailSettingsMachine::disable_mail`. |
| **Provision the mailbox onto a *second nest* the user owns** (the home-with-public-relay home box) | **Automatic** — folded into linking the home/relay box (no separate UI step); the long-term-correct "automation all the way" shape | `MlsSnapshotBlob` (per actor), `WrappedMsekBlob` (default credential), `provision_recipient_mls_pubkey` (per actor) — **all from the same MSEK as the primary**. **No** `WrappedSubmissionTokenBlob` — the home box runs no MTA (`deployment-home-with-public-relay.md` § Outbound mail). | `libs/fauna-client-mail-settings::provision_relay_mailbox` reusing the account's MSEK (the mail custody, `fauna.state.mail`). The mail `bridge_*` blobs do **not** federate between paired nests, so each box is provisioned independently; only the MSEK is shared — read from the account's mail custody and re-sealed onto the peer. The client holds an authenticated connection to both boxes (the linked-nests `connect_peer` seam). |
| **MLS state change** (any commit on any group the user is in) | Background, no UI | `MlsSnapshotBlob` (atomic replace on nest) | `libs/fauna-client-mail-settings/src/snapshot_sync.rs` on any of the user's Fauna apps that's online when the state change occurs. |
| **Submission-token refresh** (default cadence: every 7 days; expiry 30 days) | Background, no UI | `WrappedSubmissionTokenBlob` (per credential_id, atomic replace) | `libs/fauna-client-mail-settings/src/token_refresh.rs` — **skeleton today**: cadence constants + the intended shape only, no per-app task-scheduler wiring on any of the 7 apps yet (`ui/mail-settings.md` § Don't do these). The 30-day token lifetime means the gap is currently covered by the token's own expiry margin, not by an active refresher. |
| **Rotate mail keys** ("hard revoke" — suspected compromise) | `mail-settings` page → "Rotate mail keys" button + confirmation dialog | `WrappedMsekBlob` × N (one per surviving credential), `MlsSnapshotBlob` (new MSEK) | Same crate; resumable on crash per § Rotation and recovery. |
| **Revoke a credential** (retire-old-password) | `mail-settings` page → credential row → "Revoke" | None minted; `revoke_wrapped_mls_blob` + `revoke_wrapped_submission_token` are deletions. | n/a |
| **Identity succession** (the aftermath's MSEK leg — ratified 2026-08-05, **built 2026-08-11**, shared Rust + driven on tui; `succession-aftermath.md` § Re-key scope owns the ruling) | **Automatic** — the successor's post-auth aftermath task, no user gesture; progress in Settings § Recovery kit | `MlsSnapshotBlob` (sealed under fresh MSEK′) + `provision_recipient_mls_pubkey` (successor actor); **zero** `WrappedMsekBlob`s — every pre-succession credential is excluded by construction and its blobs (both kinds) deleted (§ Rotation and recovery → *Succession*) | The successor's client, same crate — a degenerate rotation with an empty survivors list. |
| **Approve a pending bridge** (admin-only) | `admin-bridges-pending` page → approval card | None directly. Bridge enrollment registers the service-user; the bridge's X25519 pubkey becomes available for DKIM / TLS sealing afterwards. | n/a — admin-side, covered by `mail-bridge-lifecycle.md` § Pending approval. |
| **Provision deployment DKIM key** | Automatic — no admin UI (provisioned like TLS/ACME) | none — the key is nest-held (`mail_dkim_keys`, per (domain, selector)) | Automatic; covered by `mail-bridge-lifecycle.md` § DKIM provisioning (automatic). The resulting DKIM TXT record is surfaced + verified on the `admin-dns` page (`dns-management.md`). |
| **TLS cert auto-provisioning** | Background; not user-driven | `TlsCertBlob` × bridges (per (bridge_role, bridge_id, domain)) | Nest's ACME pipeline; covered by `mail-bridge-lifecycle.md` § TLS provisioning (three paths). |

**What is explicitly NOT a trigger:**

- **Enrolling a new Fauna app device.** Adding a laptop or phone to the user's Fauna app fleet goes through identity-seed onboarding per `docs/goal/behavior/devices.md`; the new device gets the identity seed via QR / recovery phrase, becomes a peer of the user's existing fleet, and reads the MSEK from the account's mail custody (`fauna.state.mail`, the account plane's fleet-only rows). It does NOT mint any wrapped-blob. Wrapped-blobs are for **third-party MUAs** (Thunderbird, iOS Mail, Apple Calendar) — not Fauna's own apps. *(Contrast: linking a second **nest** the user owns — the home-with-public-relay home box — **does** re-provision the mailbox there, because the `bridge_*` blobs are per-nest and don't federate. It reuses the same MSEK — no fresh blob from a fresh MSEK — so it is not minting new key material, just re-sealing the existing MSEK onto the new box. See the "Provision the mailbox onto a second nest" trigger above.)*
- ~~**Storage-mode change** (offline migration tool)~~ — moot: the storage-mode axis itself is retired (`nest/storage-modes.md`; the offline `migrate --to` tool was never built and won't be), so there is no deployment posture left to migrate between. Kept here only so a session that remembers the old non-trigger doesn't wonder where it went.

> **Implementation status today (2026-06-12) — the multi-nest "provision the
> mailbox onto a second box" trigger.** The **shared mechanism is landed**:
> `MailSettingsMachine::provision_relay_mailbox` (`ProvisionRelayMailbox`
> dispatch) reuses the account's MSEK + the default credential to
> provision the read recipe (snapshot + recipient-pubkey + wrapped-MSEK, **no**
> submission token) onto the nest the machine is bound to — idempotent
> atomic-replace, errors if mail isn't enabled. Proven by the crate test
> `provision_relay_mailbox_reuses_fleet_msek_across_paired_boxes` (two machines
> sharing one fleet config → same MSEK-derived recipient pubkey on both boxes;
> the home box's wrapped-MSEK unseals to the fleet MSEK). The same-MSEK →
> relay-transport → MDA-decrypt chain composes with the existing tier_3
> `conformance_cross_nest_mail_relay.rs` + tier_4 `test_mail_relay_two_nest.py`.
> **Automatic trigger — wired (shared substrate + linux lead, 2026-06-12).**
> A both-ends `LinkBoth` now fires this on link via a generic `PostLinkHook` on
> `fauna-client-pair`'s `LinkedNestsMachine`: the mail impl
> (`fauna-client-mail-settings`) builds a peer-bound machine (peer nest + the
> *primary's* config store) and dispatches `ProvisionRelayMailbox`, gating on
> mail-enabled (a no-op for content-only multi-homing). Wired once per app via
> the shared `build_linked_nests_machine_with_mail_relay` constructor (linux calls
> it natively; the fauna-ffi export is ready for the native render lifts). Linux is
> the lead; the native lifts + the web second-origin-WS seam are route-3 follow-ons
> (same shape as the existing linked-nests/serve-here lifts; web stays on the
> hook-less builder until the second-origin seam lands). Proven by the tier_3
> two-nest `conformance_cross_nest_relay_provision_client.rs` (link the home box →
> it is auto-provisioned with the public box's MSEK-derived recipient pubkey, no
> submission token) + the `fauna-client-pair` post-link-hook seam tests. So the
> one-action user flow is live: a user links their home box once and relayed mail
> is immediately readable there.
>
> **Native lifts — all four native apps done, all on 2026-06-13.** The
> fauna-ffi mail-relay builder switch — now the combined
> `build_linked_nests_machine_with_mail_relay_and_trust` constructor, which also
> hydrates the `nests` page's trust facet, falling back to the plain hook-less
> machine when the actor secret or keypair derivation is unavailable — is wired
> on windows (`Controls/NestsPanel.xaml.cs`), android
> (`core/ApiClient.kt::buildLinkedNestsMachine`), and macos/ios (the shared
> FaunaKit `LinkedNestsView`/`LinkedNestsVM`,
> `FaunaKit/Sources/FaunaKit/Core/APIClient.swift`) — each embedded in that
> app's `nests` page (apple landed the same day as the windows/android lift
> above, superseding an earlier "apple's linked-nests page doesn't exist yet"
> note here; `linked-nests.md` § Implementation status today). web stays on the
> hook-less builder until the second-origin-WS seam lands.

---

## Auto-enable for new users

**Every user on a mail-enabled deployment gets a working `<handle>@<domain>` mailbox automatically** — the works-out-of-box invariant (`principles.md` § Product invariants) extended from "the box works" to "every user's mailbox works", with **no** admin provisioning and **no** manual user toggle. This is the default; it is a deployment policy the admin can flip and a user can opt out of.

**The whole flow is client-driven — it has to be, because the nest cannot mint a mailbox** (the MSEK is client-held; § MSEK lifecycle). On a freshly-registered user's **first authenticated client setup**, the per-app launch glue auto-runs the same **"Enable mail" trigger** as a manual enable (`MailSettingsMachine::enable_mail`, the shared auto-mint helper, auto-generated password) **iff**:

1. the deployment has mail enabled (`fauna.setup.status` `email_enabled` — the admin's `mail.enabled` toggle), **and**
2. the deployment policy `auto_enable_mail_for_new_users` is on (`fauna.setup.status` `auto_enable_mail_for_new_users`, default-on — `mail-policy-config.md` § Tier-2 *Auto-enable mail for new users*), **and**
3. the actor holds no MSEK yet (none in the mail custody — a dormant one left by a teardown counts, so a user who turned mail off is never turned back on) — so it never re-mints for an existing mailbox.

**No separate nest-side alias step is needed.** The "Enable mail" mint already makes the address routable *and* readable atomically: its `provision_recipient_mls_pubkey` call (User-class self-register) auto-creates the canonical `<handle>@<domain>` exact alias via the nest's `ensure_canonical_handle_alias` (`mail-aliases.md` § Kind 1 — Exact). The inbound resolver (`resolve_recipient` — its exact-match first step) then routes `<handle>@<domain>` to the actor. (`validate_recipient` survives only as the AUTH-time exact-only username resolver, not the inbound RCPT-TO resolver — `smtp-server.md` § Implementation status today.) So auto-enable reuses the *existing* mint path wholesale — there is **no** admin-side alias write and **no** registration-time nest hook; the only new nest surface is the deployment policy flag.

**The generated password is surfaced once** (the one-time reveal on `mail-settings` / first authenticated session, per § Auto-generated bridge password) — auto-mint never mints silently with no way for the user to see the credential.

**Controls (default-on ≠ un-disableable).** The admin flips the deployment default with `fauna.bridges.set_auto_enable_mail_for_new_users` (Admin-class; `mail-policy-config.md`). This is a deployment *default*, never a per-user admin control — the admin never enables/disables mail for an individual user (mail is user-controlled, `admin.md` § Don't do these). A user who doesn't want mail opts out from their own app (disabling their mailbox / the per-actor `actor_mail_serving` serve-here toggle), exactly as a user who never enabled mail in the opt-in model.

> **Implementation status today (2026-06-12).** **Nest half landed:** the deployment policy is a settable-both-ways `mail_auto_enable_new_users` DB singleton (`db/mail_enable.rs`), written by Admin-class `fauna.bridges.set_auto_enable_mail_for_new_users` and surfaced on `fauna.setup.status` (`SetupStatusReply.auto_enable_mail_for_new_users` / `FfiSetupStatus`, unset ⇒ ON). The mint path that creates the canonical alias (`provision_recipient_mls_pubkey` → `ensure_canonical_handle_alias`) is long-shipped. **Admin toggle UI landed (2026-06-12, linux + web):** `admin-mail-auto-enable-new-users-toggle` on the flat `admin-mail` page lets the admin flip the deployment default from their app (shared `MailPolicyMachine` — writes `set_auto_enable_mail_for_new_users`, reads back from `fauna.setup.status`); windows and macos/ios lifted the same machine field/action later the same day, and tui lifted 2026-07-18 with its own flat `admin-mail` sub-page; android's lift is still pending (`mail-policy-config.md` § Tier 2 — admin (box-wide)). **Client auto-mint half — linux landed (2026-06-12):** the shared decision `MailSettingsMachine::auto_enable_mail_for_new_user(deployment_mail_enabled, auto_enable_policy, display_name) -> Option<SecretString>` (applies gates 1–3 and reuses `enable_mail_with_generated_password`; returns `None` — not an error — when a gate fails, so the glue can call it unconditionally) + the linux first-setup launch glue `FaunaClient::provision_mail_at_first_setup` (one entry, fired from the fresh-onboarding-only `launch_main_app_after_signin`; `am_i_admin`-discriminated — the admin claim honors the onboarding enable-email checkbox, a new non-admin user auto-mints iff the two `setup.status` flags + the no-mailbox-yet gate hold). The two flags are injected from `fauna.setup.status` (the mail-settings `NestClient` seam carries no setup.status read). This also **fixed a latent bug**: the linux non-admin invite-redeem path previously auto-minted *ungated* (`enable_email` defaults ON — `EncryptionModeSnapshot::idle()` — and is reset from the handle only on the admin-claim path, so a non-admin redeem reached the mint regardless of the policy flags). **Web + android also landed (2026-06-12):** the same unified `am_i_admin`-discriminated first-setup entry — web's `provisionMailAtFirstSetup` (onboarding `+page.svelte`, fired once at the wizard's terminal `LoggedIn` outcome), backed by a new `WasmMailSettingsMachine::autoEnableMailForNewUser` wasm binding; android's `MailEnableGlueVM::provisionMailAtFirstSetup` (gated on a once-per-fresh-onboarding `OnboardingHost.pendingFirstSetupMail` latch, since android's glue fires from the every-launch authed surface rather than a fresh-onboarding-only hook). Both fixed the same latent ungated-non-admin-mint bug linux did. **Client-driven tier_3 landed (2026-06-13):** `tests/e2e-unified/tests/test_mail_auto_enable_first_setup.py` proves the *Done =* end-to-end on linux — a fresh non-admin user self-registers over open registration (no admin call), the believable onboarding wizard's first-setup auto-mints, and a Python IMAP client AUTH-PLAIN-logs-in at `<handle>@<domain>` with the generated password against a real MDA bridge (the client-minted wrapped-MSEK blob unwraps at the bridge AUTH). It also drove a linux **mail-page refresh-on-show fix** (`apps/fauna-linux/src/settings/mail.rs`): the settings sub-stack is built once at app launch, so the page now re-hydrates `UserConfig` on every show (`connect_map`, mirroring `logs.rs`) — without it the background-minted credential (and its one-time generated password) stayed invisible in the minting session. **Windows also landed (2026-06-13):** the same unified `am_i_admin`-discriminated first-setup entry — windows's first-setup entry (`App.xaml.cs`, fired fresh-onboarding-only at the wizard's `LoggedIn` outcome; since unified into the ONE shared-Rust `apply_post_claim_serving_enablement` call) branches on `fauna.account.am_i_admin`: an admin claim honors the onboarding enable-email checkbox via the prior `EnableMailWithGeneratedPassword` admin auto-mint, a new non-admin user mints via the shared `AutoEnableMailForNewUser` with the two `setup.status` flags injected from `fauna.setup.status`. No latch needed (fresh-onboarding-only; returning users take the launch-machine path, never `OnLoggedIn`), and **no refresh-on-show fix needed** — windows's `SettingsMailPage` sets no `NavigationCacheMode`, so WinUI recreates it on each navigation and the panel re-hydrates `UserConfig` (`Panel_Loaded → LoadAsync → Hydrate`), unlike linux's app-launch-built GTK sub-stack; the auto-minted credential surfaces on first open. Windows never had the latent ungated bug, as its non-admin path previously never fired the auto-mint at all. **macos/ios refresh-on-show prerequisite landed (2026-06-13):** the shared FaunaKit `MailSettingsVM.configure` (both targets) now re-hydrates `UserConfig` on every call — i.e. on every `.task` (re)appear, which SwiftUI re-runs each time the page appears — instead of short-circuiting after the first machine build (its `guard machine == nil else { return }` skipped `hydrate()` on re-appear), so a background-minted credential and its one-time generated password surface in the minting session (mirrors linux's `connect_map` fix; the machine is still built once). **macos/ios also landed (2026-06-16):** the shared FaunaKit `MailEnableGlue` (`provisionMailAtFirstSetup` + `applyPendingCaldavEnable`, both targets — unified into the ONE shared-Rust `applyPostClaimServingEnablement` call) wired the same unified `am_i_admin`-discriminated first-setup entry — an admin claim honors the onboarding enable-email checkbox via `enableMailWithGeneratedPassword`, a new non-admin user mints via the shared `autoEnableMailForNewUser` with the two `setup.status` flags injected from `fauna.setup.status`; CalDAV fires separately via the bridge-approval machine (`.setCalDavEnabled`). Gated on a once-per-fresh-onboarding `SessionState.pendingFirstSetupMail` latch (android-style, since apple's glue fires from the every-launch authed surface — `completeAuthenticatedLaunch` on macOS, the silent-sign-in block on iOS — not a fresh-onboarding-only hook). This also closed a prerequisite **apple gap**: fresh onboarding previously entered the authed UI with a **nil `FaunaClient`** (the client was built only on a *subsequent* launch's silent-challenge gate), so the `.feed` exit now re-runs `checkKeychainOnLaunch()` to build the authed client via the same gate a returning user hits (`OnboardingContainerView` / iOS `WelcomeView` → the new `appState.onLaunchAuthenticated` hook) — unblocking *all* post-onboarding authed actions, not just mail. **Client-driven tier_3 LIVE-verified on macos + ios (2026-06-16/17):** both apple apps now pass the in-process live-e2e proof — `test_mail_auto_enable_first_setup.py` (non-admin first-setup auto-mint → IMAP login) AND `test_mail_enable_at_admin_claim.py` (admin enable-email-at-claim → deployment mail + admin mailbox) green **`--client macos`** AND **`--client ios`** (2026-06-17 — both apple apps drive the same shared FaunaKit `MailEnableGlue`; the two test gates were widened from `linux/macos` to include `is_ios()`). The decision logic is tier_1-proven (`libs/fauna-client-mail-settings/tests/state_machine.rs`). **tui also landed (2026-07-19), last of the seven:** tui's wizard (M2) predated this launch glue on every other app; `mail_glue::{provision_mail_at_first_setup, provision_caldav_mailbox_at_first_setup, set_caldav_enabled}` port the same `am_i_admin`-discriminated first-setup entry from linux's `client.rs`, wired from `wizard::handle_wizard_done`'s `WizardOutcome::LoggedIn` arm — no latch needed (fresh-onboarding-only, mirroring windows). Since Phase-4 S8.7 (2026-07-12) deleted the onboarding enable-{email,caldav} checkboxes without relocation, enablement is machine-derived only (`OnboardingMachine::{email_enable_requested,caldav_enable_requested}` — real-domain-handle AND NAT-mode != Private — no ui.yaml surface). `test_mail_enable_at_admin_claim.py` widened to `--client tui` and green, proving the derived-OFF admin-claim path no-ops correctly. **The non-admin auto-mint path is now also verified on tui** (`test_mail_auto_enable_first_setup.py` widened to `--client tui`, 2026-07-19: full non-admin first-setup auto-mint → IMAP login round-trip against a real MDA bridge). **The positive derived-ON admin-claim path remains unverified — on every native in-process app, not a tui-specific gap:** the native in-process e2e harness has no seam letting a domain-shaped typed handle drive the derivation while the post-claim authenticated session still reaches the local test nest, so the admin auto-mint branch (`is_admin && email_enable_requested()==true`) has never been e2e-proven on any app; the derivation logic itself is unit-tested (`libs/fauna-onboarding-machine/tests/serving_enablement_derivation.rs`) — only the end-to-end glue-fires-through-a-real-client wiring is unproven.

---

## MSEK lifecycle

The MSEK (MLS Storage Encryption Key, 32-byte AEAD key) is the load-bearing client-side piece this doc adds. It exists per actor, persists in the account's mail custody (`fauna.state.mail`, § Persistence below), syncs across the user's Fauna app fleet, and is the unwrap target every wrapped-MSEK blob serves.

**Generation.** Fresh random 32 bytes, sampled on the first Fauna app that runs the "Enable mail" flow for the actor. Source: the platform's cryptographically-secure random generator (`fauna_client_mail_settings::machine::fresh_msek`, which delegates to the project's one minter `fauna_core::secret::fresh_secret_32` — `rand::thread_rng()`, with `getrandom` as the underlying OS-entropy source, notably on the wasm target). Never derived from the identity seed (per `key-material-hierarchy.md` Path B § What MSEK seals — MSEK is unrelated to `BackupKey`, despite both being owner-only; the distinction enables credential rotation without identity rotation).

**Persistence.** In the account's **mail custody**, the `fauna.state.mail` account-plane kind (`config-dissolution.md` § The `__config` dissolution schedule — the kinds table's row and *Bounded rows* → *The mail plane* own its shape): the `msek` field of its one state row (a 32-byte value, optional — held in Rust as the zeroize-on-drop `SecretArray32` custody type, which serializes as a 32-byte CBOR byte string like every fixed-width byte field — `architecture/serialization.md` § Canonical IPLD dag-cbor → *Fixed-size byte arrays*). Fleet-only and sealed under the account's generation tip (`owner-key-material.md` § Path A-sibling-2); the user's Fauna app fleet reads it through the account runtime (`AccountStoreHandle::mail`). Plane-only since the 2026-09-30 consumer cut: no app reads or writes `UserConfig.mail`, and the field itself retired with the `__config` rail on 2026-10-02. Synced across every Fauna app the user owns; **every Fauna app is "primary" for I3 purposes** — the "primary client" framing in the wrapped-blob crypto spec is a UX consideration (showing the wrap UI on the device the user is on), not a load-bearing architectural distinction.

**Lifetime.** Stable for the actor's lifetime, except when the user explicitly rotates via the rotate-mail-keys flow (§ Rotation and recovery). MSEK is not rotated on credential add (the new credential's wrapped-MSEK blob wraps the *same* MSEK under the new credential's KDF). MSEK is not rotated on credential revoke (soft revoke deletes the wrapped-MSEK blob for that credential only; other credentials still unwrap the same MSEK).

**Why store MSEK on the client.** The state machine needs MSEK to wrap under a new credential's KDF when the user adds a credential. Three alternatives considered and rejected:

1. *Re-fetch + re-unwrap on every add-credential.* User would need to type an existing credential to authenticate the add-credential flow. Bad UX for users with multiple MUAs.
2. *Re-generate everything on every add-credential.* Fresh MSEK + fresh snapshot + re-wrap-all-existing-credentials. Wasteful; conflates "add a credential" with "rotate keys."
3. *Persist MSEK in client memory only.* Loses MSEK on logout/restart; user has to re-derive from the identity seed or store somewhere persistent — same problem.

The chosen shape (persist in the owner's own fleet-only custody) preserves the trust property: anyone with the identity seed reads MSEK; nobody else does, including the nest. Audience is unchanged; key-material-hierarchy rule #1 (pick the key by audience) is satisfied.

**Credential secrets in the mail custody.** Adding a credential requires the bytes of that credential to wrap MSEK under it. To support unattended rotation (rotate-mail-keys without re-prompting for every credential's bytes), credential secrets are persisted beside the MSEK — one `credential/<credential_id>` row each — in the schema below. Same trust property — fleet-only, audience = the user's Fauna app fleet. The block is the composite **READ fold** (`MailConfig`: the state row plus every credential row that is not revoked); the rows themselves are `fauna_core::mail_rows::{MailStateRow, MailCredential}`.

```
mail = {
  msek: Option<SecretArray32>,           // zeroize-on-drop custody newtype (fauna_core::secret);
                                         // serializes byte-identically to a serde_bytes [u8; 32]
                                         // (a 32-byte CBOR byte string — pinned by
                                         // merge_policy::tests::the_mail_credential_secret_is_a_cbor_byte_string)
  prior_mseks: [SecretArray32],          // hard-revoke grace keys, most-recent first,
                                         // uncapped since 2026-10-06 — the READ fold of the generation rows (Path B-sibling-2 → Pre-rotation mail at rest)
  credentials: [
    {
      credential_id: String,             // kebab-case from display_name
      display_name: String,              // user-supplied, e.g. "iPhone Mail"
      kind: "plain" | "oauthbearer",
      secret: SecretByteBuf,             // raw credential bytes; zeroized-on-drop newtype
                                         // (fauna_core::secret) encoding as a CBOR byte string
                                         // (re-cut from SecretBytes, an integer array, 2026-09-30
                                         // by fauna.state.mail phase A); empty on a marked row
      created_at: u64,                   // unix seconds
      updated_at: u64,                   // the row's own stamp, microseconds (a Timestamp) —
                                         // written by every credential writer since phase A
      wrapped_under: Option<[u8; 32]>,   // MSEK fingerprint of the generation the row's blobs
                                         // are wrapped under (§ Rotation → The generation marker;
                                         // MsekFingerprint) — None reads as owed a re-wrap
      revoked_at_unix: Option<u64>,      // soft-revoke marker, monotone — the fold hides a
                                         // revoked row, and its id stays spent
      burned: Option<MailSuccessionBurn>,// succession-burn marker, monotone (§ Succession)
    },
    …
  ],
  pending_rotation: Option<{
    new_msek: SecretArray32,             // the incoming MSEK, in custody across the resumable swap;
                                         // the credentials still owed a re-wrap are DERIVED from
                                         // wrapped_under (§ The generation marker), never stored
  }>,
  mail_enabled: Option<bool>,            // email on/off, distinct from holding key material
                                         // (None = never written ⇒ off; merged present-wins)
  caldav_enabled: bool,                  // calendar rides the same shared msek + default credential
  carddav_enabled: bool,                 // contacts sibling of caldav_enabled
  prior_msek_retirements: [{             // retirement instant for each prior_mseks entry, keyed by
    msek: SecretArray32,                 // the retired MSEK itself (never positional — the custody
    retired_at_unix: u64,                // merge unions/reorders prior_mseks); an entry with no
  }],                                    // recorded instant provably predates this field (added
                                         // 2026-07-19) and is treated as pre-flip. Feeds the bounded-
                                         // mail-mint rotation-heal cross-generation coverage; owned
                                         // by `../architecture/encryption-at-rest.md` § Capability
                                         // tiering — this doc only tracks the schema + when it's set
                                         // (§ Hard revoke step (f)).
}
```

Every `msek`-bearing field (`msek`, `prior_mseks`, `pending_rotation.new_msek`, `prior_msek_retirements[].msek`) is held as `SecretArray32` — zeroize-on-drop, redacted `Debug` — per `architecture/key-material-hierarchy.md` § Carrier shape (closed 2026-08-12: `MailConfig`'s MSEK family was the survey's one outstanding non-conformant carrier). The wire shape is unchanged; this is a Rust-side custody hardening, not a schema migration.

The **authoritative field set is `MailConfig`** (`libs/fauna-core/src/data.rs` — the doc-comments there carry the per-field semantics) together with its two row types in `libs/fauna-core/src/mail_rows.rs`; this block is its schema rendering and must track it.

This is a known design choice — credential secrets do live on the user's client fleet, in the fleet-only mail custody. Alternative considered: a separate `__mail` reserved folder with stricter quota / replication. Rejected for v1 — the account plane already has the right audience, the right seal, the right sync mechanism, and the data volume is trivial (kilobytes per actor); inventing a new reserved folder is overhead with no security gain. Re-litigation requires a goal-doc update + plan; see § Don't do these.

---

## KDF choice per credential type

Per the wrapped-blob crypto design (ratified 2026-05-08; tracked internally) § Per-credential wrapped MLS-key blob:

| Credential type | KDF | Parameters (provisioning-time default) |
|---|---|---|
| `PLAIN` (password) | Argon2id v1.3 (RFC 9106) | `m = 65,536` KiB (64 MiB), `t = 2`, `p = 1`, `salt = 16` random bytes, output = 32 bytes (the AEAD key). Parameters serialize into the blob; older blobs unwrap with their own (older) parameters. |
| `OAUTHBEARER` (bearer token) | HKDF-SHA-256 (RFC 5869) | `prk = HKDF-Extract(salt = blob.salt, ikm = token_bytes)`; `info = "fauna.wrapped-blob.oauth.v1" || actor_id || credential_id`; `key = HKDF-Expand(prk, info, 32)`. No cost parameters — OAUTHBEARER tokens are ≥ 128-bit entropy by construction. |

**OAUTHBEARER is preferred.** Recommended in the credential-add UX as the default. Argon2id Interactive is strong but a weak user-chosen password still cracks in days–weeks at attacker budgets; high-entropy bearer tokens close the brute-force door entirely. Both are supported uniformly — the retired storage-mode axis never forked credential-type support (per the candidate (b) ratification in `docs/goal/behavior/imap-server.md` § Authentication).

**OAUTHBEARER token issuer (current shape — v1).** The client generates a fresh ≥ 128-bit random token via shared Rust `fauna_client_mail_settings::password_gen::generate_bridge_token()` (32 random bytes → 64 lowercase hex chars from the OS CSPRNG; surfaced to native apps as the `fauna-ffi` export `generate_bridge_token`), and displays it on the credential-add screen for the user to copy into their MUA. **Nest** never holds a recoverable copy (§ Credential — "never in nest in any recoverable form"); the user's own client does, in its mail custody (`fauna.state.mail`), because unattended rotation requires it — so a user who closes the dialog before copying re-reads the token from the credential row's reveal rather than revoking and re-adding (§ Implementation status → *Secret re-reveal*, landed 2026-06-04, owns this). *(Corrected 2026-07-17: this paragraph and `../ui/mail-settings.md`'s § Errors + § Don't-do-these still described the pre-2026-06-04 shown-once-only shape, three weeks after the reveal shipped on four apps — the client-side "does not persist a recoverable copy" half was never true of the token specifically, only of nest.)* Tracked as a known follow-on (§ OAUTHBEARER issuer (future)) to land an OAuth issuer endpoint on nest that lets MUAs perform the standard OAuth dance against the user's nest — at which point token rotation / revocation / introspection move to standard OAuth surfaces.

### Auto-generated bridge password (PLAIN)

OAUTHBEARER closes the brute-force door, but **CalDAV/CardDAV and many IMAP/SMTP MUAs only speak HTTP/SASL Basic-auth with a password** — the MDA's DAV paths are Basic/PLAIN-only (the shared `bins/fauna-bridges/internal/mda/davauth/auth.go` middleware, serving CalDAV **and** CardDAV, unwraps with `KdfKindArgon2id`). So the PLAIN path must reach the *same* brute-force resistance, because a mailbox/calendar with the bridge enabled is only as resistant-at-rest as this password (it Argon2id-wraps the actor's MLS capability; nest stores both the sealed bodies and the password-wrapped blob, so a stolen DB is offline-brute-forceable down to the password). This is the same property as ProtonMail's mailbox password — and the same reason Proton **Bridge generates a random password** rather than reusing a human-chosen one.

Therefore, when the user picks the PLAIN credential type:

- **The client generates the password by default.** An **"Auto-generate" toggle is ON by default** (`mail-add-credential-autogenerate-toggle`). Shared Rust `fauna_client_mail_settings::password_gen::generate_bridge_password()` produces a random **`a-zA-Z0-9`-only** secret (no special characters, so it pastes cleanly into any MUA field) of **24 characters ≈ 143 bits** of entropy, from the OS CSPRNG (rejection-sampled, unbiased). It is displayed once (`mail-add-credential-password-input`, read-only) for the user to copy into their MUA, exactly like the OAUTHBEARER token reveal; the credential secret persists in the credential's mail-custody row like every other.
- **Turning auto-generate off enables manual entry** and always reveals `mail-add-credential-weak-password-warning` (`warn_manual_password(auto_generate=false, nest_encrypted=true)`): a manually-chosen password bounds the mailbox/calendar's at-rest security, on every nest — there is no deployment posture left where the box owner "can already read the data" so a weak password would add no incremental exposure. `warn_manual_password`'s `nest_encrypted` parameter is a legacy holdover from the retired storage-mode axis: nest-side, `fauna.setup.status`'s `mode` field is now a hardcoded `Some("Encrypted")` shim (`nest/storage-modes.md` § What replaced each piece of the axis), so every caller passes `nest_encrypted=true` and the warning is unconditional, not mode-aware. (The function keeps the parameter rather than dropping it — no client call site needed to change, and the shim is what a future client-side read of `setup_status.mode` would still see.)

The toggle and warning are the same on all 7 apps (priority #1); the generation + warning rule live in shared Rust (priority #2), surfaced to native apps via `fauna-ffi` (`generate_bridge_password` / `warn_manual_bridge_password`) and to web via wasm.

**Mint-once sequencing.** `fauna_client_mail_settings::password_gen::resolve_autogenerated_password(kind, autogenerate)` is the single source for the autogenerate-vs-manual decision: `Some(<freshly-minted secret>)` when `kind` is PLAIN and auto-generate is on, `None` otherwise (including OAUTHBEARER, regardless of the toggle's stale value from a prior PLAIN visit). Every app calls it **only at a settled toggle edge** (the type-selector landing on PLAIN, or the auto-generate toggle flipping) — **never again at submit** — and stores the returned value, so the credential persisted is byte-identical to what the user copied. This is the fix for the sequencing bug class an apple 2026-07-13 build hit twice: re-minting a fresh, different password at submit (because the edge-mint and the submit path called `generate_bridge_password()` independently) and closing the form on an OAUTHBEARER mint before the shown-once token could be copied. Surfaced to native apps via `fauna-ffi`'s `resolve_autogenerated_bridge_password` and to web via wasm's `resolveAutogeneratedBridgePassword`.

---

## MUA setup conventions

The mail-settings page surfaces the connection details every MUA needs, after the user enables mail. Same shape on all 7 apps.

| Field | Value | Notes |
|---|---|---|
| IMAP host | `mail.<nest_domain>` | Derived client-side by the shared `MuaInstructions::for_node_url` (`libs/fauna-client-mail-settings/src/state.rs`) from the nest URL — there is no nest-side hostname knob. |
| IMAP port | 993 (implicit TLS) — recommended; 143 (STARTTLS) — fallback | Per `imap-server.md` § Process topology. |
| SMTP host | same as IMAP host |  |
| SMTP port | 465 (implicit TLS) — recommended; 587 (STARTTLS) — fallback | Per `smtp-server.md`. |
| Username | `<handle>+<credential_id>@<domain>` (RFC 5233 sub-addressing) | The `+<credential_id>` suffix tells the MDA which wrapped-MSEK blob to fetch. For a credential named "default", the suffix is omitted and the username is `<handle>@<domain>`. |
| AUTH mechanism | OAUTHBEARER (recommended) or PLAIN over TLS (legacy MUA fallback) | Both (SASL OAUTHBEARER per RFC 7628, SASL PLAIN per RFC 4616) flow through the same AEAD-unwrap-as-AUTH at the bridge per `imap-server.md` § Authentication; this is today's advertised set on every AUTH surface (IMAP, SMTP submission, DAV Basic — the IMAP `LOGIN <user> <pass>` *command* over TLS rides the same path, but the SASL LOGIN mechanism is not advertised). SCRAM-SHA-256 is a target-state addition for the submission listeners once built (`smtp-server.md` § Auth on each port). |
| Credential | the PLAIN password OR the OAUTHBEARER token the user chose at credential-add time | Stored in the MUA, never in nest in any recoverable form. |

> **Implementation status today (2026-06-03).** The MUA-username → credential_id
> resolution is now **implemented end-to-end** (it was the deferred "I3" gap —
> previously every credential silently collapsed onto a single hardcoded
> `"default"` blob, so a second credential could never authenticate). Two halves
> landed together: (1) nest stores **per-credential** `bridge_wrapped_mls_blobs`
> / `bridge_wrapped_submission_tokens` rows keyed on `(actor_id, credential_id)`
> — the `provision_wrapped_mls_blob` / `provision_wrapped_submission_token` RPCs
> now carry `credential_id` instead of hardcoding `"default"` (matching the
> long-standing `fetch_*` / `revoke_*` sides); (2) the MDA's IMAP + CalDAV PLAIN
> AUTH **and** the MTA submission AUTH resolve the credential_id from the
> username's RFC 5233 `+suffix` via the shared
> `internal/auth.CredentialFromLocalPart` (a bare username maps to `"default"`).
> So a user's second+ credential authenticates at
> `<handle>+<credential_id>@<domain>`. Proven by tier_3
> `tests/e2e-unified/tests/test_mail_multi_credential_auth.py` (a client-minted
> second PLAIN credential authenticates over **both** CalDAV and IMAP) + Go
> `TestCredentialFromLocalPart`.
>
> **Per-credential MUA username display — landed on linux (lead), 2026-06-04.**
> Each credential row now shows its **concrete** `<handle>+<credential_id>@
> <domain>` username (bare `<handle>@<domain>` for `default`) + a copy
> affordance (`mail-settings-credential-item-username` /
> `mail-settings-credential-item-copy-username`), so the user no longer infers
> it from the generic `mail-settings-mua-username-format` template and can't pick
> the wrong (bare) form for a `+suffix` credential. The bug-prone parts (the
> `+<credential_id>` suffix, the `default`→bare rule, the domain) are resolved in
> **shared Rust** (`MuaInstructions::username_for` →
> `MailCredentialSummary::mua_username`, leaving only `{handle}` for the renderer
> to substitute via the shared `resolve_mua_username`), so every app renders
> an identical, correct username (priority #2). Proven by the tier_3
> `test_mail_settings_enable_renders_credential_row` assertion (the row shows a
> concrete `local@domain`, no leftover template placeholder) + shared-Rust unit
> tests for the suffix/default-bare rule. **Landed on web** 2026-06-04 (the Svelte
> credential row over `MailCredentialSummary.mua_username` + the `resolveMuaUsername`
> wasm twin — the old local `+suffix` re-derivation deleted — proven by
> `test_mail_settings_enable_renders_credential_row[web]`). **Android also landed it,
> 2026-06-04** (tracked internally): `MailSettingsScreen.kt`
> `CredentialRow` renders the username + copy over `MailCredentialSummary.mua_username`
> resolved by the `resolveMuaUsername` UniFFI export, passing the **bare** handle
> (`CredentialStore.cachedHandle` stripped of any `@domain` — it is re-qualified after
> factory reset); proven by the Robolectric `MailSettingsContentTest` (resolved-username
> assertion). **Windows also landed it, 2026-06-04** (tracked internally):
> `Controls/MailSettingsPanel`'s credential `DataTemplate` renders
> `mail-settings-credential-item-username` + `-copy-username` over
> `MailCredentialSummary.mua_username` resolved by the `ResolveMuaUsername` UniFFI
> export, passing the logged-in handle (`ISecretStore.LoadCachedHandle()`); verified on
> MSBuild ARM64 + `dotnet test`. **Landed on macos/ios (2026-06-06).**
> `MailSettingsView.credentialRow` (FaunaKit, shared by both targets) renders
> `mail-settings-credential-item-username` + `-copy-username` over
> `MailSettingsVM.muaUsername(_:)` → the same `resolveMuaUsername` UniFFI export
> (`fauna-ffi/src/mail_admin.rs`) the other native apps consume — the
> mail-settings page (and this username row) now ships on all 7 apps.
>
> **Secret re-reveal — landed on linux (lead), 2026-06-04.** Each credential row
> now reveals (`mail-settings-credential-item-reveal-secret` →
> `mail-settings-credential-item-secret`, hidden by default) + copies
> (`mail-settings-credential-item-copy-secret`) its secret (PLAIN password /
> OAUTHBEARER token), so a user can recover the exact secret to (re)configure a
> MUA **without** revoke + re-add. The secret **is** held client-side
> (the credential's row in the fleet-only mail custody, persisted for
> unattended rotation), so this is a pure read via the new
> `MailSettingsMachine::reveal_credential_secret(credential_id) -> SecretString`
> — kept **out** of the passive `MailSettingsSnapshot` (secrets never ride the
> rendered state); this explicit on-demand accessor is the only path that
> surfaces it, mirroring the shown-once reveal at credential-add time. Proven by
> the shared-Rust unit `reveal_credential_secret_round_trips_from_user_config` +
> a tier_3 linux e2e (reveal shows the secret). The row "Credential" entry above
> ("never in nest in any recoverable form") is about **nest** — the box's
> store never holds a recoverable secret; the user's own client *does* (in its
> mail custody, `fauna.state.mail`, by design, for rotation), which is what this reveal reads.
> **Landed on web** 2026-06-04 (the per-row reveal/hide toggle + on-demand
> `revealCredentialSecret` fetch — secret never in the snapshot — proven by
> `test_mail_settings_reveal_credential_secret[web]`). **Android also landed it,
> 2026-06-04** (tracked internally): `CredentialRow` reveals
> (`mail-settings-credential-item-reveal-secret` toggles the hidden `-secret` Text) +
> copies (`-copy-secret`) via `MailSettingsVM.revealSecret` →
> `MailSettingsMachine.revealCredentialSecret` (`SecretString` = `kotlin.String`), the
> FFI kept in the VM so the Content stays JVM-testable; proven by
> `MailSettingsContentTest` (reveal-toggle + copy-secret). **Windows also landed it,
> 2026-06-04** (tracked internally): `MailSettingsPanel`'s credential row
> reveals (`mail-settings-credential-item-reveal-secret` toggles the hidden `-secret`) +
> copies (`-copy-secret`) via `MailSettingsMachine.RevealCredentialSecret`
> (`SecretString` = C# `string`), the secret kept out of the snapshot and
> `MailCredentialItem` made observable (`INotifyPropertyChanged`) so the reveal flips the
> row in place; verified on MSBuild ARM64 + `dotnet test`. **Landed on macos/ios
> (2026-06-06).** `MailSettingsView.secretRow` (FaunaKit, shared by both targets)
> reveals (`mail-settings-credential-item-reveal-secret` toggles the hidden
> `-secret` text) + copies (`-copy-secret`) via `MailSettingsVM.revealSecret` →
> the same `reveal_credential_secret` UniFFI export (`machine.rs`) the other
> native apps consume. **All 7 apps now render both the username and
> secret-reveal rows.**

**Shared `credential_kind_badge` formatter (2026-06-24).** The credential row's
type badge (`mail-settings-credential-item-type`: Password / Bearer token) is
rendered through the canonical `fauna_client_mail_settings::credential_kind_badge(CredentialKind) -> LocalizedText`
— one source of truth for the `CredentialKind`→i18n-key map (`settings.mail.kind_{password,bearer}`),
mirroring `alias_kind_badge` / `member_status_label` / `bridge_display_name`. It
lifts the identical two-arm map that all six apps hand-rolled (priority
#1/#2/#4) — the **last** un-lifted mail-settings enum→label map; closing it fully
single-sources the mail-settings enum→label surface. **linux** consumes it
directly (`.resolve(crate::i18n::strings::lookup)`) and **web** via the wasm twin
`credentialKindBadge` (`resolveLocalized`); both previously hard-coded the English
strings, bypassing i18n — adopting this routes them through the shared catalog.
For the native apps it is `#[uniffi::export]`ed (`fauna-client-mail-settings`
is gated out of the Go mail-bridge `--no-default-features` build, so no
`libs/fauna-mail-go` regen); **android** consumes it in `MailSettingsContent` via
`resolveLocalized(credentialKindBadge(kind))` — a `kindLabel` lambda injected from
the stateful `MailSettingsScreen` into the FFI-free `CredentialRow`, deleting the
local 2-arm `when`-map (2026-06-24, host-.so-bindgen compile-verified +
`MailSettingsContentTest` 19/19 green); **windows** consumes it in
`MailSettingsPanel.ToItem` via `S.Resolve(FaunaClientMailSettingsMethods.CredentialKindBadge(kind))`
(2026-06-24, `windows-debug`-verified); **apple** consumes it in
`MailSettingsView.credentialRow` (`:195`) via
`renderLocalizedText(credentialKindBadge(kind: cred.kind))` — deleting the local
2-arm `kindLabel(_:)` switch, the shared FaunaKit view serving macOS + iOS so one
consume covers both (2026-06-24; `mac-debug` host-binding regen +
`swift-test` 138/4, text-only no-id-change so no ui-actual edit). **All 7 apps
now consume the shared fn — the mail-settings enum→label family is complete.**
Verified by
`fauna_client_mail_settings::state::tests::credential_kind_badge_maps_every_variant`.

---

## Rotation and recovery

### Soft revoke (retire a credential)

`revoke_wrapped_mls_blob(actor_id, credential_id)` + `revoke_wrapped_submission_token(actor_id, credential_id)`. Deletes the per-credential blobs on nest. MSEK is unchanged; other credentials keep working. Sessions already AUTH'd on the retired credential continue holding the unwrapped MSEK in memory until they zeroize at LOGOUT / idle timeout (the wrapped-blob spec § Credential revocation calls this out as the limit of soft revoke). No multi-blob coordination needed.

UX: confirmation dialog ("Revoke this credential? MUAs using it will lose access; other MUAs are unaffected."), then `revoke` actions, then the credential row disappears from the credentials list.

### Hard revoke (suspected compromise)

The rotate-mail-keys flow. Rotates MSEK + re-snapshots + re-wraps every surviving credential. Resumable from any crash. The user has the option to mark specific credentials as "compromised, do not re-wrap" before confirming — those credentials will lose access after the rotation completes.

**Algorithm.**

1. User clicks "Rotate mail keys" → confirmation dialog (with optional "select compromised credentials to exclude" checkbox list) → confirm.
2. Client state machine:
   a. Generate fresh MSEK' ←$ {0,1}^256.
   b. Build new MLS-state snapshot bytes from the current OpenMLS read-side state.
   c. Persist the sentinel: `mail.pending_rotation = { new_msek: MSEK' }` — the sentinel carries the incoming MSEK alone, and the set still owed a re-wrap is DERIVED from the credential rows' generation markers (*The generation marker* below; ruled 2026-09-30), never stored. The credentials the user flagged as compromised are then revoked — both blob kinds deleted, then the soft-revoke marker on their rows — so they are outside the owed set by construction.
   d. Call `provision_mls_snapshot_blob` with snapshot sealed under MSEK'. (Idempotent atomic replace on nest; any in-flight bridge session decoded under old MSEK can no longer decrypt the new snapshot — sessions tear down on next fetch, which is the rotation's goal.)
   e. For each live credential row owed against MSEK' — `wrapped_under ≠ fingerprint(MSEK')`, the row re-read immediately before each provision so a row marked revoked or burned meanwhile is skipped:
      i. Re-wrap MSEK' under that credential's KDF (Argon2id or HKDF, per the credential's `kind` field; the credential bytes are the row's `secret`).
      ii. Call `provision_wrapped_mls_blob`.
      iii. Write the row's `wrapped_under = fingerprint(MSEK')` (a per-row put, stamped above the stored row).
   e′. *(Retired 2026-10-06, the day it was ruled — no body sealed to a generation is ever re-sealed; every generation is carried instead: `../architecture/owner-key-material.md` § Path B-sibling-2 → *Pre-rotation mail at rest*. Finalize follows (e) directly.)*
   f. When no live row is owed against MSEK' — the owed set as read from the plane at that moment, so a credential another device added mid-rotation is re-wrapped first if its row has arrived, and healed later by *The generation marker* if it has not: replace the state row's `msek` with MSEK' and clear `pending_rotation`. Before the swap, write the outgoing MSEK's generation row — its retirement instant keyed by the outgoing MSEK itself, folded into `prior_mseks` / `prior_msek_retirements` (every generation ever retired, uncapped — `../architecture/owner-key-material.md` § Path B-sibling-2 → *Pre-rotation mail at rest*, ruled 2026-10-06; idempotent across finalize re-drives — an already-recorded instant is kept, not overwritten) — this is the only writer of those rows, and it is what lets the bounded-mail-mint healer (`../architecture/encryption-at-rest.md` § Capability tiering) bound which rotation generations still need coverage. Finalize **verifies the swap actually committed by reading the state row back** rather than trusting the write: a sibling's own concurrent swap (a second rotation finalizing on another device) under a later stamp wins the row's MSEK join (a deliberate-rotation latest-wins on the stamp) and reverts the swap when the walk merges it in (see § Cross-device finalize race below). So finalize keeps a sentinel carrying MSEK' set across the swap (so MSEK' stays recoverable from the custody until the swap is durable), re-drives — stamped strictly above what it read — if the read back shows a reverted swap, and reads back again after the sentinel clear (the clear restates no key material, so it cannot itself revert the swap, but a sibling's row can land between the two), re-driving rather than returning success on a reverted clear. It finishes only once `msek == MSEK'` is confirmed stored *after* the sentinel is cleared; on a persistent racer it surfaces a loud error rather than declaring a reverted rotation complete.
3. Crash anywhere between (c) and (f):
   a. On next client start, the mail-settings page detects `mail.pending_rotation` is set, surfaces a banner ("A previous mail-credential rotation didn't finish — resume?") with a "Resume" button.
   b. Resume re-enters step (e) over the derived owed set — the live rows whose `wrapped_under` is not the sentinel's MSEK' (*The generation marker*). Idempotent — `provision_mls_snapshot_blob` and `provision_wrapped_mls_blob` both atomic-replace at nest.

**Cross-device finalize race (publish-order safety).** The recipient pubkey derived from MSEK' is published to nest at step (d), and MSEK' is wrapped under every surviving credential at step (e), **before** the `msek` swap commits at step (f) — so senders begin sealing incoming mail to MSEK''s HPKE key before the custody's `msek` is MSEK'. MSEK' is irrecoverable for the Fauna app once it leaves the custody: the on-nest wrapped blobs are unwrapped **only** by the mail bridge (session-local MLS capability for IMAP/MDA), never re-fetched by the Fauna app to repopulate `msek`. The state row's MSEK join does not union the displaced key into the priors (a generation row is written only by a finalize, for the key it retires), so if a finalize that was silently reverted by a sibling's row declared success — clearing the sentinel with `msek` back to the pre-rotation key — MSEK' would be absent from the custody (not in `msek`, `prior_mseks`, or the cleared sentinel) and the next rotation's grace window would omit it, eventually making mail sealed to the already-published new pubkey undecryptable. The finalize read-back-and-re-drive in step (f) closes this: it reads back after **both** writes finalize makes — the `msek` swap *and* the sentinel clear — so neither a reverted swap nor a reverted clear can return a silent success with MSEK' gone. **Every other state-row write restates no key material** (the flags, `mail_enabled`, the sentinel and the burns are written with `msek` absent and the window empty, which the join reads as "no change" — `MailStateRow::recreatable_of`), so the only writes that can revert a swap are another device's own swap or fresh mint: two concurrent rotations, the documented cross-device residual (pinned at the plane in `fauna-sync-engine/tests/peer_leg_convergence.rs::a_reverted_mail_key_swap_is_caught_on_read_back_and_re_driven`, and at the machine in `libs/fauna-client-mail-settings/tests/rotation_resume.rs`'s two `finalize_re_drives_…` cases).

**The generation marker (ruled 2026-09-30) — every live credential row names the MSEK generation its nest-side blobs are wrapped under, and the owed set is derived, never stored.** `MailCredential.wrapped_under` is the **MSEK fingerprint** of that generation: `blake3::derive_key("fauna.mail.msek-fingerprint.v1", msek)`, 32 bytes, a CBOR byte string — a public identity of a generation that reveals nothing about it (the MSEK has 256 bits of entropy), so a row may carry it beside the secret without adding a second copy of any key. **The invariant:** a live row (neither burned nor revoked) whose `wrapped_under` is not the fingerprint of the current `msek` is **owed a re-wrap** — `None` reads as unknown, which is owed — and any device holding the plane can pay it, because the row carries the credential's secret and the state row the MSEK: re-wrap under the current `msek`, provision, write the marker. The rotation's step (e) is one instance of this heal (owed against MSEK' while the sentinel stands), the crash resume is another (there is nothing to resume from but the rows), and the third is the cross-device case the blob could only lose: a credential added on device B while device A rotates is wrapped under the freshest generation B holds — the sentinel's `new_msek` when B has synced one, else B's `msek` — and if that generation loses (the old MSEK once A's finalize commits; the losing side of two concurrent rotations, whose key the MSEK join displaces without keeping it in the window), B's row reads owed on every replica the moment the rows meet, and the next device to read it re-wraps it. Until healed, that MUA cannot open the snapshot — the shipped outcome, now bounded by the next sync instead of silent and permanent; the mail-settings machine may report owed rows in its snapshot (any new page element is `ui.yaml`'s). **The marker's dual:** a marked row (burned or revoked) still naming a generation is **owed a delete** — its blobs may exist on the nest, because a re-wrap can race the marker — so a revoke writes the marker and `wrapped_under = None` together, and any device reading a marked row with a generation re-issues the two idempotent revoke calls and writes `None`; a burned row is never re-wrapped (it has no secret) and never owed a re-wrap. Both halves converge under the row's merge (`config-dissolution.md` § Phases and gates → *Bounded rows* → *The mail plane*): the markers are monotone, the marker field is latest-wins on the row's own stamp, and a re-wrap loop that re-reads before each provision skips marked rows, so the only re-provision that can land after a revoke is one already in flight. The per-`credential_id` blobs on the nest are unchanged by any of this — the marker is client-side state about them.

> **Implementation status today (2026-09-30) — BUILT, plane-only (the `fauna.state.mail` consumer cut).** The fields (`updated_at`, `wrapped_under: Option<MsekFingerprint>`, `revoked_at_unix`), the fingerprint (`fauna_core::data::MsekFingerprint::of`) and the plane rows and their door are phase A's (`config-dissolution.md` § Implementation status today, the `fauna.state.mail` entry). The cut moved every consumer onto them: the mail-settings machine reads and writes through the `fauna_client_config::MailStore` seam (production `AccountMailStore` over the seat's account runtime); rotation and resume run over the derived owed set (`MailRows::owed_rewrap`), `PendingRotation.credentials_remaining` is deleted and the snapshot's remaining count derived; a soft revoke writes the marker (nest deletes first, then the marker with the generation cleared); `derive_credential_id` dedups against `MailRows::spent_credential_ids` (revoked ids included); the heal and its dual run best-effort at every mail-settings hydrate (`MailRows::heal_owed` / `delete_owed`); every flag write is a recreatable-half write (`MailStateRow::recreatable_of`), so it cannot revert a concurrent swap. The blob's `MailConfig::merge` keeps the shipped whole-record pick of the recreatable five until the rail retires, read by nothing.

**DAV bodies across a rotation (ruled 2026-10-06; refutable downstream).** Every CalDAV/CardDAV blob at rest — calendar and address-book collection metadata (`encrypted_metadata`), event and card bodies (`encrypted_body`), the Fauna sidecars (`encrypted_fauna_ext`) and the index hints the apps seal — is a `MailRecordEnvelope` sealed to the recipient-mail keypair of ONE MSEK generation (`fauna_mls::wrapped_blob::dav_body`; the Go MDA's `put.go` twins; the keypair is `../architecture/owner-key-material.md` § Path B-sibling-2's). The algorithm above re-wraps credentials and the snapshot and said nothing about these bodies: the apps opened them with the current generation alone, so a rotation hid the user's own calendar and contacts on every app at once, and a third rotation — the cap-2 window closing — would have lost them for good (witnessed 2026-10-06 on linux: every calendar entry of a just-rotated user failing `unseal DAV body: HPKE open failed`). Two facts shape the fix: the envelope carries no key id, and the segment store the bodies rest in is append-only and content-addressed (`../architecture/message-segment-store.md` § Record identity → pre-check 1), so a body is never rewritten in place. Four rulings as first written; **rulings 2 and 3 were RETIRED the same day by the carriage ruling** (`../architecture/owner-key-material.md` § Path B-sibling-2 → *Pre-rotation mail at rest*: every generation is kept, the window never closes, and nothing sealed to a generation is ever re-sealed — mail records and DAV bodies alike), so ruling 1 is the whole mechanism and ruling 4 narrows to it:

1. **Reads walk the standing ring, on every leg; writes seal to the current generation alone.** The DAV opener is the standing recipient key set — `derive_standing_mail_keypairs` over `[msek] ++ prior_mseks`, trialled newest-first by `open_mail_record_standing` — the exact set the MDA already reads out of the snapshot's `leaf_init_keypairs` (`MailRecordOpener::open`, which every Go DAV open site already goes through — `OpenStoredRecord`, the metadata and card openers — so the MDA leg needs nothing) and the apps' receive path already holds (`MailKeys.standing`, `mail-app-surface.md` § Inbound client receive). `DavRecipientKeys` becomes that ring, derived from the custody's `msek` and `prior_mseks` together, never `msek` alone; a miss names itself a ring exhaustion, as the index ring's does (`MailcalKeyRing`). Sealing stays current-only (`seal_dav_body*` take one MSEK), for the index ring's reason: a grace key may open, never extend a superseded generation forward.
2. **RETIRED 2026-10-06 — the rotating client converges the corpus — step (e′), between (e) and (f).** *Kept as the record of what was weighed; nothing in it is built or to be built.* Per DAV collection of the actor (calendars and their events; address books and their cards; both collections' metadata), fetch every sealed blob and open it with the ring: one that opens under MSEK′ is converged; one that opens only under a grace generation is re-sealed under MSEK′ (body and sidecar with the X-Wing suite the app writer uses; the index hint re-sealed when a grace key opens it, carried verbatim otherwise — an MDA-written hint is sealed to the nest-side index pubkey, which has no rotation walk) and written back through the ordinary replacement write — `put_event_ciphertext` / `put_card_ciphertext` on the same `uid_hash`, `provision_calendar(update_metadata=true)` and its address-book twin for metadata — accepting the etag/modseq bump (every MUA re-fetches the collection once per rotation; rotation is the rare response to a suspected compromise). The owed set is DERIVED, never stored: the AEAD is the only honest answer to "which generation sealed this" — two writers (app and MDA), no key id in the envelope, and a stored label can lie or lag; at calendar and contact corpus sizes the fetch is cheap. A blob no key in the ring opens is counted and skipped, never a rotation failure — it is already lost or foreign, and blocking the response to a compromise on it would hold the compromise open. **Finalize (f) requires (e′) complete in the same drive.** The compromise is already ended at (d) and (e) — the snapshot re-sealed, the survivors re-wrapped, the excluded revoked — so waiting costs security nothing, and it gives the invariant the window needs by construction: at the swap, the only bodies not under MSEK′ are concurrent-writer stragglers (a sibling device still sealing under the outgoing key until the swap syncs to it; an MDA session that AUTH'd before (d)), which the cap-2 ring covers with two rotations' margin and ruling 3 closes. (e′) is idempotent and crash-resumable from the same sentinel as (e) — a resume re-enters (e), then (e′) — and the succession burn's degenerate rotation (§ Succession) runs it too, so a successor's bodies leave the thief's generation.
3. **RETIRED 2026-10-06 with ruling 2 — the heal — the same pass, best-effort, at every mail-settings hydrate**, beside the generation marker's `heal_owed` / `delete_owed`, by whichever device holds the plane: it converges ruling 2's stragglers and finishes a pass that lost its nest mid-loop. Never write-on-read in the calendar or contacts readers — a read surface must not churn etags under a MUA's sync, and a reader that opened under a grace key has done its job by opening.
4. **Where it lives.** The ring is shared Rust (`DavRecipientKeys`, `fauna_mls::wrapped_blob::dav_body`); the apps change only by handing their opener `prior_mseks` beside the `msek` they hand it today. Priorities #2–#4: the ring is the richest existing pattern (`MailcalKeyRing`, `MailRecordOpener`). (The pass's home — the DAV crates, driven from `fauna-client-mail-settings` — went with the pass.)

**Rejected:** a stored per-row generation fingerprint (a label beside the seal, with a Go writer to keep in step — the AEAD already answers); a nest-side re-seal (the nest holds no key — unconditional). *Withdrawn 2026-10-06:* the rejection of an unbounded ring this block first carried — keeping a rotated-away key in the owner's custody adds no reach to whoever captured it, and the complete ring is now the ruling (Path B-sibling-2 → *Pre-rotation mail at rest*). **Disable mail is not a rotation** (§ Trigger taxonomy, the *Disable mail* row): the MSEK stays while a DAV protocol needs it, so no body is stranded by a disable/re-enable cycle. **Mail records are ruled with it:** the same carriage, the same no-re-seal — Path B-sibling-2 → *Pre-rotation mail at rest* (2026-10-06).

> **Implementation status today (2026-10-06) — the read half BUILT on every leg.** `DavRecipientKeys` is the ring (`libs/fauna-mls/src/wrapped_blob/dav_body.rs`): `from_mseks(msek, prior_mseks)` builds it through `derive_standing_mail_keypairs` and `unseal` trials it through `open_mail_record_standing`, a miss rendering `no key in the ring opens it (N tried: current + K grace)`; `derive(msek)` is the ring of one; `seal_dav_body*` still take the one current MSEK. The custody chokepoint `fauna_client_config::dav_store_context` returns `DavStoreContext { actor_id, msek, prior_mseks }`, so every reader that goes through it opens through the ring: tui, linux, `fauna-wasm` (web) and `fauna-ffi` — which reads the custody itself, no MSEK crossing the FFI, so android, apple and windows are on the ring through it with no app glue. The shared readers that took a bare `msek` take `prior_mseks` beside it: `CalDavClient`'s inbound scheduling entry points (`apply_inbound_request`/`_cancel`/`_reply_from_mail`/`_scheduling_from_message` → `find_stored_event`), `CardDavClient::locate_card_by_uid_hash`, and both `fauna-client-conversations` contacts arms (from `MailKeys.prior_mseks`). The ring's width is `derive_standing_mail_keypairs`'s — every generation ever retired since the carriage lift (2026-10-07) — so it widened with it, nothing here changed. `drive_rotation_loop` goes from (e) straight to finalize, the ruled shape; the MDA leg was already true (`MailRecordOpener::open`). Witnesses: `dav_body.rs`'s ring unit tests, `fauna-client-caldav`'s `an_inbound_cancel_finds_an_event_sealed_before_a_rotation`, and tier_3 `test_mail_credentials.py::test_calendar_written_before_a_key_rotation_still_opens` (a calendar and an event made before "Rotate mail keys" still render after it and a cold relaunch). The convergence pass's row closed dropped with the retirement.

**Compromised-credential handling.** A credential the user flagged as compromised is NOT in the owed set after step (c); it loses access automatically when the rotation completes (its wrapped-MSEK blob now points to old MSEK which can't decrypt the new snapshot). The rotation also revokes the excluded row at the same time (`libs/fauna-client-mail-settings/src/rotation.rs` — the soft-revoke marker, secret emptied, id spent), so it leaves the list at once; there is no separate "Remove from list" action. (Corrected 2026-09-23. The kept-on-purpose "Compromised — access revoked" row belongs to the succession burn only, § Succession, and the per-row revoke gesture revokes it.)

### Succession (every pre-succession credential is burned)

The identity-succession aftermath runs the hard-revoke flow with **every** credential excluded — ratified 2026-08-05; `succession-aftermath.md` § Re-key scope's MSEK row owns the succession-side ruling (why nothing survives, why it is uniform for theft and loss, why § Adjudicating's mark-don't-gate does not apply). This section owns the mechanics deltas from an ordinary hard revoke:

- **Exclusion is total and not user-selectable:** the pre-succession seed thief could read the account's mail custody (`fauna.state.mail`) — MSEK, `prior_mseks`, and every credential's raw `secret` — so every pre-succession credential row goes to *"Compromised — access revoked"* with no checkbox list. `start_rotation`'s `excluded_credentials` machinery is the implementation seam; the survivors list is empty, so step (e) is a no-op and finalize runs immediately.
- **No longer a succession-specific delta as of 2026-08-11: `start_rotation` itself now deletes the excluded credentials' resting blobs, BOTH kinds** (`revoke_wrapped_mls_blob` + `revoke_wrapped_submission_token` per credential, right after the sentinel persists and before the re-wrap loop — `libs/fauna-client-mail-settings/src/rotation.rs`), so every ordinary hard revoke behaves exactly like this succession leg for its excluded set. This closes the pre-existing ordinary-flow gap, so *"Compromised — access revoked"* now means revoked for **submit** as well as for read: the `WrappedMlsBlob` delete alone was already tolerable to skip (the snapshot re-seal kills reads regardless), but the `WrappedSubmissionTokenBlob` delete was the actual security-relevant miss. Fixed at the source, mirroring this succession leg's ordering (local state durable first, nest deletes second) and its exact two-call shape. Pinned by `rotation_excluded_credentials_dropped_from_local_state` (`libs/fauna-client-mail-settings/tests/rotation_resume.rs`).
- **The thief's fetch path is already cut before this leg runs, by the ceremony itself:** bridge AUTH resolves the address to an actor at AUTH time (`bins/fauna-bridges/internal/mda/davauth/auth.go` — `validate_recipient(local, domain) → actor_id`, "the address resolves the actor; the suffix selects the wrapped-MSEK blob") and the succession transaction moves the address to the successor — so `<handle>+<credential_id>` resolves to the successor, which holds no blobs, and the old actor's orphaned blobs are reachable by no address. ⚠ **The mechanism is the *alias* re-point, not the handle move — this bullet named the wrong one, and the code implemented neither, until 2026-08-11.** On a nest that has claimed a mail domain, `validate_recipient` resolves through the `account_aliases` exact row and only that row; the handle→actor fallback beside it is gated on the domain being *unregistered*, so it never fires where mail is actually served. **Invariant since 2026-08-11:** the succession transaction re-points `account_aliases.actor_id` ([`succession-aftermath.md`](succession-aftermath.md) § Re-key scope → the ownership blockquote, where that row was also missing), so `<handle>@<domain>` resolves to the successor from the ceremony onward. That re-point is the load-bearing half, because the revoke RPCs refuse any target that is not the caller — a successor cannot clean up behind a predecessor. The e2e that pins it is `tests/e2e-unified/tests/test_identity_succession_mail_auth.py`, which asserts the AUTH refusal against a real MDA bridge rather than a client-side proxy for it. Residuals, both bounded and unreachable by any nest-side action: the bridge's per-`(actor, credential)` auth-material cache, and already-AUTH'd sessions holding the unwrapped MSEK in memory (the same stated limit as soft revoke).
- **Between the ceremony and this leg, inbound delivery is degraded but not leaking:** the handle resolves to the successor actor, which has no registered recipient pubkey, so nothing seals new mail to the thief-known MSEK-derived key. The leg ends the window at first successor sign-in by re-registering `derive(MSEK′).pubkey` under the successor. **The MTA's no-pubkey arm was verified when the leg was built (2026-08-11), and it answered the two halves differently:** there was **no fallback to any old-actor row** on any path — both the inbound MX arm and the local-submission arm resolve the actor from the address (which the succession transaction re-points) and then read *that* actor's pubkey, so the confidentiality claim holds — but the arm **rejected permanently (550 5.1.1) rather than deferring**, in `bins/fauna-bridges/internal/mta/server.go`'s inbound reject and `submission.go`'s local-recipient resolve. Its comment stated the premise — *"permanent (recipient never onboarded)"* — which succession falsifies: this recipient exists and will hold a key within one sign-in. **Fixed 2026-08-11:** `fauna.bridges.fetch_recipient_mls_pubkey`'s reply carries an additive `succession_pending` field (`CacheDb::is_succession_new_actor` — true iff `new_actor_id` in `actor_successions` names this actor, never a string match on the bridge's own error text), and both the inbound MX arm and the submission RCPT-time arm now tempfail `451 4.7.1` on that signal instead of bouncing — the tempfail-vs-reject split itself is owned by `smtp-server.md` § Error / tempfail strategy.
- **MSEK_old goes onto `prior_mseks` as usual.** Grace-decrypt serves the *owner's* history — the successor legitimately holds the predecessor's material — and grants the thief nothing: every unwrap path they could reach is deleted or re-sealed under MSEK′.

> **Implementation status today (2026-08-11) — BUILT, shared Rust + driven on tui.**
> `fauna_client_mail_settings::succession::burn_mail_after_succession` is the leg:
> it burns for the attested predecessor set (since `fauna.state.mail`'s cut,
> 2026-09-30, it reads the mail custody's rows and takes no `__config`
> outcome), marks every
> live credential row burned and empties its secret, deletes both blob kinds per
> row, and then drives the ordinary `start_rotation` with **no** explicit
> exclusions — the survivor set is the *live* rows, so a burned plane makes the
> list empty by construction and step (e) is the no-op this section describes.
> It runs as leg 6, the last leg of the post-store-ready aftermath pass
> (`fauna_client_recovery::ledger_aftermath`; where it runs and why:
> [`succession-aftermath.md`](succession-aftermath.md) § Re-key scope →
> *Where the plane raises run*), and tui renders
> `recovery-kit-mail-burn-status` + the per-row `mail-settings-credential-item-revoked`
> state (both IDs **user-approved 2026-08-16**, asked 2026-08-11).
>
> **windows paints the per-row revoked state as of 2026-08-25 — and what found it is worth keeping.** The leg itself was
> never the defect: an instrumented `--app windows` run logged
> `aftermath MailBurn: mail_burn_done` and an overall `Rekeyed` while the page
> read *0 of 1 rows are marked*, because `mail-settings-credential-item-revoked`
> was the one `mail-settings-credential-item*` leaf windows had never rendered.
> A burned row keeps rendering — it is the user's list of which mail apps to set
> up again — so without the marker a dead credential is **visually identical to a
> live one**, and the MUA username beside it reads as a login that still works.
> No Rust was owed: `MailCredentialSummary.revoked` already crosses UniFFI. Both
> mechanism assertions of `test_the_successors_mail_passwords_are_burned` now pass
> on windows — every row revoked, and the reveal returning `""`, which is the
> material proof the secret is gone rather than merely labelled. **Still owed
> there and on the other apps:** `recovery-kit-mail-burn-status` itself (the
> per-app render trickle-down), on which that test
> still ends red.
>
> **The client leg is only half of it, and the nest-side half landed later the
> same day.** The burn deletes blobs under the *caller's* actor, so on its own it
> never reached the predecessor's — those rest under an identity the successor
> cannot name (`revoke_wrapped_mls_blob` refuses a target that is not the
> caller). What closes the exposure is `record_succession` re-pointing
> `account_aliases.actor_id`, which moves the address away from the retired actor
> so nothing routes to those orphaned blobs at all. Until then the leg could
> mark every row
> revoked, empty every secret, and leave the predecessor's password
> authenticating — which is exactly what
> `tests/e2e-unified/tests/test_identity_succession_mail_auth.py` caught, and why
> a client-side render assertion is not a substitute for one at the bridge.
>
> **Two pieces of at-rest state carry it, both additive:**
> `MailConfig::succession_burns` (one union-merged entry per burned predecessor
> — the leg's idempotency device, since nothing else in `MailConfig` can tell a
> predecessor-era row from one the successor added afterwards) and
> `MailCredential::burned` (the *Compromised — access revoked* row state). The
> burn record is merged **outside** the mail block's whole-record latest-wins,
> because a reverted record would re-arm the leg against the successor's own
> fresh credentials.
>
> **Two declared residuals.** (1) The leg cannot prove a row is post-succession,
> so it burns every live row it finds; the exposed window is one sign-in wide (a
> credential added on another device between the ceremony and this pass) and the
> direction is deliberate — over-burning costs a re-add, under-burning costs the
> mailbox. (2) The per-row `burned` marks themselves ride the recreatable mail
> block's latest-wins, so a stale-base peer can transiently un-mark rows the
> burn already killed; the access is still gone (the blobs are deleted and the
> MSEK rotated), so what reverts is the *label*, and the next write from any
> up-to-date device restores it.
>
> **End-to-end evidence (2026-08-11, the same day the leg landed).** The leg
> shipped with crate-level mutation grading and tui render pins but **no**
> full-stack run; it now has one — a tier_3 journey on tui
> (`test_identity_succession_aftermath.py::test_the_successors_mail_passwords_
> are_burned`) that enables a mailbox, reveals the credential secret, runs the
> stolen-identity ceremony, and then asserts every row revoked, the reveal
> emptied, and the progress line on its *done* arm. What only that run
> establishes is the **composition** the fakes cannot reach: leg 6 in its pass
> (post-auth then; the post-store-ready pass since 2026-09-30), two revoke RPCs
> per credential against a real nest, and a rotation whose survivor list is
> empty by construction. Mutation-graded 1:1 at landing — forcing the leg onto
> a no-burn arm reds it at the revoked-row count (the arm used then, the
> `__config`-owed one, was retired 2026-10-01: the leg reads no `__config`).
> ⚠ **A non-empty progress line is NOT the pass condition** on this leg, and a
> test that treats it as one is vacuous: the running and failed arms render
> too, and neither means anything burned.
>
> **Revoked-credential row — all seven apps, complete 2026-08-25.** macOS/iOS's
> shared FaunaKit `MailSettingsView.credentialRow` renders `MailCredentialSummary::revoked`
> (the `mail-settings-credential-item-revoked` row, directly under the name, tui's
> placement) since 2026-08-19; linux (`settings/mail.rs::build_credential_row`), web
> (`MailSettingsSection.svelte`'s `CredentialRow`), and android
> (`MailSettingsScreen.kt`'s `CredentialRow` composable) landed the same paint
> 2026-08-20 — a pure render over the already-shared `MailCredentialSummary.revoked`
> field, zero new logic. windows landed last, 2026-08-25 (`MailSettingsPanel.xaml`'s
> `RevokedText`/`RevokedVisibility` bound fields, § above).
>
> **`recovery-kit-mail-burn-status` progress line — four of seven apps.** tui,
> linux (`recovery_kit.rs::mail_burn_status_text`, driven by
> `state.aftermath.mail_burn`), web (`+page.svelte`'s
> `recoveryAftermath.mailBurn` block), and macOS/iOS (the shared FaunaKit
> `RecoveryKitSection.aftermathProgressLines`, landed 2026-08-26 alongside the
> other six aftermath-progress lines) render it — all driven by the shared
> `MailBurnProgress::status_line`. **android and windows still render none of
> the Recovery Kit aftermath-progress lines at all** — this line's absence
> there is a symptom of that wider unbuilt family (seven sibling status lines, same
> gap), not a mail-specific miss.

### Lost wrapped blob

A wrapped-MSEK blob lost on nest (e.g., a restore-from-backup reverted it, or a nest-side bug) is harmless beyond that credential. User adds a fresh credential; the new wrapped-MSEK contains the same MSEK; the lost credential is dead. No MSEK rotation needed; no snapshot re-upload needed.

### Partial-state-during-minting (first-enable)

The first-enable flow provisions three blobs plus one routing-metadata registration. Order:

1. `provision_mls_snapshot_blob` (idempotent on actor_id). The snapshot carries the recipient-mail keypair(s) derived from MSEK (`docs/goal/architecture/owner-key-material.md` § Path B-sibling-2).
2. `provision_recipient_mls_pubkey` with `derive(msek).pubkey` and the MSEK-derived ML-KEM ek beside it — both halves are required ([`../architecture/security/post-quantum.md`](../architecture/security/post-quantum.md) § Capability negotiation and default-selection policy) (idempotent, atomic-replace on actor_id). User-self-registered (`target == caller`); registered *after* the snapshot so the matching secret is reachable before the MTA can seal mail to the pubkey. The MTA reads this via `fetch_recipient_mls_pubkey` on every inbound message.
3. `provision_wrapped_mls_blob` (per credential_id).
4. `provision_wrapped_submission_token` (per credential_id).

Crash between 1 and 3: snapshot (and possibly the registered pubkey) exists with no wrapped-MSEK to reach it — harmless; next enable-mail attempt overwrites all idempotently. Crash between 3 and 4: IMAP works but SMTP submission doesn't — surfaced as a partial-enable banner; clicking "Enable mail" again completes step 4 idempotently. The state machine treats first-enable as a degenerate rotation (with one credential, no surviving credentials list) and reuses the resume sentinel for crash recovery. The rotate-mail-keys (hard-revoke) flow re-derives + re-registers the recipient pubkey from the new MSEK and carries the prior keypair(s) for grace-decrypt (`MailConfig.prior_mseks`, cap 2).

Because every step is idempotent (atomic-replace / `INSERT OR REPLACE` / `DELETE` on the nest), a **transient transport failure** — a WS disconnect mid-call against a flaky or remote nest, distinct from a process crash — is retried automatically *within* the single enable gesture rather than aborting the whole sequence and reverting the UI to "Mail is disabled". The shared seam (`libs/fauna-client-mail-settings/src/rpc_glue.rs::request_idempotent`) re-issues each provision/revoke RPC up to 4 attempts with exponential backoff on a transient error (the shared `nest_error` classifier's `Transient` class), surfaced uniformly on every app; a server *rejection* (`Rejected`) is not retried. This closes the gap where enable-mail worked on a loopback nest but failed against a real remote one (no mid-flight drops on loopback). *Witnessed end to end on tui (2026-10-06)* by `test_mail_settings_controls.py::test_a_brief_drop_while_turning_mail_on_is_retried_and_finishes`: the nest's test hook (`drop-reply-once`, `bins/fauna-nest/src/rpc_hold_test_hook.rs`) runs the first `provision_wrapped_mls_blob` for real, closes the connection in place of its reply exactly once, and the page still ends with mail on, the password listed and no error. The seam is shared Rust, so the other six apps run the same retry; their legs of the journey are not yet run.

---

## Mode-uniform behavior (formerly "Plaintext-mode behavior")

**There is one mail-settings flow, full stop — the storage-mode axis this section used to fork on is retired** (`nest/storage-modes.md`). Historically (per the candidate (b) ratification in `docs/goal/behavior/imap-server.md` § Authentication, 2026-05-14) the client already provisioned the same `WrappedMsekBlob` / `MlsSnapshotBlob` / `WrappedSubmissionTokenBlob` shape regardless of the deployment's storage mode, and the mail-settings UI never surfaced it. Post-retirement every nest is sealed at rest by default (`encryption-at-rest.md` § The sealed posture) — server-side classifiers / FTS no longer read plaintext at rest in any deployment (`encryption-at-rest.md` § The read-position purpose test); the MDA bridge AUTH code was always a separate code surface from that read path and remains so, and the I3 client-side flow still never interacts with it.

This decision is load-bearing per priority #3 (`principles.md` § Engineering principles — same concepts and architecture everywhere): the app codebase carries exactly **one** mail-settings flow, not a per-deployment fork.

No `if storage_mode == "plaintext"` branches anywhere in I3 code, now or historically. A future feature that legitimately needs to branch on something must justify the divergence per priority #1 (`principles.md` § Engineering principles) and document the carve-out here — but it would be branching on a genuinely new axis, not the retired storage-mode one.

---

## Architectural rules

1. **One mail-settings flow, no per-deployment fork.** No UI branch on `storage_mode` (the axis is retired — `nest/storage-modes.md`; don't reintroduce one). Per candidate (b) ratification.
2. **MSEK persists in the mail custody (`fauna.state.mail`), fleet-only.** Every Fauna app in the user's fleet is "primary" for I3 purposes; the "primary client" framing in the wrapped-blob crypto spec is UX only.
3. **Credential secrets persist in the mail custody.** Same audience as MSEK; same seal. Enables unattended rotation. Alternative containers require a goal-doc update.
4. **Soft revoke is the default for retire-a-credential.** Hard revoke is reserved for suspected compromise and runs the resumable rotation flow.
5. **Rotation is resumable from any crash.** Sentinel in the mail custody's `pending_rotation`; on app start, the mail-settings page surfaces a banner offering to resume. Provisioning RPCs are idempotent atomic replaces at the nest layer.
6. **OAUTHBEARER preferred for new credentials.** PLAIN is supported for legacy MUAs. Both flow through the same AEAD-unwrap-as-AUTH at the bridge; KDFs differ (Argon2id vs HKDF).
7. **MUA-username carries credential_id via RFC 5233 sub-addressing** (`<handle>+<credential_id>@<domain>`). For credential_id `"default"`, the suffix is omitted. **The `@<domain>` may also be omitted** (a bare `<handle>` / `<handle>+<credential_id>`): the bridge resolves the missing domain to the box's `PrimaryDomain` via the shared `internal/auth.SplitEmailDefault`, applied uniformly across all three AUTH surfaces (CalDAV Basic, IMAP PLAIN/OAUTHBEARER, SMTP submission). This is required for macOS Calendar.app, which sends only the bare local part as the CalDAV username (`caldav-server.md` § Authentication — "Bare-username clients"); an unclaimed primary domain keeps the strict `user@domain` requirement.
8. **One MSEK per actor, stable across the actor's lifetime — and across every *box* the user owns, not just every app.** Rotated only by the rotate-mail-keys flow. Adding / revoking credentials does not rotate MSEK. When the user runs a multi-nest deployment (the home-with-public-relay home box), the second box's mailbox is provisioned with the **same** MSEK (the client re-seals the account's MSEK onto it — `bridge_*` blobs don't federate, so this is a client-side re-provision, never a fresh mint). A *different* MSEK on the second box would silently break decrypt on it (mail looks delivered but is unreadable). Implemented by `libs/fauna-client-mail-settings::provision_relay_mailbox` (the `ProvisionRelayMailbox` dispatch).
9. **MlsSnapshotBlob refreshes on every MLS state change.** Background task on the user's Fauna app fleet; any online client refreshes; idempotent atomic replace on nest.
10. **SubmissionToken refreshes on a 7-day cadence** (default; configurable per credential). Background task; any online client refreshes.

---

## Don't do these

- **Don't branch the mail-settings UI on storage mode.** The axis is retired (`nest/storage-modes.md`) — uniform per § Mode-uniform behavior.
- **Don't prompt the user for every credential's bytes during rotation.** Read from the credential rows and re-wrap unattended.
- **Don't rotate MSEK on credential add or credential revoke.** MSEK is stable; per-credential wrapped-MSEK blobs are independent.
- **Don't add a credential without persisting its bytes to the mail custody.** The state machine needs them for future rotation; not having them downgrades hard-revoke UX to "re-prompt every MUA."
- **Don't ship a soft-revoke that re-snapshots or re-wraps surviving credentials.** Soft revoke is `revoke_wrapped_mls_blob` only — single RPC, no MSEK change.
- **Don't ship a hard-revoke that isn't resumable.** A crashed rotation leaves the user in a broken state without resume; sentinel is mandatory.
- **Don't show "you're on a plaintext-mode nest" or "you're on an encrypted-mode nest" anywhere in the mail-settings UI.** There is no such deployment posture any more (`nest/storage-modes.md`) — nothing to surface.
- **Don't conflate the SubmissionToken with the wrapped-MSEK blob.** The token authorizes SMTP submission only — no decryption capability; rotated on its own 7-day cadence; revoked separately via `revoke_wrapped_submission_token`.
- **Don't add a per-user "I want this credential to not have submission capability" toggle.** Every credential gets a SubmissionToken; SMTP submission and IMAP/CalDAV access are linked by `(actor, credential_id)` and unlinked only by revoke.
- **Don't store credentials_plaintext anywhere outside the account's mail custody (`fauna.state.mail`, sealed under the account's generation tip).** The OS keyring, the device's biometric store, and any per-app sandbox are all wrong containers — they break the cross-device fleet sync that the rotation flow depends on.

---

## OAUTHBEARER issuer (future)

**Retired into the one issuer (TP5, ratified 2026-09-05) — owner: [`authorization-server.md`](authorization-server.md) § Mail clients.** There is no second OAuth issuer for MUAs: the typed-code consent start is the RFC 8628 device grant, an OAUTHBEARER credential becomes an ordinary grant row on the connected-apps roster ([`../ui/connected-apps.md`](../ui/connected-apps.md)), and the token-display-once UX becomes "open Fauna → *Connect an app* → type the code". The one requirement this section carried travels with it and stays binding: the MDA validates a presented OAUTHBEARER token **out of process** — a JWKS-verified access token, or RFC 7662 introspection against the issuer — and never treats AEAD-success-and-blob-not-revoked as AUTH-success. The wrapped-MSEK blob shape stays unchanged; only the OAUTHBEARER credential lifecycle changes. Argon2id-based PLAIN remains unchanged.

---

## Reading list

1. `principles.md` § Product invariants (nest config from apps, works out-of-the-box).
2. `docs/goal/behavior/imap-server.md` § Authentication, § Content path — the AUTH path this doc provisions for.
3. `docs/goal/ui/mail-settings.md` — the per-page UX this doc's flows render through.
4. `docs/goal/architecture/owner-key-material.md` § Path B — MSEK key derivation + audience.
5. `docs/goal/architecture/encryption-at-rest.md` — the at-rest property this doc serves.
6. `docs/goal/behavior/mail-bridge-lifecycle.md` § DKIM provisioning (automatic), § TLS provisioning (three paths) — admin-side surfaces this doc cross-references.
7. (design ratified 2026-05-08; tracked internally) — KDF/AEAD/HPKE shapes.
8. (design ratified 2026-05-07; tracked internally) — the bridge architecture I3 enables.
9. (implementation plan dated 2026-05-14; tracked internally).
10. [`../architecture/config-dissolution.md`](../architecture/config-dissolution.md) § The `__config` dissolution schedule → *The kinds* — the `fauna.state.mail` kind, the persistence channel for MSEK + credentials since the `__config` rail retired (2026-10-02, *The closure order*, step (6)).
