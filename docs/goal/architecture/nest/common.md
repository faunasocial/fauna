# Nest: Common Internals — target state

Owns: nest-substrate, client-state-recoverability, factory-reset, serving-port, nat-mode, unreadable-stored-values
Status: ratified
Authority: the shared internals every nest deployment sits on — the SQLite schema + the additive-reconcile/degraded-boot upgrade model, the content-addressed blob store, the content pipeline, the Ed25519 auth substrate, the WS-RPC dispatcher + push-event set, the background-task set, the data-dir layout, and the CLI/artifact wiring — plus the client-state-recoverability invariant (owned here; referenced by `principles.md` § Client-recoverable nest state), factory reset (restart-wipe + deployment-key preservation), the serving-port model (fronted-by-router split + fixed internal loopback), and NAT-mode persistence (`FAUNA_MODE` seed + app-set `nest_nat_mode` row). Defers deployment-mode semantics to `nest/public-mode.md` / `nest/private-mode.md` / `nest/worker.md`, storage modes to `nest/storage-modes.md` + `../encryption-at-rest.md`, ACME/TLS mechanics to `nest/tls-certificates.md`, the host-OS-maintenance lifecycle to `../installers/vps.md` § Host OS Maintenance, transport framing to `../transport.md`, and off-box identity recovery to `nest/box-recovery.md`.

## Implementation status today

The substrate is **fully implemented**: WS-RPC dispatch (bounded per-actor channels, kind-routed `RpcRouter`, idempotency cache, deadline + cancel, typed pushes; subprotocol-bearer handshake — legacy `?token=` gone), one sealed `Storage` implementation (the storage-mode axis and its trait variants are retired — `storage-modes.md` § Implementation status today), the wrapped-blob storage backends, the additive-reconcile + degraded-downgrade-boot upgrade model, the blob store, the background-task set, and the serving-port substrate (per-pillar matrix in § Serving ports). Known gaps, each tracked at its owner: the `serving_port_bind_failed` setup.status surfacing half (§ Serving ports); the factory-reset gating follow-up (cooldown / superadmin role — § Factory reset); the five native-app host-maintenance indicator legs (tracked internally); the per-actor blob ownership index (§ Blob Store → Per-actor ownership — target state, unbuilt). Client-state recoverability has **one known gap, ruled and owed** (the custody materialize verb's interrupted-flip-then-arrival strand — § Client-state recoverability → Implementation status today) — every other audited client-reachable interior write is crash-atomic (the deployment-key durable-file rewrite, the last one, found 2026-08-29 and fixed 2026-08-30 — § Client-state recoverability's closing line) — and the audit is a **standing obligation, not a one-time pass**: any *new* client-driven transition must run the verification question there before it ships. Recorded config-surface violations with app-UI migration targets at their owners: worker auth flags (`worker.md`), the residual ACME knobs (`tls-certificates.md`) — see the § CLI Flags notes. (The registration-mode flags are **closed** — the posture is app-set nest state; `public-mode.md` § Registration Modes.)

## Goal

A single shared substrate that every nest deployment reuses: one SQLite database, one content-addressed blob store, one Ed25519 auth model, one per-actor WS-RPC channel, one content-classification pipeline, and one background-task set. Deployment-mode behavior (public-internet federation, private-nest pairing, worker sidecar replication) layers above this floor without forking it. The storage-mode axis (plaintext vs. encrypted at rest) that used to be its sibling is **retired** — every nest stores content sealed at rest, uniformly (§ Storage modes; owner: `storage-modes.md`). NAT/deployment mode persists as the `nest_nat_mode` singleton (§ NAT mode below); the legacy `nest_mode` row from the retired axis is a read-only artifact (`storage-modes.md` § Legacy artifacts), not a live persisted choice.

---

## Storage modes

**Retired.** There is no storage mode — every nest stores content sealed at rest, uniformly, from first boot; no claim-time trust choice, no per-mode `Storage`-trait dispatch, no runtime or offline mode switch. Owner: the sealed posture, the transition contract (whose wire shims — `fauna.setup.storage_mode`, `SetupStatusReply.mode`, `FetchConfigReply.storage_mode` — left the wire 2026-09-24), and the legacy artifacts (the `nest_mode` row, the `storage-mode` marker) live in [`storage-modes.md`](storage-modes.md); the at-rest *property* (what's sealed, the readable classes, the capability-grant primitive) is [`../encryption-at-rest.md`](../encryption-at-rest.md). This section previously restated the substrate residue (the two wire kinds + the trait dispatch) — both are gone from the server; see the owner doc's § Implementation status today for exactly what's built vs. what's left (the client-side onboarding steps).

---

## Tech Stack

| Component | Technology |
|-----------|-----------|
| Binary | Rust (`bins/fauna-nest/`) |
| Async runtime | Tokio |
| HTTP framework | Axum |
| Database | SQLite via `rusqlite` (single-file `nest.db`) |
| Blob storage | `DiskBlobStore` on local disk, at the artifact-set blob directory — no human chooses the backend (§ Blob Store → Backend) |
| Serialization | Canonical IPLD dag-cbor (`libs/fauna-cbor/`) for every wire and on-disk byte (WS-RPC envelopes, signed content, CARv2 segment blocks); the externally-forced HTTP/JSON residue (federation JSON-LD, OAuth, NodeInfo, etc.) lives in `docs/goal/architecture/api-layers.md` § JSON-bearing endpoints. See `docs/goal/architecture/serialization.md`. |

---

## Database (SQLite)

All state is stored in a single `nest.db` file in the data directory. Its schema is the **genesis** in `db/migrations.rs`: one `CREATE … IF NOT EXISTS` block per concern, applied in order on every boot (`apply_genesis`), so a table or index added to a block reaches an existing database the same way it reaches a fresh one; `db/genesis_shape.txt` pins the whole shape (columns in order, keys, constraints, indexes) and every schema change updates it in the same commit. The migration history that had built the schema was collapsed into the genesis twice under the compat-remnant sweep's baseline reset ([`../version-compatibility.md`](../version-compatibility.md) § Dimension 2, the fourth ratified exception) — the 78-step history on 2026-09-24, and the one-shot steps of schemas 79–124 on 2026-10-04, the last schema change before the public repository — the schema-version numbering continuing at 125 with the reader floor raised to it (`../version-compatibility.md` § 2.2, the two genesis paragraphs, owns the number and what each older binary reads): no step, boot reconcile or backfill written for a database predating the genesis remains, and `CacheDb::open` refuses such a database before anything reads it (`check_genesis` — the genesis stamps the SQLite header's `application_id`, and a file carrying any other mark is not one the genesis wrote; only an empty file, a first boot, is admitted unmarked). A standing boot pass stays in `run_migrations` only while a current writer still produces its input.

**Upgrade safety — additive columns are auto-reconciled.** `run_migrations` runs a column-reconcile pass (`reconcile_added_columns`) immediately after the genesis applies — ahead of the standing boot passes, with the function finally stamping the schema version (`record_schema_meta`) last: it diffs each table against a throwaway genesis and `ALTER ... ADD COLUMN`s any column the current `CREATE TABLE` blocks declare but a long-lived `/data` lacks. The `CREATE TABLE` blocks stay the single source of truth, so **adding a nullable / constant-default column to a `CREATE TABLE IF NOT EXISTS` block correctly lands on both fresh and existing databases without a hand-written `ALTER` catch-up** — closing the outage class, where a forgotten `ALTER` (`mail_domains.dkim_selector_activated_at`) crash-looped the mail bridge against the live DB (an off-box-only brick the § Client-state recoverability invariant outlaws). Scope is exactly SQLite's additive `ADD COLUMN` class; **renames, backfills, and `NOT NULL`-without-default columns still need an explicit, guarded step in `run_migrations`** (the reconciler hard-errors on the last rather than silently skipping it), staged expand→migrate→contract, and an **index over a column added to an existing table** is created after the reconcile, never in the block — in the block it runs against the old table first and every boot of a long-lived `/data` crashes with "no such column" (the 2026-08-03 example.com outage). A CI guard (`reconciler_restores_dropped_additive_columns_for_every_table`) asserts the reconcile property holds for every managed table, and a tier_4 image-upgrade gate (`test_mail_deploy_schema_upgrade.py`) proves it end-to-end: it drives the real Docker image to serving on a persistent volume, rewrites `nest.db` down to an older schema, then redeploys a fresh container on the same `/data` and asserts the mail bridge still serves all four ports — the upgrade path the fresh-boot tier_4 tests never exercise. The reconciler closes the *additive* trigger structurally; the residual non-additive class (renames / type changes) is made **visible** by the `mail_subsystem_ok` signal on `fauna.setup.status` (§ Storage modes), which turns a mail-config query break into a client-visible red rather than a silent off-box brick. The reconciler handles the *forward* direction (a newer binary opening an older DB). The *downgrade* direction — an **older** binary opening a DB a **newer** binary wrote with a breaking change — is handled by the `schema_meta` two-number scheme (`schema_version` + `min_reader_version`): `check_schema_compatibility` at `CacheDb::open` detects the incompatible verdict and the nest boots a degraded "needs-update" mode (`serve_incompatible`) that answers a typed `fauna.nest.outdated` to clients instead of crash-looping or failing lazily at first insert — the no-off-box-brick guarantee for the downgrade case (authority: [`../version-compatibility.md`](../version-compatibility.md) § 2.2). This downgrade degraded-boot + lossless-recovery path is itself image-gated by `test_mail_deploy_schema_upgrade.py::test_incompatible_schema_boots_degraded_then_recovers`: the real image boots a `/data` stamped with an incompatible `min_reader_version`, keeps `/api/v1/health` 200, serves `fauna.nest.outdated` over anonymous WS-RPC, and recovers with the claimed admin intact when the floor is lowered again. A guarded non-additive step is also covered by an **in-memory upgrade unit test** that models the pre-change shape, seeds a row, runs `run_migrations`, and asserts the heal, the row's survival (the no-user-data-loss invariant, `principles.md` § No user-data loss) and an idempotent re-run. None exists at the genesis: the table-rebuild class the tier_4 gate used to exercise (the `segment_records` PK widen) was history the genesis replaced, so the gate's non-additive phase returns with the first post-genesis non-additive step. The gate runs on the self-hosted CI runner via the `tier4-deploy-e2e` workflow (`.github/workflows/tier4-deploy-e2e.yml`, manual `workflow_dispatch`; the heavy nest image is built on the runner, not locally).

### Unreadable stored values (read posture — ruled 2026-08-12)

**The question.** A column is present but its value no longer parses: SQLite handed bytes back, the application-level decode failed (a corrupted page, a torn write, a decoder tightened across an upgrade). Nearly every such read folds the failure into some legitimate value — `None`, an empty list, a default — and moves on. Twice in two days that fold silently converted corruption into a *differently-consequential* legitimate state: an unreadable guardian/region **policy document** read as "no policy," lifting a child-safety and a legal restriction (fixed 2026-08-11 — `dynamic-features.md` § Fail posture, the undecodable-document clause), and an unreadable **declared region** read as "never declared," hiding a still-binding document's identity from the reads (fixed 2026-08-12 — same §, the unreadable-declaration clause). Two structurally different sightings in unrelated tiers within days is a class, not two site bugs.

**The ruling: a decision rule, not machinery.** No shared three-state wrapper type, and no boot-time integrity sweep — corruption is rare, the right fold differs per value, the decoders live at the read sites, and every affected value is re-authorable from an app (§ Client-state recoverability; writers are whole-value replaces, so re-authoring clears a corrupt row in-app). What is general is the rule every parse-and-fold site applies, found or newly written:

1. **Fold by what the value is** — the consequence direction, never convenience:
   - an **authored restriction** (a policy document): unreadable **denies at its own scope**, never "this tier said nothing" (owner: `dynamic-features.md` § Fail posture, ratified 2026-08-11);
   - an **authored choice** whose absence legitimately means "use defaults": unreadable is **not** absence when the consequences differ — fold to the conservative arm, the one that cannot ship data or widen exposure against the choice the user may have made (exemplar: a corrupt `nostr_accounts.relay_list` resolves to *publish nowhere, loudly* — never to the default relays the user may have removed);
   - a **declaration/situs** whose absence is a ratified open state: unreadable reads as absent **for enforcement** (an unreadable situs restricts nobody — `dynamic-features.md` § Fail posture), subject to rule 2;
   - **authorization/consent state** (capability lists, approval lists): unreadable **denies, never grants** — already the codebase's uniform shape (`nest_pairings.capabilities`, `pending_actions.approvals`);
   - a **display projection** whose operative value is intact elsewhere: fold freely with a `warn!` and the reasoning at the site (exemplar: `db/atproto_pds.rs`'s `sets` grouping fold, whose comment carries the ruling).
2. **The read moves with the fold.** No surface may report the folded value as the truth when the distinction is consequence-bearing: a transparency or admin read that would answer "absent" while the unreadable value's consequences still bind must either report "unreadable" distinctly or recover the truth from independent intact state (the region declaration's reads do both). This is the guardian-tier lesson generalized — fixing the gate and leaving the read turns a consistent-but-wrong pair into a read promising what the gate refuses.
3. **State the direction at the site.** Every parse-and-fold on a stored value carries a one-line comment naming its direction ("corruption folds to X because Y"). A fold without a stated direction is unreviewed, not neutral.

*Survey record (2026-08-12; recipe: `rg 'unwrap_or_default|\.ok\(\)'` over stored-value parse sites plus the `warn!`-and-skip idiom).* Conforming as found: pairing capabilities and pending-action approvals (deny-direction), `db/atproto_pds.rs` `sets` (stated display fold), wireguard `lan_endpoints` (degrades connectivity only). Repaired: the policy-plane sites (2026-08-11), the region-declaration reads and write seam (2026-08-12), the relay-list resolver (2026-08-12). **Verified 2026-08-12 (tracked internally):** `folders.{include,exclude}_paths` (`folder_handlers.rs::parse_paths`) — every production reader is sealed-first (`label_custody::render_include_paths`/`render_exclude_paths`) and already collapses undecodable-or-absent identically, and the one production consumer that acted on it then (`fauna-sync`'s daemon, removed 2026-10-02) treated that as "keep the local cached filter", never "reset to unfiltered" — conforming, direction stated at the site; the one residual gap (a device's first-ever bind to a set that is simultaneously corrupted and not yet seal-backfilled defaults to unfiltered, same as a genuinely never-restricted set) is narrow, self-healing via re-save, and within this ruling's own "corruption is rare, no boot sweep" tolerance. `mail_import` cursors (`db/mail_import.rs`, two sites) — conforming: idempotent via the cursor-independent `actor_message_dedup` table, so a corrupt blob costs a redundant re-scan, never a duplicate import (outside the user's own opt-in "import duplicates anyway" mode). **A later site applies the rule at authorship:** `db/domain_expiry.rs::get_domain_expiry`'s undecodable RDAP `statuses` blob folds to an empty list rather than failing the whole read — the row is a whole-record replace on every watch attempt, so the gap is one tick and self-healing, and losing only the status arm keeps the still-readable expiry-date arm serving the alarm the read exists for. **The identifier-column arm of the same class was generalized nest-wide 2026-09-12:** the recurring "BLOB column → fixed-size array" idiom (an id/key column, never a policy value — no legitimate "absent" reading applies) was folding a length mismatch to an all-zero array, a zero-padded prefix, or an outright panic at 26 sites across `db/`; `blob_col_to_array`/`blob_to_array` (`db/mod.rs`) now close all three into a single explicit-error shape, the same posture rule 1's declaration-head exemplar above already used — conforming, no fold at all where none is legitimate.

### Tables

The inventory below is **representative, not exhaustive** — the `CREATE TABLE` blocks in `db/migrations.rs` are the source of truth, and newer feature-owned tables (`nest_nat_mode`, `serving_port`, the spam-model tables beyond `spam_models`, …) are specified by their owner docs.

**Users and Authentication**

| Table | Purpose |
|-------|---------|
| `users` | Registered actors with handle, tier, suspension/eviction status |
| `tiers` | Tier definitions and per-tier quotas (storage, devices, feeds, blob size) |
| `admin_actor_ids` | Actors with admin privileges (role, added_by) |
| `invite_codes` | Registration invite codes with tier and uses remaining |
| `handle_cooldowns` | Cooldown periods after handle release (prevents squatting) |
| `actor_last_ip` | Per-actor last-seen IP (new-location sign-in detection) |
| `eviction_tokens` | Time-limited tokens for eviction self-service |
| `inbox_modes` | Per-actor inbox mode (`allow_knock`, etc.) |
| `audit_log` | Tamper-evident admin audit trail (hash-chained) |

**Content**

