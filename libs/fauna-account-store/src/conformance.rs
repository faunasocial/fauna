//! The backend-generic **conformance suite** — one statement of the
//! [`StoreBackend`] contract, graded against every physical arm (charter:
//! `account-data-plane.md` § Store logical schema, *Physical realization*: web
//! implements the same logical schema behind the same trait).
//!
//! Every invariant that makes the store a store lives once, above the trait,
//! in [`AccountStore`] — so the one thing an arm can get wrong is the trait's
//! own promises: each method one transaction, the multi-table methods
//! (`*_put_with_row`, `record_added_with_row`, the `meta_*_all*` family,
//! `compact_retired_rows`) all-or-nothing, the orderings and max-merges the
//! doc comments state. These cases are the proof, written once:
//!
//! - [`cases`] and [`segment_cases`] — target-agnostic, run on every arm:
//!   natively over `SqliteBackend` and [`MemoryBackend`], and in a browser over
//!   web's IndexedDB arm through `wasm-bindgen-test`. The segment cases adopt
//!   the REAL writer's output, pinned as bytes ([`fixtures::PINNED_SEGMENTS`])
//!   because the writer (`fauna_segment_store`) builds natively only; a native
//!   test holds every pin to the writer's current bytes.
//!
//! An arm plugs in by implementing [`Medium`] and invoking
//! [`store_conformance_suite!`](crate::store_conformance_suite) with its test
//! attribute. Adding a case is one `pub async fn` in [`cases`] plus its name in
//! the macro's list — every arm then grades it, which is the point: an arm can
//! never silently skip a case the others pass.
//!
//! What stays out: properties of ONE medium (SQLite's `data_version`, its
//! migration lock, a pre-migrate refusal read off a raw connection, files on
//! disk) are that arm's own tests beside its code.
//!
//! Gated `cfg(any(test, feature = "test-helpers"))`: a test harness, never a
//! production surface.

use fauna_core::data::ContentHash;

use crate::backend::StoreBackend;
use crate::memory::MemoryBackend;
use crate::store::AccountStore;

/// A physical medium a conformance case can stand a store up on.
///
/// Two operations, because the suite needs two shapes of "the same store":
/// [`Medium::fresh`] is a medium no other case shares (a fresh temp dir, a
/// fresh in-memory table set, a fresh IndexedDB database name), and
/// [`Medium::open`] opens a handle on *this* medium — the first call creates
/// the store, a later one reopens it, which is what a restart (or a second
/// process on the same account) observes.
#[allow(async_fn_in_trait)] // static dispatch only, the StoreBackend precedent
pub trait Medium: Sized {
    type Backend: StoreBackend;

    /// A fresh, empty medium no other case shares.
    fn fresh() -> Self;

    /// A new handle on this medium. Panics on failure — a harness that
    /// cannot open its own medium has no verdict to report.
    async fn open(&self) -> Self::Backend;

    /// The file names in this medium's segment area, or `None` for a medium
    /// with no file area (memory) — what a case observes staging leftovers
    /// through. Read WITHOUT opening a backend: an open sweeps them.
    async fn segment_files(&self) -> Option<Vec<String>> {
        None
    }
}

/// The memory arm's medium: one shared table set, each [`Medium::open`] a
/// second handle on it ([`MemoryBackend::handle`]).
pub struct MemoryMedium(MemoryBackend);

impl Medium for MemoryMedium {
    type Backend = MemoryBackend;

    fn fresh() -> Self {
        Self(MemoryBackend::new())
    }

    async fn open(&self) -> MemoryBackend {
        self.0.handle()
    }
}

/// Instantiate every target-agnostic case as a test of `$medium`, each carrying
/// `$attr` (`tokio::test` natively, `wasm_bindgen_test::wasm_bindgen_test` in a
/// browser). The ONE list of case names — an arm grades all of them or none.
#[macro_export]
macro_rules! store_conformance_suite {
    ($medium:ty, $attr:meta) => {
        $crate::__store_conformance_emit!($medium, $attr, cases;
            the_meta_batch_calls_are_one_snapshot_and_all_or_nothing,
            the_version_pair_rises_per_key_and_compares_as_numbers,
            a_lost_seq_race_lands_no_state_entry_on_either_plane,
            an_entry_moved_since_the_read_refuses_the_pair_on_either_plane,
            a_second_handle_sees_the_first_handles_commits,
            relay_rows_are_swept_by_the_generation_their_header_names,
            relay_retire_shadowed_deletes_only_the_other_items_at_the_coordinate,
            relay_retire_at_deletes_every_item_at_the_coordinate_and_nothing_else,
            retired_compaction_refuses_the_current_writer_and_deletes_only_the_row_it_was_given,
            writer_relation_answers_for_the_store_not_the_handles_open_time_cache,
            writer_relation_falls_back_to_the_cache_only_where_a_fence_is_unrepresentable,
            a_relay_row_remembers_its_feed_seq_once_told,
            voiding_the_watermark_clears_every_feed_seq_of_its_scope,
            the_retire_record_keeps_the_newest_and_a_reader_clears_only_what_it_read,
            relay_rows_collapse_per_item_and_writer_and_never_regress,
            relay_rows_serve_past_frontier_as_ordered_prefixes_per_writer,
            relay_high_waters_are_the_seen_maximum_per_writer,
            the_listed_fact_is_per_scope_durable_and_absent_on_a_fresh_store,
            the_unkeyed_set_replaces_adds_and_keeps_its_bits_durably,
            the_let_go_set_only_grows_and_survives_a_reopen,
            the_parked_list_is_per_scope_and_writer_and_durable,
            the_nest_watermark_only_rises_and_is_per_scope,
            the_nest_watermark_is_keyed_by_the_replica_it_was_banked_from,
            a_departure_clears_that_scopes_watermark_and_no_other,
            the_custody_meter_counts_by_family_and_excludes_the_floor,
            eviction_drops_payload_oldest_first_and_never_the_floor,
            an_overage_made_of_floor_frees_what_it_can_and_stays_over,
            eviction_is_idempotent_and_never_over_frees,
            a_later_pull_rehydrates_an_evicted_row,
            states_of_kind_lists_live_entries_of_that_kind_only,
            staging_a_local_record_lands_block_index_and_journal_together,
            a_block_whose_bytes_do_not_match_its_cid_is_refused,
            the_index_is_the_always_present_layer,
            bytes_for_an_unknown_record_are_refused,
            hydration_policy_governs_dehydration,
            hydration_policy_round_trips_through_store_meta,
            a_scope_walk_is_stable_and_resumable,
            staging_the_same_record_twice_is_idempotent_in_content,
            a_lost_seq_race_writes_neither_the_index_row_nor_the_block,
            the_whole_block_plane_runs_key_less,
            local_appends_are_monotonic_and_gapless,
            local_append_skips_a_seq_taken_by_another_process,
            the_occupied_slot_is_never_overwritten,
            ingest_is_idempotent_and_refuses_equivocation,
            same_writer_seq_in_two_scopes_is_two_logs_not_equivocation,
            ingest_tolerates_gaps_from_origin_compaction,
            frontier_never_regresses_and_only_accounted_walks_advance_it,
            put_state_lands_entry_and_journal_row_together,
            a_fresh_store_stamps_the_version_pair_and_reopens,
            a_newer_breaking_store_refuses_to_open,
            a_newer_additive_store_opens_and_is_not_restamped_down,
            an_upgrade_in_place_raises_the_min_reader_floor,
            a_store_bound_to_another_identity_refuses_to_open,
            an_append_under_a_rotated_writer_refuses_typed,
            ingest_still_lands_under_a_rotated_writer,
            writer_rotation_verifies_its_predecessor_and_is_idempotent,
            the_fence_records_the_reauthor_marker_and_clears_on_demand,
            a_fence_landing_over_a_pending_marker_keeps_the_earlier_predecessor,
            the_successor_opens_and_appends_after_the_fence,
            coordinate_of_item_resolves_per_scope_from_the_introducing_row,
            a_tombstone_removes_the_record_from_the_index_and_the_block_plane,
            an_unknown_or_replayed_tombstone_is_a_no_op,
            bootstrap_refuses_an_always_kind_offer_under_an_ondemand_kind_scope_before_fetching,
            a_departure_empties_every_plane_of_that_scope,
            a_sibling_scopes_rows_survive_the_departure,
            the_departed_scopes_seen_set_entry_survives,
            dropping_an_unheld_scope_reports_zeros,
            an_intent_survives_a_restart_verbatim,
            re_appending_the_same_intent_is_a_no_op,
            an_ack_deletes_and_a_double_ack_is_a_no_op,
            fifo_holds_within_a_scope_and_scopes_are_independent,
            a_parked_intent_stays_and_attempts_are_counted,
            a_departure_proceeds_but_cannot_reach_the_outbox,
        );
        $crate::__store_conformance_emit!($medium, $attr, segment_cases;
            adopting_the_same_segment_twice_writes_nothing_the_second_time,
            a_foreign_actors_segment_never_reaches_the_store,
            a_segment_whose_kind_disagrees_with_the_scope_is_refused,
            a_lost_record_index_rebuilds_from_the_sidecars_record_order,
            a_transfer_interrupted_before_adoption_reopens_clean,
            a_segment_resident_block_refuses_to_dehydrate,
            a_segment_resident_records_tombstone_unindexes_it_without_punching_the_segment,
            bootstrap_adopts_a_hydrated_scopes_segments_in_bulk,
            a_dehydrating_replica_never_fetches_the_bulk_segments,
            bootstrap_refuses_a_segment_whose_sidecar_disagrees_with_its_own_offer,
            the_custody_meter_counts_adopted_segments_dat_evictable_meta_floor,
            segment_eviction_drops_whole_dat_files_oldest_first_and_keeps_the_metadata_floor,
            a_budgeted_bootstrap_stops_adopting_at_the_budget_and_never_refetches_a_held_segment,
        );
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __store_conformance_emit {
    ($medium:ty, $attr:meta, $module:ident; $($case:ident),* $(,)?) => {
        $(
            #[$attr]
            async fn $case() {
                $crate::conformance::$module::$case::<$medium>().await
            }
        )*
    };
}

/// The fixtures every case (and the arms' own tests) spell their rows with.
pub mod fixtures {
    use super::*;
    use crate::types::{IntentDrainer, ItemRef, StateEntry, WriterId};
    use crate::types::{JournalOp, JournalRow, NewOutboxIntent, RecordIndexEntry, RelayRow};

    pub fn writer(byte: u8) -> WriterId {
        WriterId([byte; 32])
    }

    pub fn cid(tag: &str) -> ContentHash {
        ContentHash::of_raw(tag.as_bytes())
    }

    /// A canonical post content scope (`content:<kind>:<scope-id-hex>`, the
    /// ratified encoding — `account-sync-plane.md` § Feeds and cursors →
    /// *The scope string*). Spelled as a literal because this crate
    /// deliberately holds no protocol dep; the spelling itself is pinned by
    /// `fauna_protocol::scope`'s own tests.
    pub fn post_scope() -> String {
        format!("content:post:{}", "1a".repeat(32))
    }

    /// Its mail twin. The `__`-prefixed *directory* name is not the kind tag
    /// (ruling: the segment-store § Layout table is the only authority), and
    /// `fauna_protocol::scope` pins that mis-spelling as a refusal — so these
    /// fixtures spell the canonical form even though the store itself holds a
    /// scope string opaque.
    pub fn mail_scope() -> String {
        format!("content:mail:{}", "2b".repeat(32))
    }

    pub fn entry(kind: &str, key: &str, value: &[u8]) -> StateEntry {
        StateEntry {
            kind: kind.into(),
            key: key.into(),
            scope: "state".into(), // ACCOUNT_STATE_SCOPE — the ratified spelling
            value: value.to_vec(),
            merge_meta: None,
            entry_version: 0, // store-assigned; ignored on input
            tombstone: false,
        }
    }

    pub fn relay_row(writer_byte: u8, seq: u64, item: &[u8], entry: &[u8]) -> RelayRow {
        RelayRow {
            scope: "state".into(),
            item_class: "state-entry".into(),
            writer: writer(writer_byte),
            writer_seq: seq,
            item_key: item.to_vec(),
            op: "state-put".into(),
            entry: Some(entry.to_vec()),
            feed_seq: None,
        }
    }

    pub fn relay_row_op(
        writer_byte: u8,
        seq: u64,
        item: &[u8],
        entry: &[u8],
        op: &str,
    ) -> RelayRow {
        RelayRow {
            op: op.into(),
            ..relay_row(writer_byte, seq, item, entry)
        }
    }

    pub fn intent(id_byte: u8, scope: &str, payload: &[u8]) -> NewOutboxIntent {
        NewOutboxIntent {
            intent_id: [id_byte; 16],
            kind: "fauna.contacts.knock".into(),
            scope: scope.to_string(),
            payload: payload.to_vec(),
            drainer: IntentDrainer::Rpc,
        }
    }

    /// A store on a fresh medium, as actor `aa11` / writer 7. The medium is
    /// returned so it outlives the store (a temp dir must not vanish under
    /// an open database).
    pub async fn store<M: Medium>() -> (M, AccountStore<M::Backend>) {
        let medium = M::fresh();
        let store = AccountStore::open(medium.open().await, "aa11", writer(7))
            .await
            .unwrap();
        (medium, store)
    }

    /// A bare backend on a fresh medium — for the cases that must see the
    /// trait below the store, or a store that was never opened.
    pub async fn fresh_backend<M: Medium>() -> (M, M::Backend) {
        let medium = M::fresh();
        let backend = medium.open().await;
        (medium, backend)
    }

    /// The segment fixtures' actor, as the 32 bytes a sidecar carries — and
    /// [`seg_actor_hex`] as the 64-hex string `AccountStore::open` takes.
    pub const SEG_ACTOR: [u8; 32] = [0xab; 32];

    pub fn seg_actor_hex() -> String {
        fauna_core::format::hex_full(&SEG_ACTOR)
    }

    /// The adoption-true spellings: scopes whose id IS the segment fixture's
    /// scope key (`SEG_ACTOR`) — the only shape production ever files, and
    /// what `adopt_segment` checks the pair against (the pair must belong to
    /// the scope it is filed under, not to this store's actor).
    /// `post_scope()`/`mail_scope()` keep their unrelated ids for the
    /// journal-plane cases, where a scope string is opaque.
    pub fn seg_post_scope() -> String {
        format!("content:post:{}", seg_actor_hex())
    }

    pub fn seg_mail_scope() -> String {
        format!("content:mail:{}", seg_actor_hex())
    }

    /// A store on `medium` as the segment fixtures' actor.
    pub async fn store_on<M: Medium>(medium: &M) -> AccountStore<M::Backend> {
        AccountStore::open(medium.open().await, &seg_actor_hex(), writer(7))
            .await
            .unwrap()
    }

    /// Fill `scope` on every plane a departure has to reach, so a case can
    /// assert emptiness rather than "the one table I remembered".
    pub async fn populate_scope<B: StoreBackend>(s: &AccountStore<B>, scope: &str, tag: &str) {
        let c = cid(tag);
        s.stage_local_record(scope, "post", format!("body of {tag}").as_bytes())
            .await
            .unwrap();
        s.note_record(&RecordIndexEntry {
            cid: c,
            scope: scope.into(),
            kind: "post".into(),
            size: Some(3),
        })
        .await
        .unwrap();
        s.put_block(&c, tag.as_bytes()).await.unwrap();
        // A foreign writer's row, so the frontier advance below is accounted —
        // the shape a walked-in record actually has.
        s.ingest_row(&JournalRow {
            writer: writer(9),
            seq: 4,
            scope: scope.into(),
            op: JournalOp::RecordAdded,
            item: ItemRef::Cid(c),
        })
        .await
        .unwrap();
        s.advance_frontier(scope, &writer(9), 4).await.unwrap();
        s.record_relay_row(&RelayRow {
            scope: scope.into(),
            item_class: "record-cid".into(),
            writer: writer(9),
            writer_seq: 4,
            item_key: c.as_bytes().to_vec(),
            op: "record-added".into(),
            entry: None,
            feed_seq: None,
        })
        .await
        .unwrap();
    }

    /// One segment pair the REAL writer produced, pinned as bytes so every
    /// target — a browser included — adopts exactly what the nest writes.
    /// `crate::conformance`'s native `pinned_segments_are_the_writers_bytes`
    /// holds each pin byte-equal to the writer's current output, so a pin can
    /// never drift from the format it stands for.
    pub struct PinnedSegment {
        pub name: &'static str,
        pub kind: &'static str,
        pub segment_id: u32,
        pub actor: [u8; 32],
        pub bodies: &'static [&'static [u8]],
        pub dat: &'static [u8],
        pub meta: &'static [u8],
    }

    macro_rules! pinned {
        ($name:literal, $kind:literal, $id:literal, $actor:expr, [$($body:literal),* $(,)?]) => {
            PinnedSegment {
                name: $name,
                kind: $kind,
                segment_id: $id,
                actor: $actor,
                bodies: &[$($body as &[u8]),*],
                dat: include_bytes!(concat!("../tests/fixtures/segments/", $name, ".dat")),
                meta: include_bytes!(concat!("../tests/fixtures/segments/", $name, ".meta")),
            }
        };
    }

    /// Every segment fixture a conformance case adopts. A new case needing a
    /// new pair adds a row here, creates the two (empty) files, and blesses
    /// them: `FAUNA_BLESS_SEGMENT_FIXTURES=1 cargo test -p fauna-account-store
    /// --lib pinned_segments_are_the_writers_bytes`.
    pub static PINNED_SEGMENTS: &[PinnedSegment] = &[
        pinned!("post-1-one-record", "post", 1, SEG_ACTOR, [b"one record"]),
        pinned!("post-1-foreign-actor", "post", 1, [0x11; 32], [b"not ours"]),
        pinned!(
            "mail-1-mislabelled",
            "mail",
            1,
            SEG_ACTOR,
            [b"mislabelled mail record"]
        ),
        pinned!(
            "mail-9-two-records",
            "mail",
            9,
            SEG_ACTOR,
            [b"mail record A", b"mail record B"]
        ),
        pinned!(
            "mail-2-bulky",
            "mail",
            2,
            SEG_ACTOR,
            [b"a bulky mail record"]
        ),
        pinned!(
            "mail-3-later-deleted",
            "mail",
            3,
            SEG_ACTOR,
            [b"a mail record later deleted"]
        ),
        pinned!("post-1-p1-p2", "post", 1, SEG_ACTOR, [b"p1", b"p2"]),
        pinned!("post-2-p3", "post", 2, SEG_ACTOR, [b"p3"]),
        pinned!(
            "mail-1-bulky",
            "mail",
            1,
            SEG_ACTOR,
            [b"a bulky mail record"]
        ),
        pinned!("post-2-small-post", "post", 2, SEG_ACTOR, [b"a small post"]),
        pinned!(
            "mail-1-mislabelled-offer",
            "mail",
            1,
            SEG_ACTOR,
            [b"a mislabelled offer"]
        ),
    ];

    /// The real writer's `(dat, meta)` pair for these inputs — read from the
    /// pins, so it works on every target. Panics for inputs nobody pinned (add
    /// a [`PINNED_SEGMENTS`] row).
    pub fn real_segment(
        kind: &str,
        segment_id: u32,
        actor: [u8; 32],
        bodies: &[&[u8]],
    ) -> (Vec<u8>, Vec<u8>) {
        let pin = PINNED_SEGMENTS
            .iter()
            .find(|p| {
                p.kind == kind
                    && p.segment_id == segment_id
                    && p.actor == actor
                    && p.bodies == bodies
            })
            .unwrap_or_else(|| {
                panic!(
                    "no pinned segment fixture for {kind} #{segment_id} — add a PINNED_SEGMENTS row"
                )
            });
        (pin.dat.to_vec(), pin.meta.to_vec())
    }

    /// Write a real finalized segment pair with the REAL writer
    /// (`fauna_segment_store::FramedSegment`, a native test-only dependency)
    /// and return its `(dat, meta)` bytes: the store is a reader of a format
    /// another crate owns, so a hand-rolled imitation would only prove this
    /// crate self-consistent. What every pin is held to.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn write_segment(
        kind: &str,
        segment_id: u32,
        actor: [u8; 32],
        bodies: &[&[u8]],
    ) -> (Vec<u8>, Vec<u8>) {
        use fauna_segment_store::segment::{FramedSegment, SegmentHeader};

        let dir = tempfile::tempdir().unwrap();
        let dat_path = dir.path().join(format!("seg-{segment_id:08}.dat"));
        let mut seg = FramedSegment::create(
            &dat_path,
            SegmentHeader {
                kind: kind.to_string(),
                actor_id: actor,
                segment_id,
                bucket: "2026-08".to_string(),
                created_at_secs: 1_754_000_000,
                record_count: 0,
            },
        )
        .unwrap();
        for body in bodies {
            seg.append_record(ContentHash::of_dag_cbor(body), body, b"")
                .unwrap();
        }
        seg.finalize().unwrap();
        (
            std::fs::read(&dat_path).unwrap(),
            std::fs::read(dat_path.with_extension("meta")).unwrap(),
        )
    }
}

