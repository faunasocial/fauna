# Mail-settings page — target state

Owns: mail-settings-page
Status: ratified
Authority: ui.yaml (`mail-settings`, `mail-add-credential`, `mail-rotate-keys-confirm` pages) owns IDs + per-page element scope; this doc owns the page behavior, layout, snapshot/action shape, per-flow UX (enable / add / rotate / revoke / disable, CalDAV-only reachability, serve-here), and error surfaces; cross-page credential behavior (trigger taxonomy, MSEK lifecycle, KDF choice, rotation algorithm, cross-deployment uniformity) → [`../behavior/mail-credentials.md`](../behavior/mail-credentials.md); CalDAV/CardDAV/WebDAV enablement semantics → [`../behavior/caldav-server.md`](../behavior/caldav-server.md) / [`../behavior/carddav-server.md`](../behavior/carddav-server.md) / [`../behavior/webdav-server.md`](../behavior/webdav-server.md); admin-side surfaces → [`../behavior/mail-bridge-lifecycle.md`](../behavior/mail-bridge-lifecycle.md) § Pending approval + [`../behavior/dns-management.md`](../behavior/dns-management.md) (DKIM + TLS are provisioned automatically — no manual admin UI; the former `admin-mail-deliverability` page was dropped 2026-05-24).

## Goal

The mail-settings page is the user's single touchpoint for managing third-party-MUA access to their account: enable/disable mail, see and revoke individual credentials, rotate the underlying MSEK after a suspected compromise, and read the connection details a MUA needs (hostname, port, username format). One uniform shape on all 7 apps per priority #1 (`principles.md` § Engineering principles).

The page lives under account settings → "Mail & Calendar" sub-page on every app (desktop: the Settings shell; mobile: same shape via the platform's settings drill-down). It is **not** the user-facing Bridges page (which covers feed bridges — Bluesky, ActivityPub) and **not** the admin pane (which covers DKIM and bridge approval).

## Layout & flow

**Top of page (always visible).** Page heading ("Mail & Calendar") + enabled-toggle.

- When mail is disabled: only the toggle + heading + a short explanation sentence ("Enable to set up third-party mail and calendar apps like Apple Mail, Thunderbird, and Apple Calendar").
- When mail is enabled: toggle is on, plus the rest of the page below.

**CalDAV-only mailbox (email off, calendar on).** Email (`mail-settings-enabled-toggle`) and CalDAV gate independently (`caldav-server.md` § Independent enablement), but they share **one** MSEK + **one** `default` bridge credential (`caldav-server.md` § Authentication — "no separate CalDAV credential"; the same `(actor, credential_id)` AUTHs IMAP + SMTP + CalDAV). So the **credential-management section** — the credentials list, add-credential, rotate-keys, the keys explainer — and the **serve-here** toggle render whenever the credential-management reachability predicate holds (§ Credential-management reachability owns it), **not** only when email is on. This closes the independent-enablement gap where a CalDAV-only deployment (email off, e.g. a calendar-only or trust-the-box box) had its credential-management UI entirely hidden, leaving the user no way to obtain or rotate the bridge password their CalDAV MUA needs. The `mail-settings-mua-instructions` connection-detail block is **per-protocol**: the IMAP/SMTP host+port lines describe the *email* protocol → gated on `enabled`; the CalDAV host+port line (`mail-settings-mua-caldav-host` / `-port` = `mail.<domain>` / `443`, `caldav-server.md` § Network exposure) describes the *calendar* protocol → gated on `caldav_enabled`; the username-format + auth-mechanism lines are shared by both protocols (one credential AUTHs all three) → shown whenever the block is (mailbox provisioned). The `mail-settings-enabled-toggle` label/state stays email-specific (gated on `enabled`). The page is named **"Mail & Calendar"** (`page-heading` + the page title), and the connection-detail group is titled "Mail, calendar & files app setup" — reflecting that the one shared credential + provisioned mailbox serve every protocol (IMAP + SMTP + CalDAV + CardDAV + WebDAV). *(Landed on all 6 pre-flip apps 2026-06-23; tui built the same lift as part of its own page (M8 slices 1–5, 2026-07-09 → 2026-07-18) — § Implementation status today.)*

**Where the credential rows render.** On an app whose Connected apps page is built, the credential rows are rows of that page's roster (credential class "app password") and do **not** render here — a row is moved, never shown twice ([`connected-apps.md`](connected-apps.md) § Layout & flow owns the row's place and IDs there; which apps have moved is its § Implementation status today — tui as of 2026-10-01; android, linux, macos and ios as of 2026-10-02; windows as of 2026-10-03 — web is the one app still listing them here). This page then keeps *Add credential*, *Rotate mail keys* and the keys explainer, plus one line saying where the passwords are listed. What a row shows and does — the paragraphs below — is the same in both places and is specified here.