| Table | Purpose |
|-------|---------|
| `content` | All stored content (posts, inbox messages, profiles, calendar events) — one schema plane per id (§ One id, one plane) |
| `content_links` | Typed relationship edges between content, actors, and external entities |
| `content_meta` | Denormalized metadata: `score`, `has_media`, `is_reply`, `quarantined`, `suppressed`, and the engagement counters `like_count` / `reply_count` / `repost_count` / `quote_count` (the live counts [`../../ui/feed.md`](../../ui/feed.md) § Interaction bar renders; the never-written `view_count` left at schema 100) — maintained via the single `increment_engagement_count` / `decrement_engagement_count` core (`core-client-kind-catalog.md` § Labels & Engagement is the increment-semantics authority). |
| `content_fts` | FTS5 full-text search index over the `content` table (virtual table). Server-side only; to be subsumed by the Tantivy-based per-user content index once it covers these kinds — see `docs/goal/behavior/content-index.md` (its Plan 9). |
| `content_fts_map` | Maps content IDs to FTS rowids |

**Messaging and Social**

| Table | Purpose |
|-------|---------|
| `actor_channels` | Channel membership records per actor |
| `key_packages` | MLS key packages for actors (FIFO, expire after 30 days) |
| `knocks` | Pending contact requests |
| `contacts` | Contact relationships with status (pending, accepted, confirmed, blocked) |
| `engagement_events` | Engagement events (likes, reposts, replies, quotes, views). `INSERT OR IGNORE` on a stable event id makes the interaction counters idempotent: analytics signals key by a 10-second bucket (`compute_event_id`), the like/unlike toggle by a time-independent per-(actor, target) key (`compute_toggle_event_id`), and a reply/repost/quote by its referencing post id (`compute_reference_event_id`). |
| `push_subscriptions` | Push subscriptions per actor/device, web-push and APNs alike (actor_id, device_id, transport, endpoint, key_p256dh, key_auth, created_at); UNIQUE(actor_id, device_id) — transport/registration/dispatch owned by [`../apps/common.md`](../apps/common.md) § Push Notifications |

**Feeds**

| Table | Purpose |
|-------|---------|
| `feeds` | User-defined feed configurations with filter rules and scope |
| `feed_contributors` | Remote feed contributors with poll priority and discovery metadata |

**Sync and Folders**

| Table | Purpose |
|-------|---------|
| `sync_devices` | Registered sync devices per actor (with capabilities) |
| `sync_changes` | Per-folder change log for sync catch-up |
| `sync_conflicts` | Conflict records reported by sync seats (the engine) |
| `folders` | Named folder definitions (mode, retention, schedule, selective paths) |
| `folder_members` | Devices belonging to a folder (source/sync/mirror/backup) |
| `destination_cursors` | Sync cursor position per destination |
| `file_versions` | Per-path version history with manifest hashes |

**Snapshots and Backups**

| Table | Purpose |
|-------|---------|
| `snapshots` | Folder snapshots (with soft-delete lifecycle) |
| `snapshot_files` | Files captured in a snapshot (path, hash, mode, type) |
| `backup_snapshots` | Database backup records (blob hash, size, format) |
| `operation_locks` | Exclusive locks for GC and snapshot operations |
| `upload_leases` | Per-folder upload leases with heartbeat |

**Blob Storage**

| Table | Purpose |
|-------|---------|
| `blob_metadata` | Metadata for stored blobs (size, content type, ref_count, storage flags, C2PA detection) |

**Email**

| Table | Purpose |
|-------|---------|
| `email_queue` | Outbound email delivery queue with retry tracking |
| `email_filters` | Per-user email filter rules with priority and actions |
| `account_aliases` | Per-account address-pattern surface (exact / +suffix / wildcard / disposable / forwarder / list / catch-all-adjacent) — superseded the dropped `email_aliases` table at the I6 mail-bridge cutover; canonical shape in `docs/goal/behavior/mail-aliases.md` § Storage |
| `mail_domains` | Multi-domain mail hosting (landed rename; the legacy `email_domains` table is retained alongside) — canonical shape in `docs/goal/behavior/mail-multidomain.md` § The `mail_domains` model |
| `email_domain_users` | Per-domain email user registrations (local_part mapping) |
| `expunged_uids` | Dropped at schema 99 with its twin `imap_mailboxes` (the pre-Phase-C IMAP prototype; `bridge_imap_expunged` below is the live table) |
| `auto_reply_log` | Rate limiting for auto-reply messages |

**Mail / CalDAV Bridge**

The placement-layer tables the mail / calendar bridge surface (`fauna.bridges.*` WS-RPC, served by the MDA bridge — `storage-modes.md` § Boot story) reads and writes, used uniformly on every nest since the Phase 3 segment-store rollout (`../encryption-at-rest.md` § Per-content-kind conformance, S6 — the storage-mode fork this subsection used to describe is retired). Each row says which kind it serves.

| Table | Purpose |
|-------|---------|
| `segment_records` | Per-record floor mirror for the message-segment-store, shared by all five kinds (mail, conv, calendar, card, post) via NULLable per-kind columns, one reserved folder per kind. Mail-kind rows mirror the inbound-mail floor (`record_cid` = 36-byte `fauna_cbor::Cid` of the dag-cbor block, `received_at`, `sender_dom`, `spam_disp`, `is_own_submission`, `tombstoned`); random-access reads go through the CARv2 `MultihashIndexSorted` index in the segment `.dat` (no byte-offset / byte-length columns since Layer 3); record-body bytes live in the `__mail/<actor_id_hex>/seg-NNNNNNNN.dat` files on local disk (sealed payload bytes as CARv2 blocks) and per-record opaque kind-specific floor metadata in the sibling `.meta` dag-cbor sidecar. Authority: `docs/goal/architecture/message-segment-store.md` § `segment_records` SQLite mirror (its § Layout owns the per-kind status). Replaces the former `bridge_inbound_mail` table. |
| `bridge_imap_messages` | Per-(actor, mailbox, uid) IMAP placement: pointer to the message body via `segment_records` (keyed by `kind='mail'`, `record_cid=message_id_cid`) + IMAP flags, modseq, INTERNALDATE. COPY duplicates create multiple placements over one shared record. |
| `bridge_imap_mailbox_state` | Per-(actor, mailbox) IMAP state: `uid_validity`, `uid_next`, `highestmodseq`. Standard mailboxes (INBOX/Sent/Drafts/Trash/Junk) seeded lazily on first use. |
| `bridge_imap_expunged` | IMAP deletion tombstones: for CONDSTORE/QRESYNC VANISHED responses. |
| `bridge_caldav_calendars` | Per-calendar collection state: sealed metadata + `ctag` / `highestmodseq` sync counters. |
| `bridge_caldav_events` | Per-event placement: opaque ciphertext + plaintext-floor metadata (`event_id`, `uid_hash`, `calendar_id`, `etag`/`modseq`, ciphertext size, INTERNALDATE). |
| `bridge_caldav_expunged` | CalDAV deletion tombstones: for RFC 6578 sync-collection responses. |

**Moderation**

| Table | Purpose |
|-------|---------|
| `content_labels` | Classifier output: category, confidence, mechanism, attestation per content item |
| `obligation_action_records` | Enforcement action audit trail (reject, quarantine, suppress) |
| `spam_models` | Per-user Bayesian spam model state |
| `spam_preferences` | Per-user spam thresholds and training preferences |
| `sender_behavior` | Behavioral event log (DM fanout, channel creation, response rates). Profile also queries `contacts`, `users`, `content` for social graph fields (social distance, mutual contacts, follow status, account age, post count). |

**Subscriptions**

| Table | Purpose |
|-------|---------|
| `subscription_tiers` | Author-defined subscription tier definitions |
| `subscribers` | Active subscriber records per author/tier |
| `subscribe_requests` | Pending subscription requests awaiting approval |
| `current_key_blobs` | Current key blob data per author/tier/epoch |

**Bridge and Subscriptions**

| Table | Purpose |
|-------|---------|
| `bridge_feed_subscriptions` | Per-user bridge feed subscriptions (Bluesky, Nostr, etc.) |

**Federation and Networking**

| Table | Purpose |
|-------|---------|
| `namespace_entries` | Encrypted namespace entries for handle resolution and paired nest sync |
| `nest_pairings` | Records of paired remote nests for cross-nest sync |
| `outbox` | Outbox queue for forwarding posts from private to public nest |
| `delivery_receipts` | Video CDN delivery tracking between nests |
| `video_cache_sources` | Cache-on-fetch records of which nests have served content |
| `nest_engagement_stats` | Per-nest engagement statistics |
| `nest_trust` | Per-nest trust scoring |
| `nest_keypair` | Nest Ed25519 signing keypair (singleton row) |
| `device_authorizations` | Device key authorizations per actor |
| `worker_replication` | Worker replication tracking (payload type/key) |

**Admin and Lifecycle**

| Table | Purpose |
|-------|---------|
| `pending_actions` | Delayed destructive operations with `execute_after` timestamps and quorum support |

**Web Hosting**

| Table | Purpose |
|-------|---------|
| `web_files` | Uploaded source files per actor |
| `web_rendered` | Post-processed rendered output per actor |
| `web_domains` | Custom domain registrations with verification status |

### One id, one plane (the shared `content` table — as built 2026-09-13)

Every plane that stores rows in `content` — posts, inbox messages, profiles, calendar events — shares one id space, and some planes key rows by digests that can coincide: a row keyed by a signed post's CID digest is exactly the id a signed wire of that post names (the retired group plane keyed its messages this way). So the post plane never touches another plane's row: the post read (`CacheDb::get_post`, behind `GET /api/v1/posts/{id}`, `fauna.posts.get` and `fauna.federation.post.get`) answers for `post/%` rows only; `content::insert_content` refuses to replace a row of another schema family (`schema_plane`); and neither the post store (`segments::post::store_post`) nor the discovery poller's index stub attaches a post projection to another plane's id. Before this, the anonymous post fetch served a stored group message by id, and an import-triggered trend fetch could overwrite one — emptying its payload and minting a public `content_meta` row for it. Witnessed by `db::content::tests::insert_content_refuses_to_replace_a_row_of_another_plane`, `segments::post::tests::store_post_refuses_an_id_holding_another_planes_row`, `db::feeds::tests::a_discovery_index_stub_never_attaches_to_another_planes_row`, and `conformance_federation_channel.rs::{post_get_refuses_another_planes_row_over_channel, trends_fetch_never_aliases_another_planes_row}`.

---

## Content Pipeline

Posts received via the API or a bridge daemon go through a fixed processing pipeline before storage.

```
1. Receive content — `fauna.posts.create` (WS-RPC; the HTTP twin
   POST /api/v1/posts is deleted), inbox delivery, or bridge ingestion
      │
      ▼
2. BLAKE3 hash → PostId
      │
      ▼
3. Store in content table
      │
      ▼
4. Index in content_fts (FTS5)
      │
      ▼
5. Denormalize to content_meta
```

**Retired at Phase 4 (2026-07-12): the nest-side ingest-time classify + obligation stage.** The nest classifies nothing at ingest, on any box — `process_on_ingest` / `index_on_ingest` (the old steps "Content classifiers" → "Labels stored in `content_labels`" → "Obligation rules evaluated at `EnforcementPoint::Ingest`" this diagram used to show between hashing and storage) are deleted with the storage-mode axis, for both a local `fauna.posts.create` and a federated relay-forward alike (`storage-modes.md` § Implementation status today; `../content-scoring.md` § The placement matrix, Federated inbound post row). A scorer now runs only at a capability position — the perimeter bridge pre-seal, the user's client post-decrypt, or a holder of a user-minted grant (the re-score drain) — and its output rides the scoring-metadata bus (`../content-scoring.md` § The scoring-metadata bus) rather than gating ingest synchronously.

See `data-flow.md` for the full sequence including bridge ingestion and feed query paths.

---

## Authentication

### Identity and Signing

All authentication is Ed25519 signature-based. The actor's public key IS their `ActorId` — there are no passwords, certificates, or third-party providers.

**Token mint.** Bearer tokens are minted over the pre-identity WS-RPC kind **`fauna.auth.handshake`** (shared core `auth_core::direct_auth_core`; the `POST /api/v1/auth/token` HTTP twin was deleted at the rip-out endgame).

**Token request signature** (`fauna_protocol::auth::handshake_signed_message`):

```
message = actor_id_bytes || timestamp_be_bytes [|| client_nonce]
signature = Ed25519_sign(secret_key, message)
```

The optional `client_nonce` narrows replay; the timestamp must be within ±30 seconds of server time (`MAX_TIMESTAMP_DRIFT_MS`).

### Bearer Tokens

- Format: `{actor_hex}.{random_64_hex}` (opaque to clients — do not parse)
- TTL: 1 hour
- Stored in-memory in `TokenStore` with per-token metadata: IP address, `created_at`, `last_used_at`
- A deployment-seed rotation clears the whole store — every bearer is a credential minted under the predecessor's authority (owner: `box-recovery.md` § Client acceptance → *Live-session convergence*)

### Session Management

| Operation | Description |
|-----------|-------------|
| List sessions | Returns all active tokens for the actor |
| Revoke individual | Invalidates a specific token |
| Revoke all | Invalidates every token for the actor except the one the caller names to keep — the session doing the revoking (`fauna.sessions.revoke_all`'s `keep_token_id`; a value matching none of the actor's sessions ends them all; owner [`../../behavior/devices.md`](../../behavior/devices.md) § Session Management) |
| Emergency lockout | Ed25519 signature (no bearer required) locks out all sessions for a hard-coded 24 hours (`devices.md` § Emergency lockout) |

### Rate Limiting

IP-based rate limiting applies to registration and auth endpoints. Exceeding limits returns HTTP 429. A client that keeps presenting a refused credential (a dead bearer on the WS upgrades, a dead renewal grant on `fauna.auth.device_handshake`) meets the failed-credential throttle, keyed on source IP × claimed identity and answered `429` + `Retry-After` on an upgrade — owned by [`../transport-connection.md`](../transport-connection.md) § Abuse posture → *The failed-credential throttle*.

---

## WS-RPC channel (Spec Y)

Per-actor WS-RPC channel — single WebSocket per actor connection — implemented across `bins/fauna-nest/src/{ws.rs,rpc_router.rs,routes.rs}`.

**Endpoint:** `GET /api/v1/ws/{actor_id}` with `Sec-WebSocket-Protocol: fauna.v1, bearer.<token>` (the legacy `?token=` query parameter was removed; see `transport.md`).

### State

`WsState` holds per-actor `RpcConnection` records — bounded `mpsc::channel(256)` outbound queues, an idempotency cache (LRU bounded 1000 entries, 5-min TTL; replies over 64 KiB are cached as a `too_large` marker, not replayable bytes — `../transport.md` § request lifecycle), and `pending_handlers` for cancel-target lookup. Multiple subscribers per actor are supported (multiple devices or browser tabs open simultaneously); each opens its own WS with its own `seq` space.

### Dispatch

`RpcRouter` is built once at app startup in `bins/fauna-nest/src/lib.rs` via per-area `register_<area>_handlers(&mut RpcRouterBuilder)` calls. Frames decoded by the dispatcher:

- **Request** → idempotency cache lookup; on miss, kind dispatch via `RpcRouter`; handler task spawned with `AbortHandle` registered for cancel.
- **Cancel** → looks up the registered `AbortHandle` and aborts the handler.
- **Reply / Push** are server-emitted only.

Application code on the nest side calls `state.ws.notify_push(actor, PushEvent::...)`; the WS layer encodes, allocates `seq`, and emits a canonical-CBOR `Push` frame.

### Push events

The typed `PushEvent` payloads are defined in `libs/fauna-protocol/src/push_events.rs` and routed by wire kind. **The enum is the source of truth**; the table below lists the substrate-level set. Further feature-owned variants — `SegmentsChanged`, `MailReceived`, `CalendarChanged`, `AddressBookChanged`, `SyncChanged`, `BridgeMailboxState`, `BridgeConfigChanged`, `BridgeOutboundReady`, `SpacesUpdated`, `BridgeSpamModelUpdated`, `BridgeSpamModelReset`, `LeaseChanged`, `BridgeAtprotoSessionsChanged`, `BridgeAtprotoProjectionReady`, `BridgeAtprotoAsKeyRotated`, `AtprotoConsentRequested`, `BridgeAtprotoConsentResolved`, `BridgeRescoreReady`, `BridgeSpamBaselinePublish`, `BridgeImportProgress`, `BridgeImportError`, `BridgeImportComplete` — have their triggers + wire kinds specified by their owner docs (message-segment-store, mail/bridges, `../transport.md` § Push events for `fauna.calendar.changed`, `../../behavior/carddav-server.md` for `fauna.addressbook.changed`, spaces, mail-spam, file-sync leases and the `fauna.sync.changed` same-nest remote-change nudge (`../behavior/file-sync.md` § Remote-change nudge), `atproto-pds-full.md` / `atproto-pds-bridge.md` for the atproto bridge pushes including the F4 consent + AS-key-rotation pushes, `content-scoring.md` for the re-score nudge, `mailbox-migration.md` for import progress).

