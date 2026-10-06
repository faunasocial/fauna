# Message segment store — target state

Owns: segment-store, continuation-records, record-identity
Status: ratified
Authority: the segment-store **mechanism** — segment file shape (CARv2 `.dat` + canonical dag-cbor `.meta` sidecar), the kind-tagged outer manifest, per-scope/per-kind layout, the `segment_records` SQLite mirror, the at-rest vs transport split, the continuation-record shape for bodies larger than one record, the kind → reserved-folder mapping, snapshots / pin-set / compaction / immediate-delete / restore dispatch, the destination-capability four gates, and the kind-agnostic custody-copy gate that a pure-backup destination enforces; defers the cross-location backup protocol built on all of it — writer identity, custody, GC-safety, the grace window and the two custodians — to [`segment-backup-protocol.md`](segment-backup-protocol.md), defers the per-kind at-rest property to [`encryption-at-rest.md`](encryption-at-rest.md), reserved-folder transport to [`../behavior/file-sync.md`](../behavior/file-sync.md), snapshot/GC/restore UX + walk semantics to [`../behavior/backup-restore.md`](../behavior/backup-restore.md) §§ 7/9, the placement journal + MUA-observable restore behavior to the 2026-05-14 imap-caldav-restore design owner, and the backup wire protocol to the 2026-05-15 mail-segment-backup-protocol design. Sources: `libs/fauna-segment-store/`, `libs/fauna-carv2/`, `libs/fauna-mail/src/segments/`, `bins/fauna-nest/src/segments/`.

## Why

Stream content kinds — mail, conversation messages, calendar events, posts — share an at-rest shape: append-dominated writes, single-author-per-record, range-reads-by-actor, occasional point reads. Today each kind grows its own per-row SQLite table (`bridge_inbound_mail`, the channel `content` rows, `bridge_caldav_events`). That denies them all of: cheap point-in-time snapshots, cross-location backup as a falls-out feature, uniformity with the content-index segment shape (`libs/fauna-index/`).

The segment store is the shared at-rest substrate: append-only per-actor-per-kind segments + a tiny mutable manifest, mirroring the index design. Each kind's records are opaque to the segment-store layer — sealed by the kind's existing authority before append, uniformly on every nest (the deployment-wide plaintext/encrypted mode axis this section once branched on is retired — `nest/storage-modes.md`; `encryption-at-rest.md` § Readable classes owns what a nest may additionally read via capability grants). The only at-rest exception is pre-Phase-3 legacy residue a formerly-moded box's boot backfill has not yet converged.

## Layout