**Credentials list** (visible when `credential_management_reachable` — § Credential-management reachability; **not** `enabled` alone). Component `mail-settings-credentials-list`. One row per credential with display name, kind (Password / Bearer token), created-at, last-used-at, the **concrete MUA username** for that credential (`mail-settings-credential-item-username` — `<handle>+<credential_id>@<domain>`, or bare `<handle>@<domain>` for the `default` credential) with a **copy** affordance (`mail-settings-credential-item-copy-username`), a **reveal/copy of the secret** (`mail-settings-credential-item-reveal-secret` toggles showing `mail-settings-credential-item-secret`, hidden by default; `mail-settings-credential-item-copy-secret` copies it), and a revoke button. List is indexed. The secret (PLAIN password / OAUTHBEARER token) is **never** in the snapshot (the passive state carries no secrets); the row fetches it on demand from the account's own mail custody (`fauna.state.mail`) via `MailSettingsMachine::reveal_credential_secret`, so a user can recover the exact secret to (re)configure a MUA without revoke+re-add — the same value shown once at credential-add time. The username is built in shared Rust (`MailCredentialSummary.mua_username`, only `{handle}` left for the renderer to substitute via `resolve_mua_username`) so every app shows the identical, exact login.

A row the identity-succession burn killed renders `mail-settings-credential-item-revoked` — "Compromised — access revoked" (`MailCredentialSummary.revoked`) — instead of behaving as usable: the secret is gone from the mail custody (`fauna.state.mail`) and both on-nest blobs are deleted, so the username above it authenticates nothing. The row is kept, not dropped, so the user still sees which mail apps to set up again; the existing per-row revoke gesture removes it once they're done. Full mechanics + trigger (a stolen-identity succession, not an ordinary revoke): `mail-credentials.md` § Rotation and recovery → *Succession*. Built on all 7 apps (tui, macOS, iOS, linux, web, android; windows landed last, 2026-08-25). The paired `recovery-kit-mail-burn-status` progress line is further behind — only tui, linux, and web render it (`mail-credentials.md` § Implementation status has the full per-app breakdown for both). The `mail-settings-credential-item-revoked` id string's Rule-A sign-off was given 2026-08-16 (asked 2026-08-11).