/// Every pin is the writer's current output, byte for byte — or, with
/// `FAUNA_BLESS_SEGMENT_FIXTURES` set, becomes it (see [`fixtures::PINNED_SEGMENTS`]).
#[cfg(all(test, not(target_arch = "wasm32")))]
#[test]
fn pinned_segments_are_the_writers_bytes() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/segments");
    let bless = std::env::var_os("FAUNA_BLESS_SEGMENT_FIXTURES").is_some();
    for pin in fixtures::PINNED_SEGMENTS {
        let (dat, meta) = fixtures::write_segment(pin.kind, pin.segment_id, pin.actor, pin.bodies);
        if bless {
            std::fs::write(dir.join(format!("{}.dat", pin.name)), &dat).unwrap();
            std::fs::write(dir.join(format!("{}.meta", pin.name)), &meta).unwrap();
            continue;
        }
        assert!(
            pin.dat == dat.as_slice() && pin.meta == meta.as_slice(),
            "pinned segment {} drifted from the writer's output — re-bless it \
             (FAUNA_BLESS_SEGMENT_FIXTURES=1) and review what the format change means \
             for stores already holding adopted segments",
            pin.name
        );
    }
}

/// A bootstrap source over pre-built segments, counting what it was asked for
/// — so a case can assert that a skipped kind was never *fetched*, not merely
/// never adopted.
pub struct FakeSource {
    pub segments: Vec<(crate::store::SegmentOffer, Vec<u8>, Vec<u8>)>,
    pub fetched: std::cell::RefCell<Vec<u32>>,
}

impl crate::store::BootstrapSource for FakeSource {
    async fn list_segments(&self, _scope: &str) -> anyhow::Result<Vec<crate::store::SegmentOffer>> {
        Ok(self.segments.iter().map(|(o, _, _)| o.clone()).collect())
    }

    async fn fetch_segment(
        &self,
        _scope: &str,
        offer: &crate::store::SegmentOffer,
        max_bytes: u64,
        into: &mut impl crate::segments::SegmentSink,
    ) -> anyhow::Result<()> {
        self.fetched.borrow_mut().push(offer.segment_id);
        let (_, dat, meta) = self
            .segments
            .iter()
            .find(|(o, _, _)| o.segment_id == offer.segment_id)
            .expect("offered");
        // The trait's bound, as a network source keeps it: a half longer than
        // `max_bytes` is refused, never handed over.
        if dat.len() as u64 > max_bytes || meta.len() as u64 > max_bytes {
            anyhow::bail!(
                "segment {} exceeds the {max_bytes}-byte fetch bound",
                offer.segment_id
            );
        }
        // In pieces, as a network source hands them over: the staging slot
        // must reassemble a half from its chunks, not receive it whole.
        for chunk in dat.chunks(7) {
            into.write(crate::segments::SegmentHalf::Dat, chunk).await?;
        }
        for chunk in meta.chunks(7) {
            into.write(crate::segments::SegmentHalf::Meta, chunk)
                .await?;
        }
        Ok(())
    }
}

pub fn fake_source(specs: &[(&str, u32, &[&[u8]])]) -> FakeSource {
    FakeSource {
        segments: specs
            .iter()
            .map(|(kind, id, bodies)| {
                let (dat, meta) = fixtures::real_segment(kind, *id, fixtures::SEG_ACTOR, bodies);
                (
                    crate::store::SegmentOffer {
                        kind: (*kind).to_string(),
                        segment_id: *id,
                        dat_size: Some(dat.len() as u64),
                    },
                    dat,
                    meta,
                )
            })
            .collect(),
        fetched: std::cell::RefCell::new(Vec::new()),
    }
}

/// The target-agnostic cases — every arm, every target.
pub mod cases {
    use super::fixtures::*;
    use super::*;
    use crate::store::*;
    use crate::types::*;

    // ── The trait's own multi-table promises, stated at the seam ────────────

    /// The `meta_*_all*` family: a multi-get is one picture in the order
    /// asked (absent keys `None`), a multi-put lands every pair, and the
    /// compare-and-delete deletes all named keys when — and only when — every
    /// one still holds what the caller saw, reporting a mismatch as `false`
    /// with nothing changed.
    pub async fn the_meta_batch_calls_are_one_snapshot_and_all_or_nothing<M: Medium>() {
        let (_medium, b) = fresh_backend::<M>().await;
        b.meta_put_all(&[("a", b"1"), ("b", b"2")]).await.unwrap();
        assert_eq!(
            b.meta_get_all(&["b", "absent", "a"]).await.unwrap(),
            vec![Some(b"2".to_vec()), None, Some(b"1".to_vec())],
            "one picture, in the order asked"
        );
        assert!(
            !b.meta_delete_all_if_unchanged(&[("a", Some(b"1")), ("b", Some(b"stale"))])
                .await
                .unwrap(),
            "a single stale expectation refuses the whole delete"
        );
        assert_eq!(
            b.meta_get_all(&["a", "b"]).await.unwrap(),
            vec![Some(b"1".to_vec()), Some(b"2".to_vec())],
            "and the refusal deleted nothing"
        );
        assert!(
            b.meta_delete_all_if_unchanged(&[("a", Some(b"1")), ("b", Some(b"2")), ("c", None)])
                .await
                .unwrap(),
            "a key seen absent matches an absent key"
        );
        assert_eq!(b.meta_get_all(&["a", "b"]).await.unwrap(), vec![None, None]);
    }

    /// `meta_put_pair_max`: each key only ever rises, independently, and the
    /// compare is numeric over the ASCII-decimal encoding — a bytewise
    /// compare would order `"10"` below `"9"` and pin the pair low.
    pub async fn the_version_pair_rises_per_key_and_compares_as_numbers<M: Medium>() {
        let (_medium, b) = fresh_backend::<M>().await;
        b.meta_put_pair_max(("v", 9), ("min", 10)).await.unwrap();
        b.meta_put_pair_max(("v", 10), ("min", 2)).await.unwrap();
        assert_eq!(
            b.meta_get_all(&["v", "min"]).await.unwrap(),
            vec![Some(b"10".to_vec()), Some(b"10".to_vec())],
            "v rose past 9 as a number; min refused to fall"
        );
    }

    /// The entry/journal pair on both planes: an occupied journal slot
    /// refuses the pair, and the entry upsert never lands without its row.
    pub async fn a_lost_seq_race_lands_no_state_entry_on_either_plane<M: Medium>() {
        let (_medium, b) = fresh_backend::<M>().await;
        let squatter = JournalRow {
            writer: writer(7),
            seq: 1,
            scope: "state".into(),
            op: JournalOp::RecordAdded,
            item: ItemRef::Cid(cid("squatter")),
        };
        assert_eq!(
            b.insert_row(&squatter, None).await.unwrap(),
            InsertOutcome::Inserted
        );
        let contested = |kind: &str| JournalRow {
            op: JournalOp::StatePut,
            item: ItemRef::StateKey {
                kind: kind.into(),
                key: "self".into(),
                entry_version: 1,
            },
            ..squatter.clone()
        };
        let account = StateEntry {
            entry_version: 1,
            ..entry("k", "self", b"v")
        };
        assert_eq!(
            b.state_put_with_row(&account, &contested("k"), None)
                .await
                .unwrap(),
            InsertOutcome::OccupiedByDifferent
        );
        assert_eq!(b.state_get("k", "self").await.unwrap(), None);

        let group = StateEntry {
            scope: "state".into(),
            kind: "g".into(),
            ..account.clone()
        };
        assert_eq!(
            b.group_state_put_with_row(&group, &contested("g"), None)
                .await
                .unwrap(),
            InsertOutcome::OccupiedByDifferent
        );
        assert_eq!(b.group_state_get("state", "g", "self").await.unwrap(), None);
        assert!(b.group_states_for_scope("state").await.unwrap().is_empty());
    }

    /// The entry/journal pair on both planes, between two instances on one
    /// store: a write whose version is not the stored version plus one — the
    /// other instance moved the entry after this one read it — answers
    /// `EntryMoved` and lands neither half, so one version of an entry never
    /// names two values under two journal rows.
    pub async fn an_entry_moved_since_the_read_refuses_the_pair_on_either_plane<M: Medium>() {
        let medium = M::fresh();
        let (first, second) = (medium.open().await, medium.open().await);
        let row = |kind: &str, seq: u64, entry_version: u64| JournalRow {
            writer: writer(7),
            seq,
            scope: "state".into(),
            op: JournalOp::StatePut,
            item: ItemRef::StateKey {
                kind: kind.into(),
                key: "self".into(),
                entry_version,
            },
        };
        let account = |entry_version: u64, value: &[u8]| StateEntry {
            entry_version,
            ..entry("k", "self", value)
        };
        let group = |entry_version: u64, value: &[u8]| StateEntry {
            scope: "state".into(),
            kind: "g".into(),
            ..account(entry_version, value)
        };

        // Both instances read "no entry"; the first lands version 1.
        assert_eq!(
            first
                .state_put_with_row(&account(1, b"first"), &row("k", 1, 1), None)
                .await
                .unwrap(),
            InsertOutcome::Inserted
        );
        assert_eq!(
            first
                .group_state_put_with_row(&group(1, b"first"), &row("g", 2, 1), None)
                .await
                .unwrap(),
            InsertOutcome::Inserted
        );
        // The second, still holding the read it made before that commit, and
        // a write that would skip a version: both refused whole.
        for stale in [1, 3] {
            assert_eq!(
                second
                    .state_put_with_row(&account(stale, b"second"), &row("k", 3, stale), None)
                    .await
                    .unwrap(),
                InsertOutcome::EntryMoved,
                "account plane, version {stale} over a stored 1"
            );
            assert_eq!(
                second
                    .group_state_put_with_row(&group(stale, b"second"), &row("g", 3, stale), None)
                    .await
                    .unwrap(),
                InsertOutcome::EntryMoved,
                "group plane, version {stale} over a stored 1"
            );
        }
        assert_eq!(
            second.state_get("k", "self").await.unwrap().unwrap().value,
            b"first"
        );
        assert_eq!(
            second
                .group_state_get("state", "g", "self")
                .await
                .unwrap()
                .unwrap()
                .value,
            b"first"
        );
        assert_eq!(
            second.max_writer_seq(&writer(7)).await.unwrap(),
            Some(2),
            "no journal row landed for a refused entry"
        );

        // Re-read, one above the stored version: lands.
        assert_eq!(
            second
                .state_put_with_row(&account(2, b"second"), &row("k", 3, 2), None)
                .await
                .unwrap(),
            InsertOutcome::Inserted
        );
        assert_eq!(
            second
                .group_state_put_with_row(&group(2, b"second"), &row("g", 4, 2), None)
                .await
                .unwrap(),
            InsertOutcome::Inserted
        );
        assert_eq!(
            first.state_get("k", "self").await.unwrap().unwrap().value,
            b"second"
        );
    }

    /// Two handles on one medium are two connections to one store: what one
    /// commits, the other reads — the multi-instance posture every arm owes
    /// (charter § Multi-instance concurrency), and the property the reopen
    /// cases below lean on.
    pub async fn a_second_handle_sees_the_first_handles_commits<M: Medium>() {
        let medium = M::fresh();
        let (first, second) = (medium.open().await, medium.open().await);
        first.meta_put("k", b"from-first").await.unwrap();
        assert_eq!(
            second.meta_get("k").await.unwrap().as_deref(),
            Some(&b"from-first"[..])
        );
        second.frontier_raise("s", &writer(1), 4).await.unwrap();
        assert_eq!(first.frontier("s").await.unwrap(), vec![(writer(1), 4)]);
    }

