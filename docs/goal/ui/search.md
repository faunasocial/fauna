# Search — target state

Owns: search
Status: ratified — the wire surface was ratified 2026-07-12 (storage-mode axis retired, `../architecture/nest/storage-modes.md`); the typed `SearchSnapshot`, the local/nest merge UX, and the navigation/filter/debounce rows were ratified 2026-08-02 (S0(e) of the backend-2 rollout — § State & data shape). The snapshot build (rollout slice S3) is landed and adopted on all seven apps (`libs/fauna-client-search/src/snapshot.rs`; this header read "pending" until a code check 2026-09-19); § Implementation status today tracks what remains.
Authority: ui.yaml (`search` page) owns element IDs + per-page element scope; this doc owns the Search-page behavior + the page's wire surface (which kind the page calls and what serves it); defers the index architecture (fauna-index/tantivy, build/query/sync model, the per-app matrix) to [`../behavior/content-index.md`](../behavior/content-index.md) and the at-rest property to [`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md) § Search and indexing.

## Goal

The Search page is the cross-content search surface: query the user's posts, conversations, groups, contacts, events, and media, with a type filter to scope results, and render unified result cards.

## The page's wire surface (what actually serves search)

The page calls **`fauna.search.query`** — the sole search surface (the HTTP twin
is deleted; `bins/fauna-nest/src/search_handlers.rs`). **Every nest** answers it
from its SQLite **`content_fts`** FTS table (`db/fts.rs::search_with_scoping`, called from `storage/sealed.rs::SealedStorage::search`)
— the storage-mode axis is retired (`../architecture/nest/storage-modes.md`), and
server-side search collapsed **upward** with it: `content_fts` is fed by the
floor-derived corpus (public post bodies, restricted-post public previews, profile
handles/bios — `../architecture/encryption-at-rest.md` § Readable classes) plus the
policy-gated **public bridge corpus** (`bridge.<id>` rows, owned by
`../behavior/content-index.md` § Bridge content in the Search corpus), so
there is no per-nest availability difference and the former `not_server_side`
refusal is gone — no nest ever returns it. Local/sealed search (mail, calendar,
conversations) is a separate, capability-position story (owner: content-index.md),
not this page's wire surface.

The tantivy/`fauna-index` architecture, the `__index` reserved-set sync model,
and the per-app availability matrix are owned by
[`../behavior/content-index.md`](../behavior/content-index.md) § Two search
backends today — that doc also records that the tantivy index is on **no
serving path yet**; do not conflate it with the live `content_fts` backend
above.

**`content_id` — three writer classes, corrected + ratified 2026-08-02 (S0(e)).**
The earlier blanket claim (`content_id` = `blake3(content_type:natural_id)` for
every row) was drift — the nest has three distinct writers:

- **Post rows (`content_type` `post` / `post/*`): `content_id` IS the real
  64-hex post id** — `db/posts.rs` passes the post id straight to
  `insert_and_index`, no re-hash. This is now a **ratified wire contract**, not
  an accident: shared Rust may mint a Post navigation target from it. The owed
  nest conformance pin is **built** (2026-08-03):
  `bins/fauna-nest/tests/conformance_search.rs::a_post_class_content_id_is_the_post_id_and_navigates`
  walks the real create → search → get journey and never computes an id itself,
  so a re-hash on the writer breaks navigation rather than merely changing a
  string.