| Wire kind | Variant | Trigger | Key fields |
|---|---|---|---|
| `fauna.conversations.channel.message` | `PushEvent::ChannelMessage` | New MLS ciphertext posted to a channel | `channel_id`, `data` |
| `fauna.conversations.welcome.received` | `PushEvent::Welcome` | MLS Welcome delivered to recipient's inbox | `welcome_bytes`, `channel_id?`, `nest_url?`, `channel_type?`, `group_id?` |
| `fauna.knock` | `PushEvent::Knock` | Incoming contact request | `sender_id`, `summary` |
| `fauna.inbox.item` | `PushEvent::InboxItem` | Inbox delivery (non-Welcome envelope payloads) | `content_id`, `kind_hint` |
| `fauna.account.update` | `PushEvent::AccountUpdated` | Tier change, eviction, or settings update | `changes`, `timestamp` |
| `fauna.notification` | `PushEvent::Notification` | New unified notification | `notification_id`, `notif_type`, `source`, `summary`, `body?` |
| `fauna.peer.wake` | `PushEvent::PeerWake` | P2P tunnel initiation | `requester_actor_id`, `requester_endpoint`, `nonce` |
| `fauna.protocol.resync_required` | `PushEvent::ResyncRequired` | Backpressure overflow on this connection | `dropped_count` |

---

## Blob Store

### Backend

The `BlobStoreBackend` trait abstracts blob storage. **The live backend is always `DiskBlobStore`**: `BackupService::new` builds it at the blob directory the deployment artifact sets (`--blob-dir` — internal wiring no person writes, bucket 1 of [`../../principles.md`](../../principles.md) § One configuration surface). No config key, flag, or nest-state value selects a backend, and no human chooses one.

- **`DiskBlobStore`**: content-addressed files at `blobs/{hash[0:2]}/{hash[2:4]}/{hash}` under the data directory (`blob_store.rs::blob_path` — two levels of hex-prefix sharding, then the full hash as filename).
- **`S3BlobStore`**: an S3-compatible implementation of the trait. **It is not a live backend anywhere**, and it is kept — reserved for the deferred S3 backup-destination pass ([`../../behavior/backup-destinations.md`](../../behavior/backup-destinations.md) § Second destination kind) — see the status paragraph below.

**Ruling — may an admin choose and move the blob-store backend? NO, for now (user decision 2026-09-26).** Fauna does not promise admin-chosen / bring-your-own object storage for a nest's *primary* blob store. It was the configuration-surface test applied to a built mechanism (put to the user 2026-09-19), and the answer keeps the backend in bucket 1 — artifact-set local disk, no app surface, no nest-state value — and makes the admin storage-migration kinds a dark surface with no human caller. Off-box durability is served by the ratified-but-deferred S3 **backup destination** kind ([`../../behavior/backup-destinations.md`](../../behavior/backup-destinations.md) § Second destination kind), the one place object storage stays in scope; the nest-resident S3 write credential (that doc's obligation (a)) is now that pass's question alone. The rule, then the branch not taken:

- **The rule — artifact-set.** The backend stays `DiskBlobStore` at the artifact-set blob directory, and the four admin storage-migration kinds `fauna.storage.migrate.{start,status,reset,delete_source}` are **retired**: they leave the wire under [`../version-compatibility.md`](../version-compatibility.md) § Dimension 2's never-had-a-caller exception — evidenced all-history, both prongs: no app or client crate ever called them or their deleted HTTP twins in any commit on any branch, and nothing peer-side or bridge-side emits them — so no (client × nest) pair can observe the removal; `migration_progress` stopped being written with the retirement and left the schema at 99 under the compat-remnant sweep's dead-schema ruling ([`../compat-remnant-sweep.md`](../compat-remnant-sweep.md) § The nest-side retirements of 2026-09-27); `S3BlobStore` is kept for the S3 backup-destination pass. A later "yes" starts from the history bullet below, not from zero — the copy engine stays in git history.
- **History — "yes, an admin choice" (the branch NOT taken, 2026-09-26; kept so a re-ask starts from the design).** Which backend the nest's blobs live on, and its credential, would be bucket 2: UI on the admin nest page in all 7 apps, persisted in nest state. The credential is **nest-resident plaintext-equivalent** (the nest must hold it to write blobs; it is not a user key — blobs stay sealed, so the object store sees ciphertext). The design centres on **switching the live backend**, with copying as one step: `local` → `copying` → `copied` → `switched` (destination live, source kept as read fallback) → `source deleted`. The switch is the single atomic decision point — one nest-state row flip, reconciled at boot (§ Client-state recoverability); a client crash at any step leaves a state the admin page can resume or roll back. Deleting the source is legal **only after** the switch, behind an explicit confirm.

**Implementation status today (2026-09-27).** The retirement is landed: the four storage-migration kinds are gone from the wire (handlers, registry, allowlist, offline-class rows, wire types, the copy engine `backup/migrate.rs` — git history keeps it), with both evidence prongs cited in the retiring commit. `migration_progress` was dropped by schema 99's one-shot retire step (2026-09-30). `S3BlobStore` has **no production constructor** — it compiles and keeps its own module tests, reserved for the S3 backup-destination pass. The per-user `BackupService::blob_store_for_user`/`create_store_for_user` "storage" config path — dead under either branch above (no production code called it, and it could not work as written: it read the user's config, which is client-sealed and unreadable to the nest) — was deleted earlier, along with its store cache. Before the retirement the shipped migration was a copy with no cut-over, and `delete_source` refused with `fauna.storage.conflict` from 2026-09-19 (before then it deleted blobs the nest still served — a user-data-loss bug reachable by any admin, never reached in practice because no app called the kind). Wire tombstone: [`../core-client-kind-catalog.md`](../core-client-kind-catalog.md) § Storage Migration.

### Storage shape

`DiskBlobStore::put`/`get` store and return **raw bytes** — no transform in the general store (`blob_store.rs`). Storage is content-addressed by BLAKE3 hash, so identical blobs are stored once regardless of how many times they are uploaded. New writes are guarded by the **disk-guard floor** (writes refused past a free-space floor — owner: [`../../behavior/backup-restore.md`](../../behavior/backup-restore.md) § Blob-store disk guard). The zstd(`0x01`-prefix) + AES-256-GCM codec formerly described here belongs to the **backup** blob path (`backup/mod.rs::encode_blob`, key optional), not the general store; the at-rest-posture authority is [`../encryption-at-rest.md`](../encryption-at-rest.md).

### Per-actor ownership (target state — ratified 2026-08-10; not built)

The pool stays **one** content-addressed dedup pool; per-actor blob
*scoping* (a pool per actor) is **rejected** — it would end cross-actor
dedup and force a blob migration, buying an isolation the seal posture
already provides. What the nest adds is an **ownership index**: per-(blob,
actor) ownership rows, maintained through the same per-actor table registry
the deletion-orphans work builds (the enumerable per-actor boundary —
consumer contract: [`../account-data-plane.md`](../account-data-plane.md)
§ Nest-side requirements; work tracked in the nest area queue, the
orphaned-per-actor-rows track and its evidence-pack rider). With it,
"every blob actor X owns" is enumerable for deletion, export, succession,
and grant-scoped serving, and blob GC generalizes from bare `ref_count` to
owner-count: a blob dies when its last owner's last reference dies. One
security rule rides the shared pool: **no cross-actor existence
disclosure** — an upload never short-circuits on "another actor already
stored these bytes"; an actor gains an ownership row only by presenting
the bytes (dedup remains a private storage optimization, never an oracle).

---

## Background Tasks

The table below is **representative, not exhaustive** — it covers substrate-level tasks plus any feature-owned task with cross-cutting or security-relevant reach (a pointer row to its owner, mirroring the ACME/backup/domain-expiry rows). Purely single-feature janitorial sweeps spawned in `main.rs`/`lib.rs` (rate-limiter bucket sweepers, retention sweepers for audit/greylist/alias/scan-result/invite-request tables, the trend-decay/membership-lapse/succession-pull/region-tier/feature-gate-usage/atproto-blob sweeps, the mail/CalDAV/CardDAV/WebDAV/port enable-flag reconcile ticks) are specified, where they need specifying at all, by their owning feature doc — `bins/fauna-nest/src/{lib.rs,main.rs}`'s spawn sites are the source of truth for the full set.

| Task | Interval | Purpose |
|------|----------|---------|
| Snapshot scheduler | ~60s | Auto-snapshot changed folders; auto-prune old snapshots |
| Pending action executor | ~60s | Execute delayed destructive operations whose `execute_after` has passed |
| GC scheduler | 6 hours (21600s) | Apply retention policy, garbage-collect unreferenced blobs, and purge soft-deleted snapshots past their `purge_after` window (Phase 3: `list_expired_soft_deleted()` + `delete_snapshots()`) |
| Knock/contact expiry | 1 hour | Expire stale knocks (90d) and accepted-but-unconfirmed contacts (30d) |
| Token store GC | 10 minutes | Clean up expired entries from three in-memory stores on one tick: bearer tokens (`TokenStore`), bulk-byte transfer tokens (`BulkByteTokenStore`), and pre-identity auth-challenge nonces (`ChallengeStore`, scheduled 2026-07-24 — its `gc()` existed from the start but was never called, leaking one entry per abandoned sign-in against an unthrottled pre-identity writer) |
| Eviction task | Continuous | Advance eviction lifecycle: warning → suspended → deleted |
| ACME cert lifecycle + TLS cert watcher | Owner: [`tls-certificates.md`](tls-certificates.md) | Obtain/renew/hot-reload the TLS cert (issuance conditions, retry budget, cold-boot bootstrap — all owned there) |
| Domain-expiry watch | Owner: [`domains-and-tls-bootstrap.md`](domains-and-tls-bootstrap.md) § Domain loss → Detection | Daily-class RDAP fetch of the deployment's primary domain registration (`domain_expiry.rs`); persists `(expiry, statuses, fetched_at, outcome)` served by `fauna.domain.expiry.get` and read by the critical-alerts feeder — cadence, thresholds, and the SSRF-guarded fetch path all owned there |
| Outbox worker | Continuous (paired nests) | Forward posts from private nest outbox to paired public nest |
| Namespace sync worker | Continuous (paired nests) | Sync namespace entries with paired public nest (+ the sealed-mail relay leg — `nest/private-mode.md`) |
| Outbound mail hand-off | — (no nest loop) | The nest only **enqueues** outbound mail; the MTA-role Go bridge drains it via `fetch_outbound_due` polling + the `BridgeOutboundReady` nudge push — owner: [`../../behavior/smtp-server.md`](../../behavior/smtp-server.md) § Outbound delivery |
| Nest-side segment backup (`NestBackupWorker`) | Owner: [`../../behavior/backup-restore.md`](../../behavior/backup-restore.md) § Background Tasks | Sweeps every owner with a granted `NestBackupKey` × registered destination, backing them up with no client or agent alive — mechanism, cadence, and key model all owned there |
| P2P relay sidecar (`fauna-iroh-relay`) | Continuous — its own s6 service, not a nest loop | Owner: [`../../behavior/p2p.md`](../../behavior/p2p.md) § The relay — the rendezvous + forwarding relay for members' devices, admitted per endpoint over the sidecar channel; address discovery ruled 2026-10-05, measured first. (The WG-era embedded STUN server on UDP 3478 this row once named was deleted 2026-08-23 with that stack — p2p.md § NAT hole punching.) |

---

## TLS / ACME (pointer)

Whether a nest runs ACME is **derived, never configured** — there is no `[acme].enabled` field (`config.rs` documents this explicitly; `acme::build_acme_config` derives the decision from domain/identity state). The whole certificate story — per-SNI selection + the self-signed floor, the acquisition tiers (nest HTTP-01; client-driven DNS-01 — no DNS-provider key ever touches the nest), issuance/renewal conditions, the Let's Encrypt rate-budget pacing + persisted retry state, hot-reload + bridge fan-out, and the certless cold-boot bootstrap — is owned by [`tls-certificates.md`](tls-certificates.md) (registry: `acme`, `cert-lifecycle`), including the ruling that the CA and the account contact are constants (no human-set ACME knob). Substrate facts that remain here: the HTTP-01 challenge is served on the nest's own HTTP listener (port 8080 in the Docker image, fronted at 80), and certs live under `{data-dir}/acme/` (`fullchain.pem` / `privkey.pem` + the persisted `acme-retry-state.json`).

---

## Pending Actions System

Destructive operations are not applied immediately. They are written to the `pending_actions` table with an `execute_after` timestamp and applied by the background executor.

| Action (`ActionType`) | Delay | Quorum (min. distinct approvals) |
|--------|-------|-------|
| Handle change | 6 hours | — |
| Snapshot delete | 48 hours | — |
| Snapshot bulk prune | 7 days | — |
| Account delete | 14 days | — |
| Admin: suspend user | 4 hours | — |
| Admin: delete user | 7 days | — |
| Admin: bulk-delete users | 7 days | 2 |
| Admin: add admin | 1 day | 1 |
| Admin: remove admin | 1 day | 2 |
| Admin: change role | 1 day | 1 |
| Admin: backup-purge override | 30 days | 2 |

(Source of truth: `ActionType::{delay_secs, requires_quorum}` in `pending_actions.rs`.)