**Add credential** (button when `credential_management_reachable`). Opens the `mail-add-credential` dialog/sheet. The PLAIN form defaults auto-generate ON (`mail-add-credential-autogenerate-toggle` → shared `generate_bridge_password`); toggling off enables manual entry and always reveals `mail-add-credential-weak-password-warning` (via `warn_manual_password`, with `nest_encrypted` read from `setup_status.mode` — a retired-storage-mode-axis shim that's now permanently `true`; `mail-credentials.md` § Mode-uniform behavior). The advisory strength meter is shared Rust — `password_strength_label(password) -> Option<LocalizedText>` (`libs/fauna-client-mail-settings/src/password_gen.rs`; canonical thresholds `< 8` Weak / `< 16` Fair / `≥ 16` Strong), rendered by all 7 apps. The shared helpers (`generate_bridge_password` / `generate_bridge_token` / `warn_manual_password`) surface to native apps via `libs/fauna-ffi/src/mail_admin.rs` and to web via `libs/fauna-wasm/src/mail_admin.rs`.

**Rotate mail keys** (button when `credential_management_reachable`, ≥ 1 credential exists). Opens the `mail-rotate-keys-confirm` modal.

**Keys explainer** (`mail-settings-keys-info`, always visible when `credential_management_reachable`). An (i) informational affordance beside the rotate-keys button — copy `S.mail_settings.keys_info`. Explains the rationale behind the mail encryption keys: what they protect, *when to rotate* (suspected compromise — a leaked password/token or a lost/stolen device that had mail set up) versus *when to just revoke* a single retired credential, and that already-received mail stays readable after a rotation. The product surfaces no automatic "rotate now" prompt today, so this explainer is how the user knows the difference between the two actions (see `docs/goal/behavior/mail-credentials.md` § Trigger taxonomy).

**MUA setup instructions** (visible when the credential-management reachability predicate holds — see § Credential-management reachability). Component `mail-settings-mua-instructions`. Read-only, **per-protocol**: IMAP host + port and SMTP host + port (the *email* protocol, shown when `enabled`); CalDAV host + port (the *calendar* protocol, shown when `caldav_enabled` — `mail.<domain>` / `443`); the WebDAV collection-root URL (the *files* protocol, shown when `serves_webdav_set` — see § WebDAV files); and the username-format string + auth-mechanism guidance (shared by every protocol — one credential AUTHs IMAP+SMTP+CalDAV+CardDAV+WebDAV — shown whenever the block is). Includes a "Copy" affordance per field. The connection-detail group is titled "Mail, calendar & files app setup".

**Credential-management reachability** (the predicate this page's shared surfaces render on). Computed **once in shared Rust** as `MailSettingsSnapshot::credential_management_reachable` = `enabled || caldav_enabled || carddav_enabled || serves_webdav_set` — every app reads the field rather than re-deriving the disjunction, so a future DAV sibling widens it in one place (priority #2/#4). It gates the credential-management section (keys explainer, credentials list, add / reveal / rotate), the serve-here toggle, and the MUA block; the per-protocol *connection-detail rows inside* the MUA block stay individually gated as listed above.

> **WebDAV files (approved 2026-07-06; built 2026-07-09 — slice 6b-2).** This page carries the WebDAV sibling of the CalDAV/CardDAV pattern ([`../behavior/webdav-server.md`](../behavior/webdav-server.md) § Independent enablement): the `webdav_enabled` deployment toggle is machine-derived at claim from the handle's locality (no onboarding checkbox — the former `onboarding-enable-webdav-checkbox` retired with `encryption_mode_choice`, not relocated; no-modes ratified 2026-07-12, `../behavior/onboarding.md` § 3b) and the admin shell (`admin-files-webdav-enabled-toggle`) is the change surface thereafter, and this page gains **one** new element, `mail-settings-mua-webdav-url` — the full collection-root URL the user mounts (`https://mail.<domain>/webdav/`; on a local-target box the bare locator + the DAV port, the same any-locator carve-out the CalDAV rows take). A **full URL, not a host+port pair**, because no SRV autodiscovery exists for WebDAV, so this line is the primary setup surface. Built in shared Rust (`MuaInstructions::webdav_url`) so all 7 apps copy an identical URL.
>
> **Both the URL row and WebDAV's term in the reachability predicate gate on the *per-actor* serve state** (`MailSettingsSnapshot::serves_webdav_set` — "this actor serves ≥1 folder over WebDAV", folded from the `User`-class `fauna.folders.list`), **not** on the deployment-wide `webdav_enabled` toggle. Two reasons, both load-bearing: (1) that toggle **defaults ON** for a real-domain box ("harmless-on — nothing is served until a set is flagged", `webdav-server.md` § Independent enablement pt 1), so gating on it would render credential management, and a dead mount URL, for *every* actor on *every* box — exactly what the CalDAV-only gating exists to prevent; (2) it is `Admin`-only to read (`fauna.bridges.get_mail_config`), and this is a `User`-class page. An actor who serves no set sees no WebDAV row; one who serves a set needs the bridge credential its WebDAV client AUTHs under, so the credential section opens. An actor with no MSEK cannot serve at all — the per-set `folder-webdav-toggle` carries that "set up mail first" hint (`../ui/folders.md`).

**Local IMAP/CalDAV-serving toggle.** A per-user toggle "Serve my mail & calendar over IMAP/CalDAV on this nest" (default **on**). Turning it **off** tells *this* nest's MDA not to serve the user's mailbox — used in the private-home-behind-public-relay deployment, where the user reads mail on their private LAN nest and turns serving **off** on the public relay nest (composition rationale → [`../architecture/nest/deployment-home-with-public-relay.md`](../architecture/nest/deployment-home-with-public-relay.md) § MUA reach). It is **user-controlled** (the user decides where they read their mail — product invariant), **admin-visible read-only**. Mechanically: a per-actor nest DB flag (`actor_mail_serving`, default on / absent ⇒ on) set via the User-class, caller-scoped RPC `fauna.bridges.set_mail_serving_enabled` (read via `get_mail_serving_enabled` — User reads own, Admin reads any for the read-only audit view), consulted **per request** by the nest-side `require_local_mail_serving` gate (IMAP) and the `BridgeMda`-only path of the CalDAV handlers — **not** carried in `fauna.bridges.fetch_config` (that reply is deployment-wide; the per-request nest gate alone enforces the per-actor opt-out). The user's own client reads (this page + Events) are `User`-class and never gated. Rendered as `mail-settings-serve-here-toggle` (flat mail-UX ID family, **no `bridges-detail-*` IDs**) backed by the shared `MailSettingsMachine`'s `SetServingEnabled` action + `serving_enabled` snapshot field. The admin's read-only audit view is `admin-users-mail-serving-status` per `user-row`, sourced from the `fauna.admin.users.list` projection's `AdminUser.mail_serving_enabled` field (one batch read, no per-row fan-out — [`../behavior/admin.md`](../behavior/admin.md) § Users). *(Both surfaces on all 7 apps — § Implementation status today.)*

**Status indicator.** Subtle line: "Mail is disabled" (when `!enabled`) / "All up to date" / "Syncing mail credentials…" / "Rotation in progress (3 of 5 credentials remaining)." Maps to `MailSettingsSnapshot.status` **gated on `enabled`**: the `Idle` status renders "All up to date" only when mail is on and "Mail is disabled" otherwise (a disabled mailbox must never read "up to date"; pre-hydrate it also reads "disabled"). The decision lives in shared Rust `fauna_client_mail_settings::settings_status_label(status, enabled) -> LocalizedText` — one source of truth all apps resolve through their own i18n runtime. The exact "All up to date" / "Mail is disabled" wording is the cross-app enabled/disabled signal the e2e reads (`tests/e2e-unified/actions/mail_settings.py` `STATUS_ENABLED` / `STATUS_DISABLED`, used by `wait_for_enabled_status`).

**Pending-rotation banner.** If `pending_rotation` is set on the mail custody's state row (`fauna.state.mail`; a previous rotation didn't finish), the page surfaces a banner at the top of the credentials list with text "A previous mail-credential rotation didn't finish. Resume?" and a "Resume" button. Banner clears when the rotation completes.

## Element IDs

ui.yaml owns the inventory — the `mail-settings`, `mail-add-credential`, and `mail-rotate-keys-confirm` page blocks plus the `mail-settings-credentials-list` (indexed) and `mail-settings-mua-instructions` components; read the lists there. This doc owns only the **visibility conditions**:

- `mail-settings-add-credential-button`, `mail-settings-rotate-keys-button` (≥1 credential), `mail-settings-keys-info`: when `credential_management_reachable` (§ Credential-management reachability) — **not** `enabled` alone; a CalDAV-only actor (email off) sees all three, since the shared bridge credential these buttons manage isn't email-specific. Code-verified 2026-07-18 on macOS (`FaunaMacApp`'s `MailSettingsView.manageSection`) and linux (`mail.rs::manage_group`), which both gate this section purely on `mailbox`/`credentialManagementReachable`, with no additional `enabled` check.
- `mail-settings-disable-confirm` + `-button`: present only while the destructive disable dialog is open (enabled-toggle off-path).
- `mail-settings-mua-*` rows: per-protocol gates as specified in § MUA setup instructions (IMAP/SMTP on `enabled`; caldav rows on `caldav_enabled`; `mail-settings-mua-webdav-url` on `serves_webdav_set`; username/auth whenever the block renders).
- `mail-add-credential-autogenerate-toggle` (PLAIN, default ON), `-password-input`/`-password-show-toggle`/`-password-strength-meter` (PLAIN manual), `-weak-password-warning` (PLAIN manual, unconditional — see § Add credential), `-token-display`/`-token-copy-button` (OAUTHBEARER, shown once).
- `mail-settings-pending-rotation-banner`, `mail-settings-pending-rotation-resume-button`: only when `pending_rotation` is set. `mail-rotate-keys-progress-indicator`: during multi-step rotation.
- `mail-settings-credential-item-revoked`: only on a credential row the identity-succession burn killed and kept (`MailCredentialSummary.revoked`) — see § Credentials list. Net-new id, user-approved 2026-08-16 (asked 2026-08-11); built on all 7 apps as of 2026-08-25. Where the rows render on the Connected apps roster (§ Where the credential rows render) this id is not painted: the roster row says the same thing in its own words and carries a `revoked` attribute ([`connected-apps.md`](connected-apps.md) § Implementation status today).

