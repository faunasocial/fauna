# Bridges — target state

Owns: bridges, bridged-authors
Status: ratified — 2026-06-28 (the unified feed-side Bridges page: metadata-driven list + detail shape, the `fauna.bridges.*` link/settings/follows wire, the three feed-side providers) + the 2026-06-28 user ruling that bridge feed subscription stays on the Feed page; per-app gaps closed (see § Implementation status today)
Authority: the unified feed-side Bridges page + the `fauna.bridges.*` UI wire — the metadata-driven list/detail shape (`BridgeStatus`/`BridgeLinkMode`/`BridgeLinkField`/`BridgeSetting`/`BridgeFollow`, `libs/fauna-protocol/src/bridges_ui.rs`), link modes/settings/follows semantics, the provider registry model, and the consume-side Bluesky provider behavior (OAuth linking + callback residue, unified feed ingestion, bridge-feed subscriptions, the `bluesky.feed.thread` kind, `write_through` cross-posting, bridged notifications); ui.yaml (`bridges` page) owns element IDs + per-page element scope; defers the Nostr dedicated page to [`../ui/nostr.md`](../ui/nostr.md), the mail-bridge surfaces to [`../ui/mail-settings.md`](../ui/mail-settings.md) + [`mail-credentials.md`](mail-credentials.md) + [`mail-bridge-lifecycle.md`](mail-bridge-lifecycle.md), bridge-as-client enrollment/allowlist to [`../architecture/apps/bridges.md`](../architecture/apps/bridges.md), the host-direction PDS to [`atproto-pds-bridge.md`](atproto-pds-bridge.md), the feed-subscribe form's UI home to [`../ui/feed.md`](../ui/feed.md), and element IDs/scope to ui.yaml.

Last verified: 2026-08-14 (docs-consistency sweep — the windows `bluesky`-exclusion carve-out (`BridgesViewModel.NoDedicatedPageOnWindowsYet`) was deleted 2026-07-31 once windows' own dedicated AT Protocol page landed; this doc still described it as live — corrected in § Goal and § Where logic lives, plus a matching stale sentence in `ui/atproto.md` § Migration; also corrected two "all 6 apps" pre-tui-flip phrasings in § Don't do these / § Done definition) | Sources: `libs/fauna-protocol/src/bridges_ui.rs`, `bins/fauna-nest/src/bridge_management.rs`, `bins/fauna-nest/src/{bluesky,activitypub,nostr}/bridge_provider.rs`, the bridges client-architecture design (2026-05-04, tracked internally), `docs/goal/architecture/api-layers.md`

## Goal

The Bridges page lets the user manage the bridges connecting their nest to external networks: IMAP/SMTP (email — its own `mail-settings` page, see § Scope), Bluesky/ActivityPub (feed reading — the inbound subscribe form lives on Feed today), Nostr (a deep integration with its **own dedicated page** like mail — see § Scope + [`nostr.md`](../ui/nostr.md)), and any future bridge types. A bridge is a server-side process (`bins/fauna-bridges` for SMTP / IMAP / CalDAV, serving both the MTA and MDA roles and talking WS-RPC to nest) that the user enables, configures, and monitors.

**Scope vs. the mail-bridge surfaces** (landed alongside the i3 client-provisioning plan, 2026-05-14, tracked internally). This page covers user-facing *feed-side* bridges — the kind the user enables and configures to bring external-network content into their unified feed, follows graph, or notification stream (Bluesky, ActivityPub, Nostr feeds; future protocols). The *mail bridge* (the MTA + MDA processes implementing SMTP / IMAP / CalDAV for third-party MUAs like Thunderbird / Apple Mail) is a different surface and lives in different docs: user-side credential management on the mail-settings page lives in [`docs/goal/ui/mail-settings.md`](../ui/mail-settings.md) (feature behavior in [`docs/goal/behavior/mail-credentials.md`](mail-credentials.md)); admin-side DKIM/TLS provisioning is **automatic** (no manual admin UI; [`docs/goal/behavior/mail-bridge-lifecycle.md`](mail-bridge-lifecycle.md)), with the resulting DKIM/MX/SPF/… records surfaced + verified on the unified `admin-dns` page ([`docs/goal/behavior/dns-management.md`](dns-management.md)); bridge approval lives in [`mail-bridge-lifecycle.md`](mail-bridge-lifecycle.md) (rendered on the `admin-bridges-pending` admin-pane page). The unified design here does not surface the mail bridge; the admin-facing "is mail bridge connected" status indicator lives in the admin pane only, alongside the rest of the mail-bridge config.

**Bluesky has its own dedicated page too — the same treatment as Nostr and mail (ratified 2026-07-22, user; [`../ui/atproto.md`](../ui/atproto.md), landed same day).** The consume-side OAuth link this page owns becomes the **"Linked account"** level of that page's ordered integration-depth selector (the two identity backings are alternatives — [`atproto-pds-bridge.md`](atproto-pds-bridge.md) § Relationship — so the selector has to hold all of them). This page keeps the ActivityPub + future feed-bridge cards and at most links out to the AT Protocol page, exactly as it does for Nostr. The hand-over was per app, specified in [`../ui/atproto.md`](../ui/atproto.md) § Migration (the `is_unified_bridges_page_bridge` predicate excludes `"bluesky"` once each app's AT Protocol page lands) — **complete on all 7 apps as of 2026-07-31** (windows last; see § Where logic lives below), so this page's consume-side provider card is retired everywhere. The provider *behavior* (OAuth linking, ingestion, `write_through`, …) stays owned here regardless of which page renders it.

**Nostr has its own dedicated page — the same treatment as mail (§ Scope above), NOT folded into this unified Bridges page** (ratified 2026-06-13, user; following linux's lead — supersedes the 2026-06-08 "managed here as one bridge among others" position). Nostr is a **deep** integration (posture 2, ratified 2026-06-08 — see [`docs/goal/ui/nostr.md`](../ui/nostr.md) § Architecture): the user's nest is a first-class Nostr home (NIP-01 relay + bidirectional outbox sync + custodial/NIP-46 signer + `you@<nest-domain>` NIP-05 identity), important enough to warrant its own page rather than being one bridge type among many. So the unified design here does **not** surface Nostr; its control-plane (link/status/settings/relays/follows) renders on the dedicated [`nostr.md`](../ui/nostr.md) page. **Transport vs. page are independent:** that control-plane still rides the shared `fauna.bridges.*` WS-RPC (`NostrProvider`, `bridge_id:"nostr"`, registered in `bins/fauna-nest/src/lib.rs` — `register(NostrProvider)`) — the unified *wire* is kept, only the *page* is dedicated. Its Nostr-native *content* features (DMs → unified Conversations; reactions/reposts/long-form/communities/live/classifieds/zaps/badges → the feed/content surfaces) live where that content lives, not on a settings page. Server-side bridging (agent acts: signing, relaying, gift-wrap unwrap) is gated on a user's deposited `nsec`, not on any storage posture — a keyless box (no deposit) delegates to a paired head that holds one, via the home-with-public-relay seam ([`nostr.md`](../ui/nostr.md) § The bridging gate → Phase 2, BUILT 2026-07-23).

The unified design is **ratified** (below): a metadata-driven list + per-bridge detail page served identically to all 7 apps from `libs/fauna-client-bridges`, rendering each provider's `BridgeStatus` (link modes, settings, follows) from server-declared metadata. (The standalone `nostr` ui.yaml page is **kept** as Nostr's own dedicated surface — like mail — and is **not** retired into this unified page; ratified 2026-06-13. This page is for the *other* feed-side bridge types — Bluesky/ActivityPub feed bridges + future protocols.)

## Layout & flow

Per the bridges client-architecture design (2026-05-04, tracked internally),
the page renders as a metadata-driven list page + drill-down detail page,
served identically to all seven apps from `libs/fauna-client-bridges`.
Per-bridge hand-coded card layouts and per-bridge "Disconnect" terminology
are explicitly dropped in favour of the shared shape; the link/unlink
verbs come from the shared `BridgeLinkMode` metadata.

**Standalone sidebar item on all 7 apps (ratified).** All seven apps now surface Bridges as a standalone sidebar item, macOS included (`SidebarItem.bridges` → `BridgesSettingsView`) — the former "macOS not a sidebar item" drift is resolved (2026-06-28); tui's Bridges page landed 2026-07-23 (`bridges-tab`, `apps/fauna-tui/src/bridges.rs`).

**Bridge feed subscription lives on the Feed page (ratified 2026-06-28, user).** The inbound subscribe-to-a-custom-feed-by-URI form (`bridge-form-*` elements: `bridge-feed-subscribe-toggle`, `bridge-form-bridge-select`, `bridge-form-uri-input`, `bridge-form-name-input`, `bridge-form-subscribe-button`, `bridge-form-cancel-button`, `bridge-feed-unsubscribe-button`) stays on the **Feed** page, where the user is already browsing feeds. The Bridges detail page does **not** host a subscribe form; at most it links to Feed. Consequently the `bridge-card` component's `bridge-feed-subscriptions` / `bridge-feed-item` / `bridge-subscribe-feed-button` elements — which exist today only as **invisible shims** on most apps (web `placeholder-hidden`, linux 1px markers) and so violate the "no invisible shim elements" rule — are **retired**; subscription is the Feed surface's job, not the bridge card's. (This was the one open Layout decision the prior art could not settle — two non-superset existing patterns, so per priority #4 it went to the user.)

### Link modes

Each bridge exposes one or more **link modes**, declared by the
`BridgeProvider` server-side and rendered from metadata. Each mode defines:

- **Fields** — text inputs for credentials or identifiers (e.g. app password,
  handle), described declaratively per mode.
- **Client action** — optional client-side action keyed off the mode; the
  spec lists `nip07` (trigger the Nostr browser extension) and `oauth`
  (open an OAuth flow that returns via callback) as the canonical examples.

Link flow: user selects a mode (if more than one is available on the current
platform), fills in any fields, and submits. For `oauth`-style modes the
client redirects to the provider and resumes on callback; for `nip07`-style
modes the client invokes the platform extension surface directly. On success
the bridge is `linked` and the snapshot reflects the external identifier.

`link_modes` is `Option`al on the wire and **may be absent** — a bridge can
declare no applicable mode, either because `provider.status()` errored (the
degraded shape) or because every declared mode is scoped to another platform.
That case is not a link flow at all: see § Errors & edge cases → *A bridge that
cannot be linked right now* for the ratified behavior.

### Bridge settings

When linked, each bridge may expose three field shapes, all rendered from
metadata:

- **Toggle** — on/off boolean (e.g. "Auto-publish posts", "Sync reactions").
- **Text** — editable string (e.g. relay URL).
- **Select** — dropdown choice from a declared option set.

Settings auto-save with a 500ms debounce on change; there is no per-page
"Save" button. This supersedes the older per-bridge explicit-Save UX
(see the bridges client-architecture design, tracked internally —
"Settings semantics: Auto-save on change with debounce").