**The quorum column is the *nominal* count; the requirement stored on a person-initiated row is that count capped at the admins who could actually approve, and floored at one approval whenever any other admin exists (ruled by the user 2026-09-24, in-thread).** Self-approval is refused, so on a one-admin nest the nominal "add admin = 1" was unreachable: the grant expired unexecuted after its day, and the co-admin instrument `behavior/admin.md` § Admin continuity and succession ratifies could never complete on the deployment it exists for. `pending_actions::schedule` therefore stores `max(min(nominal, peers), min(1, others))` (`pending_actions::effective_quorum`), where *others* is every admin but the creator and *peers* is *others* minus the target. One consequence per roster size: a **sole admin's** grant needs none and executes on its delay alone, guarded by the creator's notice and cancel like every other delayed action — nobody else exists to ask; on a **two-admin** nest no roster change is unilateral — an add needs the other admin's approval, and a removal (or a role change) needs one approval that only its target can give, so **the target's approval is consent** (the approve door refuses only the creator; the target's cancel remains their veto) and an unconsented removal expires; on **three** admins a removal needs one other admin, target or peer — a 2-of-3 majority or consent; from **four** the nominal two stands. A quorum-gated action thus never executes on its creator's word alone while another admin exists — the rule the user chose over the cap alone, which stored 0 on a two-admin removal and let one admin remove the other on the delay window and the target's attentiveness, and over the nominal count alone, under which a two-admin nest could never shrink again, not even by consent. The formula is per action, not per type: the dormant types (`admin.change_role`, `admin.bulk_delete_users`, `admin.backup_purge_override` — nothing schedules them today) follow it without carve-outs, so a sole admin's bulk delete would run on its delay alone, as their one-at-a-time user deletion already does. The nest's own scheduled prunes and every direct `CacheDb::create_pending_action` caller keep the nominal value. An action still short of its stored requirement at `execute_after` **expires** — and rings `Transition::Expired` to everyone it concerned (`behavior/notifications.md` § Security notices → *Pending actions*), never silently. Pinned: `pending_actions::security_notice_tests::{a_sole_admin_can_grant_a_co_admin_on_the_delay_alone, a_two_admin_removal_needs_the_targets_consent}` and the `effective_quorum_caps_at_the_peers_who_can_approve` table.

- Users can **cancel** any pending action during the delay window.
- The pending action executor runs every ~60 seconds and processes all rows with `execute_after <= now()`.

**The executor claims before it acts, so a cancel the nest accepts always holds (ruled 2026-10-08).** The tick reads its batch once, then works through it row by row. Each row is first claimed with one conditional write, `pending` → `executing`, which succeeds only while the row is still `pending`. Only a claimed row runs, and its quorum is judged from the row the claim returned, not from the batch read. A cancel (the user's or an admin's `fauna.pending_actions.cancel`) that lands before the claim therefore wins: the claim finds no `pending` row, and the action never runs. A cancel that lands after the claim is refused honestly ("not cancellable"), because the action is already running. The two terminal marks, `executed` and `expired`, move only an `executing` row, so neither ever overwrites a terminal status. A failed run returns its row to `pending`, so the next tick retries it as before. A row left `executing` by a crash is returned to `pending` at boot, before the executor starts, so it is never stranded (§ Client-state recoverability). `executing` is a nest-internal transient: the `list` replies carry it, and every app's pending-actions section, which lists only `status == "pending"`, hides it, which is right, since it can no longer be cancelled. The identity-succession disarm (`db/successions.rs`) cancels a retired identity's `executing` rows as well as its `pending` ones. It cannot recall a run already under way, but a run that then fails is never re-armed. The executor writes a `pending_action.executed_after_disarm` audit entry when a run completes on a row the disarm has already cancelled, so the record stays honest. Pinned by `pending_actions::claim_tests`.

---

## Client-state recoverability (absolute invariant)

**No state a client can put the nest into is unrecoverable by a client — ever, including a client crash at any point during the operation.** Every state transition a client can initiate (claim, factory reset, mail enable/disable, domain add, bridge approval, …) MUST satisfy: whatever partial or final state results — even if the driving client is SIGKILL'd / loses power / its host is rebooted mid-operation — a client can still drive the nest forward to a working system, with **no operator shell access, no SSH, no manual DB surgery**. There is no client-reachable "brick". This is a hard requirement, not a goal: a transition that can strand the nest in a state only fixable off-box is a **bug**, not a deferred feature. (It is the crash-safe form of the "works out-of-the-box" product invariant — `principles.md` § Client-recoverable nest state points here for the mechanics.)

The enabling techniques (and why the existing flows already satisfy it):

- **Crash-atomic transitions** — commit through a single atomic decision point plus a boot reconcile, never a multi-write sequence a crash can tear. *Factory reset* stages a filesystem marker and performs the destructive wipe at the **next boot, before the DB opens** (§ Factory reset): a crash either leaves the marker (next boot completes the wipe → fresh/unclaimed → re-claimable) or not (unchanged). *Storage mode* (historical — the axis is retired, § Storage modes) applied the same pattern: a write-once singleton committed to the `nest_mode` row before the filesystem marker, with a boot mismatch/corruption being a fatal-restart, never a silent half-state.
- **Unrecoverable half-states made unrepresentable** — a partially-completed claim that left an admin with **no handle** (mail/AUTH/IMAP/CalDAV login resolves nobody, the canonical alias is unmaterializable — an off-box-only fix) was exactly such a brick; the claim type-promotion made it impossible to even represent (`fauna-protocol::ClaimAdminRequest.handle` is a required `String` — a handle-less request fails to decode; see [`../../behavior/onboarding.md`](../../behavior/onboarding.md) § 3a). Prefer this — make the bad state fail to *exist* — over detect-and-repair.
- **The fresh/unclaimed state is the universal recovery floor** — an unclaimed nest is the out-of-box state any client can claim, and factory reset deliberately targets it, so "reset to fresh, re-onboard" is always available as the last resort (and is itself client-driven).

**Per-object remedies — the invariant's unit of account is the box, not each object (ruled 2026-08-31).** A client-reachable **object** state with no in-place repair — a row no honest power can move, an object nobody can rebind — does **not** breach this invariant, provided a client-driven replacement reaches equivalent working function. The floor above already blesses abandon-and-rebuild at whole-box scope as the universal last resort; a per-object fresh start is the same shape at strictly smaller blast radius. Four conditions, ALL required — an object state failing any one of them is still a bug under this section: **(1) contained** — the effects stay on the object: no unrelated flow blocks, boot is untouched, no resource (quota, port, lock) is held hostage or grows; **(2) replaceable client-side** — an ordinary in-app flow reaches equivalent working function, no shell, no DB surgery; **(3) nothing user-irrecoverable trapped** — abandoning the object destroys no data the user cannot recreate (`principles.md` § No user-data loss is not waived here); **(4) inert residue** — what remains is a stale row that decays with the object's abandonment, never a hazard that compounds. The worked example is the **pinned foreign-member misbinding on an unclaimed conversation channel** ([`../federation.md`](../federation.md) § Cross-nest shared folders + channel append, the TOFU bullet — the owner of the binding, its pin, and its remedies): a confirmed grant there deliberately has no mover except the bound nest itself, because lending anyone else one is exactly the rebind the first-use pin refuses — permanence against everyone but the incumbent is the security property, not a recoverability defect. The adversarial case's remedy is a fresh channel, and all four conditions hold: one channel is affected; conversations are client-created; MLS content is client-held and the other members' own bindings — history reads included — are untouched; the misbound nest's residual read is of future traffic, which the abandonment itself ends. The cooperative case never needs the fresh channel at all: the incumbent nest's own `channel.leave` deletes even a confirmed row (the delete is deliberately not pin-guarded; regression-pinned by `channel_leave_releases_a_confirmed_binding_and_reopens_the_grant`), re-opening the insert-if-absent arm for the re-invite.

**Verifying a new client-driven transition:** ask *"if the client dies the instant after each individual write, can a client still recover the box?"* If any intermediate answer is no, the transition is not crash-atomic — fix the write-ordering (single decision point + boot reconcile) or make the bad intermediate state unrepresentable, **before** shipping it. A live chaos event on 2026-06-03 (a sibling session's `pkill` SIGKILL'd the onboarding client mid-factory-reset) confirmed the floor holds in practice: the box landed cleanly fresh/unclaimed and was re-claimable from a client — no off-box fix needed.

### Implementation status today

**One audited transition is known to breach it, ruled and owed (2026-09-29):** the custody materialize verb (`fauna.backup.custody.materialize`) adopts a restored corpus before the one transaction that serves it, and an ordinary mail arrival landing in that window — reachable by a nest crash or by the dispatcher's deadline abort of a large restore — leaves every retry refusing the target as lived-in, the adopted corpus stranded on disk with no client gesture that completes it. The remedy is the in-flight marker: the gesture recorded durably before the first observable write, the id space and the mailbox tree fixed before the halves, and a retry that completes over whatever arrived since (owner: [`../../behavior/backup-destinations.md`](../../behavior/backup-destinations.md) § Third destination kind → *Re-seed* → *the in-flight marker*; build declared at [`../segment-backup-protocol.md`](../segment-backup-protocol.md) § Implementation status today). Move the verb into the upheld list below when its pins land.

The invariant is **upheld for the audited transitions**: factory reset (marker + restart-wipe, `factory_reset.rs`), the now-retired storage-mode commit (write-once `nest_mode` singleton + boot reconcile — historical, the singleton itself dropped 2026-09-27; § Storage modes), admin claim (handle-less admin unrepresentable), and **user suspension** (2026-07-09: suspension is the eviction machine's `suspended` state entered directly, so `suspended` has exactly one writer and the already-client-reachable `fauna.admin.users.cancel_eviction` is its only exit — the orphaned `suspended = 1, eviction_status = ''` half-state that no client could undo is now unrepresentable, and an admin, who would lose the dispatch rights needed to restore themselves, cannot be suspended or evicted at all; [`../../behavior/admin.md`](../../behavior/admin.md) § 2 Users → *Cutting a user off*), and **admin deletion** (2026-07-10: deleting a user drops its `users` row but not its `admin_actor_ids` row, and the claim gate keys on an admin row existing — so a deleted admin left the box reporting *claimed* with nobody able to authenticate as its admin, an off-box brick. Both scheduling doors (`fauna.admin.users.delete`, `fauna.account.delete`) now refuse an admin target, and `pending_actions::finalize_user_deletion` refuses one too, because a persisted pending action can outlive the upgrade that added the doors; scope + the sole-superadmin `factory_reset` exit at [`../../behavior/admin.md`](../../behavior/admin.md) § 2 Users → *Cutting a user off*), and **admin removal + role change** (2026-08-14: the last-superadmin refusal was a schedule-time check on a 24 h-delayed quorum action and the executor applied blind, so N individually-legal removals scheduled while the roster was full could all execute and empty it — a zero-superadmin box answers `fauna.admin.permission_denied` to everyone, factory reset included, and the claim gate reads claimed with its single-use code long consumed: an off-box brick, measured by probe. The floor now lives **in the roster writers themselves**: `CacheDb::remove_admin_actor` / `set_admin_role` refuse — under the same lock acquisition as the write — any mutation that would leave zero superadmins, the `"admin.remove"` / `"admin.change_role"` executor arms park a refused action pending-and-retryable exactly like `"admin.add"`'s registered-user re-check, and the scheduling door's synchronous 409 is target-aware; ruling + the declined reservation-count half at [`../../behavior/admin.md`](../../behavior/admin.md) § 2 Users → *Cutting a user off*). The claim's claimed-state is **DB-positive** (`setup.status.claimed`, the `already_claimed` gate, and the boot `reconcile_claim_code` all key on an *admin* row existing — `admin_count > 0` over `admin_actor_ids` — not on the single-use claim-code file's absence): a claim code that lingers un-deleted on an already-claimed box (a read-only cloud-init mount, an `EBUSY`/transient unlink failure) no longer wedges the box in a "claimable-but-claimed" off-box-only-fixable limbo, and a lingering still-readable code cannot let a different actor become a second admin (`claim_core.rs` § 3a gate). Keyed on an *admin* (not any user), so a half-completed claim — a `users` row written before `add_admin_actor` — still reads `claimed=false` with its code preserved, keeping the recovery retry open.

**The invariant is now exercised adversarially, not only by design review** (2026-07-12): the tier_3 crash-recovery harness (`tests/e2e-unified/tests/test_crash_recovery_journeys.py`, marker `crash_recovery`, `just e2e-crash-recovery-test`) SIGKILLs the driver's own app child mid-operation (`PlatformDriver.kill_uncleanly`, timed by the debug-level dispatch-receipt beacon in `dispatch_core.rs` — "ws-rpc dispatch received"), then asserts a relaunched client drives the box to the working end state through the UI. First journeys: client kill mid-**domain-add**, mid-**mail-enable**, mid-**claim**, mid-**factory-reset** (floor assertion), plus an unclean **nest** kill mid-domain-add (the boot-reconcile half, via `common.nest.stop_nest(graceful=False)`). Domain add and mail enable thereby move from "not yet audited" to adversarially audited at their dispatch seams. A sixth journey (2026-07-13) covers the **stale slot** left by a permanently-failed reset dispatch — the CR-2 arm below. Journeys **7–8 (2026-07-15)** add the inverse transitions — client kill mid-**domain-remove** and mid-**mail-disable**. **On linux: journeys 1–11 green** (journey 11, the within-grace honor arm, joined 2026-07-17 — `e2e_status_and_tips.md` § Crash-recovery has the linux-specific harness-timing note) — journey 6 (CR-2) **joined 2026-07-15** via the driver-agnostic `seed_pending_factory_reset` seam (`drivers/base.py`), which arranges the stale slot in each platform's REAL store format: the macOS/iOS keychain file's un-prefixed keys AND the linux File backend's `legacy/pending_factory_reset_*` keys (mapped verbatim by `CredentialStore::account_for`). The reconcile therefore reads back a GENUINE slot on linux — cleared across two consecutive relaunches — driving the identical shared-Rust `RegistryLaunchPersistence` legacy-fallback path (`index_absent()`) macOS already proves, so the skip is retired rather than replaced by a vacuous shim. **On macOS: journeys 1–11 green** (apple joined 2026-07-13, once it adopted the shared `LaunchMachine` and its e2e Keychain gained a durable backing — see the per-app note below; journeys 7–8 confirmed and journey 11 — the within-grace honor arm — joined 2026-08-01, full-suite run). Journey 11 needed two apple-specific fixes the harness had never previously exercised (its own SIGKILL used to race ahead of the client's own mint, always landing on the ordinary silent-challenge path instead): `seedWizard`'s `.pendingFactoryReset` case re-read the slot via the raw legacy Keychain instead of the same registry-scoped `LaunchPersistence` the machine itself branched on (a mismatch invisible on a migrated multi-account install, since `mirror_active_to_legacy` only mirrors from a *materialized* index — the common un-migrated single-account case never reaches one, matching the `.awaitingManualDns` case's already-correct pattern fixed it); and the e2e `applySessionPatch` test hook persisted `secret_hex`/`node_url`/`device_id` to Keychain but not `handle`, so an injected admin session never populated the cached handle `AdminNestVM.factoryReset()` falls back to once its own live account lookup fails. **On iOS: journeys 1–6 and 11 green** (2026-08-01, same session, same two fixes — apple's app and driver code is shared between the two targets); journeys 7–8 pass solo but join the pre-existing contention-flaky set (transport errors, not a stable signal) under a full-suite run at load — `e2e_status_and_tips.md` has the detail. **On web: ALL 11 journeys GREEN (2026-07-17, two consecutive full-suite runs)** — journey 3 (mid-claim), the last red, was root-caused and fixed the same day: the SPA's singleton `WsRpcClient` (`$lib/rpc.ts` `getClient`) was cached by **actor id alone** and captured `nodeUrl()` at construction, so a client built while `fauna_node_url` was absent (the same-origin production fallback — exactly the torn store a mid-claim kill leaves: the identity secret is persisted at import-key, the URL only at claim success) kept dialing the SPA origin forever; no later URL write could retarget it, every fail-closed `checkIsAdmin` bounced the admin shell to settings, and the auth-fail reconnect looped at a flat 1 s. Fix: the singleton is keyed on **(actor id, `nodeUrl()`)** and the superseded client is torn down via the new shared `WsRpcClient::close()` (`fauna-rpc-wasm`) — the web twin of the apple pattern "re-vend the cached machine when its `APIClient` changes" (e2e tips § machine-backed VMs). `WebBridgeDriver` gained a browser-page unclean-kill primitive (`kill_uncleanly` closes the Playwright *page* but keeps its `BrowserContext`, so `localStorage` — the durable client store — survives, the web analogue of a native SIGKILL leaving the data dir; the relaunch reopens a page in the same context) plus the web `seed_pending_factory_reset` seam (a throwaway same-origin page writes, before any SPA boot reads them, the three global legacy keys the wasm `LocalStorageSecretStore` maps — `fauna-client-accounts` `web_store.rs`). **Landing them required a real web-side CR fix and upgraded journey 6 (CR-2) from a *vacuous* to a *genuine* pass:** the `/app` layout guard (`apps/fauna-web/.../routes/+layout.svelte`) ran the launch machine only when credentials were *absent*, so a client that crashed mid-factory-reset — credentials still present — landed on the feed with the pending-factory-reset slot never consulted (stranded on a wiped box, or a stale slot silently lingering on a still-claimed one); it now also routes to the launch machine when that slot is present, so the boot-reconcile clears a stale slot exactly as native does. **Journeys 3 (mid-claim) and 4 (CR-1 factory-reset re-claim) were the long-red pair — both now fixed (2026-07-17; hunt history kept because its wrong turns are instructive) — and the "the web WS-RPC transport does not re-authenticate" diagnosis recorded here is RETIRED as WRONG (2026-07-16).** It was never verified, it steered sessions into a transport hunt, and attacking it with a type-checker refuted it in seconds. Two *independent* client-side defects were found in its place. **(1) A missing `await` — FIXED.** Both launch-resume rows in the web onboarding page (`routes/onboarding/+page.svelte`) called their **async** slot reader without awaiting it, so a `Promise` — always truthy — passed the `if (rec)` guard and every field read `undefined`. That broke the CR-1 re-claim **and** the deferred-DNS resume (which no journey covers, so nothing had ever exercised it). It was invisible to every gate because **nothing type-checked the SPA**: `just web` / `web-test` run `vite build`, whose esbuild transform strips types without checking them, and the merge gate never builds web at all. `just web-check` (svelte-check) + a CI step now close that hole; the SPA is at 0 errors, so the gate is debt-free. **(2) "The launch probe has no timeout" — RECORDED HERE, THEN REFUTED (2026-07-16). This is the SECOND wrong root cause at this spot; do not chase it, and do not add a third without a measurement.** The claim was that `probe_setup_status` awaits connect + request with no deadline, leaving `LaunchMachine::start()` unresolved forever. **On web that is false**, three ways independently: the wasm request *is* bounded (a 30 s `TimeoutFuture` backstop in `fauna-rpc-wasm`'s `dispatch_typed`) and wasm `connect()` is synchronous, so `start()` cannot hang; waiting **90 s — 3× that bound** still yielded `claim-code-input` count 0; and `Unclaimed` **and** `Unreachable` both route to the *same* `WizardAt{PendingFactoryReset}` claim page, so the probe's outcome cannot decide this row at all. Journey 4 also **passes unchanged** under *higher* load than a failing run. *(An unbounded connect IS real, but **native-only** — `fauna-anon-client`'s `ws.rs` `TcpStream::connect` — so it cannot explain a **web** failure; it stays worth closing on its own merits, since the three-way probe's safety argument does assume the probe RESOLVES.)* **Journey 4 ROOT-CAUSED AND FIXED (2026-07-17), by measurement — a browser-console/`pageerror` capture in the web e2e bridge (`GET /page/console`, appended to the launch-surface dump) made the invisible visible.** The bimodality was the race between the client's SIGKILL and the reset *reply*: when the reply won, the admin page navigated to onboarding and the shared `LaunchMachine`'s boot reconcile probed the box **in the window where it had staged the wipe but not yet self-exited — so it still answered `Claimed` — and the CR-2 stale-slot arm cleared the freshly-minted slot**, destroying the only copy of the claim code moments before the wipe executed: CR-1 data loss delivered by CR-2's own reconcile, on every app that routes launch through the machine. *The fix is the mint-freshness grace* (shared Rust, all apps inherit): `PendingFactoryResetRecord` records `minted_at_secs` (required since the 2026-09-24 compat-remnant sweep; a record without it does not parse), and the reconcile's `Claimed` arm HONORS a slot minted within `FACTORY_RESET_CLAIM_GRACE_SECS` (15 min) instead of clearing it — per the standing asymmetry doctrine (clearing wrongly is unrecoverable; honoring wrongly is a recoverable claim page with a visible "already claimed" exit that self-heals past the grace). Unit-proven (`pending_factory_reset_reconcile.rs`, incl. the fresh-slot-on-`Claimed` arm) and adversarially green: journey 4 passed 6/6 consecutive runs including under synthetic CPU load that previously forced the loss deterministically. Two earlier wrong theories from this hunt are retired: the failing dumps' slot-less store was **not** a localStorage IPC/renderer-crash loss (an IndexedDB durability mirror was built for that theory, then measured irrelevant and reverted), and the mode-A "silent eternal loader" class is now structurally loud — the web onboarding page wraps its whole launch sequence (try/catch + a 45 s watchdog for the never-settling class + defensive null-read rows), surfacing every death through the existing `error-message` element with a seeded fallback wizard. **The invalid inference to avoid repeating** stays: a stuck `Loading…` alone distinguishes nothing — capture the console.

**Journey 3's harness assumptions needed a further hardening (2026-07-23, test-only, no product change).** `test_kill_client_mid_claim_box_recovers[web]` went red again on a faster machine: a mid-claim SIGKILL routinely lands **between** the two writes import performs (the identity secret first, the nest binding only at claim success), leaving a TORN store — and a faster machine hits that timing deterministically rather than sporadically. Both recovery arms had assumed an un-torn store; the invariant already held, so the fix lives entirely in the harness. `claimed=false`: the relaunch legitimately resumes onboarding at either `identity_choice` (a wiped native store) or `handle_entry` (web's preserved store) — `OnboardingActions.resume_or_import_identity` (`tests/e2e-unified/actions/onboarding.py`) now races both surfaces instead of assuming the former. `claimed=true`: the web account registry's boot mirror (`accounts.ts` `mirrorActiveToLegacy`) re-derives the legacy `fauna_node_url` from the crash-torn UNBOUND registry account on every load, so a plain session injection kept getting overwritten back to unbound; `inject_admin_session(..., torn_store_resilient=True)` (`tests/e2e-unified/helpers/crash_recovery.py`) now clears the torn registry first so the inject's own rebuild lands WITH the nest binding — the box's claimed+admin state is asserted server-side, independent of the client store, so this stays a faithful proof. Green again on `--client web`.

**On tui: ALL 11 journeys GREEN (2026-07-30)** — the kill primitive landed first (`TuiDriver.kill_uncleanly` group-SIGKILLs the pty session leader; all 11 had previously *skipped* on a `supports_unclean_kill()` check that tested a Popen tui never had), and the last 2 reds then surfaced **a real recoverability defect plus a harness one, both of which had been masked by an agent timeout**. *The defect:* tui sourced the pre-reset handle from the authoritative `fauna.account.get` **alone**, with no fallback to the registry's cached handle — so against an unreachable nest (exactly the permanently-failed-dispatch arm journeys 6 and 11 drive) the CR-1 slot was persisted with an **empty handle**, the launch machine honored the fresh slot, pre-filled the claim page, and the re-claim was then refused with *"a handle is required to claim the nest"* — a trapped client with no in-app exit, i.e. this section's invariant broken on tui. linux has always had the cache fallback (`apps/fauna-linux/src/client.rs::factory_reset`) and it is the same fix apple needed (§ *How apple got there*, item 1); tui had adopted only the authoritative-first half, so this is priority-#4 drift rather than a new design question. **Note the reinforcing gap this exposes:** `mint_and_persist_pending_factory_reset` verifies only that the **claim code** round-tripped (`libs/fauna-launch-machine/src/persistence.rs`) — neither it nor `load_pending_factory_reset` validates the handle, so an empty-handle slot persists and reloads silently. The handle-required rule is enforced only at the claim, one whole app-restart later, which is what let a client mint its own trap. *The harness half:* tui's e2e agent **awaited** the confirm click's op, so an op that legitimately blocks on an unreachable nest (two RPCs, each burning the 30 s client-side spec default — the per-kind registry is unwired in production, `fauna-protocol::kind` § the `register_*_kinds` caveat) returned a 504 instead of the app's own error. Fixed by `PageOp::outlives_click` (`apps/fauna-tui/src/app.rs`), which makes the agent *spawn* that op — matching linux, whose entry point is a **sync** fire-and-forget by construction and which is precisely why these journeys were always green there. No product behaviour changed for a human: tui's keyboard path already spawned, so the UI never froze and the failure always reached the page's own `error-message`.

**CR-1, the factory-reset reply-loss window — FOUND AND FIXED 2026-07-12/13** (found by the harness's factory-reset journey, closed by the fix below). *The gap was:* the post-reset claim code existed only in the synchronous `FactoryResetReply`, and no client pinned or persisted `FactoryResetRequest.new_claim_code` before dispatch (linux dispatched `factory_reset(None)`; all six apps shared the shape) — so a client SIGKILL'd between dispatch and reply-render lost the code with **no client able to learn it**. The box landed at the recovery floor (fresh/unclaimed, healthy — harness-asserted), but the re-claim was impossible with the code gone: the only client-driven exit was the box-recovery re-provision path ([`box-recovery.md`](box-recovery.md)) on a cloud box, and an installer re-run on a self-hosted one — a disproportionately heavy recovery for a lost reply, and arguably off-box for self-hosted.

*The fix (a client-side single atomic decision point).* The client now **mints the code itself, durably persists `(nest_url, handle, claim_code)` BEFORE dispatching, and pins it** via the already-existing `new_claim_code` field, which the nest honors verbatim (§ Factory reset step 1). The reply is therefore no longer load-bearing: it can die with the client at no cost. On relaunch the launch machine finds the slot and routes to the claim page with the code **pre-filled** — resuming, not re-deriving. The ordering that matters is enforced by construction rather than by convention: `mint_and_persist_pending_factory_reset` (`libs/fauna-launch-machine/src/persistence.rs`) returns the code *only after* the store write, so **a caller cannot hold a code it has not already persisted** — the crash-unsafe ordering is unrepresentable, the same "make the bad state fail to exist" technique as the handle-less admin above. The slot is a new row on the one long-term-store contract (`LaunchPersistence`, alongside the pending-invite and awaiting-manual-dns rows — it is structurally the same object: a durable pre-committed record carrying a claim code that the launch flow resumes from), so it reaches every app through the existing seam (UniFFI native, wasm web) rather than six bespoke stores; the new `LaunchWizardEntry::PendingFactoryReset` row is evaluated **before all others**, since the box was just wiped and a silent challenge against the saved `nest_url` would only fall through to `launch_retry`. It is cleared at the claim terminal, with the row swept per-actor on sign-out.

*Proof.* The harness journey now asserts the full round trip, not just the floor: the client is SIGKILL'd the instant the nest receives `fauna.admin.factory_reset`, and the **relaunched client re-claims the box through its UI** with the persisted code — checked against the code the wiped box actually booted with, so a client cannot pass by echoing a value it never lost (`test_kill_client_mid_factory_reset_floor_holds`, green on linux). Per-app status: linux adversarially proven; **macOS adversarially proven (2026-07-13)**; **web adversarially proven (2026-07-17)** — the CR-1 re-claim journey is GREEN on web (6/6 consecutive runs incl. under adversarial CPU load) after the mint-freshness grace closed the reconcile race described above (the box still answers `Claimed` between the reset dispatch and its self-exit, and the machine's stale-slot arm used to clear the fresh slot in exactly that window); android compile-verified **and its slot persistence unit-proven (2026-07-17)** — the grace field `minted_at_secs` round-trips verbatim through android's per-field `PendingFactoryResetSlot` (the one client not yet on the shared `RegistryLaunchPersistence` JSON seam), Robolectric-executed incl. the legacy-row-loads-with-null-mint and corrupt-timestamp-degrades-to-old cases (`SecureStoragePendingFactoryResetTest`); windows pending a build on its own platform (tracked internally). **iOS is adversarially proven too (2026-07-15)** — `drivers/ios.py` gained `kill_uncleanly()` (SIGKILLs the real host PID `simctl launch` reports for its own launch, never a name-based kill) and `preserve_state_across_relaunch()` (mirrors macOS's credential-dir pin), and `test_kill_client_mid_factory_reset_floor_holds[ios]` is green and reliable across repeated runs including the most aggressive kill timing — the fix is shared FaunaKit, so this is the same code macOS proves, now adversarially confirmed on both apple targets.

*How apple got there, and the two bugs the proof surfaced.* The crash-kill proof used to be unobtainable on apple: its e2e `KeychainStore` was a **process-static in-memory dict**, so a real `kill_uncleanly()` + relaunch always came back with an empty store regardless of whether the code was correct — `preserve_state_across_relaunch()` reported `False` and both factory-reset journeys skipped (a correct skip, not a false green). Porting that store to a temp-file backing keyed by `FAUNA_E2E_CREDENTIAL_DIR` (the shape linux already used) made them runnable — and they immediately caught two real defects that the skip had been hiding:

1. **apple sourced the factory-reset handle from a *display cache*.** `AdminNestVM.factoryReset()` read the resume handle from `KeychainStore.cachedHandle` alone — a best-effort cache whose write every caller swallows on the stated grounds that a failed write "costs at most one stale greeting line". It does not: the mint's read-back guard rejects a record with an empty handle, so the client **refuses to dispatch**, and an admin whose cache write had once failed could then never factory-reset their box at all. Fixed by asking the still-live session for the authoritative handle first (`fauna.account.get`) and keeping the cache as the fallback — which is what linux already did (priority #4).
2. **the e2e between-tests reset was incomplete.** It deleted three hand-listed keys and left the rest — including the pending-factory-reset slot. Because the apple e2e app process is session-scoped and reused across a module's tests, a slot one test wrote leaked into the next, which then launched onto a pre-filled claim page for a nest that no longer existed. Now driven off `Key.allCases`, so a slot added later cannot silently become the next such bug.

Both were invisible while the journeys skipped, which is the point: a skip is not a pass, and a store the harness cannot preserve makes the *whole* CR-1/CR-2 assertion vacuous.

*A store write that silently fails is treated as a failure to reset.* `SecretStore::set` is infallible on every platform (a locked libsecret collection, a Keychain denial, a `QuotaExceededError` on web are all swallowed), so `mint_and_persist_pending_factory_reset` **reads the row back** and yields no code if it did not land; the client then **refuses to dispatch** the reset and says so (`admin.settings_page.factory_reset_persist_failed`). Not starting a reset is always recoverable; wiping a box against a code nobody holds is not — that would be CR-1 again, and worse, because the client would believe it was safe.

**CR-2, the stale-slot launch hijack — FIXED 2026-07-13.** *The gap was:* the slot is deliberately *not* cleared when the reset **dispatch** fails, because an error cannot distinguish "the nest never reset" from "the nest reset and the reply was lost" — and clearing in the second case is CR-1 again. But the row is evaluated before every other launch row, so a slot left by a permanently-failed reset routed the client to a pre-filled claim page for a box that is still claimed and healthy, on **every** launch, with no in-app exit (short of a sign-out, which erases the identity). A client-side trap, not a nest-side one — the box stays fine — but the wrong shape.

*The fix (the same doctrine as the reset itself: a **boot reconcile**).* At launch, with a slot present, the launch machine asks the box (`LaunchMachine::start`, `libs/fauna-launch-machine/src/machine.rs` — one place, never re-derived per app). The probe already existed: it is what distinguishes the `ClaimCode` launch row.

*Reach — **all seven apps** inherit it (2026-07-13).* linux, tui, windows, android and web route their launch on the machine's `LaunchWizardEntry`, so the reconcile reached them for free from the start. **macOS + iOS used to hand-roll the launch branch in Swift** (`FaunaMacApp.swift` / `FaunaApp.swift`: `if let pending = keychain.loadPendingFactoryReset()` short-circuited ahead of the silent challenge, mirroring the machine's precedence *by convention* rather than calling it), so they never ran the probe and a stale slot trapped them on every launch. That hand-rolled branch is now **deleted**: both targets construct `LaunchMachine` over `KeychainLaunchPersistence` in `runLaunch()` and route on `LaunchSnapshot.phase`, so they inherit the reconcile like everyone else — the priority-#2 fix (consume the shared machine), not a second Swift copy of the probe. The one apple-specific rule the cutover had to keep: **re-read the slot *after* `start()`** when rendering the `PendingFactoryReset` row, because the reconcile may have just deleted it (an `if slot != nil → claim page` that does not consult the machine's verdict is the CR-2 bug wearing a new hat — web had exactly that shape too). **It answers in three ways, and the third is load-bearing** — the reconcile and the pre-existing `NotRegistered` fallback need *opposite* safe defaults for an unreachable box, so the probe reports a three-way `ClaimProbe` (`probe.rs`) rather than the `bool` it used to collapse every failure into:

| `fauna.setup.status` says | pending-factory-reset reconcile | `NotRegistered` fallback (unchanged) |
|---|---|---|
| `Unclaimed` | the reset really landed → resume the pre-filled claim | → `claim_code` |
| `Claimed` | never reset, or already re-claimed → **clear the slot**, take the ordinary rows (the silent challenge just logs the admin back in) | → `invite_request` |
| `Unreachable` | **keep the slot**, resume the claim — see below | → `invite_request` (assume claimed; graceful) |

**A probe failure must never be read as `Claimed`.** The old `bool` probe resolved *any* failure — connect, transport, decode, server error — to `true`, which is the right default for the `NotRegistered` fallback but is data-losing here: it would make a launch during a network blip delete the only copy of the claim code for a box that really *was* wiped. That is CR-1 again, reached through a new door. An unreachable box tells us nothing, so the slot survives and the probe re-runs on the next launch — the same asymmetry as the store-write rule above: *not* clearing is always recoverable; clearing wrongly is not.

**And `Claimed` itself must not be read as "stale" for a FRESH slot (grace refinement, 2026-07-17).** Between the reset dispatch and the box's self-exit the box still answers `Claimed`, so a launch in that window (web enters it by design — the admin page navigates to onboarding the moment the reply lands) used to clear the just-minted slot moments before the wipe: CR-1 loss delivered by this very reconcile, measured on web (journey 4). The probe alone cannot distinguish "reset in flight" from "dispatch failed for good"; **time can**: the slot records `minted_at_secs`, and a `Claimed` probe within `FACTORY_RESET_CLAIM_GRACE_SECS` (15 min, hard-coded) HONORS the slot — the pre-filled claim page shows a visible, retryable "already claimed" error if the reset really did fail, and the stale-clear resumes past the grace. The stamp is required: the one writer always sets it, and a record without it does not parse (the adapter reads an unparseable slot as no slot).