**Spelling is part of the contract: `content_id` is LOWERCASE hex** (all three
classes — every row's id is exactly 32 bytes). This is not cosmetic. The merge
below dedups by the raw `(kind class, content_id)` **string**, so if the two
backends spell the same bytes differently the dedup does not error — it
silently never matches, and every post in both backends renders twice with
"local wins" never firing. Until 2026-08-03 the nest emitted **uppercase** here
(SQLite's `hex()`), making search the one 32-byte-id surface disagreeing with
`fauna.posts.create`/`get`, actor ids, and `fauna_core::hex32::encode`; it now
selects `lower(hex(...))` (`db/fts.rs`), and **lowercase is the contract**: the
client's normalize-on-receipt shim for older, uppercase-emitting nests left
with the compat-remnant sweep (`../architecture/version-compatibility.md`
§ Dimension 2). Nothing persists `content_id`
(§ Persistence), so the change needed no migration.
- **Profile rows:** `blake3("profile:" + hex(actor_id))`
  (`db/mod.rs::content_id_for_document`) — one-way; the actor id is not
  recoverable client-side. A profile row names the handle its actor holds **now**, and only
  then: it is derived from `users.handle` inside every write that sets or clears a handle or deletes the account, moves to the successor on a succession, and is re-derived at every boot (`db/fts.rs::sync_profile_row` / `reconcile_profile_rows`) — a renamed account is found by its new handle only, and a retired handle is never searchable.
- **Bridge rows:** `blake3("bridge.<id>:" + natural_id)` — one-way; the nest
  keeps the reverse `bridge_index_map` for its delete triggers only, unexposed
  on any RPC.

So profile and bridge rows still cannot deep-link from the reply; closing that
is a named-but-deferred **additive** `SearchResult` wire field carrying the
natural id (additive-everywhere evolution) — until it ships, those rows are
honestly non-navigable. Apps never parse `content_id` themselves in any class —
the shared snapshot layer (§ State & data shape) is the only reader.

## Layout & flow

- Search bar (component `search-bar`) — query field, submit, type filter, toggle/clear/cancel affordances.
- Results panel (component `search-results-panel`) — `search-results-view`, `search-no-results`, and `search-load-more-button` (shown when the current result count hits the page limit — the API has no cursor yet; clicking bumps the limit by 50 and re-fires the search; hidden when a partial page proves no more rows, **and hidden once the limit reaches the nest's page ceiling**, below — ui.yaml's `search-results-panel` component; built on all 7 apps).

**The page ceiling.** `fauna.search.query` clamps `limit` into
`[fauna_protocol::search::MIN_LIMIT, MAX_LIMIT]` = `[1, 100]` and returns the
truncated page **silently** — there is no "you asked for too much" error. So a
query can surface at most `MAX_LIMIT` rows, and once the client's limit reaches
it, re-firing returns the rows already on screen. `search-load-more-button` is
therefore hidden at the ceiling: an affordance that cannot produce a new row is
not offered. Ratified 2026-08-02 with the paging lift (§ Where logic lives);
before it, every app grew its limit past the ceiling unaware, so the third click
was a no-op that then hid the button by accident — see § Implementation status
today. Raising the ceiling is a wire-behavior change, not a constant tweak —
the skew consequences and the bump precondition live on `MAX_LIMIT`'s doc in
`fauna_protocol::search` (the file a bump must touch).
- Result rows (component `search-result-card`) — `search-result-item` indexed.

## Element IDs

Page elements (from ui.yaml): `page-heading`, `error-message`.
Components used: `search-bar`, `search-results-panel`, `search-result-card`.
ui.yaml owns the authoritative list — see its `search:` block.

## State & data shape

**Ratified 2026-08-02 (S0(e) of the backend-2 rollout; closes the Plan-6 TBD —
build lands with rollout slice S3).** The page reads one typed snapshot from a
shared `SearchManager` in `fauna_client_search`, modeled on the
`FeedSnapshot`/`FeedManager` template (`libs/fauna-feed`) with its two hard
lessons applied: the observer trait is named **`SearchSnapshotObserver`** (a
bare `SnapshotObserver` collides in the C# bindgen's flattened namespace —
CS0104), and snapshot rows are **projections with no serde-flatten catch-all**
(UniFFI/wasm-exposed types cannot carry `extra` maps).

- `search_snapshot()` → `SearchSnapshot { query, type_filter, results: Vec<SearchResultRow>, in_flight, no_results, has_more, error: Option<LocalizedText> }`.
  `query` is the **last fired** query, not the live input buffer (the field
  stays plain local state per § Where logic lives — *Search debouncing*), which
  is what makes "has a search been run" derivable rather than a second stored
  flag. `has_more` is the manager's derived `search-load-more-button`
  visibility — see the paging row in § Where logic lives for why it lives here.
- `SearchResultRow` carries: the badge (`LocalizedText`, via the existing
  render helpers), the cleaned snippet, the timestamp, its **source**
  (`Nest` — backend 1 — or `Local` — the sealed backend-2 replica), and
  `navigation: Option<SearchNav>` — a typed target (`Post { post_id }`,
  `Mail { thread_id, message_id }`, `Draft { thread_id: Option<…> }`, …).
  A **draft** gets its own variant rather than riding `Mail`: `Mail`'s contract
  is "open the thread *and* select this message in it", and an unsent draft has
  no message to select — its target is the composer holding it, and `None` there
  is the thread-less new-thread compose. Local rows always carry `Some` (the
  sealed index stores producer-owned real ids); nest post-class rows carry
  `Some(Post)` via the ratified id contract (§ The page's wire surface); nest
  profile/bridge rows carry `None` until the additive wire field ships. A
  `None` row renders inert — exactly today's behavior on all 7 apps.
- **Faces:** UniFFI manager object + observer (windows/apple/android), wasm
  snapshot-poll without a foreign observer (the feed pattern — the SPA awaits
  the call then re-reads), direct on linux/tui. The **local arm is
  feature-gated off on wasm** (no browser Tantivy — content-index.md § Where
  queries run); the web SPA's manager runs nest-only, structurally.

**The local/nest merge (the two-backend UX):**

- The manager fires both arms concurrently — `fauna.search.query` (backend 1)
  and the local sealed-index query (backend 2, when a replica + keys exist on
  this app). `in_flight` holds until both settle; the local arm settles
  near-instantly, so local hits render first and nest hits merge in. A failed
  nest arm sets `error` and keeps the local rows — partial results are shown
  honestly, never blanked.
- **Dedup by identity, local wins.** For post-class rows the nest `content_id`
  is the post id, so a locally-indexed twin dedups against it; the local row
  is kept (it carries navigation and the private-corpus snippet path).
  Dedup key: `(kind-class, id)`. The id half is compared as a **raw string**,
  so both arms must spell hex lowercase (§ The page's wire surface — *Spelling
  is part of the contract*).
- **Ordering: BM25-family score, descending** — the local Tantivy score and
  the nest's `rank / 1e6` are the same statistic family — tie-broken by
  recency then id for determinism. This preserves today's nest-only ordering
  exactly (`ORDER BY f.rank`). Ordering is a refutable UX detail: an
  implementing session may refine it against real corpora without
  re-ratification, provided the order stays deterministic given both replies.
- **Snippets:** nest rows carry the FTS snippet; local rows render theirs from
  locally-held content (the index stores postings only — D5's local-render
  case). The cross-device snippet RPC (D5's full mechanism) is deferred with
  the classifier ledger.
- **An unresolvable local hit is DROPPED, not rendered inert** (ratified
  2026-08-03 with the local arm's first implementation; wire-resolution
  refinement 2026-08-05). The sealed index
  outlives the client's content store — segments persist across launches, while
  the mail thread store is rebuilt by each launch's mailbox re-walk — so the
  index can legitimately return a content id the store cannot currently
  resolve. Such a hit has neither a snippet nor a navigation target, and
  emitting it would produce an empty row with a dead click *and* falsify the
  "local rows always carry `Some`" rule above. Dropping it keeps that rule true
  by construction, and nothing is *lost*: the hit re-resolves on a later query
  once the store's re-walk catches up. (An earlier revision justified the drop
  by "backend 1 covers the same content from the nest side" — true only for
  the public floor corpus, not for any sealed kind; re-resolution, not backend
  1, is the general guarantee.)
  **For kinds with no client-resident corpus — the third ingest class
  (contacts, posts — ruled 2026-08-05; files — ruled 2026-08-10;
  `content-index-ingest.md` § Ingest triggers, v1 and `content-index.md`
  § Where queries run own it) —
  resolution is a wire read at query
  time**: the kind's own read RPC, batched per query where the wire allows,
  with DROPPED applying to the read's outcome — a read that finds nothing
  (the card or post was deleted, the file was deleted or renamed) drops the
  hit, keeping deletion
  display-correct with no index mutation. A File hit's target is the Media
  page's item detail via `SearchNav::File` (the arm's shape is
  `content-index-ingest.md`'s to own — § Ingest triggers, v1 → the files/media
  ruling); Media as a kind is refuted there for v1. Consequently the local arm settles
  within the network envelope for those kinds rather than near-instantly
  (`in_flight` already holds for both arms), and an offline or failed resolve
  yields no local rows — never an error row.
- **Type filter:** one shared mapping applies the page's filter to both arms
  (the nest `content_type` parameter and the local kind set). Both are read
  through a single **kind class** vocabulary — the union of the local index's
  `ContentKind` variants plus the nest-only `profile` class plus an `Other`
  passthrough for a type this build has never heard of. The class is what makes
  the dedup key meaningful across two id vocabularies, and it is the one
  classification the badge map is also expressed over, so a row's badge and its
  merge identity can never disagree. A filter that selects no local class (today
  `profile`) **skips** the local arm rather than asking it a question with a
  guaranteed empty answer.
- **The filter has three arms, not two, and all three are shared:** which tokens
  exist (`TYPE_FILTER_OPTIONS`), what each selects (the nest parameter + the
  local kind set), and **what each is called**. The display arm
  (`type_filter_label`) is expressed over the same badge map, so an option is
  labelled with the very `LocalizedText` the rows it selects carry — a picker
  and its own results can never read differently. An unrecognised token surfaces
  **raw**, exactly as an unknown row badge does; it is never absorbed into the
  "All" label, which would render a second option reading "All".

## Implementation status today

- **Wire + backend:** `fauna.search.query` over `content_fts` is
  live and serves the Search page on all seven apps, uniformly on every nest
  (§ The page's wire surface).
- **The `content_id` contract is pinned, and its spelling fixed (2026-08-03).**
  The owed nest conformance pin exists
  (`conformance_search.rs::a_post_class_content_id_is_the_post_id_and_navigates`),
  and writing it surfaced that the nest emitted **uppercase** hex here while
  every other 32-byte-id surface emits lowercase — latent today (backend 2
  serves only mail, so no post-class row is in both arms yet) but a silent
  double-render the moment S4 adds post kinds to the local index. Both halves
  landed: the nest now selects `lower(hex(...))`, and the manager normalized on
  receipt for older nests (retired with the compat-remnant sweep, 2026-09-24).
  The nest side is pinned.
- **`SearchNav::Draft` landed 2026-08-05** with the drafts arm of backend 2
  (`../behavior/content-index-ingest.md` § Ingest triggers, v1 → *Drafts are a snapshot
  kind*). It landed purely **additive on all 7 apps** — at the time no app
  acted on a `SearchNav` variant, so nothing needed a match arm. Acting on it
  (opening the composer from a search row) has since landed on all seven apps
  (§ Implementation status today → the result-row navigation paragraph).
- **`SearchNav::Contact` landed 2026-08-06** with the contacts arm
  (`../behavior/content-index-ingest.md` § Ingest triggers, v1 → the contacts ruling).
  Carries the card's `uid_hash` (hex-lowercase) — the identity that survives an
  in-place vCard edit — and was **additive on all 7 apps** at the time for the
  same reason `SearchNav::Draft` was: no app acted on a variant yet. Its snippet
  is the card's *current* wire text, resolved at query time, so an edited card
  reads correctly even before the walk restages it. (tui now acts on it — see
  *`Contact` navigation landed 2026-08-10* below for the `uid_hash`→`card_id`
  resolve that took.)
- **Local `Post` rows landed 2026-08-10** with the posts arm of backend 2
  (`../behavior/content-index-ingest.md` § Ingest triggers, v1 → the posts ruling's
  BUILT sub-bullet). `SearchNav::Post` itself predates this (backend 1's rows
  mint it — § The page's wire surface); what is new is local rows carrying it:
  the user's own posts resolve per hit over `fauna.posts.get` at query time, a
  deleted post's hit is DROPPED (display-healed), and a local row dedups
  against its backend-1 twin by the `(kind class, id)` key with local winning —
  the exact collision the 2026-08-03 hex-spelling fix above was made for, now
  exercised for real (tier_3-pinned:
  `a_pre_existing_post_is_a_navigable_local_row_after_the_attach_walk`).
- **`SearchNav::File` and local `File` rows landed 2026-08-10** with the File arm
  of backend 2 (`../behavior/content-index-ingest.md` § Ingest triggers, v1 → the
  files/media ruling's BUILT sub-bullet, which owns the arm's shape). The variant
  carries the **durable identity pair** — the set's stable `FolderSummary.id`
  plus `path_hash` (hex-lowercase) — and deliberately not the set *name*, which
  the user may rename at any time. It is **additive on all 7 apps** for the same
  reason `SearchNav::Draft` and `SearchNav::Contact` were: no app acts on a
  variant yet. The snippet is the file's *current* path, unsealed client-side at
  query time, so a file whose label this seat cannot open contributes no row
  rather than an empty-named one; a deleted **or renamed** file's hit is DROPPED
  (a rename is structurally delete + create, so both verbs heal identically).
  ⚠ **The acting-on leg owed an explicit identity→item lookup**, not a
  spelling match: the Media page keys its item state on rendered row fields, not
  on this pair — pre-stated here because the `SearchNav::Contact`
  `uid_hash`→`card_id` mismatch is the same class, and it fails *silently*.
  **Both were discharged 2026-08-10 the same way** — a shared lookup that joins
  on the durable identity, never a cast at the call site; see *`Contact`
  navigation* and *`File` navigation* below.
- **Shared render lift — adopted on all seven apps:** the prefix-aware badge map
  + FTS-marker snippet cleanup live in `libs/fauna-client-search/src/render.rs`
  (`content_type_badge` → `LocalizedText`, `clean_snippet`), exposed as UniFFI
  `search_content_type_badge`/`search_clean_snippet` (windows/apple/android) and
  wasm `searchContentTypeBadge`/`searchCleanSnippet` (web); linux and tui call
  the crate directly. Six apps adopted it by 2026-06-22 (last of that batch:
  apple); tui — not yet an app at the time — called it directly from its
  first search page (M5 slice 8, 2026-07-16), closing out all seven. The
  earlier per-app exact-match maps (`post/*` bug, `imap`→Message wording,
  leaked `<b>` markers) are gone. **Every known kind class carries a labelled badge since 2026-09-21** (`kind.rs::class_badge_key`, pinned against the string table by `every_known_class_carries_a_badge_the_string_table_resolves`): the local-index classes — conversation message, contact, file, draft, media — had passed through raw "until they have rows to label" long after their arms shipped, so a draft hit painted the wire token `draft` on every app; only `Other` stays raw. **The nest's arm of the type filter matches a CLASS since 2026-09-22** (`db/fts.rs`; the `content_type` doc on `fauna_protocol::search`): a post is stored under its body subtype (`post/text`, `post/media`, …) and the nest compared `schema = 'post'` exactly, so narrowing to posts dropped every nest post on all seven apps — the filter's local arm and the badge map were prefix-aware all along; now `post` reaches `post/*` on the nest too, while a full subtype still narrows to itself.
- **Shared type-filter LABEL lift — landed on all 7 apps, complete 2026-08-27
  (windows last).** The option list had been shared since 2026-08-04,
  but the *label* for each option stayed a per-app hand-rolled match, and the
  six copies had already drifted: **windows labelled `profile` "Contacts" and
  `imap` "Messages"** (its own `common/posts`/`common/contacts`/`common/messages`
  key set) where linux/web/android/apple read "Profile"/"Email", and **tui
  painted the bare wire token** (`imap`). Every copy was a *closed* match with an
  `_ => all` fallback, so a token added to the shared `TYPE_FILTER_OPTIONS` would
  have rendered a second option reading "All" on five apps at once. The map now
  lives once as `fauna_client_search::type_filter_label` (`kind.rs`, expressed
  over `badge_for` — § State & data shape → *Type filter*), exposed as UniFFI
  `search_type_filter_label` and wasm `searchTypeFilterLabel` beside the existing
  options faces. **tui** and **linux** call the crate directly (tui through the
  `Role::Select` `display` value, so `get_text`/`select` keep round-tripping the
  raw token). **web** and **android** adopted it 2026-08-20, each deleting its
  own closed match and resolving the returned `LocalizedText` through the
  resolver it already had — web `resolveLocalized(searchTypeFilterLabel(token))`
  in `+page.svelte` over a new `$lib/wasm` wrapper, android
  `resolveLocalized(context, com.fauna.ffi.searchTypeFilterLabel(token))` in
  `SearchResultsScreen.kt`. Both resolvers already surface an unknown key raw,
  so the unrecognised-token arm holds without app glue, and the `<option>` /
  chip value stays the wire token. **apple** (macOS + iOS) adopted it
  2026-08-21, deleting its own closed `switch` and resolving the returned
  `LocalizedText` through the existing `renderLocalizedText` pipeline — the
  same one `localizedBadge` already uses — at both call sites
  (`SearchResultsView.swift`, `ContentView.swift`); the `Picker` tag stays the
  raw wire token, only the `Text` changed. **windows** adopted it 2026-08-27, deleting its own closed match
  (`common/posts`/`common/contacts`/`common/messages`, which had read
  "Contacts"/"Messages") and resolving the returned `LocalizedText` through
  `Strings.Resolve` — the same pipeline `SearchResult.FromRow`'s badge already
  uses — in `SearchResultsPage.PopulateTypeFilter()`; the `ComboBoxItem.Tag`
  stays the raw wire token, only the `Content` changed. No e2e drives
  `search-type-filter`'s rendered option text on any app — not a per-app gap
  but a driver-contract one: `get_text` on a select-type element reports its
  current **wire value**, never a rendered label, by deliberate cross-app
  convention (`actions/admin.py::registration_mode`'s docstring states it
  explicitly), and no driver exposes a "read option N's label" primitive.
  Building one is its own piece of work, not a side effect of a wording fix.
- **Shared paging lift — landed 2026-08-02, adopted on all 7 apps.** The
  policy now lives once in `fauna_client_search::paging` (§ Where logic lives).
  **tui** (`apps/fauna-tui/src/search.rs`) and **linux**
  (`apps/fauna-linux/src/views/search.rs`) consume it directly and their local
  `INITIAL_LIMIT`/`LOAD_MORE_STEP` constants are deleted; **web**
  (`apps/fauna-web/src/routes/search/+page.svelte`) consumes it transitively —
  its own `resultLimit`/`loadingMore` state and all three bare literal `50`s
  are gone, since the shared snapshot below now owns paging entirely (2026-08-04).
  **apple** (macOS + iOS, `FaunaKit/.../ViewModels/SearchVM.swift`) adopted it
  2026-08-04 with the manager below — its `currentLimit`/`initialLimit`/
  `loadMoreStep` fields are gone; `hasMore` reads straight from the snapshot.
  **windows adopted 2026-08-05** with the manager below — its
  `InitialLimit`/`LoadMoreStep` fields and the fetch-time-stored `HasMore` are
  gone; paging is entirely the manager's. **android adopted 2026-08-09** with
  the manager below — `SearchVM.kt`'s `INITIAL_LIMIT`/`LOAD_MORE_STEP`
  constants and its `currentLimit` StateFlow are gone; `search-load-more-button`
  now reads `snapshot.hasMore` directly, closing the ceiling bug (its third
  "load more" used to ask for 150, receive the same 100 rows, and hide its own
  button).
- **The shared snapshot — BUILT 2026-08-03, adopted on all 7 apps.**
  `fauna_client_search::SearchManager` now owns the query/filter/paging/merge
  decisions and publishes `SearchSnapshot` (§ State & data shape), with the
  `SearchSnapshotObserver` reactivity the feed and conversations pages already
  use. **tui** (`apps/fauna-tui/src/search.rs`) consumes it and is now a paint
  shell: its per-page `type_filter` / `last_query` / `SearchPaging` / `results` /
  `searched` state is deleted, and it composes no badge or snippet itself.
  **web** (`apps/fauna-web/src/routes/search/+page.svelte`) adopted it
  2026-08-04 the same way, over a new wasm face (below) rather than an
  observer — web has no foreign-callback boundary, so the SPA `await`s each
  async manager method then re-reads `snapshot()` (the `WasmFeedManager`
  pattern). Its client-side type-filter post-filter (comparing the
  *localized badge label* against `<option value>`, which happened to also be
  the label — never the wire `content_type`) is gone: `search-type-filter`'s
  values are now the shared `TYPE_FILTER_OPTIONS` tokens, re-firing the query
  through the manager's one shared kind mapping on every change, matching
  tui/linux.
  **linux** (`apps/fauna-linux/src/views/search.rs`) adopted it 2026-08-04, the
  `LinuxFeedManager`/`crate::feed` observer-bridge shape mirrored as
  `crate::search::host` + `crate::search::observer` (a process-wide manager
  slot built at `build_main_window`, a GTK main-thread refresh loop off
  `SearchSnapshotObserver`) — a direct-Rust consumer like tui, no FFI hop. Its
  hand-rolled `SearchState { paging, content_type, searched }` and the
  `fauna.search.query` call in `client.rs` are gone.
  **apple (macOS + iOS) adopted it 2026-08-04** — `FaunaKit/.../ViewModels/SearchVM.swift`
  is now a thin `@Observable` proxy over `FfiSearchManager` (the `FeedVM`
  pattern): `results` / `localResults` / `currentLimit` / `searchLocal()` /
  `highlightSnippet()` are gone, and both `Fauna-macOS` and `Fauna-iOS`
  `SearchResultsView.swift` render the ONE merged `[SearchResultRow]` list off
  `manager.snapshot()`. `search-type-filter` is wired for real on both targets
  (iOS's had been a `.constant("all")` stub with no macOS mirror; macOS had no
  filter at all) — a `Picker` over `FaunaFFISwift.searchTypeFilterOptions()`,
  mapped to a label through the same `search_page` badge keys web/linux reuse
  (`searchTypeFilterLabel`, `FaunaKit/.../Models/SearchResult.swift`) — since
  superseded by the 2026-08-20 label lift above; apple deleted the local match
  and adopted `search_type_filter_label` 2026-08-21 (§ *Shared type-filter
  LABEL lift* above).
  **windows adopted 2026-08-05** — `SearchViewModel`
  (`FaunaApp.Core/ViewModels/SearchViewModel.cs`) is now a thin observer over
  `FfiSearchManager` (the `FeedViewModel` pattern): its own paging fields are
  gone and `SearchResultsPage` renders the ONE merged `SearchResult` list off
  `Snapshot()`. `search-type-filter`'s options come from the shared
  `search_type_filter_options()` token list (below), which also retired the
  combo's dead `"file"` tag — not a real nest `content_type`, so that option
  always returned zero rows. Its `TypeFilterLabel` match had kept a
  **different key set** from every other app (`common/posts`/
  `common/contacts`/`common/messages`), so the `profile` option read
  "Contacts" and `imap` read "Messages"; the 2026-08-20 label lift above
  superseded it, and windows adopted `search_type_filter_label` 2026-08-27
  (§ *Shared type-filter LABEL lift* above), fixing the wording.
  **android adopted 2026-08-09** — `SearchVM.kt` is now a thin proxy over
  `FfiSearchManager` (via a new `SearchManagerHost` process-wide singleton,
  the `FeedManagerHost` pattern) and `SearchResultsScreen.kt` renders the ONE
  merged `SearchResultRow` list off `snapshot()`; its `results` StateFlow,
  client-side `selectedFilter` post-filter (which keyed on a per-app literal
  set — `"conversation"`/`"group_message"`/`"feed_post"`/`"event"` — that
  never matched a real nest `content_type` at all, so the filter chips were
  silently inert against real data), and raw `FfiSearchResult` composition are
  gone. `search-type-filter`'s options come from the shared
  `search_type_filter_options()` token list, closing the same dead-filter gap
  windows' adoption closed for its combo. The now-fully-dead raw
  `fauna.search.query` Kotlin wrapper (`ApiClient.search`, `FfiSearchClient`
  plumbing) is deleted with it — the manager façade is the only search path
  left in the app.
  **All seven apps have now adopted the shared snapshot.** The UniFFI façade
  (`libs/fauna-ffi/src/search_manager.rs`'s `FfiSearchManager`, wrapping the
  concrete `SearchManager<Arc<NestClient>>` — the generic can't be
  `#[uniffi::export]`ed — built via `FfiNestClient::search_manager()`, the
  `FfiFeedManager` pattern, gated `search-manager`, default-on) and the wasm
  façade (`libs/fauna-wasm/src/search.rs`'s `WasmSearchManager`) both landed
  2026-08-03/04 and are now fully drained — no leg remains gated on a manager
  face.
- **The local arm (backend 2) is REGISTERED and serving — 2026-08-03, on tui.**
  `fauna_client_index::MailLocalSearch` implements the `LocalSearchIndex` seam
  over the sealed mail/calendar replica: it opens the replica off the `__index`
  rail, queries it off the reactor, and projects each hit into a display row —
  the snippet and the `SearchNav::Mail` target both resolved from the
  conversations thread store, per the merge rules above. `tui` registers it at
  the post-auth hook, so a mail indexed this session is found by the Search
  page and merges into the nest rows under one relevance order.
  **The one holder of the MSEK is the registrar:**
  `fauna_client_conversations::NestMailIndexLauncher` owns both the build side
  (`launch`) and the query side (`local_search_index`), and hands app glue an
  opaque index — never a key. A login with mail disabled has no arm to register
  yet, and the page is nest-only meanwhile — the same rendered state as a device
  with no published index. **Since 2026-08-05 that state is no longer permanent
  for the process:** glue registers a `LocalIndexResolver` and the manager mints
  the arm on the first query made after mail is enabled, so enabling mail
  mid-session makes that session's mail searchable. The rule and its build-side
  twin are owned by `../behavior/content-index-ingest.md` § Ingest triggers, v1 → *An
  arm attaches when its precondition arrives*. **linux registered 2026-08-10**
: `conv_backend.rs`'s
  `start_conversations_session` calls `crate::search::host::manager()` and
  registers the resolver right where it builds the `NestMailIndexLauncher`,
  needing no new process-wide slot — `conversations::host::manager()` re-reads
  the same singleton the caller's own `manager` param came from, so nothing
  needs threading down from the `AuthSuccess` arm; e2e-proven by
  `test_search_local_index.py[linux]` (real MTA delivery → local index →
  Search finds it). **android registered 2026-08-10** with its `SearchManager`
  adoption (`SearchManagerHost.attachLocalIndexIfNeeded`, called from
  `SearchVM.kt`) — production-reachable, but not yet in
  `test_search_local_index.py`'s `pytestmark` (android e2e is host-emulator-
  gated fleet-wide, same standing block as every android e2e track — not
  specific to search). **windows registered 2026-08-31:** its
  `SearchResultsPage` makes the one call below (`AttachLocalSearchIndexAsync`)
  once the manager is built, and `test_search_local_index.py`'s `pytestmark`
  carries `windows`. **Never registered:** web has no local arm by design
  (§ State & data shape — off on wasm, structurally) and never will — this is
  also why `SearchNav::Mail` can never reach web in production (`conversations.md`
  § The selected message).
  **The registration path for the UniFFI legs EXISTS as of 2026-08-03:**
  `FfiNestClient::attach_local_search_index(manager)` (`libs/fauna-ffi`), the
  FFI twin of the two lines tui runs at its post-auth hook. It is a method on
  the *client* rather than a `set_local_index` on `FfiSearchManager`, because
  `LocalSearchIndex` is deliberately not a UniFFI-exported trait: the
  implementation is minted Rust-side by `NestMailIndexLauncher`, which owns the
  MSEK and hands app glue an opaque index, never a key
  (`../behavior/content-index.md` § Encryption posture). Exporting the seam as a
  foreign trait would invert that ownership; routing the registration through the
  client that already holds the launcher preserves it, and no key crosses the
  boundary in either direction. It answers `false` — a **normal state, not an
  error** — when there is no session yet or the actor has no mail.
  The local arm reaches the sealed index through a `LocalSearchIndex` trait
  seam, which is what makes "off on wasm" structural: the crate depends on
  neither tantivy nor an index crate, so the SPA's WASM graph cannot contain one
  even by accident. **tui, linux, apple (macOS + iOS), android and windows
  register one today** — apple's `SearchVM.attachLocalIndexIfNeeded()` calls
  `APIClient.attachLocalSearchIndex(manager:)` once the manager is built and
  again on WS-RPC reconnect (best-effort, mirroring tui: the first attempt can
  race the login-time `conversationsSession` build that stashes the launcher,
  so a reconnect is the retry); android's `SearchManagerHost.attachLocalIndexIfNeeded()`
  mirrors the same shape, retried on each Search-page entry rather than on
  reconnect (a `false` return is idempotently retryable either way — no
  reconnect-tick plumbing is owed unless a session finds the page-entry retry
  insufficient in practice). The manager runs nest-only on the remaining legs,
  which is a normal state and not an error state.
  **tui now acts on a result row's navigation (2026-08-10, the lead-app
  rule); linux, android, and web landed the same trickle-down 2026-08-14/15;
  macOS and iOS followed 2026-08-25** — one
  shared FaunaKit `SearchResultsView.openResult` covers both apple targets —
  `search-result-item[i]` navigates on `Post` (post detail; ids
  match the ratified `content_id` contract, e2e-verified both apple
  targets), `Draft` (both the thread-tied and the thread-less single-slot
  composer cases), and `Mail` (the thread jump only — see the next bullet).
  **windows closed the routing half 2026-08-26**: `SearchResultsPage.SearchResult_Click`
  switches on the clicked row's `Nav` and routes all five variants, including
  the `Post` deep-link's fetch-if-unloaded leg (`FfiFeedManager.ResolvePost`,
  the same `deepLinkedPost` union every other app reads).
- **`Mail`'s message-highlight half LANDED 2026-08-10 on tui (the lead app),
  closing the contract's second half.** It was filed the same day as an open
  cross-app affordance track — nothing in the product distinguished one message
  *inside* a thread, so the `message_id` had no seam to land on — and narrowing
  the contract to drop the field was rejected then and stays rejected: the index
  carries it for free, and a mail hit in a long thread without it makes the user
  re-find their own result by eye.
  The concept now exists, and it exists **shared**:
  `ConversationsManager::select_thread_and_message` + the read-time-resolved
  `ThreadDetail.selected_message_id` (owner: [conversations.md](conversations.md)
  § The selected message, which owns the page behavior — this bullet only
  records that `Mail` is what drives it). No new ui.yaml id was needed: the
  observable is a `selected` attribute on `dm-message-timestamp`, the
  `recipient-resolve-status` precedent for a state of an existing element.
  **Not closed by the 2026-08-14/15 search-navigation trickle-down itself**: that leg's `Mail` arm only had to route somewhere real, and at
  that point linux/web routed to the plain thread-jump only, deferring the
  paint half. **linux closed both halves the next day (2026-08-15, row
  197)** — its `Mail` arm now calls `select_thread_and_message` too, and
  `message_bubble.rs` paints the `selected` attribute on every render arm,
  mirroring tui (`conversations.md` § The selected message carries the
  current per-app table). android's routing already called
  `select_thread_and_message`, and its own paint leg landed the same day
  (2026-08-15) alongside linux's. **macOS closed both halves 2026-08-25** — its
  `Mail` arm now calls `selectThreadAndMessage` and `DmMessageBubble.swift`
  paints the `selected` attribute on every render arm, e2e-proven
  (`test_search_local_index.py` green `--app macos`). **iOS landed the same
  paint the same day** (shared FaunaKit code with macOS); `test_search_local_index.py`
  still cannot exercise it, since `CLIENT_BUILDS_INDEX` is `false` there and a
  single-seat run seeds no local index to search on a phone, but **real-simulator
  grading closed 2026-08-27** via a new
  `conversations_select_message` test-only command that drives the identical
  `ConversationsVM.selectThreadAndMessage` call a real `SearchResultsView` row
  tap makes: `test_conversations_selected_message.py` green `--app ios` on a
  real booted simulator. **windows closed both
  halves 2026-08-26** — its
  `Mail` arm calls `ConversationsViewModel.OpenThreadAndMessage` →
  `SelectThreadAndMessage`, and `DmMessageBubble` paints the `selected`
  attribute on every render arm plus an amber ring (cross-app-consistent with
  linux/android) and scrolls the marked bubble into view. Its end-to-end
  witness became possible 2026-08-31, when windows registered its local search
  arm (the registration bullet above): `test_search_local_index.py` now lists
  `windows` among the apps its selected-message walk exercises, so a
  `Mail`-class row can reach the result list in a Windows test run; the paint
  itself is exercised by construction (every bubble render path always calls
  `Bind`) as well as by that live `SearchNav::Mail` hit. web never reaches
  either half — `SearchNav::Mail` cannot occur on web by construction
  (previous bullet). Remaining work is captured in the per-app trickle-down
  NEXTs.
- **`Contact` navigation landed 2026-08-10 on tui, as an id-space RESOLVE
  rather than a wire-up.** Activating a contact row opens that card's detail in
  the Contacts page's Address Book segment. The row carries the card's
  `uid_hash` and every Address Book keys its open card on the server-assigned
  `card_id`; the two are the same width and both hex, so passing one where the
  other is expected opens nothing and raises no error. The join is therefore a
  read — `fauna_client_carddav::CardDavClient::locate_card_by_uid_hash`, shared
  so no app re-derives it — which walks the actor's books and answers with the
  holding book, its decoded cards, and the target `card_id`. Reading rather
  than scanning loaded state is load-bearing for a second reason: the holding
  book need not be the one the user last opened, or opened at all, and
  resolving against whatever is in memory would render nothing for exactly the
  case the feature exists for (the class `feed.md`'s deep-link slot closes for
  posts — *the loaded page is not the addressable universe*). A card no book
  holds any more — deleted between being indexed and being clicked — surfaces
  on `error-message` rather than opening a blank pane: the same DROPPED outcome
  a query-time resolve gives, arriving one click later with a user waiting on
  it. **linux, android, and web landed their navigation lift 2026-08-14/15**,
  each over its own reachable `locate_card_by_uid_hash` face (native for
  linux, a new UniFFI export for android, a new wasm export for web — none of
  the three re-derives the id join). **Their DROPPED handling did not
  actually match tui's `error-message` outcome until 2026-08-27**: until then
  a `None` resolve on linux/android silently repainted the book picker with
  no signal at all, and web showed a generic off-element "Not found" string
  instead of the ratified `card_not_found` copy — web's address-book error
  render site had no canonical `error-message` element at all until that
  fix. All three now surface the same
  `contacts.address_book.card_not_found` string on `error-message` as tui and
  apple. **windows landed its page glue
  2026-08-26** —
  `SearchResultsPage.OpenContactAsync` calls the same shared
  `INestRpcClient.CarddavLocateCardByUidHashAsync` → `FfiCarddavClient.
  locate_card_by_uid_hash`, picks the located `FfiFoundCard`'s matching row by
  `id == card_id` (never `cards[0]` — the holding book's WHOLE card list rides
  along, per `FfiFoundCard`'s own doc), and navigates to `CardDetailPage` on a
  found card. **windows closed its DROPPED-case gap 2026-09-06** — a `None` resolve (deleted card, or
  a stale index row with no matching entry) now deep-links to `ContactsPage`'s
  Address Book segment instead of `OpenContactAsync` silently returning:
  `ContactsPage` repaints whatever book picker rows still exist (something
  `SearchResultsPage` itself has no picker to repaint) and surfaces the
  ratified `contacts/address_book/card_not_found` string on `error-message`,
  matching every other app's leg. `test_activating_a_contact_search_result_for_a_deleted_card_surfaces_error`
  is now a real, unskipped assertion on windows (a local search arm is
  registered, so a `Contact`-class row does reach windows' result list in a
  test run — the `skip_if_no_local_search_arm` degrade no longer applies).
  **macOS and iOS landed
  the page glue 2026-08-25** (`AddressBookVM.locateCard`,
  the same shared read). **The macOS e2e leg is GREEN as of 2026-08-25**. Its two blockers were neither this join nor the
  suspected local-search-index content-coverage gap, and both are closed:
  (i) apple's e2e login built no `ConversationsSession` unless the test opted
  into the real conversations backend, and the index arm *and* builder are
  created inside that factory — so nothing published segments and nothing
  queried them, and the `search-result-item` count stayed zero for every
  client-seeded Contact/File hit (the Post arm was unaffected throughout,
  being served by the nest arm); (ii) `AddressBookVM.configure` guarded its
  lazy fetch with a flag set *before* its `await`, so the deep-link task —
  which must configure then locate — returned while `carddav` was still nil
  and `locateCard` no-op'd in silence onto a page that then filled in behind
  it. **iOS is witnessed by a desktop-seat twin, not declared absent:** a
  phone builds no index of its own (§ Build vs. query, `CLIENT_BUILDS_INDEX`),
  so a single-seat run never publishes a segment for it to query — but the
  phone's arm queries a synced replica exactly like a desktop's, so its
  witness is a twin in which a desktop seat of the same account indexes the
  card and the phone activates the row, both the found and the deleted-card
  branch (`../architecture/feature-catalog.md` § Implementation status today,
  the 2026-09-26 marked-witness settlement).
- **`File` navigation landed 2026-08-10 on tui, discharging the pre-stated
  identity→item lookup.** Activating a file row opens that file's
  `media-item-detail` on the Media page. The lookup is
  `fauna_client_media::MediaSnapshot::locate_file(folder_id, path_hash)`
  (reached by every app through `MediaMachine::locate_file`), and three of its
  properties are load-bearing rather than incidental:
  - **It joins the set by `id`, not by name.** `MediaFolder` now carries the
    control plane's `FolderSummary.id`; the page's rows carry the set's
    *name*, which a rename moves. Joining on the id is what makes a reference
    minted before a rename still land after it.
  - **It matches the row's own wire `path_hash`, not a hash recomputed from
    `path`** — the same field the index's walk derives a file's identity from,
    so the two halves mint the identical spelling by construction. A row that
    carries none (a reader outside the set's label audience, or a nest
    predating the field) cannot be addressed by identity and answers no rather
    than guessing; that file has no indexed hit to resolve either, for the same
    reason.
  - **It runs over the raw drained aggregate, never the rendered view.** The
    active `media-folder-filter` and sort are the user's browse state and say
    nothing about what they just asked to open; `fauna.media.list` is drained
    to exhaustion, so the raw aggregate is every readable item and the lookup
    has no "not paged in yet" case. tui additionally points the filter at the
    opened file's set, so closing the detail cannot land on a list excluding
    it.

  A pair that resolves to nothing — deleted, renamed (structurally delete +
  create), or in a set this seat cannot read — surfaces on `error-message`,
  the same DROPPED outcome one click later that the `Contact` case takes.
  **linux, android, and web landed their navigation lift 2026-08-14/15**, each
  over `MediaMachine::locate_file` (already on every app's reachable face —
  the UniFFI export existed; only web's wasm mirror was new).
  **windows landed its page glue 2026-08-26** — `SearchResultsPage.SearchResult_Click`'s
  `File` arm navigates to `MediaPage` carrying the durable
  `(folder_id, path_hash)` pair via `ServiceClients.DeepLinkFile`, and
  `MediaPage.Page_Loaded` resolves it through `MediaMachine.LocateFile` (the
  same shared lookup, never the rendered row's name) before opening the
  detail sheet — e2e-unverified for the same no-local-arm reason as `Contact`
  above. **macOS and iOS landed the page glue 2026-08-25** (`MediaFileLocate` — a process-wide staged
  target actively resolved via `MediaMachine.locateFile`, the durable
  `(folder_id, path_hash)` pair, never the renameable `MediaDeepOpen` name+path
  the FP-context door uses). **The macOS e2e leg is GREEN as of 2026-08-25** — it was blocked by blocker (i) under `Contact`
  above (no `ConversationsSession` under e2e ⇒ no index arm and no builder),
  never by this lookup, and needed no change of its own once that was fixed.
  **iOS is witnessed the same way `Contact` is** — a desktop seat of the same
  account indexes the file, and the phone opens its detail across the active
  filter.
  **The `Post` target's fetch gap is CLOSED (2026-08-10)** — activating a post
  row opened the detail sub-page but rendered it *empty* for a post the feed
  query never loaded, which is the common case for a search hit. The shared
  `FeedManager::resolve_post` now fetches such a post into the snapshot's
  one-slot deep-link surface; the design and its ratified properties are owned
  by `feed.md` § The read model → *Opening a post the timeline never loaded*.
  Because the gap was structural to `libs/fauna-feed`, the other apps
  inherit the fix with their own navigation lift — each needs only to read
  `FeedSnapshot::find_post` instead of `snapshot.posts` on its `post_detail`
  surface, and to drive `resolve_post` on detail-open. **linux, android, and
  web landed this 2026-08-14/15** (`resolve_post` needed a new UniFFI export
  for android and a new wasm export for web; linux calls the shared crate
  natively, same as tui). linux additionally had to make its detail pane
  switch to a visible loading state IMMEDIATELY on open rather than only
  after the resolve completes — matching tui's synchronous `Mode::PostDetail`
  flip — since the alternative left the destination invisible for the whole
  round trip. The taken-down refinement
  is **built too (2026-08-10)**: `PostSummary.legal_takedown_ref` (the
  `QuotedPostView.legal_takedown_ref` twin) carries the reference and
  `resolve_post` parks a tombstone summary in the deep-link slot, so a hit on a
  legally-taken-down post opens `post_detail` and shows the shared tombstone in
  the **body area** rather than on `error-message`. Adopted on tui, linux,
  android, and web. **windows landed its `if` 2026-08-26** — `FeedPostItem.IsLegalTakedown` /
  `.LegalTakedownDisplayBody` (the exact twin of the quoted embed's own
  fields) gate `BuildPostDetailPanel`'s body paint, checked FIRST and ahead of
  every other detail element, mirroring `DmMessageBubble`'s collapse-to-
  tombstone posture — closed for BOTH open paths (the ordinary post-card
  click and the search-nav deep link) since they share one panel builder.
  macOS and iOS inherit the shared half and add
  one `if` on their `post_detail` body
  surface with its navigation lift.
- **macOS's bespoke pre-manager "local results" list is RETIRED — apple (macOS
  + iOS) adopted the shared manager 2026-08-04.** The `Fauna-macOS`-only split
  section under an *On This Device* header, fed by `SearchVM.searchLocal()`'s
  `localizedCaseInsensitiveContains` substring scan over the media file list,
  is gone; both apple targets now render the one merged, relevance-ordered list
  the manager publishes, with rows sourced from the nest and (once the local
  arm attaches) the sealed index — never a split section. The retired
  `local_results_header` / `nest_results_header` / `searching_nest` /
  `local_type_*` i18n keys are deleted from `en.yaml` and every generated file.

## Where logic lives

- **Query execution.** The nest, behind `fauna.search.query` (`content_fts`,
  uniformly on every nest) — § The page's wire surface. The future
  local/sealed-content search path is owned by content-index.md; when it
  lands, the page still consumes one shared query seam, never a per-app
  engine.
- **Paging policy (page size, "load more" growth, load-more visibility).**
  `libs/fauna-client-search/src/paging.rs` — `SearchPaging::initial()` /
  `::load_more()` / `::has_more(result_count)` / `::at_limit()`, over
  `fauna_protocol::search`'s `DEFAULT_LIMIT`/`MIN_LIMIT`/`MAX_LIMIT` (the one
  owner of the nest's clamp, applied nest-side by
  `search_handlers.rs::query_handler`). Exposed as UniFFI
  `search_paging_initial_limit`/`_load_more`/`_has_more` (windows/apple/android)
  and wasm `searchPagingInitialLimit`/`searchPagingLoadMore`/`searchPagingHasMore`
  (web); linux and tui call the crate directly. **A state object, not loose
  constants** — the three decisions are coupled (a limit that outgrows the
  ceiling makes the visibility predicate ask an unanswerable question), and it
  is the *predicate* that had drifted per-app, not the numbers. A consumer never
  re-derives the growth step or the visibility rule. Ratified 2026-08-02.

  **Who holds the limit** (settled 2026-08-03 with the manager build — a
  refinement of the above, not a reversal): the **`SearchManager`** does, and it
  publishes the answer as `SearchSnapshot.has_more`. The 2026-08-02 wording had
  the app keep the limit in its own view model; that was right for the
  pre-manager world and stays right for every app that has not adopted the
  manager yet, but it cannot survive the two-backend merge. `has_more` must be
  evaluated against the **nest arm's** row count, and once local rows are merged
  into `results` that number is no longer visible to the app — an app deriving
  it from the rows it can see would offer a page the wire cannot produce. So: an
  un-migrated app keeps its own `SearchPaging` and asks this face directly; an
  app on the manager reads `has_more` and holds no limit at all.

- **Type filter application.** Shared Rust — the `SearchManager`'s one mapping
  onto both arms (§ State & data shape). *(Ratified 2026-08-02; today each app
  passes `content_type` to the wire itself — collapses into the manager at S3.)*
- **Result ranking / ordering.** Shared Rust — the manager's merged ordering
  (§ State & data shape; refutable UX detail, deterministic always).
- **Result navigation (deep link).** Shared Rust — the snapshot row's typed
  `navigation` target; app glue navigates on `Some`, renders inert on
  `None` (§ State & data shape). Constrained by the corrected `content_id`
  classes (§ The page's wire surface).
- **Search debouncing.** Resolved 2026-08-02: the page is **submit-driven, no
  debounce** — the shape all 7 apps already ship. Incremental
  search-as-you-type, if ever introduced, is a manager-owned policy change,
  never per-app glue.

## User actions

| Element | Action | Where it runs |
|---|---|---|
| `search-query-field` | Update query. | `SearchManager` (S3); today per-app view state. |
| `search-submit-button` | Run query. | `SearchManager` — fires both arms (§ State & data shape). |
| `search-type-filter` | Set type filter. | `SearchManager` — one mapping onto both arms. |
| `search-toggle-button` | Toggle search affordance (per ui.yaml). | App glue (pure view affordance, no search semantics). |
| `search-clear-button` | Clear query. | `SearchManager` (S3); today per-app view state. |
| `search-cancel-button` | Cancel in-flight search. | `SearchManager` — settles the snapshot, drops late replies. |
| `search-result-item[i]` | Open destination. | Shared Rust returns the row's typed `navigation` target; app glue navigates on `Some`, inert on `None`. |
| `search-load-more-button` | Bump the page limit by 50 (saturating at the nest's ceiling) and re-run the query (no cursor yet). Shown only when the result count hits the current limit *and* the limit is below the ceiling. | **Shared Rust** — `fauna_client_search::paging` (§ Where logic lives). Folds into the shared query seam once the wire paginates properly. |

## Persistence

None (resolved 2026-08-02): the snapshot is session-state; nothing search-page
persists. A recent-queries cache remains an optional future refinement — if
introduced, it is manager-owned state, never per-app storage. (The local
*index* the local arm queries is content-index.md's domain, not page state.)

## Errors & edge cases

- `error-message` page-level.
- `search-no-results` for empty result sets (both arms settled, zero merged rows).
- Network failure / nest-arm error: the snapshot's `error: Option<LocalizedText>`
  is set and any local rows are kept (§ State & data shape — partial results
  shown honestly). Query syntax errors surface the same way (the nest maps
  fts5 syntax errors to `invalid_params`; the local arm reports its parse
  errors through the same field).

## Architectural rules

1. Observer-driven rendering once shared snapshot exists.
2. Indexed list IDs (`search-result-item[i]`).
3. Result navigation routes are shared Rust.

## Don't do these

- Don't implement search per-app. The page consumes the one `fauna.search.query` seam (today: nest `content_fts`; the future local/sealed-content index is content-index.md's design, still behind one shared seam) — never a per-app engine.
- Don't compose result-card text per-app. The shared render helpers (and, post-Plan-6, the snapshot) supply localized strings.
- Don't deep-link from `content_id` — it's an opaque FTS doc key (§ The page's wire surface).

## Done definition

- [ ] All ui.yaml `search` elements render with canonical IDs.
- [ ] Query, type filter, results, navigation all driven by shared snapshot.
- [ ] `tests/e2e-unified/ui-actual-<app>.yaml`'s `search` block refreshed; `ui-actual-lint` introduces no new errors.

## Reading list

1. `principles.md` — product invariants + engineering principles.
2. `tests/e2e-unified/ui.yaml` — `search:` page block + components.
3. [`../behavior/content-index.md`](../behavior/content-index.md) § Two search backends today — the index architecture + availability matrix; and [`../architecture/encryption-at-rest.md`](../architecture/encryption-at-rest.md) § Search and indexing.
4. `tests/e2e-unified/ui-actual-<app>.yaml`.