Two uniform search-policy settings — **"show in search"** (toggle) and
**"limit posts in search"** (numeric cap; an additive `number`
setting shape) — ride these rows for every content bridge, supplied by the
provider registry rather than per-provider copies. Both rows are supplied by the registry rather than by any provider — the
nest appends them in `fauna.bridges.list` and strips them in
`fauna.bridges.set_settings`, so a provider never sees them. Policy owner:
[`content-index.md`](content-index.md) § Bridge content in the Search
corpus (ratified 2026-07-22; nest + shared Rust built 2026-08-02, the
per-app editable control for the `number` shape built on tui 2026-08-17 —
lead app, `apps/fauna-tui/src/bridges.rs::setting_elements` — the other six
apps still render it read-only per the additive-evolution fallback).

### Follows

Bridges that support follows expose a follows list. Each follow entry
carries:

- **External ID** — the followed account's identifier on the external network.
- **Petname** — optional friendly name.

Add-follow takes the external ID + optional petname; remove-follow takes the
external ID. Both go through `libs/fauna-client-bridges` action methods.

### Follow requests (designed 2026-10-01; nest, wire and shared Rust built 2026-10-04; app surface unbuilt — element IDs pending the user's rule-A approval)

A bridge whose network lets an account decide who follows it shows the requests that are waiting, beside the follows list on its card. The provider declares it — `BridgeStatus.supports_follow_requests` (a required `bool`; `false` renders no section) — and one provider does: ActivityPub, where a request exists only while the account's *accept follows by itself* setting is off. What a request is at rest, and what approving or refusing sends to the requester's server, is owned by [`activitypub.md`](activitypub.md) § Follow requests; this section owns the card surface and the wire.

- **Layout.** A *Follow requests* list on the linked card, under the follows list, rendered only for a provider that declares the capability; its heading carries the count. One row per request: the requester's address on that network, their display name when the nest knows it, **Approve** and **Refuse**. No confirm on either: an approval is undone by the requester unfollowing or by the account's own later tools, and a refused requester can ask again. An empty list renders its heading with a zero count and no rows — the list is where the user checks, so unlike a consent tray it does not vanish.
- **Wire (additive; shared Rust → nest).** `fauna.bridges.list_follow_requests { bridge_id }` → `{ requests: Vec<BridgeFollowRequest> }` with `BridgeFollowRequest { id, name: Option<String>, requested_at: Option<i64>, extra }`, `id` in the form `BridgeFollow.id` already uses and `extra` the same provider-metadata slot `BridgeFollow.extra` is, with one key shared across providers — `handle`, the requester's address as a person types it (`@user@host`), when the nest knows it; and `fauna.bridges.resolve_follow_request { bridge_id, id, approve: bool }` → `{ ok }`, idempotent — answering a request that is already gone (withdrawn, or answered from another device) succeeds. Both are `User`-class and self-scoped exactly as `list_follows` is. Neither takes the guardian `feed_sources` gate: that gate governs what an account reads, a follower is audience, and approving one grants nothing the default *accept by itself* position does not already grant. An older nest answers the unknown kind and the app renders no section; an older app never asks.
- **Where logic lives.** The list read, the two action methods and the row model join `libs/fauna-client-bridges` beside the follows methods (`BridgesClient::list_follow_requests`, `approve_follow_request`, `refuse_follow_request`; `follow_request_row` decides what a row shows — the address is `extra.handle` when the provider sent one, otherwise the `id`); apps paint rows and forward two clicks. A provider declares the capability by overriding `BridgeProvider::supports_follow_requests` and the trait's two default-refusing methods; every other provider needs no code. A resolve re-reads the list before painting (non-optimistic, like remove-follow).
- **Element IDs — proposed, NOT yet in ui.yaml (rule A).** `bridge-follow-requests-list`, `bridge-follow-request-item` (indexed), `bridge-follow-request-approve`, `bridge-follow-request-refuse` — members of the `bridge-card` component, named after `bridge-follows-list` / `bridge-follow-item` / `bridge-follow-remove`. They land in ui.yaml with the lead app's build, after the user approves them.

## Element IDs

Page-level (from ui.yaml `bridges` page): `page-heading`, `error-message`.

The page renders two ui.yaml **components**, both declared `used_in: [bridges]` — ui.yaml owns their member lists; this doc owns only the rendering rules:

- **`bridge-card`** — one card per bridge in the list, doubling as the drill-down detail. The action verb comes from the shared `BridgeLinkMode` metadata (never a per-bridge "Disconnect" string); settings rows render from `BridgeSetting` metadata (§ Bridge settings); the follows surface renders for `supports_follows` providers. The feed-subscription elements were **retired from the component** (Feed-only decision — § Layout & flow).
- **`bridge-link-form`** — the metadata-driven link form for an unlinked bridge. **The field-key derivation rule:** each field renders as `bridge-link-field-{key}`, where `{key}` is the `BridgeLinkField.key` the provider declares (Bluesky `oauth` → `bridge-link-field-handle`; ActivityPub `enable` declares no fields). The component is shared across surfaces (mail-settings, nostr) but on *this* feed-side page only the live feed-side providers' fields render. New providers contribute their own `bridge-link-field-{key}` IDs (net-new IDs need user approval + a ui.yaml addition first, per UI rule A).

## State & data shape

