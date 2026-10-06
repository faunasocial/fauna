//! The class-1 leg of one content scope: walk the nest's feed, apply what
//! moved.
//!
//! Owner: `docs/goal/architecture/account-data-plane.md` § Store logical schema
//! → *the bootstrap contract*, whose second half this is. A fresh replica
//! adopts a scope's segment files verbatim and rebuilds its index from their
//! sidecars — that is the bulk half, `bootstrap_source.rs` — "**then walking
//! the scope's feed from a zero frontier to materialize state entries and
//! tombstones**". Segments are a snapshot of the moment they were pulled; this
//! is how the replica learns what the nest did after that: records added, and
//! records deleted.
//!
//! # It is the class-2 walk's twin, one writer narrower
//!
//! [`crate::account_state_plane`] walks the account-state scope, which is
//! multi-master — every device writes it, so its cursor is a frontier *vector*
//! and its rows carry origin coordinates. A content scope is single-writer by
//! construction: record order is the nest's, never a device's (charter § Feeds
//! and cursors → *Multi-writer fit*), which is why this walk's cursor is the
//! shipped scalar `since` and its rows need no origin columns. The two are the
//! same shape otherwise, deliberately: one feed contract, three scope families.
//!
//! # Where the nest's slot in the frontier comes from
//!
//! The store's cursor primitive is per-writer, so the nest needs a writer slot
//! — but not its key, which W2.3 (account-data-plane.md § Workstreams) ruled the client never discovers. The slot is
//! the reserved [`WriterId::NEST_SEQUENCER`], and the rows this walk ingests
//! are attributed to it, which is what lets the ordinary accounting law
//! (advance only over rows actually held) govern a nest-sequenced feed
//! unchanged.
//!
//! # The walk's ingest feeds the relay plane
//!
//! Every coordinate-valid row this walk sees also lands verbatim in the
//! store's relay plane (`item_class = record-cid`, no inline entry — the
//! coordinates ARE the payload), which is what lets this replica serve the
//! scope's feed onward to a same-account peer (`fauna_peer_sync::server`) —
//! the W3 half of R6 (account-data-plane.md § The ratified decisions)'s verbatim relay. A content scope is one-writer (the
//! nest), so the walk's ingest is the relay plane's only feeding point: there
//! is no local publish to feed it from, and the bulk bootstrap half adopts
//! segments without feed rows — its mandated zero-frontier walk is what
//! backfills the plane. Recording is **unconditional for every
//! coordinate-valid row, unknown ops included** — the same
//! non-editorializing rule as the class-2 walk: the nest would serve that
//! row to every replica too, each reader makes its own skip decision, and
//! withholding it here would only make peers' views diverge from the nest's.
//!
//! # A refusal is not an empty page
//!
//! Charter ruling 4 (§ Feeds and cursors → *The scope string*): a nest serves a
//! content scope only for kinds it knows, and to a bootstrapping replica a
//! refusal means *this nest cannot serve this scope yet* — version skew, not
//! absence. An empty scope is a **success** answer (an empty page); a refusal
//! is an [`Err`] here and never a zero-row report, so a caller cannot mistake
//! one for the other and record a refused scope as converged-empty. Pinned by
//! `a_kind_this_nest_does_not_serve_is_a_refusal_not_an_empty_walk`.

use anyhow::{Context, Result};

use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::{
    ItemRef, JournalOp, JournalRow, RecordIndexEntry, RelayRow, WriterId,
};
use fauna_core::data::ContentHash;
use fauna_protocol::RpcRequester;
use fauna_protocol::account_state::ItemClass;
use fauna_protocol::scope::ContentScope;
use fauna_protocol::sync::{SyncChange, SyncChangesListReply, SyncChangesListRequest};