One reserved folder per kind. Every device implicitly a member, created at first use, not listed by the folders list surface. Mirrors `__drafts` / `__index` (and `__config`, retired 2026-10-02 — the name stays reserved; [`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution schedule → *The closure order*, step (6)).

```
__mail/<actor_id_hex>/
├── manifest.mail              tiny typed file (~KB), only mutable file
├── seg-00000001.dat           immutable standard CARv2 segment file
├── seg-00000001.meta          immutable dag-cbor sidecar (per-record floor + segment header)
├── seg-00000002.dat
├── seg-00000002.meta
├── ...
```

**The authoritative kind table.** The on-disk directory is `__<kind>` derived from the `SegmentManager`'s kind tag (`SegmentManager::scope_dir` = `<data_dir>/__<kind>/<scope_hex>/`, `libs/fauna-segment-store/src/manager.rs`):

| Kind tag | Directory | Scope (`scope_id`) | Status |
|---|---|---|---|
| `"mail"` | `__mail/<actor_hex>/` | recipient actor | live (Plan 2) |
| `"conv"` | `__conv/<channel_hex>/` | MLS channel (audience-scope is the group, not any one actor) | live (Plan 7) |
| `"calendar"` | `__calendar/<actor_hex>/` | owner actor | live (S6.6 + S6.7 back-fill, 2026-07-09; compaction S6.8b/c, snapshot S6.9, backup four-gates S6.10, all 2026-07-10) |
| `"card"` | `__card/<actor_hex>/` | owner actor | live (calendar's S6.6/S6.7 twin, 2026-07-09; same S6.8b/c/S6.9/S6.10 arms, 2026-07-10) |
| `"post"` | `__post/<author_hex>/` | author | live (Track C, 2026-06-15/16) |

There is no `"cal"` kind and never was — the calendar kind tag is `"calendar"`. Every kind enumeration in this doc and its consumers points here rather than restating the list.

**Per-(audience-scope), per-kind.** Encryption-at-rest's per-user isolation rule (`encryption-at-rest.md` § Architectural rules: "Don't index across user boundaries.") and per-kind seal differences (mail under recipient's MLS read key; channel messages under MLS group epoch key; posts under `derive_post_key`) mean records of different audience-scopes and different kinds never share a segment. The **audience-scope is the segment-store scope_id**: `actor_id` for mail / cal / post (per-recipient / per-owner), `channel_id` (the MLS group) for conv — a channel's records are shared by the group's members, so the natural isolation boundary is the group, not any one actor (spec D1's audience-scope generalization).

**Filenames are `seg-{segment_id:08}.{dat,meta}`.** `segment_id` is `u32` (4B segments per actor-kind is sufficient at one segment per month for ≫ 100M years). The dag-cbor sidecar carries the calendar bucket; the filename does not — bucket-keyed names would collide on cap-size split.

## Segment file format

Each segment is **two files** that share a stem:

1. **`seg-NNNNNNNN.dat`** — a standard **CARv2** container (`libs/fauna-carv2` writes it; `go-car/v2`, `kubo dag import`, and any other CARv2-capable tool reads it without Fauna-specific code):

   ```
   [CARv2 pragma — 11 bytes, fixed]
   [CARv2 header — 40 bytes: characteristics, data_offset = 51, data_size, index_offset]
   [CARv1 data section, starting at byte 51:]
     [CARv1 file header: varint(len) || dag_cbor({"version": 1, "roots": [<placeholder/finalized>]})]
     [varint(cid_len + block_len) || record_cid || block_bytes]   -- record 1
     [varint(cid_len + block_len) || record_cid || block_bytes]   -- record 2
     ...
   [CARv2 MultihashIndexSorted index, at index_offset, written at finalize]
   ```

   - Block CID is `fauna_cbor::Cid::of_dag_cbor(block_bytes)` — codec `0x71` dag-cbor, multihash `0x1e` BLAKE3-256, 32-byte digest, total 36 bytes (per `docs/goal/architecture/serialization.md` § CID shape). Identity-IS-the-content-hash is the ratified end state for **every** kind (§ *Record identity per kind* below), and since the 2026-08-17 cutover legs **every kind complies**: `post` from birth, `mail`, `calendar`, `card` and `conv` by cutover.
   - Block bytes are the kind's existing-seal output, **opaque to the segment-store layer**.
   - Active (open, not-yet-finalized) segments have `index_offset = 0` and the CARv1 `roots` slot zero-filled; readers scan blocks linearly from byte 51. After `finalize()`, the `MultihashIndexSorted` index is appended, `index_offset` is patched in place in the v2 header, and `roots` is rewritten with the final segment-root CID. **Same byte-position-stable in-place rewrite pattern the pre-Layer 3 BARE format used**, with a CID slot replacing the integer slot.

2. **`seg-NNNNNNNN.meta`** — a **canonical dag-cbor sidecar** holding the segment metadata that doesn't fit in standard CARv2's header (which only stores roots, version, characteristics, and the digest→offset index). Schema:

   ```
   SegmentSidecar {
     format_version: u16,                  // sidecar schema version (writer's format)
     min_reader_format_version: u16,       // oldest format_version that can read this; #[serde(default)] = 1
     kind: String,                         // a § Layout kind tag ("mail" | "conv" | "calendar" | "card" | "post")
     actor_id: [u8; 32],
     segment_id: u32,
     bucket: String,                       // "YYYY-MM"
     created_at_secs: u64,
     record_order: [Cid, ...],             // append order (CARv2's index sorts by digest, not append order)
     floor_metadata: [<bytes>, ...]        // parallel to record_order; opaque kind-specific bytes
   }
   ```

   Loaded once at `open()`; rewritten at `finalize()`. Single-digit KB per segment in typical use. **At-rest version tolerance.** `format_version` + `min_reader_format_version` are the same two-number scheme as the nest DB (`version-compatibility.md` § 2.2): `open()` reads any sidecar whose `min_reader_format_version ≤` this binary's writer version (so a newer-*additive* sidecar is tolerated — I2 backward-compat) and errors only on a newer-*breaking* one, never orphaning user segment data (I1). The same pair lives on the kind-level `Manifest`. Both are baseline `1`; the field is the "tolerate before any bump" enablement. Do **not** reject-and-recreate on a version bump — evolve additively, or migrate at a major version.

   **Why a sidecar (not in-CAR).** The Layer 3 plan task evaluated three in-CAR placements (synthetic-CID metadata blocks, on-disk-CID ≠ consumer-visible-CID envelopes, per-record entries in the outer manifest) and all three lose: A leaks Fauna-specific block-naming convention into the `.dat` (a third-party CARv2 tool sees synthetic blocks with no provenance); B breaks the carv2-CID = consumer record-id invariant the index-lookup path depends on; C balloons `KindManifest`'s in-memory size by an order of magnitude. The sidecar keeps the `.dat` **byte-identical to what `go-car` / `iroh-car` / `kubo dag import` would produce** for the same records — verified by the cross-language byte-parity test (Layer 3 Task 3.6 — Go go-car/v2 oracle on Rust-produced `.dat`s). Rationale lives in `libs/fauna-segment-store/src/segment.rs`'s top-level docstring.

**Segment size — there is no legal ceiling (established 2026-09-29).** A segment finalizes when its scope's append crosses into a new month bucket (`bucket_for`, `"YYYY-MM"`), on a read that flushes the open segment, and when compaction rewrites a bucket; nothing rolls it on size. A single record is bounded (`fauna_carv2::v1::MAX_RECORD_LEN`, 16 MiB — larger bodies are § Continuation records), but a segment's record count is not, so a `.dat` grows with one scope's volume in one month, and the `.meta` sidecar grows with the same record count (`record_order` + `floor_metadata`). "Single-digit KB" above is the typical case, never a bound. **So a reader that must bound a segment download bounds it by something other than a constant** (user ruling 2026-09-29, over the alternative of a size-rolling writer): the size the source's own listing declared for the `.dat` (`SegmentRef.size_bytes` — a body longer than declared is refused mid-read), and, for a custodian, its remaining byte budget (`account-data-plane.md` § Replica posture → *Custody policy (T15)*). The bounded read is `fauna_nest_http::capped::CappedBody`, which refuses while streaming, never after buffering. **A transfer that keeps a segment is file-backed (2026-09-29):** the download bound limits what reaches *disk*, and a transfer's *memory* is a constant chunk (`fauna_account_store::segments::SEGMENT_TRANSFER_CHUNK` plus one transport chunk), never the budget or the declared size. Each half streams, a chunk at a time, into a staging slot in the adopting store's own segment area (`StoreBackend::segment_stage`, files named `%staging-…`, which no adopted segment's name can start with), is admitted by reading it back one record at a time (`segments::admit` over `Read + Seek`; the `.meta` is decoded whole, since it is the admission's own index), and is adopted by rename, files before rows. A slot dropped un-adopted takes its bytes with it; one a crash leaves behind is an unrouted file that the store's next open sweeps. On the native arm each staging file stays under an exclusive advisory lock while in use, so one process's sweep never deletes a live sibling's transfer. Every adoption path takes this one shape: custody adoption in both forms, a fresh device's bootstrap, and `adopt_segment` over bytes already in memory. **The owner's backup pass keeps a pair the same way on the custodian side:** each half streams into a directory under the custodian store's `staging/` (inside its root, so under the root's cloud-backup exclusion, and outside the byte planes the reclaim sweep and footprint read), hashed as it arrives for the in-transit check against the listing; each half is then sealed off its file a chunk at a time (`fauna_core::blob_seal::seal_reader`, which `seal_blob` wraps, so the artifacts stay byte-identical to a nest destination's), its sealed bodies staged beside it until the pair's cap charge admits the pair as one unit. There the memory constant is one sealed chunk (`MAX_CHUNK`, the content-addressed format's own unit), not a transfer chunk. A staged pair is removed when the pass is done with it; one a crash leaves behind is swept by a later pass once it is a day old.

**Read-by-CID is O(log n)** via the `MultihashIndexSorted` index in the `.dat`. **Read-by-append-order** (compaction, restore rebuild) walks `record_order` from the sidecar. **Per-record tombstone state is the caller's `segment_records` SQLite mirror** — both CARv2 index and sidecar are immutable post-finalize.

**Records are opaque to the segment-store layer.** They are whatever the kind's storage produces — a sealed mail body / sealed conversation envelope, uniformly on every nest (the plaintext/encrypted deployment-mode axis this used to branch on is retired — `nest/storage-modes.md`; `encryption-at-rest.md` § Readable classes owns what a nest may read via capability grants). The only unsealed exception is pre-Phase-3 legacy residue a formerly-moded box's boot backfill has not yet converged. The segment store frames; it never opens, seals, or unseals a record.

### Record identity per kind — content addressing is the end state (RULED 2026-08-17, user-ratified)

**The rule.** A record's filing CID — its identity in the CARv2 container, the sidecar's `record_order`, the `segment_records` mirror, placement journals, and the wire fetch — is `Cid::of_dag_cbor(block_bytes)`: **identity IS the content hash of the stored bytes, for every kind.** This is what makes `fauna_account_store::segments::admit`'s anti-poisoning predicate — re-hash every block against the CID it is filed under — the **single, kind-agnostic, one-line admission check** at every boundary that adopts bulk containers from a source it must not trust, custody adoption above all.

**Where code stands, and the cutover.** `post` complies from birth (`Cid::of_dag_cbor(body)`). **`mail` complies since its 2026-08-17 cutover leg**: `segments::mail::append_record` encodes the envelope once and files under `Cid::of_dag_cbor(env_bytes)` (`fauna_mail::segments::ops::encode_record` is the single mint), the relay destination re-derives the same identity from the verbatim bytes, the pre-append dedup is `(scope_id, kind, record_cid)`-scoped inside the seq lock, and the retired intake digests (`derive_message_id` + both domain tags) are deleted. **`calendar` and `card` comply since their joint 2026-08-17 cutover leg**: `segments::{cal,card}::append_record` file under `Cid::of_dag_cbor(env_bytes)` (the shared `{Cal,Card}RecordEnvelope::encode_record` mint), the identity is no longer re-derivable from the `event_id`/`card_id` PK so it is **stored** on the `bridge_caldav_events` / `bridge_carddav_cards` row (`record_cid`) and every read/reap path resolves through that column *fail-closed*, and the S4 seal-backfill module retires with them (its corpus is deleted by the reset, so `content_seal_backfill.rs`, its boot calls, its marker table and its e2e harness are gone). **`conv` complies since its 2026-08-17 cutover leg** — the last kind to converge: `segments::conv::append` files under `Cid::of_dag_cbor(env_bytes)` (`fauna_mls::segments::encode_record` is the single mint, shared with the client), the retired `derive_record_id(channel_id, seq, body)` is deleted, `seq` stays the cross-member identity (unchanged: dedup, ordering, read watermarks, `target_seq` and every history-fetch wire shape are seq-keyed), and the pre-append dedup is `(scope_id, kind, record_cid)`-scoped inside the per-channel seq lock — which also makes a retried `channel.send` of the identical envelope **idempotent** (it returns the first record's `seq` instead of duplicating the message at a second one). The client half of the identity — `fauna_conversations::plane::plane_ref`, which stamps the plane coordinates an app reports T1 observations against — derives through that same shared mint from the envelope bytes it already holds, so client and nest agree by construction rather than by agreement. Every kind converges via a **hard cutover under the alpha deletion carve-out (user-approved 2026-08-17, superseding the expand→migrate→contract path ruled earlier the same day)**: at alpha scale the migration is not worth building — at the cutover boot the nest **deletes** each non-compliant kind's at-rest records instead of re-filing them (per kind, the `reconcile_record_identity_cutover` boot pass — a tombstone pass that preserved seq slots and re-assigned `changed_seq`, so client/peer cursors survived and replicas observed the deletion, segment-file bytes reclaiming via ordinary compaction; retired with its marker by the 2026-09-24 genesis of the nest schema, no pre-cutover record resting anywhere). What dies, where, what users redo (the carve-out's required naming): mail, conversation, calendar and card records plus their kind-scoped aux rows (IMAP placement maps, content scores, scan results, spam-training history) on every pre-cutover nest; `post` survives; users re-sync mail from its copied sources, re-upload calendar/card from their DAV clients, and conversation history is lost. The carve-out is at-rest only — client-only key material and client↔nest↔nest wire compatibility are untouched (`version-compatibility.md`). Build status: tracked internally.

**Why (the marathon ruling).** (a) *Security* — custody admission is a boundary between actors, at scale between organizations; the predicate inside it must be the strongest available and the simplest to audit, with no per-kind parser in the trusted path. (b) *Performance* — per-record cost is one blake3 either way; content addressing dedups replayed blobs and keeps custody, backup, sync, and restore joins kind-agnostic instead of branching per identity scheme forever. (c) *Uniformity* — one identity rule for every kind: `post` is the richest existing pattern, the other four are drift (priorities #3/#4).

**Rejected: set-equality admission.** Verifying only sidecar↔index consistency without re-hashing bodies would let a dishonest source nest file arbitrary bytes under fabricated ids — poisonable custody for the bulkiest plane. Never, for any kind.

**Sanctioned as bridge only: per-kind re-derivation.** Re-computing mail's tagged digest from the block's own bytes plus floor fields inside a kind-aware admission arm is an equal-strength commitment (blake3 binds the bytes either way) and covers the entire existing corpus with zero at-rest churn. Permitted **only if a pre-check below blocks the migration**, and it retires when the migration lands — as an end state it puts per-kind parsers inside the trust boundary and keeps five identity schemes alive forever.

**Pre-checks — RUN 2026-08-17 (first pass; verdicts refutable, each disposition recorded here so the migration's builders inherit decisions, not questions):**

1. **Record-block immutability — CLEARS with one sequenced retirement.** The census
   found the framing layer structurally append-only (`FramedSegment` finalizes
   write-once; the duplicate-CID guard; compaction carries bytes + CIDs verbatim,
   byte-parity test-pinned; tombstoning and takedown flags are mirror-row-only;
   the relay appends the source's sealed envelope verbatim), and
   `encrypted_index_hint` is written ONCE at record creation — content-index key
   rotation is grace-key carriage, never a re-seal, and the nest-side index
   pubkey has no rotation walk at all. **The one violating path is the S4 boot
   backfill's `segments::mail::reseal_record_in_place`** (new-segment append
   under the OLD cid + mirror swap — deliberate, its own docstring says so), whose
   corpus is frozen (gated on the write-once legacy `nest_mode` row; a fresh nest
   skips in O(1)) and which cannot touch a content-addressed record already
   (continuation parts are skipped). **Disposition, ruled: sequence, then retire.**
   Per box, the identity migration for a kind runs only AFTER the S4 backfill
   reports complete for that kind's store (boot-reconcile ordering) — by then
   every record it could ever reseal is sealed, so the path's corpus is empty
   post-migration and it retires at the contract step. No re-key cascade, no
   grandfathering.
2. **The ingest-vs-append non-collision property — CLEARS; the tags drop with the
   migration.** No justification beyond restatement exists in code, tests, plan
   docs, or the introducing commit; mail seals are HPKE-ephemeral-fresh (identical
   plaintext never yields identical ciphertext — a content-hash collision is a
   literal byte replay, which the pre-append dedup already absorbs); and the
   codebase already ratifies exactly this collision UNTAGGED between submit and
   ingest ("first writer wins", test-pinned). **One binding migration obligation
   surfaced in its place:** the pre-append dedup
   (`records_db::lookup_actor_for_record`) is scope-AGNOSTIC — harmless today
   only because `actor_id` is inside the hash. Content addressing removes it, so
   the migration MUST scope the dedup to `(scope_id, kind, record_cid)` in the
   same change (the scoped query already exists as `lookup_record`), or a byte
   replay across actors silently drops the second actor's record.
3. **Family convergence, conv included — CLEARS, with the premise corrected and
   three binding constraints.** The rid is minted NEST-side inside the per-channel
   seq lock (`conv.rs`: `derive_record_id(channel_id, seq, body)` — the MLS
   engine never touches it), and the genuinely cross-member identity is **`seq`**,
   which the migration leaves untouched: dedup, ordering, read watermarks,
   reactions and deletes (`target_seq`), and every history-fetch wire shape are
   seq-keyed; MLS-sealed bodies carry NO rid. The rid reaches other machines only
   as feed/peer/custody *coordinates* (metadata — `put_block`'s re-hash +
   `ADOPTABLE_KINDS` mean no replica holds conv bytes under the old cid) and as
   one optional field inside the client-sealed history slice
   (`plane_ref.record_digest`), whose only consumer fails soft to a dropped
   observation. Constraints the conv leg builds under: **(i) fresh coordinates,
   never an in-place rekey** — the account store's equivocation refusal makes a
   changed cid at a held `(scope, writer, seq)` an error that breaks every
   member's walk, so migration emits tombstone(old)+add(new) pairs; **(ii)** the
   one-time T1 re-observation churn that causes (conversation READ state rides
   seq watermarks and is untouched) is **accepted, stated** — re-evaluate the
   watermark-bump mitigation at the conv leg using the earlier legs' measured
   churn; **(iii)** the moderation door (`content_id` = the rid, the one place a
   client hands the nest a record id) gets a migration-window old-digest→new-cid
   alias, and pre-migration pinned snapshots' `restore_conv` must not re-file
   old-shape cids into a migrated mirror. conv sequences LAST — not because it is
   hardest at rest, but because its healing convergence depends on other
   machines coming online. ⚠ The new pre-image is the **envelope bytes**
   (`Cid::of_dag_cbor(serialize_envelope(...))`), not the sealed payload the old
   digest covered — the client-side `plane_ref` derivation is a real model-crate
   change, not a constant swap.

**Transition superseded — the alpha reset (user-approved 2026-08-17).** The cutover replaces the migration the dispositions above were written for, and they partially collapse: **(1)** the S4 reseal path's sequencing question is moot — its corpus is deleted at the cutover, so `segments::mail::reseal_record_in_place` retires **in the same change**, no sequencing, no grandfathering; **(2)'s obligation STANDS in full** — the pre-append dedup must scope to `(scope_id, kind, record_cid)` in the same change as the identity switch; it is a property of the new identity, not of the transition; **(3)**'s constraint (i) (fresh coordinates, never in-place rekey) and (iii)'s moderation-door alias are moot — no old cid survives the reset, so there is no dual-identity window — and (ii)'s re-observation churn does not occur (the plane is fresh). Two conv items STOOD, and **both are BUILT in the conv leg (2026-08-17)**: the ⚠ envelope-bytes pre-image note (the client-side `plane_ref` derivation is part of the identity rule — an old client deriving the old digest for a *new* record hits the documented fail-soft consumer and drops the observation, accepted within the major; `plane_ref` now calls the same shared mint the nest files under, and its `seq` parameter is gone with the retired derivation), and (iii)'s `restore_conv` guard question in reduced form — **verified reachable and guarded**: every restore-from-disk mirror rebuild (conv, mail, and the shared `walk_pinned_segments` cal/card walk) re-hashes each pinned record's block against the CID it is filed under and refuses the whole restore on a mismatch, because the CARv2 reader verifies only the *framed* CID at the indexed offset, never `blake3(block)` — so without the guard a pre-cutover pinned snapshot would silently re-file old-shape, permanently un-admittable records into a store the reset had already cleared. Fail-closed: the restore transaction rolls back and the (already reset) live state stands.

**`ADOPTABLE_KINDS` corollary.** A kind enters `fauna_account_store::segments::ADOPTABLE_KINDS` exactly when its at-rest identity is the content hash — since 2026-08-17 that is **every** kind: `post`, `mail`, `calendar`, `card` and `conv` (the reset means no store running the code holds any kind under an upstream digest). Never widen it ahead of identity: offering an upstream-digest segment to `admit` buys a guaranteed verification failure. **Adoptable ≠ reachable** — the list answers "may these bytes be admitted", not "does a nest offer them", and the two questions have had different answers all along, though as of 2026-08-18 they coincide for every kind: `segments::list_handler::segment_manager_for_kind` serves all five adoptable kinds, `conv` last — under the member-mint rule (§ *Which kinds the two planes serve*, below) rather than the `{ of_owner }` shape the other four use, since conv's segment scope is a **channel**, not an actor.

## At-rest vs transport — two storage tiers

The load-bearing distinction. Different kinds get different at-rest shapes based on whether the nest must read them.

1. **At rest on a nest, segments are plaintext-framed local files** in `<data_dir>/__<kind>/<scope_hex>/seg-<id>.{dat,meta}` (the § Layout `scope_dir` — there is no `segments/` parent directory). "Plaintext-framed" means the CARv2 framing (`.dat`'s pragma, header, CARv1 framing, MultihashIndexSorted index) and the dag-cbor sidecar (`.meta` — kind, actor_id, bucket, created_at, segment_id, per-record floor metadata) are plaintext (these are exactly the floor items `encryption-at-rest.md § Plaintext floor` already commits to nest-readable in both modes). **The record payload byte shape inside each CARv2 block is sealed in both modes** (Phase 3, built 2026-07-08 — `encryption-at-rest.md` § Readable classes is the owner): the payload stays sealed by the kind's inner seal, so the nest reads framing / index / sidecar but never the record bodies; no raw payload rests, and a reader refuses one. The segment-store *framing* layer is unaffected either way — no new segment-store key material is involved.

2. **At transport time (and at a pure-backup destination), segments ride the fauna-sync chunk pipeline.** When `__mail/<actor>/` is replicated to another location, the source nest reads the local plaintext-framed segment file, hands it to fauna-sync, and standard chunking + zstd + ChaCha20-Poly1305 under the data-owner's `BackupKey` applies. Destination behavior depends on its capability (active-replica → reconstitute local segments; pure-backup → keep encrypted chunks).

This is the **only** way to satisfy both invariants simultaneously: "every nest reads the floor" *and* "no nest ever holds `BackupKey`" (the plaintext/encrypted deployment-mode axis these invariants used to be phrased against is retired — `nest/storage-modes.md`; every nest is now uniformly what used to be the encrypted-mode shape at rest). It is a deliberate divergence from `__drafts` (chunk-encrypted at rest because the nest never reads them).

## Continuation records — a body larger than one record (ratified 2026-07-12)

Owner of the over-cap-body mechanism. A CARv2 record over `fauna_carv2::v1::MAX_RECORD_LEN`
(16 MiB) is refused on read — a garbage-parse guard, deliberately kept, that is **not** a
product ceiling on content size. A sealed payload larger than the per-record cap is stored as
**continuation records**: N *part* records plus one *head* record, all ordinary records in the
**same** kind store (same segment files, same mirror, same manifests). Mail is the first
adopter (`behavior/mail-message-size.md` § Message size limits owns mail's constants and enforcement
points); the mechanism is kind-agnostic and any kind that outgrows one record adopts this
shape rather than inventing another.

- **Parts are ciphertext ranges of the one sealed payload.** The producer seals **once**,
  exactly as today (the kind's inner seal — seal derivation, key material, and quota charging
  are unchanged); the storer splits the sealed bytes into consecutive ranges of at most the
  kind's part cap and appends each range as its own record. Reassembly is concatenation in
  head-declared order. Integrity is free: records are content-addressed (`record_cid` pins the
  part bytes; the CARv2 index verifies on read) and the head pins the ordered part-CID list
  plus the total sealed length.
- **The head is untrusted input; the join bounds it before and during the read (ratified 2026-09-28).** A relay stores a peer's head verbatim, so the serve-join refuses a head declaring more than its part list could hold (`parts × MAX_RECORD_LEN`), a head naming any part more than once (an honest writer never does: each part is a distinct range of one ciphertext), and — mid-read — the part that carries the join past `total_body_len`; it never preallocates the declared total. A count bound derived from the part cap is deliberately not used: an older peer's smaller parts would fail it.
- **The head is the commit point** (invariant 1 below, extended): parts append first, the head
  appends last, and only the head receives a placement / projection row. A crash between parts
  and head leaves unreachable part records — benign, and reclaimed by an **age-watermarked
  headless-part reaper** (the calendar/card orphan-reaper pattern: single held lock, age
  watermark, fail-closed on an empty mirror) — but only once a **head at a higher `seq`** proves
  the run truly orphaned (next bullet).
- **A headless part is reaped only when a live head above it proves its own head was skipped —
  age alone is not enough.** At a relay destination a family always spans pull pages (parts are
  1 MiB, the page budget is one 2 MiB frame minus headroom), so the destination routinely holds a
  headless part-run whose head is simply in a later page, and the relay acks *mid-family* (the ack
  is the highest contiguous stored seq, which between pages is the last part), so the source has
  already purged the parts it handed over. Reaping such a run on age alone — the moment a stall
  outlasts the grace and a compaction pass lands in the gap — destroys the only copy of the body:
  the head arrives later, its join finds no parts, and the re-pull cannot heal because the source
  purged (the No-user-data-loss invariant, no attacker needed). The discriminator is structural:
  parts and their head take consecutive seqs under one lock hold and the relay never skips a
  record, so a live head at a higher `seq` for the actor proves the run's own head was never
  allocated a seq — a genuinely crashed family write — while a stall leaves *no* head above the
  run. (A *normal* record above the run proves nothing: relayed records take no family-spanning
  lock, so a local append can interleave.) The fix lives in the reaper, not the ack: at a mail
  relay the ack value *is* the destination's next pull cursor, so holding the ack to protect a
  family would freeze the cursor and livelock the relay. The fail direction stays a leak, never a
  loss — a crash-orphaned run is retained until the actor's next over-cap message writes a head
  above it. **⚠ The head-above proof is provenance-blind and rests on a topology invariant
  (2026-07-18): no nest both relay-pulls an actor's mail and locally ingests
  mail for that same actor.** A relayed run's head is allocated on the *source*, so a
  locally-written family's head landing above a relayed awaiting run would falsely license the
  reap. The invariant holds today because the only production family writer is MTA ingest and
  the paired deployment keeps the MTA on the public box; any shape that breaks it (a migration
  flow reusing `mail_pull` beside a live MTA, a home-box MTA) must revisit the reaper first —
  structural fix on the next schema touch: an additive `relayed` provenance flag on mirror rows
  with per-provenance orphan proofs (full menu in the `reap_headless_parts` safety comment).
- **The reaper's watermark ages on `stored_at` — when *this* nest stored the record — never on
  `received_at`.** The distinction is load-bearing at a relay destination, where the two diverge:
  `received_at` is the *message's* receive time and is forwarded verbatim (it drives INTERNALDATE
  and the co-location bucket, so it must survive the relay — what the origin nest stamps into it, and
  why it is never the sender's `Date:` header, is
  [`../behavior/imap-server.md`](../behavior/imap-server.md) § SEARCH's to say), while a **backfill/catch-up relay of
  historical mail** delivers a part whose `received_at` is days old *on arrival*. Aging on that
  would make a just-arrived part instantly past the grace, so a compaction pass landing between a
  family's parts and its head tombstones parts that are still needed — and the source purges on
  ack, so the body is unrecoverable (the No-user-data-loss invariant). `stored_at` is therefore
  assigned by the storing nest at append and **overwritten on the relay path**, exactly as `seq`
  is, and is the floor's only other purely-local fact; it rides the footer (authoritative) and
  mirrors into `segment_records.stored_at`, carried verbatim through compaction so a rewrite
  neither restarts nor drops the grace. Unknown (`0` on a pre-field floor → `NULL` in the mirror)
  is **not** reapable: the reaper errs toward leaking a part, never toward losing one. This is
  the mail form of the principle the calendar/card reaper gets from server-assigned `created_at`
  — with the twist that mail's `received_at` *cannot* be local, which is why mail needs the
  second timestamp at all.
- **Parts are invisible to every content surface.** They carry a part-marked floor and get no
  placement or projection row, so IMAP `SELECT`, the client feeds, and per-kind projections
  never list them; only the head's serve path reads them (head → part CIDs → concatenate).
- **Delete fans out.** Tombstoning a head tombstones its parts in the same transaction (the
  head's part list is readable at tombstone time — invariant 4's delete-then-tombstone applies
  to the whole family). The relay's ack-purge covers parts by seq ordering (parts append
  before the head, so `tombstone_up_to_seq` through the head's seq reclaims the family).
- **The head is a new major record-format version, never an additive field** (the frozen-v1 /
  version-dispatch discipline, e.g. mail's envelope dispatcher): a pre-continuation reader
  meeting a head fails with an honest per-record decode error. An additive `parts` field on
  the existing shape would instead decode as an **empty body and silently serve it** — the
  forbidden failure mode. Enabling continuation *writes* is therefore gated exactly like any
  at-rest format flip: every deployed reader (nest rollback window, peer nests, installed
  clients where the envelope is wire) must carry the continuation-aware decoder first
  (`version-compatibility.md` § 2.1). One accepted rollback residual, stated rather than
  hidden: a reader that predates the *floor* part-marker may create a placement row for a
  part (a phantom entry whose fetch then errors honestly); the write gate above is what keeps
  that window empty in practice.
- **Nothing unsealed enters a segment** (invariant 3) **holds with one part-aware arm**: a
  part's payload is not itself a decodable sealed envelope (it is a range of one), so the
  typed append gate admits parts only through the continuation writer, which mints them from
  an already-verified `SealedRecordBytes` — the seal check happens once, on the whole payload,
  at the same wire edge as today.

**Why the five lifecycle rails need zero new machinery** — the load-bearing property, and the
reason this shape was chosen. Parts and heads are ordinary records in ordinary segments, so:
**GC** — the blob-store GC is untouched (bulk-plane staging chunks remain disposable orphans
it reaps after grace; nothing at rest lives in the blob store); **backup** — the `__<kind>`
chunk pipeline ships segment files, and parts are inside them; **nest↔nest relay** — parts
forward as ordinary records, each small enough for the 2 MiB federation frame
(`architecture/nest/deployment-home-with-public-relay.md` § Relay frame budget owns the
page-budget rule); **restore** — reconstituted segment files contain the parts; **compaction**
— records copy verbatim, and the fan-out rule above keeps family liveness consistent. Snapshot
pins hold parts and head together because they live in the same segments.

**Rejected alternative — byte-plane-resident bodies** (the record holds a chunk-hash
reference into the blob store; the sealed bytes stay where bulk-plane staging put them).
Rejected 2026-07-12 after pricing it against the same five rails: it splits a kind's at-rest
custody across two stores permanently — the blob GC must learn record-rooted liveness with
snapshot-aware chunk lifetimes; the backup pipeline must grow a second rail for chunks that
ride no segment file; the relay must grow a chunk-transfer leg (its purge-on-ack would
otherwise orphan the source's chunks while the destination holds a dangling reference —
irrecoverable loss of the body); restore mirrors the backup hole; and every future lifecycle
rail pays the two-homes tax forever. Its benefits (no full-body segment write at ingest, no
re-staging on oversized fetches) are bounded and local. Do not reopen without new evidence —
the staging blob store is a **transport** surface; the segment store is the **only** at-rest
home for message-kind content.

**Implementation status (2026-07-18): built and LIVE.** The mechanism is fully implemented
for mail and continuation *writes* are ON — the deployed-reader gate cleared 2026-07-18
(below). Landed:
- **Reader expand (ships to every reader).** The v3 head decoder + part-marked floor:
  `fauna_mail::segments::MailRecord` / `MailContinuationHead` (v3 = `part_cids` + `total_body_len`
  + inline hint; `MailRecordEnvelope::decode` errors honestly on a head — never a silent empty
  body) and `MailFloorMetadata.continuation_role` (0/1/2, additive `#[serde(default)]`), mirrored
  into `segment_records.continuation_role`.
- **Writer.** `segments::mail::append_continuation_record` splits an over-cap **already-verified**
  `SealedRecordBytes` into ≤ `fauna_mail::transport_limits::MAIL_BODY_PART_CAP_BYTES` (1 MiB) parts
  at the `append_record` choke point (invariant 3 holds — parts minted below the typed gate from
  one whole-payload seal check), parts first (consecutive seqs) then the head last (commit point).
- **Serve-join.** `segments::mail::read_sealed_body_with_floor` rejoins the parts (fail-closed on a
  missing part or length mismatch), consumed by `fetch_message_ciphertext` (IMAP, inline-or-`body_ref`)
  and the client feed (`mailbox_fetch`, materialize-then-inline-or-`body_ref`; the reference serve
  is built and unconditional — its deployed-reader gate flipped with this file's continuation gate and left with the 2026-09-24 compat-remnant sweep —
  `behavior/mail-message-size.md` § Message size limits owns the leg); the index feed reads a head's inline
  hint.
- **Reaper.** `segments::mail::reap_headless_parts` (age-watermarked on `stored_at`, ms) runs
  as the compaction worker's pre-pass under the `"gc"` lock — the calendar/card orphan-reaper pattern.
  The `stored_at` basis + the additive `MailFloorMetadata.stored_at` / `segment_records.stored_at`
  (schema v26) landed 2026-07-17 as a pre-flip fix: keying the grace on the relay-forwarded
  `received_at` could tombstone a relayed family's parts before its head arrived. A **second
  pre-flip fix** (2026-07-17) added the head-above guard: the reaper's candidate SQL now also
  requires a live head at a higher `seq` for the actor (an `EXISTS` correlated subquery), so a
  headless part-run still awaiting its head across a relay stall is spared — closing the
  mid-family-ack loss the `stored_at` fix did not reach (§ Continuation records, the head-above
  bullet).
- **Relay + fan-out.** The relay skips a part's IMAP placement but stores it (`nest_sync_worker`); the
  ack-purge `tombstone_up_to_seq` reclaims the family by seq (parts precede the head); compaction
  carries the role verbatim. Delete fan-out is free — mail has no per-record content-delete path.

**Gate — CLEARED 2026-07-18, and removed 2026-09-30 under the compat-remnant sweep:** `append_record` (`segments::mail`)
always splits an over-cap body, so production nests emit v3 heads. Flipped **together with the v2 envelope write format and the
client-feed reference gate** — one deployed-reader confirmation cleared all three
(`behavior/mail-message-size.md` § Message size limits owns the go/no-go and records the evidence).
Consequence: the interim per-record write guard (`append_record` refusing a body over the
single-record ceiling) is now an internal invariant only — a body over `MAIL_BODY_PART_CAP_BYTES`
splits before ever reaching it, so it can no longer act as a message ceiling.

**Known gap (now observable):** IMAP `RFC822.SIZE` reports a head's small block size, not the joined
body size, for a continuation message — advisory, and live as of the flip rather than hypothetical;
no fix is scheduled. See `behavior/mail-message-size.md` § Message size limits for mail's sequencing;
its ceiling retirement landed the same day (2026-07-18) as this gate, not a pending "next slice".

## `segment_records` SQLite mirror

The nest mirrors the per-record metadata the segment carries (CARv2 `.dat` index + dag-cbor `.meta` sidecar), so hot paths (mail routing on inbound, IMAP `SELECT` on the MDA bridge, calendar conflict detection) get O(1) lookup without re-parsing segments. The mirror is rebuildable from segment files during recovery.

One sidecar table rides beside it for the `conv` kind and is **not** rebuildable from segment files: `conv_attachment_refs(channel_id, seq, blob_hash)`, the sender-listed plaintext content addresses of the sealed attachment blobs a record's sealed body names, written in the same transaction as the record's mirror row and read by the blob GC, the legal-takedown blob-serve withhold, and a community room's attachment facet (which reads one record's refs to bound which of the addresses its sealed body names the nest will open) — owned by [`../behavior/conversations-at-rest.md`](../behavior/conversations-at-rest.md) § Encryption at rest → *Attachment reachability* (the floor ruling) and [`../behavior/backup-restore.md`](../behavior/backup-restore.md) § 9 step 2g (the walk). Liveness for that walk is this mirror's `tombstoned` flag at the reference's `(scope_id, kind='conv', seq)`; the reference rows themselves are never deleted.

```sql
CREATE TABLE segment_records (
    scope_id     BLOB    NOT NULL,    -- owner: actor_id for mail/calendar/card/post; channel_id for conv
    kind         TEXT    NOT NULL,    -- a § Layout kind tag ('mail' | 'conv' | 'calendar' | 'card' | 'post')
    segment_id   INTEGER NOT NULL,    -- u32 from KindManifest
    record_cid   BLOB    NOT NULL,    -- 36-byte fauna_cbor::Cid (dag-cbor codec, blake3-256)
    bucket       TEXT    NOT NULL,    -- 'YYYY-MM'
    tombstoned   INTEGER NOT NULL DEFAULT 0,
    changed_seq  INTEGER NOT NULL DEFAULT 0,  -- content-scope change cursor; owner: account-sync-plane.md § Feeds and cursors
    -- kind-specific sparse floor (nullable per kind):
    received_at  INTEGER,             -- mail: the MESSAGE's receive time; relayed verbatim (drives INTERNALDATE + the bucket)
    stored_at    INTEGER,             -- mail: when THIS nest stored the record; always local (see § Continuation records). NULL = unknown
    sender_dom   TEXT,                -- mail: envelope sender domain
    spam_disp    TEXT,                -- mail: 'accept' | 'accept_to_spam_folder' | 'policy_junk'
    is_own_submission INTEGER,        -- mail: 1 if own-submission path (Sent vs INBOX placement)
    seq          INTEGER,             -- conversation: per-channel sequence (active for kind='conv', Plan 7); mail: per-actor relay cursor
    legal_takedown_ref TEXT,          -- conv: legal-compulsion withhold flag; owner: behavior/moderation.md (legal-takedown)
    report_hash  BLOB,                -- report-sharing content identity; owner: behavior/report-sharing.md
    continuation_role INTEGER NOT NULL DEFAULT 0,  -- mail: 0 normal | 1 part | 2 head (§ Continuation records)
    PRIMARY KEY (scope_id, kind, segment_id, record_cid)
);
CREATE INDEX idx_segment_records_scope_kind_bucket
    ON segment_records(scope_id, kind, bucket);
CREATE INDEX idx_segment_records_record_cid
    ON segment_records(record_cid, kind);  -- supports scope-by-CID lookup
```

The segment file pair (`.dat` + `.meta`) is the authoritative copy of per-record metadata; the SQLite mirror is the lookup index. Recovery: walk segments → re-emit mirror rows. Byte-offset / byte-length columns are gone — random-access reads go through the CARv2 `MultihashIndexSorted` index in the `.dat`, not a SQLite-mirrored offset table. **There is no `dtstart` column** — calendar was never given a reserved bucket-key column and buckets on `created_at` (server receive time) exactly like every other kind, deliberately never the event's own start time: a meeting scheduled years out must not create a far-future bucket and strand its record from compaction (`bins/fauna-nest/src/segments/cal.rs`).

**A rebuild re-emits the mirror's WHOLE column set, through the kind's own insert helper (ratified 2026-08-23).** "Re-emit mirror rows" means every column the floor is authoritative for, not the subset a given rebuild site happened to list: restore DELETEs the scope's mirror rows before rebuilding (§ Restore dispatch), so a column a rebuild omits is **destroyed**, not left stale. Rebuild sites therefore call the kind's canonical writer (`records_db::insert_mail` / `insert_conv` / `insert_calendar` / `insert_card`) rather than hand-rolling an INSERT — a hand-listed column set silently drifts from the one the append path writes, and the compiler cannot see the drift. Measured (2026-08-23): `rebuild_mail_segment_records_from_disk` hand-rolled its INSERT and omitted `seq`, `report_hash` and `stored_at`, while its conv sibling — shaped from it — had carried `floor.seq` all along. `seq` was the load-bearing loss: the relay drain selects `WHERE seq > ?` and SQLite's `NULL > n` is NULL, so a restored mail corpus was permanently invisible to `mail_pull` and the ack that purges the source never came; `next_mail_seq`'s `COALESCE(MAX(seq), 0) + 1` also restarted the counter at 1, hiding post-restore arrivals under a puller's existing cursor. The helper additionally assigns `changed_seq`, which the hand-rolled INSERT left at 0 until the next boot's backfill. Pinned by `filesync_handlers::tests::restore_mail_preserves_the_relay_seq_cursor_and_the_floor_columns`.

**What goes in the mirror columns vs the segment sidecar's `floor_metadata`.** Mirror columns hold the floor items the nest queries on by SQL (range-by-`received_at`, filter-by-`spam_disp`, lookup-by-`record_cid`). Audit-only or compaction-only floor items (timestamp, SPF / DKIM / DMARC verdicts, spam score) live in the per-record `floor_metadata` slot in the `.meta` sidecar — read at compaction time or via the per-record fetch path, not by SQL. The uniform scoring-bus rows (`MailFloorMetadata.scores`, 2026-07-05) follow the same authoritative-footer rule with one twist: their SQL-queryable mirror is the dedicated `content_scores` table (not a `segment_records` column), rebuildable from the footers the same way (`content-scoring.md` § The scoring-metadata bus).

## Outer manifest per kind

Each `<kind>/<actor>/manifest.<short>` is a tiny typed file (~KB) listing the live state:

```rust
// libs/fauna-segment-store — one kind-tagged outer manifest type for every
// kind (the `kind` field self-tags the file so the loader verifies
// scope/kind alignment). Replaced the per-kind `MailManifest` wrapper at the
// Plan 6 composition lift.
struct Manifest {
    format_version: u16,             // writer's format
    min_reader_format_version: u16,  // oldest format_version that can read it; #[serde(default)] = 1
    kind: String,                 // a § Layout kind tag
    kind_manifest: KindManifest,
}
// KindManifest = { next_seg_id: u32, live_segments: Vec<u32>, tombstoned_segments: Vec<u32> }
```

Persisted via atomic-rename (write to `manifest.mail.tmp`, fsync, rename over `manifest.mail`, fsync directory). Stays in low KB. Rewritten on each segment finalize and each compaction.

The same `KindManifest` type backs `libs/fauna-index/`'s outer manifest (one type, two consumers — priority #3).

## Record lifecycle (formerly "Per-mode behavior" — the plaintext/encrypted deployment-mode axis is retired)

The table this section used to carry described a `Plaintext mode` / `Encrypted mode` bifurcation
(down to `PlaintextStorage` / `EncryptedStorage` Rust types); the axis is retired
(`nest/storage-modes.md`) and those types are deleted. Every nest now runs the one sealed
posture below.

- **Record ingest** (e.g. inbound mail at MTA bridge). MTA bridge seals the record under the
  recipient's HPKE key before it ever reaches the nest; `segments::mail::append_record` accepts
  only a typed `SealedRecordBytes` (§ Invariants binding any kind whose body moves out of a
  SQLite column, invariant 3 — enforced by type, S6.12) → appends it to the actor's open segment
  (composes `SegmentManager` + the mail encoder) → updates `segment_records`. Content that
  arrives already sealed (mail relayed from a peer; a client-sealed conversation envelope)
  stores verbatim. Server-side classifiers + content-index ingest never run unconditionally
  after append — they run only at capability positions (the AUTH'd MDA session, the user's
  client, or a granted content-processing bridge; `encryption-at-rest.md` § Readable classes).
- **Reads** (IMAP `SELECT`, conversation history). MDA bridge / shared lib queries
  `segment_records` for routing and reads the local plaintext-framed segment file directly via
  `FramedSegment::read_records_bulk`; records are sealed, so an AUTH'd MDA session (session-scoped
  MLS read key access) or a key-holding client decrypts. Off-session: nest serves floor metadata +
  opaque sealed records only.