## State & data shape

The page renders `MailSettingsSnapshot` from `libs/fauna-client-mail-settings/src/state.rs`. Snapshot shape (mirrored from the Rust definition):

```
MailSettingsSnapshot {
  enabled: bool,                            // EMAIL on (MailConfig::is_mail_enabled — the mail_enabled flag, not msek.is_some())
  caldav_enabled: bool,                     // CalDAV on (MailConfig::caldav_enabled); independent of `enabled` — both ride the one shared MSEK + `default` credential
  carddav_enabled: bool,                    // CardDAV on (MailConfig::carddav_enabled); the contacts sibling, same shared MSEK + `default` credential
  serves_webdav_set: bool,                  // this actor serves >=1 folder over WebDAV (nest-sourced fold of fauna.folders.list); NOT the deployment-wide webdav_enabled toggle
  credential_management_reachable: bool,    // enabled || caldav_enabled || carddav_enabled || serves_webdav_set — computed once in shared Rust; apps read it, never re-derive
  serving_enabled: bool,                    // per-actor actor_mail_serving nest flag (default on); the serve-here toggle
  credentials: [MailCredentialSummary],     // see below
  pending_rotation: Option<PendingRotationStatus>,
  mua: MuaInstructions,                     // hostnames, ports, URLs, username format
  status: "Idle" | "Syncing" | { kind: "RotationInProgress", credentials_remaining: u64 },
  error: Option<String>,
}

MailCredentialSummary {                     // named `Mail…` because the FFI surface
                                            // already exposes a `CredentialSummary`
  credential_id: String,
  display_name: String,
  kind: "Plain" | "OAuthBearer",
  created_at: u64,                          // unix seconds; per-app renders local time
  last_used_at: Option<u64>,                // nest aggregates from report_auth_event audit log (planned; not in the struct yet)
  mua_username: String,                     // concrete `<handle>+<id>@<domain>` (bare for `default`), only `{handle}` left for the renderer to substitute via resolve_mua_username
  revoked: bool,                            // the identity-succession burn's "Compromised — access revoked" row state (§ Credentials list); secret + both on-nest blobs are gone, row kept so the user knows to re-add it
}

MuaInstructions {
  imap_host: String,
  imap_port: u16,                           // 993
  smtp_host: String,
  smtp_port: u16,                           // 465
  caldav_host: String,                      // "mail.<domain>" (caldav-server.md § Network exposure); rendered when caldav_enabled
  caldav_port: u16,                         // 443
  webdav_url: String,                       // the full "https://mail.<domain>/webdav/" collection root (§ WebDAV files); rendered when serves_webdav_set
  domain: String,                           // the deployment's mail domain (bare host of the node URL); the raw material for the above, not itself a row
  username_format: String,                  // the generic TEMPLATE, placeholders intact: "{handle}+{credential_id}@{domain}". Rendered raw — it teaches the address convention. The CONCRETE per-credential login is MailCredentialSummary.mua_username (resolve_mua_username), never this
  auth_mechanism: String,
}
```