/// What one walk did. Every field is a count of rows *this* walk accounted, so
/// a caller can tell "nothing had changed" (all zero) from "the scope is
/// empty" (also all zero, but reached from a zero cursor) by the cursor it
/// asked with — and from "this nest does not serve the scope" by the `Err`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ContentWalkReport {
    /// The scope this walk covered, canonically spelled. Carried on the report
    /// itself so a caller holding several (the pump's per-pass vector) reads
    /// each one's subject off the report instead of re-deriving the order it
    /// registered them in.
    pub scope: String,
    /// Feed pages fetched. A walk that found nothing still fetches one.
    pub pages: u32,
    /// Rows seen across those pages.
    pub rows: u32,
    /// Records this walk indexed (an arrival, or a re-presented record whose
    /// index row was already there — the upsert is idempotent).
    pub indexed: u32,
    /// Records this walk removed on a tombstone.
    pub tombstoned: u32,
    /// Rows whose op this binary does not know — skipped and left unaccounted,
    /// exactly as the class-2 walk leaves an unopenable entry, so a later
    /// reconcile re-presents them after an upgrade rather than the frontier
    /// swallowing them.
    pub unknown_op: u32,
}

impl crate::page_walk::PageTally for ContentWalkReport {
    fn page_fetched(&mut self) {
        self.pages += 1;
    }
}

/// The class-1 leg of one content scope.
pub struct ContentScopePlane<'a, B: StoreBackend, R: RpcRequester> {
    store: &'a AccountStore<B>,
    rpc: &'a R,
    scope: ContentScope,
    /// The canonical scope string, computed once — it is the at-rest key of
    /// every row this walk writes and the feed request's `scope` both, and
    /// re-deriving it per row would invite the two to drift.
    scope_text: String,
    /// The custodied account this walk addresses, when the walk is a
    /// CUSTODIAN's nest leg rather than the account's own. `None` is
    /// the shipped self-pull: the nest resolves the plane from the connection
    /// actor. `Some(owner_hex)` names the owner, and the nest re-derives the
    /// custody verdict from the live capability row on EVERY request — the same
    /// addressing field and the same per-request re-check the class-2 walk's
    /// `custody_pull` uses, so revoke severs this walk mid-flight too.
    of_owner: Option<String>,
}

impl<'a, B: StoreBackend, R: RpcRequester> ContentScopePlane<'a, B, R> {
    pub fn new(store: &'a AccountStore<B>, rpc: &'a R, scope: ContentScope) -> Self {
        let scope_text = scope.to_string();
        Self {
            store,
            rpc,
            scope,
            scope_text,
            of_owner: None,
        }
    }

    /// Address a custodied owner — the custodian's nest leg. The
    /// scope must be one the custodian's grant covers; the nest is the
    /// authority on that and refuses loudly otherwise (it never answers an
    /// empty page, which a walking replica would record as converged-empty).
    #[must_use]
    pub fn of_owner(mut self, owner: [u8; 32]) -> Self {
        self.of_owner = Some(hex::encode(owner));
        self
    }

    /// The scope's canonical string — the same one the store files rows under.
    pub fn scope(&self) -> &str {
        &self.scope_text
    }

    /// Walk forward from what this replica has already accounted: the
    /// incremental path, and what a `fauna.sync.changed` nudge should trigger.
    pub async fn walk(&self) -> Result<ContentWalkReport> {
        let start = self.stored_cursor().await?;
        self.run(start).await
    }

    /// The full-state reconcile: the same walk from a **zero** cursor, which
    /// the feed answers with the scope's entire current state — one row per
    /// record, live or tombstoned — rather than a replay of history, because
    /// the nest derives the feed from its record mirror instead of retaining an
    /// event log.
    ///
    /// This is the backstop for the two cases the incremental walk cannot see:
    /// a replica that missed rows, and — the one that has no other answer — a
    /// tombstone the nest's own compaction reclaimed before this replica walked
    /// past it. Both converge here, because what comes back is *truth*, not
    /// deltas.
    pub async fn reconcile(&self) -> Result<ContentWalkReport> {
        self.run(0).await
    }

    async fn stored_cursor(&self) -> Result<i64> {
        Ok(self
            .store
            .frontier(&self.scope_text)
            .await?
            .into_iter()
            .find(|(w, _)| *w == WriterId::NEST_SEQUENCER)
            .map_or(0, |(_, seq)| seq as i64))
    }