The wire types live in `libs/fauna-protocol/src/bridges_ui.rs` (CDDL `libs/fauna-protocol/schemas/bridges_ui.cddl`); the app renders entirely from them — no per-bridge hand-coded shapes (priority #1/#2):

- **`BridgeStatus`** `{ id, name, available, linked, identity: Option<BridgeIdentity>, mode: Option<String>, settings: Vec<BridgeSetting>, supports_follows, link_modes: Option<Vec<BridgeLinkMode>>, error: Option<String>, extra }` — one per registered provider; the list page is `fauna.bridges.list` → `Vec<BridgeStatus>`.
- **`BridgeLinkMode`** `{ mode, label, client_action: Option<String>, platform: Option<String>, fields: Vec<BridgeLinkField>, extra }` — `client_action` is the optional client-side hook keyed off the mode (`"oauth_redirect"` for Bluesky, `"nip07"` for Nostr's web extension mode); `platform` scopes a mode to one app family (e.g. `"web"` for NIP-07).
- **`BridgeLinkField`** `{ key, label, field_type, placeholder: Option<String>, extra }` — `field_type` is `"text"` or `"secret"`.
- **`BridgeSetting`** `{ key, label, setting_type, value: Value, options: Option<Vec<BridgeSettingOption>>, extra }` — `setting_type` ∈ toggle (`bool`) / text / select / number (`libs/fauna-protocol/src/bridge_search_policy.rs::SETTING_TYPE_NUMBER`, additive); `BridgeSettingOption { value, label, extra }` for select.
- **`BridgeFollow`** `{ id, petname: Option<String>, created_at: Option<i64>, extra }`.

## Where logic lives

- **All bridge ops are shared Rust → nest WS-RPC.** `libs/fauna-client-bridges` is the single client surface; every op dispatches a `fauna.bridges.*` kind into the matching server-side `BridgeProvider` trait method (`bins/fauna-nest/src/bridge_management.rs` — `id`/`name`/`available`/`link_modes`/`supports_follows`/`status`/`link`/`unlink`/`update_settings`/`list_follows`/`add_follow`/`remove_follow`). Apps never control a bridge daemon directly.
- **Provider registry (nest).** Providers register at boot, each behind its build feature: `BlueskyProvider` (`bluesky`), `ActivityPubProvider` (`activitypub`), `NostrProvider` (`nostr`) — `bins/fauna-nest/src/lib.rs`. The page is provider-agnostic: it renders whatever providers `fauna.bridges.list` returns as `available`.
- **Configuration validation** is server-side (the provider validates `LinkRequest.params` / `SetSettingsRequest.settings` and returns `fauna.bridges.invalid_params`).
- **Status** comes from the same `fauna.bridges.list` snapshot; observer-driven rendering once the shared snapshot is wired on a client.
- **Unified-page membership filter** (which `fauna.bridges.list` rows this page renders — see § Scope's Nostr + Bluesky carve-outs). Shared Rust — `fauna_client_bridges::is_unified_bridges_page_bridge(id: &str) -> bool`, which excludes both `"nostr"` and `"bluesky"` (UniFFI `is_unified_bridges_page_bridge` / wasm `isUnifiedBridgesPageBridge`, both plain-bool, ungated). `list()` itself stays unfiltered (each dedicated page's own fetch needs its own row); each app applies this predicate only when populating the unified page. Was hand-rolled identically (`id != "nostr"`) on apple/web/linux/android; web and linux swapped onto the shared predicate the same session, android followed via a same-day shared-Rust-lift consumption commit (`BridgesVM.kt`), and **apple's swap landed 2026-07-18** (`BridgeManagerVM.swift`'s `refresh()` now calls `isUnifiedBridgesPageBridge(id:)` instead of the hand-rolled filter; apple was the last of the four consumers). **windows adopted the shared predicate 2026-07-29** (`BridgesViewModel`, landed the same change as its dedicated Nostr page, `ui/nostr.md` § Implementation status today) — the 5th consumer, and all seven apps now filter Nostr off the unified page. The predicate grew to exclude `bluesky` too once Bluesky got its own dedicated page (ratified 2026-07-22, [`../ui/atproto.md`](../ui/atproto.md)); windows carried one temporary, declared exception in the interim (`BridgesViewModel.NoDedicatedPageOnWindowsYet` kept rendering the generic Bluesky card until windows had its own AT Protocol page to fall back to), and that carve-out was itself **deleted 2026-07-31**, landed in the same commit as windows' `AtprotoPage` (`ui/atproto.md` § Implementation status today). All seven apps now apply the unmodified predicate, filtering both Nostr and Bluesky off the unified page identically — no per-app exception remains.

## User actions

| Element | Action | Wire (shared Rust → nest) |
|---|---|---|
| `bridge-action-button` (unlinked) | Link via the selected mode; fill `bridge-link-field-*`. | `fauna.bridges.link` (`forbid_replay`; for `oauth_redirect` the reply `redirect_url` opens in the browser; for `nip07` the client invokes the platform extension). |
| `bridge-action-button` (linked) | Unlink. | `fauna.bridges.unlink` (idempotent). |
| settings toggle / text / select | Change a setting; **auto-save on change, 500 ms debounce** (no per-page Save button). | `fauna.bridges.set_settings`. |
| `bridge-add-follow-button` | Add follow (external ID + optional petname). | `fauna.bridges.add_follow` (`forbid_replay`). |
| `bridge-follow-remove` | Remove follow. | `fauna.bridges.remove_follow` (idempotent). |

(Bluesky-specific behaviors — OAuth callback, cross-posting `write_through`, unified feed ingestion, thread view, interactions, notifications — are the per-provider detail in § Bluesky bridge below; they consume the same unified surfaces.)

## Persistence

Bridge configurations, links, settings, and follows live **nest-side** in each provider's tables (e.g. `bluesky_accounts`, the ActivityPub/Nostr provider tables); the client holds no bridge config of its own and surfaces nest state via the snapshot. No user-irrecoverable bridge data is dropped on evolution (alpha invariant).

## Errors & edge cases

- `error-message` page-level.
- Provider errors surface on the wire: `fauna.bridges.invalid_params` (bad link/settings params), `fauna.bridges.not_found` (unknown bridge / follow id / wrong actor). A provider whose `status()` errored carries the message in `BridgeStatus.error` while staying in the list.
- An unavailable provider (feature not built, or a precondition of the deployment unmet) returns `available: false` and renders disabled, not absent.
- **An unavailable provider may say WHY, and when it can, it must** (`BridgeProvider::unavailable_reason`, surfaced as `BridgeStatus.error` on the `available: false` row). The apps need no new rule for it: `link_block_of` already renders any `error` verbatim once no link mode applies, so the nest's sentence lands in `bridge-link-blocked-reason` exactly as the `provider.status()`-errored case below. The live instance is **Bluesky on a nest with no public identity domain** — the `client_id` Bluesky's authorization server must fetch by name does not derive, so the row carries `available: false, link_modes: None, error: Some(<the missing-domain sentence, naming what would change it>)`. A provider whose unavailability is only "the feature is not built" adds nothing by saying so and returns `None`.

### A bridge that cannot be linked right now (ratified 2026-08-11)

The nest has a **degraded shape**: when `provider.status()` errors it still emits the bridge, as `available=true, linked=false, settings=[], link_modes=None, error=Some(why)` (`bins/fauna-nest/src/bridges_ui_handlers.rs`, the `Err(e)` arm). Extending *renders disabled, not absent* above to that case, three rules bind every app:

1. **Disabled, not absent, and never live-but-inert.** When no declared link mode applies on this platform, `bridge-action-button` still renders — **disabled**. An app may not hide it, and may not leave it clickable-but-inert.
2. **The reason is rendered**, in `bridge-link-blocked-reason` beside the button: the nest's `BridgeStatus.error` **verbatim** when present, else the localized `bridges.no_link_method`. The nest's sentence is the only account of what actually went wrong — it is never replaced by the generic string, and never dropped at decode.
3. **No app may drop a Link activation silently** (convention 11's rule applied to product code): a control the user can still activate either produces an RPC or a visible message.

**Where the rule lives.** `fauna_client_bridges::link_block` / `link_block_of` owns it, exported on both consumption faces (`bridge_link_block` — UniFFI + wasm). Apps supply the count of modes applying to *them* — and since 2026-08-15 the platform string-match behind that count is **also shared**: `fauna_client_bridges::mode_applies` / `applicable_modes` (exported as `bridge_mode_applies` — UniFFI + wasm, predicate-shaped so no shell maps its model types), lifted from the seven per-app copies the 2026-08-11 pass had left in place (tui held none at all, so a `platform`-scoped mode counted as applicable there). **The `platform` vocabulary is the seven canonical app names** (`linux`/`windows`/`macos`/`ios`/`android`/`web`/`tui`, ui.yaml's platform vocabulary); a mode scoped to an unknown name applies nowhere — the safe direction, since rendering a form whose `client_action` the app cannot perform is exactly the live-but-inert control this rule forbids. The `"desktop"` alias apple's copy matched is **retired**: no provider ever emitted it, and matching it left iOS answering to nothing, so a `platform: "ios"` mode would have been dropped by the very app it targets. What stays genuinely caller-local: *which name the caller is* (apple picks `macos`/`ios` at runtime), and runtime capability checks — web additionally drops a `nip07` mode when no browser extension is present, which no shared crate can observe. A **linked** bridge is never blocked: its button is Unlink, which needs no mode.

## Architectural rules

1. Unified design across all 7 apps (incl. macOS's standalone sidebar Bridges item — resolved 2026-06-28).
2. Observer-driven rendering once shared snapshot exists.

## Don't do these

- Don't ship per-app bridge UI variants without lifting to all 7.
- Don't expose direct daemon control bypassing the nest API.

## Done definition

- [x] Unified design settled and reflected in this doc (ratified 2026-06-28); ui.yaml `bridges` page wired to its `bridge-card` + `bridge-link-form` components.
- [x] Bridge enable/disable/configure all go through shared Rust → nest API (`fauna.bridges.*` → `BridgeProvider`).
- [x] Every app renders the metadata-driven link form with real `bridge-link-field-*` testids (all 7 — see § Implementation status today).
- [x] `bridge-card` feed-subscription shim elements retired on all 6 apps that had them (Feed-only decision; linux last, `views/bridges/detail.rs` — verified 2026-07-10; tui n/a — its page was built 2026-07-23, after the shims were already retired, so it never rendered them).
- [x] `tests/e2e-unified/ui-actual-<app>.yaml`'s `bridges` block refreshed; `ui-actual-lint` introduces no new errors (re-verified 2026-07-10).

## Implementation status today

**Follow requests: nest, wire and shared Rust built (2026-10-04); app surface pending.** § Follows → *Follow requests*: the `supports_follow_requests` flag, both wire kinds, the provider-trait pair and the `fauna-client-bridges` list read, action methods and row model exist and are pinned. No app renders the list, the UniFFI and wasm faces of the three client methods are not written (they land with the first app that calls them), and the four element IDs are proposed, not approved. Mechanics and the nest-side pins: [`activitypub.md`](activitypub.md) § Follow requests and § Implementation status today, gap 5.

**The Bluesky thread view renders on linux only (re-measured 2026-10-01).** § Bluesky bridge → *Bluesky-native thread view*'s closing status paragraph: android's seam is unused, five apps have no consumer, the linux surface has no element ID and no witness, and it is ruled a seven-app feature (2026-10-03): its element IDs are approved and the lift is unbuilt.

**Bridged Bluesky notifications are live (gap declared 2026-09-19, closed 2026-09-20).** § Bluesky bridge → *Notifications* is built: `bluesky::notif_worker::BlueskyNotifWorker` (`bins/fauna-nest/src/bluesky/notif_worker.rs`) is spawned beside the Nostr and ActivityPub sync workers and runs `notif_sync::poll_bluesky_notifications` for each linked account on a `NOTIF_POLL_INTERVAL_SECS` cadence (a Rust constant — nobody chooses how often a bridge polls). Two corrections the build had to make first, both of which the gap hid: the poller passed `content_id: None`, and `insert_notification` dedups on `(actor, type, sender, content)`, so the **second** Bluesky like for an actor was swallowed for ever — a bridged row now carries the notification's own AT-URI as its dedup token, which meant carrying that URI through `BlueskyNotification` (`libs/fauna-bridge-atproto`), which dropped it; and the enumeration is over *consume-side* linked accounts only, which is where § D7's hosted-backing gate ([`atproto-pds-full.md`](atproto-pds-full.md)) lives for this poller — an account backed by a nest-hosted identity reads its Bluesky account activity through service-auth proxying and is never polled here. That gate compares the actor's decoded bytes, never a hex spelling of them, and the poll site re-checks it before it starts, because nothing further in does: the OAuth session is restored by DID alone and never sees the backing. The apps' side needed nothing new, as declared: they read the unified `fauna.notifications.*` surface, and `NotifItem.source` already carried `bluesky`. Witnesses: `bluesky::notif_sync::tests` (two distinct notifications both land; a re-poll of the same window adds nothing), `bluesky::notif_worker::tests` (only consume-side-linked actors are enumerated; no non-canonically spelled link row ever is; a `both` actor is refused at the poll site itself), `bluesky::db_helpers::tests` (the link write site admits only the canonical actor spelling) and `tests/e2e-unified/tests/test_bluesky_notifications_bridged.py` (the unified wire carries `source`, and the unified list renders the bridged row). **Still unwitnessed end-to-end:** the one network step — the consume-side poller against a real Bluesky account's PDS under a real OAuth session, for which the harness has no fake (`helpers/atproto_fakes.py` fakes the PLC directory and DNS for the *hosted* PDS bridge, not the consume-side AppView).

**Consume-side feed ingestion — BUILT 2026-09-26 (declared UNBUILT earlier the same day).** § Unified feed ingestion → *Bridge ingestion* is code: `bluesky::feed_worker::BlueskyFeedWorker` polls each consume-side linked account's timeline and subscribed custom feeds, `bluesky::feed_ingest::ingest_feed_posts` stores each post through `store_post` with source `bluesky` plus its `bluesky_posts` row (`at_uri` + `cid` + `author_did`, deduped by `at_uri`), and `resolve_uri_and_cid` reads the stored CID, so the Bluesky arm of `fauna.posts.interact` is reachable for the first time — pinned headlessly (`feed_ingest.rs`: translate + store idempotent by `at_uri`; a reply parent carried by the page threads and an unknown one rests top-level; the interact door's `like` arm resolves an ingested post and fails only at the unconfigured agent, never at the map). **The harness far end — BUILT 2026-09-29.** The poller's network half runs against a canned far end through a session the OAuth client itself earned, never a seeded blob: `bins/fauna-nest/tests/conformance_bluesky_feed_ingest.rs` drives `fauna.bridges.link` → PAR → the real callback route (a DPoP-nonce challenge on the token leg) → `poll_bluesky_feeds` (timeline + a subscribed custom feed stored once, a re-poll stores nothing, a `getFeed` outage keeps the timeline). A running nest reaches the same far end through `FAUNA_TEST_ATPROTO_FAR_END` (`state.rs::build_bluesky_oauth_client`, compiled only under `test-hooks` and loopback-only — every request of the consume-side client goes to the harness's `FakeAtprotoFarEnd` with its original host in a header), and `POST /api/v1/test/bluesky/feed/poll-now` runs one pass on demand; `tests/e2e-unified/tests/test_bluesky_feed_ingest.py` is the tui journey (the post with its `protocol-badge`, a reply written through with `reply.parent` naming it). **The first test to reach the callback found it broken:** it parsed the actor out of the query `state`, which is the OAuth client's own random key, so no link could ever complete (`storage_error`); the callback now reads `{actor_hex}|{return_url}` from the app state `callback()` returns, and every exit before it redirects to `/bridges`. **Found by the same journey — RULED 2026-09-29, BUILT the same day:** a bridged post reached every app with an EMPTY list-card body, true of ActivityPub and nostr as much as Bluesky; the ruling (a `content_meta.preview` projection column written by the one index funnel, never a Search-corpus lookup) is [`../ui/feed.md`](../ui/feed.md) § The read model → *The list-card preview*, whose § Implementation status today carries the build and its pins. Two cross-bridge gaps this build inherits rather than creates: no bridge served a synthetic author's display name or avatar to the apps (ruled the same day — the next paragraph), and no app renders a `MediaItem.remote_url` — both captured as uniform lifts, never a Bluesky-only fix; the `remote_url` render was RULED 2026-09-28 ([`../architecture/render-model.md`](../architecture/render-model.md) § D6c: a `ProxiedImage` block, bearer-fetched from the reader's nest; ruling 4 above now fixes the URL form) and is BUILT on tui 2026-10-02 (the reveal posture is user-ruled 2026-09-30: it paints immediately, no reveal; the six other apps' paint trickles down — render-model.md's status table). `test_bluesky_feed_ingest.py` witnesses it: the followed post's image embed reaches tui as one `post-image` addressed by its `/api/v1/bluesky/media?url=…` path. **Found by that witness:** no app could decode ANY bridged post's body fetched by id — the nest stores a bridged post as a bare canonical `Post`, and the apps' by-id decode read only signed envelopes — so no bridged post's media, quote or deep link had ever resolved; the shared by-id decode now accepts the bare body bound to its id, rendered `Unchecked` ([`../architecture/security.md`](../architecture/security.md) § App display of unverified content owns the rule). `bluesky_saved_feeds` (`db_helpers::{save_feed,get_saved_feeds}`, no production writer) is a pre-unification duplicate of `bridge_feed_subscriptions` and feeds nothing; retiring it is a contract step for a later session.

**Bridged-author display (§ Unified feed ingestion → *Bridged authors*) — RULED and BUILT 2026-09-26, nest half + shared wire + tui leg.** In code: `db/bridge_authors.rs` + `SCHEMA_BRIDGE_AUTHORS` (newest-wins upsert, batched `get_many`); the three writers — `activitypub::db_helpers::upsert_remote_actor` (every cache write, `@user@host` + `name` + the icon behind `/api/v1/media/proxy`), the nostr sweep's `metadata_filter` (kind 0, no `since`) with `inbound_lifecycle::ingest_metadata_event` below the author gate, and `bluesky::feed_ingest` upserting each page's distinct authors before the dedupe from the lifted `IngestablePost.{author_display_name, author_avatar}`; `feed_routes::decorate_bridged_authors_best_effort` on the three local kinds (rows whose `source` is not `fauna` only); `FeedPostItem.author_display` → `PostSummary.author_display: Option<AuthorDisplayView>` (`map_post`, the UniFFI records, the `TestPostSpec` seam) → tui `feed/mod.rs::author_label` over `peer_display_label`. Pinned as the ruling lists, plus `tests/api/test_activitypub_federation.py::TestActivityPubInboundIngest::test_an_ingested_notes_author_carries_the_bridged_face` (the `FakeFollower` actor document now carries `name` + `icon`). **Open, captured:** the six-app trickle-down of the label, the `fauna.posts.get` deep-link carrier, and the avatar's first renderer, which rides the `remote_url` lane. A stranger's post carried in by a nostr kind-6 repost keeps the short-id fallback until they are followed (only followed authors are profiled).

**Reply and quote on a Bluesky post — the unified shape (§ Interactions, § Cross-posting → *A post that references a Bluesky record*): BUILT 2026-09-26, the same day it was ratified.** The door: `bluesky::interact_routes::reply_door` answers the native ack from the account tables alone and refuses with the remedy (`reply_door_tests` — ack without a mapping or a PDS; unlinked refused; a D7 hosted backing refused); the nest-mint helpers are deleted and the dispatcher passes no `body` to the arm. The derivation: `bluesky::decide_write_through` is the create leg's decision half — linked account, decode, off-box servability, the reference looked up through `bluesky_posts` + `content.source`, the mode gate the reference overrides (`crosspost_admitted`), the replay guard — and `write_through_create_inner` resolves a mapped reference at the PDS (`db_helpers::resolve_reply_refs`: the parent's current CID and its own root via `thread_root_of`; a quote through `resolve_uri_and_cid`) and hands `fauna_post_to_bsky_record` its typed `reply`/`quote` inputs (`write_through_reference_tests`: the Bluesky-origin override under mode 0 for reply and quote, the self-thread following the mode, the unmapped reference standalone, the unlinked author, the media-only reply, the mode-gate table, and the executor resolving rather than posting standalone; `db_helpers::tests` pins the root rule; the atproto crate's `outbound_test.rs` pins the record shapes). The app leg: `FeedManager::compose_referencing_post_as` routes every non-native source through ack → compose (`libs/fauna-feed`, so all 7 apps at once). **The PDS leg is witnessed since 2026-09-29:** `test_bluesky_feed_ingest.py` replies on tui to an ingested post and the harness's far end receives the `createRecord` with `reply.parent` and `reply.root` naming it (the far-end seam — the entry above).

The unified *wire + behavior* is built and shipping on all seven apps (per-app verification 2026-07-10; tui added 2026-07-23):

| Leg | web | linux | windows | macos | ios | android | tui |
|---|---|---|---|---|---|---|---|
| Generic link form + `bridge-link-field-{key}` testids | ✅ 2026-06-28 | ✅ | ✅ 2026-06-30 | ✅ 2026-06-28 | ✅ 2026-06-28 (shared `BridgeManagerVM`) | ✅ 2026-06-28 | ✅ 2026-07-23 (`apps/fauna-tui/src/bridges.rs`, generic `link_modes[].fields[]` iteration) |
| Feed-subscription shims retired | ✅ 2026-06-28 | ✅ (`views/bridges/detail.rs`) | ✅ 2026-06-29 | n/a (never rendered) | ✅ 2026-06-28 | ✅ 2026-06-28 | n/a (page built post-retirement) |
| Standalone sidebar item | ✅ | ✅ | ✅ | ✅ 2026-06-28 (`SidebarItem.bridges`) | ✅ | ✅ | ✅ 2026-07-23 (`pages.rs::Page::Bridges`, `bridges-tab`) |
| Un-linkable bridge: disabled button + `bridge-link-blocked-reason` (§ Errors & edge cases) | ✅ 2026-08-11 | ✅ 2026-08-11 | ✅ 2026-08-16 (e2e-proven on Windows; the 2026-08-11 "written" was **on the card's link DIALOG only** — see below) | ✅ 2026-08-25 (e2e-proven on macOS; see below — a driver-registration gap, not windows' dialog bug) | ⏳ (rides macos; shared `BridgeCardContent.swift`, iOS e2e run owed — simulator-build-gated) | ✅ 2026-08-11 | ✅ 2026-08-11 (lead) |
| `number`-typed `BridgeSetting` renders as an editable, committing input (the search-policy cap — § Bridge settings) | ✅ 2026-08-17 (`BridgeCard.svelte`, commit-on-blur via the shared `parseCountI64`; also fixed an unrelated bug — an unrecognized `setting_type` was silently dropping the row, now a read-only fallback) | ✅ 2026-08-17 (`views/bridges/detail.rs`; the whole metadata-driven settings loop — bool/select/text/number — was built fresh, since the generic renderer only ever painted a static "no settings" placeholder before this) | ✅ 2026-09-09 (`Controls/BridgeCard.xaml.cs`; the whole metadata-driven settings loop — bool/text/number, plus a read-only fallback — was built fresh, since the card only ever rendered name/identity/follows before this; commit-on-blur-and-Enter via the shared `ParseCountI64`, matching the `mail-spam-threshold-override-input` idiom; `BridgeSetting`/`BridgeSettingValue` gained a `NumberValue` field) | ✅ 2026-08-25 (`BridgeCardContent.swift`'s shared `BridgeNumberSettingRow` — one FaunaKit view covers both apple targets; commit-on-submit via the shared `parseCountI64`, matching android's commit-on-IME-Done) | ✅ 2026-08-25 (shares `BridgeCardContent.swift` with macOS) | ✅ 2026-08-17 (`BridgesScreen.kt::SettingRow`, commit-on-IME-Done, `FfiCborValue.Integer`) | ✅ 2026-08-17 (lead; `input_commit`, the `folder-member-cap-input` idiom) |

**The 2026-08-11 pass, and what it found.** All seven apps met the degraded shape differently — six distinct behaviors, and **0 of 7 rendered the nest's `error`**: web/macos/ios offered a live button that returned early (a silent no-op); tui sent `fauna.bridges.link` with an empty `mode` and let the nest refuse (loud, but it discarded the specific explanation for a generic round-trip error); linux opened a dialog whose submit was dead with no reason; windows displayed **`bridges/no_config_needed` — "No configuration needed."**, the exact inverse of the truth; and android's `?: return` rendered **nothing at all** — no button, no message. Two apps dropped `error` at decode, not just at render: windows' `BridgeInfo` had no `Error` member, and apple's hand-written `BridgeInfo` had no `error` field and its `BridgeFFIMapping` silently discarded `ffi.error` (the second instance was not previously known). The five per-platform i18n strings for one fact (`no_link_methods`, `no_link_methods_desktop`, `no_link_methods_ios`, `no_link_modes_android`, `no_config_needed` — the base one literally read *"for web"*) collapsed into a single `bridges.no_link_method`.

**The windows leg, closed 2026-08-16 — and why "written" was not "built".** The 2026-08-11 pass
recorded windows as *written, build+run owed*. The first e2e run against it failed with
`bridge-link-blocked-reason` at `count=0`, and the cause was neither a test-ordering problem nor a
missing string: windows rendered the reason **only inside the link `ContentDialog`**
(`BridgeCardActions.BuildLinkDialogContent`), never on the card, and its `ActionButton` gated on
`BridgeInfo.Linked` alone, so it never disabled. Those two combine into the trap this section
exists to forbid: a live-looking Link button whose only explanation sits behind a dialog that a
correctly-disabled button would never open. The fix moved the id to the card
(`Controls/BridgeCard.xaml`), drove both the reason and `IsEnabled` off the shared
`fauna_client_bridges::link_block` rule via the existing UniFFI face, and dropped the id from the
dialog copy so no duplicate id can exist in the UIA tree.

**The apple leg, closed 2026-08-25 — a different bug from windows', not the same one.** Both
`test_activitypub_link_blocked_*` tests failed at `count=0` on macOS, but apple's render was
already correct (`FaunaKit/Views/BridgeCardContent.swift`): the reason renders inline on the card
(no dialog to hide it in), and the action button already `.disabled(!bridge.linked && linkBlock !=
nil)`, driven by the same shared `link_block` rule. The bug was **driver-observability, not
render**: (1) `bridge-link-blocked-reason` carried only a bare `.accessibilityIdentifier` —
invisible to the in-process `AutomationRegistry` the macOS/iOS drivers actually read (confirmed via
the dev-fleet automation-registration checker's `--list` mode, which reported the id as
registration-baselined, i.e. a *known*-unregistered site); (2) `bridge-action-button`'s
`.automationActivate` passed no `isEnabled:`, and the real `.disabled` sits *inside* that modifier,
which `AutomationRegistry`'s ancestors-only folding never sees — so `/element/enabled` answered
`true` regardless, meaning the in-process driver's own strict-actuation gate would have let a click
through on a live-but-inert Link button, exactly what rule 3 above forbids. Fixed by swapping the
reason's `Text(...).accessibilityIdentifier(...)` for `automationText(...)` (registers
`.automationValue` too) and adding `isEnabled: { bridge.linked || linkBlock == nil }` to the
button's `.automationActivate`, mirroring its `.disabled` predicate exactly. One fix, shared
`BridgeCardContent.swift` — covers both apple targets; e2e-proven on macOS only this pass (iOS's
own run is simulator-build-gated, tracked with the rest of the apple `--app ios` backlog).

**Where this section's coverage lives — and the premise it refuted.** The degraded shape was
believed **unassertable end-to-end**: every registered provider's `status()` errors only on a
genuine DB fault, and every declared `BridgeLinkMode` today has `platform: None`, so a running
nest never organically reaches either blocked shape and no fixture could inject one. That premise
was **wrong, and the seam is cheap** — `bins/fauna-nest/src/bridge_status_test_hook.rs`
(`test-hooks` feature, absent from production builds per convention 15) exposes
`POST /api/v1/test/bridges/{id}/status-override` with `error` / `no_applicable_modes`, forcing
`list_handler`'s override branch into exactly the two shapes `fauna_client_bridges::link_block`
recognizes. The consumers are two **tier_3, app-agnostic** journeys —
`tests/e2e-unified/tests/test_bridges.py::test_activitypub_link_blocked_shows_the_nests_own_reason`
(rule 2's verbatim arm) and `::test_activitypub_link_blocked_falls_back_to_the_generic_reason`
(its `bridges.no_link_method` arm) — each asserting rule 1's *rendered-but-disabled* button in the
same pass. They run on whatever `--app` set is selected, so a per-app leg is a **run**, not new
test code. ⚠ Install the override **before login**: login is what triggers the one-shot initial
`fauna.bridges.list` fetch on native apps, and linux never re-fetches on a bare nav to Bridges.
Green: tui/linux/web 2026-08-15, windows 2026-08-16 (where it caught the two-part product bug
above), macos 2026-08-25 (where it caught the driver-registration gap above — see *The apple leg*).
Owed: ios (rides macos's shared `BridgeCardContent.swift` fix; its own e2e run is
simulator-build-gated) and android (its e2e is emulator-host-gated).

tui renders every bridge inline (the web/apple shape, not linux's list→detail split), keys each card `.within("bridge-card", i)` for scoped queries, and consumes the shared `fauna_client_bridges::{BridgesClient, is_unified_bridges_page_bridge}` directly (no FFI hop) — the second direct-Rust app after linux. Metadata-driven setting rows and the follow-add id/petname inputs render untagged (ui.yaml scopes them no id), matching every app (the `bridge-card` component lists only `bridge-action-button` + the follows elements).

Load-bearing render constraints that survive the matrix: no app invents
per-bridge render logic (the generic `link_modes[].fields[]` iteration is the
only form path; windows filters by `BridgeLinkForm.ModesForPlatform` so a
`platform`-scoped mode like Nostr's `"web"` NIP-07 drops); both apple apps
open the oauth `redirect_url` via the shared `OpenURL` helper; the Account-page
"Connected Services" Bluesky link is a separate blessed surface (`settings.md`
§ Account). **ui.yaml reconciliation DONE 2026-07-17:** the real currently-shipping
feed-side field keys are `handle` (Bluesky oauth), `nsec` (Nostr import), and
`bunker_url` (Nostr remote/NIP-46) — verified directly against each provider's
`link_modes()` (`bins/fauna-nest/src/{bluesky,nostr,activitypub}/bridge_provider.rs`).
The previously-cited `instance`/`relay_list` keys never existed as link fields:
ActivityPub's real mode is `enable` with no fields (not an `instance-url` field),
and `relay_list` is a `BridgeSetting` on `bridge-card`, not a `bridge-link-form`
field. `ui.yaml` § `bridge-link-form` now lists exactly the three real IDs.

**Provider availability.** Bluesky, Nostr, and ActivityPub all ship in the
production Docker image (`Dockerfile` builds `--features
bluesky,nostr,activitypub` — Nostr and
ActivityPub both landed 2026-07-16, [`../architecture/installers/docker.md`](../architecture/installers/docker.md)
§ Feature flags). The page is provider-agnostic, so shipping a provider needs
no client change; shipping ActivityPub mounts its routes but federates
nothing by itself — only per-actor enablement (`ap_accounts.enabled`,
client-only) exposes any content, so the blast radius stays per-account until
a user opts in ([`activitypub.md`](activitypub.md) § Implementation status
today). AP bridge *behavior* — actor serving, inbound pipeline, delivery, the
produce direction — is owned by [`activitypub.md`](activitypub.md), and so
is the Fediverse naming rule (its § Naming: the Fediverse rail; the
conversations rail it named was retired 2026-10-02 onto `Rail::Bridged`).

**Bluesky's availability is DERIVED from the deployment's own identity domain
(landed 2026-09-02).** All three providers ship and register.
`BlueskyProvider::available` is `AppState::bluesky_oauth().is_some()`, and that
client is built — on demand, cached under the URL it was built for — from
`https://<identity-domain>`, the domain the box learns at **claim**. So a nest
claimed onto a real domain offers the Bluesky bridge with **no flag, no env and
no config file**, on the shipped image's own launch line, and a later domain
change re-mints the `client_id` rather than serving one Bluesky's authorization
server can no longer fetch.

This replaced a `--bluesky-public-url` CLI flag that was the *only* writer of
the client and that **no shipped launch path passed** — neither the image's
`docker/s6/fauna-nest/run` nor `bins/fauna-nest/install.sh`'s `ExecStart` — so
the bridge had been dark on every real deployment while
[`../../guides/bridges-bluesky-nostr.md`](../../guides/bridges-bluesky-nostr.md)
§ Bluesky, today told users the OAuth account link "ships now". The flag was
also the banned operator tier of [`../principles.md`](../principles.md) § One
configuration surface — a nest's own public URL is nobody's *choice* — and got
the same disposal `--push-relay-url` did
([`../architecture/nest/common.md`](../architecture/nest/common.md) § Bluesky /
Web Push). The unread `[bluesky] enabled` config section went with it.

**A box with no public identity domain reports the bridge unavailable, with a
reason** — see § Errors & edge cases. That is the honest answer rather than a
degraded one: an OAuth round requires Bluesky's authorization server to resolve
and fetch this nest's client-metadata document **by name over public HTTPS**, so
a domainless / `localhost` / `.local` / IP-literal box genuinely cannot complete
one. Witnessed against the artifact by
`tests/e2e-unified/tests/platform/docker/test_bridge_availability_in_image.py`,
which boots the shipped image twice: a domainless claim (unavailable, reason
rendered) and a domained one (available, OAuth mode offered), with ActivityPub
on the same box as the control.

## Bluesky bridge — current behavior

> **Two directions, do not conflate.** Everything in this section is the **consume** direction: the user OAuth-links their *existing external* Bluesky account, reads feeds/DMs, and optionally cross-posts (`write_through`) by writing to **the user's external DID repo** via XRPC. A separate **host** direction — Fauna's nest hosting an ATProto PDS that mints the DID and holds the repo *locally* — lives in [`atproto-pds-bridge.md`](atproto-pds-bridge.md) and is largely built (the content-mirror chain; the login-capable full PDS is landing in parallel, [`atproto-pds-full.md`](atproto-pds-full.md)) — current build state in each doc's § Implementation status today. They are **alternative identity backings** for one user's ATProto presence (external PDS vs. Fauna-hosted PDS); the read/interact surfaces below are shared plumbing, but with a host backing the `createRecord`/`write_through` writes target the local repo instead of an external XRPC call.

The Bluesky bridge is the most-developed bridge today. Per
the bridges client-architecture design (2026-05-04, tracked internally),
its account / settings / follows surfaces fold into the unified
metadata-driven shape; the Bluesky-specific UX claims about a
standalone "Bluesky tab" or first-class "Bluesky Timeline" view are
explicitly dropped. The endpoints described here remain — they're the
nest API surface the unified bridge UI consumes for this provider.

### Linking a Bluesky account (OAuth)

The bridge management API is protocol-agnostic and the entry point.

**1. List available bridges:**

Wire: WS-RPC kind `fauna.bridges.list` (request `ListBridgesRequest{}`,
reply `ListBridgesReply { bridges: [BridgeStatus] }`; types in
`libs/fauna-protocol/src/bridges_ui.rs`, CDDL at
`libs/fauna-protocol/schemas/bridges_ui.cddl`). The HTTP twin
`GET /api/v1/bridges` was deleted in the T9+T10 sweep
(tracked internally); the wire surface is
WS-RPC-only.

Returns all bridges including Bluesky with `available`, `linked`,
`identity`, `mode`, `settings`, and `link_modes` fields. When
`linked: false` and Bluesky OAuth is configured on the nest,
`link_modes` contains a single entry with `mode: "oauth"` and
`client_action: "oauth_redirect"`.

**2. Initiate OAuth:**

Wire: WS-RPC kind `fauna.bridges.link` (request
`LinkRequest { bridge_id, mode, params: fauna_cbor::Value }`, reply
`LinkReply { linked, identity, redirect_url }`; types in
`libs/fauna-protocol/src/bridges_ui.rs`, CDDL at
`libs/fauna-protocol/schemas/bridges_ui.cddl`). `forbid_replay=true` —
the auto-retry path won't re-issue the call, so the client must
explicitly re-call on disconnect (a double-invocation can corrupt the
upstream pending-nonce state). The HTTP twin `POST /api/v1/bridges/{id}/link`
was deleted in the T9+T10 sweep
(tracked internally).

Apps dispatch the kind with `mode: "oauth"` and a `fauna_cbor::Value::Map`
of per-mode params (`{"handle": "yourname.bsky.social"}` for Bluesky
OAuth). Wire bytes are canonical dag-cbor on the WS-RPC frame per
`docs/goal/architecture/transport.md` § Wire format. The reply carries
`{ redirect_url: "https://bsky.social/oauth/authorize?..." }`, which
the client opens in the user's browser. (The OAuth *callback* below
stays HTTP — it's an OAuth 2.0 spec surface on the HTTP residue list,
not nest↔client traffic.)

**2a. Proof-of-possession challenge (external-signer modes).** Wire: WS-RPC
kind `fauna.bridges.link_challenge` (request `LinkChallengeRequest { bridge_id,
mode }`, reply `LinkChallengeReply { challenge, expires_at, payload:
fauna_cbor::Value }`; same files as `link`). Replay-safe, 5 s: a re-issue mints
a fresh nonce that supersedes the last. `payload` is provider-shaped like
`LinkRequest.params` — what the external signer must sign — and only a (bridge,
mode) whose identity is held outside the nest issues one; every other refuses
(`invalid_params` for a known mode, `invalid_mode` otherwise). The `link` that
follows carries the signed object back inside `params` and is refused
`fauna.bridges.proof_required` without it — the typed, surfaced answer to an
app older than the challenge, never a silent link. The one user today is Nostr
`nip07` (`window.nostr.signEvent` over an unsigned kind-22242 event; `link`
params `{pubkey, proof_json}`); the semantics — what is proven, how, and the
`remote` mode's nest-side twin — are [`../ui/nostr.md`](../ui/nostr.md) § Errors
& edge cases → *Proof of possession*. The `BridgeProvider` trait method is
`link_challenge` (default: refuses).

**3. Callback:**

Bluesky redirects back to
`GET /api/v1/bluesky/auth/callback?code=...&state=...`. Nest exchanges
the code for DPoP session tokens, fetches the user's Bluesky profile
via `getProfile`, and stores the link in the `bluesky_accounts` table
(DID, handle, session metadata). Browser is redirected to
`/bridges?bridge=bluesky&result=linked`.

**4. Unlink:**

Wire: WS-RPC kind `fauna.bridges.unlink` (request
`UnlinkRequest { bridge_id }`, reply `UnlinkReply { ok: true }`).
Idempotent (already-unlinked is a no-op); replay-safe. The HTTP twin
`DELETE /api/v1/bridges/{id}/link` was deleted in the T9+T10 sweep
(tracked internally).

The lower-level Bluesky-specific auth twins (`POST /api/v1/bluesky/auth/start`,
`GET /api/v1/bluesky/auth/status`, `DELETE /api/v1/bluesky/auth`) and the
`GET /api/v1/bluesky/profile` twin were **deleted**
(2026-06-05; profile in Commit A, the auth
trio in Commit C — tracked internally). They duplicated
`fauna.bridges.{link,list,unlink}` exactly — each dispatched into the same
`BlueskyProvider` trait methods, and the linked identity is read from
`fauna.bridges.list`. Apps link / read status / unlink Bluesky through the
unified bridge surface above; **linux is done** (2026-06-06 —
the legacy account-page Bluesky-OAuth row that drove the deleted `auth/start` /
`auth/status` twins was removed; linux links Bluesky on the unified Bridges page
via `fauna.bridges.link`, whose reply `redirect_url` the client now opens in the
browser), and **apple is done too** (macOS via the 2026-06-28 ratification legs,
iOS via the 2026-06-28 Leg 2b unification onto the shared `BridgeManagerVM` — both
link Bluesky through the unified Bridges page and open `redirect_url` in the
browser via the shared `OpenURL` helper). The only
Bluesky-specific HTTP that
survives is the OAuth residue — `GET /api/v1/bluesky/auth/callback` +
`GET /.well-known/atproto-oauth-client` (far end is Bluesky's OAuth server) —
and the `media` CDN proxy (byte-bulk). The unified link path still restores the
user's `fauna_bridge_atproto::oauth::Agent` from the stored `bluesky_accounts`
session and makes its XRPC calls on the user's behalf; DPoP tokens are handled
transparently by the agent.

### Generic bridge management ops

These are protocol-agnostic and the rest of the bridge surface (link,
unlink, settings, follows) shares the same WS-RPC shape across bridges
— each kind dispatches into the matching `BridgeProvider` trait method.

**Update settings.**

Wire: WS-RPC kind `fauna.bridges.set_settings` (request
`SetSettingsRequest { bridge_id, settings: fauna_cbor::Value }`, reply
`SetSettingsReply { ok: true }`; types in
`libs/fauna-protocol/src/bridges_ui.rs`, CDDL at
`libs/fauna-protocol/schemas/bridges_ui.cddl`). Idempotent
overwrite of the per-bridge settings blob the
`BridgeProvider::update_settings` trait method consumes (typed
`fauna_cbor::Value` end-to-end since T9+T10). The HTTP twin
`PUT /api/v1/bridges/{id}/settings` was deleted in the same sweep
(tracked internally).

**List follows.**

Wire: WS-RPC kind `fauna.bridges.list_follows` (request
`ListFollowsRequest { bridge_id }`, reply
`ListFollowsReply { follows: [BridgeFollow] }`). Pure read; replay-safe.
The wire `BridgeFollow` carries `id` / `petname` / `created_at` plus an
`extra` slot (`null` when absent) so the CBOR map's key set stays stable
across bridges. The HTTP twin `GET /api/v1/bridges/{id}/follows` was
deleted in the T9+T10 sweep
(tracked internally).

**Add follow.**

Wire: WS-RPC kind `fauna.bridges.add_follow` (request
`AddFollowRequest { bridge_id, id, petname, extra }`, reply
`AddFollowReply { ok: true }`). `forbid_replay=true` — the
duplicate-follow constraint is server-enforced, and the auto-retry
path would surface the conflict as a spurious error; the caller must
explicitly re-issue on disconnect. `extra` is `fauna_cbor::Value` on the wire
and rides through the `BridgeProvider::add_follow` trait method
typed end-to-end (no JSON shimmer since T9+T10). The HTTP twin
`POST /api/v1/bridges/{id}/follows` was deleted in the same sweep
(tracked internally).

**Remove follow.**

Wire: WS-RPC kind `fauna.bridges.remove_follow` (request
`RemoveFollowRequest { bridge_id, follow_id }`, reply
`RemoveFollowReply { ok: true }`). Idempotent (already-removed is a
no-op); replay-safe. The HTTP twin
`DELETE /api/v1/bridges/{id}/follows/{follow_id}` was deleted in the
T9+T10 sweep (tracked internally).

### Unified feed ingestion

Bluesky posts flow into the unified feed through two paths.

**Bridge ingestion — the consume-side poller (ruled and built 2026-09-26).** `bluesky::feed_worker::BlueskyFeedWorker` (`bins/fauna-nest/src/bluesky/feed_worker.rs`) is spawned beside `notif_worker` and, on a `FEED_POLL_INTERVAL_SECS` cadence (a Rust constant — nobody chooses how often a bridge polls, [`../principles.md`](../principles.md) § One configuration surface), runs `poll_bluesky_feeds` for every consume-side linked account: one `app.bsky.feed.getTimeline` page per account plus one `app.bsky.feed.getFeed` page per `bridge_feed_subscriptions` row the account holds for this bridge. The translation is shared Rust (`fauna_bridge_atproto::ingest`); the network half and the ingest half are split exactly as the notification poller splits them (`feed_worker::poll_bluesky_feeds` owns the XRPC calls, `feed_ingest::ingest_feed_posts` owns every decision about what came back), so the ingest half is pinned against a bare database. Every post a page carries is stored as a `content` row with source `bluesky` through `segments::post::store_post` — the door the ActivityPub inbox uses: body in the author's `__post` segment, the feed-index projection (`content_meta`, FTS, links) beside it — plus its `bluesky_posts` row, so it reads through `fauna.feed.*` like any other post and is resolvable as an interaction and reply target (§ Interactions, § Cross-posting). The rulings, each unified with the ActivityPub and nostr inbound precedents rather than a third shape:

1. **The remote author is a synthetic actor keyed on the DID.** `fauna_bridge_atproto::ingest::synthetic_actor_id(did)` derives the 32-byte `ActorId` as `blake3::derive_key("fauna-bluesky-synthetic", did)` — the ActivityPub shape (`fauna_bridge_activitypub::identity::synthetic_actor_id` over the actor URI) with this bridge's own domain constant; nostr derives the same way over the pubkey. The DID, never the handle: a handle is reassignable, the DID is the identity. The three constructions stay per-bridge on purpose — one shared helper would either change nostr's at-rest author ids or carry three domain tags for one line of code.
2. **The map row is the post's identity on this side; the residue columns are inert.** Ingestion writes `bluesky_posts (fauna_post_id, at_uri, cid, author_did)` per post with `INSERT OR IGNORE` on the table's `UNIQUE(at_uri)` — which is the dedupe: a re-poll of the same window, or a second linked account whose timeline carries the same post, adds nothing (the notification poller's pin, mirrored). `cid` is written by both map writers (2026-09-26): `resolve_uri_and_cid` returns the stored pair without a network round trip — the write-through stores the `createRecord` reply's CID the same way (the `getRecord` fallback for a row without one went with the compat-remnant sweep, 2026-09-30). The one caller that still fetches is the reply derivation's `resolve_record` (§ Cross-posting): it needs the parent *record* for the thread root, which nothing stores, and one `getRecord` serves the root and the CID alike. **CID freshness:** the stored CID is the record's CID as the nest saw it. A strong ref built from it names that revision, and that is the only revision a Bluesky post has in practice — the apps that mint posts never rewrite them and the write-through never `putRecord`s — so no door re-fetches it; a record rewritten upstream would leave the like or reply pointing at its earlier revision, which the AppView still resolves by URI. **Retention:** an ingested row persists exactly like an AP or nostr bridged post — no expiry, no cache sweep. `content_json` (written empty), `cached_at` (the write instant) and `expires_at` (written 0, never read) are the residue of the cache design this table was first built for; they stay as columns until an expand→migrate→contract step retires them ([`../principles.md`](../principles.md) § No user-data loss — the additive reconciler refuses a drop), and no reader may give them meaning.
3. **References resolve through the map, or the post rests top-level — never a synthetic id** ([`../ui/nostr.md`](../ui/nostr.md) § Replying to and quoting a nostr note's rule, applied here). A page item's `reply.parent` view and its quoted `ViewRecord` are ingested BEFORE the post that references them, so one-level threads and quotes resolve within the same pass; a reference becomes `Reference::Reply`/`Reference::Quote` on the local content id only when the target's `at_uri` maps to a local row, else the post is stored without it. The thread root is not stored: the produce direction derives it from the parent record (§ Cross-posting). A repost item enters as the reposted post itself, once, under its author — the repost relation is not carried.
4. **Media rides as `remote_url` through the bridge's privacy proxy — and a `remote_url` is ALWAYS a nest-relative, already-proxied path, at rest and on the wire (the avatar's rule below, applied to post media; ruled 2026-09-28, BUILT 2026-10-02).** Each image of the view's embed becomes a `MediaItem` with `remote_url = rewrite_media_url(fullsize)` (`/api/v1/bluesky/media?url=…`, the rewrite the thread view applies), a zero `blob_hash`, and the declared aspect ratio — the ActivityPub shape (`ap_note_to_fauna_post`) with the CDN kept behind the proxy. The ActivityPub and nostr twin is `shared_media_proxy_url` (`/api/v1/media/proxy?url=…`, https origins only — a non-https attachment yields no item, as it yields no avatar), which **moves from the nest (`db/bridge_authors.rs`) into shared Rust, `fauna_core` beside `MediaItem`**, so the one function serves the nest's avatar upsert, `ap_note_to_fauna_post` at ingest (which today stores the raw attachment URL) and the client-side render fold's defensive arm alike: an absolute `https://` `remote_url` the fold still meets is rewritten by that same function, so no app ever sees, let alone dials, the remote origin. The apps do not read `remote_url` themselves: the shared feed fold turns it into `RenderBlock::ProxiedImage { path }` (a `video/*` item: `ProxiedVideo`, ruled 2026-10-02 — the same doc), fetched from the reader's own nest with the session bearer exactly like `/api/v1/blob/<hash>` — the block shape, the fetch rule, the `media_hash` guard and the reveal posture (user-ruled 2026-09-30: paints immediately) are [`../architecture/render-model.md`](../architecture/render-model.md) § D6c's. Video and link cards are not carried; the text and its facets are. A self-labelled post carries its first label as `content_warning`. **Each bridged item carries its attachment's own description as `MediaItem.alt` (ruled 2026-10-02, BUILT 2026-10-03):** `alt: Option<String>` is an additive field (`#[serde(default, skip_serializing_if = "Option::is_none")]` — omitted from the encoding when `None`, so an item without one encodes byte-for-byte as before and an older peer ignores the key; [`../architecture/version-compatibility.md`](../architecture/version-compatibility.md) § Dimension 2), written by exactly two writers — `build_fauna_post` from the embed image's `alt` and `ap_note_to_fauna_post` from the attachment's AS2 `name` — through the one shared `fauna_core::data::media_alt` (absent, empty or whitespace-only rests as `None`, never `Some("")`). A native upload leaves it `None`: no compose-time per-item alt input exists (it would be a new element — `ui.yaml` rule A). **It is independent of the post-level `PostBody::Media.alt_text`**, which is the *body text* of a media-only post (what `PostBody::text()` returns, and what the Bluesky projection puts on the first image): neither is derived from the other, the feed fold reads only the per-item field (no fallback to `alt_text`), and `alt_text` is untouched. Not yet carried, both follow-ons: the outbound directions (the Bluesky projection's per-image `alt` and the ActivityPub attachment `name` still ignore `MediaItem.alt`), and an image a third-party client writes through the PDS (`reverse_translate`'s media arm drops the record's per-image `alt`).
5. **The poller honours D7 where it starts.** It enumerates `list_consume_side_linked_actors`, and `poll_bluesky_feeds` re-checks `consume_side_poll_allowed` before its first network step, exactly as the notification poller does — a hosted-backed account never polls consume-side ([`atproto-pds-full.md`](atproto-pds-full.md) § D7). No timeline cursor is persisted: `getTimeline`'s cursor pages into the past, not forward from a mark, so each tick reads the head page and the map's dedupe discards what the nest already holds.
6. **The ward is spent at the door, not at the poll.** `feed_sources` ([`family-safety.md`](family-safety.md) § Feed-source approvals) gates *adding* a source — `link` when the account is linked, `feed` when a custom feed is subscribed — and the poller owes it nothing: a supervised ward's timeline is the consequence of an approved link, as an approved fediverse follow's Notes are.
7. **The Search corpus is fed at this transit point** ([`content-index.md`](content-index.md) § Bridge content in the Search corpus): each stored post is indexed under `bridge.bluesky` : `at_uri` through the same `index_bridge_content` the other two bridges call, so the policy stays one policy.

Every door that writes a remote-asserted `created_at` is bounded ([`../ui/feed.md`](../ui/feed.md) § The read model): the ingest refuses a post dated past the bridged cushion, silently, before any write.

**Bridged authors — one projection, one wire shape (ruled 2026-09-26; build status → § Implementation status today).** Every bridged post rests under a synthetic `ActorId` (ruling 1 and its ActivityPub and nostr twins) that no `Profile` will ever be signed for — a synthetic actor has no key — so `fauna.profile.get` answers `not_found` for one exactly as for a never-published native actor, and every app painted the raw hex. The face already lives in one place per bridge: the AP inbox's `ap_remote_actors` cache (`preferredUsername`, `name`, `icon`), the author's nostr kind-0 metadata event, and the `author` view (handle, `displayName`, `avatar`) on every Bluesky post the poller ingests. Three rulings make it one mechanism, not three:

1. **One nest-side projection, `bridge_authors`** (`db/schema.rs` beside the other bridge-generic tables; module `db/bridge_authors.rs`): `(actor_id BLOB PRIMARY KEY — the synthetic id, bridge, external_id, handle, display_name, avatar_url, updated_at)`, one row per synthetic actor, **newest `updated_at` wins** on upsert — a writer whose source carries its own timestamp (a kind-0 event's `created_at`) passes it, so a lagging relay's stale profile never regresses a fresher one; a writer without one (an AP actor fetch, a AT Protocol page) passes the write instant. Derived, never user-authored: every row is re-derivable from the bridge's next transit, so the table is recreatable and is not user data ([`../principles.md`](../principles.md) § No user-data loss). `handle` is the bridge's own user-facing handle — AP `@preferredUsername@host` (the host from the actor URI, the spelling the `Create`-push's `Mention` already names), nostr the `nip05` address when the event carries one else its `name`, Bluesky the handle; `display_name` is AP `name`, nostr `display_name`, Bluesky `displayName`; `avatar_url` is stored **already rewritten through the bridge's own privacy proxy** (Bluesky `rewrite_media_url`, AP and nostr the shared `/api/v1/media/proxy` — ruling 4's lane), so the apps see one shape, a nest-relative URL fetched exactly like a `MediaItem.remote_url`, and never the remote origin.
2. **Three writers, one per bridge, each at the transit point where that bridge already learns the facts — and no fourth path.** *ActivityPub:* `activitypub::db_helpers::upsert_remote_actor` projects on every cache write (the inbox's fetch-and-cache, `Update{Person}`, the push and interact resolves) — one site, every caller, so a renamed fediverse account refreshes on its next activity. *Nostr:* the inbound sweep's per-relay subscription gains a **kind-0 filter** beside `post_filter` — the same followed `authors`, `kinds: [0]`, and **no `since`**: kind 0 is replaceable, the relay serves each author's *current* one, and the posts' catch-up cursor would skip a profile older than itself (the freshness-window trap `process_inbound_event` documents); `process_inbound_event` gains a kind-0 arm **after the author gate** (kind 0 rides the author-constrained filter, so a stranger's is dropped exactly like a stranger's note) that parses the NIP-01 metadata JSON and upserts — never stored as an event, never translated, never advancing the posts cursor. Only followed authors are profiled: a stranger's post carried in by a kind-6 repost keeps the short-id fallback until they are followed. *Bluesky:* `IngestablePost` carries the view author's `display_name` and (proxied) `avatar` beside its handle, and `ingest_feed_posts` upserts once per distinct author per page **before** the per-post dedupe — a re-poll of an already-held window still refreshes the face.
3. **How it reaches the apps: an additive `FeedPostItem.author_display: Option<AuthorDisplay { handle, display_name, avatar_url }>`, filled by the nest after the page is queried — never a JOIN in `query_feed`.** Every local feed kind (`fauna.feed.posts`, `.local.posts`, `.trending.posts`) decorates its page where `augment_viewer_state_best_effort` already post-processes it: one `SELECT … WHERE actor_id IN (<the page's distinct authors>)` per page, best-effort like the viewer state (a read failure serves the feed faceless, never fails a query that succeeded); [`../ui/feed.md`](../ui/feed.md) § The read model's index-only `SELECT` is untouched. A native author has no row and reads `None` — and the field is shaped so a later native profile projection can fill the same field, never a second one. **Why not the `fauna.profile.get` fall-through the row first weighed:** that reply is the verbatim *signed* `Profile` bytes and its decoder is strict signed-only ([`../ui/profile.md`](../ui/profile.md) § Where logic lives — the bare fallback went with the 2026-09-24 sweep), so serving a synthetic actor would mean the nest minting unsigned profile bytes: the trust class profile.md refuses outright (*a nest-written profile would be unsigned, inventing a trust class § Encryption at rest does not sanction*) and bytes every existing client would fail to decode — an error where today there is a clean miss; and profile.md § Persistence forbids client-side profile caching, so the fall-through is one round trip per author per page from every app, against one batched read here. `author_display` is **bridge-asserted, nest-relayed** — the trust class of the row's `source` badge, not of a signed profile; the apps hand its `display_name` and `handle` to `peer_display_label` ([`value-formatting.md`](value-formatting.md) § Peer display label) in the self-published-name slot, so the viewer's own nickname still wins and the chain otherwise reads display name → handle → short id for every author alike. `PostSummary.author_display` mirrors it 1:1 (`map_post`), the UniFFI records mirror it as additive consume-safe fields, and tui — the lead app — renders it in `feed/mod.rs::author_label` (card and detail); the other six follow in the trickle-down (web's card still paints `shortActor`). The avatar has no tui consumer (a terminal) and shares the unbuilt `remote_url` lane above.

Pins for the ruling: `fauna-protocol` (`AuthorDisplay` present/omitted round-trip), `fauna-bridge-atproto` (the lifted author fields), nest unit pins at each transit point (AP `upsert_remote_actor` → row; the nostr kind-0 parse-and-upsert helper, followed author vs stranger; `ingest_feed_posts` → row) and on the decoration (a bridged row decorated, a native row `None`), and one tier_3 over `fauna.feed.local.posts` after a `FakeFollower` `Create{Note}` — its actor document carries `preferredUsername`/`name`/`icon` — in `tests/api/test_activitypub_federation.py`'s `TestActivityPubInboundIngest`.

**Custom feed subscriptions.** Users subscribe to a Bluesky custom
feed to import additional content into the unified feed. Wire: WS-RPC
kinds `fauna.bridges.feeds.{list,create,delete}` (HTTP twins
`/api/v1/bridge-feeds*` deleted in T9+T10). All three are replay-safe
at 5 s — `feeds.create` is idempotent because the DB row carries
`UNIQUE(actor_id, bridge, feed_uri)` and the handler uses
`INSERT OR IGNORE`, so duplicate-create returns the same server-assigned
id. `feeds.delete` returns `fauna.bridges.not_found` for unknown-id and
wrong-actor (same shape the BridgeProvider-trait kinds use).

Bridge feed subscription **stays on the Feed page** (where the
`bridge-form-*` elements live today) — ratified 2026-06-28 (user); see
§"Layout & flow". The `fauna.bridges.feeds.{list,create,delete}` surface
is unchanged; only its UI home is fixed to Feed.

### Bluesky-native thread view

The unified feed is the primary content surface. The standalone
timeline / custom-feed / author-feed views were **dropped** (above) and
their endpoints removed (`GET /api/v1/bluesky/{feed/timeline,feed,
feed/author/{did}}`; 2026-06-05) —
they had no app consumer. The one surviving protocol-unique view is
the **thread**: a post's full reply context in Bluesky's own thread
structure, which the linux and android post-detail surfaces show for a
crossposted post. It restores the user's Bluesky OAuth agent, calls
`app.bsky.feed.getPostThread`, and translates the ATproto response to a
flat list of fauna posts (ancestors oldest-first, the focal post, then
its direct replies — `translate_thread`, *not* a nested tree).

Wire: WS-RPC kind **`bluesky.feed.thread`** — the *one* genuinely
protocol-unique consume-side surface that keeps a `bluesky.*` kind.
(Auth/settings/follows are the unified `fauna.bridges.*`; interactions
and notifications are the unified `fauna.posts.*` /
`fauna.notifications.*`.) Request `BlueskyThreadRequest` is an
externally-tagged enum carrying the thread to fetch — `AtUri { uri }`
(for a surface holding the Bluesky AT-URI directly) or `PostId { post_id }`
(the raw 32-byte Fauna post id — **what both the android and linux
post-detail surfaces hold**, having navigated from a *crossposted* Fauna
post whose id is the hex `[u8; 32]` Fauna hash, not the AT-URI; the
handler hex-encodes it and resolves the AT-URI through the
`bluesky_posts` crosspost mapping). Reply
`BlueskyThreadReply { posts: [BlueskyPost] }`; types in
`libs/fauna-protocol/src/bluesky.rs` (float-free mirrors of the bridge
types — `fauna-protocol` does not depend on `fauna-bridge-atproto`, so
the conversion lives nest-side). `User`-gated (Admin inherits),
replay-safe read, 30 s deadline (it makes a live `getPostThread` XRPC
round-trip). Errors: `bluesky.not_found` (no crosspost mapping for a
`PostId`, or upstream `NotFoundPost`), `bluesky.blocked` (upstream
`BlockedPost`), `bluesky.upstream` (agent-restore or XRPC failure — e.g.
Bluesky not linked / session expired). The two HTTP twins
(`GET /api/v1/bluesky/feed/thread/{uri}` + `GET /api/v1/bluesky/thread?post_id={hex}`)
were **deleted** (2026-06-05;
Commit B — tracked internally). **android migrated** onto the
kind via the shared `fauna-client-bluesky` crate + the `FfiBlueskyClient`
UniFFI seam (2026-06-06), and **linux
migrated** (2026-06-06; `client.rs fetch_bluesky_thread`
→ `BlueskyClient::thread(PostId)` directly on the proto types, no FFI
seam). **Corrected 2026-10-01: that was the wire migration, not a
complete fan-out — only linux renders the view today** (the status
paragraph closing this section). The reply is a flat list that carries `focal_index`, the index into `posts` of the requested post, filled by the nest (`translate_thread` returns it as the ancestor count); linux reads it from the wire and the UniFFI apps from `FfiBlueskyThreadReply.focal_index`, which the `fauna-ffi` seam passes through — no app guesses the focal post.

**Status, re-measured 2026-10-01, ruled 2026-10-03 — one app renders the view, and all seven owe it.** linux's post detail shows a *View Thread* button on a post whose source is `bluesky` (`apps/fauna-linux/src/views/feed/post_detail.rs`) and paints the reply as a page of the feed stack; neither the button nor the thread page has a ui.yaml element ID, and no e2e test drives them. android's `ApiClient.blueskyGetThread` and the `FfiBlueskyClient` seam are retained but **unused**: its post detail renders the snapshot preview and never calls them (`tests/e2e-unified/ui-actual-android.yaml`, the feed block's note). web, windows, macos, ios and tui have no consumer. So "the linux and android post-detail surfaces show" above describes what the two apps were wired to on 2026-06-06, not what they render today. A view one app has and six lack is a per-app divergence that no catalog `absences` entry declares ([`../architecture/feature-catalog.md`](../architecture/feature-catalog.md) § Closing a gap), **Ruled 2026-10-03: the view is kept and lifted to all seven apps; its reply carries the nest-named `focal_index` (above).** A person reading a bridged post expects to open the conversation around it, replies from accounts they do not follow included; without the view that conversation is visible nowhere in Fauna, and the feature costs no new nest capability (the kind rides the Bluesky session the feed bridge already holds). One thing is owed before the lift. (The focal post is already named on the wire, not guessed — the `focal_index` above; the wire change was free before the 2026-10 baseline, [`../architecture/version-compatibility.md`](../architecture/version-compatibility.md) § Dimension 2, the fourth exception.) **The element IDs — approved by the user 2026-10-03 (rule A), not yet in ui.yaml or on any app:** `feed-post-view-thread-button` (on the post detail, shown only for a post whose source is `bluesky`), `feed-thread-page` (the thread as a page of the feed stack) and `feed-thread-post` (indexed; the focal post carries the selected state). Then: tui first with its tier_3 witness, the outcome on the `atproto` catalog page, a guide passage, the batched trickle-down, and linux and android re-pointed onto the IDs. Until the tui build lands the IDs, linux keeps what it has. *The question as it stood before the ruling, kept for the record* — it resolved one of two ways. **Lift it to all seven apps**, the standing priorities' default: approve element IDs for the affordance and the thread page (rule A), build on tui first, add the outcome and its witness to the `atproto` catalog page, then the batched trickle-down; the nest kind and the shared `fauna-client-bluesky` crate already exist. **Or retire it**: remove linux's button and thread page, android's unused call, the FFI seam and the shared crate; the `bluesky.feed.thread` kind then has no consumer and leaves at the next major version ([`../architecture/version-compatibility.md`](../architecture/version-compatibility.md)). Until the ruling, linux keeps what it has and no other app adds it.

### Interactions

Post interactions go through the **unified** WS-RPC kind
`fauna.posts.interact` (its HTTP twin is deleted).
The nest identifies the post's source; when it is "bluesky" it resolves
the AT-URI + CID from the `bluesky_posts` mapping and propagates the
action (like / repost / reply / quote / unlike / unrepost) as
`com.atproto.repo.createRecord` / `deleteRecord` on the user's DID repo
(the `route_unified_interaction` path in
`bins/fauna-nest/src/bluesky/interact_routes.rs`, which stays as shared
plumbing for the unified handler).

The lower-level bluesky-specific `interact/*` HTTP routes
(like/unlike/repost/unrepost/reply/quote/post/follow/unfollow/block/unblock)
were **removed** (2026-06-05) — they
had no app consumer. Graph actions (follow/block) had no replacement
path and none was wired (the unified `BridgeProvider::add_follow` for
Bluesky returns "Bluesky does not support bridge follows").

**Reply and quote — the unified shape (ratified 2026-09-26; it supersedes the *recorded deviation* of the same morning).** A reply to, or a quote of, a Bluesky post is **an ordinary signed post of the replier** — the same `Reference::Reply`/`Reference::Quote` post every native reply is ([`../ui/feed.md`](../ui/feed.md) § Interaction bar → *Reply and quote on a bridged post* owns the app-side rule and the one-reply-shape rule; [`activitypub.md`](activitypub.md) § Reply and quote records why the nest-mint alternative was rejected) — composed by the app, created through `fauna.posts.create`, and **derived** into the `app.bsky.feed.post` record by the write-through's create leg (§ Cross-posting → *A post that references a Bluesky record*). The `reply`/`quote` arms of `route_unified_interaction` are the **eligibility door only**: on a post whose source is `bluesky` they answer the native arm's ack verbatim — `{action, target_post_id, source}` — when the caller holds a consume-side link that D7 admits (a `bluesky_accounts` row, `consume_side_poll_allowed`), and otherwise refuse with the arm's `400` naming the remedy (link a Bluesky account from the AT Protocol page); a `body` on this door is ignored, the words travel in the signed post. `counts` stay `None` on this arm — a Bluesky post's counters live at the origin, the one difference from the AP and nostr doors — and the client's existing `None` handling leaves the rendered numbers alone. **The nest-mint path is deleted, not windowed.** Until 2026-09-26 the two arms minted the reply at the user's PDS from the interact `body`, so no local post existed — nothing for `fauna.posts.delete`, export or local threading to see. No older-client accept path is kept: under the 2026-09-24 baseline reset ([`../principles.md`](../principles.md) § No user-data loss, its last paragraph) no installation exists to keep one for, so the `body`-minting arm is removed in place — and it was in any case **unreachable**, since it fired only for a `content` row with source `bluesky`, which nothing wrote until consume-side ingestion landed later that day (§ Implementation status today → *Consume-side feed ingestion — BUILT 2026-09-26*).

### Cross-posting (Fauna → Bluesky)

By default, fauna posts stay on fauna. Enable cross-posting with
`write_through`:

| Value | Behavior |
|-------|---------|
| `0` | Disabled (default) |
| `1` | Auto — every fauna post is cross-posted to Bluesky automatically |
| `2` | Manual — only posts with a `crosspost` tag in facets are cross-posted |

The `write_through` setting is read and written through the **unified**
bridge surface: `fauna.bridges.list` exposes it as a `write_through`
select field in the Bluesky bridge's `settings` array, and
`fauna.bridges.set_settings` updates it (both dispatch into
`BlueskyProvider::{status,update_settings}`). Invalid values (anything
other than 0, 1, 2) return a `fauna.bridges.invalid_params` error. The
deprecated `GET`/`PUT /api/v1/bluesky/settings` HTTP twins were
**removed** (2026-06-05).

**Mode 2 (manual) opt-in mechanism:** the client adds a
`Tag { name: "crosspost" }` facet to posts that should be cross-posted.
This tag is protocol-agnostic — the client doesn't need to know about
Bluesky. Any bridge with a "manual" mode can check for this tag.
Posts without the tag are skipped silently.

**A post that references a Bluesky record — the reference is the intent (ratified 2026-09-26).** Before the mode is consulted, the create leg resolves the post's `Reference::Reply`/`Reference::Quote` target through the `bluesky_posts` map. **A target whose `content.source` is `bluesky`** — a post the consume side ingested — makes the post cross-postable **whatever the mode**: the user tapped reply or quote on a Bluesky post, which is the very intent the toggle exists to ask for — exactly as a nostr reply is pushed regardless of `auto_publish` ([`../ui/nostr.md`](../ui/nostr.md) § Replying to and quoting a nostr note) and a fediverse reply is delivered regardless of the follower fan-out ([`activitypub.md`](activitypub.md) § Reply and quote). The interact door (§ Interactions) has already refused when no account is linked, so this arm never publishes for an unlinked user; a post from an unlinked author is skipped before any network step. **A target that is one of the user's own cross-posted Fauna posts** (source `fauna`, mapped by an earlier write-through) follows the mode like any other post — a self-thread continued after the toggle was switched off is not a Bluesky conversation — and, when the mode admits it, carries the thread refs. Either way the derived record is: for a reply, `reply: {parent, root}` — parent = the target's AT-URI + current CID (`resolve_uri_and_cid`), root = the parent record's own `reply.root` when it carries one, else the parent itself (the ATProto rule the hosted projection applies too — [`atproto-pds-bridge.md`](atproto-pds-bridge.md) § Projection & backfill, *Translation edges*); for a quote, `embed: app.bsky.embed.record {uri, cid}` — both through `fauna_bridge_atproto::outbound::fauna_post_to_bsky_record`'s `reply`/`quote` inputs, the same builder the projection uses. A reference that resolves to no map row — a native target never cross-posted, an AP or nostr one — cross-posts a plain top-level record, the projection's *standalone* rule: the target was never a Bluesky record, so there is nothing to thread under. **The map is one table for both directions:** the write-through writes a row per cross-posted post today; the consume-side ingestion, when it lands, writes one per ingested post (`at_uri` + `cid`), which is what makes an ingested post resolvable as a target — nothing else does.

**Hook + decode (2026-07-16):** the write-through fires from the shared
create-side bridge fan-out `routes::spawn_post_bridge_fanout` — called
wherever a post enters the store as a **new** row: a local
`fauna.posts.create` *and* the forwarded-post receive on a paired public
nest (`fauna.federation.post.forward`). The mandate model is owned by
[`activitypub.md`](activitypub.md) § The produce direction
(paired-deployments bullet): fan-out authority is the author's own
per-actor bridge enablement on the publishing nest — for Bluesky, their
`write_through` setting — never the post's arrival path. The post body is
decoded via the shared `Post::decode_resolved_bytes` path (embed-as-bytes
*and* bare) — the write-through originally used the bare-only decode and
silently no-opped on every native app post (fixed 2026-07-16; unit
pin: `bluesky::write_through_decode_tests`).

Cross-posted posts are tracked in the `bluesky_posts` table mapping
`fauna_post_id` ↔ `at_uri`. This mapping is what the
`bluesky.feed.thread` kind's `PostId` request variant uses to look up
thread context by fauna post ID. It is also what the **write-through
delete leg** consumes: deleting a fauna post removes its cross-posted
Bluesky record via `com.atproto.repo.deleteRecord` and clears this
mapping — the delete-propagation behavior is owned by
[`../ui/feed.md`](../ui/feed.md) § State & data shape → *Post deletion*.

### Notifications

Bluesky notifications (likes, replies, reposts, follows, mentions) are
**bridged into the unified notification stream** — a background poller
(`bluesky::notif_sync::poll_bluesky_notifications`) calls
`app.bsky.notification.listNotifications`, translates each, and inserts
it into the unified notifications table. Apps read them through the
unified `fauna.notifications.*` surface alongside native fauna
notifications. There is **no** bluesky-specific notification HTTP route
(an earlier revision of this doc described `GET/POST
/api/v1/bluesky/notifications*` routes — they never existed in code; the
poller-into-unified path is the only one).

### Search and discovery

The bluesky-specific search / feed-discovery surfaces
(`search/actors`, `search/feeds`, `feeds/{suggested,saved,save,unsave}`)
were **removed** (2026-06-05) — they
had no app consumer. Custom-feed subscription (the kept feature) runs
through the unified `fauna.bridges.feeds.{list,create,delete}` surface
(§ Custom feed subscriptions above). If Bluesky-native search is ever
wired to a client, it is designed then as a net-new `bluesky.*` kind.

## Reading list

1. `principles.md` — product invariants + engineering principles.
2. `tests/e2e-unified/ui.yaml` — `bridges:` page block.
3. `bins/fauna-bridges/` (Go, MTA + MDA roles; talks WS-RPC to nest).
4. The bridges client-architecture design (2026-05-04, tracked internally) — unified shared-shape design.
5. `docs/goal/architecture/` — search for `bridge`.
6. `tests/e2e-unified/ui-actual-<app>.yaml`.