- **Compaction.** Nest runs it (landed in Plan 3 — every 6 h plus on-demand via the
  `fauna.segments.compact` WS-RPC kind). Reads SQLite floor, copies live byte ranges between
  segments, updates SQLite + manifest. Payload bytes are copied verbatim — compaction never opens
  or reseals them; no record-key access required.
- **Cross-location backup sync.** The data owner's *client* mediates uniformly: fetches
  plaintext-framed segment bytes from the source nest via
  `GET /api/v1/segments/{kind}/{actor}/{segment_id}` (the permanent segment byte route — see
  § Cross-location backup protocol below), chunks + encrypts with the client's `BackupKey`,
  uploads chunks to the destination via the standard `POST /chunks` pipeline. The MLS read-side
  key never enters this code path because the segment-store layer never opens record payloads
  (parent spec § D4; this spec ratifies it operationally). Wire surface design ratified
  2026-05-15 (tracked internally).
- **Snapshot create / restore / delete.** Landed in Plan 3. Manifest pin + restore via the kind's
  restore handler. The manifest is floor-shape, so a snapshot persists without record-key access.

The one remaining source of unsealed at-rest bytes is **pre-Phase-3 legacy residue**: a record
ingested on a formerly-plaintext-mode box before Phase 3 (2026-07-08) rests raw until the
boot-time backfill converges it (`nest/storage-modes.md` § Legacy artifacts;
`encryption-at-rest.md` § Readable classes item 3). Readers discriminate per record
(`is_sealed_mail_record`) throughout that transition window.