The per-app UI subscribes to snapshot updates and re-renders on change. Actions dispatched back to the state machine:

```
MailSettingsAction =
  | EnableMail { display_name, kind, secret }
  | AddCredential { display_name, kind, secret }
  | RevokeCredential { credential_id }
  | DisableMail                              // CalDAV-aware teardown — see § User actions
  | StartRotation { excluded_credentials: [String] }
  | ResumeRotation
  | SetServingEnabled { enabled }            // the serve-here toggle — see § Local IMAP/CalDAV-serving toggle
  | ProvisionRelayMailbox                    // home-with-public-relay: reuse the fleet MSEK on a 2nd nest the user owns — see mail-credentials.md § Trigger taxonomy
```

## Where logic lives

- **Wrap / unwrap / KDF / AEAD / HPKE**: `libs/fauna-mls/src/wrapped_blob/` (shipped 2026-05-08).
- **State machine** (snapshot building, action dispatch, RPC orchestration, rotation resume, MSEK persistence): `libs/fauna-client-mail-settings/` (shipped Phase A of the client-provisioning plan, 2026-05-14; tracked internally).
- **Per-app UI**: renders the snapshot, dispatches actions. No business logic in the UI layer; per priority #2 (`principles.md` § Engineering principles).
- **MSEK + credentials persistence**: the account's mail custody, `fauna.state.mail` (fleet-only account-plane rows); see `docs/goal/behavior/mail-credentials.md` § MSEK lifecycle and [`../architecture/config-dissolution.md`](../architecture/config-dissolution.md) § The `__config` dissolution schedule → *The kinds*.

## User actions

| Action | UI touchpoint | Effect |
|---|---|---|
| Enable mail (first credential) | Toggle → add-credential dialog → submit | `EnableMail` action; provisions WrappedMsekBlob + MlsSnapshotBlob + WrappedSubmissionTokenBlob; persists MSEK in the mail custody (`fauna.state.mail`). |
| Add a credential | "Add credential" button → dialog → submit | `AddCredential` action; provisions new WrappedMsekBlob (same MSEK) + WrappedSubmissionTokenBlob; adds a credential row to the mail custody. |
| Revoke a credential | Credential row → revoke button → confirm | `RevokeCredential` action; revokes both blobs on nest; marks the credential revoked in the mail custody. |
| Rotate mail keys | "Rotate mail keys" button → modal → optional exclude list → confirm | `StartRotation` action; runs the resumable rotation algorithm per `mail-credentials.md` § Hard revoke. |
| Resume mid-crash rotation | Pending-rotation banner → "Resume" button | `ResumeRotation` action; picks up from where the previous rotation left off. |
| Disable mail | Enabled-toggle off → `mail-settings-disable-confirm` dialog → `mail-settings-disable-confirm-button` | `DisableMail` action. **CalDAV/CardDAV-aware** (`caldav-server.md` / `carddav-server.md` § Independent enablement — one shared MSEK + `default` credential serve email, CalDAV *and* CardDAV): when the mail custody's `caldav_enabled` **or** `carddav_enabled` is set, it revokes only the outbound **submission tokens** and flips `mail_enabled` off, **preserving** the shared `msek` + read credential the calendar/address-book store still seals + AUTHs under; when both are off it does the email-only teardown — revoke every credential (the soft-revoke marker, both blob kinds deleted) and turn email off, the MSEK and its grace window staying dormant for a later re-enable (`mail-credentials.md`, the Disable-mail row). Either way the snapshot returns to `enabled: false` (`MailConfig::is_mail_enabled` keys on the `mail_enabled` flag, **not** on `msek.is_some()` — so a CalDAV-only actor keeps its MSEK and still reads mail-disabled). It deliberately does **not** call the Admin-scoped, deployment-wide `set_mail_enabled(false)` — that flag boots/tears the box-level s6 mail subsystem (`/data/imap-enabled`) shared by every mailbox, so one user disabling their own mail must not turn mail off for everyone. (The asymmetry with Enable mail, which *does* flip the subsystem on, is intentional and admin-gated.) Flipping the toggle off keeps it rendering "Mail enabled" — the dialog is the real decision point. |