    /// The class-1 page loop. Its *law* is `crate::page_walk`'s, shared with
    /// the class-2 walks — including the spin refusal, whose single-writer
    /// form ("advanced the cursor past N", not "no writer cursor") is carried
    /// by `PageCursor for i64` rather than repeated here: this scope's cursor
    /// is one nest sequence number, so only a strict increase is progress.
    async fn run(&self, cursor: i64) -> Result<ContentWalkReport> {
        crate::page_walk::drive(
            cursor,
            ContentWalkReport {
                scope: self.scope_text.clone(),
                ..Default::default()
            },
            &format!("content-scope walk ({})", self.scope_text),
            async |cursor: &i64| {
                let reply: SyncChangesListReply = self
                    .rpc
                    .request(
                        "fauna.sync.changes.list",
                        SyncChangesListRequest {
                            since: *cursor,
                            item_class: Some(ItemClass::RecordCid.as_wire().to_string()),
                            scope: Some(self.scope_text.clone()),
                            // Deliberately absent: a content scope has exactly one
                            // writer, so there is no device slot to carry. `since`
                            // IS its frontier (charter: "an omitted frontier is
                            // `{nest: since}`").
                            frontier: None,
                            // `None` on a self-pull; the custodied owner on a
                            // custodian's nest leg.
                            of_owner: self.of_owner.clone(),
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "fauna.sync.changes.list (record-cid, scope {}): {e}",
                            self.scope_text
                        )
                    })?;
                Ok(reply)
            },
            async |change: &SyncChange, report: &mut ContentWalkReport, cursor: &mut i64| {
                self.apply(change, report, cursor).await
            },
        )
        .await
    }

    async fn apply(
        &self,
        change: &SyncChange,
        report: &mut ContentWalkReport,
        cursor: &mut i64,
    ) -> Result<()> {
        report.rows += 1;
        // Seen — whatever the row turns out to be, this page has shown it to
        // us, so paging always moves even when the row itself is skipped.
        *cursor = (*cursor).max(change.seq);

        let cid = record_cid(change)?;

        // The relay plane (module docs: *The walk's ingest feeds the relay
        // plane*): the verbatim coordinate row, held so this replica can serve
        // the scope onward peer-wise. Before the journal ingest, like the
        // class-2 walk — the frontier only advances at the end, so a crash
        // between the two re-presents the row (the put is idempotent).
        self.store
            .record_relay_row(&RelayRow {
                scope: self.scope_text.clone(),
                item_class: ItemClass::RecordCid.as_wire().to_string(),
                writer: WriterId::NEST_SEQUENCER,
                writer_seq: change.seq as u64,
                item_key: cid.digest().to_vec(),
                op: change.change_type.clone(),
                entry: None,
                feed_seq: u64::try_from(change.seq).ok(),
            })
            .await
            .context("content-scope walk: relay plane")?;

        let op = match JournalOp::parse(&change.change_type) {
            Ok(op @ (JournalOp::RecordAdded | JournalOp::Tombstone)) => op,
            // `state-put` on a content scope, or a spelling from a newer nest.
            // Skipped and left unaccounted rather than guessed at.
            _ => {
                report.unknown_op += 1;
                return Ok(());
            }
        };

        let row = JournalRow {
            writer: WriterId::NEST_SEQUENCER,
            seq: change.seq as u64,
            scope: self.scope_text.clone(),
            op,
            item: ItemRef::Cid(cid),
        };
        // The row lands first, then its effect: the frontier's accounting law
        // refuses to advance past a row the store does not hold, so a crash
        // between the two re-presents the row on the next walk instead of
        // losing it.
        self.store.ingest_row(&row).await?;

        match op {
            JournalOp::RecordAdded => {
                // Index-only. Whether the bytes ever arrive is hydration
                // policy's call — "the always-present layer is the index, not
                // the blocks" — and for a hydrating replica the bulk half has
                // usually brought them already.
                self.store
                    .note_record(&RecordIndexEntry {
                        cid,
                        scope: self.scope_text.clone(),
                        kind: self.scope.kind().to_string(),
                        size: None,
                    })
                    .await?;
                report.indexed += 1;
            }
            JournalOp::Tombstone => {
                self.store.apply_tombstone(&cid).await?;
                report.tombstoned += 1;
            }
            JournalOp::StatePut => unreachable!("filtered above"),
        }

        self.store
            .advance_frontier(&self.scope_text, &WriterId::NEST_SEQUENCER, row.seq)
            .await?;
        Ok(())
    }
}