*Proof.* Five transition tests (`libs/fauna-launch-machine/tests/pending_factory_reset_reconcile.rs`) cover all arms — the fresh-slot-on-`Claimed` honor included — plus the guard that the reconcile costs the ordinary launch path **no** round trip (the probe fires only when a slot is present). The harness journey `test_failed_factory_reset_does_not_trap_the_client` drives the STALE arm end-to-end: the `seed_pending_factory_reset` seam arranges a slot stamped twice the grace in the past — overwriting any fresh mint, since a seconds-old slot would correctly be honored, not cleared — against a claimed, healthy box; the relaunched client must land **logged in** with the slot gone, asserted across *two* consecutive relaunches, so a reconcile that routed past a stale slot without deleting it cannot pass. (The seam writes BOTH slot shapes on every driver: the per-actor row `fauna/<actor_id>/pending_factory_reset` — aged by moving its `minted_at_secs` past the grace, never invented — and the legacy global triple. The registry's legacy fallback is index-gated (`may_read_legacy_globals` → `index_absent`), so a legacy-only seed is invisible to the machine on any indexed install and the journey would green **vacuously**: no slot means no trap, and "landed on the feed" then proves nothing. Web has written the per-actor row since 2026-07-17; **the native seams were unified onto it 2026-08-28**, when journey 6 first ran with the client's own mint reliably landing and the honored-fresh-slot failure exposed that the legacy triple the native seams wrote had never been the row being read.) The within-grace failed-dispatch scenario — fresh slot + still-claimed box → claim page with a visible exit — is now itself harness-proven end-to-end (journey 11, 2026-07-17): `test_within_grace_failed_reset_honors_fresh_slot_with_visible_exit` confirms against a live nest holding the dispatch ahead of its handler and takes the nest down still holding (deterministic — `stage_factory_reset` never runs, so the box cannot be wiped; see the journey's own docstring for the two racy arrangements this replaced, and `apps/apple-e2e-automation.md` § The actuation gate for why the live nest is also what makes the `OnlineOnly` confirm click legal at all) and, unlike the STALE-arm journey above, leaves the client's own CR-1 mint+persist untouched rather than overwriting it, so the reconcile sees a genuinely fresh slot; asserts the relaunch lands on the pre-filled claim page (not feed), submitting it against the still-claimed box surfaces the dedicated `onboarding.claim_code.error.already_claimed` message on `claim-code-status`, and `claim-code-back-button` exits cleanly. Green on `--client web` and `--client linux`, ×2 consecutive full-suite runs, and on `--app macos`/`--app ios` (2026-08-01) — see the per-app note above for the two apple-specific bugs the journey surfaced. Closing this journey also surfaced and fixed a real gap: `ClaimAdminError`'s catch-all had been folding the nest's `fauna.auth.already_claimed` rejection into the generic `onboarding.claim_code.invalid` message, which has no `.details` to substitute for this code and so fell back to the bare wire string (`libs/fauna-onboarding-machine`).

**CR-3, the per-install slot collision + the upgrade re-key — shared half FIXED 2026-07-13.** *The gap was:* the slot (and its two sibling wizard-resume slots) is **per-IDENTITY** on linux/tui — `fauna/{actor_id}/pending_factory_reset` through the shared `AccountRegistry` — but web / apple / windows / android implemented `LaunchPersistence` in-language over a **single global slot**, so with two accounts on one install, account B's factory reset would overwrite account A's pending row and destroy A's claim code (CR-1 for A's box, through the multi-account door — latent until the in-flight multi-user tracks land). *The fix is the seam collapse, not four per-app re-keys:* the four in-language `LaunchPersistence` impls are replaced by the one shared `RegistryLaunchPersistence` (per-actor by construction), reached over UniFFI via `FfiSecretStore`/`FfiAccountRegistry` (`libs/fauna-ffi/src/accounts_registry.rs`) and over wasm for web — the platform's only foreign seam is the key/value `SecretStore`. **The guarantee is per-identity: each account on an install can hold its own outstanding factory reset (and pending invite / awaiting-DNS), independently, swept with its identity on remove/sign-out.** Because the re-key from a global slot to `fauna/{actor}/…` was an **at-rest layout change**, the registry carried a legacy bridge (since RETIRED by the 2026-09-24/28 compat-remnant sweep: the registry now touches no `legacy/*` key) so a slot written by the *old* client version was still honored by the new reader (fallback read gated on no-index, migration materializes, the legacy keys stayed as a set-or-clear downgrade mirror of the *active* account) — without it the fix itself would have delivered the exact CR-1 loss it exists to prevent, on upgrade; the retirement is owned by [`../long-term-store.md`](../long-term-store.md) § Downgrade mirror + abandoned-append recovery. *Status:* shared Rust + the UniFFI seam + the upgrade/downgrade bridge landed 2026-07-13 (unit-proven end-to-end through the real `LaunchMachine`, including the upgrade-boot route to the pre-filled claim); web's wasm wiring landed 2026-07-13 and **windows' `FfiSecretStore` leg landed 2026-07-14** (per-identity pending slots; the pre-upgrade global `FaunaPendingFactoryReset*` rows are bridged by its key map, with an end-to-end test through the real `LaunchMachine` proving the upgrade boot still routes to the pre-filled claim). **Apple's `FfiSecretStore` leg landed 2026-07-14** too, independently and the same day (`KeychainSecretStore` maps the `legacy/*` keys onto the Keychain rows the app has always written; `KeychainLaunchPersistence` is deleted, so there is one persistence path — two would have re-opened the collision from the other side; a Swift test seeds a real pre-upgrade Keychain layout and asserts the claim code still resolves). **Android's `FfiSecretStore` leg landed 2026-07-18** (`LogicalSecretStore` + `SecretKeyMap` map the `legacy/*` keys onto the native `secret_key`/`node_url`/slot rows the app has always written; `LaunchPersistenceImpl` is deleted, so there is one persistence path, and a host-JVM JNA test (`AccountRegistryLaunchTest`, `just android-host-test`, no Robolectric needed — it seeds an in-memory `SecretBackend` rather than `EncryptedSharedPreferences`) drives the real `LaunchMachine` across the `FfiSecretStore` seam to prove the upgrade boot still routes to the pre-filled claim). CR-3 is thus closed on shared Rust + all seven apps (linux and tui were never broken — both already went through the shared `AccountRegistry` per-identity from the start; web/windows/macos/ios/android's `FfiSecretStore`/wasm legs landed 2026-07-13 through 2026-07-18 above). **One hazard the apple leg surfaced and closed in shared Rust for every app:** `mirror_active_to_legacy()` on an *un-migrated* store used to ERASE the legacy pending slots — with no index, `active()` still resolves via the legacy fallback, so the mirror ran, found no per-actor slot, and its set-or-clear rule cleared rows whose only copy was the legacy one. The boot mirror could therefore destroy a live claim code on the first launch after upgrade: CR-1 through the upgrade door, delivered by the very bridge meant to prevent it. Both native legs wire a boot re-mirror, so both were exposed (linux escaped only by hand-rolling a call-site index gate). The mirror now refuses unless the index is materialized and readable, making the hazard unrepresentable rather than each platform's job to remember; regression: `a_boot_mirror_on_an_unmigrated_store_preserves_the_legacy_pending_slots`. *(History: the mirror and the legacy global slots were deleted 2026-09-28 by the compat-remnant sweep — `long-term-store.md` § Downgrade mirror + abandoned-append recovery — RETIRED.)*

**Audited 2026-07-15:** domain *remove* (single atomic soft-delete — `db.soft_delete_mail_domain`, one `UPDATE` stamping `removed_at`; dependent DKIM/TLS/alias rows survive for the atomic 30-day GC) and mail *disable* (client-orchestrated cascade that commits per row and resumes on re-dispatch) gained client-kill journeys (7–8); **bridge approve** is single-transaction-atomic (`approve_bridge_service_user`, one `conn.transaction` for the status flip + audit `users` row) and **DKIM/TLS provisioning** is nest-side per-write atomic (the `dkim_selector_activated_at` stamp is co-written with the selector in one `UPDATE`; TLS delivery self-heals via authoritative seal-on-read) — audited by analysis, no client-driven transition to journey. **Bridge *revoke* of an MTA — no interior tear exists.** The revoke is one row write (`CacheDb::revoke_bridge_service_user`) that touches no DKIM key — the key is the nest's own (`../../behavior/mail-bridge-lifecycle.md` § Service-user re-keying) — so a nest crash lands it committed or not, with nothing beside it to tear (the two-write DKIM-blob tear closed 2026-07-15 went with the sealed-blob path, 2026-10-04); the in-memory `revoke_actor_authority` cut runs *after* the durable commit and is re-derived from the revoked row on the next boot. Proven by the nest-kill journey `test_kill_nest_mid_mta_revoke_leaves_dkim_key_untouched` (journey 9, end-to-end: an MTA revoke interrupted by a nest kill leaves the DKIM selector set unchanged). **The admin deployment-wide mail-DISABLE flag reconcile is now a genuine boot reconcile, adversarially proven (journey 10, 2026-07-16).** The admin `set_mail_enabled(false)` toggle (distinct from journey 8's per-USER `DisableMail` cascade — different transition, different handler) writes **twice**: the authoritative `mail_enabled` DB toggle, then the derived `{data_dir}/imap-enabled` flag file the MDA's s6 run-script gates on (`bridge_blob_handlers.rs::set_mail_enabled_handler`). A nest crash between them can leave the flag disagreeing with the DB — recoverable-but-degraded (the MDA's up/down is briefly wrong), **never a brick**: the DB is authoritative and `fetch_config`/boot read it, so no off-box fix is ever needed. The DB is the single decision point; `reconcile_mail_enable_flag_once` re-asserts the derived flag from it. That reconcile is now a real **boot reconcile** — the six `mail_enable.rs::spawn_*_reconciliation` tasks run their first pass **immediately at boot** (previously they skipped the first tick, deferring the heal to the 60 s periodic interval), so a torn flag heals at restart, not up to a minute later — the boot reconcile the recovery technique above names. Proven by the nest unit tests `reconcile_removes_flag_when_disabled_but_flag_present` (heal logic) + `spawn_mail_enable_reconciliation_runs_a_boot_reconcile` (boot wiring, deterministic) and the nest-kill journey `test_kill_nest_mid_admin_mail_disable_reconciles` (journey 10, green on `--client linux`). Every client-reachable interior write is now audited **and** crash-atomic — **the last exception is CLOSED (found 2026-08-29, fixed 2026-08-30).** `reconcile_deployment_keypair`'s durable-file rewrite (`deployment_key.rs`'s `write_secret_file_0600`, the boot-time heal-forward branch a rotation ceremony's post-commit crash window can hit) **was** a truncate-then-write, not a single atomic decision point — a crash between the truncate and the write could leave `nest_deployment.key` zero-length or short — and the reconcile **used to** refuse to heal: it `bail!`ed on a wrong-size file instead of falling through to `adopt_db_key_into_file`, which exists precisely to reconstruct the file from the still-authoritative DB `nest_keypair` row. `write_secret_file_0600` is now a temp-file-then-`rename(2)` (the temp file and the parent directory both `fsync`ed), so a crash at any point leaves the durable file either wholly the old bytes or wholly the new ones, never short; and a malformed file that still somehow arises (external damage, or one already corrupt from before this fix) now heals from the DB row via `heal_malformed_file_or_bail` instead of bailing — only a file malformed **with no DB row to heal from** is still a hard bail, the pinning-break floor this module exists to protect. Proven by three tier_1 tests: `a_malformed_file_heals_from_the_db_row` (0-byte file + intact DB row → boot reconciles, file restored byte-identically), `a_malformed_file_with_no_db_row_still_bails` (nothing to heal from → still refuses), and `fauna_core::secret_file::tests::replaces_via_rename_not_in_place_truncate` (structural: a rewrite lands on a fresh inode, never truncates the live path in place — lifted into shared `fauna-core` 2026-09-01 so `fauna-iroh-relay`'s relay-key write shares the same proof; `deployment_key.rs`'s own `rotation_heal_replaces_via_rename_not_in_place_truncate` remains as the crate-local call-path pin). A session adding or touching any transition MUST run the verification question above and, if it can strand the box, fix it in the same change. The harness is the standing tool for closing these: add a journey per transition (its file docstring is the recipe).

## Factory reset

Factory reset is the canonical instance of the **Client-state recoverability** invariant above — the universal recovery floor (return to fresh/unclaimed), and itself crash-atomic via the boot-time marker.

The WS-RPC kind **`fauna.admin.factory_reset`** (Admin-gated;
`bridge_method_allowlist`) returns the nest to **fresh / unclaimed** so it can be
re-onboarded from a client as if newly deployed — the enabler for the
client-driven live-mail test (designed 2026-05-30; tracked internally)
and the admin "start over" affordance on a disposable box.

**Mechanism — restart-wipe** (not in-process truncation, which is incomplete and
racy while the DB is open). The handler:

1. Determines the post-reset claim code — `new_claim_code` if the request pins
   one (trimmed, non-empty), else a fresh random single-use claim code
   (drawn from the ambiguity-free alphabet —
   `libs/fauna-core/src/claim_code.rs`).
2. Stages a `{data-dir}/factory-reset-requested` marker carrying that claim code.
3. Replies `FactoryResetReply { claim_code }` — returned **synchronously** so the
   caller drives the re-claim with it (never reads `/data` off the box). The reply
   is a convenience, not the client's only copy: clients mint and persist the code
   *before* dispatching and pin it via step 1, so a client killed before rendering
   the reply still resumes the re-claim from its own slot (this is what closed gap
   CR-1 — § Client-state recoverability → Implementation status today).
4. Schedules a short-delayed `std::process::exit(0)` (after the reply flushes +
   a DB flush). The s6 `longrun` supervisor restarts the process.

At the **next boot**, before the DB is opened, `factory_reset::maybe_run_factory_reset`
detects the marker and wipes deployment state, then installs the staged claim
code and clears the marker. Implementation: `bins/fauna-nest/src/factory_reset.rs`.

| Wiped (deployment state) | Preserved |
|---|---|
| `nest.db` (+ `-wal`/`-shm`) — all actors, handles, admin grants, mail domains, `account_aliases`, bridge enrollments, mail records, audit log | *(the legacy `nest_identity.key` host key is **retired** — the deployment key below is the nest's single identity)* |
| `blobs/` contents — mail blobs, bridge wrapped-MLS blobs, content blobs | `acme/` (on-disk TLS cert — the box stays reachable for the re-claim) |
| every service-gating enable flag → deleted (`imap-enabled`, `caldav-enabled`, `carddav-enabled`, `webdav-enabled`, `atproto-enabled`), and with the database every service switch → **unset**. Unset mail reads off, and an unset calendar, contacts or files switch follows mail (`../../behavior/caldav-server.md` § Independent enablement), so straight after a reset nothing is served; the three come back with mail when it is switched on again, unless the admin sets them otherwise | `nest.toml`; the mail domains' DKIM signing keys — **carried** across the wipe, not left in place (*The DKIM keys are carried*, below) |
| | bridge keypairs under `keys/` (bridges keep their identity and re-enroll against the fresh nest — auto-approved once mail is re-enabled, § Onboarding auto-approval) |
| `claim-code` → replaced by the staged code | `nest_deployment.key` (the **deployment signing key** — the channel-binding `nest_actor_id` a client TOFU-pins / a public domain publishes as DNS `self=`; see below) |

After a reset the nest boots fresh: migrations recreate an empty DB, the claim
code is the staged one, storage is the single sealed implementation from first boot (no unconfigured state — `storage-modes.md` § Boot story), and the co-located mail
bridges re-enroll over loopback. On re-enable — the normal real-domain re-claim,
email-on-by-default at claim — they are **auto-approved** (§ Onboarding
auto-approval), so no manual re-approval click; a bridge that re-enrolls while
mail is still off lands `pending` until the admin re-enables. See
`mail-bridge-lifecycle.md`.

**The DKIM keys are carried (ruled and built 2026-10-04).** A mail domain's DKIM key rests in the database the wipe deletes (`mail_dkim_keys` — custody owner [`../../behavior/mail-bridge-lifecycle.md`](../../behavior/mail-bridge-lifecycle.md) § DKIM provisioning (automatic)), and its public half is published in DNS, in manual mode by hand: a reset that lost the key would leave every re-registered domain's mail failing DKIM until a person republished the record. So the key crosses the reset, as the deployment key and the certificate do. Three steps. (1) Before it deletes the database, the wipe copies each active domain's **active-selector** key — sealed, as it rests — into `{data-dir}/dkim-keys-carried`; a re-run of the wipe after the database is gone leaves that file as it is. (2) The boot that follows seats those keys on the fresh database marked *carried* (`mail_dkim_keys.carried_at`, nest schema 124) once the deployment key is reconciled, and removes the file. A carried key is no selector to any reader — `fauna.bridges.list_dkim_selectors`, `fauna.setup.status` `dkim_records` and the sign site see none — so the unclaimed box says nothing about the domain it used to serve. (3) **Re-attachment is by (domain, selector).** `mail_domains` is wiped too, so the key waits for the door that registers its domain again (the claim's primary domain, or a later add). That door registers the domain under the carried key's selector when the caller names none — a domain that had rotated to a `<YYYYMM>` selector comes back on the one its DNS still publishes — and every mint door adopts a carried key for its (domain, selector) in place of minting. The selector list is then what it was before the reset: same selector, same `public_dns_value`, same mint time. **Not carried:** a superseded or not-yet-active selector's key (a rotation in flight starts over) and a soft-removed domain's. **Best-effort, never a condition of the reset:** the reset is the recovery floor, so a database that cannot be read, or a key that does not open under the box's deployment seed at the next boot, carries nothing — that domain mints a new key when it is added, and its record is republished (`mail-bridge-lifecycle.md` § DKIM provisioning (automatic) → *No DNS auto-reconcile in manual mode*). **Residue:** a carried key whose domain is never registered again is deleted by the mail-domain GC on the 30-day soft-delete window. Code: `mail_dkim_key.rs` (*Carried keys*) and `factory_reset.rs` step 0. Pinned: `factory_reset::tests::{a_factory_reset_carries_the_dkim_key_to_the_re_registered_domain, an_unclaimed_carried_key_is_never_listed, a_rerun_of_the_wipe_keeps_the_carried_keys}`.