**Presentation rule (apple, load-bearing):** the add-credential form, rotate-keys modal, and disable confirm render as **inline `@State`-driven reveals** in the page's own eager container — never SwiftUI `.sheet`/`Menu`/`.confirmationDialog`/`.popover`, whose children don't register in the in-process automation driver; and the page container is the eager `ScrollView { VStack { GroupBox } }` shape, not a lazy `Form`/`List` (below-the-fold sections would never register). Owner: `../architecture/apps/apple-e2e-automation.md` § Registration rules. This matches the inline reveals web/linux/windows/android already render (priority #1).

## Implementation status today

**The page is built and machine-backed on all seven apps** (linux reference `apps/fauna-linux/src/settings/mail.rs`; web `MailSettingsSection.svelte`; windows `MailSettingsPanel` hosted by `SettingsMailPage`; macos + ios the one shared FaunaKit `MailSettingsView`; android `MailSettingsScreen.kt` with the FFI-free testable `MailSettingsContent`; tui `apps/fauna-tui/src/settings/mail.rs`, COMPLETE against ui.yaml as of 2026-07-18).

Surface × app:

| Surface | linux | web | windows | macos | ios | android | tui |
|---|---|---|---|---|---|---|---|
| Page render + enable/add/revoke/rotate over the shared machine | ✅ ref | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Disable-mail destructive confirm (CalDAV-aware) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| CalDAV-only lift §§1–3 (reachability gate, per-protocol MUA rows, "Mail & Calendar") | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Serve-here toggle + admin read-only indicator | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Shared password-strength meter | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Autogenerate toggle + weak-password warning | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Shared `settings_status_label` consume | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Credential-row concrete username display | ✅ ref | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Credential rows moved to the Connected apps roster (§ Where the credential rows render) | ✅ | — | ✅ | ✅ | ✅ | ✅ | ✅ |

Dated one-liners: page landed linux/web/windows 2026-05→06, apple (shared view) 2026-06-13, android in the same wave; serve-here toggle + admin indicator all-6 2026-06-08; disable-mail all-6 2026-06-09 (apple's `.confirmationDialog` stopgap replaced by the inline overlay); CalDAV-only lift all-6 2026-06-23 (tracked internally, closed; render-gate e2e-proven by linux's `test_caldav_onboarding_variants.py` real_domain × caldav-only cells; per-app caldav-only e2e assertions deferred to each app's own gate); strength meter all-6 2026-06-24; credential-row concrete username display lifted linux (ref) / android / web / windows 2026-06-04, apple from the shared view's 2026-06-13 inception; status-label lift 2026-06-25/26 web/linux/android/windows **and apple** (same window — apple was the app whose `Idle`-but-disabled mailbox read "All up to date", the shared fn's gate is the fix); WebDAV URL row built 2026-07-09 (slice 6b-2); **apple consumed both 6b-2 legs 2026-07-12 — the shared FaunaKit `MailSettingsView` dropped its hand-rolled `enabled || caldavEnabled` gate for the shared `credential_management_reachable` field (apple was the LAST of the 6 re-deriving it, so the disjunction now lives in exactly one place) and gained `mail-settings-mua-webdav-url` on the per-actor `serves_webdav_set`**. **Apple e2e:** macOS green across the surface (2026-06-14/16, in-process driver); the iOS mail-enable lazy-`Form` registration gap was fixed 2026-07-02 (eager `ScrollView` shape — the § presentation rule), and the 8 iOS re-confirm runs are tracked internally. **Android autogenerate toggle + weak-password warning** landed 2026-07-18 (consuming the shared mint-once decision). **tui** built the page across M8 slices 1–5 (2026-07-09 → 2026-07-18): the page spine + enable flow + the full credentials-list component (incl. the concrete-username row, 2026-07-17) + the per-protocol-gated MUA-instructions block + the whole add-credential form + rotate-keys / serve-here / the pending-rotation banner — `test_mail_credentials.py --client tui` green. **2026-10-01: tui's credential rows moved to the Connected apps roster** (§ Where the credential rows render); the other six follow in the connected-apps trickle-down, and the cross-app e2e helpers (`actions/mail_settings.py`) read the rows wherever the app under test lists them.

**Interrupted rotation and the live status line, 2026-10-05 (§ Architectural rules 5, § Status indicator).** Shared Rust now refuses a `StartRotation` while a rotation's sentinel is still set — overwriting it would strand a staged MSEK′ the nest may already hold — and a failed dispatch leaves the status at `RotationInProgress` while the sentinel stands (it fell back to `Idle`, so the page read "All up to date" beside its "didn't finish" banner). tui disables `mail-settings-rotate-keys-button` while a rotation is pending or running, and paints the status line from the machine's live status once hydrated, so "Syncing" shows while a change is still running (it painted only the last folded snapshot, which never carries `Syncing`). Witnessed on tui by `test_mail_settings_controls.py`; the other six apps' rotate-button gate and live status line are unverified and captured in their trickle-down rows.

**Marking passwords compromised at rotation, 2026-10-06 (§ User actions → "Rotate mail keys"; `mail-credentials.md` § Hard revoke).** tui renders one `mail-rotate-keys-exclude-item` checkbox per app password on the rotate-keys confirm form (flat-indexed, text = the display name) and dispatches the ticked ids as `StartRotation { excluded_credentials }`; witnessed on tui by `test_mail_settings_controls.py::test_a_password_marked_compromised_at_rotation_stops_working` (the marked password leaves the list and its IMAP login is refused, the other keeps working). linux, apple, android and windows dispatch the ticked set but leave the checkboxes untagged; web's boxes are disabled and it dispatches `excluded_credentials: []` — each captured in its trickle-down row.

**Mail setup on a second device, 2026-10-06 (`mail-credentials.md` § Goal; § MSEK lifecycle → *Persistence*).** A second tui seat of the same identity, launched from an empty state root after the first turned mail on and added a second password, opens this page reading mail on with both passwords listed by name — no build change, the mail custody (`fauna.state.mail`) hydrates from the account plane. Witnessed on tui by `test_mail_settings_controls.py::test_mail_setup_and_passwords_follow_you_to_a_second_device`; linux, macos and windows can run the same journey (the harness launches their second seats), web's second seat is its own fixture (`alice_second_web_device`), and ios and android have no second-seat launcher yet — each captured in its trickle-down row.

**Apple add-credential form — both shown-once secrets were unreachable, fixed 2026-07-13.** The PLAIN password field never mounted while auto-generate was ON (its default), so the client-minted ~143-bit secret had nowhere to appear and submit re-minted a *different* one (the autogenerate row above flipped to ✅ with it). Its OAUTHBEARER twin: `submit()` closed the form on *any* success, and the form is the sole mount point of `mail-add-credential-token-display` — so the one-time bearer token was destroyed at the instant the mint landed. PLAIN still closes on success; OAUTHBEARER now stays open until the user closes it, matching linux/windows/web. **Blocking both was a harness bug that made every apple mint look like a nest failure:** `login_as_nest_admin` omitted `device_id`, and the apple test agents gated `FaunaClient` construction on it while setting `isAuthenticated` first — an authenticated shell with no client, so `MailSettingsVM` was never configured and every dispatch silently no-opped (the agents now derive the device id, and a nil machine surfaces an error instead of returning quietly). With those closed, the **dedicated-mail-nest tier_3 family runs green on `--client macos`** across 8 files / 12 tests (addressbook, carddav round-trip, enable→MUA incl. the OAUTHBEARER params, bare-username auth, multi-credential auth, external send + IMAP auth, CalDAV discovery walk, MKCALENDAR round-trip).

## Persistence

All persistence lives in nest (wrapped blobs in `bridge_wrapped_mls_blobs` / `bridge_mls_snapshot_blobs` / `bridge_wrapped_submission_tokens`) or in the account's mail custody, the `fauna.state.mail` plane kind (MSEK + credentials + pending_rotation sentinel). The mail-settings page itself holds no per-app persistent state — restarting the app and re-loading the page re-derives the snapshot from the mail custody + a `fetch_*` reconciliation against nest.

## Errors & edge cases

Errors map to `MailSettingsSnapshot.error` (rendered in the page's `error-message` element) and to the dialog-local `error-message` elements (each `mail-add-credential` / `mail-rotate-keys-confirm` has its own).

| Error class | Surface | Recovery |
|---|---|---|
| Wrap-side panic (crypto failure) | Page-level `error-message`: "Couldn't prepare mail credentials. Try again or contact support." | User retries; structured-log line attached for diagnosis. |
| RPC failure mid-flow | Page-level: "Couldn't reach nest. Check your connection and try again." | App retries with exponential backoff; rotation flow uses the pending_rotation sentinel for resume. |
| Server-side rejection (rate-limited, blob slot conflict, unknown actor) | Page-level + structured reason: e.g., "Too many credentials added recently — wait 60s." | User waits / fixes; action becomes retryable. |
| Mid-rotation crash | Banner on next app start: "A previous mail-credential rotation didn't finish. Resume?" + button. | User clicks Resume; rotation completes from the persisted credentials_remaining list. |
| OAUTHBEARER token-display loss (user closed the dialog before copying) | None — not an error condition since 2026-06-04. | The credential row re-reveals it (`mail-settings-credential-item-reveal-secret`); no revoke + re-add needed. See § Credentials list. |

## Architectural rules

1. **Same shape on all 7 apps.** Element IDs, action set, snapshot subscription, dispatch surface — uniform per priority #1.
2. **No storage-mode branching in the UI.** The mail-settings page never renders different content depending on the deployment's storage mode — the axis is retired (`nest/storage-modes.md`), don't reintroduce one; per `mail-credentials.md` § Mode-uniform behavior.
3. **One state machine drives every flow.** Per-app UI is dumb rendering of `MailSettingsSnapshot` + dispatch of `MailSettingsAction`. New flows added to the state machine in shared Rust before they appear in per-app UI.
4. **Credentials list is observer-driven.** Reads from `MailSettingsSnapshot.credentials`; never polls nest directly. Updates arrive via state-machine snapshot delta.
5. **Rotation banner is gating, not blocking.** A user with a pending_rotation can still browse the credentials list and dismiss the banner (re-shown on next app start); but the snapshot's `status` field shows `RotationInProgress` and the rotate-keys-button is disabled to prevent a second rotation racing with the first.

## Don't do these

- **Don't render storage-mode anywhere.** No "you're on a plaintext-mode nest" badge; no per-mode UI variants — there is no such deployment posture any more (`nest/storage-modes.md`).
- **Don't put any secret in `MailSettingsSnapshot`.** The passively-rendered state carries no secrets; the row's reveal/copy is an explicit, on-demand `MailSettingsMachine::reveal_credential_secret` read of the account's own mail custody (§ Credentials list; owner: `mail-credentials.md` § Implementation status → *Secret re-reveal*). Caching a secret onto the snapshot to save that call is what would undo the property. *(This bullet used to read "don't show the OAUTHBEARER token after the dialog closes" — superseded 2026-06-04 when the re-reveal shipped on four apps, and corrected here 2026-07-17: it is **nest** that never holds a recoverable secret, not the user's own client, which needs one for unattended rotation.)*
- **Don't poll nest for credential changes.** Snapshot delivery is observer-driven from the state machine; nest is only contacted on user-dispatched actions and the background submission-token-refresh task (`libs/fauna-client-mail-settings/src/token_refresh.rs` — skeleton today, cadence constants only, pending per-app task-scheduler wiring; `mail-credentials.md` § Trigger taxonomy "Submission-token refresh").
- **Don't expose a "regenerate token" button on existing OAUTHBEARER credentials.** Add a new credential and revoke the old one — that's the same flow with cleaner audit semantics.
- **Don't ship per-app variants of the credentials-list component.** Indexed list, same IDs, same row layout. Platform-specific layout (e.g., iOS's swipe-to-delete) is fine *for the row affordance* but the IDs and the action dispatch are uniform.

## Done definition

- [x] `mail-settings`, `mail-add-credential`, `mail-rotate-keys-confirm` pages registered in `tests/e2e-unified/ui.yaml`.
- [x] All 7 `tests/e2e-unified/ui-actual-<app>.yaml` snapshots have the pages with matching IDs. **7 of 7 confirmed.** web/macos/ios/android/tui carried full element inventories already; the earlier linux/windows authoring gap (both apps implemented the page in full but their ui-actual files carried no `mail-settings`/`mail-add-credential`/`mail-rotate-keys-confirm` block) was backfilled — linux 2026-08-20, windows 2026-08-26 (both enumerated directly from the built page's ID call sites, not inferred from ui.yaml).
- [ ] `pytest tests/e2e-unified/tests/test_mail_credentials.py` 100% pass across the per-machine app matrices (iOS re-confirm runs outstanding — § Implementation status today).
- [x] `ui-actual-lint` exits 0.

## Reading list

1. `docs/goal/behavior/mail-credentials.md` — the feature behavior this page renders.
2. `docs/goal/behavior/imap-server.md` § Authentication — the AUTH path the credentials provision for.
3. `docs/goal/architecture/owner-key-material.md` § Path B — MSEK lifecycle.
4. The client-provisioning implementation plan, 2026-05-14 (frozen provenance; tracked internally).
5. `tests/e2e-unified/ui.yaml` — registered IDs.