## Snapshots — pin a manifest

A snapshot of a message kind for an actor is exactly: a copy of that kind's outer manifest at time T, paired with the matching placement manifest (for kinds with a placement layer — mail, calendar, and card today). Stored as one row in the existing `snapshots` SQLite table with three new nullable columns landed in Plan 3:

```sql
ALTER TABLE snapshots ADD COLUMN message_kind TEXT;        -- NULL for folder snapshots; a § Layout kind tag ('mail' | 'conv' | 'calendar' | 'post' | …) otherwise.
ALTER TABLE snapshots ADD COLUMN message_manifest BLOB;    -- serialized outer manifest (the kind-tagged Manifest).
ALTER TABLE snapshots ADD COLUMN placement_manifest BLOB;  -- serialized MailPlacementManifest / CalPlacementManifest; NULL for kinds without a placement layer.
```

Existing folder snapshot rows keep their semantics — `message_kind IS NULL`. The new columns are populated only for message-kind snapshots.

**Reuse vs. new table.** Reuse for uniformity with folder snapshots — same retention policies, same deletion safety windows (`docs/goal/behavior/backup-restore.md` § 7), same admin / UI surface (`docs/goal/ui/backups.md`).

**Atomicity.** When the `fauna.filesync.snapshot.create_message_kind` WS-RPC kind is invoked with `kind = "mail"`, the nest serialises both the content `Manifest` and `MailPlacementManifest` inside the same SQLite transaction as the `INSERT INTO snapshots` — a message-kind snapshot is never created with a stale pairing between content and placement. (The placement layer itself is owned by the IMAP+CalDAV restore design (ratified 2026-05-14; tracked internally) § D4 — and the placement-journal infrastructure that exposes a load-current-manifest API was landed under that design's Plan 1, tracked internally.)

**Coverage.** Plan 3 lands snapshots for the `mail` kind (segment manifest from the mail `SegmentManager` via `segments::mail`, placement manifest from `MailPlacementSegmentManager`). **Calendar + card snapshots have landed at full mail parity (S6.9, 2026-07-10):** both pin the kind's content `Manifest` (from `cal_segments` / `card_segments`) *and* the placement manifest (`CalPlacementManifest` / `CardPlacementManifest`, format v2 — carrying the content-record id, effective `encrypted_fauna_ext`, and tombstone `deleted_at` that make the placement⋈content restore join exact); restore rebuilds the bridge rows, the `segment_records` mirror, and the expunged tombstones in one transaction under the `"gc"` op lock. **Conversation (`conv`) snapshots have landed** (the conversations-segment rollout's Plan 8): a conv snapshot pins the channel's segment `Manifest` (from `conv_segments` via `segments::conv`) and `placement_manifest = NULL` (conv has no placement layer — no UIDVALIDITY equivalent). A conv snapshot is **scoped to a `channel_id`** (the MLS group), not an actor — the pinned manifest is keyed on `folders.actor_id = channel_id` via the `__conv/<channel_hex>` reserved folder (`get_or_create_reserved_conv_folder`, `bins/fauna-nest/src/db/snapshots.rs`); see § Restore dispatch on message_kind for the conv restore arm. **Post snapshots have landed** (Track C posts, Plan C C1): a post snapshot pins the author's post `Manifest` (from `post_segments` via `segments::post`) and `placement_manifest = NULL` (posts have no placement layer). A post snapshot is **author-scoped** — keyed on `folders.actor_id = author` via the fixed `__post` reserved folder (`get_or_create_reserved_folder(author, "post")`, the mail shape, not conv's per-scope-named one). See § Restore dispatch on message_kind for the post restore arm — which, unlike conv/mail, is **additive** (no-data-loss recovery, not revert).

## Pin set for compaction GC

The compaction worker treats a segment as eligible for input only if it is **not** referenced by any pinned snapshot manifest **and** not already in the active manifest's `tombstoned_segments` list (already-compacted segments awaiting 14 d retention). The active manifest's `live_segments` *are* the compaction candidates — those are what compaction picks up and rewrites. Implementation:

1. Build a `PinSet` (from `libs/fauna-segment-store::PinSet`) seeded with the active `Manifest`'s `tombstoned_segments` only. (Including `live_segments` would make compaction a no-op — `pick_compaction_inputs` skips any segment in the pin set.)
2. Query `snapshots` for rows matching `(message_kind = <kind>, soft_deleted = 0, deletion_pending = 0)` for the target scope (the actor for `mail`/`cal`; the `channel_id` for `conv`) — the pin-set query `list_active_message_kind_snapshot_manifests(scope, kind)` keys on `folders.actor_id`, which carries the channel_id for conv, so the query is unchanged across kinds; for each row, deserialise `message_manifest` and `extend_from_manifest(&pin_set)` with its `kind_manifest`. (Soft-deleted snapshots still pin until their `purge_after` elapses — same recovery semantics as today's folder snapshot delete; see § Immediate-delete override for the exception.) This adds both `live_segments` and `tombstoned_segments` from each pinned snapshot — anything any snapshot references must be preserved across compaction.
3. Pass the pin set to `pick_compaction_inputs` (`libs/fauna-segment-store::compaction::pick_compaction_inputs`). Any segment in the pin set is filtered out of the input list before the threshold gate fires.

A pinned snapshot therefore holds segment bytes across compaction; the segment moves to `tombstoned_segments` only once every active and pinned-snapshot manifest has forgotten it. (Physical-delete eligibility — the 14 d retention gate that fauna-sync GC applies to `tombstoned_segments` — is a separate, broader question that *does* take both `live_segments` and `tombstoned_segments` of every active/pinned manifest into account; the design spec § D8's "GC pin set" language refers to that broader concept. The compaction-input-eligibility gate above is a strictly narrower question.)

## Nest dehydration (segment eviction) — direction ratified 2026-08-10, unbuilt

The nest's own replicas gain hydration policy (`account-data-plane.md` R10 (account-data-plane.md § The ratified decisions) owns the decision; this section owns the segment-store mechanics when they are built — the eviction predicate + worker themselves are unbuilt, T18 holds their first-build detail; their prerequisite, the custody receipt contract below, is now built):

- **The eviction unit is the whole segment file.** A CARv2 file is immutable — there is no per-record hole-punching; the same law the client-side store applies to adopted segments. Evicting keeps the sidecar-derived index rows (`segment_records` stays complete — "complete in metadata" is a nest property too) and drops the `.dat` payload bytes.
- **The predicate is confirmed custody elsewhere.** A segment's payload may be dropped only when the custody accounting this doc already runs (`backup_custody` latest-per-path, destination `fauna.backup.status`, client-device custodian check-ins) confirms at least one other replica holds its bytes — or the owner explicitly accepted reduced redundancy at opt-in (the file-sync content-residency case, `../behavior/file-sync.md` § Content residency). No confirmation and no consent → the nest refuses to evict; disk pressure alone never overrides the predicate (the example.com disk-fill incident is the motivation, not a licence).
- **The predicate's evidence is a first-class contract: custody receipts** (ruled 2026-08-11, greenfield finding A7 — promoted from build detail because "confirmed custody elsewhere" is distributed reference counting plus availability attestation, the load-bearing sentence between dehydration and silent data loss). A custody receipt is a **signed, dated attestation** by a holding replica covering a scope/CID-range; eviction requires **N-of-M receipts younger than an aging margin**, receipts are **re-verified on a cadence** (a receipt is a claim, not a fact — holders die, lie, or get wiped), and the honest failure mode is stated in UI rather than absorbed: a dead or lying custodian is **degraded redundancy the owner sees**, never a silently-weakened predicate. **The receipt contract itself is now built** (`fauna_core::custody_receipt` — mint, sign/verify against the custodian's bound device key, carriage over its own channel-message body, and owner-side monotone fold; landed as the replica-posture custody grant's periodic check-in, W8.7 (account-data-plane.md § Workstreams)). What remains unbuilt is the **consumer**: the parameters (N, M, margins, cadence) that turn a set of receipts into a dehydration go/no-go, and the eviction worker itself, are T18's unscheduled first-build detail — **no dehydration ships before that consumer lands** (`account-data-plane.md` R10 owns the predicate's ruling; this bullet owns the receipt mechanics).
- **The bootstrap consequence is stated, not hidden.** The nest is the content bootstrap for new devices (`account-data-plane.md` § Nest-side requirements); a dehydrated scope bootstraps metadata-complete with payload hydration deferred to a live holding replica — surfaced in the opting UI, never discovered.

## Manual compact endpoint

`fauna.segments.compact` WS-RPC kind. Request shape (canonical-CBOR, defined in `libs/fauna-protocol/src/segments.rs`):

```
CompactRequest {
  kind:     Option<String>,   // "mail" | "conv" | "post" | "calendar" | "card" | None (None = all accepted kinds)
  actor_id: Option<ActorId>,  // None = whole-nest scope (admin-only). For kind="conv", carries the channel_id.
}
```

Reply carries `segments_rewritten`, `errors`, and an echo of the resolved scope. Runs the compaction worker immediately for the requested scope (per spec D10). Identical to the periodic 6 h run except the tombstone-fraction threshold is dropped to 0 — any closed-month bucket with at least one tombstoned record is rewritten. Reclaims local disk for tombstoned records.

The worker + `fauna.segments.compact` accept `kind ∈ {"mail", "conv", "post", "calendar", "card"}`, and the whole-nest scheduled default covers all five (`segments_for_kind` / `resolve_kinds` in `bins/fauna-nest/src/segments/compaction.rs`; calendar/card arms landed S6.8c, 2026-07-10, with the per-kind orphan reaper running first under the same `"gc"` lock). For a `conv` request the `actor_id` carries a **`channel_id`** (the MLS group, not an actor), and the scoped-compact authorization is **channel membership** — `bearer ∈ list_channel_actors(channel)` (consistent with conv ingest/reads) OR nest admin, not owner-equality (`bins/fauna-nest/src/segments/compact_handler.rs`). Post requests authorize on owner-equality (`bearer == author`), the mail shape.

**Does not:**

- Violate snapshot pins. The pin set is rebuilt for the run; pinned segments are never inputs.
- Skip the 14 d tombstoned-segment retention. Compaction's outputs (the newly-tombstoned inputs) are held for 14 d before fauna-sync GC reclaims chunks.
- Delete snapshots. That's the immediate-delete override below.

**Authorization:** the scoped-compact membership test is kind-dependent (`bins/fauna-nest/src/segments/compact_handler.rs`):

- **Scope owner / member:** always, all kinds. For `mail`/`cal` the scope is an actor, so "owner" = `bearer == actor_id`. For `conv` the scope is a `channel_id`, so owner-equality is never true; a conv request authorizes on **channel membership** — `bearer ∈ list_channel_actors(channel)` (consistent with conv ingest/reads).
- **Nest admin (≠ owner) on a plaintext-framed local node** (primary nest or active-replica destination with `read` capability): yes — compaction operates on floor + payload bytes without opening or resealing them. The payload is sealed under the kind's own authority; compaction never needs record-key access regardless of what capability grants the box's admin happens to hold (the plaintext/encrypted deployment-mode axis this used to turn on is retired — `nest/storage-modes.md`; `encryption-at-rest.md` § Capability tiering owns what an admin may separately read).
- **Nest admin on a pure-backup destination** (`backup` capability, no local segment files reconstituted): cannot run; returns `fauna.segments.pure_backup_destination` with the same explanatory message as the retired HTTP twin. The admin can still run standard fauna-sync blob-store retention (evicting chunks the source has tombstoned). This holds for `conv` too (Plan 9): `is_pure_backup_destination("conv", channel)` derives the per-channel `__conv/<channel_hex>` set, so a custody-copy conv channel refuses manual compact after the channel-membership auth.

## Immediate-delete override

`fauna.filesync.snapshot.delete_immediate` WS-RPC kind. Request shape (canonical-CBOR, defined in `libs/fauna-protocol/src/filesync.rs`):

```
SnapshotDeleteImmediateRequest {
  snapshot_id: i64,
  confirm_id:  String,   // = snapshot_id retyped as a string
  acknowledge: String,   // "I understand this is immediate and irreversible."
}
```

Skips the standard 48 h `SnapshotDelete` pending action and the 30 d soft-delete window. Frees segments pinned only by this snapshot at the next compaction cycle (the underlying segment bytes still observe the 14 d tombstoned-segment retention — the override governs the snapshot lifecycle, not the segment lifecycle).

**Still enforces:** the hard floor of 3 active snapshots (`docs/goal/behavior/backup-restore.md` § 7 Layer 1) — returned as `fauna.filesync.snapshot.hard_floor_breach`.

**Authorization:** **owner only**. Nest admins, even on the owner's primary nest, **cannot** invoke this kind. The admin's lever for reclaiming local disk is the `fauna.segments.compact` WS-RPC kind (above); deleting another user's recovery point is not an admin concern. Mismatched `confirm_id` → `fauna.filesync.snapshot.confirm_mismatch`; mismatched `acknowledge` (byte-for-byte compare) → `fauna.filesync.snapshot.acknowledge_mismatch`; neither mutates state.

## Restore dispatch on message_kind

`fauna.filesync.snapshot.restore_message_kind` WS-RPC kind. Request shape:

```
SnapshotRestoreMessageKindRequest {
  snapshot_id: i64,
  confirm_id:  String,   // = snapshot_id retyped as a string
}
```

Dispatches on the snapshot row's `message_kind`:

- **`message_kind IS NULL`** — folder snapshot. The WS-RPC kind refuses with `fauna.filesync.snapshot.unknown_kind`; folder restore is client-side, the owner's app walking the snapshot's manifests (`docs/goal/behavior/backup-restore.md` § 4).
- **`message_kind = 'conv'`** — conversation snapshot. **Landed** (the conversations-segment rollout's Plan 8) via `restore_conv` (`bins/fauna-nest/src/filesync_handlers.rs`). The conv scope is a `channel_id`; authorization is **channel membership** (`bearer ∈ list_channel_actors(channel)`), not owner-equality. **No `bridge_active_for_actor` precondition** — no bridge serves conversations — and **no placement layer**. Handler (one SQLite transaction): `finalize_open` the channel's `conv_segments`, verify each pinned `live_segments` entry's `seg-NNNNNNNN.dat` is on disk (else error — fauna-sync chunk pull incomplete), `DELETE FROM segment_records WHERE scope_id = <channel> AND kind = 'conv'`, rebuild the conv mirror rows from the pinned segments' on-disk `.dat` footers (`rebuild_conv_segment_records_from_disk` — decodes each record's `fauna_mls::segments::ConvFloorMetadata` floor, re-emitting the per-channel `seq`), replay the restoring member's roster row (`INSERT OR IGNORE INTO actor_channels (actor_id, channel_id, created_at)`), and record a `restore_history` row under the bearer. The conv reply carries no `__config` / bridge-AUTH note (no bridge to re-AUTH).
- **`message_kind = 'post'`** — post snapshot. **Landed** (Track C posts, Plan C C1) via `restore_post` (`bins/fauna-nest/src/filesync_handlers.rs` → `segments::post::restore_from_manifest`). The post scope is the **author**; authorization is **owner-equality** (`bearer == author`, the mail shape), and — like conv — there is **no `bridge_active_for_actor` precondition** (no bridge serves posts; the feed read runs off the `content` projection) and the reply carries no `__config`/bridge-AUTH note (`config_present = true`). **The post restore is ADDITIVE — it does NOT `DELETE FROM segment_records` first**, the load-bearing divergence from the conv/mail revert-replace. Two reasons: (1) **no-data-loss** — posts are public, user-authored content and a restore RECOVERS the snapshot's posts (the cross-location-backup → fresh/recovery-nest case) without dropping the author's *newer* posts (which a DELETE-all-then-rebuild-from-pinned-segments would); (2) posts carry a **separate cross-actor `content` feed-index projection** (the read model `query_feed` scans) that conv's `segment_records`-as-read-model has no analogue of, so the rebuild re-asserts BOTH the `segment_records` mirror (so `load_post_body` resolves the body by CID) AND the projection (so `query_feed` lists the post — rebuilt from each restored body via `put_post_index_only`, which derives the source from the body and FTS-indexes it). Each re-assertion is idempotent: the mirror by a skip-if-present author-scoped CID guard (the `record_cid` index is non-UNIQUE), the projection by content-addressed upsert. The handler finalizes the store, verifies each pinned `live_segments` entry is on disk (else error — fauna-sync chunk pull incomplete), then re-asserts; a `restore_history` row is recorded under the bearer. tier_3 `bins/fauna-nest/tests/segment_post_snapshot_restore.rs` proves the full create → snapshot → wipe-mirror-and-projection → restore → `posts.get`/`query_feed`-serve round trip.
- **`message_kind IN ('mail', 'calendar', 'card')`** — message-kind snapshot. Mail new in Plan 3 (migrated to WS-RPC in that slice); calendar/card at full parity since S6.9 (2026-07-10).

  Pre-conditions (handler-side; refusal does not mutate state):

  1. **`confirm_id` matches snapshot id** — mismatch → `fauna.filesync.snapshot.confirm_mismatch`.
  2. **Bridge not serving the actor** — `fauna.filesync.snapshot.bridge_active` if a `subscribe_mailbox_state` registration exists for the actor (`docs/goal/behavior/imap-server.md` § IDLE wiring). The admin stops the bridge via the client-side bridge-lifecycle controls before restore.
  3. **Wrapped-MLS-blob recovery** (design ratified 2026-05-14; tracked internally — § D7) — advisory only: the handler checks the actor's wrapped MLS blobs are present (`has_wrapped_mls_blobs`); a missing one is reported in the reply's `config_present` and `note` fields, restore still proceeds (mail/calendar restore doesn't structurally require the blobs, but the bridge can't AUTH after restart without them). The field name and the note's wording still carry the `__config` name, a rail retired 2026-10-02 ([`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution schedule → *The closure order*, step (6)).

  Handler steps (one SQLite transaction):

  1. Drop existing `bridge_imap_*` / `bridge_caldav_*` rows for the actor — an **intentional restore-time replace** (restore overwrites the actor's current bridge state with the snapshot's), made crash-safe by the enclosing single SQLite transaction (a failure rolls back, leaving the prior rows intact). This is a *replace*, not a schema-evolution drop, so it respects the alpha no-user-data-loss rule (`version-compatibility.md` § I1 / `principles.md` § No user-data loss).
  2. Reconstitute `__mail` / `__mail-placement` (or calendar equivalents) segment files locally. The data-owner's client mediates uniformly on every nest (the plaintext/encrypted deployment-mode axis this step used to branch on is retired — `nest/storage-modes.md`), per `2026-05-14-message-segment-store-design.md` § D9 — this section does not redefine the mediation; the implementation calls the existing client-driven path.
  3. Replay the pinned `placement_manifest` into the corresponding SQLite tables via the placement-journal replay API (`replay_mail_placement_manifest` / `replay_cal_placement_manifest` — exposed by the placement-journal owner; consumed here as an interface).
  4. Rebuild `segment_records` from the reconstituted content segments (mechanism inherited from § `segment_records` SQLite mirror — "The SQLite mirror is rebuildable from segments during recovery"). The rebuild walks each `live_segments` entry from the restored `message_manifest`, opens the `.dat` via `libs/fauna-carv2::Reader` for per-record CIDs and the `.meta` sidecar for per-record floor metadata, and re-emits one mirror row per record. Per the `SegmentManager` flush-on-read discipline, any reader opening segments directly must call `finalize_open()` first.
  5. Insert a `restore_history` row recording `(snapshot_id, actor_id, kinds_restored, source_member_id, completed_at)`.

  **Post-condition:** the admin restarts the bridge via the same client-side bridge-lifecycle controls. The MDA re-reads `storage_mode` via `fetch_config` — a wire-compat constant only, since there is no storage mode (`docs/goal/behavior/imap-server.md` § Content path). MUAs reconnect via their existing IMAP / CalDAV paths. MUA-observable behaviour (UIDVALIDITY preserved, QRESYNC-stale-modseq fallback, divergence logging) is owned by `docs/goal/behavior/imap-server.md` § Restore divergence detection (IMAP) and `docs/goal/behavior/caldav-server.md` § Restore divergence (CalDAV).

  CalDAV `etag` is already content-derived from `modseq` (`bins/fauna-nest/src/db/bridge_caldav.rs:43-44` — `format_etag(modseq) = format!("{:016x}", modseq)`) and `modseq` is preserved by the placement manifest, so MUA `If-Match` continuity falls out of the restore without further work — verified in this plan; `2026-05-14-imap-caldav-restore-design.md` § D8 open item closed.

## Destination capability

When a message-kind reserved folder (`__mail`, `__conv/<channel_hex>`, …) is created on a destination nest (the nest *receiving* chunks for another nest's data, not the source nest where the data is originally appended), the destination nest marks the row a **custody copy** as it provisions it (`folders.custody_copy`, nest-internal — [`../behavior/reserved-folders.md`](../behavior/reserved-folders.md) § Destination capability owns the discriminator and the 2026-09-28 ruling that retired the owner-declared `mode`; built 2026-09-28, schema 94). The destination nest enforces four gates on a custody copy (`is_pure_backup_destination`, which reads the flag) (described for `__mail` first; the conv, post, and calendar/card variants follow):

1. **IMAP serving** — every `fauna.bridges.*` WS-RPC handler registered by `register_bridge_imap_handlers` (`list_mailboxes`, `select_mailbox`, `list_messages`, `fetch_message_metadata`, `fetch_message_ciphertext`, `fetch_index_segments_since`, `store_flags`, `expunge`, `copy`, `move`, `append`, `search_messages`, `get_quota`, `create_mailbox`, `delete_mailbox`, `rename_mailbox`) refuses with a typed `pure_backup_destination` error. No local plaintext-framed segment file exists to read from; opaque chunks cannot satisfy an IMAP `SELECT` or `FETCH`.

2. **Manual compact** — `fauna.segments.compact` with `actor_id = Some(<pure-backup actor>)` returns `fauna.segments.pure_backup_destination` with the explanatory message `"compaction requires local plaintext-framed segments; this destination holds opaque chunks only"`. The whole-nest variant (`actor_id = None`) still runs but skips per-actor work on pure-backup actors inside the worker loop.

3. **Scheduled compact** — the whole-nest `CompactionWorker::run_once` pass skips pure-backup `(scope, kind)` tuples in its scope×kind loop (`segments/compaction.rs`). The gate is **per-(scope, kind)**, not per-scope: the whole-nest scheduled default compacts every scope against every kind `resolve_kinds` returns (`"mail"`, `"conv"`, `"post"`, `"calendar"`, `"card"`), so a nest that is a pure-backup destination for one kind/scope still compacts the kinds/scopes it serves locally. Manual triggers hit the same predicate at the route's 409 gate (gate 2) before the worker runs.

4. **Message-kind snapshot create** — `fauna.filesync.snapshot.create_message_kind` with `kind = "mail"` returns `fauna.filesync.snapshot.pure_backup_destination` with the same explanatory message as gate 2. Snapshots pin a manifest of local plaintext-framed segment files; a pure-backup destination has no such manifest to pin.

**Conversations (`kind = "conv"`)** wire the same four gates (the conversations-segment rollout's Plan 9), scoped to a `channel_id` (the MLS group) rather than an actor — the `__conv/<channel_hex>` reserved set, `folders.actor_id = channel_id`. The gates differ only at gate 1, since conversations have no IMAP surface:

1. **Channel-history reads** — `fauna.conversations.channel.fetch` refuses with `fauna.conversations.pure_backup_destination` (`require_local_conv_serving`, `conversations_handlers.rs`); opaque chunks cannot satisfy a channel-history query. (The federation `mls_pull` path is nest-to-nest and requires pairing; a pure-backup destination is never a paired *home* nest, so it is unreachable there and not separately gated. `group.list_messages` reads the legacy `group_messages` table, not the conv segment store.)
2. **Manual compact** — `fauna.segments.compact { kind = "conv", actor_id = Some(channel) }` returns `fauna.segments.pure_backup_destination` after the channel-membership auth (`compact_handler.rs`).
3. **Scheduled compact** — covered by the per-(scope, kind) gate above (a pure-backup `(channel, "conv")` is skipped in the whole-nest loop).
4. **Snapshot create** — `fauna.filesync.snapshot.create_message_kind { kind = "conv" }` resolves the target channel set first, then refuses the whole call with `fauna.filesync.snapshot.pure_backup_destination` if *any* target channel is a pure-backup destination, before creating a single snapshot row (`create_conv`, `bins/fauna-nest/src/filesync_handlers.rs`) — all-or-nothing across a batched `actor_id = None` call, not a per-channel partial commit.

**Posts (`kind = "post"`)** wire the same four gates (Track C posts, Plan C), scoped to the post **author** — the fixed `__post` reserved set, `folders.actor_id = author`. As with conv, the gates differ only at gate 1:

1. **Body reads** — gate 1 is satisfied **by construction**, with **no explicit refusal handler**. Posts have no always-on plaintext-serving surface bound to backup custody (no IMAP/CalDAV bridge, no channel-fetch registration). Every post read — single-post `fauna.posts.get` → `segments::post::load_post_body`, and the cross-actor `query_feed` — reads off the local `__post` segments + the `content` feed-index projection, which a pure-backup destination does **not** hold for the backed-up author (it holds only opaque `BackupKey`-sealed chunks in `backup_custody`, never decoded for a read). So a post read on a pure-backup destination returns a natural not-found / empty feed — there is no local plaintext to leak and nothing to refuse. (A destination that *also* legitimately holds some of the author's posts as plaintext — e.g. a federation peer — serves those and not-founds the backup-only ones, which is correct: it serves only what it holds locally as plaintext.)
2. **Manual compact** — `fauna.segments.compact { kind = "post", actor_id = Some(author) }` returns `fauna.segments.pure_backup_destination` after the owner-equality auth (`compact_handler.rs`).
3. **Scheduled compact** — covered by the per-(scope, kind) gate above (a pure-backup `(author, "post")` is skipped in the whole-nest loop, which now sweeps `"post"` too).
4. **Snapshot create** — `fauna.filesync.snapshot.create_message_kind { kind = "post" }` refuses with `fauna.filesync.snapshot.pure_backup_destination` (the author-scoped gate, alongside mail's).

The cross-location backup *protocol* (Plan 5), the custodian custody + GC-safety (Track B — the `backup_custody` projection + GC reference walk), and the device-pull exclusion are all **kind-agnostic** (they key on the custody-copy flag, not the segment kind), so posts reuse them unchanged: a `__post` backup folder routes through `upsert_backup_custody` / the GC `backup_custody_manifest_hashes` walk with no post-specific code. (The per-kind sweep orchestration — which scopes a kind backs up to which destinations — remains deferred uniformly for all kinds, as for mail + conv; posts joined the shared `BACKED_UP_KINDS` list the nest arm sweeps on 2026-09-29, `segment-backup-protocol.md` § Implementation status today.)

**Calendar and card (`kind = "calendar"` / `"card"`)** wire the same four gates (S6.10, 2026-07-10), scoped to the owner **actor** — the fixed `__calendar` / `__card` reserved sets, `folders.actor_id = actor`, the mail shape. The gates differ only at gate 1, since calendar/card have no IMAP surface:

1. **CalDAV / CardDAV serving** — every calendar/card RPC's caller-scoping check (`require_caller_scope` in `bridge_caldav_handlers.rs` / `bridge_carddav_handlers.rs`) refuses with `fauna.bridges.pure_backup_destination` once `is_pure_backup_destination("calendar" | "card", target)` is true, for every caller class (bridge-MDA or a direct client), read or write — the mail twin is `require_local_mail_serving`.
2. **Manual compact** — `fauna.segments.compact { kind = "calendar" | "card", actor_id = Some(actor) }` returns `fauna.segments.pure_backup_destination` after the owner-equality auth (`compact_handler.rs`), the same kind-generic route mail and post use.
3. **Scheduled compact** — covered by the per-(scope, kind) gate above (a pure-backup `(actor, "calendar" | "card")` is skipped in the whole-nest loop).
4. **Snapshot create** — `fauna.filesync.snapshot.create_message_kind { kind = "calendar" | "card" }` refuses with `fauna.filesync.snapshot.pure_backup_destination`, the same owner-scoped gate as mail and post (`filesync_handlers.rs`).

The gate predicate is a single DAO call `CacheDb::is_pure_backup_destination(kind, scope_id) -> bool`, which derives the kind's reserved folder name (`"mail"` → fixed `__mail`, scoped by `actor_id = actor`; `"post"` → fixed `__post`, scoped by `actor_id = author` — posts are author-scoped exactly like mail, one `__post` scope per author, so they take mail's fixed-name shape rather than conv's per-scope-named one (Track C); `"calendar"` → fixed `__calendar`, scoped by `actor_id = actor`; `"card"` → fixed `__card`, scoped by `actor_id = actor` — both the mail shape, landed S6.10; `"conv"` → `__conv/<channel_hex>`, scoped by `actor_id = channel_id` — the channel hex in the name is historical, predating the per-actor `UNIQUE(name, actor_id)` key) and returns `true` iff a `folders` row exists with that `name`, `scope_id = <param>`, and `custody_copy` set **and, for the actor-scoped kinds (every kind but channel-scoped `conv`, whose scope holds live records on every member nest), the scope holds no live `segment_records` for the kind** (added 2026-09-26; no enroll points an owner's destination at their own home nest, so for those kinds the pair arises only from a materialize). "Pure" names the corpus shape the gates refuse on, opaque chunks with no local plaintext-framed segments, and not the flag alone. A re-seeded nest keeps its custody set marked a custody copy after `fauna.backup.custody.materialize` (the custody rows stay, and a retried delivery still records into them), but the scope then holds live records and is the owner's live account, so every gate lets it through (`../behavior/backup-destinations.md` § Re-seed, "seeded means a live account"). The predicate reads off the per-scope reserved-set row, created lazily at **snapshot create / serve** (mail does *not* create `__mail` at append — only at snapshot create + IMAP/CalDAV serve; conv mirrors this via `get_or_create_reserved_conv_folder` at conv snapshot create) in `bins/fauna-nest/src/db/snapshots.rs`. The destination-side custody-copy row is created when the data owner adds a backup destination — the destination-management UI, **ratified 2026-06-14** in `docs/goal/ui/backups.md` § Manage backup destinations (owner-administered destinations; held-for-friends deferred). The reserved folder is provisioned **lazily at first upload** for each `(kind, scope)` — since the slice-5 flip, resolved-or-created by the destination's federation record handler behind the nest-writer grant gate (the retired client coordinator used idempotent `fauna.folders.create { mode: "backup" }` + `members.add { role: "backup" }`) — not synchronously at enroll time. The gates fire defensively the moment such a row exists, with or without any chunks yet shipped.

**Why `folders.mode` and not a new `capability` column — SUPERSEDED 2026-09-28 (history: the folders mode contraction retires the column, and the discriminator is the nest-internal `folders.custody_copy`, `../behavior/reserved-folders.md` § Destination capability).** The set-wide `mode` column already exists (default `'sync'`, CHECK constraint admits `'sync' | 'backup' | 'web'`), is owner-set via the user-facing API (matching the project invariant that nest configuration is set from apps), and already documents `'backup'` as an intended value. Three other columns adjacent to this question (`folder_members.role`, the since-deleted `folder_destinations.sync_mode`, the inert `folders.mode` itself) all carried overlapping vocabulary; introducing a fourth column for the same axis would amplify the existing confusion. The data-access property the gates enforce is set-wide ("this whole folder on this nest is pure-backup"), not per-member, so the set-wide column is the right granularity. (Per-member capability heterogeneity within one set would require a separate column; spec § D9 describes capability as a per-destination decision, achievable by separate sets.)

## Cross-location backup protocol

**Split out 2026-09-06 — this concept now lives in [`segment-backup-protocol.md`](segment-backup-protocol.md).** That doc owns who writes a cross-location backup and under what identity, the nest-to-destination auth transport, the pair door and the corpus's `.meta` sidecar, which kinds the two planes serve, the custodian-authoritative custody model and its supersede/remove handshake, the GC reference-walk extension, the custody grace window T with its destination-derived charge and generation recovery, and the client-device custodian's pull plane, seal reproduction and re-seed delivery — with its build ledger. Heading text is unchanged there, so a `§ Cross-location backup protocol` citation resolves by changing only the file. What stays here is the store the protocol backs up: § Segment file format, § Record identity per kind, § Continuation records, § Record lifecycle, § Snapshots, § Pin set for compaction GC, § Restore dispatch on message_kind and § Destination capability — the four gates a pure-backup destination enforces are a property of the store's serving surface, so they stay.

## Reading list

1. (design ratified 2026-05-14; tracked internally) — the design spec.
2. `libs/fauna-segment-store/` — the foundation crate (Plan 1).
3. `libs/fauna-mail/src/segments/` — the mail-kind encoding helpers (`ops.rs`) over the kind-agnostic `SegmentManager`.
4. `bins/fauna-nest/src/segments/` — the nest-side per-kind coordination (`mail.rs` free functions bridging `SegmentManager` + the `segment_records` mirror) + the `records_db` DAO. "Per-kind" names where the code lives, not a requirement that each kind spell out the same mechanics: the kind-agnostic parts live once in `mod.rs` (§ *Landed substrate* → Plan 6 composition lift).
5. `docs/goal/behavior/file-sync.md` § `__mail` Sync — the file-sync-side authority for what rides the chunk pipeline, who chunks-and-encrypts (the data owner's client, uniformly across every nest), and the backup-destination capability flag (`backup` vs `read`).
6. `docs/goal/architecture/encryption-at-rest.md` — Mail body / Mail subject / Mail attachments rows; floor list; Backups (own + held-for-friends) rows.

## Implementation status today

**The backup mechanism's build ledger moved 2026-09-06** — *Backup custody (Plan 4)*, the deferred per-member sweep orchestration, the *Re-seed ceremony* run and both closed *Corpus gap* entries are in [`segment-backup-protocol.md`](segment-backup-protocol.md) § Implementation status today, with the rules they carry. What remains below is this store's own record: the per-kind cutover table, record identity, the substrate layers, the placement journals and the continuation-record leg.

**File-backed transfers (§ Segment size) — built for adoption and the owner's backup pass, one gap (2026-09-29).** Every adoption path stages to disk and verifies from the file, pinned by `libs/fauna-sync-engine/tests/segment_transfer_memory.rs` (an 8 MiB segment adopted over HTTP with no allocation near its size) and the store conformance case `a_transfer_interrupted_before_adoption_reopens_clean`. The owner's backup pass (`segment_backup::SourceBinding` → the custodian pull) stages and seals a chunk at a time, pinned by the same file (a 48 MiB segment kept with no allocation over one sealed chunk, its generation byte-identical to the whole-blob seal). The pull's one-time sidecar backfill, which moves a `.meta` alone into a store that predates sidecars, still reads that sidecar whole. One gap remains: the IndexedDB arm stages to OPFS chunk by chunk but reads the staged `.dat` back whole for admission, because OPFS offers no synchronous random-access read outside a worker and the CARv2 reader is synchronous. No web path fetches segments today, since the transfer lives in the native-only sync engine.

The store, the mail/conv/post rollouts, and the backup custody model are
**built and tier_3-proven**. Calendar + card are now feature-complete too —
content store, boot back-fill, compaction, snapshot create/restore and the
backup four-gates all landed (S6.6–S6.10, 2026-07-09/10); S6.11 is their
tier_3 proof. Per-kind state (the § Layout kind table is the canonical status
column; last verified 2026-07-11):

| Kind | Ingest/serve cutover | Boot back-fill | Compaction | Snapshot create/restore | Backup four-gates | Proof |
|---|---|---|---|---|---|---|
| mail | ✅ Plan 2 | n/a (born on segments) | ✅ Plan 3 (6 h + manual) | ✅ Plan 3 (placement-paired) | ✅ Plans 4/5 | `segment_backup_gc_safety.rs`, `nest_backup_coordinator.rs` |
| conv | ✅ Plan 7 (legacy channel `content` rows dropped) | n/a | ✅ Plan 8 (membership auth) | ✅ Plan 8 (no placement; membership auth) | ✅ Plan 9 | conv four-gate conformance (`conformance_folders.rs` reserved `__conv/*` arms; the old `round_trip_conv` wiremock test retired with the client upload path) |
| post | ✅ Plan B 2026-06-15 (`store_post`/`load_post_body`; feed-index projection retained — `query_feed` never read bodies) | ✅ Plan B3 2026-06-16 (expand→migrate→contract, no row dropped; retired 2026-09-25 with the nest schema's genesis — no pre-cutover post rests anywhere) | ✅ Plan C 2026-06-16 (owner-equality auth) | ✅ Plan C C1 — restore is **ADDITIVE** (no-data-loss recovery; rebuilds mirror AND `content` projection), never a revert | gates 2–4 ✅; gate 1 **by construction** (no plaintext post-serving surface binds to backup custody) | `segment_post_{cutover,snapshot_restore}.rs` |
| calendar | ✅ S6.6 2026-07-09 (`put_event_ciphertext_handler` appends first; serve via `load_event_body`, resolved by the row's stored `record_cid` — the legacy-column fallback retired with the 2026-08-17 record-identity cutover, § *Record identity per kind*) | ✅ S6.7 2026-07-09 (predicate-as-marker; mirror-guarded contract; retired with the same cutover — a live row now has its body only in the segment) | ✅ S6.8b/c 2026-07-10 (orphan reaper, 3 fences; `compact_bucket` + tx arms) | ✅ S6.9 2026-07-10 (placement-paired, mail shape; journal at format v2 carrying the content-record id + sidecar + `deleted_at`; restore rebuilds rows + mirror + expunged under the `"gc"` lock) | ✅ S6.10 2026-07-10 (serve/mutate refusal in `require_caller_scope`; compact worker + manual + snapshot-create gates kind-parameterized) | ✅ S6.11 2026-07-11 — `tests/e2e-unified/tests/test_dav_content_at_rest_e2e.py` (tier_3, 9 tests, real binary + real CalDAV/CardDAV MDA): body rests only in the segment (no body column + live mirror + no plaintext on disk); sealed-envelope conformance walk (body AND hint); snapshot create→restore, boot back-fill, and the v1→v2 placement-manifest boot heal all byte-identical across the transition — both boot paths mutation-proven (the heal and its proof retired 2026-09-24 with the v1 read path, by the compat-remnant sweep) |
| card | ✅ S6.6 twin (`put_card_ciphertext_handler` / `load_card_body`) | ✅ S6.7 twin | ✅ S6.8b/c twin | ✅ S6.9 twin | ✅ S6.10 twin | same |

**Gap CLOSED 2026-08-17 (declared, narrowed and closed the same day):** the § *Record identity per kind* ruling is code for **every kind** — mail first (content-hash filing, scoped dedup, per-kind boot reset, the S4 mail reseal arm retired), then calendar + card (the stored `record_cid` on the DAV rows, the whole S4 module deleted), then conv last (the shared `fauna_mls::segments::encode_record` mint on both sides of the wire, `derive_record_id` deleted, the restore-from-disk identity guard). `RECORD_IDENTITY_CUTOVER_KINDS` covered mail/calendar/card/conv (retired with the cutover pass at the 2026-09-24 genesis) and `ADOPTABLE_KINDS = ["post", "mail", "calendar", "card", "conv"]`. The transition per kind was the **user-approved alpha reset**, not a migration (§ *Record identity per kind* → *Transition superseded*). **The last serve-plane bound CLOSED 2026-08-18 (ruled and built the same day):** conv joined `segment_manager_for_kind` under the member-mint contract (§ *Which kinds the two planes serve* — no enumeration door, explicit-list member-mint authorization with per-request membership re-derivation, home-nest coverage bound), so **every adoptable kind is now served**. The build touched all four seams the ruling named: the gate arm, the co-authored admission (`custody_admission::custody_admits_co_authored_scope`), the pump's scope-id addressing (`binding_for` now derives the fetch address from the scope itself), and the store-side admit expectation (the pair is checked against the scope it is filed under, so a conv pair's channel-keyed sidecar admits). tier_3: `conformance_custody_nest_door_client::the_pump_adopts_a_channels_conv_segments_under_the_member_mint_rule`.

**Per-actor export of segment pairs (2026-08-17):** the account export reaches
this store behind `include_blobs` — ruling + mechanism owned by
[`account-data-plane.md`](account-data-plane.md) § Nest-side requirements
item 1, the *Payload stores* ruling.

Landed substrate, one line each:

- **Plan 6 composition lift** (2026-05-22): kind-agnostic `SegmentManager` +
  kind-tagged `Manifest`; per-kind coordination lives in
  `bins/fauna-nest/src/segments/{mail,conv,cal,card,post}.rs`; renames
  `MailBackupCoordinator`→`BackupCoordinator`, `MailCompactionWorker`→`CompactionWorker`,
  `is_pure_backup_destination_for_mail(actor)`→`is_pure_backup_destination(kind, scope_id)`.
  **Extended 2026-09-01:** the coordination layer's own kind-agnostic mechanics
  are shared in `segments/mod.rs` rather than written out per kind, the shape
  `records_db` already used one layer down (a kind-parameterized core plus thin
  named per-kind wrappers, "which exist to name the kind, not to duplicate the
  statement"). Three cores: `compact_bucket_with` — the bucket-rewrite skeleton
  (live-set pre-fetch → candidate id → filtered rewrite → mirror transaction),
  which **all five** kinds' `compact_bucket` now delegate to, passing only a
  floor-decode closure and their named `records_db::apply_*_compaction_tx`;
  `live_record_cids` and `read_point_read_record`, shared by the two point-read
  DAV kinds. What stays per-kind is what genuinely differs: the typed
  envelope/floor encode and decode, and — deliberately at the call site, since
  burying it is how it gets copied wrong — the floor's **timestamp unit**
  (mail/conv/post are epoch milliseconds and divide by 1000 before `bucket_for`;
  calendar/card are seconds and must not).
  **Extended again 2026-09-01 (the placement plane):** the same rule reached
  `bins/fauna-nest/src/segments/{mail,cal,card}_placement.rs`, which — unlike
  the coordination layer's ragged parallel — were a clean *three*-kind one: the
  same nine manager methods in the same order over the same per-actor `DashMap`
  + `Mutex`. They are now one `PlacementJournal<K>` in `segments/mod.rs`, the
  three `*PlacementSegmentManager` names kept as aliases so call sites still
  say which journal they mean, with `PlacementKind` carrying the whole of what
  differs. Three more kind-agnostic cores land beside it: the manifest rebuild
  (`replay_placement_journal`, taking the journal label from
  `VersionedManifest::LABEL` rather than three hand-written string literals),
  the boot heal's shell (`heal_v1_placement_manifests`, taking the
  already-healed predicate from `VersionedManifest::CURRENT_VERSION` rather
  than each kind's own `*_PLACEMENT_FORMAT_VERSION` re-spelling), and the three
  helpers `compute_placement_cid` / `placement_now_secs` / `list_actor_dirs`
  (the last of which was already shared — it just lived in `cal_placement` with
  its two siblings reaching across into it). The fail-closed decisions this
  consolidates were byte-identical in all three copies, so there is no per-kind
  refusal that could be weakened by sharing: `actor_state`'s
  SchemaMismatch-refuses / Encoding-rebuilds split, and `open_replay_segment`'s
  corruption-vs-crash-tail split beneath it, are one policy applied once.
  (The boot-heal shell and `list_actor_dirs` retired 2026-09-24 with the v1
  heal itself — the compat-remnant sweep, [`compat-remnant-sweep.md`](compat-remnant-sweep.md) § Program 4.)

  **What stays per-kind in the placement plane, and why** (the same question
  the coordination layer answered above):

  - **The record semantics** — `apply_record_to_manifest`,
    `apply_put_*` / `apply_delete_*`, `update_modseq`. Five, five and ten record variants over different manifest
    state, keyed by `&str` (mail's mailbox name) against `[u8; 32]` (the two DAV
    kinds' collection id). Exactly one of them reaches the shared manager, as
    the single `PlacementKind::apply_record` method.
  - *(Retired 2026-09-24 with the v1 boot heal, by the compat-remnant sweep:
    the heal's per-kind SQLite join, which refilled the fields the v1 journal
    never recorded.)*
  - ⚠ **The path helpers — because a kind has TWO on-disk name spellings and
    they are not the same string.** Calendar's directory is
    `__calendar-placement` (`PlacementKind::SCOPE_KIND`, also the name
    `is_high_cadence_reserved_kind` tests for and the one park reports log)
    while its manifest label is `cal-placement` (`VersionedManifest::LABEL`,
    on-disk through both the manifest filename and the atomic-save tmp
    extension). Card and mail happen to spell the two the same, which is
    exactly what makes calendar's split easy to miss. Deriving either spelling
    from the other would rename a live journal directory out from under every
    existing actor — the one "obvious unification" in this plane that is a
    data-loss bug, now a documented trait const rather than scattered literals.
- **CARv2 + sidecar format** (Layer 3, 2026-05-17): `.dat` is standard CARv2
  via `libs/fauna-carv2` (go-car/v2 byte-parity oracle-proven); `.meta` is
  canonical dag-cbor `SegmentSidecar`; pre-Layer-3 BARE framing fully removed.
- **Canonical dag-cbor everywhere** (Layer 6): every per-kind
  envelope/floor/placement type flipped serde_bare → canonical dag-cbor;
  `serde_bare` dropped from `fauna-mail`, `fauna-calendar`, `fauna-mls`
  (`serialization.md:29`).
- **Mirror at target shape**: `scope_id` (was `actor_id`), full 36-byte
  `record_cid` key, `byte_offset`/`byte_length` dropped — IMAP size/quota/SEARCH
  derive size from the CARv2 index at the handler layer
  (`segments::mail::record_size(s)`; design ratified in
  [`../behavior/imap-server.md`](../behavior/imap-server.md) §§ SEARCH, QUOTA;
  per-call cost is O(segments-touched) opens + cheap index probes).
- **Reclaim on delete/supersede (S6.8a, 2026-07-09)**: calendar/card row
  deletes, PUT-supersedes, and the addressbook cascade tombstone the content
  record via `records_db::tombstone_by_cid`, atomic with the row write, once no
  live row of the actor still references it (invariant 4).
- **Placement-journal format v2 (S6.9 bump, 2026-07-10)**: `PutEvent`/`PutCard`
  carry the content-record id + the row's effective `encrypted_fauna_ext`;
  `DeleteEvent`/`DeleteCard` carry the record id + `deleted_at` (what makes the
  tombstone retention prune possible — pruned on DELETE below the same
  effective `tombstone_retention_days.max(7)` window the sync serve path
  enforces). A corrupt placement manifest now **rebuilds by replaying the
  journal** instead of failing every append. The v1 read path this bump
  shipped — frozen `v1` record/manifest shapes read through an upgrade, the
  `Option` placement/tombstone fields that carried what v1 never recorded, a
  boot heal that refilled them from the live SQLite rows, and restore's
  `(collection, uid_hash)` floor-join fallback for an id-less entry — was
  retired 2026-09-24 by the compat-remnant sweep ([`compat-remnant-sweep.md`](compat-remnant-sweep.md) § Program 4):
  no pre-sweep manifest or journal exists anywhere, so a v1 manifest or
  journal frame is now refused, and every placement entry and DAV tombstone
  carries its record id (and a DAV tombstone its delete time). The one-number
  version ladder itself stays, at its baseline.
- **Corruption recovery on all three placement journals (2026-08-23)**: the
  rebuild sentence above was written for the calendar/card journals and was
  silently false for **mail** — the kind the other two were copied from —
  until this date. `MailPlacementSegmentManager::actor_state` propagated every
  manifest `load` error, so an undecodable `manifest.mail-placement` failed
  every append, read and prune for that actor permanently, fixable only off-box
  (`nest/common.md` § Client-state recoverability). All three now implement the
  two arms `VersionedManifest::load` documents: `SchemaMismatch` (intact file,
  unknown version — *this binary is too old*) refuses loudly, and `Encoding`
  (damaged file) replays the journal and persists the rebuilt manifest. Mail's
  replay needed the frozen v1 **record** shape its module had omitted precisely
  because nothing replayed its journal; that shape and
  `VersionedMailPlacementRecord::decode_any` then existed beside the cal/card
  twins' (all retired 2026-09-24 by the compat-remnant sweep). Each arm is pinned per kind — the two tests only the calendar twin
  carried are now on all three.
- **The rebuild's segment door refuses too (2026-08-30)**: the replay above
  walks every segment, and it used to skip *any* segment that would not open
  with a `tracing::warn!` and a bare `continue`, on the reasoning that an
  unopenable segment is a crash between the last append and its `finalize()`.
  That reasoning covers exactly one of `FramedSegment::open`'s eight failure
  exits — the missing `.meta` sidecar, which is always the highest-id segment
  because ids are monotonic. The other seven (a damaged `.meta`, a damaged
  `.dat`, an I/O error on either, a drifted CARv2 block count, a corrupt
  `floor_metadata` length, and `SchemaMismatch`) are reachable on a **middle**
  segment, where the crash-tail reasoning is simply false — and skipping one
  drops its records from the rebuilt manifest, which is then persisted and
  decodes cleanly forever after, so nothing rebuilds again. For mail that is
  delivered mail going unreachable over IMAP and expunged mail coming back,
  silently and terminally (`principles.md` § No user-data loss), witnessed
  only by a log line the product has no operator to read (§ One configuration
  surface). So the replay now **fails closed** on every unopenable segment
  except the highest id present, matching the `?` the record-level decode
  beside it always used — less corruption failed closed while more corruption
  failed open. `SchemaMismatch` fails closed at **every** position, the tail
  included: it is the same variant the manifest door raises as a hard refusal
  immediately before falling into this rebuild, and rebuilding past it performs
  precisely the downgrade-strips-fields that arm exists to refuse. The policy
  is homed once (`bins/fauna-nest/src/segments/mod.rs::open_replay_segment`)
  and pinned per kind by three tests each — damaged middle refuses, crash tail
  still skips, newer-binary sidecar refuses at the tail. The refusal is
  actor-scoped and loud rather than silent and permanent; it is not a
  client-reachable state, so `nest/common.md` § Client-state recoverability is
  untouched — a corrupt segment file is storage misbehaving, and a loud refusal
  is repairable where a silent drop is not. The v1 boot heal was per-actor
  fault-isolated in the same change, so one unreplayable actor no longer
  stopped the remaining actors from healing (the heal retired 2026-09-24).
- **All three placement journals are now literally one implementation
  (2026-09-01)**: the two bullets above each had to say "all three now do X"
  because X was three hand-copies that had drifted apart once already — mail
  reached 2026-08-23 without the rebuild its twins had, precisely because
  nothing about the copies made the omission visible. The manager and the
  replay (and, until its 2026-09-24 retirement, the boot-heal shell) are now
  single functions over `PlacementKind`
  (§ *Landed substrate* → Plan 6 composition lift, second extension), so a
  fail-closed rule added to one kind cannot be missing from another: there is
  no other kind to add it to. What genuinely differs per kind — the record
  semantics and the two on-disk name spellings — is enumerated there.
- **Sealed-at-rest both modes (Phase-3 S1–S3, 2026-07-08)**: record payloads
  seal at every ingest perimeter in both storage modes (the mode axis itself
  was retired later, at Phase 4 — `nest/storage-modes.md`); owner of the
  at-rest shape is [`encryption-at-rest.md`](encryption-at-rest.md) §
  Readable classes.
- **HTTP residue**: `GET|PUT /api/v1/blob/{cid_b32}` is the canonical
  byte-source; the `GET /api/v1/segments/{kind}/…` route is the permanent
  segment byte route (ruled 2026-10-01, `segment-backup-protocol.md` § Byte-source
  endpoint).
- **Built and live: continuation records** (§ Continuation records — a body
  larger than one record). Writer/reader/fan-out/reaper all landed (see
  § Continuation records → Implementation status); the mail single-record at-rest
  ceiling that was their interim placeholder is **retired** — `MAX_SEALED_BODY_AT_REST_BYTES`
  deleted, `max_message_bytes` alone binds (`mail-message-size.md` § Message size limits,
  ceiling retirement 2026-07-18).

### Invariants binding any kind whose body moves out of a SQLite column

Ratified with the calendar/card cutover (S6.6–S6.12); target-state rules, not history:

1. **Append before commit — and prove it.** The content record must be durable
   *before* the metadata row exists (a row committed first + an idempotent
   retry = silent, user-irrecoverable loss). Writes live at the handler layer;
   the write arms refuse a row unless the `segment_records` mirror proves its
   content record live. The reverse crash window is benign (an
   appended record with no row is unreachable).
2. **A content record holds only state functionally determined by its id.**
   Records are append-only under a fixed CID, so anything mutated in place
   against a fixed id (etag, modseq, `encrypted_fauna_ext`) must live in the
   **placement journal**, and snapshot restore sources it from there — which
   is why every placement journal carries the content-record id.
3. **Nothing unsealed enters a content segment — enforced by type (S6.12).**
   `segments::{cal,card,mail}::append_record` accept payloads only as
   `fauna_mls::wrapped_blob::SealedRecordBytes` (sole constructor = the strict
   sealed-envelope decode); handlers mint the type at the wire edge. Verbatim
   at-rest carriers (relay append, S4 reseal, compaction/restore) operate on
   pre-encoded outer envelopes below the typed boundary; the tier_3 raw-inject
   hook is `test-helpers`-gated out of production builds.
4. **Delete the row, then tombstone the record** (S6.8a): a tombstoned record
   is reclaimable, so tombstoning under a live row is user-irrecoverable loss
   (the reverse window merely leaks an unreachable record); and never
   tombstone a record another live row of the actor still references — the
   record CID hashes only the sealed envelope while the row id also mixes in
   the timestamp, so a byte-identical re-PUT under a new timestamp, or the same
   sealed bytes in two of the actor's collections, share one record. The
   calendar/card DAOs count the actor's rows by `record_cid` (indexed) after
   the row DELETE and any superseding INSERT, and tombstone only at zero.
5. **The non-empty-column predicate is the back-fill completion marker** —
   deliberately no marker row (a second source of truth can read "done" while
   rows remain, permanently stranding a row from backup); and a body that is
   not a sealed record is skipped, never moved (raw content must not become
   backup-eligible).
6. **Once the back-fill has run its course, the emptied column goes.** A body
   column every writer leaves empty is dead schema, dropped in a schema step
   (calendar and card at nest schema 110), so no row can carry a second copy
   of a body that the segment scan never inspects.

Design notes that survive per kind: posts carry no `seq` and no per-record
envelope (`record_cid = Cid::of_dag_cbor(body)`, whose digest *is* the
`post_id`); calendar/card records file under their envelope CID like mail, and
the row stores it (`record_cid`) because the row's primary key cannot name it
(an `event_id`/`card_id` is `blake3(DST‖actor‖timestamp‖body)` — one record
can back several rows, invariant 4);
cards are sealed at ingest in both modes and have never rested raw, so the
kind has no seal-back-fill arm and must not grow one (its S6.7 back-fill moves
a pre-cutover *body* out of the column — a different pass from mail/calendar's
S4 *seal* back-fill, and the only one the card kind has).

Calendar and card reclaim is complete: their delete/supersede paths tombstone
the content record, the compaction worker carries their arms, and — because
their PUT handler appends the content record *before* the DAO can reject the
write — a per-kind **orphan reaper** tombstones any live mirror row whose
metadata row is absent, so compaction can reclaim it. The reaper is the one
place a record is tombstoned without its row having been deleted first, so it
is fenced three ways: it reads the live-row set and writes its tombstones under
a **single held connection lock** (a PUT's row INSERT cannot interleave); it
ignores rows younger than an age watermark (an in-flight PUT is
indistinguishable from an orphan); and it **fails closed when the actor has no
metadata rows at all**, which is a nest whose rows are not rebuilt yet rather
than a corpus of orphans. Consequently **anything that rebuilds metadata rows
from a manifest — snapshot restore — must be mutually exclusive with the
compaction worker's `"gc"` op lock**, or a restore caught between its mirror
rebuild and its row rebuild presents every record as orphaned.