    /// **A shredded generation's relay residue is found by index.** A form-v2
    /// row is stamped with the generation its cleartext header names, a v1
    /// row is not; the stamp survives payload eviction; the sweep drops
    /// exactly the named generation's rows, whoever wrote them.
    pub async fn relay_rows_are_swept_by_the_generation_their_header_names<M: Medium>() {
        const SCOPE: &str = "state-fleet";
        let (_medium, backend) = fresh_backend::<M>().await;
        let (g, other) = ([0x61u8; 32], [0x62u8; 32]);
        let relay = |writer: u8, item: u8, entry: Vec<u8>| RelayRow {
            scope: SCOPE.into(),
            item_class: "state-entry".into(),
            writer: WriterId([writer; 32]),
            writer_seq: 1,
            item_key: vec![item; 32],
            op: "state-put".into(),
            entry: Some(entry),
            feed_seq: None,
        };
        // Form v2: the form byte, then the 32-byte generation id in clear.
        let v2 = |generation: &[u8; 32]| [&[2u8][..], generation, &[0xEE; 40]].concat();
        for r in [
            relay(1, 1, v2(&g)),
            relay(2, 2, v2(&g)),
            relay(1, 3, v2(&other)),
            relay(1, 4, [&[1u8][..], &[0x61; 64]].concat()),
        ] {
            backend.relay_put(&r).await.unwrap();
        }
        let mut held = backend.relay_generations(SCOPE).await.unwrap();
        held.sort_unstable();
        assert_eq!(held, vec![g, other]);
        // The let-go's read names every writer's row under one generation by
        // its coordinates, and nothing under another or under none.
        let mut under: Vec<(u8, u8)> = backend
            .relay_rows_sealed_under(SCOPE, &g)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.item_key[0], r.writer.0[0]))
            .collect();
        under.sort_unstable();
        assert_eq!(under, vec![(1, 1), (2, 2)]);
        assert!(
            backend
                .relay_rows_sealed_under(SCOPE, &[0x5A; 32])
                .await
                .unwrap()
                .is_empty()
        );

        // Eviction clears the payload, never the stamp.
        backend
            .relay_evict_payload(SCOPE, "state-entry", u64::MAX, &[])
            .await
            .unwrap();
        assert_eq!(
            backend.relay_forget_sealed_under(SCOPE, &g).await.unwrap(),
            2,
            "both writers' rows under the generation go"
        );
        let mut left: Vec<Vec<u8>> = backend
            .relay_rows(SCOPE, "state-entry", &[], u32::MAX)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.item_key)
            .collect();
        left.sort_unstable();
        assert_eq!(left, vec![vec![3u8; 32], vec![4u8; 32]]);
        assert_eq!(backend.relay_generations(SCOPE).await.unwrap(), vec![other]);
    }

    /// The store-level half of the format law: a stored pair whose floor this
    /// binary cannot meet refuses the open, typed, and stamps no identity.
    /// (The SQLite arm's pre-migrate half — nothing written even by the
    /// backend's own open — is that arm's own test.)
    pub async fn a_newer_breaking_store_refuses_to_open<M: Medium>() {
        // The probe handle is opened first: an arm may refuse to OPEN a
        // newer-breaking store at all (the SQLite arm's pre-migrate half).
        let medium = M::fresh();
        let (b, probe) = (medium.open().await, medium.open().await);
        b.meta_put(META_FORMAT_VERSION, b"9").await.unwrap();
        b.meta_put(META_MIN_READER, b"9").await.unwrap();
        let err = AccountStore::open(b, "aa11", writer(7)).await.unwrap_err();
        let incompatible = err.downcast_ref::<StoreIncompatible>().unwrap();
        assert_eq!(incompatible.min_reader, 9);
        assert_eq!(
            stamped_writer(&probe).await.unwrap(),
            None,
            "a refused open stamps no writer"
        );
    }

    /// The carry arm's relay primitive (`account-replica-posture.md` § The
    /// store device principal, refinement 11): at one coordinate it deletes
    /// the relay rows under every item but the one the fleet serves there,
    /// leaves other coordinates and other writers alone, and is a no-op the
    /// second time.
    pub async fn relay_retire_shadowed_deletes_only_the_other_items_at_the_coordinate<M: Medium>() {
        let (_medium, store) = store::<M>().await;
        let (w, other) = (writer(7), writer(8));
        let relay = |writer: WriterId, seq: u64, item: &[u8]| RelayRow {
            scope: "state".into(),
            item_class: "state-entry".into(),
            writer,
            writer_seq: seq,
            item_key: item.to_vec(),
            op: "state-put".into(),
            entry: Some(b"sealed".to_vec()),
            feed_seq: None,
        };
        for row in [
            relay(w, 5, b"burnt-item"),
            relay(w, 5, b"fleet-item"),
            relay(w, 6, b"another-item"),
            relay(other, 5, b"burnt-item"),
        ] {
            store.record_relay_row(&row).await.unwrap();
        }
        assert_eq!(
            store
                .retire_shadowed_relay_rows("state", &w, 5, b"fleet-item")
                .await
                .unwrap(),
            1,
            "only the other item at the coordinate goes"
        );
        assert_eq!(
            store
                .retire_shadowed_relay_rows("state", &w, 5, b"fleet-item")
                .await
                .unwrap(),
            0,
            "idempotent"
        );
        let mut left: Vec<(WriterId, u64, Vec<u8>)> = store
            .relay_rows("state", "state-entry", &[], 100)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.writer, r.writer_seq, r.item_key))
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                (w, 5, b"fleet-item".to_vec()),
                (w, 6, b"another-item".to_vec()),
                (other, 5, b"burnt-item".to_vec()),
            ]
        );
    }

    /// The refusal arm's relay primitive (`account-replica-posture.md` § The
    /// store device principal, refinement 11 → *a refused row's relay
    /// residue*): at one coordinate it deletes the relay row under EVERY
    /// item — the nest's final word covers the coordinate, whatever item
    /// this replica recorded there — leaves other coordinates and other
    /// writers alone, and is a no-op the second time. Beside it, a row a
    /// transient failure left un-published is untouched: nothing here fires
    /// on an outage, only on the typed refusal.
    pub async fn relay_retire_at_deletes_every_item_at_the_coordinate_and_nothing_else<
        M: Medium,
    >() {
        let (_medium, store) = store::<M>().await;
        let (w, other) = (writer(7), writer(8));
        let relay = |writer: WriterId, seq: u64, item: &[u8]| RelayRow {
            scope: "state".into(),
            item_class: "state-entry".into(),
            writer,
            writer_seq: seq,
            item_key: item.to_vec(),
            op: "state-put".into(),
            entry: Some(b"sealed".to_vec()),
            feed_seq: None,
        };
        for row in [
            relay(w, 5, b"refused-item"),
            relay(w, 5, b"refused-twin"),
            relay(w, 6, b"outage-pending-item"),
            relay(other, 5, b"refused-item"),
        ] {
            store.record_relay_row(&row).await.unwrap();
        }
        assert_eq!(
            store.retire_relay_rows_at("state", &w, 5).await.unwrap(),
            2,
            "every item at the refused coordinate goes"
        );
        assert_eq!(
            store.retire_relay_rows_at("state", &w, 5).await.unwrap(),
            0,
            "idempotent"
        );
        let mut left: Vec<(WriterId, u64, Vec<u8>)> = store
            .relay_rows("state", "state-entry", &[], 100)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.writer, r.writer_seq, r.item_key))
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                (w, 6, b"outage-pending-item".to_vec()),
                (other, 5, b"refused-item".to_vec()),
            ],
            "the outage-pending row and the other writer's row stay"
        );
    }

    /// The compaction primitive's two refusals (`account-replica-posture.md`
    /// § The store device principal, refinement 11): it never touches the
    /// store's CURRENT writer, whose append counter would re-issue a freed
    /// seq, and it deletes a row only while its coordinate still holds exactly
    /// the op and item the caller read.
    pub async fn retired_compaction_refuses_the_current_writer_and_deletes_only_the_row_it_was_given<
        M: Medium,
    >() {
        let (a, b) = (writer(7), writer(8));
        let (_medium, store) = store::<M>().await;
        let (_, seq) = store
            .put_state(StateEntry {
                kind: "moderation".into(),
                key: "muted".into(),
                scope: "state".into(),
                value: b"v".to_vec(),
                merge_meta: None,
                entry_version: 0,
                tombstone: false,
            })
            .await
            .unwrap();
        let row = store
            .scope_rows("state", &a, 0, 10)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(row.seq, seq);

        assert!(
            store
                .backend()
                .compact_retired_rows(std::slice::from_ref(&row))
                .await
                .is_err(),
            "the current writer's log is never compacted"
        );
        assert_eq!(store.max_held_seq("state", &a).await.unwrap(), Some(seq));

        rotate_writer_identity(store.backend(), &a, &b)
            .await
            .unwrap();
        let mut elsewhere = row.clone();
        elsewhere.item = ItemRef::StateKey {
            kind: "moderation".into(),
            key: "other".into(),
            entry_version: 1,
        };
        assert_eq!(
            store
                .backend()
                .compact_retired_rows(&[elsewhere])
                .await
                .unwrap()
                .journal_rows,
            0,
            "a coordinate holding a different item is not the caller's row"
        );
        assert_eq!(store.max_held_seq("state", &a).await.unwrap(), Some(seq));
        assert_eq!(
            store
                .backend()
                .compact_retired_rows(&[row])
                .await
                .unwrap()
                .journal_rows,
            1
        );
        assert_eq!(store.max_held_seq("state", &a).await.unwrap(), None);
    }

    /// **The classifier reads the store, not the handle.** A handle open across
    /// a co-located sibling's fence keeps answering `A` from [`AccountStore::writer`]
    /// and `[]` from [`AccountStore::retired_writers`] forever — those are `open`-time
    /// snapshots and `rotate_writer_identity` takes the backend, not the store.
    /// [`AccountStore::writer_relation`] must answer for the store as it stands
    /// now: the successor is CURRENT, the predecessor the handle still caches as
    /// its own writer is RETIRED, and a third identity is FOREIGN.
    pub async fn writer_relation_answers_for_the_store_not_the_handles_open_time_cache<
        M: Medium,
    >() {
        let (a, b, c) = (writer(7), writer(0xbb), writer(0xcc));
        let (_medium, store) = store::<M>().await;

        // Pre-fence, the cache and the store agree — assert that, so the
        // post-fence divergence below is a difference and not a fixture.
        assert_eq!(store.writer(), a, "the fixture opens as A");
        assert_eq!(
            store.writer_relation(&a).await.unwrap(),
            WriterRelation::Current
        );
        assert_eq!(
            store.writer_relation(&b).await.unwrap(),
            WriterRelation::Foreign
        );

        rotate_writer_identity(store.backend(), &a, &b)
            .await
            .unwrap();

        assert_eq!(
            store.writer(),
            a,
            "the handle must still be stale, or there is nothing to test"
        );
        assert!(
            store.retired_writers().is_empty(),
            "and its retired cache must still be the empty one it opened with"
        );

        assert_eq!(
            store.writer_relation(&b).await.unwrap(),
            WriterRelation::Current,
            "the successor is the store's own current writer — reading it as anything \
             else is what let a peer's echo move an un-pushed tail's publish watermark"
        );
        assert_eq!(
            store.writer_relation(&a).await.unwrap(),
            WriterRelation::Retired,
            "the fence retires the predecessor in the same transaction, so a handle \
             still caching it as CURRENT would refuse to advance its transport-\
             independent slot"
        );
        assert_eq!(
            store.writer_relation(&c).await.unwrap(),
            WriterRelation::Foreign,
            "a third identity is nobody's own history"
        );
        assert!(
            WriterRelation::Current.is_own() && WriterRelation::Retired.is_own(),
            "both own arms hold their rows as this store's own history"
        );
        assert!(!WriterRelation::Foreign.is_own());
    }

    /// The `None` arm: a store with no `META_WRITER_ID` at all has never been
    /// fenced — `rotate_writer_identity` is vacuous there and stamps nothing —
    /// so falling back to the `open`-time cache is the live answer, not a
    /// staleness. Written through the raw meta key because no public API can
    /// produce this state on an open store.
    pub async fn writer_relation_falls_back_to_the_cache_only_where_a_fence_is_unrepresentable<
        M: Medium,
    >() {
        let (_medium, store) = store::<M>().await;
        store.backend().meta_delete(META_WRITER_ID).await.unwrap();
        assert_eq!(
            stamped_writer(store.backend()).await.unwrap(),
            None,
            "sanity: the stamp is the thing that is gone"
        );

        assert_eq!(
            store.writer_relation(&writer(7)).await.unwrap(),
            WriterRelation::Current,
            "with no stamp to read, the handle's own writer is still its own"
        );
        assert_eq!(
            store.writer_relation(&writer(0xbb)).await.unwrap(),
            WriterRelation::Foreign
        );
    }

    /// The feed coordinate (`RelayRow::feed_seq`): unknown until told, filled
    /// in by a replay at the held coordinates or by the publish leg's stamp,
    /// kept by a replay that carries none, and superseded with the row — a
    /// newer row of the same item starts unknown again until its own feed
    /// coordinate is known.
    pub async fn a_relay_row_remembers_its_feed_seq_once_told<M: Medium>() {
        async fn at<B: StoreBackend>(s: &AccountStore<B>) -> Vec<(u64, Option<u64>)> {
            s.relay_rows_at("state", "state-entry", b"item-a")
                .await
                .unwrap()
                .into_iter()
                .map(|r| (r.writer_seq, r.feed_seq))
                .collect()
        }
        let (_medium, s) = store::<M>().await;
        s.record_relay_row(&relay_row(1, 1, b"item-a", b"sealed-1"))
            .await
            .unwrap();
        assert_eq!(at(&s).await, vec![(1, None)]);
        // The walk's own echo: the same coordinates, now with the feed seq.
        s.record_relay_row(&RelayRow {
            feed_seq: Some(40),
            ..relay_row(1, 1, b"item-a", b"sealed-1")
        })
        .await
        .unwrap();
        assert_eq!(at(&s).await, vec![(1, Some(40))]);
        // A replay carrying none keeps what is known.
        s.record_relay_row(&relay_row(1, 1, b"item-a", b"sealed-1"))
            .await
            .unwrap();
        assert_eq!(at(&s).await, vec![(1, Some(40))]);
        // The publish leg's stamp names the coordinates exactly: a stamp for
        // a seq the store does not hold is a no-op.
        s.stamp_relay_feed_seq("state", &writer(1), b"item-a", 2, 99)
            .await
            .unwrap();
        assert_eq!(at(&s).await, vec![(1, Some(40))]);
        s.stamp_relay_feed_seq("state", &writer(1), b"item-a", 1, 41)
            .await
            .unwrap();
        assert_eq!(at(&s).await, vec![(1, Some(41))]);
        // A newer row of the item supersedes the coordinate with the row.
        s.record_relay_row(&relay_row(1, 3, b"item-a", b"sealed-3"))
            .await
            .unwrap();
        assert_eq!(at(&s).await, vec![(3, None)]);
        // The by-writer read carries it too.
        s.stamp_relay_feed_seq("state", &writer(1), b"item-a", 3, 77)
            .await
            .unwrap();
        let of_writer: Vec<Option<u64>> = s
            .relay_rows_of_writer("state", "state-entry", &writer(1))
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.feed_seq)
            .collect();
        assert_eq!(of_writer, vec![Some(77)]);
    }

    /// A feed coordinate is a position in one replica's log
    /// (`account-sync-plane.md` § The bind leg, ruling 2), so voiding a
    /// scope's watermark clears every coordinate of that scope with it — the
    /// watermark, the replica it was keyed to, and each relay row's
    /// `feed_seq` — while the rows themselves stay, another scope keeps
    /// both, and a later stamp or replay fills a coordinate in afresh.
    pub async fn voiding_the_watermark_clears_every_feed_seq_of_its_scope<M: Medium>() {
        async fn feed_seqs<B: StoreBackend>(s: &AccountStore<B>, scope: &str) -> Vec<Option<u64>> {
            s.relay_rows(scope, "state-entry", &[], u32::MAX)
                .await
                .unwrap()
                .into_iter()
                .map(|r| r.feed_seq)
                .collect()
        }
        let (_medium, s) = store::<M>().await;
        for row in [
            RelayRow {
                feed_seq: Some(40),
                ..relay_row(1, 1, b"item-a", b"sealed-1")
            },
            RelayRow {
                feed_seq: Some(41),
                ..relay_row(2, 1, b"item-b", b"sealed-2")
            },
            relay_row(3, 1, b"item-c", b"sealed-3"),
            RelayRow {
                scope: "fleet".into(),
                feed_seq: Some(42),
                ..relay_row(1, 2, b"item-a", b"sealed-4")
            },
        ] {
            s.record_relay_row(&row).await.unwrap();
        }
        for scope in ["state", "fleet"] {
            s.raise_nest_watermark(scope, Some(b"replica-a"), 50)
                .await
                .unwrap();
        }

        s.void_nest_watermark("state").await.unwrap();
        assert_eq!(s.nest_watermark("state").await.unwrap(), None);
        assert_eq!(s.nest_watermark_replica("state").await.unwrap(), None);
        assert_eq!(
            feed_seqs(&s, "state").await,
            vec![None, None, None],
            "every coordinate of the scope is void, and every row stays"
        );
        assert_eq!(s.nest_watermark("fleet").await.unwrap(), Some(50));
        assert_eq!(
            feed_seqs(&s, "fleet").await,
            vec![Some(42)],
            "another scope's log is another question"
        );

        // The new replica's walk and put replies stamp afresh.
        s.record_relay_row(&RelayRow {
            feed_seq: Some(3),
            ..relay_row(1, 1, b"item-a", b"sealed-1")
        })
        .await
        .unwrap();
        s.stamp_relay_feed_seq("state", &writer(2), b"item-b", 1, 4)
            .await
            .unwrap();
        assert_eq!(feed_seqs(&s, "state").await, vec![Some(3), Some(4), None]);
        // Idempotent on a scope with nothing banked.
        s.void_nest_watermark("state").await.unwrap();
        assert_eq!(feed_seqs(&s, "state").await, vec![None, None, None]);
    }

    /// The retire record (`account-sync-plane.md` § The bind leg, ruling 5):
    /// what one handle records a second handle reads, oldest first, belt and
    /// answer intact; a retire asked again at the same coordinates replaces
    /// its entry and becomes the newest; the record keeps its newest `cap`
    /// entries; and a reader clears through what it read, leaving an entry
    /// recorded since.
    pub async fn the_retire_record_keeps_the_newest_and_a_reader_clears_only_what_it_read<
        M: Medium,
    >() {
        use crate::types::IssuedRetire;
        fn retire(item: u8, seq: u64) -> IssuedRetire {
            IssuedRetire {
                scope: "state-fleet".into(),
                item_key: [item; 32],
                writer: writer(7),
                writer_seq: seq,
                no_rows_sealed_under: None,
                delete_escrow_wraps: false,
                settled: false,
            }
        }
        async fn read<B: StoreBackend>(s: &AccountStore<B>) -> Vec<IssuedRetire> {
            s.issued_retires()
                .await
                .unwrap()
                .into_iter()
                .map(|(_, r)| r)
                .collect()
        }
        let (medium, s) = store::<M>().await;
        assert!(read(&s).await.is_empty(), "a fresh store records nothing");

        let belted = IssuedRetire {
            no_rows_sealed_under: Some([0x42; 32]),
            delete_escrow_wraps: true,
            settled: true,
            ..retire(2, 9)
        };
        s.record_issued_retire(&retire(1, 5)).await.unwrap();
        s.record_issued_retire(&belted).await.unwrap();
        // The process that reads is not the one that recorded.
        let reader = AccountStore::open(medium.open().await, "aa11", writer(7))
            .await
            .unwrap();
        assert_eq!(read(&reader).await, vec![retire(1, 5), belted.clone()]);

        // Asked again at the same coordinates — a deferred retire, every pass
        // — the entry is replaced with the new answer and becomes the newest.
        let answered = IssuedRetire {
            settled: true,
            ..retire(1, 5)
        };
        s.record_issued_retire(&answered).await.unwrap();
        assert_eq!(read(&reader).await, vec![belted.clone(), answered.clone()]);
        // The same item at another coordinate is another retire, and so is
        // the same coordinate on another scope.
        let other_scope = IssuedRetire {
            scope: "state".into(),
            ..retire(1, 5)
        };
        s.record_issued_retire(&retire(1, 6)).await.unwrap();
        s.record_issued_retire(&other_scope).await.unwrap();
        assert_eq!(
            read(&reader).await,
            vec![
                belted.clone(),
                answered.clone(),
                retire(1, 6),
                other_scope.clone()
            ]
        );

        // The reader clears through what it read; an entry recorded since
        // stays for its next run.
        let through = reader
            .issued_retires()
            .await
            .unwrap()
            .last()
            .map(|(ord, _)| *ord)
            .unwrap();
        s.record_issued_retire(&retire(3, 1)).await.unwrap();
        reader.clear_issued_retires_through(through).await.unwrap();
        assert_eq!(read(&s).await, vec![retire(3, 1)]);
        reader.clear_issued_retires_through(through).await.unwrap();
        assert_eq!(read(&s).await, vec![retire(3, 1)], "idempotent");

        // Bounded, newest kept.
        for seq in 10..16 {
            s.backend()
                .issued_retire_put(&retire(4, seq), 3)
                .await
                .unwrap();
        }
        assert_eq!(
            read(&s).await,
            vec![retire(4, 13), retire(4, 14), retire(4, 15)]
        );
        // An order token is never reused while an entry stands: one recorded
        // after the trim is still the newest.
        s.backend()
            .issued_retire_put(&retire(4, 13), 3)
            .await
            .unwrap();
        assert_eq!(
            read(&s).await,
            vec![retire(4, 14), retire(4, 15), retire(4, 13)]
        );
    }

    /// The collapse law: one live row per `(scope, writer, item)` — a newer
    /// row from the same writer for the same item supersedes in place, and a
    /// replayed or out-of-order OLDER row is a no-op (never a regression).
    pub async fn relay_rows_collapse_per_item_and_writer_and_never_regress<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        s.record_relay_row(&relay_row(1, 1, b"item-a", b"sealed-1"))
            .await
            .unwrap();
        s.record_relay_row(&relay_row(1, 3, b"item-a", b"sealed-3"))
            .await
            .unwrap();
        // A replay of the superseded row must not regress the live one.
        s.record_relay_row(&relay_row(1, 2, b"item-a", b"sealed-2"))
            .await
            .unwrap();

        let rows = s
            .relay_rows("state", "state-entry", &[], 100)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "one live row per (scope, writer, item)");
        assert_eq!(rows[0].writer_seq, 3);
        assert_eq!(rows[0].entry.as_deref(), Some(&b"sealed-3"[..]));
    }

    /// The serve semantics a peer's walk relies on: frontier-gated (a writer
    /// absent from the frontier is at 0), ordered prefixes per writer, and
    /// `limit` truncates without breaking per-writer order.
    pub async fn relay_rows_serve_past_frontier_as_ordered_prefixes_per_writer<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        s.record_relay_row(&relay_row(1, 1, b"item-a", b"a1"))
            .await
            .unwrap();
        s.record_relay_row(&relay_row(1, 2, b"item-b", b"b2"))
            .await
            .unwrap();
        s.record_relay_row(&relay_row(2, 5, b"item-c", b"c5"))
            .await
            .unwrap();

        // Writer 1 already accounted through seq 1 → only its seq-2 row plus
        // writer 2's row are due.
        let rows = s
            .relay_rows("state", "state-entry", &[(writer(1), 1)], 100)
            .await
            .unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| (r.writer, r.writer_seq))
                .collect::<Vec<_>>(),
            vec![(writer(1), 2), (writer(2), 5)]
        );

        // The limit truncates the page; per-writer ascending order survives.
        let rows = s.relay_rows("state", "state-entry", &[], 2).await.unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| (r.writer, r.writer_seq))
                .collect::<Vec<_>>(),
            vec![(writer(1), 1), (writer(1), 2)]
        );

        // A different item class is a different feed arm.
        assert!(
            s.relay_rows("state", "record-cid", &[], 100)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// What the relay plane has SEEN of each writer: its highest live
    /// `writer_seq`, within the one `(scope, item_class)` asked about.
    pub async fn relay_high_waters_are_the_seen_maximum_per_writer<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        s.record_relay_row(&relay_row(1, 1, b"item-a", b"a1"))
            .await
            .unwrap();
        s.record_relay_row(&relay_row(1, 4, b"item-b", b"b4"))
            .await
            .unwrap();
        s.record_relay_row(&relay_row(2, 5, b"item-c", b"c5"))
            .await
            .unwrap();
        s.record_relay_row(&RelayRow {
            scope: "state-fleet".into(),
            ..relay_row(3, 9, b"item-d", b"d9")
        })
        .await
        .unwrap();
        s.record_relay_row(&RelayRow {
            item_class: "record-cid".into(),
            ..relay_row(4, 9, b"item-e", b"e9")
        })
        .await
        .unwrap();

        let mut seen = s.relay_high_waters("state", "state-entry").await.unwrap();
        seen.sort();
        assert_eq!(
            seen,
            vec![(writer(1), 4), (writer(2), 5)],
            "the maximum per writer, and nothing from another scope or class"
        );
    }

    /// The listed fact (`account-client-lifecycle.md` § The client-side
    /// lifecycle → *The first listing*, clause (1)) is per scope, survives a
    /// reopen of the same medium, and is absent on a store created afresh.
    pub async fn the_listed_fact_is_per_scope_durable_and_absent_on_a_fresh_store<M: Medium>() {
        let (medium, s) = store::<M>().await;
        assert!(
            !s.listed("state").await.unwrap(),
            "a new store has listed nothing"
        );

        s.record_listed("state").await.unwrap();
        s.record_listed("state").await.unwrap();
        assert!(s.listed("state").await.unwrap());
        assert!(
            !s.listed("state-fleet").await.unwrap(),
            "one fact per scope"
        );
        drop(s);

        let reopened = AccountStore::open(medium.open().await, "aa11", writer(7))
            .await
            .unwrap();
        assert!(
            reopened.listed("state").await.unwrap(),
            "the fact survives a restart"
        );
        assert!(!reopened.listed("state-fleet").await.unwrap());

        let (_other, recreated) = store::<M>().await;
        assert!(
            !recreated.listed("state").await.unwrap(),
            "a re-created store starts unlisted"
        );
    }

    /// The let-go set (`account-data-taxonomy.md` § The generation machinery
    /// → *Fleet-scope reclamation*, clause (3)(j)): an add only grows it, a
    /// repeat writes nothing new, and it survives a reopen.
    pub async fn the_let_go_set_only_grows_and_survives_a_reopen<M: Medium>() {
        use std::collections::BTreeSet;
        let (g1, g2) = ([1u8; 32], [2u8; 32]);
        let (medium, s) = store::<M>().await;
        assert!(s.let_go().await.unwrap().is_empty());
        s.add_let_go(&BTreeSet::from([g1])).await.unwrap();
        s.add_let_go(&BTreeSet::from([g1, g2])).await.unwrap();
        assert_eq!(s.let_go().await.unwrap(), BTreeSet::from([g1, g2]));
        drop(s);
        let reopened = AccountStore::open(medium.open().await, "aa11", writer(7))
            .await
            .unwrap();
        assert_eq!(reopened.let_go().await.unwrap(), BTreeSet::from([g1, g2]));
    }

    /// The unkeyed set (`account-client-lifecycle.md` § The client-side
    /// lifecycle → *The first listing*, clause (5), *The fact*): a full
    /// listing replaces it and carries each surviving id's answered-empty
    /// bit, a walk only adds, the bit is set only on an id the set holds, and
    /// all of it is per scope, survives a reopen and is absent on a store
    /// created afresh.
    pub async fn the_unkeyed_set_replaces_adds_and_keeps_its_bits_durably<M: Medium>() {
        use std::collections::{BTreeMap, BTreeSet};
        let (g1, g2, g3) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        let ids = |v: &[[u8; 32]]| v.iter().copied().collect::<BTreeSet<_>>();
        let (medium, s) = store::<M>().await;
        assert!(s.unkeyed("state-fleet").await.unwrap().is_empty());

        s.replace_unkeyed("state-fleet", &ids(&[g1, g2]))
            .await
            .unwrap();
        assert!(
            !s.mark_unkeyed_answered_empty("state-fleet", &g3)
                .await
                .unwrap(),
            "an id the set lacks records nothing"
        );
        assert!(
            s.mark_unkeyed_answered_empty("state-fleet", &g1)
                .await
                .unwrap()
        );
        s.add_unkeyed("state-fleet", &ids(&[g1, g3])).await.unwrap();
        assert_eq!(
            s.unkeyed("state-fleet").await.unwrap(),
            BTreeMap::from([(g1, true), (g2, false), (g3, false)]),
            "a walk adds, keeping the bit it found"
        );
        assert!(
            s.unkeyed("state").await.unwrap().is_empty(),
            "one set per scope"
        );
        drop(s);

        let reopened = AccountStore::open(medium.open().await, "aa11", writer(7))
            .await
            .unwrap();
        reopened
            .replace_unkeyed("state-fleet", &ids(&[g1, g3]))
            .await
            .unwrap();
        assert_eq!(
            reopened.unkeyed("state-fleet").await.unwrap(),
            BTreeMap::from([(g1, true), (g3, false)]),
            "the set and its bits survive a restart; a full listing drops what it no longer \
             left unopened and carries the survivor's bit"
        );
        reopened
            .replace_unkeyed("state-fleet", &BTreeSet::new())
            .await
            .unwrap();
        assert!(reopened.unkeyed("state-fleet").await.unwrap().is_empty());

        let (_other, recreated) = store::<M>().await;
        assert!(recreated.unkeyed("state-fleet").await.unwrap().is_empty());
    }

    /// The parked list (`account-replica-posture.md` § The store device
    /// principal, refinement 11 → *A row refused for room is parked*): park
    /// and unpark are idempotent, the list is per scope and per writer,
    /// survives a reopen, empties on a clear, and is absent on a store
    /// created afresh.
    pub async fn the_parked_list_is_per_scope_and_writer_and_durable<M: Medium>() {
        use std::collections::BTreeSet;
        let (medium, s) = store::<M>().await;
        let me = writer(7);
        assert!(s.parked("state", &me).await.unwrap().is_empty());

        s.park("state", &me, 300).await.unwrap();
        s.park("state", &me, 12).await.unwrap();
        s.park("state", &me, 12).await.unwrap();
        assert_eq!(
            s.parked("state", &me).await.unwrap(),
            BTreeSet::from([12, 300]),
            "a seq parks once, and the list reads in seq order"
        );
        assert!(
            s.parked("state-fleet", &me).await.unwrap().is_empty(),
            "one list per scope"
        );
        assert!(
            s.parked("state", &writer(8)).await.unwrap().is_empty(),
            "one list per writer"
        );
        assert!(
            !s.unpark("state", &me, 13).await.unwrap(),
            "13 was never parked"
        );
        drop(s);

        let reopened = AccountStore::open(medium.open().await, "aa11", writer(7))
            .await
            .unwrap();
        assert_eq!(
            reopened.parked("state", &me).await.unwrap(),
            BTreeSet::from([12, 300]),
            "the list survives a restart"
        );
        assert!(reopened.unpark("state", &me, 12).await.unwrap());
        assert!(!reopened.unpark("state", &me, 12).await.unwrap());
        assert_eq!(
            reopened.parked("state", &me).await.unwrap(),
            BTreeSet::from([300])
        );
        reopened.park("state", &writer(8), 5).await.unwrap();
        reopened.clear_parked("state", &me).await.unwrap();
        assert!(reopened.parked("state", &me).await.unwrap().is_empty());
        assert_eq!(
            reopened.parked("state", &writer(8)).await.unwrap(),
            BTreeSet::from([5]),
            "a clear empties one writer's list"
        );
        reopened.unpark("state", &writer(8), 5).await.unwrap();
        assert!(
            reopened
                .parked("state", &writer(8))
                .await
                .unwrap()
                .is_empty()
        );

        let (_other, recreated) = store::<M>().await;
        recreated.park("state", &me, 1).await.unwrap();
        let (_third, fresh) = store::<M>().await;
        assert!(
            fresh.parked("state", &me).await.unwrap().is_empty(),
            "a re-created store starts with nothing parked"
        );
    }

    /// Rising-only, per scope, and read back as the raise left it — with the
    /// numeric compare the ASCII-decimal encoding needs ("10" sorts below "9"
    /// as bytes), and a value SQLite's signed integer cannot hold refused
    /// rather than wrapped.
    pub async fn the_nest_watermark_only_rises_and_is_per_scope<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        assert_eq!(s.nest_watermark("state").await.unwrap(), None);

        assert_eq!(s.raise_nest_watermark("state", None, 9).await.unwrap(), 9);
        assert_eq!(s.raise_nest_watermark("state", None, 10).await.unwrap(), 10);
        assert_eq!(
            s.raise_nest_watermark("state", None, 3).await.unwrap(),
            10,
            "a lower raise leaves the watermark where it was"
        );
        assert!(
            s.raise_nest_watermark("state", None, u64::MAX)
                .await
                .is_err()
        );
        assert_eq!(s.nest_watermark("state").await.unwrap(), Some(10));
        assert_eq!(
            s.nest_watermark("state-fleet").await.unwrap(),
            None,
            "one watermark per scope"
        );
    }

    /// A watermark is valid only for the replica it was banked from
    /// (`account-sync-plane.md` § The bind leg, ruling 2): a raise under
    /// another replica id — or under none where one is stored, or the reverse
    /// — starts from nothing, never max-merging with the other replica's
    /// higher value; a raise under the same id max-merges as always.
    pub async fn the_nest_watermark_is_keyed_by_the_replica_it_was_banked_from<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        let (a, b) = ([0xAA; 16], [0xBB; 16]);
        assert_eq!(s.nest_watermark_replica("state").await.unwrap(), None);

        assert_eq!(
            s.raise_nest_watermark("state", Some(&a), 40).await.unwrap(),
            40
        );
        assert_eq!(
            s.raise_nest_watermark("state", Some(&a), 30).await.unwrap(),
            40
        );
        assert_eq!(
            s.nest_watermark_replica("state").await.unwrap().as_deref(),
            Some(a.as_slice())
        );

        assert_eq!(
            s.raise_nest_watermark("state", Some(&b), 7).await.unwrap(),
            7,
            "another replica's echo re-keys the bank from nothing"
        );
        assert_eq!(
            s.nest_watermark_replica("state").await.unwrap().as_deref(),
            Some(b.as_slice())
        );
        assert_eq!(
            s.raise_nest_watermark("state", None, 5).await.unwrap(),
            5,
            "a nest naming no replica is not the one banked"
        );
        assert_eq!(s.nest_watermark_replica("state").await.unwrap(), None);

        s.raise_nest_watermark("state", Some(&a), 9).await.unwrap();
        s.clear_nest_watermark("state").await.unwrap();
        assert_eq!(s.nest_watermark("state").await.unwrap(), None);
        assert_eq!(s.nest_watermark_replica("state").await.unwrap(), None);
    }

    /// A scope's departure takes its watermark with it — a re-joined scope
    /// must not inherit a claim to a log prefix its rows no longer back — and
    /// leaves every other scope's alone.
    pub async fn a_departure_clears_that_scopes_watermark_and_no_other<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        s.raise_nest_watermark(&post_scope(), None, 42)
            .await
            .unwrap();
        s.raise_nest_watermark(&mail_scope(), None, 7)
            .await
            .unwrap();

        s.drop_scope(&post_scope()).await.unwrap();

        assert_eq!(s.nest_watermark(&post_scope()).await.unwrap(), None);
        assert_eq!(s.nest_watermark(&mail_scope()).await.unwrap(), Some(7));
    }

    /// The meter answers T15's "bytes and item counts, by scope family", and
    /// separates payload from floor: a tombstone row's bytes are held but not
    /// evictable.
    pub async fn the_custody_meter_counts_by_family_and_excludes_the_floor<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        s.record_relay_row(&relay_row(1, 1, b"item-a", b"12345"))
            .await
            .unwrap();
        s.record_relay_row(&relay_row(1, 2, b"item-b", b"1234567890"))
            .await
            .unwrap();
        s.record_relay_row(&relay_row_op(2, 3, b"item-c", b"1234", "tombstone"))
            .await
            .unwrap();

        let meter = s.custody_meter(&["tombstone"]).await.unwrap();
        assert_eq!(meter.scopes.len(), 1, "one family here");
        let f = &meter.scopes[0];
        assert_eq!(
            (f.scope.as_str(), f.item_class.as_str()),
            ("state", "state-entry")
        );
        assert_eq!(f.rows, 3);
        assert_eq!(f.payload_bytes, 19, "every held byte counts");
        assert_eq!(f.evictable_rows, 2, "the tombstone is floor");
        assert_eq!(f.evictable_bytes, 15);
        assert_eq!(meter.floor_bytes(), 4);
    }

    /// The T15 eviction contract, end to end at the store: over budget, payload
    /// bytes go **oldest first**, the tombstone's payload never goes, every
    /// row's coordinate floor survives, and the plane still serves the shape.
    pub async fn eviction_drops_payload_oldest_first_and_never_the_floor<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        s.record_relay_row(&relay_row(1, 1, b"item-a", b"aaaaaaaaaa"))
            .await
            .unwrap();
        s.record_relay_row(&relay_row(1, 2, b"item-b", b"bbbbbbbbbb"))
            .await
            .unwrap();
        s.record_relay_row(&relay_row_op(2, 3, b"item-c", b"cccccccccc", "tombstone"))
            .await
            .unwrap();

        // Cap of 20 over 30 held bytes → free 10, which is exactly the oldest
        // non-floor row.
        let meter = s.custody_meter(&["tombstone"]).await.unwrap();
        let plan = fauna_core::custody_policy::plan_custody_eviction(&meter, 20);
        assert_eq!(
            plan.state,
            fauna_core::custody_policy::CustodyBudgetState::OverBudget
        );
        let freed = s
            .apply_custody_eviction(&plan, &["tombstone"])
            .await
            .unwrap();
        assert_eq!((freed.rows, freed.bytes), (1, 10));

        let rows = s
            .relay_rows("state", "state-entry", &[], 100)
            .await
            .unwrap();
        assert_eq!(rows.len(), 3, "the coordinate floor survives eviction");
        let by_item = |k: &[u8]| {
            rows.iter()
                .find(|r| r.item_key == k)
                .expect("row still present")
        };
        assert_eq!(by_item(b"item-a").entry, None, "oldest payload went");
        assert_eq!(by_item(b"item-a").writer_seq, 1, "its coordinate did not");
        assert_eq!(by_item(b"item-a").op, "state-put");
        assert!(by_item(b"item-b").entry.is_some(), "the newer row stayed");
        assert!(
            by_item(b"item-c").entry.is_some(),
            "T15: tombstones are always-present, never evicted"
        );

        // And the meter now reports the shrunken coverage — the number a
        // receipt carries, so the owner sees degraded redundancy.
        let after = s.custody_meter(&["tombstone"]).await.unwrap();
        assert_eq!(after.held_bytes(), 20);
        assert_eq!(after.rows(), 3, "rows are still held, payload-less");
    }

    /// A custody whose whole overage is floor frees what it can and stays over
    /// — the store half of `AtFloor`. Never an eaten floor, never a lie.
    pub async fn an_overage_made_of_floor_frees_what_it_can_and_stays_over<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        s.record_relay_row(&relay_row(1, 1, b"item-a", b"aaaaa"))
            .await
            .unwrap();
        s.record_relay_row(&relay_row_op(2, 2, b"item-b", &[0u8; 100], "tombstone"))
            .await
            .unwrap();

        let meter = s.custody_meter(&["tombstone"]).await.unwrap();
        let plan = fauna_core::custody_policy::plan_custody_eviction(&meter, 10);
        assert_eq!(
            plan.state,
            fauna_core::custody_policy::CustodyBudgetState::AtFloor
        );
        assert_eq!(plan.unreclaimable, 90);
        let freed = s
            .apply_custody_eviction(&plan, &["tombstone"])
            .await
            .unwrap();
        assert_eq!((freed.rows, freed.bytes), (1, 5));
        assert_eq!(
            s.custody_meter(&["tombstone"]).await.unwrap().held_bytes(),
            100,
            "the floor is still held, and still over the cap"
        );
    }

    /// Eviction is idempotent and self-limiting: a plan asking for more than a
    /// family has frees only what is there, and re-running frees nothing more.
    pub async fn eviction_is_idempotent_and_never_over_frees<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        s.record_relay_row(&relay_row(1, 1, b"item-a", b"aaaaa"))
            .await
            .unwrap();
        let plan = fauna_core::custody_policy::plan_custody_eviction(
            &s.custody_meter(&[]).await.unwrap(),
            0,
        );
        assert_eq!(s.apply_custody_eviction(&plan, &[]).await.unwrap().bytes, 5);
        assert_eq!(
            s.apply_custody_eviction(&plan, &[]).await.unwrap().bytes,
            0,
            "a re-run of the same plan frees nothing — already dehydrated"
        );
    }

    /// A re-pull re-hydrates an evicted row: the pull writes a NEWER
    /// `writer_seq`, which the collapse law accepts. (An equal-seq re-put is
    /// correctly a no-op — the store never regresses, and the custodian's next
    /// pull advances past it.)
    pub async fn a_later_pull_rehydrates_an_evicted_row<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        s.record_relay_row(&relay_row(1, 1, b"item-a", b"aaaaa"))
            .await
            .unwrap();
        let plan = fauna_core::custody_policy::plan_custody_eviction(
            &s.custody_meter(&[]).await.unwrap(),
            0,
        );
        s.apply_custody_eviction(&plan, &[]).await.unwrap();

        s.record_relay_row(&relay_row(1, 2, b"item-a", b"fresher"))
            .await
            .unwrap();
        let rows = s
            .relay_rows("state", "state-entry", &[], 100)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entry.as_deref(), Some(&b"fresher"[..]));
    }

    /// `states_of_kind` enumerates live entries of one kind (discovery's
    /// read: one device-endpoints entry per device writer), skipping
    /// tombstones and other kinds.
    pub async fn states_of_kind_lists_live_entries_of_that_kind_only<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        s.put_state(entry("fauna.state.device-endpoints", "aa", b"ep-a"))
            .await
            .unwrap();
        s.put_state(entry("fauna.state.device-endpoints", "bb", b"ep-b"))
            .await
            .unwrap();
        s.put_state(entry("fauna.state.moderation", "config", b"m"))
            .await
            .unwrap();
        let mut dead = entry("fauna.state.device-endpoints", "cc", b"");
        dead.tombstone = true;
        s.put_state(dead).await.unwrap();

        let got = s
            .states_of_kind("fauna.state.device-endpoints")
            .await
            .unwrap();
        assert_eq!(
            got.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
            vec!["aa", "bb"],
            "live entries of the kind, ordered by key; tombstones and other kinds excluded"
        );
    }

    pub async fn staging_a_local_record_lands_block_index_and_journal_together<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        let bytes = b"canonical sealed post block bytes";

        let (cid, seq) = s
            .stage_local_record(&post_scope(), "post", bytes)
            .await
            .unwrap();

        // Identity is the content address, computed from the bytes — and
        // dag-cbor-coded, the same identity a nest segment gives the same
        // record, so folding it into one later does not mint a second row.
        assert_eq!(cid, ContentHash::of_dag_cbor(bytes));
        assert_eq!(s.block(&cid).await.unwrap().as_deref(), Some(&bytes[..]));
        assert!(s.is_present(&cid).await.unwrap());

        let indexed = s.record(&cid).await.unwrap().expect("index row");
        assert_eq!(indexed.scope, post_scope());
        assert_eq!(indexed.kind, "post");
        assert_eq!(indexed.size, Some(bytes.len() as u64));

        // And the journal row that announces it to every peer.
        let rows = s
            .scope_rows(&post_scope(), &s.writer(), 0, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, seq);
        assert_eq!(rows[0].op, JournalOp::RecordAdded);
        assert_eq!(rows[0].item, ItemRef::Cid(cid));
    }

    pub async fn a_block_whose_bytes_do_not_match_its_cid_is_refused<M: Medium>() {
        // The F9-shaped anti-poisoning predicate: without it a peer could file
        // arbitrary bytes under a well-known CID.
        let (_medium, s) = store::<M>().await;
        let err = s
            .put_block(&cid("claimed"), b"entirely different bytes")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("content-address mismatch"),
            "got: {err}"
        );
        assert!(!s.is_present(&cid("claimed")).await.unwrap());
    }

    pub async fn the_index_is_the_always_present_layer<M: Medium>() {
        // A record known but not held: indexed, sized, absent — the shape a
        // dehydrated replica lives in.
        let (_medium, s) = store::<M>().await;
        let bytes = b"a record this replica has not fetched";
        let c = ContentHash::of_raw(bytes);
        s.note_record(&RecordIndexEntry {
            cid: c,
            scope: mail_scope(),
            kind: "mail".into(),
            size: Some(bytes.len() as u64),
        })
        .await
        .unwrap();

        assert!(s.record(&c).await.unwrap().is_some(), "indexed");
        assert!(!s.is_present(&c).await.unwrap(), "but not held");
        assert_eq!(s.block(&c).await.unwrap(), None);
        assert_eq!(
            s.record(&c).await.unwrap().unwrap().size,
            Some(bytes.len() as u64)
        );

        // Bytes can arrive later and must verify.
        s.hydrate(&c, bytes).await.unwrap();
        assert!(s.is_present(&c).await.unwrap());
        assert!(s.hydrate(&c, b"wrong bytes").await.is_err());
    }

    pub async fn bytes_for_an_unknown_record_are_refused<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        let bytes = b"orphan block";
        let err = s
            .hydrate(&ContentHash::of_raw(bytes), bytes)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("no record-index row"),
            "got: {err}"
        );
    }

    pub async fn hydration_policy_governs_dehydration<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        let mail = b"a bulky mail record with attachments";
        let post = b"a small post record";
        let (mail_cid, _) = s
            .stage_local_record(&mail_scope(), "mail", mail)
            .await
            .unwrap();
        let (post_cid, _) = s
            .stage_local_record(&post_scope(), "post", post)
            .await
            .unwrap();

        // Default policy hydrates everything, so nothing may be dropped.
        assert!(s.dehydrate(&mail_cid).await.is_err());

        s.set_hydration_policy(&HydrationPolicy {
            default: Hydration::Always,
            per_kind: [("mail".to_string(), Hydration::OnDemand)]
                .into_iter()
                .collect(),
        })
        .await
        .unwrap();

        // Now the bulky kind may placeholder — index survives, bytes go.
        assert!(s.dehydrate(&mail_cid).await.unwrap());
        assert!(!s.is_present(&mail_cid).await.unwrap());
        assert!(
            s.record(&mail_cid).await.unwrap().is_some(),
            "index survives"
        );
        // The always-hydrated kind is still protected.
        assert!(s.dehydrate(&post_cid).await.is_err());
        assert!(s.is_present(&post_cid).await.unwrap());
    }

    pub async fn hydration_policy_round_trips_through_store_meta<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        assert_eq!(
            s.hydration_policy().await.unwrap(),
            HydrationPolicy::default()
        );

        let policy = HydrationPolicy {
            default: Hydration::OnDemand,
            per_kind: [
                ("post".to_string(), Hydration::Always),
                ("mail".to_string(), Hydration::OnDemand),
            ]
            .into_iter()
            .collect(),
        };
        s.set_hydration_policy(&policy).await.unwrap();
        let read_back = s.hydration_policy().await.unwrap();
        assert_eq!(read_back, policy);
        assert_eq!(read_back.for_kind("post"), Hydration::Always);
        assert_eq!(read_back.for_kind("mail"), Hydration::OnDemand);
        assert_eq!(
            read_back.for_kind("calendar"),
            Hydration::OnDemand,
            "an unlisted kind takes the default"
        );
    }

    pub async fn a_scope_walk_is_stable_and_resumable<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        for i in 0..5u8 {
            s.stage_local_record(&post_scope(), "post", &[i; 16])
                .await
                .unwrap();
        }
        s.stage_local_record(&mail_scope(), "mail", b"elsewhere")
            .await
            .unwrap();

        let all = s.records_in_scope(&post_scope(), None, 100).await.unwrap();
        assert_eq!(all.len(), 5, "scope-scoped, so the mail record is excluded");
        assert!(
            all.windows(2)
                .all(|w| w[0].cid.as_bytes() < w[1].cid.as_bytes()),
            "stable CID order is what makes the walk resumable"
        );

        // Paging with `after` reproduces exactly the same sequence.
        let mut paged = Vec::new();
        let mut cursor = None;
        loop {
            let page = s
                .records_in_scope(&post_scope(), cursor.as_ref(), 2)
                .await
                .unwrap();
            if page.is_empty() {
                break;
            }
            cursor = Some(page.last().unwrap().cid);
            paged.extend(page);
        }
        assert_eq!(paged, all);
    }

    pub async fn staging_the_same_record_twice_is_idempotent_in_content<M: Medium>() {
        // Two stages of identical bytes: one block, one index row, but two
        // journal rows — the log records that the writer asserted it twice,
        // while the content plane dedups by construction.
        let (_medium, s) = store::<M>().await;
        let bytes = b"the same record, twice";
        let (a, seq_a) = s
            .stage_local_record(&post_scope(), "post", bytes)
            .await
            .unwrap();
        let (b, seq_b) = s
            .stage_local_record(&post_scope(), "post", bytes)
            .await
            .unwrap();

        assert_eq!(a, b);
        assert_ne!(seq_a, seq_b);
        assert_eq!(
            s.records_in_scope(&post_scope(), None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    pub async fn a_lost_seq_race_writes_neither_the_index_row_nor_the_block<M: Medium>() {
        // The atomicity contract of `record_added_with_row`, exercised at the
        // backend seam because the store layer retries past a lost race. An
        // index row without its journal row would be a record no peer ever
        // hears about; a block without either would be unreachable ballast.
        let (_backend_medium, backend) = fresh_backend::<M>().await;
        let s = AccountStore::open(backend, "aa11", writer(7))
            .await
            .unwrap();

        // Occupy (writer 7, seq 1) with an unrelated row.
        s.append_tombstone(&post_scope(), ItemRef::Cid(cid("squatter")))
            .await
            .unwrap();

        let bytes = b"a record that loses the race";
        let c = ContentHash::of_raw(bytes);
        let contested = JournalRow {
            writer: s.writer(),
            seq: 1, // already taken, by a *different* row
            scope: post_scope(),
            op: JournalOp::RecordAdded,
            item: ItemRef::Cid(c),
        };
        let outcome = s
            .backend()
            .record_added_with_row(
                &RecordIndexEntry {
                    cid: c,
                    scope: post_scope(),
                    kind: "post".into(),
                    size: Some(bytes.len() as u64),
                },
                Some(bytes),
                &contested,
                None,
            )
            .await
            .unwrap();

        assert_eq!(outcome, InsertOutcome::OccupiedByDifferent);
        assert!(s.record(&c).await.unwrap().is_none(), "no index row landed");
        assert!(!s.is_present(&c).await.unwrap(), "no block landed");
    }

    /// The key-less invariant, stated as a test: **every block-plane API works
    /// with no read keys.** The store is handed opaque bytes throughout — it hashes them and
    /// stores them, and never needs to interpret one. A custodian replica holds
    /// and serves this plane with zero read reach, which is exactly what makes
    /// custody possible at all.
    pub async fn the_whole_block_plane_runs_key_less<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        // Bytes that are *sealed* as far as this store is concerned: it cannot
        // open them, and nothing below asks it to.
        let sealed = b"\x01\x9f\xa3sealed-ciphertext-no-key-here\xff";
        let c = ContentHash::of_dag_cbor(sealed);

        let (staged, _) = s
            .stage_local_record(&mail_scope(), "mail", sealed)
            .await
            .unwrap();
        assert_eq!(staged, c);
        assert_eq!(s.block(&c).await.unwrap().as_deref(), Some(&sealed[..]));
        assert!(s.is_present(&c).await.unwrap());
        assert!(s.record(&c).await.unwrap().is_some());
        assert_eq!(
            s.records_in_scope(&mail_scope(), None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        s.set_hydration_policy(&HydrationPolicy {
            default: Hydration::OnDemand,
            per_kind: Default::default(),
        })
        .await
        .unwrap();
        assert!(s.dehydrate(&c).await.unwrap());
        assert!(
            s.record(&c).await.unwrap().is_some(),
            "index outlives the bytes"
        );
        s.hydrate(&c, sealed).await.unwrap();
        assert!(s.is_present(&c).await.unwrap());
    }

    pub async fn local_appends_are_monotonic_and_gapless<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        assert_eq!(s.append_record_added("posts", cid("a")).await.unwrap(), 1);
        assert_eq!(s.append_record_added("posts", cid("b")).await.unwrap(), 2);
        // Gapless holds across scopes: the log is per-writer, scope is a row
        // attribute (charter § Ordering model).
        assert_eq!(s.append_record_added("mail", cid("c")).await.unwrap(), 3);
    }

    pub async fn local_append_skips_a_seq_taken_by_another_process<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        assert_eq!(s.append_record_added("posts", cid("a")).await.unwrap(), 1);
        // Another process of the SAME replica (same writer id) appended seq 2
        // — simulate via a direct backend write.
        let foreign = JournalRow {
            writer: s.writer(),
            seq: 2,
            scope: "posts".into(),
            op: JournalOp::RecordAdded,
            item: ItemRef::Cid(cid("their-row")),
        };
        assert_eq!(
            s.backend().insert_row(&foreign, None).await.unwrap(),
            InsertOutcome::Inserted
        );
        // Our next append lands after it, never on top of it.
        assert_eq!(s.append_record_added("posts", cid("d")).await.unwrap(), 3);
    }

    pub async fn the_occupied_slot_is_never_overwritten<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        s.append_record_added("posts", cid("original"))
            .await
            .unwrap();
        let clash = JournalRow {
            writer: s.writer(),
            seq: 1,
            scope: "posts".into(),
            op: JournalOp::RecordAdded,
            item: ItemRef::Cid(cid("usurper")),
        };
        assert_eq!(
            s.backend().insert_row(&clash, None).await.unwrap(),
            InsertOutcome::OccupiedByDifferent
        );
        let rows = s.scope_rows("posts", &s.writer(), 0, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].item, ItemRef::Cid(cid("original")));
    }

    pub async fn ingest_is_idempotent_and_refuses_equivocation<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        let peer = writer(9);
        let row = JournalRow {
            writer: peer,
            seq: 1,
            scope: "posts".into(),
            op: JournalOp::RecordAdded,
            item: ItemRef::Cid(cid("peer-row")),
        };
        assert_eq!(s.ingest_row(&row).await.unwrap(), InsertOutcome::Inserted);
        assert_eq!(
            s.ingest_row(&row).await.unwrap(),
            InsertOutcome::IdenticalPresent
        );
        let mut equivocation = row.clone();
        equivocation.item = ItemRef::Cid(cid("second-story"));
        let err = s.ingest_row(&equivocation).await.unwrap_err();
        assert!(err.to_string().contains("equivocation"), "{err}");
        // Original row intact.
        let rows = s.scope_rows("posts", &peer, 0, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].item, ItemRef::Cid(cid("peer-row")));
    }

    /// The nest's sequencer counter is per (scope, kind)
    /// (`records_db::next_changed_seq`), so every walked content scope starts
    /// at seq 1 under the same reserved `NEST_SEQUENCER` name — two scopes'
    /// coordinates are two logs, never equivocation. Regression: the journal
    /// PK was `(writer_id, writer_seq)` and refused the second scope's first
    /// row (found 2026-08-12 by the seen-set producer's fixtures).
    pub async fn same_writer_seq_in_two_scopes_is_two_logs_not_equivocation<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        for (scope, item) in [("content:mail:aa", "mail-1"), ("content:post:aa", "post-1")] {
            let row = JournalRow {
                writer: WriterId::NEST_SEQUENCER,
                seq: 1,
                scope: scope.into(),
                op: JournalOp::RecordAdded,
                item: ItemRef::Cid(cid(item)),
            };
            assert_eq!(
                s.ingest_row(&row).await.unwrap(),
                InsertOutcome::Inserted,
                "{scope}"
            );
            s.advance_frontier(scope, &WriterId::NEST_SEQUENCER, 1)
                .await
                .unwrap();
        }
        // Both logs held, each scope's frontier its own.
        for (scope, item) in [("content:mail:aa", "mail-1"), ("content:post:aa", "post-1")] {
            let rows = s
                .scope_rows(scope, &WriterId::NEST_SEQUENCER, 0, 10)
                .await
                .unwrap();
            assert_eq!(rows.len(), 1, "{scope}");
            assert_eq!(rows[0].item, ItemRef::Cid(cid(item)), "{scope}");
            assert_eq!(
                s.frontier(scope).await.unwrap(),
                vec![(WriterId::NEST_SEQUENCER, 1)],
                "{scope}"
            );
        }
        // Within ONE scope the equivocation refusal is unchanged.
        let mut equivocation = JournalRow {
            writer: WriterId::NEST_SEQUENCER,
            seq: 1,
            scope: "content:mail:aa".into(),
            op: JournalOp::RecordAdded,
            item: ItemRef::Cid(cid("a-different-story")),
        };
        let err = s.ingest_row(&equivocation).await.unwrap_err();
        assert!(err.to_string().contains("equivocation"), "{err}");
        // And the refusal names the scope (three coordinates, three names).
        assert!(err.to_string().contains("content:mail:aa"), "{err}");
        equivocation.item = ItemRef::Cid(cid("mail-1"));
        assert_eq!(
            s.ingest_row(&equivocation).await.unwrap(),
            InsertOutcome::IdenticalPresent
        );
    }

    pub async fn ingest_tolerates_gaps_from_origin_compaction<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        let peer = writer(9);
        for seq in [1, 5] {
            let row = JournalRow {
                writer: peer,
                seq,
                scope: "posts".into(),
                op: JournalOp::RecordAdded,
                item: ItemRef::Cid(cid(&format!("r{seq}"))),
            };
            assert_eq!(s.ingest_row(&row).await.unwrap(), InsertOutcome::Inserted);
        }
        assert_eq!(
            s.backend()
                .max_scope_writer_seq("posts", &peer)
                .await
                .unwrap(),
            Some(5)
        );
    }

    pub async fn frontier_never_regresses_and_only_accounted_walks_advance_it<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        let peer = writer(9);
        for seq in 1..=5 {
            s.ingest_row(&JournalRow {
                writer: peer,
                seq,
                scope: "posts".into(),
                op: JournalOp::RecordAdded,
                item: ItemRef::Cid(cid(&format!("r{seq}"))),
            })
            .await
            .unwrap();
        }
        // Unaccounted: no rows past 5 are held.
        let err = s.advance_frontier("posts", &peer, 7).await.unwrap_err();
        assert!(err.to_string().contains("unaccounted"), "{err}");
        assert_eq!(s.frontier("posts").await.unwrap(), vec![]);
        // Accounted advance.
        assert_eq!(s.advance_frontier("posts", &peer, 5).await.unwrap(), 5);
        // Replaying an older walk never regresses the high-water.
        assert_eq!(s.advance_frontier("posts", &peer, 3).await.unwrap(), 5);
        assert_eq!(s.frontier("posts").await.unwrap(), vec![(peer, 5)]);
        // And the vector is per-scope: another scope is untouched.
        assert_eq!(s.frontier("mail").await.unwrap(), vec![]);
    }

    pub async fn put_state_lands_entry_and_journal_row_together<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        let (version, seq) = s
            .put_state(entry("fauna.settings", "quiet-hours", b"v1-bytes"))
            .await
            .unwrap();
        assert_eq!((version, seq), (1, 1));
        let stored = s
            .state("fauna.settings", "quiet-hours")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.value, b"v1-bytes");
        assert_eq!(stored.entry_version, 1);
        let rows = s.scope_rows("state", &s.writer(), 0, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].item,
            ItemRef::StateKey {
                kind: "fauna.settings".into(),
                key: "quiet-hours".into(),
                entry_version: 1,
            }
        );
        // A second put supersedes the entry and appends a second row.
        let (version, seq) = s
            .put_state(entry("fauna.settings", "quiet-hours", b"v2-bytes"))
            .await
            .unwrap();
        assert_eq!((version, seq), (2, 2));
        let stored = s
            .state("fauna.settings", "quiet-hours")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.value, b"v2-bytes");
        assert_eq!(stored.entry_version, 2);
    }

    pub async fn a_fresh_store_stamps_the_version_pair_and_reopens<M: Medium>() {
        let dir = M::fresh();
        {
            let b = dir.open().await;
            AccountStore::open(b, "aa11", writer(7)).await.unwrap();
        }
        let b = dir.open().await;
        assert_eq!(
            b.meta_get("format_version").await.unwrap().unwrap(),
            FORMAT_VERSION.to_string().as_bytes()
        );
        AccountStore::open(b, "aa11", writer(7)).await.unwrap();
    }

    pub async fn a_newer_additive_store_opens_and_is_not_restamped_down<M: Medium>() {
        let (_b_medium, b) = fresh_backend::<M>().await;
        b.meta_put(META_FORMAT_VERSION, b"2").await.unwrap();
        b.meta_put(META_MIN_READER, b"1").await.unwrap();
        let s = AccountStore::open(b, "aa11", writer(7)).await.unwrap();
        assert_eq!(
            s.backend()
                .meta_get(META_FORMAT_VERSION)
                .await
                .unwrap()
                .unwrap(),
            b"2"
        );
    }

    /// **The `min_reader` floor rises on every compatible open** — the § 2.2
    /// restamp-at-every-run `max` idiom
    /// (`version-compatibility.md` § 2.2).
    /// A floor stamped only at creation fails open on exactly the population
    /// it exists for: the first breaking bump would refuse a store this
    /// binary *created* and admit every store already in the field
    /// (PROBE-360). Injected versions because the real floor is `1`
    /// today, where a constants-only assertion is vacuous.
    pub async fn an_upgrade_in_place_raises_the_min_reader_floor<M: Medium>() {
        let (_b_medium, b) = fresh_backend::<M>().await;
        b.meta_put(META_FORMAT_VERSION, b"1").await.unwrap();
        b.meta_put(META_MIN_READER, b"1").await.unwrap();
        verify_and_stamp_format(&b, 2, 2).await.unwrap();
        assert_eq!(
            b.meta_get(META_FORMAT_VERSION).await.unwrap().unwrap(),
            b"2"
        );
        assert_eq!(
            b.meta_get(META_MIN_READER).await.unwrap().unwrap(),
            b"2",
            "the floor must rise with the format on an upgrade-in-place — \
             stamped-at-creation-only admits every store already in the field"
        );
    }

    pub async fn a_store_bound_to_another_identity_refuses_to_open<M: Medium>() {
        let dir = M::fresh();
        {
            let b = dir.open().await;
            AccountStore::open(b, "aa11", writer(7)).await.unwrap();
        }
        let b = dir.open().await;
        let err = AccountStore::open(b, "bb22", writer(7)).await.unwrap_err();
        assert!(err.to_string().contains("different actor"), "{err}");
        let b = dir.open().await;
        let err = AccountStore::open(b, "aa11", writer(8)).await.unwrap_err();
        assert!(err.to_string().contains("different writer"), "{err}");
    }

    /// The refusal is asserted per append path, on the typed struct — a
    /// stale process must be able to tell "reassemble" from corruption, so
    /// the downcast (not the message) is the contract.
    pub async fn an_append_under_a_rotated_writer_refuses_typed<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        rotate_writer_identity(s.backend(), &writer(7), &writer(9))
            .await
            .unwrap();

        let err = s
            .append_record_added(&post_scope(), cid("a"))
            .await
            .unwrap_err();
        let stale = err
            .downcast_ref::<StaleWriter>()
            .expect("typed StaleWriter");
        assert_eq!(stale.held, writer(7));
        assert_eq!(stale.current, writer(9));
        assert!(is_stale_writer(&err));

        let err = s.put_state(entry("k", "self", b"v")).await.unwrap_err();
        assert!(is_stale_writer(&err), "{err}");

        let err = s
            .stage_local_record(&post_scope(), "post", b"bytes")
            .await
            .unwrap_err();
        assert!(is_stale_writer(&err), "{err}");

        let err = s
            .outbox_append(&NewOutboxIntent {
                intent_id: [1; 16],
                kind: "k".into(),
                scope: post_scope(),
                payload: vec![1],
                drainer: crate::types::IntentDrainer::Rpc,
            })
            .await
            .unwrap_err();
        assert!(is_stale_writer(&err), "{err}");

        // And nothing landed behind the fence.
        assert_eq!(
            s.max_held_seq(&post_scope(), &writer(7)).await.unwrap(),
            None
        );
        assert!(s.outbox_undrained().await.unwrap().is_empty());
    }

    /// Ingest is never guarded: an origin's row is valid whoever this
    /// replica's writer is — the successor still ingests the fleet's (and
    /// its own predecessor's) history after a rotation.
    pub async fn ingest_still_lands_under_a_rotated_writer<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        rotate_writer_identity(s.backend(), &writer(7), &writer(9))
            .await
            .unwrap();

        let row = JournalRow {
            writer: writer(3),
            seq: 1,
            scope: post_scope(),
            op: JournalOp::RecordAdded,
            item: ItemRef::Cid(cid("x")),
        };
        assert_eq!(
            s.ingest_row(&row).await.unwrap(),
            InsertOutcome::Inserted,
            "ingest of another writer's row must pass the fence"
        );
        let state_row = JournalRow {
            writer: writer(7), // even the RETIRED writer's fleet-held history
            seq: 1,
            scope: "state".into(),
            op: JournalOp::StatePut,
            item: ItemRef::StateKey {
                kind: "k".into(),
                key: "self".into(),
                entry_version: 1,
            },
        };
        assert_eq!(
            s.ingest_state(&state_row, entry("k", "self", b"v"))
                .await
                .unwrap(),
            InsertOutcome::Inserted,
            "ingest of the retired writer's own echoed history must pass too"
        );
    }

    /// The fence's probe-then-act discipline: idempotent against a racing
    /// successor that already landed, refusing on any predecessor mismatch,
    /// refusing on a store with no identity at all.
    pub async fn writer_rotation_verifies_its_predecessor_and_is_idempotent<M: Medium>() {
        let (_medium, s) = store::<M>().await; // stamps writer(7)
        let b = s.backend();

        rotate_writer_identity(b, &writer(7), &writer(9))
            .await
            .unwrap();
        // A racing successor already completed the same rotation: Ok.
        rotate_writer_identity(b, &writer(7), &writer(9))
            .await
            .unwrap();
        // A rotator with a stale picture of the predecessor: refused.
        let err = rotate_writer_identity(b, &writer(5), &writer(6))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("neither"), "{err}");

        // No identity at all: the fence is VACUOUS (a store-less machine has
        // no journal to fence) — Ok, and nothing is stamped.
        let (_empty_medium, empty) = fresh_backend::<M>().await;
        rotate_writer_identity(&empty, &writer(7), &writer(9))
            .await
            .unwrap();
        assert_eq!(
            pending_writer_reauthor(&empty).await.unwrap(),
            None,
            "a vacuous fence stamps nothing — there is no tail to re-author"
        );
    }

    /// The fence stamps the pending-re-author marker with the re-stamp (one
    /// transaction — asserted here by observation, pinned structurally by
    /// `meta_put_two`'s contract), the racing-successor arm leaves it alone,
    /// and the clear is idempotent.
    pub async fn the_fence_records_the_reauthor_marker_and_clears_on_demand<M: Medium>() {
        let (_medium, s) = store::<M>().await; // stamps writer(7)
        let b = s.backend();
        assert_eq!(pending_writer_reauthor(b).await.unwrap(), None);
        rotate_writer_identity(b, &writer(7), &writer(9))
            .await
            .unwrap();
        assert_eq!(
            pending_writer_reauthor(b).await.unwrap(),
            Some(writer(7)),
            "the fence must record WHOSE tail awaits re-authoring"
        );
        // The racing-successor idempotent arm leaves the marker untouched.
        rotate_writer_identity(b, &writer(7), &writer(9))
            .await
            .unwrap();
        assert_eq!(pending_writer_reauthor(b).await.unwrap(), Some(writer(7)));
        let snap = pending_reauthor_snapshot(b).await.unwrap();
        assert!(clear_writer_reauthor_if_unchanged(b, &snap).await.unwrap());
        assert_eq!(pending_writer_reauthor(b).await.unwrap(), None);
        // Idempotent against its OWN snapshot re-read: nothing pending, so
        // "delete the keys I saw absent" is a no-op that still reports done.
        let after = pending_reauthor_snapshot(b).await.unwrap();
        assert!(clear_writer_reauthor_if_unchanged(b, &after).await.unwrap());

        // The retired-writers memory is PERMANENT (it survives the marker
        // clear), append-only across rotations, and idempotent per retiree.
        assert_eq!(retired_writers(b).await.unwrap(), vec![writer(7)]);
        rotate_writer_identity(b, &writer(9), &writer(11))
            .await
            .unwrap();
        assert_eq!(
            retired_writers(b).await.unwrap(),
            vec![writer(7), writer(9)],
            "each rotation appends its predecessor, oldest first"
        );
    }

    /// Two fences with no walk between them (a crash window; a lost slot
    /// right after a revocation rotation) keep BOTH predecessors pending:
    /// the earlier one moves to the overflow key instead of being
    /// overwritten, the newest stays in the original key an older binary
    /// reads, and one clear drops both.
    pub async fn a_fence_landing_over_a_pending_marker_keeps_the_earlier_predecessor<M: Medium>() {
        let (_medium, s) = store::<M>().await; // stamps writer(7)
        let b = s.backend();
        rotate_writer_identity(b, &writer(7), &writer(9))
            .await
            .unwrap();
        rotate_writer_identity(b, &writer(9), &writer(11))
            .await
            .unwrap();
        assert_eq!(
            pending_writer_reauthor(b).await.unwrap(),
            Some(writer(9)),
            "the original key names the NEWEST predecessor (what an older binary walks)"
        );
        assert_eq!(
            pending_writer_reauthors(b).await.unwrap(),
            vec![writer(9), writer(7)],
            "the walk sees both — the earlier tail is still owed"
        );
        assert_eq!(stamped_writer(b).await.unwrap(), Some(writer(11)));
        // The idempotent racing-successor arm changes nothing.
        rotate_writer_identity(b, &writer(9), &writer(11))
            .await
            .unwrap();
        assert_eq!(
            pending_writer_reauthors(b).await.unwrap(),
            vec![writer(9), writer(7)]
        );
        let snap = pending_reauthor_snapshot(b).await.unwrap();
        assert_eq!(
            snap.priors,
            vec![writer(9), writer(7)],
            "the snapshot reads the same two-key marker the walk iterates"
        );
        assert!(clear_writer_reauthor_if_unchanged(b, &snap).await.unwrap());
        assert!(pending_writer_reauthors(b).await.unwrap().is_empty());
        assert_eq!(pending_writer_reauthor(b).await.unwrap(), None);
        // A fresh store: nothing stamped, nothing pending.
        let (_empty_medium, empty) = fresh_backend::<M>().await;
        assert_eq!(stamped_writer(&empty).await.unwrap(), None);
        assert!(pending_writer_reauthors(&empty).await.unwrap().is_empty());
    }

    /// After the fence, the SUCCESSOR opens and appends ordinarily — the
    /// whole point of the rotation.
    pub async fn the_successor_opens_and_appends_after_the_fence<M: Medium>() {
        let dir = M::fresh();
        {
            let b = dir.open().await;
            let s = AccountStore::open(b, "aa11", writer(7)).await.unwrap();
            s.append_record_added(&post_scope(), cid("pre"))
                .await
                .unwrap();
            rotate_writer_identity(s.backend(), &writer(7), &writer(9))
                .await
                .unwrap();
        }
        let b = dir.open().await;
        // The predecessor can no longer OPEN either (adopt refuses)…
        let err = AccountStore::open(b, "aa11", writer(7)).await.unwrap_err();
        assert!(err.to_string().contains("different writer"), "{err}");
        // …and the successor opens and appends from seq 1 of its own log.
        let b = dir.open().await;
        let s = AccountStore::open(b, "aa11", writer(9)).await.unwrap();
        let seq = s
            .append_record_added(&post_scope(), cid("post"))
            .await
            .unwrap();
        assert_eq!(seq, 1);
    }

    /// `coordinate_of_item` is the T1 intake's resolution step
    /// (`account-data-plane.md` § The replica boundary → T1). Three properties
    /// it must hold, because the seen-set entry is keyed per scope: it answers
    /// only from the asked scope, it answers the *introducing* row, and it says
    /// "no" rather than guessing.
    pub async fn coordinate_of_item_resolves_per_scope_from_the_introducing_row<M: Medium>() {
        let dir = M::fresh();
        let s = store_on(&dir).await;
        let shared = ContentHash::of_dag_cbor(b"a record two scopes both carry");
        let absent = ContentHash::of_dag_cbor(b"never journalled here");

        for (scope, seq) in [(post_scope(), 4u64), (post_scope(), 9), (mail_scope(), 2)] {
            s.ingest_row(&JournalRow {
                writer: WriterId::NEST_SEQUENCER,
                seq,
                scope,
                op: JournalOp::RecordAdded,
                item: ItemRef::Cid(shared),
            })
            .await
            .unwrap();
        }

        assert_eq!(
            s.coordinate_of_item(&post_scope(), &ItemRef::Cid(shared))
                .await
                .unwrap(),
            Some((WriterId::NEST_SEQUENCER, 4)),
            "the introducing row, not the later op on the same item"
        );
        assert_eq!(
            s.coordinate_of_item(&mail_scope(), &ItemRef::Cid(shared))
                .await
                .unwrap(),
            Some((WriterId::NEST_SEQUENCER, 2)),
            "a sibling scope's coordinate for the same record is its own"
        );
        assert_eq!(
            s.coordinate_of_item(&post_scope(), &ItemRef::Cid(absent))
                .await
                .unwrap(),
            None,
            "unresolvable is None — the intake drops and re-records, never guesses"
        );
    }

    /// A class-1 tombstone takes the record out of every store API: its index
    /// row and its loose bytes both go, and the scope listing no longer shows
    /// it.
    pub async fn a_tombstone_removes_the_record_from_the_index_and_the_block_plane<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        let (cid, _) = s
            .stage_local_record(&post_scope(), "post", b"a post that gets deleted")
            .await
            .unwrap();
        assert!(s.is_present(&cid).await.unwrap());

        assert!(
            s.apply_tombstone(&cid).await.unwrap(),
            "the replica knew this record"
        );
        assert!(s.record(&cid).await.unwrap().is_none());
        assert!(s.block(&cid).await.unwrap().is_none());
        assert!(
            s.records_in_scope(&post_scope(), None, 100)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// Applying the same tombstone twice, or one for a record this replica
    /// never held, is a no-op rather than an error — a zero-frontier reconcile
    /// re-presents every live tombstone, and a replica that bootstrapped after
    /// the delete legitimately never saw the record.
    pub async fn an_unknown_or_replayed_tombstone_is_a_no_op<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        assert!(!s.apply_tombstone(&cid("never held")).await.unwrap());

        let (c, _) = s
            .stage_local_record(&post_scope(), "post", b"deleted once")
            .await
            .unwrap();
        assert!(s.apply_tombstone(&c).await.unwrap());
        assert!(
            !s.apply_tombstone(&c).await.unwrap(),
            "the second apply finds nothing and says so"
        );
    }

    /// Pin (c): an offer labelled "post" (Always-hydrated) under
    /// the "mail" scope (OnDemand here) — the reverse of the OnDemand-under-
    /// Always case the skip above already handles. Refused **before** the
    /// fetch: `source` panics if `fetch_segment` is ever called, which would
    /// happen if the label were trusted over the scope. Mutate: delete the
    /// pre-fetch scope-kind comparison in `bootstrap_scope_segments` and this
    /// test reds (panics) — the mislabelled offer gets fetched.
    pub async fn bootstrap_refuses_an_always_kind_offer_under_an_ondemand_kind_scope_before_fetching<
        M: Medium,
    >() {
        struct PanicsOnFetchSource {
            offers: Vec<SegmentOffer>,
        }

        impl BootstrapSource for PanicsOnFetchSource {
            async fn list_segments(&self, _scope: &str) -> anyhow::Result<Vec<SegmentOffer>> {
                Ok(self.offers.clone())
            }

            async fn fetch_segment(
                &self,
                _scope: &str,
                _offer: &SegmentOffer,
                _max_bytes: u64,
                _into: &mut impl crate::segments::SegmentSink,
            ) -> anyhow::Result<()> {
                panic!("fetched a segment the pre-fetch kind check should have refused")
            }
        }

        let dir = M::fresh();
        let s = store_on(&dir).await;
        s.set_hydration_policy(&HydrationPolicy {
            default: Hydration::Always,
            per_kind: [("mail".to_string(), Hydration::OnDemand)]
                .into_iter()
                .collect(),
        })
        .await
        .unwrap();
        let source = PanicsOnFetchSource {
            offers: vec![SegmentOffer {
                kind: "post".to_string(),
                segment_id: 1,
                dat_size: Some(1),
            }],
        };

        let report = s
            .bootstrap_scope_segments(&seg_mail_scope(), &source)
            .await
            .unwrap();
        assert_eq!(report.refused_kind_mismatch, 1);
        assert_eq!(report.adopted, 0);
        assert_eq!(report.skipped_dehydrating, 0);
    }

    /// Every plane the charter names, emptied in one call — and the counts say
    /// so, because a departure that silently reached nothing is the failure
    /// this path exists to prevent.
    pub async fn a_departure_empties_every_plane_of_that_scope<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        populate_scope(&s, &post_scope(), "gone").await;

        let counts = s.drop_scope(&post_scope()).await.unwrap();
        assert!(counts.journal_rows > 0, "journal: {counts:?}");
        assert_eq!(counts.record_index_rows, 2, "staged + noted: {counts:?}");
        assert!(counts.blocks > 0, "loose blocks: {counts:?}");
        assert_eq!(counts.frontier_rows, 1, "{counts:?}");
        assert_eq!(counts.relay_rows, 1, "{counts:?}");

        assert!(
            s.records_in_scope(&post_scope(), None, 100)
                .await
                .unwrap()
                .is_empty(),
            "record index emptied"
        );
        assert!(s.frontier(&post_scope()).await.unwrap().is_empty());
        assert!(
            s.relay_rows(&post_scope(), "record-cid", &[], 100)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            s.scope_rows(&post_scope(), &s.writer(), 0, 100)
                .await
                .unwrap()
                .is_empty(),
            "journal emptied"
        );
        assert!(s.block(&cid("gone")).await.unwrap().is_none());
    }

    /// The blast radius is one scope. A sibling scope shares the store, the
    /// journal table and the block plane, and must not notice.
    pub async fn a_sibling_scopes_rows_survive_the_departure<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        populate_scope(&s, &post_scope(), "gone").await;
        populate_scope(&s, &mail_scope(), "stays").await;

        s.drop_scope(&post_scope()).await.unwrap();

        assert_eq!(
            s.records_in_scope(&mail_scope(), None, 100)
                .await
                .unwrap()
                .len(),
            2,
            "the sibling keeps its index rows"
        );
        assert_eq!(s.frontier(&mail_scope()).await.unwrap().len(), 1);
        assert_eq!(
            s.relay_rows(&mail_scope(), "record-cid", &[], 100)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            s.block(&cid("stays")).await.unwrap().is_some(),
            "the sibling's bytes are not collateral"
        );
    }

    /// **T2 transition 4 against transition 3.** The seen-set entry FOR a
    /// departed scope lives in the account-state scope keyed by that scope's
    /// string; the account observed those items and leaving does not unobserve
    /// them, so a grow-only set must not shrink here. Structural, not careful:
    /// a `WHERE scope = ?` delete cannot reach a row whose `scope` column says
    /// `state`.
    pub async fn the_departed_scopes_seen_set_entry_survives<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        populate_scope(&s, &post_scope(), "gone").await;
        s.put_state(entry(
            "fauna.state.seen-set",
            &post_scope(), // the KEY is the departed scope — the trap
            b"watermarked",
        ))
        .await
        .unwrap();

        s.drop_scope(&post_scope()).await.unwrap();

        let kept = s
            .state("fauna.state.seen-set", &post_scope())
            .await
            .unwrap()
            .expect("the seen-set entry for a left scope is still the account's own history");
        assert_eq!(kept.value, b"watermarked");
    }

    /// Dropping a scope this replica never held is a no-op, not an error — the
    /// property that lets a caller replay a departure without bookkeeping.
    pub async fn dropping_an_unheld_scope_reports_zeros<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        let counts = s.drop_scope(&post_scope()).await.unwrap();
        assert!(counts.is_empty(), "{counts:?}");
        assert_eq!(counts.total(), 0);
    }

    /// The whole point of the outbox: a mutation issued offline is durably
    /// queued and survives a process restart, byte-for-byte.
    pub async fn an_intent_survives_a_restart_verbatim<M: Medium>() {
        let dir = M::fresh();
        {
            let s = store_on(&dir).await;
            assert!(
                s.outbox_append(&intent(1, "state", b"knock-req"))
                    .await
                    .unwrap()
            );
        }
        let s = store_on(&dir).await;
        let undrained = s.outbox_undrained().await.unwrap();
        assert_eq!(undrained.len(), 1);
        let got = &undrained[0];
        assert_eq!(got.intent_id, [1u8; 16]);
        assert_eq!(got.kind, "fauna.contacts.knock");
        assert_eq!(got.payload, b"knock-req");
        assert_eq!(got.status, crate::types::IntentStatus::Pending);
        assert_eq!(got.retry_count, 0);
    }

    /// Re-appending the same intent_id is the composer's crash-retry: a
    /// no-op reported as `false`, never a second row or a new seq.
    pub async fn re_appending_the_same_intent_is_a_no_op<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        assert!(s.outbox_append(&intent(1, "state", b"v1")).await.unwrap());
        assert!(!s.outbox_append(&intent(1, "state", b"v1")).await.unwrap());
        let undrained = s.outbox_undrained().await.unwrap();
        assert_eq!(undrained.len(), 1);
        assert_eq!(undrained[0].channel_seq, 1);
    }

    /// Completion is deletion: the ack removes the row; a crash-replayed
    /// drain's double-ack is a no-op, not an error.
    pub async fn an_ack_deletes_and_a_double_ack_is_a_no_op<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        s.outbox_append(&intent(1, "state", b"x")).await.unwrap();
        assert!(s.outbox_ack(&[1u8; 16]).await.unwrap());
        assert!(s.outbox_undrained().await.unwrap().is_empty());
        assert!(!s.outbox_ack(&[1u8; 16]).await.unwrap());
    }

    /// FIFO holds within a scope (channel_seq is append order) and scopes
    /// order independently — the per-channel ordering the MLS contract
    /// requires, expressed at the store as data.
    pub async fn fifo_holds_within_a_scope_and_scopes_are_independent<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        s.outbox_append(&intent(1, &post_scope(), b"a1"))
            .await
            .unwrap();
        s.outbox_append(&intent(2, &mail_scope(), b"b1"))
            .await
            .unwrap();
        s.outbox_append(&intent(3, &post_scope(), b"a2"))
            .await
            .unwrap();

        let undrained = s.outbox_undrained().await.unwrap();
        let posts: Vec<_> = undrained
            .iter()
            .filter(|i| i.scope == post_scope())
            .collect();
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0].payload, b"a1");
        assert_eq!(posts[1].payload, b"a2");
        assert!(posts[0].channel_seq < posts[1].channel_seq);
        let mails: Vec<_> = undrained
            .iter()
            .filter(|i| i.scope == mail_scope())
            .collect();
        assert_eq!(mails.len(), 1);
        assert_eq!(mails[0].channel_seq, 1, "scopes count independently");
    }

    /// A park is not a drop: `mark_failed` flips the status and the row
    /// stays, and the backoff pair records inconclusive attempts.
    pub async fn a_parked_intent_stays_and_attempts_are_counted<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        s.outbox_append(&intent(1, "state", b"x")).await.unwrap();
        assert!(s.outbox_record_attempt(&[1u8; 16]).await.unwrap());
        assert!(s.outbox_mark_failed(&[1u8; 16]).await.unwrap());
        let undrained = s.outbox_undrained().await.unwrap();
        assert_eq!(undrained.len(), 1);
        assert_eq!(undrained[0].status, crate::types::IntentStatus::Failed);
        assert_eq!(undrained[0].retry_count, 1);
        assert!(undrained[0].last_attempt_at.is_some());
        assert!(
            !s.outbox_mark_failed(&[9u8; 16]).await.unwrap(),
            "unknown id"
        );
    }

    /// **The phase-0 interaction, both ways.** A scope departure (T2
    /// transition 3) must proceed normally with undrained intents present for
    /// that very scope — and it must not touch them: the outbox is outside
    /// every scope-keyed plane, so `drop_scope` structurally cannot reach it.
    /// An undrained intent is the only copy of a pending write.
    pub async fn a_departure_proceeds_but_cannot_reach_the_outbox<M: Medium>() {
        let (_medium, s) = store::<M>().await;
        populate_scope(&s, &post_scope(), "gone").await;
        s.outbox_append(&intent(1, &post_scope(), b"pending-send"))
            .await
            .unwrap();

        // One way: the departure itself is unimpeded — every plane empties.
        let counts = s.drop_scope(&post_scope()).await.unwrap();
        assert!(counts.journal_rows > 0);
        assert!(
            s.records_in_scope(&post_scope(), None, 100)
                .await
                .unwrap()
                .is_empty()
        );

        // The other way: the intent survived, verbatim and still pending.
        let undrained = s.outbox_undrained().await.unwrap();
        assert_eq!(undrained.len(), 1, "departure must not drop an intent");
        assert_eq!(undrained[0].intent_id, [1u8; 16]);
        assert_eq!(undrained[0].scope, post_scope());
        assert_eq!(undrained[0].status, crate::types::IntentStatus::Pending);
    }
}

