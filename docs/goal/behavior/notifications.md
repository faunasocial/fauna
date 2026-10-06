# Notifications — target state

Owns: notifications
Status: draft — resolved by a reconciliation pass against the landed `fauna.notifications.*` surface (the doc's remaining TBDs postdate the code; §§ below record the landed answers, and the open remainder is the snapshot error variants: the typed `notif_type` was built 2026-10-02 (§ The notification type), its localized-body and deep-link-destination halves having been sliced out and ratified separately on 2026-09-20). Security-notice ruling landed 2026-08-10 (§ Security notices). Retention ruled 2026-09-24 (§ Retention); its security-notice window ruled the same day (§ Retention, rule 2's carve-out), built nest-side 2026-09-27.
Authority: ui.yaml (`notifications` page) owns IDs and per-page element scope; this doc owns behavior, data shape, and the Rust/app split. Push transports/dispatch → [`../architecture/apps/common.md`](../architecture/apps/common.md) (registry: push-notifications). On conflict in the other doc's domain, raise it.

## Goal

The Notifications page shows the user's notification feed (mentions, replies, reactions, knock requests, group invites, admin actions, system messages) with an unread count badge and a "mark all read" affordance. Notifications are typed; rendering uses a shared row component with a per-type icon.

## Layout & flow

- Top: heading, unread count (`notification-count-badge`), `notification-mark-read` button.
- List: `notification-row` component (carries `notification-item`, `notification-type-icon`).

## Element IDs

Page elements (from ui.yaml):

- `page-heading`, `notification-mark-read`, `notification-count-badge`, `error-message`.

Components used: `notification-row`.

State fields (from ui.yaml):
- `data.notifications.unread_count`.

## State & data shape (landed)

The wire surface is `fauna.notifications.{list, mark_read, count, dismiss,
clear}` (`bins/fauna-nest/src/notifications_handlers.rs`; the HTTP twins are
deleted). Wire types live in `libs/fauna-protocol/src/notifications.rs`:

- `NotifListRequest { cursor: Option<i64>, limit: Option<i64> }` — cursor
  is the last-seen `id`; default page 25, clamp 1..=100.
- `NotifItem { id: i64, notif_type: NotifType, source: String, sender_id:
  Option<String /* hex [u8;32] */>, content_id: Option<String>,
  subject_uri: Option<String>, summary: String, is_read: bool,
  created_at: i64 /* micros */ }` — `notif_type` is `like`/`reply`/… (§ The
  notification type);
  `source` is the origin protocol (`fauna`/`bluesky`/`nostr`/`activitypub`).
- `NotifListReply { notifications, cursor }`,
  `NotifMarkReadRequest { up_to: Option<i64> }` → `{ marked_read }`,
  `NotifCountRequest {}` → `{ count }`.
- `NotifDismissRequest { id: i64 }` → `{ dismissed: bool }` and
  `NotifClearRequest { up_to: Option<i64> }` → `{ cleared: i64 }` — the two
  user-initiated deletes § Retention rules (added 2026-09-24; both keyed on
  the connection actor, both idempotent).

The earlier proposed `notifications_snapshot()` / `NotificationsSnapshot`
shape **lost** to this typed-client + wire-reply shape — apps consume
`libs/fauna-client-notifications::NotificationsClient` (wasm passthroughs
in `libs/fauna-wasm/src/rpc.rs`) and render the replies directly.

**Open remainder (the draft part):** the snapshot error variants. The typed
`notif_type` is built (§ The notification type, 2026-10-02); two pieces were
**sliced out of that bundle earlier and ratified on their own**: the
`LocalizedText` body (§ Localized body, 2026-09-20), the part a non-English
reader sees on every row; and the **deep-link destinations** (§ Deep-link
destinations, 2026-09-20). Additive evolution only (new optional fields;
`extra` catch-alls exist on every type).

## The notification type (built 2026-10-02)