**Why the deployment signing key is preserved.** The `nest_keypair` lives in
`nest.db` (wiped) and migrations would otherwise mint a *fresh random* one on the
post-reset boot — silently rotating the deployment identity. That identity is the
`nest_actor_id` a self-signed/LAN client **TOFU-pins** through the TLS channel
binding and a public domain publishes as DNS `self=` (`security.md` § Transport
trust). A rotation it didn't ask for makes every pinned client reject the next
channel binding as a TOFU identity change and refuse to reconnect — no TLS error;
since 2026-07-13 the shared launch machine surfaces this as the identity-changed
warning with an explicit per-user re-trust button (`security.md` § Transport
trust), so the box is reachable again only after every user of the nest manually
clicks through a warning that is indistinguishable from a real MITM — the exact
click-through habit the warning must never train. So the
key is given a durable file home (`nest_deployment.key`, a raw 32-byte seed not in
the wipe's delete list); on boot
`deployment_key::reconcile_deployment_keypair` restores it into the freshly-migrated
DB, so the re-claimed nest re-presents the **same** identity and pinned clients
reconnect seamlessly. This deployment key is the nest's **single** identity
(single-identity unification, `box-recovery.md`): `start_server` derives
`nest_identity` (nest.info/federation/backup/pairing/sync) from it, and the legacy
separate `nest_identity.key` is retired. The key still rotates on a *deliberate* admin event —
the rotation ceremony, [`box-recovery.md`](box-recovery.md) § Deployment-seed
rotation (ratified 2026-08-11; build status owned there, § Implementation status
today) — never on a reset. (A WebPKI/public-CA client
is unaffected either way — it authenticates the box via WebPKI and never pins
`nest_actor_id`.) This on-disk preservation covers a **wipe** — the disk
survives. **Total box loss** (the disk itself destroyed, so `nest_deployment.key`
is gone) is the strictly harder case the on-box copy cannot cover; off-box
recovery of the *same* identity — the seed custodied in the admin's account-state plane kind
`fauna.state.deployment-seeds` ([`../config-dissolution.md`](../config-dissolution.md)
§ The `__config` dissolution schedule → *The kinds*) and re-installed at re-provision — is specified in
[`box-recovery.md`](box-recovery.md).

**Persistent-client reconnect across a reset — re-onboarding by design, not seamless session resume (WONTFIX).**
The deployment-key preservation above makes the *box identity* (the channel-binding
`nest_actor_id`) stable across a wipe, so a TOFU-pinned client's *transport* no longer
rejects the re-claimed box (TRACK 1). It does **not** — and deliberately should not —
make a persistent client's *actor session* silently resume: the wipe clears `nest.db`
including the actor's own row, so the re-claimed box has **forgotten the actor**. Recovery
is **re-onboarding** (the universal recovery floor, § Client-state recoverability), which is
exactly what the wiped box routes the client through. Mechanism: the persistent supervisor's
WS drops at the reset-restart as a transport close → `Retry`, not 4401 (`supervisor.rs`); on
reconnect the stale cached bearer is rejected, and on the 4401 the client's `refresh_auth`
(`libs/fauna-client/src/reconnect.rs`) clears + re-mints via `fauna.auth.handshake`
(the shared `auth_core::direct_auth_core`; its `POST /api/v1/auth/token` HTTP twin was
deleted at the rip-out endgame). Against a wiped box the re-mint hits `NotRegistered`
→ `refresh_auth` errors → the supervisor stops (`SupervisorError::AuthRefresh`) and the client
surfaces re-onboard UI — the correct terminal state, and now the **only** one, in every
registration mode.

This used to depend on the nest's posture. On an "open" nest the re-mint instead
*auto-registered a fresh ghost `free` actor* (no handle, no prior data, no admin grant) and
"succeeded" — silently masking the wipe under the user's old identity key, which is *worse*
than a clean failure. That branch is **deleted** (2026-07-12): an actor with no `users` row is
refused in every mode, so the wiped box can no longer invent an account for the returning
client. Owner: [`public-mode.md`](public-mode.md) § Implementation status today.

So seamless keepalive auto-reconnect across a wipe+reclaim is **WONTFIX**: undesirable, not merely
infeasible — re-establishing a session against a box that forgot you is semantically wrong. The
precise close-code path and how the re-onboard prompt is surfaced are a app-UX concern, out of
scope for this nest-side decision.
*(Refutable: a future design that genuinely wants minimal-disruption resume would still detect
the forgotten-actor / wiped-data condition and prompt, never silently auto-register — so it
reduces to better re-onboard UX, not a nest auto-reconnect.)*

> **Gating — DEFERRED (v1).** Factory reset is `Admin`-gated only; the single-admin
> dogfood box's admin is effectively the superadmin. A cooldown / finer
> superadmin-role check (client-compromise protection — this is a powerful
> destructive op with no rate-limit in v1) is a **follow-up before any non-dogfood
> use**. Acceptable today on the disposable dogfood VPS
> (`dogfood-vps-is-disposable-preprod`).

### Implementation status today

Implemented: the kind, the restart-wipe mechanism, the wipe/preserve split above
(including deployment-key preservation via `deployment_key::reconcile_deployment_keypair`
+ the durable `nest_deployment.key`, tested in `deployment_key::tests` —
`deployment_key_is_stable_across_factory_reset_wipe`), and the claim-code round-trip
in the reply (`factory_reset.rs` + `admin_ws_handlers.rs`, tested in
`factory_reset::tests`). **Not yet implemented:** the gating follow-up (cooldown /
superadmin-role distinction) — tracked in the goal-doc note above.

**✅ CLOSED 2026-07-23 — the wipe list now clears every service-gating enable
flag.** `caldav-enabled` was added to the delete list in the same commit as the
CalDAV-enablement feature itself, but the later CardDAV and WebDAV enablement
slices never made the matching addition, so the wipe removed only `storage-mode`,
`imap-enabled`, and `caldav-enabled`. Re-verification while closing it found a
**third** missed flag, `atproto-enabled` — worse than its two siblings because it
has no boot reconcile at all, and because the ATProto bridge only became genuinely
supervised in the same change (`installers/docker.md` § Implementation status
today: the service had no `contents.d` marker, so nothing started it either way).
All five are now deleted by constant rather than string literal, so a renamed flag
fails the build instead of silently dropping out of the reset, and the wipe test
enumerates the same constant list. **This was never a client-state-recoverability
violation:** the MDA binds each protocol's listener from its own `fetch_config`
read of the (wiped, freshly-migrated) DB toggle, never from the on-disk flag, so a
stale flag could not re-expose CardDAV/WebDAV on a reset box; the effect was a
service held up serving nothing.

---

## Configuration

### NAT mode (deployment-mode axis persistence)

The public/private deployment-mode axis is persisted as the app-set **`nest_nat_mode`** DB singleton — authoritative from the moment an app sets it (wizard step: [`../../behavior/onboarding.md`](../../behavior/onboarding.md) § 3b-bis). The **`FAUNA_MODE`** env var is a **pre-claim boot seed only**, set by the deployment artifact (the home compose bundle sets `private`); accepted values are exactly `public` | `private` — `NodeMode::from_wire_str` ignores anything else and the box boots the default public posture — and every runtime reader consults the DB-resolved `AppState.node_mode`, never the config/env seed (`bins/fauna-nest/src/main.rs`, `db/nest_nat_mode.rs`). Mode *semantics* → [`public-mode.md`](public-mode.md) / [`private-mode.md`](private-mode.md); the two-box home composition → [`deployment-home-with-public-relay.md`](deployment-home-with-public-relay.md); this doc owns only the persistence + seed rule.

### Serving ports — client-facing API + CalDAV (admin choice; default 443)

A nest has two externally-facing serving ports: the **client-facing HTTPS API port** (the WS-RPC transport + the served SPA; default `443`) and the **CalDAV/IMAP port** ([`../../behavior/caldav-server.md`](../../behavior/caldav-server.md) § Network exposure). Both are **admin choices** per the iron-clad config-surface invariant (`principles.md` § One configuration surface) — a port a human picks is **app UI + nest state**, never a config file an operator edits. **There is no operator:** on a cloud deployment the user picks a provider (e.g. Hetzner) at onboarding and *our* provisioning orchestrator stands the box up from the API keys, so every layer — the Docker port-map, the `ufw` firewall, the DNS — is written by us (`libs/fauna-provisioning/src/cloud_init.rs`, e.g. `ports: "443:443"` + `ufw allow 443/tcp`).

The admin's chosen client-facing port is the `serving_port` singleton (set via `fauna.admin.set_serving_port`, Admin-class; default [`fauna_protocol::node_policy::DEFAULT_SERVING_PORT`] = 443). It is the **value** the admin wants; which of *our* components *realizes* it depends on the deployment (the realization plumbing — interface/host, the container-internal `:3000`, the loopback split — is bucket-2 IPC, exactly as `caldav_port`'s value is the bucket-3 choice while `caldav_bind_host` is bucket-2 plumbing):

- **Desktop-direct / bare-IP / domainless** (no router) — the nest binds `serving_port` on its **external** client-facing listener (`0.0.0.0:serving_port`) **and** a **fixed internal-loopback listener** `127.0.0.1:`[`CANONICAL_INTERNAL_LOOPBACK_PORT`][`fauna_protocol::node_policy::CANONICAL_INTERNAL_LOOPBACK_PORT`]` = 3000` (the canonical `--bind`/`listen`/`FAUNA_PORT` internal port) for **co-located IPC** — see *Same-box reach* below. Apply-on-restart: the nest cannot hot-rebind its own `TcpListener` (unlike the CORS/registration `ArcSwap` singletons), so the desktop supervisor reads the `/data/serving-port` value-flag and restarts the nest; the **external** listener moves to the new port while the **internal-loopback** listener stays fixed, so co-located clients (bridge + same-box app) — which dial the fixed loopback — are never stranded. (On desktop the off-box firewall rule for the chosen port is a **sanctioned manual user step** — a narrow, explicit exception to the no-operator rule: the user adds a one-time OS firewall allow for the port they picked in the app. The Windows installer keeps its `:443`-pinned rule, correct for the default port out-of-box; it is not auto-managed. Decision: user, 2026-06-21.)
  - **How the nest tells the two cases apart** (bucket-2 IPC, no operator): the boot-resolve overrides the `--bind`/`listen` seed's **port** with `serving_port` **unless** `FAUNA_FRONTED_BY_ROUTER` is set in its environment. The Docker image's nest run-script (`docker/s6/fauna-nest/run`) sets it unconditionally — the image **always** runs the SNI router, so every Docker deployment (domain, bare-IP, dev) is fronted. The Windows desktop service, the **macOS desktop nest daemon** (`social.fauna.nest` under `_fauna` — `installers/macos.md`), and a hand-run bare-metal `fauna-nest --bind` set nothing ⇒ direct listener ⇒ the singleton applies. (The two desktop services also set `FAUNA_INTERNAL_LOOPBACK_PORT` so the nest binds the fixed `127.0.0.1:3000` co-located-IPC listener alongside the external port — § *Same-box reach*.) So the discriminator is "is this the SNI-router-fronted Docker artifact", detected by the artifact declaring itself fronted — not a heuristic on the bind address (the Docker nest binds `0.0.0.0:3000`, not loopback). Inert behind the router means the singleton is *not* applied to nest's own bind (it still rides `setup.status`); the external port there is realized by the cloud path below.
- **Cloud VPS / any router-fronted (Docker) deployment** — the client-facing API is served on a **fixed `443`** by the always-up `fauna-sni-router`, which owns the external `:443` and forwards to nest's internal `:3000` (the compose port-map maps host `443` → container `443`). The chosen `serving_port` singleton is **inert** for nest's own bind here. **Decision D (user, 2026-06-23): a custom `serving_port` is a *direct-listener* feature only (desktop / self-hosted / bare-IP) — a Docker/cloud nest always serves `443`.** The cloud-realization that would have let a cloud box honor a non-`443` port — threading the port into the cloud-init port-map + `ufw`, a no-SSH live host reconfigure, the WS-push, and a client-published `_fauna._tcp` SRV — is **NOT pursued**: a domain box's handle is already port-hidden (SRV) and `443` is the universal expectation, while the only no-host-exec realization (host-network mode + the router rebinding on the `/data/serving-port` flag) costs a container-isolation + firewall-posture change not justified by the near-never need to move a cloud box's API off `443`. Full reasoning: the plan's § *Decision 3b — design analysis* + § *Decision D*. Because the value can never apply behind the router, **`fauna.admin.set_serving_port` is rejected on a router-fronted nest** (`is_fronted_by_router()`; error code `fauna.node_policy.serving_port_fronted`) rather than silently persisting an inert value — no config theatre; `setup.status.serving_port` then reads the default `443`. So the admin never even reaches that rejection, the nest advertises the wiring fact on the anonymous heartbeat — **`setup.status.fronted_by_router`** (`true` on a fronted box, sourced from `is_fronted_by_router()`) — and the admin app renders the `admin-nest-serving-port` field **read-only** ("served on 443 by this deployment") when it is `true`, surfacing the port as a non-choice on *this* deployment exactly as the one-configuration-surface invariant prescribes (the editable field is a genuine admin choice only on a direct-listener). The rejection is the floor (no silent theatre even against an older/edge client); the read-only render is the target UX.

A **live** `serving_port` change therefore only happens on a **direct-listener** (non-fronted) nest, where the desktop supervisor restarts the nest onto the new bind (above). No client is stranded and no path reaches a client-unrecoverable state (§ Client-state recoverability): co-located clients ride the fixed internal loopback (§ *Same-box reach*); a remote/LAN client whose persisted URL fails re-resolves `_fauna._tcp` when the box has a public SRV (Pillar B SRV-aware reconnect), else re-enters `host:newport` — never SSH / manual-DB recovery. (A WS-RPC push of the new port to connected clients is a possible future nicety, **not** required for the invariant; it was previously scoped for the now-dropped cloud live-change.)

> **⚠ Privileged-port safety (bind-fallback LANDED 2026-06-25; setup.status surfacing pending).** The recoverability above holds only if a `serving_port` the nest **cannot bind** never bricks the box. `fauna.admin.set_serving_port` accepts any `1..=65535` (rejects only `0` + router-fronted — `node_policy_handlers.rs`), so an admin picking a privileged `<1024` port on an **unprivileged** direct-listener (a macOS per-user `LaunchAgent`, or a Linux systemd unit without `CAP_NET_BIND_SERVICE`) would `EACCES`; a once-fatal external bind (`lib.rs` `TcpListener::bind(addr).await?`) → `KeepAlive` relaunch → **permanent crash-loop** with the bad singleton persisted in `nest.db`, **client-unrecoverable** — a breach of § Client-state recoverability. **Implemented (the stay-up half):** the external bind now uses a **bind-fallback** (`lib.rs::serving_port_bind_should_fall_back` + the `start_server` bind site) — when the **resolved** admin port (not the seed) is unbindable for `EACCES`/`AddrInUse`, the nest logs the failure and **falls back to the bind seed instead of crashing**, staying reachable so a client can pick a bindable port; a seed the artifact itself can't bind, or any other error, still propagates. The pure decision is unit-tested (`serving_port_bind_fallback_tests`). (The macOS **machine-daemon** re-shape independently lets the nest bind `:443` via launchd socket activation, removing the most common trigger; the fallback is still required for genuinely-unbindable ports. **Socket activation is wired daemon-side (2026-06-25, tracked internally, S1):** `fauna-nest-daemon` inherits the root-pre-bound `:443` fd via `launch_activate_socket("FaunaNest")` and serves it through `start_server`'s new **pre-bound-listener seam** (`external_listener` — when `Some`, the resolve/bind/seed-fallback is skipped because launchd owns that port). The shared serve loop consumes the activated listener on the **first** iteration only; an admin `serving_port` change re-enters `start_server` with the listener already taken, so the restart **direct-binds** the new port (and the bind-fallback covers an unbindable choice). The one residual — an admin moving the port *back* to the launchd-owned `:443` after a change — needs a real launchd to design+test and is an e2e that needs a maintainer-run environment; recoverability still holds, as the nest stays up on the seed. The plist `Sockets` dict that declares the `FaunaNest` socket ships with the `.pkg` (S3).) **Still pending (the surfacing half):** recording the failed choice on `setup.status` (an additive `serving_port_bind_failed: Option<u16>` field across `SetupStatus`/`SetupStatusReply`/FFI/wasm) so a client can show "your chosen port couldn't be bound — the nest is serving on the seed" instead of `setup.status.serving_port` misleadingly reporting the chosen (un-bound) port. Deferred to land **with its app consumer** (no app reads it yet — the `fronted_by_router` precedent added the field with its 6-app UI). Tracked internally (the 2026-06-24 macOS machine-service decision record, Issue A).

#### Same-box reach — co-located IPC rides a fixed internal loopback (not the movable `serving_port`)

The three self-heal paths above cover **remote** clients. A **desktop** box additionally hosts **co-located** processes — the mail **bridge** and the **same-box desktop app** — that reach the nest over loopback. These must **not** chase the external `serving_port`: that is bucket-2 artifact-wiring IPC (the "nest↔MDA↔SNI-router loopback split" of `principles.md` § One configuration surface), distinct from the bucket-3 admin-chosen external port. So co-located IPC rides a **fixed internal-loopback port** (`127.0.0.1:`[`CANONICAL_INTERNAL_LOOPBACK_PORT`][`fauna_protocol::node_policy::CANONICAL_INTERNAL_LOOPBACK_PORT`]` = 3000`, the canonical internal port) that **never moves** when the admin changes `serving_port`:

- **Docker already has this split intrinsically** — the nest binds `0.0.0.0:3000` and the SNI router fronts the external port, so the in-container bridge dials `127.0.0.1:3000` (a fixed internal port) and `serving_port` is inert for nest's own bind. No change.
- **Desktop-direct** — the nest binds the **fixed `127.0.0.1:3000` listener alongside** the external `0.0.0.0:serving_port` listener (an **additive** second listener serving the **same router + same self-signed floor cert** — SPKI-pinned per authority, so a distinct loopback port is trusted identically). The co-located bridge and the same-box app both target `127.0.0.1:3000` — the bridge supervisor hands its Go MDA child a fixed `--nest-endpoint https://127.0.0.1:3000` (`libs/fauna-mda-supervisor`, on macOS/Linux/Docker), while on Windows the same value rides `device.toml.nest_port` (`fauna_ipc::DeviceConfig` is Windows-only — the cross-OS bridge does NOT read a `device.toml`). A `serving_port` change moves only the **external** listener; the internal one is fixed, so same-box clients are never stranded and the admin can always change the port back from their own box. **This is what makes a desktop `serving_port` change safe** (closes the DoD-#6 same-box gap; supersedes the earlier "design fork" — resolved 2026-06-22 via the long-term question + the product invariant).
- **Mechanism (bucket-2 IPC, no operator):** the extra listener is requested by the **in-process Windows nest-service** via the `FAUNA_INTERNAL_LOOPBACK_PORT` env (the artifact-wiring sibling of `FAUNA_FRONTED_BY_ROUTER`; resolved in `lib.rs::resolve_internal_loopback_addr`). Absent on Docker (already fronted) / dev / e2e / bare-metal ⇒ no extra listener, zero behavior change. The same-box **app** therefore needs **no** WS-RPC push for a *same-box* port change (it dials the fixed loopback) — the WS-push (path 1 above) is for **remote** connected clients only.

### Implementation status — serving ports (per-pillar matrix)

| Pillar | Status |
|---|---|
| `caldav_port` (admin-set CalDAV/IMAP port) | Built — `db/caldav_port.rs`, `fauna.bridges.set_caldav_port`; UI on all 7 apps. |
| Pillar A — `serving_port` substrate | Built — `db/serving_port.rs` singleton; `fauna.admin.set_serving_port` (Admin-class; **rejected on a router-fronted nest** — `fauna.node_policy.serving_port_fronted`, no inert-value theatre); desktop boot-resolve (`lib.rs::resolve_serving_bind_addr`, `FAUNA_FRONTED_BY_ROUTER` gate); `/data/serving-port` value-flag; the shared cross-OS serve/restart loop `fauna_nest::desktop_serve::run_serve_loop`, driven by the Windows SCM service and the macOS `fauna-nest-daemon` (`bins/fauna-nest/tests/desktop_serve_loop.rs`). |
| Same-box reach (fixed internal loopback) | Built — `resolve_internal_loopback_addr` + the additive `127.0.0.1:3000` listener (`FAUNA_INTERNAL_LOOPBACK_PORT`); Windows dialer legs landed (`device.toml.nest_port=3000`, app fallback `:3000`). The live port-change → co-located-dial proof is now automated (`bins/fauna-nest/tests/desktop_serve_port_change_loopback.rs`, landed 2026-07-10): it drives the shared `run_serve_loop` through a real `db.set_serving_port` + flag-file admin change and asserts the fixed loopback keeps serving across the restart while the external listener moves — no real Windows machine or bridge process needed, since the loop is the same one both desktop shells run. |
| Bind-fallback (unbindable admin port never bricks) | Stay-up half built — `lib.rs::serving_port_bind_should_fall_back` (EACCES/AddrInUse on the resolved admin port falls back to the bind seed; unit-tested). **Pending: the surfacing half** — an additive `serving_port_bind_failed: Option<u16>` on `setup.status`, deferred to land with its first app consumer. Tracked internally (the 2026-06-24 macOS machine-service decision record, Issue A). |
| macOS `:443` socket activation | Wired daemon-side (`launch_activate_socket("FaunaNest")` → `start_server`'s pre-bound-listener seam); the admin-moves-port-back-to-`:443` case needs a real launchd e2e (needs a maintainer-run environment; the nest stays up on the seed either way). |
| `fronted_by_router` read-back + read-only field UX | Built on all 7 apps — `setup.status.fronted_by_router` (tier_3-proven both ways: `serving_port_fronted.rs`, `serving_port_api.rs`); `admin-nest-serving-port` field gated read-only + the shared `serving_port_fronted_hint`. |
| App `set_serving_port` surface | Built on all 7 apps — shared `AdminClient::set_serving_port` → `FfiAdminClient::set_serving_port` (UniFFI) / wasm `adminSetServingPort`; `admin-nest-serving-port-*` IDs. |
| Pillar B — SRV handle-reach + SRV-aware reconnect | Built (native + wasm; additive + self-correcting — dormant until a `_fauna._tcp` SRV is published). |
| Pillar C — host-class scheme-guess removal | Built — `resolve_handle_domain` returns uniform `https`; same-box/e2e reach rides the injected `nest_url` override (`../../behavior/onboarding.md` §2). |
| Cloud non-443 realization / WS port-change push / SRV publish | **NOT pursued** — Decision D (user, 2026-06-23): a Docker/cloud nest always serves `443`; rationale in the plan's § Decision 3b + § Decision D. |

The nest's client-facing listener surviving a storage-mode commit was tier_3-pinned historically (`tests/e2e-unified/tests/api/test_storage_mode_encrypted_serve_tls.py`, predating the Phase-4 axis retirement) — TLS serving is now unconditional from first boot regardless (`storage-modes.md` § Boot story). Design: the admin-choosable serving-ports design (2026-06-21, tracked internally). The dated landing history (including the resolved 2026-06-22 windows-e2e misdiagnosis — a harness nav gap, not a nest regression) lives in git.

### CLI Flags

**Core**

| Flag | Default | Purpose |
|------|---------|---------|
| `--bind` | — | Address to bind the HTTP server to (full SocketAddr). Overrides config file `listen`. Falls back to config value, then **`0.0.0.0:3000`** — **all interfaces** (a nest listens with no distinction by client origin: `localhost`, LAN, and WAN reach it identically — see [`../installers/windows.md`](../installers/windows.md) § Network-reachable nest), on the canonical internal nest port `3000` (the `FAUNA_PORT` default that Docker fronts with the `:443` SNI router; `:443` is privileged on Linux, so the unprivileged dev binary keeps `3000` while production fronts the uniform client-facing `:443`). Mirrors `config/default.toml`'s `listen`. **Pre-claim seed only for the *port* on a direct-listener deployment** — once claimed, the app-set `serving_port` singleton (`fauna.admin.set_serving_port`, Admin-class) overrides the port component (boot-resolved, applies on restart — see § Serving ports below). The interface/host stays artifact-wiring; behind the SNI router the whole bind is artifact-wiring and the singleton is inert. |
| `--db` | — | Path to the SQLite database file. Overrides config file `db_path`. Falls back to config value, then `./nest.db`. |
| `--blob-dir` | — | Directory for blob/backup storage |
| `--config` | — | Path to TOML configuration file. CLI flags (`--bind`, `--db`, `--cors-origin`) always take precedence over config file values. |
| `--static-dir` | — | Directory of static web assets (the served SPA bundle). |
| `--cors-origin` | — | **Pre-claim seed only** — the trusted browser origin(s) for nest's own HTTP API (repeatable). The authoritative value is the app-set `nest_cors_origins` DB singleton (`fauna.admin.set_cors_origins`, Admin-class), boot-resolved into the live `AppState.cors_origins` (an `ArcSwap` the CORS layer's `AllowOrigin::predicate` reads per request, so a change applies without a reboot) and read back on `fauna.setup.status`. An empty list trusts only the built-in default origin. A flag that is **given** replaces the config file's own `cors_origins` list wholesale (the `--config` row's precedence rule); a flag that is not given leaves it alone, which is the path the Docker artifact takes — its entrypoint seeds `[nest].cors_origins` from `FAUNA_CORS_ORIGINS` and passes no flag. Per the product invariant, an admin-chosen security policy comes from apps, not CLI/env. |

**Registration and Users**

| Flag | Default | Purpose |
|------|---------|---------|
| `--handle-domain` | — | Domain used in handle addresses (e.g. `fauna.social`) |

There is deliberately **no** `--reserved-handle` flag — it was **deleted 2026-07-17** after its plumbing was found to displace the default list at boot. The reserved-handle deny-list is now enforced only as the hard-coded shared constant `fauna_protocol::handle::RESERVED_HANDLES` — a correctness constant nobody configures.

The registration **posture** has no flags. `--registration-open`, `--registration-invite-required`, `--max-free-users`, `--require-registration` and `--no-require-registration` were all **deleted** on 2026-07-12: choosing who may register is an admin choice, so it is app-set nest state (`fauna.admin.set_registration_mode` → the `nest_registration_mode` singleton), with `[nest] registration_mode` as the pre-claim seed only. The one flag left above is not policy — a handle domain is derived from the nest's own domain. Owner: [`public-mode.md`](public-mode.md) § Registration Modes. This closes the recorded invariant violation that stood here.

**Worker**

There are **no** worker flags. The `--worker-key` and `--worker-allow-ip` flags were **removed 2026-10-02**: attaching a storage worker is an admin choice, so the ratified target is **enrollment-style worker authorization** (admin approves a worker from the app UI like a pending bridge; keypair discovered from the enrollment row — user ruling 2026-07-09), and until that lands the proxy authorizes no worker and its worker and sidecar WebSocket gates admit loopback only. Owner: [`worker.md`](worker.md) § Implementation status.

**TLS / ACME**

| Flag | Default | Purpose |
|------|---------|---------|
| `--acme-dir` | — | Directory to store ACME certificates (artifact wiring) |

> There is no flag for the ACME domain, contact or staging (removed 2026-10-02): the CA and the contact are constants and the domain comes from the claim — owner [`tls-certificates.md`](tls-certificates.md) § ACME settings — constants, not choices. The tier_4/pebble directory-URL wiring is `[acme] directory_url`, not a flag.

**Eviction** — there are **no eviction CLI flags**. The warning and suspension windows are
**hard-coded constants** ([`fauna_protocol::node_policy::EVICTION_WARNING_DAYS`] /
[`EVICTION_SUSPENSION_DAYS`][`fauna_protocol::node_policy::EVICTION_SUSPENSION_DAYS`], both `14`), not
an admin choice: [`../../behavior/admin.md`](../../behavior/admin.md) § 2 Users → *Cutting a user off*
names them only as ladder timings and surfaces no control for them on any app. Bucket (1) of the
one-configuration-surface invariant. (The `--eviction-warning-days` / `--eviction-suspension-days`
flags were removed 2026-07-12 with the registration-mode migration below; the values are still echoed
to apps on `fauna.account.get` (`AccountNodePolicy`) so they can render the eviction-warning UI.)

**Mail** — there are **no mail CLI flags**. The legacy in-nest SMTP path and its whole `--email-*`/`--dkim-dir` flag family were deleted with the `email` cargo feature at the I6 cutover; mail is solely the Go bridge + `libs/fauna-mail`, configured through the app UI + nest state (owners: [`../../behavior/mail-bridge-lifecycle.md`](../../behavior/mail-bridge-lifecycle.md), [`../../behavior/smtp-server.md`](../../behavior/smtp-server.md)).

**Bluesky** (requires `bluesky` feature) — there are **no Bluesky CLI flags**.
The OAuth `client_id` the bridge presents to Bluesky's authorization server is
`https://<identity-domain>/.well-known/atproto-oauth-client`, derived from the
domain the nest learns at claim (`AppState::handle_domain_if_set` →
`bluesky::oauth_public_url`) and rebuilt when that domain moves; the ES256
keypair sits beside the database and self-generates on first use, like the VAPID
keypair below. A box with no public identity domain reports the bridge
unavailable with a reason rather than fabricating a client_id
([`../../behavior/bridges.md`](../../behavior/bridges.md) § Implementation status
today owns the behavior).

**`--bluesky-public-url` was REMOVED 2026-09-02** — the same class
as the VAPID key and `--push-relay-url` below. It named this nest's *own* public
URL, which is its identity domain and nobody's choice, so the flag was the banned
operator tier rather than a configuration surface; and being the sole writer of
the OAuth client while no shipped launch line passed it, it left the Bluesky
bridge dark on every real deployment. The unread `[bluesky] enabled` config
section was deleted with it (`NestConfig` sets no `deny_unknown_fields`, so an
existing `nest.toml` carrying the section still parses).

**Web Push**

| Flag | Purpose |
|------|---------|
| `--vapid-pem <PATH>` | Dev-only override: use this PEM for the current boot instead of the nest's self-generated keypair. Never persisted — omit the flag for the ordinary path (auto-generated on first boot, works out of the box). |

Web push needs no flag in the ordinary case: the nest generates a VAPID P-256 keypair on first boot and persists it (singleton `vapid_keypair` DB row), matching the one-configuration-surface invariant — no user or admin ever chooses a VAPID key. Owner: [`../apps/common.md`](../apps/common.md) § Push Notifications → Implementation status today.

**`--push-relay-url` was REMOVED 2026-08-15** — it is the same class as the VAPID key above, and it failed the same test. It named the URL of `bins/fauna-push-relay`, a **first-party Fauna service that is not part of a nest deployment** ([`../api-layers.md`](../api-layers.md) § Push relay), so no user or admin ever chooses it; a flag was the banned operator tier, not a real configuration surface. Its sole consumer was `SecurityNotifier`'s channel 2, which POSTed to a `/v1/notify` route the relay has never implemented — and since no shipped artifact ever passed the flag, that channel never fired at all. The channel was deleted with it. Security notices still reach the user through channel 1 (the guaranteed inbox message plus the `notifications` row the apps render) and channel 3 (the sealed INBOX email). If a mobile push story later needs a nest→relay call, it gets a signed message and a compiled-in first-party URL, never a flag.

### Data Directory Layout

```
{data-dir}/
  nest.db                  ← SQLite database
  blobs/
    00/                    ← first two hex chars of BLAKE3 hash
      aabbcc...            ← remaining hash chars (blob content)
    01/
      ...
  __mail/<actor_hex>/      ← per-kind, per-audience-scope CARv2 segment trees:
  __conv/<channel_hex>/      manifest + immutable seg-NNNNNNNN.{dat,meta} pairs.
  __calendar/<actor_hex>/    Directory is `__<kind>` from the SegmentManager kind
  __card/<actor_hex>/        tag (`SegmentManager::scope_dir`). The authoritative
  __post/<author_hex>/       kind table, the scope-per-kind rule and the file
                             format are owned by ../message-segment-store.md
                             § Layout — do not restate them here.
  segments/
    __mail-placement/<actor_hex>/      ← the placement journals for the three
    __calendar-placement/<actor_hex>/    actor-scoped kinds (manifest +
    __card-placement/<actor_hex>/        segments), deliberately a sibling tree
                                         rather than living under the kind dirs
  claim-code               ← single-use bootstrap code (deleted after first admin claims; a boot reconcile re-deletes any code that pre-db startup regenerated on an already-claimed nest, so setup.status.claimed stays true across restarts — claim::reconcile_claim_code)
  nest_deployment.key      ← deployment signing seed = the nest's SINGLE identity (channel-binding nest_actor_id + nest.info/federation/backup/pairing/sync; PRESERVED across factory reset)
  acme/                    ← on-disk TLS cert (fullchain.pem/privkey.pem + .real-backup) + acme-retry-state.json (persisted retry budget); PRESERVED across factory reset
  serving-port             ← value-flag: admin-chosen client-facing port (desktop supervisor restart-trigger — § Serving ports)
  keys/                    ← bridge service-user keypairs (per-role subdirs; PRESERVED across factory reset — artifact-minted, see installers/docker.md)
  nest.toml                ← rendered config (PRESERVED across factory reset)
  dkim-keys-carried        ← transient: the sealed DKIM keys a factory reset carries from the wipe to the boot that follows (§ Factory reset)
  maintenance/             ← nest→host rw channel (restart-requested flag — owner installers/vps.md § Host OS Maintenance)
  maintenance-host/        ← host→nest :ro status mount (HM-1/HM-2 trust split — owner installers/vps.md § Host OS Maintenance)
  imap-enabled             ← mail-enable gate (presence = on; reconciled from DB; caldav/carddav/webdav sibling flags alongside)
  factory-reset-requested  ← transient: staged by fauna.admin.factory_reset, carries the next claim code; consumed (wipe) at next boot
```

### Compile-Time Feature Flags

Bridge protocol support is opt-in at build time via Cargo features:

| Feature | Protocol / Capability |
|---------|---------|
| `bluesky` | AT Protocol / Bluesky |
| `nostr` | Nostr |
| `activitypub` | ActivityPub / Fediverse |
| `test-hooks` | Test-only hooks for the e2e harness — never enabled in production builds |

(The former `email` feature is deleted — mail is solely the Go bridge + `libs/fauna-mail`; see § CLI Flags → Mail.)

---

## Moderation

### Client-Side

Apps classify content locally, post-decrypt, with shared-Rust scorers — the text heuristic (`fauna_core::text_heuristic`), the per-user Bayesian model fetched sealed over `fauna.bridges.fetch_spam_model`, and any tier-3 labeler artifact the user subscribes to — and results are never reported back to the nest. The nest distributes **no classifier model of its own**: the ONNX model + vocabulary byte downloads (`GET /api/v1/moderation/{model,vocab}`, served from two config-file paths no deployment ever set) are **retired 2026-10-02 and deleted** ([`../content-scoring.md`](../content-scoring.md) § The placement matrix → *Deployment-wide content models at the client position*).

### Server-Side (retired at Phase 4, 2026-07-12)

**The nest no longer runs any classifier on ingested content, on any box.** The three mechanisms this section used to list as running server-side on every ingested post (a text heuristic, a WASM/ONNX classifier, the Bayesian spam scorer) — and the `Reject` / `Quarantine` / `SuppressFromFeeds` obligation-rule enforcement they fed — were the `process_on_ingest` pipeline, deleted with the storage-mode axis (`storage-modes.md` § Implementation status today; § Content Pipeline above). Content scoring now runs only at a capability position — the perimeter bridge, the user's client, or a granted content-processor — per `../content-scoring.md` § The placement matrix, and its output rides the scoring-metadata bus (`../content-scoring.md` § The scoring-metadata bus) rather than a nest-side `content_labels` write + ingest-time obligation gate.

---

*(Implementation status lives in `## Implementation status today` at the top of this doc, per the reading protocol.)*
