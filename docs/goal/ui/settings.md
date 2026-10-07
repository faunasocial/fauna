# Settings — target state

Owns: settings, identity-export
Status: partially-specified — § Navigation model + the rail set are ratified (2026-06-03 / 2026-06-28) and built on all seven apps; § Recovery kit is ratified (2026-08-01); the section is built on tui (2026-08-01/02), linux and web (2026-08-17), macOS and iOS (2026-08-21), windows (2026-08-24) and android (2026-10-06, its stolen-identity leg still to follow); § State & data shape / § Persistence remain TBD — resolved by the settings-snapshot design
Authority: ui.yaml (`settings` page) owns element IDs + per-page element scope; this doc owns the Settings shell (rail set, sidebar-swap model, `settings-nav-back`, the two-element sub-page nav), the root-page behavior (account / quota / handle / sign-out / delete / data export / identity export / recovery kit / push), and the Rust/app split; the recovery kit's *surface placement and view state* only — the RecoveryKey itself, every ceremony behind these buttons, and the pending-replacement condition → [`../behavior/identity-succession.md`](../behavior/identity-succession.md); each rail sub-page's content → its own owner doc ([status.md](status.md), [devices.md](devices.md), [folders.md](folders.md), [mail-settings.md](mail-settings.md), [nests.md](nests.md), [nostr.md](nostr.md); settings-logs → [observability.md](../architecture/apps/observability.md); muted-words → [content-moderation-and-ranking.md](../architecture/content-moderation-and-ranking.md); task-delegation → [participants.md](../behavior/participants.md); subscriptions → [monetization.md](../behavior/monetization.md); web → [web-content-hosting.md](../behavior/web-content-hosting.md)); multi-account switcher behavior → [long-term-store.md](../architecture/long-term-store.md) § Multi-account evolution.

## Goal

The Settings page is the catch-all account / preferences surface: account info (actor ID + copy), quota display, handle change, inbox-mode and spam preferences, email filter rules, push notification toggle, data export, sign-out, account deletion, and self-host onboarding link. Web also inlines the Status info on this page. Every desktop app reaches the sub-pages through the sidebar-swap shell below.

## Navigation model (canonical — ratified 2026-06-03)

Settings uses the **same shell model as admin** (`admin.md` § Navigation model): on desktop a **distinct Settings shell** you enter and exit, *not* a modal window and *not* a single scroll of short sections flattened onto one page. Ratified by the user 2026-06-03 (linux-first; the prior linux state — a header-cogwheel `adw::PreferencesWindow` modal plus a confusingly-mislabelled "Status" left-menu item that opened a scroll-of-embedded-settings — was the confusion this replaces). Concretely:

1. **Entry — a left-menu `settings-tab`.** A single Settings entry in the main navigation (sidebar row / tab) opens the shell. The former header **cogwheel is removed** (`Ctrl+,` is kept as a shortcut to the shell). There is no separate Preferences window. **What that entry lands on — the Status sub-page, never the sub-page a previous visit left open — is the cross-shell canonical-entry rule owned by [`README.md`](README.md) § Navigation model** (ratified 2026-08-13); it binds this shell and admin's identically, so it is stated once there rather than twice here.
2. **Internal switcher — a vertical sidebar-swap on desktop; idiomatic on mobile.** Inside the shell, the settings pages are switched by a **vertical nav rail that replaces the main app sidebar in place** while Settings shows (entering Settings swaps the normal nav for the settings pages in the *same* sidebar slot — no second rail, no horizontal sub-tabs, no modal), and `settings-nav-back` (step 3) swaps it back. The rail is **flat, one entry per page** (mirroring admin's flat rail — not a scroll of stacked sections): **Status · Account · Members to review · Privacy · Muted words · Personalization · Community labelers · General · Encryption · Devices · Sessions · Folders · P2P · Nostr · AT Protocol · Subscriptions · Web · Mail & Calendar · Mail aliases · Mail spam · Mail export · Mail import · Import from other services · Mail lists · Mail list members · Nests · Task delegation · Connected apps · Terminal (tui only) · Logs** (canonical target order — web matches it exactly, `apps/fauna-web/src/routes/settings/+layout.svelte`; linux and windows have not yet converged the Subscriptions/Web positions to it, an ordinary per-app rail-order gap tracked in § Implementation status today, not a doc/code contradiction; the `mail-settings` page is titled "Mail & Calendar" — the shared credential/connection hub for both the email and calendar protocols, per `mail-settings.md`; the email-specific `mail-*` sub-pages keep their names). **Mail import** (ui.yaml page `mail-import`, ratified 2026-08-27) — ADDED to this canonical order here (this slot was missed when the page's element-ID set was ratified; `mailbox-migration.md` § UX shape names it as `mail-export`'s twin, so it takes the adjacent rail slot right after Mail export) — this doc owns only the rail slot; behavior is owned by [`../behavior/mailbox-migration.md`](../behavior/mailbox-migration.md). **Import from other services** (ui.yaml page `archive-import`, IDs user-approved 2026-09-08) takes the slot directly after Mail import — the second import wizard beside the first; this doc owns only the rail slot, the page is owned by [`../behavior/archive-import.md`](../behavior/archive-import.md). **Personalization** (ui.yaml page `personalization`, titled "Personalization") and **Community labelers** (ui.yaml page `labeler-catalog`) sit right after Muted words as its sibling personalization surfaces — the unified home hubbing trained topic factors + the labeler catalog; both are owned by [`content-moderation-and-ranking.md`](../architecture/content-moderation-and-ranking.md) (§ Composition + § Tier-3) — this doc owns only the rail slots. **AT Protocol** (ui.yaml page `atproto`, renamed from the interim `bluesky-settings` key 2026-07-22 and from `bluesky` 2026-09-28) — RATIFIED here 2026-07-22 as its own rail sub-page, placed after Nostr (the sibling federation-protocol bridge settings page); the page itself (the integration-depth selector + its per-level sub-settings, of which the login-plane groups are the deepest level's) is owned by [`atproto.md`](atproto.md) — this doc owns only the rail slot. *(Rail-entry provenance, per [`../behavior/participants.md`](../behavior/participants.md): **"Linked nests" renamed to "Nests"** with the v1 nest-trust facet (ui.yaml page `linked-nests` → `nests` + IDs, and the page doc [`nests.md`](nests.md), landed 2026-07-07; per-app render frontier → `nests.md` § Implementation status today) — and **"Task delegation"** — RATIFIED 2026-07-08 (user) as its **own rail sub-page** (ui.yaml page `task-delegation` + the indexed `task-delegation-kind-item` component; placed **after Nests** as the cross-participant capstone), whose shared `fauna-client-delegation` view-model composes the `fauna.state.delegation` pins + the live `fauna.delegation.observe` lease into per-kind runner/assignment rows; built on all **seven** apps as of tui's leg 2026-07-29.)* **Sessions** (ui.yaml page `sessions`, titled "Sessions") — RATIFIED here 2026-09-25 (user) as its own rail sub-page, placed directly **after Devices**: "machines enrolled" beside "sign-ins live now", deliberately not folded into the roster-only Devices page and not added to the build-once Account sub-page (§ Live-data placement); unbuilt on every app, IDs approved under rule A and allocated with the tui build; the page — the live-sessions list, revoke one / sign out everywhere else, the 24-hour lock — is owned by [`sessions.md`](sessions.md), this doc owns only the rail slot. **Connected apps** (ui.yaml page `connected-apps`) — RATIFIED here 2026-09-05 as its own rail sub-page, placed **after Task delegation** as the third participants-cluster surface: the roster of everything acting for the user from outside the seven apps, the merged home of the OAuth-grant, app-password and NIP-46 rosters; unbuilt, IDs deferred to rule A at the tui build; the page is owned by [`connected-apps.md`](connected-apps.md) — this doc owns only the rail slot. **Muted words** (ui.yaml page `muted-words`, placed after Privacy as the sibling personal content-filtering surface to the spam preferences on Privacy) is the deterministic keyword-mute filtering surface — its behavior + data shape are owned by [`content-moderation-and-ranking.md`](../architecture/content-moderation-and-ranking.md) (the `muted-keywords` registry row; a per-keyword weight, default −1000, offered on the page as a per-row two-level picker — § Muted words — the level picker, ratified 2026-10-02, unbuilt), not here; the shell owns only its rail slot (per-app UI: linux + web + android + windows landed 2026-07-07/08, apple (macOS + iOS) landed 2026-07-12, `fauna-tui` 2026-07-29 — all **seven** apps now render the sub-page and both collapse surfaces). **Members to review** (ui.yaml page `member_review`, titled "Members To Review"; nav id `member-review`) — RATIFIED here 2026-08-17, the rail slot for the **permanent** half of the post-succession unattested-member review, whose page and ID family the user approved 2026-08-16 as *"a Settings sub-page on the `devices`/`file-sets` precedent, never a top-level nav entry"*. Placed directly **after Account**, because the Account sub-page's Recovery Kit section renders the *ephemeral* half of the very same review and the two belong side by side. The surface's behavior — the two-surface split, `(person, raising event)` keying, and the rule that *Remove*'s verdict is **derived** from the eviction rather than chosen — is owned by [`../behavior/succession-propagation.md`](../behavior/succession-propagation.md) § Propagation; this doc owns only the slot and the one shell-level ruling below. **The rail entry is unconditional, and the page is empty most of the time by design** — it holds only a backlog somebody explicitly postponed, so it is empty before any recovery and empty again once the backlog is worked through. Gating the entry on the backlog being non-empty was weighed and refused: the page is the ratified permanent *home* of a deferred backlog, a home reachable only while occupied is not one, and the gate would leave `member-review-empty` — approved with the page — unreachable by any route a user has. That emptiness is also why the page needs no dismiss affordance and never becomes standing clutter, which is the property the whole two-surface split was ratified to buy. **Devices** (ui.yaml page `devices`) and **Folders** (ui.yaml page `folders`) occupy the former single `Sync` slot — placed between Encryption and P2P because the three form the **device/sync administration cluster** (the roster, the folder control plane, and peer connectivity sit together). **Terminal** (ui.yaml page `tui-settings`, titled "Terminal") is tui's own platform-exclusive rail sub-page — the terminal app's external-media handoff mode (ask / always / never); `platforms: [tui]` in ui.yaml, so no other app renders it (behavior owned by [`../architecture/apps/tui.md`](../architecture/apps/tui.md) § External media handoff) — placed directly before Logs, matching tui's own rail order. The **Logs** sub-page (last) is the app's durable log record — it renders the in-process `fauna_log` ring newest-first with a severity filter, copy, and clear (`docs/goal/architecture/apps/observability.md` § Surfaces); ui.yaml page `settings-logs`. The former standalone **Status page is folded in as the first rail sub-page** (identity / connection / sync / p2p / quota / node) — it is no longer a separate left-menu item. Mobile (ios / android) keeps its platform-idiomatic settings nav. The *page set* and *IDs* are uniform across apps; only the mobile widget differs.
3. **Uniform exit — `settings-nav-back`.** The rail exposes `settings-nav-back`, which **exits the shell and returns to the non-settings app** (the primary view, e.g. Conversations). It is the single "leave settings" affordance, parallel to `admin-nav-back` — and both land on the **same** primary view: `admin-nav-back` also returns directly to the main view (Conversations), not to this Settings shell (corrected 2026-06-07, user — admin is a top-level nav peer, not nested under Settings; see `admin.md` § Navigation model).

**Devices + Folders sub-pages (ratified 2026-06-28 — sync/folder unification).** Two surfaces that were elsewhere fold into this shell; settings.md owns only the **shell / navigation structure** for them, never their content:

- **Devices** (rail label "Devices", ui.yaml page `devices`) — the **device roster**, formerly the top-level **"Peers"** page. `Peers` is **removed as a top-level nav item**; its roster now lives here. The roster's behavior, data shape, and element IDs are owned by **`docs/goal/ui/devices.md`** — do not duplicate them here.
- **Folders** (rail label "Folders", ui.yaml page `folders`) — the **folder control plane** (list + create wizard + per-set config + desktop location binding + conflicts), **renamed from the former `Sync` sub-page**. It absorbs the folder list/wizard that used to live under Peers, and the **conflict-resolution surface** — the standalone "Sync conflicts" page (and Windows's separate `conflicts-tab` nav entry) is **retired**; conflicts now render on this Folders sub-page. The folder behavior, data shape, and element IDs are owned by **`docs/goal/ui/folders.md`** — do not duplicate them here.

**Mobile (iOS / Android).** Devices and Folders are reached **inside Settings** (as sub-pages of the idiomatic settings nav), **not** as a top-level tab or a "More"-menu entry. Same page set + IDs as desktop; only the navigation widget differs (the existing mobile-idiomatic rule above).

**Live-data placement.** Because the shell is built once (the modal was rebuilt on each open), all live nest-data display lives on the **Status sub-page** (`account-actor-id`, `quota-section`/`quota-inbox`/`quota-storage`/`quota-devices`, `status-actor-id-copy-btn`, `status-node-url-copy-btn`, connection, node) — wired to live updates. The **Account sub-page is pure actions** (`new-handle`/`change-handle`, identity export, data export, `sign-out-button`, `settings-delete-account-button`) so build-once is safe. *(A stale "bluesky link" entry was removed from this list 2026-07-22 — no app renders one; Bluesky linking lives on the dedicated `atproto` page, [`atproto.md`](atproto.md).)* Privacy/General/mail pages self-fetch (build-once-safe).

**ui.yaml mapping.** The `settings` page (account/quota/inbox-mode/spam/filters elements) is realized across the Status/Account/Privacy rail sub-pages; the rail's remaining entries are the already-separate ui.yaml pages named in the rail list above (each owned by its page doc). The e2e action layer navigates per-sub-page with the admin-style two-element nav `{"view":"settings"},{"view":"settings","id":"<page>"}` (the cross-page convention — `ui/README.md` § Navigation model); the explicit id `"status"` and the bare `{"view":"settings"}` (no id) both land on the Status sub-page on every app — there is no remaining single-scroll app whose sub-id is ignored (corrected 2026-07-30: web's `routes/settings/[[subpage]]` and windows's `SettingsShellPage` are both genuinely sub-paged, matching linux/tui; web's Status rail entry used an empty-string id where every other app uses the explicit `"status"` id, which is what let this go undetected), so the shared cross-app tests stay green.

**Reference implementation:** **linux** (`apps/fauna-linux/src/views/settings_shell.rs` + `views/nav_rail.rs` + `app.rs`, 2026-06-03) — the settings sub-stack is the content (the main content-stack's "settings" child); the rail (with `settings-nav-back` on top) is built by the shared `build_nav_rail` helper (the same helper the admin rail now uses) and swapped into the `OverlaySplitView` sidebar slot via the content-stack's visible-child notify; `settings-nav-back` → Conversations.

**Every sub-page must paint a real, visible heading — not just title metadata a shell container ignores.** This binds Settings identically to Admin and is stated once, cross-shell, at [`README.md`](README.md) § Navigation model rather than twice here; the per-app conformance measurement is in § Implementation status today, below.

## Layout & flow

Sections, top to bottom (realized as the Status/Account/Privacy/General sub-pages on shell apps):

1. **Account.** Actor ID display (`account-actor-id`), copy button (`account-actor-id-copy-btn`), `account-settings-link` as page landmark.
2. **Quota.** `quota-section` — inbox / storage / devices breakdown.
2b. **Feature limits.** `feature-limits-section` — one row per controversial-class registry member, each carrying its bounds, the headroom left, and **which tier set each bound**. Placed directly after Quota as its sibling "what bounds me" surface, and a section on this page rather than a rail entry (the `recovery-kit-section` precedent). This doc owns **only that slot**: the plane's registry, rule-setter tiers, policy shape and the hide-or-disable rule are owned by [`../architecture/dynamic-features.md`](../architecture/dynamic-features.md) (§ Transparency & auditability, § Evaluation points item 2) — do not duplicate them here. The section renders only once the transparency read resolves; a limits section painted before its data would tell the user nothing restricts them. **The same slot hosts the self-limits control (design 2026-09-19, IDs user-approved under rule A 2026-09-25; built on tui 2026-09-27, the other six follow):** each row gains `feature-limits-own-summary` and `feature-limits-own-edit-button`, opening the shared `feature-policy-editor` family in place — the read and the write of one plane belong together, so the control follows this section wherever it is mounted (this landing, or the Status sub-page on a shell app — § Live-data placement). Everything about the editor itself is [`../architecture/dynamic-features.md`](../architecture/dynamic-features.md) § Authoring surfaces'.
3. **Identity export.** Show-QR toggle + warning (see "Identity export" below). Placed before Handle change, mirroring the shipped Apple order.
4. **Recovery kit.** `recovery-kit-section` — status line + the create / replace / lost / stolen actions (see "Recovery kit" below). Placed immediately after Identity export: both are root-secret affordances, and a user who just read "whoever scans this QR gains the identity" is in exactly the frame of mind the recovery kit needs.
5. **Credential store.** `credential-store-section` — status line + the change-passphrase affordance (see "Credential store" below). Placed after Recovery kit as the third root-secret-custody affordance (export the seed / escrow the seed / how the seed is held on this device). Renders **only when the app's active credential backend has per-device settings to offer** — today exactly the tui sealed arm; an OS-store backend has nothing user-actionable here.
6. **Handle change.** `new-handle` input + `change-handle` button.
7. **Inbox mode.** Component `inbox-mode-selector` — open / allow_knock / contacts_only / closed. **The selector shows the account's stored mode, never a default.** Until `inbox_mode_get` has answered, the mode is *unknown*: all four paint unmarked and the page's `error-message` carries `settings.privacy_page.inbox_mode_unknown` ("…has not loaded, so none of the four below is marked. Your setting is unchanged…"). Marking a mode nobody fetched is forbidden — this is a privacy control, so a guessed value is a false statement about who can reach the user, and `open` (the natural-looking default) is the most permissive of the four. Displaying the stored mode must not write it back: painting the selection is not a user choice and issues no `inbox_mode_set`.
8. **Spam moderation.** Component `spam-moderation-controls` — preferences, thresholds, training toggles.
9. **Email filters.** Component `email-filter-panel` — rules list, add filter, per-rule type/value/action.
10. **Push notifications.** Toggle (see "Push notifications" below).
11. **Data export.** `Export My Data` button (see "Data export" below).
12. **Run Your Own Nest.** Link to the self-host provisioning entry (web: the `/onboarding` wizard — owner [`../behavior/onboarding.md`](../behavior/onboarding.md); there is no `/setup` route); always shown, separated by a divider.
13. **Sign out.** `sign-out-button` (clears local credentials + state, returns to onboarding).
14. **Delete account.** `settings-delete-account-button`. The confirmation's own text reminds the person to take their data first — the account data export (item 11) and, where they have mail, the mail export (Settings → Mail → Export) — as a sentence of the existing confirmation on every app, with no element of its own (ruled 2026-10-01: [`../behavior/mail-export.md`](../behavior/mail-export.md) § Don't do these promised the reminder without naming its surface; this item is that surface. **Unbuilt on every app** — no string exists yet).

Per-app notes (from ui.yaml):
- iOS: idiomatic settings nav (a `NavigationStack` of sub-pages, not a sidebar swap); inbox mode + spam live on the **Privacy sub-page** (the shared `PrivacySettingsView`), not the root List. `account-settings-link` is a `NavigationLink` on the root List.
- Windows / macOS / linux / web: the sidebar-swap shell — one sub-page visible at a time (see § Navigation model).
- Web: also inlines Status info (status-* copy buttons live here, not on a separate /status route).

## Element IDs

ui.yaml (`settings` page block) owns the inventory — read it there; this section carries only the behavior notes:

- **Root/account family:** `page-heading`, `account-settings-link`, `account-actor-id` + `account-actor-id-copy-btn`, `quota-section`/`quota-inbox`/`quota-storage`/`quota-devices`, `new-handle` + `change-handle`, `settings-delete-account-button`, `sign-out-button`, `error-message`; components `inbox-mode-selector`, `spam-moderation-controls`, `email-filter-panel`.
- **Multi-account switcher family** (`account-switcher-list`, `account-switcher-item`, `account-item-handle`, `account-item-active-indicator`, `account-add-button`, `account-remove-button`, `account-require-confirm-toggle`; optional `account-switcher-button` for compact shells, `account-activate-reauth-prompt` + `account-activate-reauth-confirm-button`/`account-activate-reauth-cancel-button` (re-auth-on-activate, platforms with no native OS prompt), and `account-open-new-instance-button` (concurrent instances)): this page is the render home only — behavior, data shape, and per-app rollout are owned by [`../architecture/long-term-store.md`](../architecture/long-term-store.md) § Multi-account evolution (re-auth-on-activate) and [`../architecture/apps/account-scoping.md`](../architecture/apps/account-scoping.md) § Concurrent instances (the new-instance spawn).
- **Identity-export family:** `identity-export-section`, `identity-export-description`, `identity-export-show-qr-button` (one toggle; label flips `settings.identity_export.show_qr` ↔ `hide_qr`), `identity-export-warning`, `identity-export-qr`. The last two render **only while the QR is shown**. **Approved and allocated in `ui.yaml` 2026-07-11** (rule A). See § Identity export.
- **Recovery-kit family:** `recovery-kit-section` (container/scope), `recovery-kit-status` (the one status line), and the four actions `recovery-kit-create-button` / `recovery-kit-replace-button` / `recovery-kit-lost-button` / `identity-stolen-button` (with its type-to-confirm gate `identity-stolen-confirm-field`), plus `recovery-pending-veto-button` (renders **only** while a seed-alone replacement pends), `recovery-kit-escrow-reseal-button` (renders **only** in the no-escrow state — the kit-in-hand re-put repair; ID user-approved 2026-08-16, asked 2026-08-10) and `recovery-kit-sweep-retry-button` (renders **only** while the post-succession group sweep shows unfinished work — see § Recovery kit → *Finishing an unfinished group sweep*; ID user-approved 2026-08-16), the let-go trio `recovery-kit-unreadable-status` / `recovery-kit-let-go-confirm-field` / `recovery-kit-let-go-button` (rendered **only** while a dead generation exists — see § Recovery kit, the fifth act; IDs user-approved 2026-10-02) and the six post-succession aftermath progress lines `recovery-kit-backup-regrant-status` / `recovery-kit-mls-reseal-status` / `recovery-kit-grant-remint-status` / `recovery-kit-corpus-reseal-status` / `recovery-kit-drafts-reseal-status` / `recovery-kit-mail-burn-status` (all optional, and rendered in that order — leg 7's drafts line above leg 6's mail line, because the burn is the only leg that takes something away; the fourth's IDs user-approved 2026-08-03, the fifth's, sixth's and seventh's user-approved 2026-08-16 (asked 2026-08-05, 2026-08-11 and 2026-08-16); see § Recovery kit → *The post-succession aftermath's progress lines*), plus the four **ephemeral member-review pass** IDs `member-review-row` / `member-review-keep-button` / `member-review-remove-button` / `member-review-defer-button` (all optional, rendered only inside the ceremony flow that raised them; IDs user-approved 2026-08-16, asked 2026-08-10 — see § Recovery kit → *The ephemeral member-review pass*). When a ceremony returns a kit, the section renders it through the **onboarding screen's own** display IDs — `recovery-kit-secret-display`, `recovery-kit-secret-copy-btn`, `recovery-kit-qr` — rather than settings-specific twins, because it is the same artifact shown the same way (priority #3). **Approved and allocated in `ui.yaml` 2026-08-01** (rule A). See § Recovery kit. **No banner IDs live here** — the standing pending-replacement banner is a `critical-alerts` feeder, not a Settings element ([`../behavior/critical-alerts.md`](../behavior/critical-alerts.md)).
- **Credential-store family** (all optional): `credential-store-section` (container/scope), `credential-store-status` (how credentials are held on this device), `credential-store-rekey-button`, and the modal it arms — `credential-store-rekey-modal`, `credential-store-rekey-seed-nudge`, the three **masked** inputs `credential-store-rekey-current-input` / `-new-input` / `-confirm-input` (the tui-unlock masking contract: registered text is a same-length bullet mask), `credential-store-rekey-submit-button` / `-cancel-button`, and `credential-store-rekey-success`. Optional because the section renders only when the app's active credential backend has per-device settings to offer — today exactly tui's passphrase-sealed headless arm; **deliberately not tui-declared**, per the shared "credential store settings" concept framing (user-approved 2026-08-05) — another app that grows a store affordance adopts these IDs. This page is the render home; the sealed-arm mechanics (what the re-key does, crash-safety) are owned by [`../architecture/apps/tui.md`](../architecture/apps/tui.md) § Credential storage. **Approved and allocated in `ui.yaml` 2026-08-06** (rule A). See § Credential store.
- **Optional:** `settings-nav-back` (desktop shells), `sign-out-confirm-button` (the uniform inline destructive confirm), `settings-delete-confirm-field` (type-to-confirm gate for account deletion — see § User actions; optional because linux/android still gate behind a native OS Yes/No alert with no drivable id, a separate, non-headless-testable confirm of its own — **apple left that group 2026-08-17** and now ships the field on both targets). **Platform:** `close-to-tray-toggle` (linux + windows, desktop tray apps — behavior owned by `apps/linux.md` / `apps/windows.md` § System Tray), `settings-autostart-toggle` (linux + windows + macos — `apps/linux.md` / `apps/windows.md` § App Lifecycle), `settings-icloud-backup-toggle` (macos + ios only — `apps/ios.md` § Credential Storage); this page is the render home only for the last two, same as `close-to-tray-toggle`.

State fields (from ui.yaml): `settings.inbox_mode`, `settings.spam_threshold`.

## State & data shape

TBD — resolve before behavior-changing work.

Target shape (proposed):
- `settings_snapshot()` → `SettingsSnapshot { account: AccountInfo, quota: QuotaInfo, handle_change: HandleChangeState, inbox_mode: InboxMode, spam: SpamPrefs, filters: Vec<EmailFilter> }`.

## Where logic lives

- **Handle change** (validation, conflict check, server call). **Format validation = shared Rust** (`fauna_protocol::handle::validate_handle` — 3–63 chars, lowercase ASCII alphanumeric + hyphens, no leading/trailing hyphen; the canonical rules, also enforced by the nest on `fauna.profile.handle.change` and on register/claim/invite). Apps call it pre-submit for instant, identical feedback (linux natively; apple/windows/android via the UniFFI `validate_handle`; web via a wasm wrapper). **Conflict** (handle already taken) stays **server-authoritative** — surfaced from the change RPC's rejection in `error-message`. (The onboarding handle-check's DNS / nest-probe / price phases do **not** apply: a settings handle-change renames the bare local part on the user's existing nest, it does not claim an identity on a domain.)
- **Inbox mode + spam threshold** persistence. **Shared Rust (done).** Inbox-mode over `fauna-client-contacts::ContactsClient::inbox_mode_{get,set}` (`libs/fauna-client-contacts/src/lib.rs:171`/`185`); spam over `fauna-client-spam::SpamClient::{get,set}_preferences` (`libs/fauna-client-spam/src/lib.rs:40`/`54`), with band labels via `fauna_protocol::spam::spam_threshold_band` (`libs/fauna-protocol/src/spam.rs:121`). Adoption matrix in § Implementation status today.
- **The inbox-mode selector's rows.** `fauna_protocol::contacts::INBOX_MODES` — the four `(wire token, label)` pairs in canonical button order, both halves shared (lifted 2026-08-22, the same shape as the spam band labels above). The wire half still derives from `fauna_core::data::InboxMode::to_wire`; what moved is the **label** half, which linux and tui each held as a byte-identical private table, each under a comment asserting it matched the other — a promise no build checked, so a fifth mode or a renamed label would drift on whichever app was edited second. Position is load-bearing (both shells index a parallel radio-button vector by it) and is now test-pinned along with token canonicality and distinctness. **Corrected 2026-08-23 — the "five non-Rust apps are unaffected" claim below was false and is retired.** `inboxModeValues` is a `wasm_bindgen` export; only **web** can call it. **apple, windows and android** structurally cannot, and hand-wrote the four tokens *and* their labels/descriptions themselves until `fauna-ffi`'s `inboxModeOptions` (`libs/fauna-ffi/src/contacts_client.rs`, value-format-gated, no `fauna-ffi`/Go-binding facade for `fauna_protocol::contacts::INBOX_MODES` itself — a parallel `LocalizedText`-keyed catalog, since that table carries already-resolved English text for tui/linux's direct paint, not i18n keys) gave them the same door; **android adopted it 2026-08-23**. § Implementation status today tracks the remaining apple + windows adoption.
- **Email filter evaluation.** **Shared Rust (done).** The MTA perimeter evaluates stored rules via `fauna_mail::filter::evaluate` (uniffi-exported, called from the Go MTA) — consistent rule semantics with no app involvement; see `email-filters.md` § Email filter rules for the full evaluation + action-composition contract.
- **Email filter create-dialog encoding.** `fauna_protocol::email::encode_filter_rule(kind, value)` + `encode_filter_action(&FilterActionInputs)` map the `filter-rule-type` / `filter-action-select` dropdown tag + `filter-rule-value` + the action's own inputs onto the typed `EmailFilterRule` / `EmailFilterAction` wire variants — the single source of truth that keeps the emitted shape from drifting (the mail analog of feed's `encode_filter_rule`; `email-filters.md` § Email filter rules). The dropdown tags are the **PascalCase variant names** (`SenderIs`, `SenderDomain`, `SubjectContains`, `BodyContains`, `HeaderExists`; actions `SUPPORTED_ACTION_KINDS` = `Allow` / `Discard` / `Reject` / `Forward`); an unknown tag is an error, not a silent fallback, and an empty Reject reason rides `DEFAULT_REJECT_REASON`. **`FilterActionInputs` is one typed struct holding every action input the form collects** — the tag, the Reject reason, and a Forward's `filter-forward-address` destination and `filter-keep-local-copy` checkbox (default checked = `copy`; unchecked = `redirect`, `mail-forwarding.md` § Per-rule "forward to") — so each action kind the form learns (file-into, add-label, auto-reply) adds a field, never another encoder signature. The Forward destination is trimmed and checked with `validate_forward_target(address, &[])` — the nest's own rule-path check, with no hosted-domain bar (that bar is forward-all's alone); the predicate lives beside the wire enums in `fauna_protocol::email` and `fauna_mail::forward_config` re-exports it. **Edit is the exact reverse:** `describe_filter_action(action) -> Option<FilterActionInputs>` returns every input a stored action carries (a `redirect` Forward reopens with the box unchecked, a Reject with its reason), `None` for an action no form collects, and `filter_is_editable(filter)` (one rule, both describable) gates each row's `filter-edit`. **A form gates its edit affordance on `filter_is_editable_for(rules, action, kinds)`**, passing every `SUPPORTED_ACTION_KINDS` entry, so it never opens a stored action it would save back narrowed. The Rust-native tui and Linux apps call these directly; the others go through `fauna-ffi` (Apple / Windows / Android) or `fauna-wasm` (web): `encodeEmailFilterActionInputs` / `describeEmailFilterActionInputs` / `emailFilterIsEditableFor` (`filterIsEditableFor` on web). Adoption matrix in § Implementation status today.
- **Email filter action display label.** `fauna_protocol::email::filter_action_label(action) -> LocalizedText` — the `filter-action` list-row badge for a *stored* filter, covering all seven `EmailFilterAction` variants (unlike `describe_filter_action`, which is `None` for the richer variants no form collects because that call gates dialog *editability*, not display). Lifted 2026-08-22: linux/tui rendered the bare wire-variant name for the four richer actions, windows fell back to raw snake_case tokens, web hardcoded the literal `"Reject"` for every struct-variant action regardless of which one it actually was, and apple alone had a complete, correct label — this converges every app on apple's canonical short labels (`status.email_filters.action_file_into`/`action_forward`/`action_auto_reply`/`action_add_label`, new keys; `action_allow`/`action_discard`/`action_reject` reused). Rust-native linux/tui call it directly; web via the wasm `emailFilterActionLabel` (`fauna-wasm`); android via the UniFFI `emailFilterActionLabel` (`fauna-ffi`), resolved through the existing `localized()` helper. Apple's `FfiEmailFilterAction.label` (FaunaKit) now delegates to the same UniFFI export, resolved through `renderLocalizedText`. **windows now consumes it too** (2026-08-25) — `EmailFilterPanel.xaml.cs`'s hand-rolled `ActionLabel` switch is deleted; the row's badge resolves via `S.Resolve(FaunaFfiMethods.EmailFilterActionLabel(f.action))`, the same shared UniFFI export every other app now calls.
- **Identity export.** **Shared Rust, both halves.** Payload = `fauna_core::identity_qr::IdentityQr::to_uri(secret, handle)` (the same `(identity, handle)` URI the import parser accepts — [`../behavior/onboarding.md`](../behavior/onboarding.md) § 1 Identity). Rendering = `fauna_core::qr_matrix::qr_matrix(uri)` → a `QrMatrix { size, modules }` boolean grid at EC level M; each app draws the grid with its own toolkit and adds the `QUIET_ZONE_MODULES` margin. **No app links a platform QR library** — that would be seven encoders to keep in step (priorities #1/#2). The secret itself is read from the platform secure store, never from a settings snapshot ([`../architecture/apps/common.md`](../architecture/apps/common.md) § Credential storage), so this section does **not** depend on this doc's TBD § State & data shape.
- **Sign-out** (clear local credentials + state, return to onboarding). Transport shared (`fauna-client-account`); the residual per-platform glue (credential-store/session clear + navigation) is inherently client-side.
- **Account deletion** (server call to queue a delayed, cancellable pending action; no local cleanup at request time). **Shared Rust (done)** — `fauna-client-account::AccountClient::delete()` / `fauna.account.delete`, transport-generic over `RpcRequester` like the rest of this crate; every app calls it the same way (linux natively, apple/windows/android via the UniFFI face, web/tui via their own bindings). The app makes no local state change or navigation when the call returns — **no sign-out, no credential or store erase, no navigation** — the account stays active until the pending action executes later, so unlike sign-out there is nothing client-side to clear yet; the pending-actions row (§ Pending actions) is the receipt and its cancel button the way back. Proven end-to-end (confirm → the app is still on the page, signed in → force the pending action due → actual removal) by `test_delete_account_via_confirm_field`. **Ruled 2026-08-26 — the doc is right; four app targets had drifted, all now fixed (macOS/iOS/android closed 2026-08-27, web — the last leg — closed 2026-08-31):** web (`doDeleteAccount` → `logout()` + `goto`), macOS + iOS (the shared FaunaKit `AccountSettingsVM.deleteAccount` plus `AccountSettingsView`'s `onAccountReset()`) and android (`AccountSettingsVM.deleteAccount` → `registry.clearAll()` … + `onSignOut()`) tore the session down at confirm on the false premise that the account is already gone — stranding the user outside the cancel window with only their exported identity secret as the way back in; linux, tui and windows conformed from the start, and the sibling delayed verb `fauna.profile.handle.change` leaves the session up on all seven. The teardown was a bug, not a second design: § Pending actions was user-approved on exactly this affordance. The per-machine fixes are recorded; the cancel *surface* on those four apps is a separate, still-open item — the pending-actions trickle-down. **What happens at execution:** the account's next token mint is refused `fauna.auth.not_registered` ([`../behavior/login.md`](../behavior/login.md) § Errors — opaque by design, no deleted-vs-suspended oracle), so the app must **not** auto-erase on that refusal (a suspension may be lifted; erasing local content on an opaque 403 would be data loss) — the user's explicit sign-out is the erase. [`../architecture/apps/account-scoping.md`](../architecture/apps/account-scoping.md) § Erasure follows scope binds at execution and by the user's act, never at request time.
- **Quota fetch / display.** TBD — shared Rust returns numbers; app formats with i18n.

## User actions

| Element | Action | Where it runs |
|---|---|---|
| `account-actor-id-copy-btn` | Copy. | App glue. |
| `new-handle` + `change-handle` | Submit handle change. | Format check via shared `fauna_protocol::handle::validate_handle`; on pass, `fauna.profile.handle.change` (`fauna-client-account`). A taken handle surfaces from the server reply in `error-message`. |
| `inbox-mode-*` (4 buttons) | Set inbox mode. | Shared Rust — `fauna-client-contacts::ContactsClient::inbox_mode_{get,set}` (linux calls via its `FaunaClient` wrapper — `settings/privacy.rs:120` for the set on toggle, `settings/privacy.rs:153` for the fetch on page build). |
| `spam-threshold` / `phishing-threshold` / `save-spam-prefs` | Spam prefs. | CRUD over `fauna.spam.{get,set}_preferences` (`fauna-client-spam`). Presentation contract — the threshold band label — is shared Rust `fauna_protocol::spam` (see § Spam threshold slider labels). **The `auto-train` and `share-model` controls are retired (ruled 2026-10-02 — `../behavior/mail-spam.md` § Implicit signals are forbidden → *A block is not a spam verdict*):** their preferences lost their last consumer with the universal spam seal, and a control that does nothing is banned by `../principles.md` § One configuration surface; they left ui.yaml and all 7 apps on 2026-10-03. |
| `add-filter-btn`, `filter-name-input`, `filter-rule-type`, `filter-rule-value`, `filter-action-select`, `create-filter` | Add email filter. | Shared Rust `encode_filter_rule` / `encode_filter_action` (see *Where logic lives*); CRUD over `fauna.email.filters.*`. |
| `filter-forward-address`, `filter-keep-local-copy` (present only while `filter-action-select` is Forward) | A Forward rule's destination and copy mode — "keep a local copy", checked by default; unchecked forwards without a local copy (`mail-forwarding.md` § Per-rule "forward to"). | The `FilterActionInputs` fields `encode_filter_action` validates and encodes (see *Where logic lives*). |
| `filter-edit` (per row, gated `filter_is_editable`) + `save-filter` | Edit an existing filter — opens the same form pre-populated via `filters_get`. | Shared Rust `describe_filter_rule` / `describe_filter_action` / `filter_is_editable` (the reverse of the create-side encoders, same dropdown-covered subset); `fauna.email.filters.{get,update}`. See `docs/goal/behavior/email-filters.md` § Email filter rules (the owner) for the CRUD surface. |
| `filter-delete` (per row) | Delete filter. | `fauna.email.filters.delete` (`fauna-client-email::EmailClient::filters_delete`). |
| `identity-export-show-qr-button` | Toggle the identity QR. | Client-local view state (no persistence, no server call). On show: read the secret from the secure store, `IdentityQr::to_uri` → `qr_matrix` (both shared Rust), draw the grid. See § Identity export. |
| `recovery-kit-create-button` | Create the recovery kit (first registration, or retrofit after a skip). | Shared Rust — `fauna_client_recovery::kit::create_kit`, then mirror the returned chain head (`RecoveryKit::chain_head`) into the signed `Profile`. See § Recovery kit. |
| `recovery-kit-replace-button` | Replace the kit using the one you hold — takes effect immediately. | Same `create_kit` entry point: it reads the chain head and picks the RecoveryKey-authorized-replacement arm from what is actually registered. The ceremony re-seals and re-puts the escrow blob in the same call. |
| `recovery-kit-lost-button` | Replace a **lost** kit using the identity seed alone — opens the 30-day window. | Shared Rust — `fauna_client_recovery::replacement::request_seed_alone_replacement`. Returns the new kit's secret **now** (`PendingKit`), because there is no second chance to display it. |
| `recovery-pending-veto-button` | Cancel a pending replacement you did not request. | Shared Rust — `fauna_client_recovery::replacement::veto_pending_replacement` (challenge-gated; needs the kit you hold). Renders only while `pending_replacement` returns a window. |
| `identity-stolen-button` | Run the succession ceremony — mint a successor identity and re-point the account. | Shared Rust — `fauna_client_recovery::succession::succeed_identity`. Destructive and irreversible: gated behind the same type-to-confirm idiom as account deletion. See § Recovery kit. |
| `credential-store-rekey-button` → modal → `credential-store-rekey-submit-button` | Change the sealed store's passphrase (re-seal under a new passphrase). | Client-local, synchronous (the tui-unlock submit precedent): `fauna-credential-store::sealed::SealedFileStore::change_passphrase` verifies the current passphrase against the file itself (the AEAD open IS the check), re-seals the same namespace map under fresh salt + current interactive Argon2id params + fresh nonce, tmp+rename. No server call — the store is per-device. Mechanics + crash-safety: [`../architecture/apps/tui.md`](../architecture/apps/tui.md) § Credential storage. |
| `sign-out-button` | Sign out. | Client-local (all 7 apps): clear local credentials + state, navigate to onboarding (`identity_choice`). Confirmed via a drivable `sign-out-confirm-button` (uniform with `admin-factory-reset-confirm-button`); per-app the confirm matches that app's own factory-reset confirm idiom (inline on windows/web/apple/tui, drivable native dialog on linux/android) — mirrors the pre-existing accepted divergence. Cross-app e2e: `tests/e2e-unified/tests/test_sign_out.py`. |
| `settings-delete-account-button` (gated by `settings-delete-confirm-field` on web/windows/tui/**apple** — types the literal string "DELETE" to enable; linux/android instead gate behind a native OS Yes/No alert. Apple adopted the shared idiom 2026-08-17, retiring its `.alert`: the alert's content is a separate presentation context, so the confirm could carry neither a drivable id nor the offline gate's environment — see [`../architecture/account-data-plane.md`](../architecture/account-data-plane.md) § Implementation status) | Delete account. | Shared Rust (transport, done) — `fauna.account.delete` via `fauna-client-account`, queues a 14-day pending action. No app navigation on confirm — and no sign-out, no credential/store erase: the user stays on the page, signed in, until the pending action later executes (ruled 2026-08-26 — § Where logic lives → *Account deletion* names the four now-fixed drifted apps and the execution-time rule). |

### Pending actions (user-approved 2026-08-13 — item 14, confirmed live with the user; tui built 2026-08-19, linux 2026-08-21, web/android/macOS/iOS 2026-08-29, windows 2026-09-07 — all seven apps built)

The three delayed verbs — handle change, account delete, snapshot delete — schedule a
**cancellable** action and say so in their replies (`pending_action_id` + `execute_after`);
today every app discards the reply, so the cancellation window the nest deliberately holds is
unreachable from any UI. The account page carries a **standing** section
(`pending-actions-section`, always present, empty when nothing is scheduled — conditional
render would hide the affordance exactly when a mis-clicker goes looking for it), listing one
`pending-action-item` row per scheduled action with `pending-action-description`,
`pending-action-execute-after`, and a one-click `pending-action-cancel-button` (**no confirm**
— cancelling is the safe direction). Two rules: **never feed the echoed new value into local
caches** (the change has *not* applied — rendering the new handle before `execute_after`
displays a handle the user does not own, or one they later cancel); and the `list` + `cancel`
wire pair is **not owed nest-side** — `fauna.pending_actions.{list,cancel}` already shipped
(the user-scoped read/manage complement to the pending-action creators, owned by
[`../architecture/core-client-kind-catalog.md`](../architecture/core-client-kind-catalog.md) § Pending Actions). The
shared-Rust client wrapper this section named as its real prerequisite **shipped 2026-08-19**:
`fauna_client_account::{pending_actions_list, pending_action_cancel}` (the `AccountClient`
that already carries the three delayed verbs; `approve` stays out — it is admin surface).
**tui led 2026-08-19** (the standing section on the Account page, three-state honest
container text — bare title un-hydrated / empty-state line / counted title — hydrated at the
Account nav edge and refreshed by both page-hosted delayed verbs and the cancel itself; the
tier_3 journey `test_pending_actions.py` schedules a handle change through the UI, cancels
it, and proves via anonymous `by_handle` the handle never changed). **linux landed 2026-08-21**
(`apps/fauna-linux/src/settings/pending_actions.rs` ports the same shape: the always-present
group, the same three-state title, and a re-list on both delayed verbs' success — never
feeding the echoed new value into any cache). **web landed 2026-08-29** (the Account sub-page's
`+page.svelte` ports the same shape over two new `libs/fauna-wasm` faces,
`pendingActionsList`/`pendingActionCancel`, plus a `describePendingAction` wrapper around the
shared row-description renderer; `pendingActionsList` filters to still-`pending` rows
client-side, same as tui's/linux's own `list_pending_actions` helper, since the wire reply
carries all statuses. Landing this leg also fixed a real pre-existing bug: web's `doChangeHandle`
was feeding the echoed new handle into the local identity cache on success — exactly the
never-cache-the-echo trap this section warns against — now removed). **android landed 2026-08-29**
(a new `PendingActionsCard` composable in `AccountSettingsScreen.kt` ports the same shape over
two new `libs/fauna-ffi` faces on `FfiAccountClient`, `pendingActionsList`/`pendingActionCancel`,
plus a `describePendingAction` free function wrapping the same shared row-description renderer;
`ApiClient.pendingActionsList` filters to still-`pending` rows client-side, same as every other
app. android's `changeHandle`/`deleteAccount` already avoided both traps this section warns
against, so no bug was found on this leg). The snapshot-delete verb schedules from the Backups
page and appears here on the next Account visit's hydrate. **macOS + iOS landed 2026-08-29**
(shared FaunaKit `PendingActionsSection.swift`, a `GroupBox` with no static label — mirroring
`SignOutSection` — so the dynamic three-state title itself is `pending-actions-section`'s
queryable id; `AccountSettingsVM` gained `pendingActions`/`loadPendingActions()`/
`cancelPendingAction()`, and `APIClient` gained `pendingActionsList()` (client-filtered to
still-`pending` rows) / `pendingActionCancel(id:)` over the existing `FfiAccountClient` surface —
no Rust/FFI changes needed, bindings were already regenerated by android's leg. Both traps
checked clean: `changeHandle` already discards the echoed reply, and account deletion already
does not sign out (fixed row 216)). **windows landed 2026-09-07** (an `INestRpcClient`/`NestRpcClient`
pair — `PendingActionsListAsync`/`PendingActionCancelAsync` — beside the existing `ChangeHandleAsync`/
`AccountDeleteAsync`, narrowed to still-`pending` rows client-side exactly like every other app;
`SettingsViewModel` gained `PendingActions`/`LoadPendingActionsCommand`/`CancelPendingActionCommand`,
refreshed on nav, after change-handle, after delete-account and after cancel; the section renders
below the Danger Zone, the last of this page's two delayed verbs. Both traps were already clean on
windows, matching the row's own prediction — no bug found on this leg). **All seven apps have now
built this section** — the pending-actions track is closed.

**A fourth row kind: an administrator's action against this account (ruled 2026-09-24).**
`fauna.pending_actions.list` returns, beside the caller's own actions, every still-pending admin
action *against* the caller (`admin.delete_user` — the class `ActionType::is_admin_action_against_user`,
matched on the hex target; `CacheDb::list_pending_actions_for_actor`). The row renders through the
same shared describer (`describe_pending_action` → "Delete the account …"), and the same one-click
`pending-action-cancel-button` cancels it — the cancel-authorization matrix admitted the target all
along; the list was the missing surface. The nest tells the target at scheduling
([`../behavior/notifications.md`](../behavior/notifications.md) § Security notices → *Pending actions*),
so the notice and the row arrive together. No app change was needed: every app filters the wire
reply to still-`pending` rows and describes them through the shared renderer, so the new row kind
paints on all seven. Journey: `test_pending_actions.py::test_an_admins_deletion_of_this_account_is_listed_and_cancellable`
(tui-first). Roster actions naming an admin are not this list's — the admin console lists those
([`../behavior/admin.md`](../behavior/admin.md) § Pending admin actions).

An opt-in toggle for browser/device push notifications (user ruling 2026-09-26;
web is built today as a status line plus an Enable/Disable button, which its
push trickle-down replaces — apple's was replaced 2026-10-03):

- Toggle on: request the platform's notification permission if needed and
  register a push subscription with the nest; on failure (permission denied,
  or the platform's push APIs unavailable) the toggle settles back off and an
  inline error message renders.
- Toggle off: unregister the push subscription. The OS permission is left alone.
- apple additionally renders a denied state when the OS permission was
  refused: a hint plus a shortcut to the platform's notification settings.

The toggle's state is the user's own opt-in for this install, **not** the
platform notification permission — the two differ exactly when it matters, since
turning it off deliberately leaves the permission granted. The bit and the rule are
owned by `../architecture/apps/common.md` § Registration. Pivoting the render on
the permission instead makes the off state unreachable on a device the OS
still allows: a successful switch-off re-renders "on", and the user is never
told their opt-out took.

web, apple (both Apple targets via APNs, macOS since 2026-08-30) and tui (the
`ws-device` transport, 2026-10-01) implement subscription —
linux/windows/android have no push call-site yet (`../architecture/core-client-kind-catalog.md` § Push Notifications); that
is the built state, not the target — the user ruled 2026-09-26 that all four
gain push, and the same day's design pass ruled each transport in
`../architecture/apps/common.md` § Push Notifications → *Transports* (the sync
agent's `ws-device` for linux, windows and tui; android UnifiedPush-first with an
embedded-FCM fallback, user-ruled 2026-09-26). The control is the same on all seven; the desktops
add one failure cause to the inline line above — no notification sink on this
machine (a headless agent). On apple the
control is one shared FaunaKit view (`PushNotificationsRows`); macOS renders it
inside the **General** sub-page, the same `SettingsPage` case iOS routes to its
`NotificationSettingsView`, so neither target carries a rail entry the other
lacks. Whether either target can actually *complete* a registration is a
separate, currently-unmet question — the `aps-environment` entitlement gate in
`../architecture/apps/common.md` § Push Notifications → *Implementation status
today*; until it is met the control renders its inline failure message, which
is the specified behavior for "the platform's push APIs unavailable" above and
the user's only witness that the gate is still shut — and a build with no
notification centre at all (the bare debug binary the macOS e2e drives) renders
the same toggle and the same line, never a control-less notice. The web control
has no `ui.yaml` element IDs today (not yet drivable by cross-app e2e); tui's
and apple's carry the family below (tui the control's first drivable leg, apple
since 2026-10-03 — its drivable journey is the gate-shut one: the toggle settles
back off with the line). The ID family, rule-A signed off by the user 2026-09-26, in `ui.yaml` since 2026-10-01 (`optional_elements` until all seven carry it; tui renders it first): `push-notifications-section` (view, always present),
`push-notifications-opt-in-toggle` (toggle; on = this install opted in — named
for the opt-in, never "enabled", because the OS permission is a different fact),
`push-notifications-error` (text, only on failure), plus apple's platform pair
`push-notifications-denied-hint` / `push-notifications-open-settings-button`.
No separate status text: the toggle's own label carries it. Outcome 9's witness
reads the persisted truth through the nest (the way every admin toggle test does).
On the desktops a missing sink is a runtime condition, not a failed enable: the
subscription still lands and the toggle stays on, while the inline line says the
agent is unreachable or this machine has no notification sink.

(The push transport/registration mechanism — `fauna.push.*` — is owned by `../architecture/apps/common.md` § Push notifications.)

### Identity export

The counterpart of the wizard's `identity_import` step ([`../behavior/onboarding.md`](../behavior/onboarding.md)
§ 1 Identity): a second device scans this QR instead of the user copying a
64-hex secret between machines by hand.

**Behavior.** The section always shows its description
(`settings.identity_export.desc`) and a toggle button. The QR and the warning
(`settings.identity_export.warning`) appear **only after the user presses show**, and
the button's label flips to `hide_qr`. Hiding is not a security control — it exists so
the secret is never on screen by accident when a user opens Settings, e.g. while sharing
a screen. Nothing is persisted and no server call is made; the toggle is view state.

**Payload and rendering** are shared Rust, both halves — see § Where logic lives. The QR
encodes exactly the URI `parse_import_input` accepts, so export → scan → import is closed
end-to-end and regression-guarded by `fauna-core`'s `qr_matrix` suite (the decode
round-trip test drives a real QR decoder over the rendered grid).

**Handle.** When the app knows the user's handle it rides in the payload, so a scanned
import pre-fills the handle step. A handle-less app writes the bare secret form.

**Risk.** The QR carries the full Ed25519 identity secret — whoever scans it gains the
identity. That is inherent to the feature (it is how a second device is added) and is why
the warning string is not optional and renders adjacent to the code, never below the fold.

**Strings.** The five strings live under `settings.identity_export.*` in
`i18n/strings/en.yaml` — keyed to the surface that consumes them. They previously sat under
`onboarding.identity_export.*`, a historical home from when only the wizard was expected to
need them; re-keyed 2026-07-11 alongside the app fan-out. The wizard's counterpart —
the *import* step that scans this QR — stays under `onboarding.identity_import.*`.

### Recovery kit

The Settings home of the RecoveryKey — the offline root that outranks the identity seed.
[`../behavior/identity-succession.md`](../behavior/identity-succession.md) owns the key, every
ceremony behind these buttons, and the threat model; **this section owns only where they render and
what view state the section carries**. § The RecoveryKey → *Creation UX* is what requires this
surface to exist: the kit is offered once during onboarding, and *"plus a Settings/Security path
that creates or re-issues the kit later"* is this.

**Placement (ratified 2026-08-01, user).** A section on the **Account sub-page**, immediately after
Identity export — not a new rail entry. The two are siblings: each reveals a root secret once, as
64-hex + QR, with a warning beside it, and neither persists anything. A separate "Security" rail
page was considered and declined — it would change the rail ratified 2026-06-03 and add a page ×
7 apps to hold six buttons that belong next to the affordance they most resemble.

**The status line.** `recovery-kit-status` renders exactly one of four states, read from the
registration chain (`fauna_client_recovery` — never a local flag, so a kit created on another
device is reflected here):

- **Registered** — a kit is registered and an escrow blob rests. The neutral state.
- **Never created** — no registration on the chain. The standing warning § The RecoveryKey →
  *Creation UX* requires after a skip; it is the same state a pre-kit account is in, so no
  "did they skip or predate the feature?" bit is needed or kept.
- **Registered, no escrow** — the chain has a kit but no escrow blob rests. A real loss-protection
  gap: recovery *by phrase* is unavailable until a re-put, even though succession still works.
  Surfaced here because the only device that can fix it is a signed-in one — which is also why the
  presence read this state needs is its own USER-class kind rather than `escrow.fetch`, whose
  RecoveryKey gate a signed-in device cannot pass ([`../behavior/identity-succession.md`](../behavior/identity-succession.md)
  § Seed escrow → *Lifecycle on the nest*). This state's copy must say both halves plainly — the
  phrase does not currently recover the account, and the repair is the kit already in hand — and the
  state renders its own repair affordance, `recovery-kit-escrow-reseal-button` (ID user-approved
  2026-08-16, asked 2026-08-10): it reads the kit-in-hand field and runs the shared head-checked re-put
  (`reseal_escrow_with_held_kit`), which restores phrase recovery **without retiring the held kit**.
  The owner of that ruling — including why the repair can never be machine-driven and never
  `create_kit` — is the § above; this section owns only that the button renders here, only in this
  state (the veto's `optional_elements` shape).
- **Replacement pending** — a seed-alone replacement is in its window. The section shows the
  countdown and `recovery-pending-veto-button`; the *loud* half is the banner below.

**The four actions** map onto the ceremonies, and which are enabled follows from the status:
create (never-created), replace (registered — you hold the kit), lost (registered — you don't), and
stolen (any). `identity-stolen-button` is irreversible and re-points the whole account, so it is
gated behind the same type-to-confirm idiom as `settings-delete-account-button` rather than a bare
click — its own `identity-stolen-confirm-field`, named off the button it guards so the pair reads as
one family (approved 2026-08-02 under rule A). The gate is a **second** condition, not a replacement
for the status one, and both are re-checked when the action fires rather than only in the render: a
disabled control emits no gesture, but a test agent driving the id reaches the handler, and an
irreversible ceremony must refuse it out loud rather than run.

**A fifth act — the let-go of a dead generation (ruled 2026-10-01, ids user-approved 2026-10-02 under rule A).** [`../architecture/account-data-taxonomy.md`](../architecture/account-data-taxonomy.md) § The generation machinery → *Fleet-scope reclamation*, clause (3)(j), owns the act, what it lists and why only the user may take it. This section owns its placement: beside the four actions, three elements rendered only while the account runtime's dead read answers non-empty — the veto's `optional_elements` shape. `recovery-kit-unreadable-status` states how many items the dead generations hold and since when, with the copy selected by the shared projection (`generation_let_go::unreadable_status`); it says the one thing the client cannot know — a device not signed in since then may still read them, so sign in there first. `recovery-kit-let-go-confirm-field` is the stolen gate's type-to-confirm idiom (the shared `LET_GO_CONFIRM_WORD`), and `recovery-kit-let-go-button` arms on it alone. The confirm word is re-checked when the act fires, and the runtime re-reads each generation dead before it retires anything.

**Kit-in-hand entry (user-approved 2026-08-01).** The four ceremonies that take a kit the user
holds — replace, stolen, veto, and the no-escrow re-seal — read it from
`recovery-entry-phrase-field`, rendered inline in the section: the onboarding `recovery_entry`
screen's own input, reused by the same same-artifact argument as the display trio below (a pasted
phrase is the same artifact as a displayed one; priority #3, and the shared `parse_kit` grammar
behind it is identical). One ID appears on two pages by design; no settings-scoped twin exists.

**Displaying a returned kit.** Any ceremony that mints a kit returns the secret **once**; the
section renders it inline through `recovery-kit-secret-display` / `recovery-kit-secret-copy-btn` /
`recovery-kit-qr` — the onboarding screen's IDs, because it is the same artifact. There is no
"show my recovery kit again" affordance and there can never be one: the secret is offline-only
(§ The RecoveryKey — *Custody*), so nothing on the device holds it after the screen closes. That is
also why a failed escrow `put` must **not** be rendered as a plain error — the registration has
already landed, and the returned secret is the only copy in existence.

**The ceremony's outcome is headlined by its arm, never by whether the call returned (ruled
2026-09-26).** `identity-stolen-button` ends in one of four arms, owned
and typed by [`../behavior/identity-succession.md`](../behavior/identity-succession.md)
§ Implementation status today (the *typed outcome* ruling): landed, nothing moved, landed for
another, undecided. Only *nothing moved* is a failure, and only it may read as one. Every app
paints the shared `StolenOutcome::message()` — an `i18n/strings/en.yaml` key the app resolves
through its own pipeline — **verbatim on `error-message`, wrapping nothing**: the sentence already
carries its own headline (`stolen_ceremony_failed` on nothing-moved; `stolen_outcome_unknown_saved` /
`_unsaved` on undecided; `stolen_landed_for_another`), so a per-app wrapper is a second headline,
and on two of the four arms a false one. The landed arm renders nothing here (the switch, or the
persist-failure message below). **The undecided arm's *unsaved* half carries the seed and is parked
exactly as the persist-failure message is** — it is the same only-copy situation; the outcome
says which arm that is (`StolenOutcome::carries_the_only_seed`, carried on the FFI and wasm
records), so no app reads the key to find out. **Per-app audit (2026-10-03):** ✅ tui, linux,
web, macOS and iOS match the outcome, paint `message()` verbatim and park the unsaved arm. windows receives
the typed record through an interim bridge in its client wrapper that surfaces every non-landed
arm as the resolved sentence, unwrapped — but through its ordinary error path, so the unsaved arm
is **not yet parked** there. android has no ceremony.

**The persist-failure message survives the page, not just the initial render (ratified
2026-09-14, all apps).** A stolen-identity succession's persist-failure message is the sharpest
instance of the *Displaying a returned kit* paragraph above: the ceremony has already re-pointed the account on the nest, the
local persist read-back failed, and the message is the ONLY surviving copy of the account's new
key — nothing on the nest and nothing in the retired identity's local state holds a second one
([`../behavior/identity-succession.md`](../behavior/identity-succession.md) § The RecoveryKey →
*At succession*). It renders on the same shared `error-message` element every other control on the
Account page writes to (rule A — one per page), so **every writer of that element — every button
click handler and every async repaint hook, not only the ones that show validation errors — must
leave a pending persist-failure message alone**: it wins over any other content on the label until
the user has had the explicit chance to act on it. Left unguarded, the ordinary next event on that
same page — a Change-handle click, another ceremony's status poll landing, a kit minted from a
different action — silently overwrites or hides the only copy of the key, which is the ordinary
path, not a race: the ceremony leaves the user parked on the very page whose shared label every one
of those writes clobbers. The widest such writer is the app's own reaction to being superseded: the
ceremony supersedes the identity the running session holds, so that session's next auth refresh is
refused the instant the nest commits — before the ceremony's own result is handled — and the
ordinary mid-session escalation to the launch surface would tear the page, and the ceremony's
result, down with it. **A supersession this device's own ceremony caused is therefore held back**
while the ceremony is running or its persist-failure message is pending, and performed once the
user leaves Account (or at once, when the ceremony ends while the user is elsewhere and no
persist-failure message is parked). Per app (audited 2026-09-26): **tui** implements it
(`App::defer_own_supersession`) on both channels the refusal arrives by — the session's own re-auth verdict and the live launch machine's `superseded` phase (`launch::route`), the second of which alone still routed a lost-reply ceremony to the import screen (measured 2026-09-26 by `test_identity_succession_ceremony.py::test_a_lost_reply_you_cannot_check_says_so_and_reopening_signs_you_in`) — and past the fold too: the refusal has no fixed order against the ceremony's result (the launch machine's was measured landing ~30 ms after an undecidable fold), so a ceremony that adopted nothing while the user was on Account keeps owning its supersession until they leave it (`App::stolen_outcome_on_screen`); **macOS** and **iOS** implement it
(2026-09-26, one FaunaKit `StolenCeremonyHold`, past the fold alike, which also drops the Account
page's other error writers while the message is pending); **windows** implements it (2026-09-27,
a port of the same hold in `FaunaApp.Core`, fed by the connection supervisor's stop — the
`session_ending_verdict` read on `Disconnected` — and by the TTL loop's refused refresh, and gating
the Account page's one error funnel; the leave edge counts both the navigation and the visual-tree
unload, since an outer Settings navigation unloads the inner-frame page with no navigated-from);
**linux** and **web** escalate
unconditionally and so drop the ceremony's result (linux shuts the client runtime down under the
ceremony task; web navigates the document away);
**android** has no stolen-identity ceremony yet. The
outcome-17 journey
(`test_identity_succession_ceremony.py::test_a_key_this_device_cannot_store_stays_on_screen_until_you_leave`)
asserts the whole rule — the key survives another writer, leaving Account discharges it, and the
held-back escalation then lands the import route — and is green on tui, macOS, iOS and windows.

This build defines the acknowledgment act as **leaving the Account sub-page** — no new element: a
dedicated dismiss/copy affordance was considered and declined, because it would need its own
`ui.yaml` ID and user approval this slice does not have, and the existing
`recovery-kit-secret-display` cannot be reused either (it is cleared on every navigation by design,
per *Displaying a returned kit* above, and it holds a different kind of secret — a freshly minted
kit, not a persist failure). The message therefore stays pinned through every unrelated write while
the page is open, and is discarded only on the nav edge away from `account`, by which point the
user has had the whole visit to read or copy it.

Every app renders this same message on this same shape of shared, many-writer error/status slot —
tui's page-keyed `app.errors`, web's `recoveryError`, apple's `errorText`, windows' `SetError` —
and each has the identical structural gap; none of them is a model to copy uncritically; each
carries its own parity row rather than inheriting this ruling automatically. **Implemented on
linux** (`apps/fauna-linux/src/settings/mod.rs`'s `render_account_error_label` guard +
`acknowledge_stolen_failed_message`, wired from the settings shell's nav-edge-away-from-`account`
hook in `apps/fauna-linux/src/views/settings_shell.rs`; ) **and apple**
(macOS + iOS, shared `FaunaKit`) — `RecoveryKitVM.swift`'s `setErrorText` guard +
`stolenFailedMessagePending` flag, discharged by `acknowledgeStolenFailedMessage()` (folded into
`clearHeldSecrets()`, so all three of its call sites — `RecoveryKitSection`'s `.onDisappear`, the
off-screen branch of its `hydrate()`, and `resetForIdentityChange()` — discharge alike; ) **and web** — `$lib/recovery-error-guard.ts`'s `RecoveryErrorGuard` (a pure `write`/`park`/
`discharge` decision object, unit-tested under `just web-unit-test`, since `+page.svelte` has no
component test runner), which every `recoveryError` writer routes through; discharged by a
DEDICATED effect watching the nav edge away from `account` (never a cleanup returned from the
entry effect, which also reads `$identity` and would otherwise discharge on every unrelated
identity-store update), and again — this time also clearing the slot, matching apple's ordering —
by a second effect keyed on `actorId` for a same-tab multi-account switch; )
**and tui** — `PageErrors::park_stolen_failed_message`/`acknowledge_stolen_failed_message`
(`apps/fauna-tui/src/app.rs`), a wrapper around the page-keyed `app.errors` map itself rather than a
guard on one render call, since no single door covers every way `(page, sub)` can leave Account here
(`Action::NavBack`, Esc, the rail `Open*` handlers, `route_subpage`, `set_page`, and a few direct
field writes elsewhere all change it independently); discharged by
`App::sync_recovery_message_nav_edge`, called once at the end of `App::handle_key` and once at the
end of `gesture_work` — "the one gesture door" every mouse/keyboard/agent actuation already passes
through — plus once per frame in the render loop as a backstop for the remaining direct writes
) **and windows** — `RecoveryKitViewModel.cs`'s `SetGuardedError` guard +
`StolenPersistFailurePending` flag, discharged by `AcknowledgeStolenPersistFailure()` (folded into
`ClearHeldSecrets()`, mirroring apple's pairing). Windows' extra wrinkle: TWO view models
(`RecoveryKitViewModel` and `SettingsViewModel`) feed the ONE shared `ErrorBar` through the page's
`RenderError` funnel (`SettingsAccountPage.xaml.cs`), so the guard is enforced a second time there —
the only point that also sees `SettingsViewModel`'s direct `ErrorMessage =` writes and the page's
own error calls, neither of which goes through any view model's guarded setter; ).

**The post-succession aftermath's progress lines.** `succession-aftermath.md` § Re-key scope requires
the successor's aftermath to be "started at first successor sign-in, **surfaced with progress**,
resumed until complete", and this section is where that progress renders — one optional line per leg
of the sequence, in the order the legs run: `recovery-kit-backup-regrant-status` (leg 2, the `NestBackupKey` re-grant and
the destination-registry rebuild), `recovery-kit-mls-reseal-status` (leg 3, the `__mls`
state-replica re-seal — the conversations plane), `recovery-kit-grant-remint-status` (leg 4,
the capability-grant re-mint), `recovery-kit-corpus-reseal-status` (leg 5, the user's own
**file corpus** — files, photos and folders), `recovery-kit-drafts-reseal-status` (leg 7, the
`__drafts` re-seal — the user's unsent drafts), then `recovery-kit-mail-burn-status` (leg 6, the
MSEK burn). **Leg 7 renders above leg 6**, which is the order the sequence runs them in and for the
sequence's own reason: the burn is the only leg that *takes something away*, so every restoring leg
reports first. Leg 1, the `__config` re-seal, retired with the blob rail (`config-dissolution.md` § The `__config` dissolution schedule → *The closure order*, step (6)), and its line with it. All six are **optional** and render nothing for
an ordinary account, which never runs the passes at all; leg 2 is additionally silent for an owner
with no backup destinations, leg 3 for an identity with no replica at rest, leg 4 for an owner whose
grant ledger is empty, leg 5 for a settled pass that moved nothing and owes nothing, leg 7 for a
predecessor who composed nothing and for the idempotent pass every later sign-in re-runs, leg 6 for
a successor who owed no burn. What each line says is
decided by a **shared projection** — `ConfigResealProgress` / `BackupRegrantProgress` in
`fauna-client-config`, `ReplicaResealProgress` in `fauna-client-mls-sync`, `GrantRemintProgress` in
`fauna-client-capabilities`, `CorpusResealProgress` in `fauna-client-sync`,
`DraftsResealProgress` in `fauna-client-drafts`, `MailBurnProgress` in
`fauna-client-mail-settings` (each projection lives
beside the outcome it projects, which is also what keeps openMLS out of the wasm-clean config
crate) — never by per-app `match`es, so the arm that is easiest to get wrong reads identically on
all seven apps: *this device cannot open it, another one will finish* is a **wait**, never damaged
data or a broken backup. **And the six projections are one shape** (lifted 2026-08-25/26):
each is an alias of `fauna_core::progress::Passage<O>` over its own `ProgressOutcome`, so the
`Running` / `Failed(reason)` lines and the resume rule — *in flight or failed means still owed* —
exist once, below every leg crate, and a leg that grows a new pass cannot forget the `Failed` arm
because it no longer writes one; only the settled outcome's copy and its own still-owed predicate
are a leg's to write.

Leg 3 carries a **fifth** state the first two lack, and it is the one worth not flattening:
*partly-owed* — a pass that unlocked some conversations while others stay sealed to an identity
this device has no key for (a twice-succeeded chain). It is progress **and** unfinished at once, so
it renders its own line rather than the done one, which would tell a user every conversation is
back while some are still dark. **Legs 5 and 7 carry it too** — both are multi-unit, so on them
partly-owed is an ordinary reading rather than an edge case. Owner of the leg semantics is
[`../behavior/succession-aftermath.md`](../behavior/succession-aftermath.md) § Re-key scope; this
section owns only where they render. The contact re-point adds no line (ruled nest-side-complete
2026-08-16); any future leg adds its own here as it lands.

Legs 6 and 7 are the two that arrived after this list was first written, and each carries a copy
rule the shared projection enforces rather than each app. **Leg 6 is the one whose *done* state is
not good news:** completing the burn breaks every configured mail app on purpose, so its done line
must say the apps need setting up again and why, or the user reads the resulting MUA failures as a
malfunction and re-enters the exact credential that must never work again; and its failed arm
must say plainly that until the burn lands, whoever held the previous identity can still read new
mail — the one aftermath leg whose incompleteness is a live exposure rather than a delay
([`../behavior/mail-credentials.md`](../behavior/mail-credentials.md) § Rotation and recovery →
*Succession* owns the mechanics). **Leg 7's copy must never read as loss:** until the pass runs the
drafts are stuck, not gone — sealed under a key this device may simply not hold — and a user told
they are unrecoverable stops looking for the device that can open them.

Leg 4's line has no owed-elsewhere arm: the grant ledger it
re-mints from is the account store's rows, read in the post-store-ready pass, and nothing in
`__config` (`succession-aftermath.md` § Re-key scope). Its **adjudication** half does not render here at all — the
per-grant review mark and Keep live on the trust rows that show the grants themselves
([`nests.md`](nests.md) § Trust facet, `nest-trust-grant-unattested-mark` +
`nest-trust-grant-keep-button`), exactly as the backup plane's mark lives on the destination rows
rather than in this section. This line reports only whether the pass ran.

Leg 5 is the only line whose work does **not** run in the app, and that shapes both where it comes
from and how it behaves. On desktop the corpus re-seal runs inside the per-user `fauna-sync-agent`
(`../architecture/apps/sync-agent.md` § Control plane split), so the app folds the line from that
agent's `ListEngines` roster on its existing status poll rather than from a pass it ran itself — a
app that re-ran the pass to render a status line would race the very work it is reporting on.
Consequences worth stating, because they are visible: the line **moves while the page is open**
(every other leg's settles once per sign-in), and it is legitimately **absent for a while after
sign-in** — the agent simply has not recorded a pass yet, which is not the same as "nothing owed"
and must not be rendered as one. It carries leg 3's *partly-owed* state for leg 3's reason, and is
alone in carrying **numbers** with it (`{done}` moved / `{remaining}` to go): this is the one leg
whose corpus can be large enough that an unqualified "still working" tells the user nothing, which
is what § Re-key scope's word *progress* asks for. Its copy must never say "unlocking" or imply the
files are unreachable — a successor can already open all of them (the read fallback landed before
the write half), and what the pass cuts is the last dependency on the retired identity.

**The sweep's own lines (ratified 2026-08-27).**
The ceremony's outcome renders **in the flow that ran it**, at the top of the section, as two
lines on two ids: `recovery-kit-sweep-status` — what the sweep did (it never ran / it could not
start / the retired identity was removed from all *N* groups / from *M* of *N*) — and, its own line,
never a qualifier on the first, `recovery-kit-sweep-unvouched-status` — the roster it cannot vouch
for. Each id is **absent whenever the projection returns no line for it** (a groupless account, an
empty roster), never present and empty. The ids were user-approved 2026-09-25 so the outcome can be
witnessed on screen; they supersede the ID-less form ratified 2026-08-27, when the e2e state key
`succession_sweep` was the only witness (it stays, as the lines' machine twin). **Which arm says what is decided by the shared projection,
`SweepView::copy` in `fauna-client-recovery`**, under the same rule as the aftermath progress lines
above: an app resolves `LocalizedText`, it never selects. Three judgments live there and nowhere
else. **(1)** A succession on an account with **no groups at all says nothing** — *"removed from all
0 of your groups"* is reassurance by vacuity, the same one the retry's answers refuse below. **(2)**
The two facts stay two lines, with no combined *you are safe* verdict to render (the refusal
`SweepReport` itself makes). **(3)** The two degraded arms name `recovery-kit-sweep-retry-button`
by its label **only where the app renders it**: the projection is told (`SweepRetryAffordance`),
and an app that has not built the button gets the same fact with the member-side remedy alone,
because a degraded line must never name a control that is not on screen (the next paragraph's
rule, applied to the seven-app roll-out rather than to a second device). `Absent` is a parity gap,
never a product choice — tui, web, macOS and iOS have the button today (§ Implementation status
today); the flag and the two `*_no_retry` strings retire when the seventh app builds it
. The button's own render gate is the
same projection's `owes_work`, so the copy naming the button and the button's presence cannot
disagree. An app carries the **view** (`SweepView`; `FfiSweepView` over UniFFI) across the account
switch — the ceremony runs before it, the lines render after — and selects at paint time
(`sweep_copy` on the FFI; web parks the view per successor in `sessionStorage` beside its
review-pass witness).

**Finishing an unfinished group sweep.** The succession ceremony re-points the user's MLS groups
as its last act, and it can fail to: conversations may not have been running when the ceremony ran,
the pass may have died on a network flake part-way through, or it may have failed outright. All
three leave the retired identity still seated in groups it can read. `recovery-kit-sweep-retry-button`
is the affordance that finishes the job, driving the shared
`fauna_client_recovery::retry_group_sweep`; the semantics — what the sweep does, why the retry is
possible at all, and what it refuses — are owned by
[`../behavior/succession-propagation.md`](../behavior/succession-propagation.md) § Propagation →
*MLS groups*, and this section owns only where it renders.

**It renders on unfinished work alone, and deliberately not on whether this device can run it.**
The retry needs the retired identity's own conversation store, which survives on the device the
ceremony ran on and nowhere else — so on a second device the button is present and pressing it
answers with the member-side remedy, naming the device that can finish it. The alternative, hiding
the button where the retry cannot run, would leave the sweep's own degraded copy naming a control
that is not on screen, which is the failure `succession-aftermath.md`'s narrowing removed
from that copy in the first place. It follows that this button, uniquely among the section's
actions, **must answer in words on every press** — a device that cannot retry has to say so.

**Its three non-sweeping answers each name what is left to do**, and none is rendered as a sweep
result: this device holds no state from the previous identity, no move of this account was ever
recorded, or the account was moved to a different identity than the one signed in here. Reporting
any of them as an empty sweep report would render as *"removed from all 0 of your groups"* — the
reassurance-by-vacuity the sweep's own copy refuses everywhere else. The four sentences — those
three plus the transport arm's — are selected by the same shared projection as the lines above
(`SweepRetryAnswer::message` in `fauna-client-recovery`), for the same reason: an app resolves, it
never words.

**The no-old-state answer names another device CONDITIONALLY, never the ceremony device by
name** (ratified 2026-08-27). It used to read *"Sign in on the device
you used to take your account back and press Finish Moving Your Groups there"*, which is a
contradiction on the device that is reading it — and that device is a routine reader of this arm,
not an exotic one: the commonest reason the retry is owed at all is that conversations were down
when the ceremony ran (the `no-engine` arm), so the ceremony device holds no history either, and on
**web** no device ever can (the retired identity's replica rests behind bearers the succession
revoked, § *The sweep's own lines*' web note). So the sentence offers the other device as a
possibility the user checks — *"if another of your devices still has those conversations"* — and
keeps the member-side remedy as the answer when there is none. This is a **condition**, not a
platform: web and native say the same words, and no app owns a variant of this string. ⚠ The
aftermath's own *owed-elsewhere* lines (§ *The post-succession aftermath's progress lines*) are
**not** this case and keep their direct phrasing — they turn on the retired identity's `BackupKey`,
which the ceremony device demonstrably held.

**The ephemeral member-review pass.** `succession-propagation.md` § Propagation rules two surfaces for
the unattested-member review, and the ephemeral one renders **here** — *"shown once per recovery, in
the ceremony flow that produced it"* — directly beneath the sweep chrome that reports the roster it
works through, which is why that line states its fact and no longer redirects anywhere. One
`member-review-row` per **person** (never per item — the owner's decision is singular, and the
collapse is the shared `open_member_reviews` projection), with `member-review-keep-button` and
`member-review-remove-button` scoped **inside** the row, and one `member-review-defer-button` below
them all. All four are **optional**: they render only while a succession sweep ran this app run, open
items remain, and the user has not deferred — outside that window the backlog belongs to the
permanent review view, and painting it here would make this section standing clutter on every
ordinary session. *Defer* hides the pass and **decides nothing**; that is precisely what leaves the
permanent view something to inherit. This section owns only where they render: what a verdict means,
and the rule that *Remove*'s verdict is **derived** from the eviction rather than chosen, is owned by
[`../behavior/succession-propagation.md`](../behavior/succession-propagation.md) § Propagation. The same
four IDs serve the permanent view — one family, two surfaces, by the same same-artifact argument
that reuses `recovery-entry-phrase-field` here. All four IDs — and the permanent `member_review`
page itself — were **user-approved 2026-08-16** (asked 2026-08-10).

**Where the deferred backlog goes: the `member_review` rail sub-page** (landed on tui, linux,
android and web, all 2026-08-17; apple and windows pending; rail slot ratified in § Navigation
model above, page def in `ui.yaml`). It renders three of those
same four IDs — `member-review-row` with its `member-review-keep-button` / `member-review-remove-button`
pair scoped inside it — plus `member-review-empty`, and deliberately **no**
`member-review-defer-button`: deferring is what sent the backlog there, so a second postponement
would defer the page to itself. The two surfaces differ in their **gate, not their markup**, which is
why the row is built once and called from both — the ephemeral pass gates on a ceremony having run
this app run, the permanent page on nothing but whether anything is still open. ⚠
**`member-review-empty` is not a safety verdict**:
[`../behavior/succession-propagation.md`](../behavior/succession-propagation.md) § Implementation
status today leaves deliberately no combined *"is the user safe"* boolean for a surface to round a
count of zero up to, and an empty review list is not one either.

**The pending-replacement banner is not here.** § The RecoveryKey → *Replacement* requires the
30-day window to be loud "on every device and app surface for the whole window," which a
Settings element cannot be — a user who never opens Settings never sees it. It is therefore a
**critical-alerts feeder**, rendered by the existing every-page `critical-alerts` banner on every
authenticated page ([`../behavior/critical-alerts.md`](../behavior/critical-alerts.md) § Feeders).
This section carries only the veto *action*, since a `critical-alert` row is text.

**Where logic lives.** All the ceremonies are shared Rust already — `libs/fauna-client-recovery`,
transport-generic over `RpcRequester` (native and wasm consume one implementation). Each button is
a render plus one call; no app re-derives chain arms, refusal classification, or the countdown.
**The status line and the enablement rule above are shared too** — `fauna_client_recovery::status`
projects the four states from the chain and answers which actions each permits, so the "which
buttons are live in which state" matrix is decided once rather than seven times, and the line itself
is `LocalizedText` (an app renders it, never composes it).
The one step the shared core deliberately leaves to its caller is mirroring the returned
chain head (`RecoveryKit::chain_head` — always both halves, never the pubkey alone)
into the signed `Profile` via `build_profile_with_recovery_head`, which belongs to the profile
plane.

### Encryption page

The rail's **Encryption** entry (§ Navigation model) shows the supply of one-use starter keys the account keeps on its nest, a read-only "low" flag, and a manual refresh. What it shows and what the refresh does are owned by [`../behavior/direct-messages.md`](../behavior/direct-messages.md) § Key Package Management (the replenish model); this section only places the page. **Status (stated 2026-10-01):** the page is built on all seven apps — windows shows the count alone, a parity debt ([`../behavior/devices.md`](../behavior/devices.md) § Implementation status today) — and **it has no `ui.yaml` page and no element ids**, so no journey can read it and every `[app]` outcome of the catalog's `encryption-settings` page is unwitnessed; the id package is the user's to approve.

### Credential store

The shared "credential store settings" concept (user-approved 2026-08-05; ID family 2026-08-06):
a section stating how credentials are held **on this device**, plus whatever per-device custody
actions the active backend admits. This page owns only the render surface — the section, the
status line, and the change-passphrase modal (element inventory in § Element IDs above); **the
sealed-arm mechanics are owned by [`../architecture/apps/tui.md`](../architecture/apps/tui.md)
§ Credential storage** (what the re-key does, its crash-safety argument, why the seed nudge is
mandatory: the sealed file has no verifier or escrow, so a forgotten new passphrase is
unrecoverable except through the identity seed — which is why the nudge points at Identity
export, two sections up on this same page).

Render rule: the section paints only when the active backend has per-device settings to offer.
Today that is exactly one case — tui running the passphrase-sealed headless arm (the web app's
sealed identity store, ruled and unbuilt, becomes the second when built and adopts this section
and its ids unchanged — [`../architecture/apps/common.md`](../architecture/apps/common.md)
§ Credential storage → *The web app's posture*). An OS-store
backend (Secret Service / Keychain / Credential Manager) renders nothing here: its custody is
OS-managed, and its one existing app-side knob (`settings-icloud-backup-toggle`, macos/ios) is a
platform element that predates this section; folding it in is a later per-app decision, not a
requirement of this slice. The modal's inputs are masked per the tui-unlock contract, validation
errors ride the page's `error-message` with the modal kept open, and success disarms the modal
and paints `credential-store-rekey-success`.

### Data export

`Export My Data` button — issues `GET /api/v1/export` and downloads the
response as a zip archive of the user's nest-stored data (`bins/fauna-nest/src/export_routes.rs:1`).
**One button, no options:** the archive always carries the user's payload bytes,
and whether it does is not a choice the UI offers — the ruling, with its four
reasons, is [`account-data-plane.md`](../architecture/account-data-plane.md)
§ Nest-side requirements item 1, *Payload stores* decision (5). Apps fetch
`fauna_nest_http::paths::account::EXPORT_FULL` rather than composing the URL.
The server's own `Content-Disposition` suggests `fauna-export-<YYYYMMDD-HHMMSS>.zip`
(`export_routes.rs:85-91`); web overrides this with its own `fauna-export-YYYY-MM-DD.zip`
name at download time (`apps/fauna-web/src/routes/settings/[[subpage]]/+page.svelte:1060`),
so the exact filename convention is a app-glue detail, not a wire contract.

**The `settings-export-data-button` ui.yaml element ID was minted 2026-08-19**
(rule-A approved) — before that
no ID existed on any app (unlike the identity-export family), so the button was
undrivable by cross-app e2e, the same gap noted for Push notifications above.
That absence is why decision (5)'s uniformity guard is a tier_1 source assertion
(`test_account_export_carries_payload_bytes.py`) rather than a journey test: the
defect it catches — an index-only archive — is a well-formed zip that downloads
and opens fine, so the requested URL is the only observable that separates right
from wrong; the journey test now exists — `test_export_my_data_journey.py` (tier_3,
2026-08-19) drives the button through the real UI and asserts a well-formed
archive (`export/manifest.json`) arrives: web captures the real browser
download, native apps save dialog-less as `fauna-export.zip` into the shared
`FAUNA_E2E_DOWNLOAD_DIR` seam (linux bypasses its save dialog under e2e for
exactly this, mirroring backups' `file_list.rs`). The URL-level
`include_blobs` guarantee stays with the tier_1 test. **Implemented on linux**
(`apps/fauna-linux/src/settings/account.rs:465-515`,
via `FaunaClient::export_account_data`, `apps/fauna-linux/src/client.rs:829`), **web**
(`+page.svelte`'s `exportData()`), **android** (`AccountSettingsScreen.kt:298-346`),
**apple** (`AccountSettingsView.swift:200-210` for the button,
`exportAndPresent()` at `:366+` for the export — shared FaunaKit, both macOS
and iOS; under e2e it bypasses the native save panel/share sheet and writes
`fauna-export.zip` straight into the shared `FAUNA_E2E_DOWNLOAD_DIR` seam via
`SnapshotFileSaver`, mirroring linux's bypass and apple's own backups
download flow),
**tui** (`apps/fauna-tui/src/settings/account.rs`, `settings-export-data-button`
→ `Action::ExportData` → `fauna_nest_http::ReqwestNestContentApi::get(EXPORT_FULL)`,
written to the downloads dir — tui has no save dialog to raise, `backups.rs`'s
`download_dir()` shared), and **windows** (`INestHttpClient.ExportAccountDataAsync`
— `DirectNestClient.cs:461`, the literal `EXPORT_FULL` path, no UniFFI door —
and `SettingsViewModel.ExportAccountDataCommand`, saving through the same
`ISnapshotFileSaver` seam single-file restore uses; the Data section sits
between Change Handle and Sign out, mirroring linux's Data group placement);
all seven carry the full-archive flag. The element ID is now stamped on all
seven apps (tui/linux/web/android/apple 2026-08-19/25, windows 2026-08-27,
the last of the seven).

### Spam threshold slider labels

The spam threshold slider's numeric label changes by range:

- `0.0–0.3` — "Aggressive"
- `0.4–0.7` — "Moderate"
- `0.8–1.0` — "Permissive"

Step is 0.1; the same range/label mapping applies on every app.

**Where logic lives.** The band mapping is **shared Rust** — `fauna_protocol::spam::spam_threshold_band(per_mille) -> SpamThresholdBand` (the wire carries per-mille `u16`; `0.1` probability = `SPAM_THRESHOLD_STEP_PER_MILLE` = 100). Its three bands map to the `spam:` i18n keys `aggressive`/`moderate`/`permissive` via `SpamThresholdBand::key()`, so each app localizes the same band rather than re-deriving the buckets inline (the spam analog of the email-filter `encode_filter_rule` precedent above; priority #2/#4). The two apps that localize **in Rust** (linux and tui) skip the key round-trip and take the label straight from shared Rust — `SpamThresholdBand::label()`, or `spam_band_label(probability)` for the whole slider-value → label path; web goes via the wasm `spamThresholdBand(threshold: f64) -> key` (`fauna-wasm`); Apple / Windows / Android via the UniFFI `spam_threshold_band(threshold_per_mille) -> key` (`fauna-ffi`). ⚠ Until 2026-08-22 this sentence said "Rust-native Linux calls it directly" while linux and tui each hand-rolled their *own* band → label match beside their slider — the buckets were shared, the label map was not. (The former `share-model` control's option contract — `fauna_protocol::spam::SHARE_MODEL_VALUES` with its UniFFI `share_model_values()`, wasm and Go faces — is retired with the control: § Spam moderation's spam-prefs row.)

The **slider↔wire conversion** is likewise **shared Rust** — `fauna_protocol::spam::probability_to_per_mille(f64) -> u16` (clamp `[0.0, 1.0]`, scale ×1000, round **half away from zero**) and its inverse `per_mille_to_probability(u16) -> f64` (over-range saturates at `1.0`). The dag-cbor wire forbids floats, so thresholds ride as per-mille `u16` while every app's slider works in probability; this is the single definition the nest, the wasm web path, and the native apps all call instead of each hand-rolling `* 1000` / `/ 1000`, so **every app rounds identically** (closing the divergence inline copies risked — C#'s `Math.Round` is banker's-rounding, half-to-even). Native apps ride the UniFFI faces in `fauna-ffi`'s default-on `value-format` module.

### Muted words — the level picker (ratified 2026-10-02)

Each `muted-word-item` row carries, beside its term and its remove button, a **level picker** — a two-option select, **Hide** / **Show less** — that sets how hard the term mutes. What the two levels do and the weights behind them are owned by [`../architecture/content-moderation-and-ranking.md`](../architecture/content-moderation-and-ranking.md) § Composition; this section owns only the page's shape:

- **One gesture, saved at once.** Changing the picker is a save, like the remove button — no apply step, no per-row edit mode. The row repaints from the stored record the save returns, its level read through the shared classification (never a number compared in the app), and the picker is disabled while any round trip is in flight, as the add and remove buttons are.
- **A new term is Hide.** The add gesture takes no level; the picker is where a term is softened afterwards.
- **A soft row shows nothing but its picker's value.** No badge, no second line, no different styling: the picker reading *Show less* is the whole difference between a soft row and a hidden one.
- **The page explains the two levels once, in its description** (the `muted-words` landmark's copy): a hidden word collapses matching posts and messages behind the reveal and sinks matching posts in ranked feeds; *Show less* only sinks matching posts in ranked feeds, and messages show as usual. **Nothing in the conversation view explains a soft term** — a soft match changes nothing there, so there is nothing to show; the explanation lives where the choice is made.
- **Element.** `muted-word-level-select` (`select`, indexed, scoped within `muted-word-item`; option values `hide` | `show-less`, the level's own spelling) is the one new id — pending rule-A approval (§ Implementation status today).

**Where logic lives.** Shared Rust end to end: the levels and their weights (`fauna_core::scoring::MutedKeywordLevel`), the delta (`fauna_account_plane::preference_surfaces::set_muted_word_level`), the faces (UniFFI `set_muted_word_level` + `muted_keyword_level`; wasm `setMutedWordLevel` + `mutedKeywordLevel`). An app paints the picker from the row's level and sends the picked level back; it never sees a weight.

## Implementation status today

**Muted words — the level picker: UX ratified 2026-10-02 (§ Muted words — the level picker); the shared write path is built; the control is unbuilt on all seven apps and its one `ui.yaml` id, `muted-word-level-select`, awaits rule-A approval. tui leads, the other six follow in trickle-down.**

**Inbox-mode "show the stored mode, never a default" (§ Privacy sub-page item 7) — built + verified on tui, linux, web, macos, ios, windows.** tui is the reference (`PrivacyState::inbox_mode: Option<String>`; `settings/mod.rs::page_error` emits `INBOX_MODE_UNKNOWN` while it is `None`). **linux landed 2026-08-05** — before that it hard-defaulted its radio group to `open` and *logged and dropped* the `inbox_mode_get` reply, so Settings → Privacy reported "open" for every account whatever the nest held (confirmed defect); the fix stores the mode as `Option`, repaints through `settings::apply_loaded_inbox_mode`, and suppresses the write-back echo the repaint would otherwise cause. Pinned by tier_1 `settings::inbox_mode_tests` (both halves mutation-graded) + the cross-app e2e `test_inbox_mode_pre_selects_the_accounts_real_mode_after_a_relaunch`, which is the only assertion that survives a process boundary — the older `test_inbox_mode_toggle` reads the mode back in the process that just set it and so passes on an app that paints a default and caches the click. **tui verified GREEN against the relaunch test 2026-08-05** — it was already correct, so this only pins it. **macos / ios verified GREEN 2026-08-26** — already correct, no app fix needed. **windows verified GREEN 2026-08-26** after a real defect: `SettingsViewModel.InboxMode` seeded `"open"` (a chosen mode, not "unknown") and the state-protocol mirror `App.TestInboxMode` was written ONLY by the button click handler, never by `LoadAsync`'s own fetch — so a relaunch's re-fetched real mode never reached the mirror the e2e reads, which kept reporting its stale "open" seed. Fixed the same shape as linux's original three rules: both seeds are now `""` (unknown), and `SettingsPrivacyPage`'s `PropertyChanged` handler is the single writer of the mirror, sourced from `InboxMode` itself (whichever path set it — load-fetch or a confirmed click) rather than the click handler writing it directly and optimistically. **web VERIFIED GREEN 2026-08-06 against the relaunch test — but that test could not see this app's actual defect, and web was NOT already correct** (corrected 2026-08-27). `routes/settings/[[subpage]]/+page.svelte` opened `inboxMode` at `'allow_knock'`, so the radio was marked — and the web test agent reads the *checked* radio (`web-bridge/agent.js`), so the state protocol reported that guess as the account's mode. The relaunch test passes on it because the guess is replaced a round trip later and the test only asserts convergence; the same blind spot hid the identical defect on apple until 2026-08-27. **Fixed 2026-08-27**: `inboxMode` is `string | null`, `null` until `getInboxMode` answers and back to `null` if it fails, and the page carries `settings.privacy_page.inbox_mode_unknown` on its `error-message` while it is — tui's ratified shape, now on five apps in three languages (`Option<String>` / `String?` / `string | null`). The one-day "web cannot run it" claim this section carried was a **harness** defect and, as written, a wrong diagnosis: `reached_authenticated_app` waited on `feed-tab`, which the Settings shell swaps out *in place* (§ Navigation model above), so a relaunch from Settings → Privacy could never satisfy it and died as a bare 90 s timeout. The injected login had been surviving the relaunch all along (the web agent writes the session to `localStorage` and the reloaded SPA re-hydrates from it). The wait now accepts any of the three authenticated shells (`tests/common/launch_harness.py`).

**The inbox-mode selector's catalog has one owner over UniFFI; one app has not adopted it yet** (2026-08-23, § Where logic lives → *The inbox-mode selector's rows*). `libs/fauna-ffi/src/contacts_client.rs::inbox_mode_options()` is the door (value-format-gated, `LocalizedText`-keyed, deliberately independent of the Rust-native `INBOX_MODES` table). **android adopted it 2026-08-23** (`PrivacySettingsScreen.kt`), replacing its hand-rolled four-row `Triple` list. **apple adopted it 2026-08-26** (`PrivacySettingsView.swift`), iterating `inboxModeOptions()` for value/label/desc alike, mirroring `AdminUsersHubView.registrationModePicker`'s shape. **windows still hand-writes the catalog** (`SettingsPrivacyPage.xaml`); tracked internally. The door carries **no default** — per the item-7 rule above, a picker with no fetched mode yet shows none selected, never a guessed one. **apple fixed 2026-08-27**: `PrivacySettingsVM.swift`'s `inboxMode` went from a non-optional `String` hardcoded to `"allow_knock"` to `String?` defaulting `nil` (matching tui's `Option<String>` shape), the page's shared `error-message` carries `INBOX_MODE_UNKNOWN` while it is `nil`, and the two state-mirror fields feeding the e2e snapshot (`AppState.inboxMode` on iOS, `MacAppState.inboxMode` on macOS, plus iOS's own early pre-fetch site in `SettingsView.swift`) got the same treatment so the snapshot cannot report a guessed mode either. `test_inbox_mode_pre_selects_the_accounts_real_mode_after_a_relaunch` still runs green (2026-08-27) — it only asserts convergence, which this fix doesn't touch. **The pre-fetch window itself is now pinned too, as of 2026-08-27**: `test_inbox_mode_is_unknown_while_its_fetch_is_still_pending` holds the `fauna.inbox.mode.get` reply pending at the nest's dispatch chokepoint, waits for the request to actually arrive, and asserts the page names **no** mode — then releases and watches it converge. The mechanism is general (any registered kind) and is owned by [`../architecture/e2e-latency-independent-assertions.md`](../architecture/e2e-latency-independent-assertions.md) § The convention → *Pre-fetch windows*, which is where the other rules of this class (`../behavior/family-safety.md` § Cold start) should reach for it. The test asserts the **mode**, not a rendering: the apps legitimately differ on whether this page is on screen during the window (linux and web build it at once and repaint; tui awaits `Op::FetchPrivacy` on the nav edge), and both shapes satisfy the rule. **tui is a declared absence for this one test, and the reason is structural, not debt:** its nav edge does not merely await the fetch, it *is* the fetch — `automation.rs::apply_nav` awaits `Op::FetchPrivacy` before the nav command acks (deliberate: a fire-and-forget spawn let a stale nav-edge refetch clobber a fresher mutation). So holding the read makes the NAV time out rather than exposing a pending page — measured 2026-08-27, the nav acked ~5 s later carrying `inbox_mode_get: The nest took too long to respond`, the request already abandoned. tui cannot show a mode it has not read because it does not show the page, so item 7 is satisfied with no window to stand in; its state provider reports `""` throughout, and the relaunch test covers its convergence. The e2e discriminator is `SettingsActions.privacy_nav_awaits_its_mode_read`, which defaults to *has a window* so an app it has never heard of is tested rather than excused. **Verified GREEN on linux 2026-08-27** — the read genuinely held, arrival observed nest-side, the page named no mode, and it converged on release. **Verified GREEN on macos and ios 2026-08-28** — the app-parametrized test ran on both apple targets for the first time; both hold the Privacy nav open while the read is pending (neither is tui's fetch-on-nav-edge shape, so `privacy_nav_awaits_its_mode_read` correctly leaves them in the tested set rather than excusing them), the held `inbox.mode.get` request's arrival was observed nest-side on both, the page named no mode on either, and both converged on release — no code fix needed, the earlier `String?` fix already carried this window correctly. **Verified GREEN on windows 2026-09-06** — windows is the linux/web repaint shape, not tui's fetch-on-nav-edge shape (`SettingsPrivacyPage.xaml.cs` paints in `OnNavigatedTo` and fetches asynchronously in `Page_Loaded`), so `privacy_nav_awaits_its_mode_read` correctly leaves it in the tested set; the held `inbox.mode.get` request's arrival was observed nest-side, the page named no mode while it was pending, and it converged to the stored mode on release — no code fix needed, the 2026-08-26 seed/mirror fix (above) already carried this window correctly. (That fix's mirror write path has a second, dormant writer — an `App.xaml.cs` `set_state` injection arm for direct state-provider seeding — which no fixture exercises; `SettingsPrivacyPage`'s `PropertyChanged` handler is the only writer any test path reaches.) **android has not been fixed** — `PrivacySettingsVM.kt`'s `inboxMode` is still `MutableStateFlow("allow_knock")`, same bug.

**Navigation shell — built on all seven apps.** Desktop sidebar-swap shell + `settings-nav-back`: linux (reference, 2026-06-03), web + windows (2026-06-04), macos (2026-06-13 — `SettingsShellView`/`SettingsNavRail`/`SettingsPage`, e2e green `--client macos` 2026-06-14/16); ios/android keep idiomatic settings nav and honor the two-element sub-page nav (iOS path-binds `AppState.selectedSettingsPage` via the shared `SettingsPage` enum — the same enum the macOS rail consumes; the user-mail sub-page nav id is `mail-settings`, its ui.yaml page id — the bare `mail` slug was renamed on all 7 apps 2026-10-02). **The 2026-06-28 Devices/Folders unification is landed on all seven apps** (android first 2026-06-28; windows `DevicesPage`/`FoldersPage` with `SyncPage`/`SyncFoldersPage`/`ConflictsPage` retired; linux `views/devices_folders/*`; web rail `devices` + `folders` sub-pages; apple shared `DevicesContent`/`FoldersContent` via `SettingsPage.case devices/.folders`; tui last, 2026-07-22 — its core folders control plane, not every per-set follow-on) — per-page frontier: `devices.md` / `folders.md`.

**Rail-order convergence gap (Subscriptions / Web) — an ordinary per-app parity gap, re-verified this sweep.** Only web places every entry at its canonical § Navigation model position (`apps/fauna-web/src/routes/settings/+layout.svelte:30-69`). linux instead places Subscriptions right after Account (`apps/fauna-linux/src/views/settings_shell.rs:314-320`) and Web right before Nests, after the mail sub-pages (`settings_shell.rs:403-409`); windows places Web right after Encryption and Subscriptions right after Web, both ahead of Devices (`apps/fauna-windows/FaunaApp/FaunaApp/Views/SettingsShellPage.xaml:63-64`). All three already agree on the Personalization/Community-labelers placement (right after Muted words). tui — which has no Nostr rail row to place (P2P and Subscriptions **are** built and wired, `apps/fauna-tui/src/settings/root.rs:155,290`; a prior version of this line claimed tui lacked them too — corrected 2026-08-13, see the heading-conformance measurement below; General and Encryption landed 2026-08-14 right after Community labelers, `root.rs:120-141` — canonical relative to their immediate neighbors, though tui's own rail as a whole still diverges further below) — diverges further: Devices, Folders, AT Protocol, Nests, Task delegation and Connected apps all render *after* Logs (`apps/fauna-tui/src/settings/root.rs:260-322`), where the canonical order places them all before it. No behavior differs — only rail position — so this is cosmetic drift toward the canonical order above, not a functional gap.

**Sub-page heading conformance (measured 2026-08-13, ruled in [`README.md`](README.md) § Navigation model; linux's gap CLOSED 2026-08-14).** A walk found linux's Settings shell painted **no heading at all** on 8 of its sub-pages — `adw::PreferencesPage.title()` is inert metadata inside the shell's plain `gtk::Stack`. Measuring the other 6 apps on the same 8 sub-pages (Account, Privacy, Muted words, General, Encryption, P2P, Web, Logs) found the bug was not fleet-wide. Fixed on linux 2026-08-14: each `build_*_page()` now returns a `gtk::Box` with a real, visible `title-2` `gtk::Label` (`crate::testid::wrap_page_with_heading`, `apps/fauna-linux/src/testid.rs`) prepended above the page, carrying `page-heading` (six pages) or the page's own ui.yaml landmark id (`muted-words`, `settings-logs`) — `apps/fauna-linux/src/walk.rs::check_surface_has_a_heading` now accepts both shapes and the sweep asserts `headless.is_empty()` with no known-gap list, mirroring the admin sweep:

| Sub-page | linux | web | windows | macos | ios | android | tui |
|---|---|---|---|---|---|---|---|
| Account | ✅ | ✅ | ✅ | ✅ | ✅ (`.pageTitle()`, FIXED 2026-08-22) | ✅ | ✅ |
| Privacy | ✅ | ✅ | ✅ | ✅ | ✅ (`.pageTitle()`, FIXED 2026-08-22) | ✅ | ✅ |
| Muted words | ✅ (own id) | ✅ | ✅ (own id) | ✅ | ✅ | ✅ (own id) | ✅ |
| General | ✅ | ✅ | ✅ | ✅ | ✅ (different feature: Notifications; `.pageTitle()`, FIXED 2026-08-22) | 🚫 not built | ✅ |
| Encryption | ✅ | ✅ | ✅ | ✅ | ✅ (`.pageTitle()`, FIXED 2026-08-22) | ✅ | ✅ |
| P2P | ✅ | ✅ (redirect stub) | 🚫 deleted 2026-08-23 (was a top-level nav page, not a Settings sub-page here) | 🚫 not built (by design) | 🚫 not built (by design) | 🚫 not built | 🚫 deleted 2026-08-23 |
| Web | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Logs | ✅ (own id) | ✅ | ✅ (own id) | ✅ (via shell-wide `.pageTitle()` wrap) | ✅ (`headingId: Ids.pageHeading` param, FIXED 2026-08-22) | ✅ | ✅ (own id) |

✅ = real, painted, id-bearing heading. ⚠️ = a real title paints and a sighted user sees it, but no `page-heading`/`*-heading` id is attached (invisible to automated/accessibility tooling). ❌ = nothing paints at all. 🚫 = the sub-page doesn't exist on that app (not a heading defect). Windows' P2P page (a top-level nav page, not a Settings sub-page — `Views/P2PPage.xaml`) was deleted 2026-08-23 with the WireGuard stack, and P2P is now ratified linux-only ([`../behavior/p2p.md`](../behavior/p2p.md), 2026-08-24) — this sentence used to note that the shared nav id `{"view":"settings","id":"p2p"}` silently resolved to Status on windows, which is moot now that windows has no P2P surface at all.

Owed work, tracked per-app (none of it platform-tool work belongs in this doc-and-measurement track): iOS's Logs heading (real bug) + the 4 `⚠️` id-only gaps CLOSED 2026-08-22 — `SettingsView.swift:349`'s `LogsView(...)` call now passes `headingId: Ids.pageHeading`; `AccountSettingsView`/`PrivacySettingsView`/`EncryptionSettingsView`/`NotificationSettingsView` swapped their bare `.navigationTitle(...)` for the shared FaunaKit `.pageTitle(...)` modifier (same call every other iOS settings sub-page already makes). Verified: full `just apple-ffi` + `just swift-test` (390/390 green, both apple targets) + `pytest tests/e2e-unified/tests/test_settings.py::test_sub_page_heading_is_visible tests/e2e-unified/tests/test_settings.py::test_general_sub_page_heading_is_visible tests/e2e-unified/tests/test_settings_logs.py::test_logs_page_heading_is_visible --app ios` (5/5 GREEN, 681.56s). android's 4 `⚠️` id-only gaps CLOSED 2026-08-14: `page-heading` added to Account/Privacy/Encryption's `Text(...)` titles (Encryption's screen split into a stateless `EncryptionSettingsContent` to make it Robolectric-testable at all, mirroring `PrivacySettingsContent`/`MutedWordsContent`; Account's much larger VM-entangled body was left as-is, with only its top bar extracted to `AccountSettingsTopBar` — a full stateless split was judged disproportionate to a missing heading id **at the time; the split itself landed 2026-08-22, once the page needed two more untestable gated controls — see `account-data-plane.md` § Implementation status**), and Muted words' own `muted-words` id moved from the wrapping `Column` to the actual `TopAppBar` title Text (mirroring linux's `wrap_page_with_heading`, which pins the id on the heading itself, not a container). tui's General/Encryption standing tui-parity gap CLOSED 2026-08-14: both sub-pages built, real `page-heading`, reachable from the rail (`apps/fauna-tui/src/settings/{general,encryption}.rs`) — General deliberately scoped to what's genuinely tui-applicable (an appearance note explaining the terminal owns the color scheme, plus the app version; no theme picker, autostart, tray, or updater — none apply to a terminal app), Encryption mirroring linux's MLS key-package count + refresh. Content besides the shared `page-heading`/`settings-nav-back` carries no ui.yaml id, matching linux/web/windows/macos (no app has ever allocated ids for this content).

**Recovery kit — ratified 2026-08-01; tui renders the section the same day, then linux + web
(2026-08-17), macOS + iOS (2026-08-21) and windows (2026-08-24). Android follows.** ⚠ Not every landing carried the
**stolen-identity trigger** (`identity-stolen-button` + its `identity-stolen-confirm-field` gate) from
day one — linux's and web's 2026-08-17 sections did not (web added it 2026-08-20, linux 2026-09-01; below), so
"renders the section" and "can run the succession ceremony" are different claims per app;
[`../behavior/identity-succession.md`](../behavior/identity-succession.md) § Implementation status
today is the authority on which app has which.
The ceremony layer is done and green (`libs/fauna-client-recovery`, wasm-verified) and **tui is the
first app to render the section** (status line, create, and the seed-alone lost flow are live).
**Kit-in-hand entry landed 2026-08-02 on tui:** `recovery-entry-phrase-field` renders inline in the
section, and **all four ceremonies now run over it** — replace drives `create_kit`'s `prior` arm,
which re-puts the escrow blob in the same ceremony as
[`../behavior/identity-succession.md`](../behavior/identity-succession.md) § Seed escrow requires;
veto cancels a pending window; and **`identity-stolen-button` went live the same day**, driving the
shared `succeed_with_held_kit` and then adopting the successor through the existing add-account
switch, so the user comes back up signed in as the successor with their handle intact. Its
type-to-confirm gate is `identity-stolen-confirm-field` (added under rule A 2026-08-02, named off its
own button so the gate and the action read as one family), matching the literal `SUCCEED` exactly —
the prompt is localized, the token never is. `optional_elements` covers both the phrase field and the
confirm field: the first renders only while a ceremony can consume it, and the second is optional for
`settings-delete-confirm-field`'s reason (an app may gate behind a native OS alert with no drivable
id). What that button does **not** do is the § Re-key scope aftermath — the successor lands in the
never-created state, so the standing warning and `recovery-kit-create-button` prompt the fresh kit
rather than it being re-minted silently, and the button's own warning copy says so.
**The let-go is built on tui (2026-10-02), macos and ios (2026-10-03):** on tui the trio renders from `RecoveryState::dead_generations`, which the Account hydrate fills from the runtime's `AccountStoreHandle::dead_generations`, and the button drives `AccountStoreHandle::let_go`. macos and ios render it once, in the shared FaunaKit `RecoveryKitSection` / `RecoveryKitVM`, over the UniFFI door to the same two handle methods, the copy projection and the confirm word (`libs/fauna-ffi/src/generation_let_go.rs`: `recovery_dead_generations`, `recovery_let_go`, `recovery_let_go_confirm_word`) — the door windows and android reuse. linux, web, android and windows render none of it yet.
**The no-escrow repair landed 2026-08-10 on tui:** `recovery-kit-escrow-reseal-button` renders in
`RegisteredNoEscrow` only (ID user-approved 2026-08-16), reads the same kit-in-hand field, and drives the
shared head-checked re-put (`reseal_escrow_with_held_kit`) — the ruling and its pins live in
[`../behavior/identity-succession.md`](../behavior/identity-succession.md) § Seed escrow →
*Lifecycle on the nest* + § Implementation status today.
**linux and web landed the section on 2026-08-17** — the same status line, create, replace and
seed-alone-lost core, with the kit-in-hand phrase field covering the replace arm only at that
landing: neither app built the veto action, the stolen-identity ceremony, or the no-escrow repair.
**Web's own stolen-identity ceremony landed 2026-08-20** (`identity-stolen-button` +
`identity-stolen-confirm-field`, driving the same shared `succeed_with_held_kit`; the phrase field's
gate widened from replace-only to `allows_replace || allows_stolen` in the same fix, matching the
`NeverCreated` state where replace is false and stolen is true) —
[`../behavior/identity-succession.md`](../behavior/identity-succession.md) § Implementation status
today owns the current per-app ledger. **Linux's own stolen-identity ceremony landed
2026-09-01** — the same pair, the same shared `succeed_with_held_kit`,
and the same phrase-field widening web made, driven **in-process** rather than over the FFI face
(linux is Rust-native like tui). Both apps built the veto action and the no-escrow repair on 2026-09-26.
Which app has which ceremony is the owner's ledger, not this section's — a restatement here went stale
(it still read "windows, macos, ios and android don't render any of it yet" months after windows and
apple landed theirs), so the roll-out state is now only pointed at. Tracked internally; the feature-side gap list is
[`../behavior/identity-succession.md`](../behavior/identity-succession.md) § Implementation status
today.
**The sweep's own lines are a shared projection as of 2026-08-27** (`SweepView::copy` +
`owes_work`, § Recovery kit → *The sweep's own lines*): the selection — including the
`groups == 0` silence tui alone had been making locally — moved out of
`apps/fauna-tui/src/settings/recovery.rs` into `fauna-client-recovery`, and **tui and web render
the lines** (web had carried the render view since 2026-08-21 and painted nothing from it). **macOS
and iOS land theirs as of 2026-08-27** — `SuccessionHandoff` carries the view across the switch
(the same lifetime as `sweep_state_json`, cleared only on a factory reset) and `RecoveryKitSection`
paints the two lines at the top of the section, above the aftermath progress lines, exactly as
tui orders them. **windows landed 2026-09-03**: `SuccessionHandoff` carries `FfiSweepView` beside `SweepStateJson` across the account
switch, `RecoveryKitViewModel` exposes `SweepOutcomeLine`/`SweepUnattestedLine` resolved through
the app's own `Strings.Resolve` i18n pipeline (never the machine's pre-localized string), and
`SettingsAccountPage` paints them at the top of the section, above the aftermath progress lines
and `recovery-kit-status`. android inherits with its ceremony leg. **The lines' two ids
(`recovery-kit-sweep-status`, `recovery-kit-sweep-unvouched-status`) are tagged on tui, macOS and
iOS as of 2026-09-27** (macOS and iOS share `RecoveryKitSection`'s `sweepChrome`, which registers
each line's automation read like the aftermath lines below); web, linux and windows paint the lines
untagged — the render predates the ids — and tag them in their trickle-down, android with its ceremony leg.

**windows renders the aftermath progress lines as of 2026-09-25** — legs 1, 2, 3, 4, 7 and 6 in
that order, above `recovery-kit-status`: `LoggingAftermathSink` records each leg's
already-resolved line into the process-wide `AftermathProgress` store (a `null` clears it; a new
pass resets it) and `SettingsAccountPage` paints them live, each Collapsed while its leg has
nothing to say. Leg 5 (`recovery-kit-corpus-reseal-status`) stays unrendered, as on apple — it
reports from the sync agent's own process into no FFI app's sink.

**Leg 3's line (`recovery-kit-mls-reseal-status`) renders on macOS and iOS as of 2026-09-12**
 **and on windows as of 2026-09-25 (above); android still holds the field `nil`.**
`libs/fauna-ffi/src/mls_sync_launch.rs`'s launcher now reports through the same `FfiAftermathSink`
`run_succession_aftermath` registers (`FfiNestClient::set_aftermath_sink`, a late-populated holder
read at the moment the reseal itself reports, so build order between the two doesn't matter), and
`FfiAftermathLeg` carries a `MlsReseal` case; apple's render was already in place and paints the
moment the field is non-nil. android and windows own their own trickle-down of the same enum
mapping (out of scope) before their `AftermathProgress`/render twins gain the line.
`recovery-kit-sweep-retry-button` renders on **tui, web, macOS, iOS and — as of 2026-09-01 —
linux**; linux paints the sweep's two lines in
the same change, off the same parked `SweepStatus`. It is the second app whose press can *finish* a
sweep: like tui it calls `ceremony::retry_sweep_as_successor` in-process over its own two store
paths, where apple and windows go through the `succession_retry_group_sweep` UniFFI face — and its
journey (`test_succession_sweep_retry.py`, `--app linux`) was **green on its first run**, with
neither of the two defects tui's own first green run met (the retired engine outliving the switch,
and the successor seated as the old leaf by its own replica restore) reproducing there. The
2026-08-27 leg — the
apple leg folded the retry's FFI face (`succession_retry_group_sweep`, `libs/fauna-ffi/src/
recovery.rs`) into the same change rather than shipping the two lines with the button declared
absent first. **windows folded the retry's UniFFI-face call into the same 2026-09-03 leg as the
two lines**, declaring it present to the projection (`rendersRetry:
true`) from the same change that ships the button, per the render rule below. Web's
press can only ever answer in words — no browser holds the retired identity's MLS state — which is
the render rule above working as ratified, not a gap. **The retry's orchestration is
shared as of 2026-08-27** — `fauna_client_recovery::ceremony::retry_sweep_as_successor` (the engine
pair, the old store's existence check, and the four answers above), lifted out of tui, which now
supplies only its two store paths. **The press RUNS on the ceremony's own device as of
2026-08-27** — until then the retired identity's engine outlived the account switch, so every
press there answered the served-elsewhere refusal with no other instance anywhere; the mechanism,
the fix and its pins are owned by
[`../architecture/apps/account-scoping-dispositions.md`](../architecture/apps/account-scoping-dispositions.md)
§ Implementation status today → the `tui (in-memory)` ledger row, and
`test_succession_sweep_retry.py` asserts the refusal is gone. **And it FINISHES there since
2026-08-27:** the first press that got past the lock had found the successor seated in its groups
as the *old* leaf — its launch restored the predecessor's openMLS provider over the group it had
just joined, so remove-old answered `CannotRemoveSelf`; ruled and built the same day
([`../behavior/succession-aftermath.md`](../behavior/succession-aftermath.md) § Re-key scope →
*What a successor's replica restore may take from a predecessor's*), and the journey's
finishing tail — one press, every group re-pointed, the button retired, a member's channel
grown — is green on tui; macOS and iOS supply only their two store paths to the same shared
driver, and `helpers/succession_retry.py::assert_the_retry_affordance_matches_the_sweep` — which
rides every succession this suite runs rather than staging one of its own — is what asserts the
gate on them too. **windows landed both its call and its paint 2026-09-03**, verified by a `RecoveryKitViewModel` test pinning the carried-view → line keys and the
press itself (swept → replaces the view + the handoff, no error; the other four kinds → the error
surface, view untouched) — no e2e coverage of the press/render half exists on any app today
(measured against the fleet's own e2e coordination notes), so this pin is the only witness. The `*_no_retry` copy
and the `SweepRetryAffordance` flag retire only when the **seventh** app renders the button, and
today one of the seven still owes it — **android**, whose section (2026-10-06) does not yet run
the stolen-identity ceremony the sweep follows — so that retirement is gated behind android's own leg alone.

**Credential store — IDs approved 2026-08-06; tui is the first (and today only structurally
eligible) implementer.** The section + change-passphrase modal are built on tui behind the
sealed-arm gate; the other six apps render nothing here by design (§ Credential store's render
rule), so this is **not** a parity gap to trickle down — an app joins only if it grows a
per-device custody affordance of its own. Mechanics + implementation state:
[`../architecture/apps/tui.md`](../architecture/apps/tui.md) § Credential storage /
§ Implementation status today.

Shared-mechanics adoption (surface × app):

| Surface | linux | web | windows | macos | ios | android | tui |
|---|---|---|---|---|---|---|---|
| Sidebar-swap shell + `settings-nav-back` | ✅ ref | ✅ | ✅ | ✅ | idiomatic | idiomatic | idiomatic (rail `SubPage` shell, `settings-nav-back` returns to Root) |
| Sign-out drivable confirm | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ (e2e emulator-gated) | ✅ |
| Handle-change shared validator | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Spam band label (`share_model_values` retired 2026-10-02 with the `share-model` control — § Spam moderation) | ✅ | ✅ | ⏳ label on next touch | ✅ | ✅ | ✅ | ✅ |
| Spam slider↔wire (shared rounding) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ (text input, same shared rounding) |
| Spam transport (`fauna.spam.*`, off the HTTP twin) | ✅ | ✅ | ✅ (migrated off `DirectNestClient` onto `FfiSpamClient`, 2026-06-13) | ✅ | ✅ | ✅ | ✅ |
| Email-filter encoder (full 5-rule dialog) | ✅ | ✅ | ✅ | ⏳ 4/5, no `HeaderExists` | ⏳ 4/5, no `HeaderExists` | ✅ | ✅ |
| Email-filter edit (`filter-edit`/`save-filter`, decode + gate) | ✅ ref 2026-07-16 | ✅ 2026-07-16 | ✅ 2026-07-18 | ✅ 2026-07-22 | ✅ 2026-07-22 | ✅ 2026-07-16 | ✅ 2026-07-19 |
| Email-filter Forward action (`filter-forward-address` / `filter-keep-local-copy`, `FilterActionInputs` seam) | ✅ ref 2026-10-02 | ✅ ref 2026-10-02 | ✅ 2026-10-02 (e2e-witnessed on windows) | ✅ 2026-10-02 | ✅ 2026-10-02 (shared FaunaKit form, e2e-witnessed on macos) | ✅ 2026-10-02 (compile-verified) | ✅ ref 2026-09-28 |
| Identity export (§ Identity export) | ✅ ref | ✅ | ✅ 2026-07-12 | ✅ 2026-07-12 | ✅ 2026-07-12 | ✅ | ✅ 2026-07-15 (independent, pre-parity-flip) |

**Identity export — all seven apps implement it.** linux/web/windows/android landed it by
2026-07-12; tui landed its own copy independently 2026-07-15 (M5 Settings account/quota ROOT),
predating the 2026-07-19 fleet-wide 6→7 parity flip — see macos/ios below for the last
pre-flip holdout. **No app links a platform QR library any more.** The element IDs were
approved (rule A) and the shared encoder now reaches the apps, so **linux (reference), web,
windows, tui, and android** render the section on the five `identity-export-*` IDs, driven by
`fauna_core::qr_matrix`. The reveal gate — description + toggle always visible, warning + QR
only after the user presses show — is pinned cross-app by
`tests/e2e-unified/tests/test_identity_export.py` (green on linux, web, windows, macos, ios, and tui; the
android marker is declaratory until android e2e runs). **windows leg (2026-07-12):** XAML
`Canvas` + one `Rectangle` per dark module (no Win2D/NuGet QR dependency) — the canvas needs
an explicit `AutomationProperties.Name` in addition to its `AutomationId`, or FlaUI prunes the
bare shape-only container from the UIA tree (`count=0`; mirrors `mail-aliases-list`'s
`Name="Aliases"` fix for the same reason). The secret is read from `ISecretStore.LoadSecret()`
(never a settings snapshot).

**macos / ios leg (2026-07-12) — the last holdout, now closed.** Apple had *shipped* the
section for months, but drew it through CoreImage's `CIFilter.qrCodeGenerator` behind an
`NSImage`/`UIImage` `#if` fork and carried **no `accessibilityIdentifier`** at all — so it was
both the tree's last platform QR encoder and untested UI living outside `ui.yaml`. Both are
fixed: one shared FaunaKit `QrCodeView` (SwiftUI `Canvas`) paints the same
`fauna_core::qr_matrix` grid for both apps — so the `#if` fork is gone — and the five
`identity-export-*` IDs are stamped. `IdentityExportSection` now holds the matrix as *optional
state* rather than a `Bool`, so collapsing drops the encoded secret instead of retaining it
off-screen (matching linux / web / android). Quiet zone comes from `qr_quiet_zone_modules()`,
never a hard-coded 4, and dark-on-light is painted explicitly rather than from theme colors (a
theme-inverted QR does not scan — the one place dark mode is deliberately ignored).
`test_identity_export.py` is **3/3 green on `--client macos` and 3/3 on `--client ios`**.

With this leg, the then-6-app fleet all drew the one shared encoder; counting tui's own
identical implementation (independently landed 2026-07-15, predating the 2026-07-19 parity
flip), **all seven apps draw the one shared encoder** — the claim in § Where logic lives
("no app links a platform QR library") is now literally true, not aspirational.

**What the apps call.** `fauna_core::qr_matrix` (feature `qr_render`, off by default —
the nest never renders a QR; CI-guarded by its own step in `ci.yml`) turns any payload into
the `QrMatrix` boolean grid, with a decode round-trip test. Its faces, both added 2026-07-11:
`fauna-ffi`'s `qr_matrix` / `qr_quiet_zone_modules` (feature `qr-render`, default-on for the
Apple/Android/Windows app FFI, gated out of the Go mail-bridge build — so the tracked Go
bindings do not churn on this surface), and `fauna-wasm`'s `qrMatrix` / `qrQuietZoneModules`.
Linux calls `fauna_core` directly. The faces are **generic over the payload**, not
identity-specific: `fauna_peer`'s invite QR is the second consumer waiting on them.

Each app draws the grid with its own toolkit and adds the `QUIET_ZONE_MODULES` margin
itself — never a hard-coded `4`, and never a platform QR library.

Dated one-liners: sign-out drivable confirm all-6 (2026-06-13; `test_sign_out` a 5-app gate, android emulator-gated); handle-change validation lifted to `fauna_protocol::handle::validate_handle` (2026-06-19 linux/windows/web; 2026-06-21 apple/android; `test_handle_change_validation` skips-pending-adoption, linux never skips); spam slider↔wire adoption completed 2026-06-29→07-01 (fixed the C# banker's-rounding edge, regression-pinned; the one inline copy left is nest-side `spam_handlers.rs::to_per_mille` — consume leg tracked internally on the nest side); the apple Settings-view lift is complete (shared FaunaKit Encryption/Privacy/Account views, 2026-06-13; `SyncSettingsView` stays per-platform by design); macos `test_quota_section` regression owned by the settings-shell track (tracked internally); web's `p2p` rail sub-page remains an honest pointer to Devices/Bridges pending real web content (verified current — `apps/fauna-web/src/routes/settings/[[subpage]]/+page.svelte`'s `p2p` branch); the former `sync` pointer is not a separate case any more — the 2026-06-28 Devices/Folders unification (§ Implementation status today, above) gave `folders` its own real content (`FoldersSection`), so "sync" no longer needs (or has) a stub (nostr is content-backed via the shared `NostrSettingsSection`, 2026-06-19).

Multi-account switcher: the ui.yaml `account-switcher-*` family renders on this page; behavior + rollout → `../architecture/long-term-store.md` § Multi-account evolution.

## Persistence

TBD. Settings persist nest-side; app mirrors via snapshot. Cross-reference identity store contract from `onboarding.md`.

## Errors & edge cases

- `error-message` page-level.
- Handle-change: **format validation** is shared Rust (`fauna_protocol::handle::validate_handle`), surfaced in `error-message` pre-submit; **conflict** (taken handle) is server-authoritative, surfaced from the `fauna.profile.handle.change` rejection. Account-delete confirmation, sign-out while unsynced — TBD; snapshot variants.

## Architectural rules

1. Observer-driven rendering once shared snapshot exists.
2. Account deletion and handle change run identical validation across all 7 apps (shared Rust).
3. Email filter rule semantics are shared Rust.
4. Web inlining of Status info is acceptable; cross-references `status.md` (shared elements: `status-actor-id-copy-btn`, `status-node-url-copy-btn`).

## Don't do these

- Don't validate handle-change per-app.
- Don't evaluate email filters per-app.
- Don't carry per-platform inbox-mode IDs — the four `inbox-mode-*` IDs are canonical.
- Don't proceed past TBDs in this doc for any settings work that affects behavior.

## Done definition

- [ ] All ui.yaml `settings` elements render with canonical IDs.
- [ ] Inbox mode, spam, filter, handle-change, sign-out, delete-account go through shared Rust.
  - Status: **inbox-mode / spam / filter — DONE** (shared `fauna-client-contacts` / `fauna-client-spam` / `fauna_protocol::email` encoders). **sign-out / delete-account — transport shared** (`fauna-client-account`); the residual glue is inherently per-platform. **handle-change — DONE** (one validator, nest + all 7 apps). See the § Implementation status matrix.
- [ ] Web's inlined status section uses `status-*` IDs; cross-references `status.md`.
- [ ] `tests/e2e-unified/ui-actual-<app>.yaml`'s `settings` block refreshed; `ui-actual-lint` introduces no new errors.

## Reading list

1. `principles.md` — product invariants + engineering principles.
2. `tests/e2e-unified/ui.yaml` — `settings:` page block + components.
3. `docs/goal/ui/status.md` — for the web inlining cross-reference.
4. `docs/goal/behavior/onboarding.md` — handle-check pattern reused for handle change; sign-out returns user there.
5. `tests/e2e-unified/ui-actual-<app>.yaml`.