/// The segment-adoption cases, over the real writer's pinned pairs
/// ([`fixtures::PINNED_SEGMENTS`]) — every arm, every target.
pub mod segment_cases {
    use super::fixtures::*;
    use super::*;
    use crate::segments::{SegmentKey, SegmentSink as _};
    use crate::store::*;
    use crate::types::*;
    use fauna_core::custody_policy::SEGMENT_ITEM_CLASS;

    pub async fn adopting_the_same_segment_twice_writes_nothing_the_second_time<M: Medium>() {
        let dir = M::fresh();
        let s = store_on(&dir).await;
        let (dat, meta) = real_segment("post", 1, SEG_ACTOR, &[b"one record"]);

        assert_eq!(
            s.adopt_segment(&seg_post_scope(), &dat, &meta)
                .await
                .unwrap(),
            Some(1)
        );
        assert_eq!(
            s.adopt_segment(&seg_post_scope(), &dat, &meta)
                .await
                .unwrap(),
            None,
            "idempotent: a re-pull of a held segment is not an error"
        );
        assert_eq!(
            s.adopted_segments(&seg_post_scope()).await.unwrap().len(),
            1
        );
    }

    pub async fn a_foreign_actors_segment_never_reaches_the_store<M: Medium>() {
        let dir = M::fresh();
        let s = store_on(&dir).await;
        let (dat, meta) = real_segment("post", 1, [0x11; 32], &[b"not ours"]);

        let err = s
            .adopt_segment(&post_scope(), &dat, &meta)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("different actor"), "got: {err}");
        assert!(s.adopted_segments(&post_scope()).await.unwrap().is_empty());
        assert!(
            !s.is_present(&ContentHash::of_dag_cbor(b"not ours"))
                .await
                .unwrap(),
            "and none of its blocks are readable"
        );
    }

    /// Pin (a): the sidecar's actor is right but its kind is not —
    /// a source cannot relabel a bulky OnDemand kind (`mail`) as an
    /// always-hydrated one (`post`) and have it filed under the wrong scope.
    /// Mutate: delete the kind comparison in [`crate::segments::admit`] and
    /// this test reds — the pair is adopted instead of refused.
    pub async fn a_segment_whose_kind_disagrees_with_the_scope_is_refused<M: Medium>() {
        let dir = M::fresh();
        let s = store_on(&dir).await;
        let (dat, meta) = real_segment("mail", 1, SEG_ACTOR, &[b"mislabelled mail record"]);

        let err = s
            .adopt_segment(&seg_post_scope(), &dat, &meta)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("kind does not match"),
            "got: {err}"
        );
        assert!(
            s.adopted_segments(&seg_post_scope())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !s.is_present(&ContentHash::of_dag_cbor(b"mislabelled mail record"))
                .await
                .unwrap(),
            "and none of its blocks are readable"
        );
    }

    /// A segment transfer the process died in the middle of: the pair was
    /// staged (part of the `.dat`, all of the `.meta`) and never adopted. The
    /// next open finds no segment row — so no row can name a missing file —
    /// and no staging file left in the segment area (the open-time sweep,
    /// `nest/common.md` § Client-state recoverability), and the same segment
    /// then adopts as if nothing had happened. Mutate: drop the open-time
    /// staging sweep and the leftover assertion reds on every arm with a file
    /// area.
    pub async fn a_transfer_interrupted_before_adoption_reopens_clean<M: Medium>() {
        use crate::backend::SegmentStaging as _;
        use crate::segments::SegmentHalf;

        let is_staging = |name: &String| name.starts_with(crate::physical::STAGING_PREFIX);
        let dir = M::fresh();
        let s = store_on(&dir).await;
        let (dat, meta) = real_segment("post", 1, SEG_ACTOR, &[b"one record"]);
        let mut staged = s.backend().segment_stage().await.unwrap();
        staged
            .write(SegmentHalf::Dat, &dat[..dat.len() / 2])
            .await
            .unwrap();
        staged.write(SegmentHalf::Meta, &meta).await.unwrap();
        staged.abandon_as_crash().await;
        if let Some(files) = dir.segment_files().await {
            assert_eq!(
                files.iter().filter(|f| is_staging(f)).count(),
                2,
                "precondition: the crash left both staging files ({files:?})"
            );
        }

        let s = store_on(&dir).await;
        assert!(
            s.adopted_segments(&seg_post_scope())
                .await
                .unwrap()
                .is_empty(),
            "an interrupted transfer adopts nothing"
        );
        assert!(
            s.backend().segment_meter().await.unwrap().is_empty(),
            "and no segment row exists to name a file"
        );
        if let Some(files) = dir.segment_files().await {
            assert!(
                !files.iter().any(is_staging),
                "the reopen swept the crash's staging files: {files:?}"
            );
        }
        assert_eq!(
            s.adopt_segment(&seg_post_scope(), &dat, &meta)
                .await
                .unwrap(),
            Some(1),
            "the same segment then adopts afresh"
        );
    }

    pub async fn a_lost_record_index_rebuilds_from_the_sidecars_record_order<M: Medium>() {
        // The mirror-is-rebuildable property. The segment files and their
        // routing rows are the durable half; the index is a mirror, so a
        // replica that lost it recovers without re-fetching a byte. Modelled
        // by filing the segment through the backend seam directly — i.e. every
        // adoption effect EXCEPT the index rows.
        let dir = M::fresh();
        let s = store_on(&dir).await;
        let bodies: &[&[u8]] = &[b"mail record A", b"mail record B"];
        let (dat, meta) = real_segment("mail", 9, SEG_ACTOR, bodies);
        let admission =
            crate::segments::admit(std::io::Cursor::new(&dat), &meta, &SEG_ACTOR, None).unwrap();
        let mut staged = s.backend().segment_stage().await.unwrap();
        staged
            .write(crate::segments::SegmentHalf::Dat, &dat)
            .await
            .unwrap();
        staged
            .write(crate::segments::SegmentHalf::Meta, &meta)
            .await
            .unwrap();
        s.backend()
            .segment_adopt(
                &SegmentKey {
                    scope: mail_scope(),
                    kind: "mail".into(),
                    segment_id: 9,
                },
                staged,
                &admission.blocks,
            )
            .await
            .unwrap();

        let c = ContentHash::of_dag_cbor(bodies[0]);
        assert!(
            s.record(&c).await.unwrap().is_none(),
            "precondition: the index is gone"
        );
        assert!(
            s.block(&c).await.unwrap().is_some(),
            "but the bytes were never lost"
        );

        assert_eq!(
            s.rebuild_index_from_segments(&mail_scope()).await.unwrap(),
            2
        );
        for body in bodies {
            let row = s
                .record(&ContentHash::of_dag_cbor(body))
                .await
                .unwrap()
                .expect("rebuilt");
            assert_eq!(row.kind, "mail");
            assert_eq!(row.scope, mail_scope());
            assert_eq!(row.size, Some(body.len() as u64));
        }

        // Converges rather than accumulating: a second rebuild is a no-op in
        // effect, which is what makes it safe to run on every boot.
        assert_eq!(
            s.rebuild_index_from_segments(&mail_scope()).await.unwrap(),
            2
        );
        assert_eq!(
            s.records_in_scope(&mail_scope(), None, 100)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    pub async fn a_segment_resident_block_refuses_to_dehydrate<M: Medium>() {
        let dir = M::fresh();
        let s = store_on(&dir).await;
        s.set_hydration_policy(&HydrationPolicy {
            default: Hydration::OnDemand,
            per_kind: Default::default(),
        })
        .await
        .unwrap();
        let (dat, meta) = real_segment("mail", 2, SEG_ACTOR, &[b"a bulky mail record"]);
        s.adopt_segment(&seg_mail_scope(), &dat, &meta)
            .await
            .unwrap()
            .unwrap();

        let c = ContentHash::of_dag_cbor(b"a bulky mail record");
        let err = s.dehydrate(&c).await.unwrap_err();
        assert!(err.to_string().contains("segment eviction"), "got: {err}");
        // The refusal is the honest answer, not a formality: the bytes are
        // still there, so reporting them dropped would be a lie.
        assert!(s.is_present(&c).await.unwrap());
        assert!(s.block(&c).await.unwrap().is_some());
    }

    /// A tombstoned record whose bytes live in an adopted segment: the index
    /// row goes (so no store API can reach it), the immutable segment keeps its
    /// bytes until segment eviction. The honest statement of both halves —
    /// `dehydrate`'s reasoning, applied to deletion.
    pub async fn a_segment_resident_records_tombstone_unindexes_it_without_punching_the_segment<
        M: Medium,
    >() {
        let dir = M::fresh();
        let s = store_on(&dir).await;
        let (dat, meta) = real_segment("mail", 3, SEG_ACTOR, &[b"a mail record later deleted"]);
        s.adopt_segment(&seg_mail_scope(), &dat, &meta)
            .await
            .unwrap()
            .unwrap();
        let c = ContentHash::of_dag_cbor(b"a mail record later deleted");
        assert!(s.record(&c).await.unwrap().is_some());

        assert!(s.apply_tombstone(&c).await.unwrap());
        assert!(
            s.record(&c).await.unwrap().is_none(),
            "unreachable through every store API — the index is the always-present layer"
        );
        assert!(
            s.records_in_scope(&seg_mail_scope(), None, 100)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            s.is_present(&c).await.unwrap(),
            "the CARv2 file is immutable; its bytes wait for segment eviction, and \
             claiming otherwise would be a lie about what was reclaimed"
        );
    }

    pub async fn bootstrap_adopts_a_hydrated_scopes_segments_in_bulk<M: Medium>() {
        let dir = M::fresh();
        let s = store_on(&dir).await;
        let source = fake_source(&[("post", 1, &[b"p1", b"p2"]), ("post", 2, &[b"p3"])]);

        let report = s
            .bootstrap_scope_segments(&seg_post_scope(), &source)
            .await
            .unwrap();
        assert_eq!(report.adopted, 2);
        assert_eq!(report.records_indexed, 3);
        assert_eq!(report.skipped_dehydrating, 0);
        assert_eq!(
            s.records_in_scope(&seg_post_scope(), None, 100)
                .await
                .unwrap()
                .len(),
            3
        );
        assert!(
            s.block(&ContentHash::of_dag_cbor(b"p3"))
                .await
                .unwrap()
                .is_some()
        );

        // Re-running is the resume path: nothing new, nothing lost.
        let again = s
            .bootstrap_scope_segments(&seg_post_scope(), &source)
            .await
            .unwrap();
        assert_eq!(again.adopted, 0);
        assert_eq!(again.already_held, 2);
    }

    pub async fn a_dehydrating_replica_never_fetches_the_bulk_segments<M: Medium>() {
        // The charter's split: verbatim adoption is the bulk path only for
        // scopes the policy hydrates; a dehydrating replica materialises the
        // index from the feed walk alone. The load-bearing assertion is that
        // the segment was never FETCHED — skipping after paying for the
        // download would defeat the policy entirely.
        //
        // Bootstraps the POST scope (not mail's): the OnDemand skip fires on
        // the offer's own kind, before the scope-kind check ever runs, so an
        // OnDemand offer under a scope of a different kind is still skipped
        // here rather than refused — this test is about the skip, not the
        // kind-binding pins below.
        let dir = M::fresh();
        let s = store_on(&dir).await;
        s.set_hydration_policy(&HydrationPolicy {
            default: Hydration::Always,
            per_kind: [("mail".to_string(), Hydration::OnDemand)]
                .into_iter()
                .collect(),
        })
        .await
        .unwrap();
        let source = fake_source(&[
            ("mail", 1, &[b"a bulky mail record"]),
            ("post", 2, &[b"a small post"]),
        ]);

        let report = s
            .bootstrap_scope_segments(&seg_post_scope(), &source)
            .await
            .unwrap();
        assert_eq!(report.skipped_dehydrating, 1, "the mail segment");
        assert_eq!(report.adopted, 1, "the post segment still comes in bulk");
        assert_eq!(report.refused_kind_mismatch, 0);
        assert_eq!(
            *source.fetched.borrow(),
            vec![2],
            "the dehydrated kind's bytes were never pulled"
        );
        assert!(
            !s.is_present(&ContentHash::of_dag_cbor(b"a bulky mail record"))
                .await
                .unwrap()
        );
    }

    /// Pin (b): the offer claims "post" (which this policy always
    /// hydrates), but the fetched sidecar actually says "mail" — only the
    /// fetched bytes prove what a segment really is, so the store refuses it
    /// at adoption and counts the refusal instead of aborting the scope's
    /// whole pull. Mutate: delete the kind comparison in
    /// [`crate::segments::admit`] and this test reds — the mislabelled
    /// segment is adopted.
    pub async fn bootstrap_refuses_a_segment_whose_sidecar_disagrees_with_its_own_offer<
        M: Medium,
    >() {
        let dir = M::fresh();
        let s = store_on(&dir).await;
        let (dat, meta) = real_segment("mail", 1, SEG_ACTOR, &[b"a mislabelled offer"]);
        let source = FakeSource {
            segments: vec![(
                SegmentOffer {
                    kind: "post".to_string(),
                    segment_id: 1,
                    dat_size: Some(dat.len() as u64),
                },
                dat,
                meta,
            )],
            fetched: std::cell::RefCell::new(Vec::new()),
        };

        let report = s
            .bootstrap_scope_segments(&seg_post_scope(), &source)
            .await
            .unwrap();
        assert_eq!(report.refused_kind_mismatch, 1);
        assert_eq!(report.adopted, 0);
        assert_eq!(
            *source.fetched.borrow(),
            vec![1],
            "the offer was trusted enough to fetch — only the sidecar caught the lie"
        );
        assert!(
            s.adopted_segments(&seg_post_scope())
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// The segment plane rides the custody meter: one
    /// [`SEGMENT_ITEM_CLASS`] family per scope, whose payload is every held
    /// `.dat` plus every `.meta`, and whose evictable part is the `.dat` alone
    /// — the sidecar is the index floor. Mutate: drop the segment half from
    /// `AccountStore::custody_meter` (or report `.meta` as evictable) and this
    /// reds.
    pub async fn the_custody_meter_counts_adopted_segments_dat_evictable_meta_floor<M: Medium>() {
        let dir = M::fresh();
        let s = store_on(&dir).await;
        let (dat1, meta1) = real_segment("post", 1, SEG_ACTOR, &[b"p1", b"p2"]);
        let (dat2, meta2) = real_segment("post", 2, SEG_ACTOR, &[b"p3"]);
        s.adopt_segment(&seg_post_scope(), &dat1, &meta1)
            .await
            .unwrap()
            .unwrap();
        s.adopt_segment(&seg_post_scope(), &dat2, &meta2)
            .await
            .unwrap()
            .unwrap();

        let meter = s.custody_meter(&["tombstone"]).await.unwrap();
        let family = meter
            .scopes
            .iter()
            .find(|f| f.item_class == SEGMENT_ITEM_CLASS)
            .expect("the segment plane is metered");
        let dat = (dat1.len() + dat2.len()) as u64;
        let meta = (meta1.len() + meta2.len()) as u64;
        assert_eq!(family.scope, seg_post_scope());
        assert_eq!(family.rows, 2, "one row per segment");
        assert_eq!(family.payload_bytes, dat + meta);
        assert_eq!(family.evictable_rows, 2);
        assert_eq!(family.evictable_bytes, dat, "the sidecars are floor");
        assert_eq!(meter.held_bytes(), dat + meta, "and they reach held_bytes");
    }

    /// Over budget, the segment plane gives up whole `.dat` files, oldest
    /// first, and keeps its metadata floor: the row, the sidecar, the index
    /// rows. The evicted segment's blocks read as dehydrated, re-offering it is
    /// "already held" (no re-download churn), and the survivor still serves.
    /// Mutate: evict newest-first, or drop the `segments` row with the file,
    /// and this reds.
    pub async fn segment_eviction_drops_whole_dat_files_oldest_first_and_keeps_the_metadata_floor<
        M: Medium,
    >() {
        let dir = M::fresh();
        let s = store_on(&dir).await;
        let (dat1, meta1) = real_segment("post", 1, SEG_ACTOR, &[b"p1", b"p2"]);
        let (dat2, meta2) = real_segment("post", 2, SEG_ACTOR, &[b"p3"]);
        for (dat, meta) in [(&dat1, &meta1), (&dat2, &meta2)] {
            s.adopt_segment(&seg_post_scope(), dat, meta)
                .await
                .unwrap()
                .unwrap();
        }
        let held = s.custody_meter(&["tombstone"]).await.unwrap();
        // One byte over: a whole file is the unit, so the older one goes.
        let cap = held.held_bytes() - 1;
        let plan = fauna_core::custody_policy::plan_custody_eviction(&held, cap);
        assert_eq!(
            plan.state,
            fauna_core::custody_policy::CustodyBudgetState::OverBudget
        );

        let freed = s
            .apply_custody_eviction(&plan, &["tombstone"])
            .await
            .unwrap();
        assert_eq!(freed.rows, 1, "one segment");
        assert_eq!(freed.bytes, dat1.len() as u64, "the whole older .dat");

        let older = ContentHash::of_dag_cbor(b"p1");
        let newer = ContentHash::of_dag_cbor(b"p3");
        assert!(!s.is_present(&older).await.unwrap(), "dehydrated");
        assert!(s.block(&older).await.unwrap().is_none());
        assert!(
            s.record(&older).await.unwrap().is_some(),
            "complete in metadata: the index row survives"
        );
        assert!(
            s.block(&newer).await.unwrap().is_some(),
            "the newer one serves"
        );
        assert_eq!(
            s.adopted_segments(&seg_post_scope()).await.unwrap().len(),
            2,
            "both rows stay — the floor"
        );

        let after = s.custody_meter(&["tombstone"]).await.unwrap();
        assert_eq!(
            after.held_bytes(),
            (dat2.len() + meta1.len() + meta2.len()) as u64
        );
        assert!(after.held_bytes() <= cap);

        // Re-offering the evicted segment is not a re-adoption.
        let source = fake_source(&[("post", 1, &[b"p1", b"p2"])]);
        let again = s
            .bootstrap_scope_segments(&seg_post_scope(), &source)
            .await
            .unwrap();
        assert_eq!(again.already_held, 1);
        assert!(source.fetched.borrow().is_empty(), "and never re-fetched");
    }

    /// The custody arm's budget is also the download bound: once the budget is
    /// spent, a declared-too-large offer is skipped before a byte moves, and an
    /// offer this replica already holds is never fetched at all. Mutate:
    /// drop the pre-fetch declared-size check (or the held-key check) and the
    /// `fetched` assertion reds.
    pub async fn a_budgeted_bootstrap_stops_adopting_at_the_budget_and_never_refetches_a_held_segment<
        M: Medium,
    >() {
        let dir = M::fresh();
        let s = store_on(&dir).await;
        let source = fake_source(&[("post", 1, &[b"p1", b"p2"]), ("post", 2, &[b"p3"])]);
        let first_pair = {
            let (_, dat, meta) = &source.segments[0];
            (dat.len() + meta.len()) as u64
        };

        let mut budget = first_pair;
        let report = s
            .bootstrap_scope_segments_within(&seg_post_scope(), &source, &mut budget)
            .await
            .unwrap();
        assert_eq!(report.adopted, 1);
        assert_eq!(report.skipped_over_budget, 1);
        assert_eq!(budget, 0, "the adopted pair was charged");
        assert_eq!(
            *source.fetched.borrow(),
            vec![1],
            "the over-budget offer was refused on its declared size"
        );
        assert_eq!(
            s.custody_meter(&[]).await.unwrap().held_bytes(),
            first_pair,
            "held == what the budget allowed"
        );

        // An unbudgeted pass picks up the rest — and skips the held one
        // without fetching it.
        let rest = s
            .bootstrap_scope_segments(&seg_post_scope(), &source)
            .await
            .unwrap();
        assert_eq!(rest.adopted, 1);
        assert_eq!(rest.already_held, 1);
        assert_eq!(*source.fetched.borrow(), vec![1, 2]);
    }
}

/// The native arms: SQLite over a fresh temp dir per case (the shipped native
/// medium, segment area included), and the memory test double. Web's
/// IndexedDB arm instantiates the same suite in a browser.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod arms {
    use super::{Medium, MemoryMedium};
    use crate::sqlite::SqliteBackend;

    struct SqliteMedium(tempfile::TempDir);

    impl Medium for SqliteMedium {
        type Backend = SqliteBackend;

        fn fresh() -> Self {
            Self(tempfile::tempdir().unwrap())
        }

        async fn open(&self) -> SqliteBackend {
            SqliteBackend::open(self.0.path()).unwrap()
        }

        async fn segment_files(&self) -> Option<Vec<String>> {
            let dir = self.0.path().join(crate::sqlite::SEGMENT_DIR_NAME);
            let Ok(entries) = std::fs::read_dir(dir) else {
                return Some(Vec::new());
            };
            Some(
                entries
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect(),
            )
        }
    }

    mod sqlite {
        crate::store_conformance_suite!(super::SqliteMedium, tokio::test);
    }

    mod memory {
        crate::store_conformance_suite!(super::MemoryMedium, tokio::test);
    }
}