`notif_type` is one shared enum, `fauna_core::notification_type::NotifType`,
re-exported as the wire type `fauna_protocol::notifications::NotifType`. On
the wire and at rest (the nest's `notifications.notif_type` column) it is the
string it always was: the enum changes how a build reads the value, never the
bytes. The named types are `like`, `reply`, `repost`, `quote`, `mention`,
`follow`, `other` (`Interaction` — a bridged interaction the origin protocol
names and Fauna has no closer type for), `message`, `event_invite`,
`group_invite`, `knock`, `security.notice`, `mail.forward_queue_evicted`,
`abuse_report.received`, `abuse_report.resolved`, and the four `family.*`
doorbells (`content_notice`, `contact_request`, `feed_source_request`,
`feed_source_approved`).

**The unknown arm is open, carrying** ([`../architecture/transport.md`](../architecture/transport.md)
§ Rule 3 in full, answer 1). The nest reads the type back out of its own table
and re-emits it on every list reply and push frame, and a downgraded nest or
an older app may meet a type a newer release minted, so a type this build does
not name is kept verbatim in `NotifType::Other(String)` and re-encodes
unchanged: a decode never fails on an unknown type. Every reader gives it the
restrictive reading — no destination (§ Deep-link destinations), the neutral
icon. A producer mints only named types; the Bluesky poller's catch-all reason
is the named `Interaction`, never a carried unknown.

**Who reads it.** Every nest producer passes a variant to
`CacheDb::insert_notification`; the list row (`NotifItem`) and the push frame
(`NotificationPayload`) carry it; the deep-link router and the icon
classification (`NotificationGlyph::of`) match it exhaustively, so a new
variant does not compile until each has decided what it does. The UniFFI
records (`FfiNotifItem`, `FfiNotification`) and the wasm rows carry the wire
string — the one form every app language reads, and the form that passes a
newer nest's type through untouched — and classify it only through the shared
calls (`notification_glyph_for_type`, `notification_destination_for`, the wasm
`notificationTypeGlyph` and `notificationDestination`), never an app-side match.

## Localized body (ratified 2026-09-20)

The nest cannot localize a notification: it does not know the reader's
locale, one user reads the same nest from apps in different languages, and a
stored row is read long after it was written. So the nest sends **what
happened** as a key plus data, and the app says it in the reader's language.

**Wire — one new optional field, three places.**

- `NotifItem.body: Option<LocalizedText>` — the wire's own
  `fauna_protocol::LocalizedText` (`key: tstr`, `args: {* tstr => tstr}`,
  plus its catch-all; `schemas/error.cddl`), the type `RpcError.message`
  already carries. Not `fauna_core::localized::LocalizedText`: that one is the
  in-process/FFI carrier, and its `HashMap` has no place on a canonical wire.
- The push twins carry the same field: `NotificationPayload.body` and
  `KnockPayload.body` (`schemas/push_events.cddl`), so an OS-level
  notification is localized exactly like the row it announces.
- At the FFI boundary the mirror converts to
  `fauna_core::localized::LocalizedText` (the one `uniffi::Record` every
  machine shares) as `FfiNotifItem.body`, `#[uniffi(default = None)]`.

`key` names an `i18n/strings/en.yaml` entry in the existing `notifications:`
section, as `notifications.<name>`; `args` are **data** — a display name, a
count, an address, a URL — substituted into named placeholders. An arg is
never a translatable term and never a key (the row is relayed text from
other people; `resolve_nested` would translate a display name that happened
to equal a key).

**At rest — two nullable columns**, `notifications.body_key TEXT` and
`notifications.body_args TEXT` (a JSON object of strings), added by
`ALTER TABLE … ADD COLUMN`. A row written before the columns existed reads
back with no `body` and renders its `summary`, forever; nothing is
backfilled, because the data an old summary was composed from is gone.

**`summary` stays, populated, for the whole major version.** It is the
English rendering of the same sentence and the compat fallback:

| Reader | Row minted by | Renders |
|---|---|---|
| older app (ignores `body`; it lands in `extra`) | newer nest | `summary` |
| newer app | older nest, or a pre-column row (no `body`) | `summary` |
| newer app | newer nest, `body.key` **in its catalog** | the localized body |
| newer app | a still-newer nest, `body.key` **not** in its catalog | `summary` — never the raw key, never a half-substituted template |
| any app | `summary` empty and no usable `body` | `notifications.default_body` |

The fourth row is why "is this key known" is part of the decision and not a
resolver detail: `LocalizedText::resolve`'s key-as-template fallback is a
diagnostic for our own machines' keys, and would paint
`notifications.some_future_key` at a user.

**Where the decision lives.** Shared Rust —
`fauna_client_notifications::notification_text(&NotifItem)` applies the
table above and returns `NotificationText::Localized(LocalizedText)` (the
`fauna_core` carrier) or `NotificationText::Verbatim(String)` (the
`summary`). An app paints the second as-is and resolves the first through
its **native** pipeline (`fauna_i18n` lookup
on linux/tui, `L()` on web, `getString` on android, the generated `L.lookup`
flat table on apple (`renderLocalizedText`, `Core/LocalizedTextRender.swift`
— not `Bundle.localizedString`, apple has no `.strings` catalog for these
keys), RESW on windows);
shared Rust never calls `resolve()` for an app, which would strand the five
apps whose catalog is not the Rust one. The known-key set is the generated
catalog itself, pinned by a test against `fauna_i18n::strings::lookup`.

**The knock toast.** Every app raises an OS-level toast for an inbound knock,
titled `notifications.knock_title`. What it says is the same kind of shared
decision over the knock push — `fauna_client_notifications::knock_push_text`
(UniFFI `knock_text_for(FfiKnock)`, `FfiKnock.body` defaulting to `None`): a
`KnockPayload.body` whose key this build knows is the knock row's own
`notifications.row_knock` sentence and localizes; anything else — an older
nest's bodyless knock, or a newer nest's unknown key — paints the toast's own
`notifications.knock_body` sentence naming the sender's 8-hex prefix (what
every knock toast said before the push carried a body). A knock's `summary`
is the knocker's raw message, not an English rendering of the row, so unlike
`NotificationPayload.summary` it is never the painted fallback. **The
knocker's message in the known-key sentence is a stranger's text, and is plain
text on every seat.** `knock_push_text` runs every arg of the knock body through
`fauna_core::control_chars::sanitize_plain_line` — control, bidi and other
format characters dropped, newlines and line separators collapsed to a space,
the result capped at 200 characters on a character boundary — so no app
paints it as anything but one plain line (the nest's own
`MAX_KNOCK_SUMMARY_BYTES` refusal is the first bound; this is the second). A
shell that hands the body to an OS surface which interprets markup escapes it
too (linux's freedesktop body: `&`, `<`, `>`), and a knock toast carries one
notification id per app, never one per sender, so a flood of fresh keys
replaces one toast instead of stacking.

**The nest cannot mint a row without a key.** `insert_notification` takes the
body and the fallback together (`NotificationText { body, summary }`), so
"a new producer composed an English sentence and forgot the key" does not
compile (the only keyless constructors are the e2e push test hook's and
`#[cfg(test)]`'s). That signature, not a lint, is what keeps the drift
closed — and the ordinary constructor derives `summary` from the catalog
entry, so the nest holds no second copy of the sentence to drift from it.

**Security notices — the boundary, ruled.** `SecurityEvent::subject()` and
`body()` feed two channels, and they part ways here:

- The **row** gets a per-event key (`notifications.security_new_token`, …)
  whose args carry the actionable detail this doc requires the row to show —
  the IP address, the pending action's id and execute time, the new
  recovery key. One localized sentence per event, not the multi-paragraph
  English block: the catalog has no multi-line entries, and the row is a
  list cell.
- The **mail** channel keeps its nest-composed English subject and body. Mail
  is message content delivered to an arbitrary MUA with no app pipeline
  behind it; localizing it is a different problem (a per-user locale stored
  on the nest) and is not claimed here.
- `summary` for a security row stays the full English subject + body, so an
  older app loses nothing.

So the rule is not "no English string literal in the nest". It is: **every
sentence a row shows is reachable through a key; nest-composed English
survives only as the `summary` compat fallback and as mail content.**

**`action_type` / `change_type` args** are nest-internal identifiers today
(`delete_account`, …) and are substituted as data. Giving them catalog labels
belongs to the typed-enum evolution, which is what will enumerate them.

## Deep-link destinations (ratified 2026-09-20)

Tapping a row takes the user to what it is about. **One shared-Rust router
decides where**, never a per-app table (§ Don't do these):
`fauna_client_notifications::notification_destination(&NotifItem)` returns
`Option<NotificationDestination>`, and app glue routes the target into the
destination page's own gesture — exactly as a direct click there would. This
is `fauna_client_search::SearchNav`'s shape and its app-glue contract
(`ui/search.md` § Where logic lives → *Result navigation*); the two enums stay
**per-domain** rather than merging, because their variant sets barely overlap
and a merged one would hand each app's glue arms it can never receive.

**The key is `source` **then** `notif_type`, never the type alone.** A bridged
row reuses the native type vocabulary while carrying completely different ids:
its `content_id` is the hex of the notification's *own* AT-URI — a dedup token,
minted because a bridged row has no 32-byte fauna sender or content key to dedup
on — and the subject it is about rides `subject_uri`. Routing a bridged `like`
on `content_id` would deep-link every one of them to a post that cannot exist.

| `source` | `notif_type` | Destination | Keyed on |
|---|---|---|---|
| `fauna` | `like` | The liked post's detail view | `content_id`, which this producer sets to the post id verbatim |
| `fauna` | `knock` | The pending-knock surface on Contacts (`knock-request-item`) | `sender_id` — a knock *is* its sender, and carries no content id |
| `fauna` | `family.content_notice`, `family.contact_request`, `family.feed_source_request`, `family.feed_source_approved` | The Family page | nothing — the page is the destination (see below) |
| `fauna` | `security.notice` | **none** — informational | — |
| `fauna` | `mail.forward_queue_evicted` | **none** — informational | — |
| `bluesky` | any, with a `subject_uri` naming an `app.bsky.feed.post` record | **Off-app**: the post's page on bsky.app, in the OS default browser (`External { url }`) | `subject_uri`, turned into `https://bsky.app/profile/<authority>/post/<rkey>` by `fauna_core::bluesky_web_url::post_web_url` — never `content_id` |
| `bluesky` | any, without a post subject (`follow`, `mention`) | **none** — informational | — |
| any other bridged (`nostr`, …) | any | **none** — informational | — |
| any | a type this build does not know | **none** — informational | — |

**Why each `None` is a decision, not a gap.** A row with no destination renders
**inert** — a plain label, never a control that silently does nothing (search's
same rule, `ui/search.md`). The reasons:

- All four family doorbells route to the Family page. The three
  **guardian-side** ones land where the guardian decides; the fourth,
  `family.feed_source_approved`, rings the **ward**, and the user ruled
  2026-09-25 that it goes where its siblings go — uniform with them — rather
  than back to the bridges surface where the refusal happened and the
  "approved — try again" state renders in place (`bridge-source-request-state`,
  `family-safety.md` § Feed-source approvals). That in-place state stays the
  ward's readout; the notification's tap is the uniform one.
- No `family.*` row is keyed on its `content_id`: all four are **dedup
  tokens** — `"{day}:{category}"`, `peer ‖ row_id`, a bare row id — not
  identities. The Family page itself is the destination, because it is where
  the pending queue (`family-approval-item`) and the per-ward readouts
  (`family-ward-content-notices`) render for a guardian, and where a ward's own
  supervision state lives.
- A **security notice** carries its whole actionable detail in the row the user
  is already reading (§ Security notices); its `content_id` is the inbox row id
  used as a dedup token, so there is no id to route on and nothing further to
  show.
- A **forward-queue eviction** carries no ids at all, and its only candidate
  surface (`admin-aliases-forwarders-section`) is admin-gated while the
  notification goes to the forwarding actor — a different role.
- A **bridged Bluesky** row opens the post it is about **outside Fauna**, on
  the network's own website (user-ruled 2026-09-25: leaving the app for a
  third-party site is the product's posture for bridged content, since no
  Fauna page can address a bridged post). The arm reads `subject_uri` — the
  AppView's `reasonSubject`: the liked, reposted, quoted or replied-to post —
  never `content_id`. Shared Rust builds the address from a fixed origin and
  charset-validated segments (`fauna_core::bluesky_web_url::post_web_url`), so
  a hostile subject cannot steer the browser off a bsky.app post page; each
  app hands the finished URL to its ordinary external-link opener (tui
  `os_open`, apple `OpenURL`, windows / android `UrlOpener`, linux
  `url_opener`, web `safe-url`) and never renders it inside a page. A row with
  no post subject — a `follow`, or a `mention`, for which the AppView sends
  none — is informational: there is no page to open, and its `content_id` is
  the hex of its own record, not an address. Only `bluesky` has an address
  form today; another bridged source renders inert until one is added here.

**Forward compatibility.** An unknown `source`, an unknown `notif_type`, or a
known one whose row lacks the id its destination needs all return `None`. A
newer nest may mint types this build has never heard of; rendering them inert is
the same posture § Localized body takes for an unknown body key.

## Where logic lives

- **Notification feed source / aggregation.** Nest-side (the notifications
  store the handlers read); apps render via the shared
  `NotificationsClient`.
- **Mark-read mutation.** Shared Rust — `NotificationsClient` →
  `fauna.notifications.mark_read` (nest-side flip, `{marked_read}` echo).
- **Deep-link routing on tap.** Shared Rust returns the destination from the
  row; app glue navigates. `fauna_client_notifications::
  notification_destination(&NotifItem)` → `Option<NotificationDestination>`
  applies § Deep-link destinations' table; `None` renders an inert row. Built
  2026-09-20 through tui (§ Implementation status today).
- **Notification type → icon mapping.** Shared Rust owns the type
  (§ The notification type) and its icon category
  (`NotificationGlyph::of(&NotifType)`); app glue picks the icon (icons are
  platform-native assets). **Partially landed 2026-08-21:** linux and web, which independently
  hand-wrote the identical `notif_type` → emoji match, now delegate to
  `fauna_core::notification_glyph` (`NotificationGlyph`, exported to web
  via wasm). tui, android and windows map their own, deliberately
  different `notif_type` vocabulary onto their own native asset family
  (terminal glyphs / Material icons / Segoe MDL2 glyphs) and are not
  lifted onto this enum. **macOS and iOS joined the enum 2026-09-20**, in
  the split's intended shape rather than by adopting the emoji family:
  `NotificationGlyph` is now a `uniffi::Enum`, `notification_glyph_for_type`
  classifies the wire string over UniFFI, and one shared FaunaKit leaf
  (`NotificationTypeIcon`, both apple apps) picks the SF Symbol and announces
  what it means. Before that both apple apps painted one fixed bell for every
  row, and registered an empty automation value for it. **The enum gained
  `Report` 2026-09-26** for the two abuse-report types
  (`abuse_report.received`, `abuse_report.resolved` — owned by
  [`moderation.md`](moderation.md) § User-initiated reporting): 🚩 for the
  emoji family, `flag.fill` on apple.

## Security notices

Ruled 2026-08-10 (user decision): a security notice — the nest's
`SecurityNotifier` events (new sign-in token, pending account action,
recovery-key replacement, identity succession, …) — **renders on this page**.
Of the three candidate shapes (notifications row / a dedicated
security-notices surface / mail-only), the row won: it reaches the user on
the page all seven apps already ship, with no new element IDs and no per-app
work, and `critical-alerts.md` had already ruled these events below the
critical-alert severity bar.

- **The nest writes the row.** `SecurityNotifier::notify`
  (`bins/fauna-nest/src/security_notify.rs`, channel 1b) writes a
  `notifications` row — `notif_type: "security.notice"`, `source: "fauna"` —
  alongside its durable inbox envelope and mail channels (the push-relay
  channel was deleted 2026-08-15 — `api-layers.md` § Push relay), and fires
  the same `PushEvent::Notification` every other producer fires.
- **The summary is the full notice** (subject + body): the body carries the
  actionable detail (the IP address, a pending action's id), and on a
  deployment with no claimed mail domain this row is the only place that
  detail ever reaches the user. One-line rendering is app-side truncation.
  The localized `body` carries the same actionable detail as args
  (§ Localized body → *Security notices*).
- **Every event rings.** The insert's dedup token is the inbox row id (the
  per-decision `feed_request_dedup_key` pattern), so a second
  `NewTokenIssued` — a fresh compromise signal — is never swallowed as a
  duplicate of the first.
- **The row outlives its author's hand for 14 days.** § Retention's
  security-notice window: rule 2's deletes refuse or skip it until then.
- **Apps ack the redundant inbox copy.** Both `apply_security_notice`
  faces (`fauna-client-conversations`, `fauna-wasm`) return `Ok` on sight —
  an honest ack, because this row is the durable render surface. Against an
  **older nest** that predates the row write, the ack consumes a notice with
  no rendered twin; accepted, since those notices were invisible on every
  app anyway (the pre-2026-08-10 `Err` stubs), and the push + mail channels
  are unaffected.
- **Pending actions — which steps ring, and who is told (ruled 2026-09-24;
  audience widened the same day).** A *person-initiated* pending action (a
  handle change, an account deletion, a snapshot deletion, an admin's user
  deletion or admin-roster change) rings at each step of its lifecycle —
  queued, executed, cancelled (naming who called it off), and **expired**
  (the delay window closed short of the approvals it needed, so nothing
  changed) — and **the audience is everyone the cancel-authorization matrix
  (`db::pending_actions::cancel_pending_action`) admits**, each told once per
  step, because a party who may act must be told there is something to act
  on. Three audiences, one surface each (`pending_actions::notify_transition`):
  - the **creator** — `PendingActionCreated` (carrying the action's id and
    execute time), `ActionExecuted`, `ActionCancelled`, `ActionExpired` —
    acting from their own Settings → Pending actions section
    (`../ui/settings.md` § Pending actions);
  - the **target** of an admin action against their account
    (`admin.delete_user`; the class `ActionType::is_admin_action_against_user`)
    — `PendingActionAgainstYou` at scheduling (naming the scheduling admin
    and the action's id), then `ActionCancelled` / `ActionExpired`; their own
    Settings → Pending actions section lists the action, since
    `fauna.pending_actions.list` returns actions *against* the caller as well
    as the caller's own, and its cancel is already theirs by the matrix. The
    target of an executed action naming one account (a suspension, admin add
    / remove / role change) is told `AdminChange`; an account a just-executed
    deletion removed is told nothing;
  - every **co-admin** (the roster minus the creator) of an admin action of
    either kind — `AdminActionPending` at scheduling (who scheduled it, whom
    it names, when it runs, how many approvals it still needs), then
    `AdminChange` on execution, `AdminActionCancelled`, `AdminActionExpired`
    — acting from the admin console's pending-admin-actions section
    (`admin.md` § Pending admin actions). This notice is what makes a quorum
    approval possible at all: a roster action short of approvals at its time
    expires unexecuted (`execute_ready_actions`), and before it existed the
    co-admins learned of a roster change only by happening to look. The
    target of a roster action is an admin and is told this way.

  There is no cancel link: each row says where to act in words and keeps
  § Deep-link destinations' no-destination rule. The nest's **own scheduled**
  actions — the automatic snapshot- and version-retention prunes
  (`backup::prune`, `backup::version_prune`) — ring at no step: they are
  housekeeping the user did not ask for, and a notice on every retention pass
  would teach the user to ignore the ones that matter. Pinned nest-side in
  `pending_actions::security_notice_tests`.
- **Not a critical alert, and not a sweep trigger.** `critical-alerts.md`
  owns the banner surface and its severity bar; the alert sweep's deliberate
  refusal to drive re-checks off the inbox drain stands on its remaining two
  reasons (`run_alert_sweep_loop`'s doc comment carries them).

## User actions

| Element | Action | Where it runs |
|---|---|---|
| `notification-mark-read` | Mark all read. | Shared Rust (`NotificationsClient` → `fauna.notifications.mark_read`). |
| `notification-item[i]` | Open what the row is about. | Shared Rust returns the destination (`notification_destination`, § Deep-link destinations); the app navigates. A row with no destination renders inert. |
| per-row dismiss (id pending user approval into ui.yaml; tui-first) | Delete this one row. | Shared Rust (`NotificationsClient` → `fauna.notifications.dismiss`); § Retention rule 2. |
| clear-all (id pending user approval into ui.yaml; tui-first) | Delete every row created up to now. | Shared Rust (`NotificationsClient` → `fauna.notifications.clear`); § Retention rule 2. |

## Persistence

Notifications live nest-side (the notifications table the handlers read);
apps hold no durable cache — pages are fetched per view, the unread
count via `fauna.notifications.count`. How long a row lives: § Retention.

## Retention (ratified 2026-09-24)

**A notification row is the user's own record of what others did toward their account, and nothing on the nest sweeps it by age or by count.** That is the reading the actor-table registry already gives the table (`bins/fauna-nest/src/db/actor_tables.rs`: purged with the account, moved with it on succession, exported verbatim — "the user's record of their own account"), and for several rows it is the *only* record on the nest: a security notice on a deployment with no mail domain (§ Security notices), a forward-queue eviction, every bridged row once the upstream's own window closes. An age or count sweep would delete that record on the nest's say-so — the no-user-data-loss invariant (`../principles.md` § No user-data loss) forbids destroying what the user cannot recreate, and the deciding case is the security notice, whose only copy an age sweep would take. The table grows with the user's own life on the box — their contacts, wards, bridges and account — except for the one stranger-written row, the knock doorbell, which is bounded at the door (`../ui/contacts.md` § Persistence: ≤ ~1 KiB of stranger text per row, ≤ 1 000 pending per recipient) and bound to its knock (rule 3), so every stranger-written byte on this table is bounded in size, in count and in time. A row therefore leaves the table on exactly three paths:

1. **With the account.** `Policy::Purge` in `actor_tables.rs`, unchanged; the same registry moves the rows to a successor and exports them verbatim.
2. **By the user's hand, on the Notifications page.** Two grains, both nest-side deletes keyed on the connection actor: `fauna.notifications.dismiss { id }` → `{ dismissed }` deletes one row (`false` when no row of that id is the caller's — idempotent, never an error, so a replay or a double-tap deletes nothing new), and `fauna.notifications.clear { up_to? }` → `{ cleared }` deletes every row of the caller's created at or before `up_to` (default now — `mark_read`'s shape, so a page that lists then clears cannot delete a row that arrived after it looked). Both are `OfflineSafe` and replay-safe @5 s. The UI is a per-row dismiss and a clear-all, **tui-first** — the lead app lands them with ids the user approves into ui.yaml, the other six follow in trickle-down; the shared `NotificationsClient` carries both calls from day one. **One carve-out: a `security.notice` row is deletable by neither grain for 14 days after it is written** — the security-notice window, below.
3. **A knock doorbell goes with its knock.** The `knock` row announces a *pending* request and its only content is the stranger's text, so it lives exactly as long as the knock: `fauna.knocks.dismiss` and `fauna.knocks.block` delete it with the knock (the knock, its pending edge and its doorbell are one thing the user turned away — two dismissals per spam knock would be a chore, and text the user refused has no business outliving the refusal), and the hourly `expire_old_knocks` sweep (`PENDING_KNOCK_TTL_SECS`, 90 days unacted) deletes it with the knock it expires — a doorbell for a request the nest no longer holds, and the step that keeps a rotating-keypair flood's residue bounded in *time* the way the knock queue already bounds it in count (the same sweep now also drops the knock's `pending` contact edge, `../ui/contacts.md` § Persistence — without that an expired knocker was refused as still-pending for ever). **`fauna.knocks.accept` keeps it**: the knock row goes on accept, so from then on the doorbell is the knocker's message's only home and the record of an accepted request — the user's own record, rule 2's to remove. **A re-knock supersedes its own doorbell**: `store_knock` deletes the sender's standing `knock` row before inserting the new one, so a second knock from the same person (after an accepted edge lapsed, say — `contacts.md` § Persistence, 30 days) rings again instead of being swallowed as a duplicate by `insert_notification`'s dedup.

**The security-notice window — rule 2's one carve-out (ruled 2026-09-24, user decision).** A `security.notice` row (§ Security notices) is the account's record of what a *session* did — a new sign-in from an unseen address, a scheduled account action, a recovery-key replacement — and the session it exposes holds a User-class bearer that can call rule 2's deletes the second the row lands: a stolen key's first sign-in would otherwise erase the one record of itself (on a nest with no mail domain the only copy; the push reached only the seats online at that instant, and the acked inbox envelope is rendered by no app). So for **`SECURITY_NOTICE_RETENTION_SECS` = 14 days after `created_at`** neither grain removes it: `fauna.notifications.dismiss` on a retained row refuses with **`fauna.notifications.retained`** (a within-family refusal in the shape of `../architecture/api-layers.md` § Refusal codes at the gate — never `{ dismissed: false }`, which means "no row of that id is yours"), and `fauna.notifications.clear` skips retained rows, counts only what went, and leaves them listed. After the window the row is the user's to remove, rule 2 unchanged; rule 1 (purge with the account, the succession move, the export) is untouched — the window binds the account's *sessions*, not the account. The constant is a hard-coded Rust constant in `libs/fauna-protocol` (bucket 1 of `../principles.md` § One configuration surface: no user would tune it), shared with the apps so a Notifications page can grey the dismiss affordance on a row whose `notif_type` is `security.notice` and whose `created_at` is inside the window with no wire change — the nest's refusal stays the authority. **Why 14 days, and why this is not a breach of the user's control of their own data** (`../principles.md` § The user always controls their data): it is the same window, for the same reason, as the delay on account deletion (`bins/fauna-nest/src/pending_actions.rs`, the 14-day-delayed account deletion) — an irreversible act by one session waits long enough for the user's other seats to see it, because the account's own session is exactly the party the nest cannot tell from a thief. The delete affordance stays in the app; only its timing moves. Every `security.notice` row takes the window, not just the new-sign-in one — one predicate, and each of those events is a record of a session's act in the same sense.

**Why the accept case is not a data-loss exception in reverse.** Dismiss, block and expiry delete a row whose subject the nest has itself already deleted (the knock) — the same class the product already treats as ephemeral in `knocks`, restated in code beside each delete. Accept is the one transition that turns a pending stranger's request into part of the user's record, and the rule keeps exactly that row.

## Errors & edge cases

- `error-message` page-level.
- Empty list renders the empty state; mark-read failure surfaces on
  `error-message` (snapshot variants arrive with the typed-enum evolution).
- A dismiss of a security notice inside its window answers
  `fauna.notifications.retained` — § Retention, the security-notice window.

## Architectural rules

1. Observer-driven rendering once the typed-enum evolution lands.
2. Notification body text is `LocalizedText`; neither the app nor the nest
   composes the sentence a row shows (§ Localized body owns the shape, the
   compat table and the security-notice boundary). `summary` is the English
   compat fallback, not the rendering.
3. Indexed list IDs (`notification-item[i]`, `notification-type-icon[i]`).
4. **The unread count is app-global, not page-scoped.** A
   `PushEvent::Notification` has to move the count while the user is anywhere
   in the app, so the count lives on a shell-lifetime holder that the push arm
   refreshes — linux's `app.notifications_unread_count`, windows'
   `MainViewModel.UnreadNotificationCount` — never only on the notifications
   page's own view-model, which exists solely while that page is mounted. The
   `notification-count-badge` renders that number; it does not own it, and a
   mark-all-read on the page re-reads the shell's copy rather than forking a
   second one. `data.notifications.unread_count` is that app-global value,
   which is what lets a test assert on it from another page entirely
   (`test_nest_flip_resilience` fires a push while sitting on the feed).

## Don't do these

- Don't compose notification body text per-app.
- Don't deep-link via per-app routing tables. Shared Rust returns the
  destination (§ Deep-link destinations).
- Don't key a destination on `notif_type` alone. `source` decides first — a
  bridged row's `content_id` is a dedup token, not a fauna id.
- Don't paint a control on a row with no destination. An inert row is a label;
  a clickable row that does nothing reads as a broken button.

## Implementation status today

- **Built:** the full wire surface (`fauna.notifications.{list, mark_read,
  count}` — handlers + kind registry; HTTP twins deleted), the shared typed
  client (`libs/fauna-client-notifications`), wasm passthroughs, and a
  notifications page on **all seven apps** (`NotificationsScreen.kt`,
  `NotificationsView.swift`, `MacNotificationsView.swift`,
  `views/notifications.rs`, `notifications/+page.svelte`,
  `NotificationsPage.xaml`, `apps/fauna-tui/src/notifications.rs`). OS-level push delivery is owned by
  apps/common.md (push-notifications row). The security-notice row channel +
  the honest ack on both client faces are built (2026-08-10, § Security
  notices). Its producers: new sign-in (both WS mints, 2026-09-24 — `login.md`
  § the handshake's side effects), the pending-action lifecycle (2026-09-24,
  `pending_actions::{schedule, notify_transition}`), recovery-key replacement,
  identity succession, archive export and mailbox-export download
  (2026-09-24, `mail-export.md` § Download flow); e2e-witnessed on tui for the archive
  and new-sign-in notices (`test_notifications_security_notice.py`).
- **Built 2026-10-02:** the typed `notif_type` (§ The notification type) —
  every nest producer, the list row and the push frame, the deep-link router
  and `NotificationGlyph::of`; tui's glyph map and linux's toast title match
  the enum. **Not built:** the snapshot error variants. android's and
  windows' own glyph vocabularies still key on the wire string in app code
  (§ Where logic lives, the icon-mapping bullet).
- **Retention (§ Retention) — ratified and built nest-side 2026-09-24.**
  Built: the two delete kinds `fauna.notifications.{dismiss, clear}` (wire
  types, kind registry, offline class, allowlist, handlers, `CacheDb::
  {dismiss_notification, clear_notifications}`), their `NotificationsClient`
  methods, and the knock-doorbell binding — `dismiss_knock_core` /
  `block_contact_core` delete the sender's `knock` row, `expire_old_knocks`
  deletes the doorbells of the knocks it expires in the same statement pair,
  `accept_contact_core` leaves it, and `store_knock` supersedes a standing
  doorbell before inserting; witnessed in
  `bins/fauna-nest/tests/conformance_notifications.rs`,
  `conformance_contacts.rs`, and the `db`/`routes` unit tests. Built
  2026-09-27: the security-notice window (§ Retention) — the shared
  `SECURITY_NOTICE_RETENTION_SECS`, `CODE_NOTIFICATION_RETAINED`,
  `NOTIF_TYPE_SECURITY_NOTICE` and `is_security_notice_retained` in
  `libs/fauna-protocol/src/notifications.rs`, one SQL predicate in `CacheDb::
  {dismiss_notification, clear_notifications}` (the `retained` refusal, the
  clear skip), pinned in `conformance_notifications.rs`. **Not built:** the per-row dismiss and clear-all UI on any app (tui leads; the
  ids need user approval into ui.yaml), the wasm passthroughs and the
  `FfiNotificationsClient` methods — each lands with its first consumer, as
  the deep-link and localized-body façades did.
- **App-global unread count (§ Architectural rules, rule 4) — macOS + iOS
  joined 2026-09-24.** Until then apple wrote `notificationsUnreadCount` only
  from the Notifications page's own load, so a push landing while the user sat
  on any other page left the count unchanged (the `test_nest_flip_resilience`
  push leg's red on both apple targets, whose message blamed a missing
  re-bridge; the broker re-bridges fine). Both shells now re-read
  `fauna.notifications.count` from their root on session start, reconnect and
  every notification push (`FaunaClient.fetchUnreadNotificationCount`).
- **Deep-link destinations (§ Deep-link destinations) — ratified 2026-09-20;
  built through tui the same day.** Built:
  `fauna_client_notifications::{NotificationDestination,
  notification_destination}` (the whole table, incl. the `source`-first rule
  pinned by a bridged-`like` arm proved non-vacuous by rule reversion), the
  UniFFI + wasm façades, and tui's render + navigation
  (`Gesture::OpenNotification` → `notifications::open_notification`, the
  `Gesture::OpenSearchResult` twin), witnessed by
  `tests/e2e-unified/tests/test_notification_tap_through.py`. **macOS + iOS
  joined 2026-09-21** through one shared FaunaKit
  seam (`Core/NotificationOpen.swift`) over the UniFFI façade, each shell
  routing the typed target into the destination page's own gesture — the Post
  arm through `FeedVM.pendingPostOpen`, the same cross-page deep-link slot a
  `search-result-item` activation uses; the Knock arm through a new
  `pendingKnockSenderId` that selects the sticky Contacts segment back to
  People. Until then apple's FFI mapping dropped `source` / `content_id` /
  `sender_id` entirely, so no apple row could be routed at all. **Not built:**
  the render + navigation on **web, linux, windows and android**, which still
  paint an inert row — the shared router is done, so each lift is navigation
  glue only. **The two product-choice destinations
  ruled 2026-09-25** — the ward-side `family.feed_source_approved` → Family,
  and a Bluesky row → its post on bsky.app (`External { url }`, built by
  `fauna_core::bluesky_web_url`) — are built in the router (unit-pinned), both
  façades, tui (`open_notification` → `os_open`) and apple (`OpenURL`;
  `subject_uri` is now carried on `NotificationItem`, which had dropped it);
  apple's leg is built and verified on macOS (`swift test` with both arms
  pinned at the mapping seam, `mac-debug`, and the Post-arm tap-through
  witness green on macos and ios), and the four remaining apps take both arms
  with their trickle-down. No e2e journey covers the off-app arm: the e2e fakes serve no
  Bluesky notification feed, so its mechanism is pinned at the router, the
  façade and tui's painted gesture, and the OS handoff by `os_open`'s own
  spawn test.
- **Localized body (§ Localized body) — ratified 2026-09-20; built through
  tui the same day.** Built: `NotifItem.body` + `NotificationPayload.body`
  (wire + `push_events.cddl`), the two columns (nest schema v71),
  `insert_notification`'s `NotificationText` signature with **all nine**
  producers keyed (the seven bridged/social sentences, knock, like, the
  forward-queue eviction, the four family doorbells, and one key per
  `SecurityEvent`), `fauna_client_notifications::notification_text`, and
  tui's render. Every producer but the security notice derives its `summary`
  from the catalog (`NotificationText::localized`), so the nest holds no copy
  of those sentences. **Built 2026-09-22:** `KnockPayload.body` (wire +
  `push_events.cddl`; the knock push carries the knock row's own
  `notifications.row_knock` body); `FfiNotifItem.body` plus the pure
  UniFFI façade `notification_text_for(FfiNotifItem) -> FfiNotificationText`
  (the twin of `notification_destination_for`) and the wasm
  `notificationText(row)` passthrough; `notification_push_text`, the same
  decision over a `fauna.notification` push; and the render on **web, linux
  and android** — linux's desktop toasts (off the list and off the push) say
  what the row says. tui, web and linux are witnessed by
  `tests/e2e-unified/tests/test_notifications_localized_body.py`, which seeds
  rows whose body and summary deliberately differ. **Not built:** android's
  e2e leg — the test carries android's mark, but no android e2e venue exists
  yet (`architecture/testing.md` § Default app and nest mode), so android's
  render is compile-proved only. **Built 2026-09-22 (macOS + iOS):** one
  shared FaunaKit seam (`Core/SocialInboxFFIMapping.swift`, both apple shells
  paint `NotificationItem.body`) resolves `notification_text_for`'s
  `.localized` arm through `renderLocalizedText` and paints `.verbatim` as-is;
  `NotificationItem` carries the raw `summary` alongside the resolved `body`
  so `NotificationOpen.swift`'s router reconstruction hands back the real
  summary, not the already-resolved text; pinned by two FaunaKit tests
  (`SocialInboxFFIMappingTests.swift`) and witnessed on macOS and iOS by
  `test_notifications_localized_body.py`. **Built 2026-09-24 (windows):**
  `NotificationsViewModel.FetchPageAsync` paints `notification_text_for`'s
  `.Localized` arm through `Strings.Resolve`, `.Verbatim` as-is, before
  handing the row to `NotificationItem`; witnessed on windows by
  `test_notifications_localized_body.py`. **Built 2026-09-23 (the knock toast, § Localized body):**
  `knock_push_text`, `FfiKnock.body` and the `knock_text_for` façade, unit
  tested arm for arm against the row's decision; linux's knock toast paints
  it, and android — which raised no knock toast until then — posts one on a
  `fauna_contacts` channel off its knock pump, compile-proved only (no android
  e2e venue). **Built 2026-09-23 (macOS + iOS):** the knock observer
  (`FaunaClient.startKnockObserver`) now keeps the `FfiKnock` `sub.next()`
  returns and raises it through `NotificationManager.postKnockNotification`
  — `knockToastBody(for:)` resolves `knockTextFor`'s `.localized` arm through
  `renderLocalizedText`, same as the notification feed's own body; pinned by
  two FaunaKit tests (`SocialInboxFFIMappingTests.swift`). Tapping the toast
  opens Contacts with the sender pre-selected, the same
  `pendingKnockSenderId` hand-off a tapped notification-feed knock row
  already used (macOS `AppDelegate.swift`, iOS `FaunaApp.swift`, both keyed
  on the toast's `knockSenderId` `userInfo`). **Built 2026-09-24 (windows):**
  `KnockToastText.For(FfiKnock)` (`FaunaApp.Core`) resolves `knockTextFor`'s
  `.Localized` arm through `Strings.Resolve`, `.Verbatim` as-is, title always
  `notifications/knock_title`; `NotificationService.ShowKnockNotification`
  paints it and tags the toast with the sender's 8-hex prefix, so a repeat
  knock from one sender replaces the earlier toast. `KnockReceived` now
  carries the whole `FfiKnock`, not just the sender id. **Built 2026-10-02
  (shared Rust, linux, android):** the sanitizer above lives in
  `fauna_client_notifications::knock_push_text` (witness
  `a_knock_message_reaches_the_toast_as_plain_capped_text`); linux escapes
  body markup in `notify_message`, `notify_group_message` and `notify_knock`;
  android posts every knock under one id. **Still owed (windows):** the
  per-app copy `KnockToastText.Sanitize` (control characters only, 200-char
  cap) is redundant with the shared rule and goes, and the toast tag is per
  sender, so a flood of fresh keys still stacks on windows; whether
  `AppNotificationBuilder.AddText` escapes toast XML is unchecked. **Not
  built:** the nest's offline web-push
  preview for a knock (`routes.rs`, "New knock from …"), which is
  nest-composed English on a channel with no app pipeline behind it and is
  not ruled here.

## Done definition

- [x] All ui.yaml `notifications` elements render with canonical IDs.
- [x] Mark-read is shared Rust; deep-link routing is shared Rust
      (`notification_destination`, § Deep-link destinations) — rendered and
      navigated on tui, the other six apps' glue outstanding.
- [x] Unread badge reads from the shared count.
- [ ] `tests/e2e-unified/ui-actual-<app>.yaml`'s `notifications` block refreshed; `ui-actual-lint` introduces no new errors.

## Reading list

1. `principles.md` — product invariants + engineering principles.
2. `tests/e2e-unified/ui.yaml` — `notifications:` page block + `notification-row` component.
3. `libs/fauna-protocol/src/notifications.rs` — the wire types.
4. `tests/e2e-unified/ui-actual-<app>.yaml`.