/// Rebuild a row's full record CID from the digest the feed carries.
///
/// The wire slot is `path_hash` and it holds the **digest**, per the charter's
/// feed-row shape ("for class-1 items the record CID's digest"). The codec half
/// is not sent because it is not a variable: every record on this plane is
/// dag-cbor-coded — that is the same assumption the nest's own mirror makes
/// when it reconstructs a post's CID from its `post_id`, and the assumption the
/// `record_cid` widening migration backfilled every historical row under.
fn record_cid(change: &SyncChange) -> Result<ContentHash> {
    let bytes = hex::decode(&change.path_hash)
        .with_context(|| format!("record-cid row at seq {}: path_hash is not hex", change.seq))?;
    let digest: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!(
            "record-cid row at seq {}: path_hash is {} bytes, want a 32-byte digest",
            change.seq,
            bytes.len()
        )
    })?;
    Ok(ContentHash::from_digest_dag_cbor(digest))
}

/// Which nest-side segments make up one of the store's scopes.
///
/// `scope` is the store's own scope string, opaque here; `kinds` are the
/// segment-store kind tags whose segments belong to it, and `actor_hex` is the
/// scope id the nest enumerates on (own-actor scopes today — the owner's
/// actor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeBinding {
    pub scope: String,
    pub kinds: Vec<String>,
    pub actor_hex: String,
}

/// A helper for callers that hold a `(kind, scope_id)` pair and want the walk's
/// bootstrap sibling to agree with it by construction — the seam the scope-string
/// ruling left open (`bootstrap_source.rs` § *The scope string is ratified*).
///
/// The fetch address (`actor_hex`) is the scope's own id, derived here rather
/// than passed, so the two cannot disagree. For the four single-principal kinds
/// that is the owner; for a co-authored kind (`conv`) it is the **channel** —
/// the serve plane's actor field carries the scope id for every kind
/// (`message-segment-store.md` § *Which kinds the two planes serve*).
pub fn binding_for(scope: &ContentScope) -> ScopeBinding {
    ScopeBinding {
        scope: scope.to_string(),
        kinds: vec![scope.kind().to_string()],
        actor_hex: hex::encode(scope.scope_id()),
    }
}

/// The nest-sequencer slot is a reserved name, not a key — and nothing derives
/// it from one. Kept here beside its only consumer so the constant cannot drift
/// into looking like a device id.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nest_sequencer_slot_is_a_readable_label() {
        let hex = WriterId::NEST_SEQUENCER.to_hex();
        assert_eq!(hex.len(), 64);
        let decoded = hex::decode(&hex).unwrap();
        assert!(
            String::from_utf8_lossy(&decoded).starts_with("fauna:nest-sequencer"),
            "the slot should read as itself in a hex dump, got {hex}"
        );
    }

    #[test]
    fn a_records_cid_round_trips_through_the_feeds_digest_slot() {
        let original = ContentHash::of_dag_cbor(b"a post body the nest filed");
        let change = SyncChange {
            seq: 7,
            path_hash: hex::encode(original.digest()),
            ..Default::default()
        };
        assert_eq!(
            record_cid(&change).unwrap(),
            original,
            "the digest slot is enough to name the record again"
        );
    }

    #[test]
    fn a_path_hash_that_is_not_a_digest_is_refused_rather_than_padded() {
        for bad in ["", "zz", &"ab".repeat(31), &"ab".repeat(33)] {
            let change = SyncChange {
                seq: 1,
                path_hash: bad.to_string(),
                ..Default::default()
            };
            assert!(record_cid(&change).is_err(), "must refuse {bad:?}");
        }
    }

    #[test]
    fn the_binding_helper_and_the_scope_string_cannot_disagree() {
        let scope = ContentScope::new("post", [0xC3; 32]).unwrap();
        let binding = binding_for(&scope);
        assert_eq!(binding.scope, scope.to_string());
        assert_eq!(binding.kinds, vec!["post".to_string()]);
        assert_eq!(binding.actor_hex, hex::encode([0xC3; 32]));

        // A co-authored scope addresses by its CHANNEL — the fetch address is
        // the scope id for every kind, derived, never passed.
        let conv = ContentScope::new("conv", [0xCE; 32]).unwrap();
        assert_eq!(binding_for(&conv).actor_hex, hex::encode([0xCE; 32]));
    }
}
