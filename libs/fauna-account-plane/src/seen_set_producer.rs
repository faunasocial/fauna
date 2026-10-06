//! The **auto-in-set seen-set producer** — the plane's first production
//! writer of `fauna.state.seen-set`.
//!
//! Owner: `docs/goal/architecture/account-data-plane.md` § The replica
//! boundary (R1 (account-data-plane.md § The ratified decisions) + T1, the producer decomposition). R1 admits an item to the
//! account's data plane three ways: first **observation**, **creation** by
//! any of the account's clients, or **delivery** to the account. For the
//! own-actor scopes ([`crate::scope_set::OWN_ACTOR_KINDS`] at the own actor)
//! every item is one of the two non-observational classes — own posts, cards
//! and events and sent mail are creation-class; received mail and delivered
//! invites are delivery-class — so the scope's whole accounted prefix is
//! in-set with **no render event**. That is precisely the licence a
//! [`SeenWatermark`](fauna_core::seen_set::SeenWatermark) raise requires
//! ("only a producer that has actually observed everything at-or-below may
//! raise it; delivery-class scopes … are the natural fit"), and the store's
//! per-scope frontier is that prefix: it advances only past rows the walk
//! accounted. So the producer is arithmetic — after the pump's content
//! walks, raise each own-actor scope's [`WriterId::NEST_SEQUENCER`]
//! watermark to the frontier and publish the entry when membership grew.
//!
//! Because it reads the *frontier* rather than any walk's report, the first
//! pass after an upgrade also heals history: a scope walked for months by a
//! producer-less binary gets its whole accounted prefix into the seen-set on
//! the next pass.
//!
//! # What this is NOT
//!
//! - **Not the T1 body-rendered trigger.** T1 governs *browse* content —
//!   member/subscribed scopes (`conv` channels; followed content scopes as
//!   they land), where an item enters only when its body is materialized for
//!   display. That producer stays itemized ([`SeenRef`](fauna_core::seen_set::SeenRef)s,
//!   never a watermark) and is gated on the first browse surface with
//!   client-side scope-feed coordinates — the charter's T1 note owns the
//!   placement.
//! - **Not read-state.** The seen-set materializes the replica *boundary*;
//!   a delivered-but-unopened mail is correctly watermark-covered. Product
//!   read markers (per-item precision, history) are their own class-2 kind
//!   (charter § The replica boundary, T2 transition 4).
//!
//! # Cadence
//!
//! Full pump passes only — a nudged single-scope walk's rows are covered by
//! the next pass. Nothing renders off the seen-set, late raises are absorbed
//! by the grow-only join, and a crash between walk and raise self-heals the
//! same way (the frontier is durable; the raise re-derives from it).

use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::WriterId;
use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_core::seen_set::SeenScopeSet;
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_SEEN_SET;
use fauna_protocol::scope::ContentScope;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// What one producer pass did. Failures are per-scope (one scope's trouble
/// never blocks the rest) and carried here for the pump's error list.
#[derive(Debug, Default)]
pub struct SeenSetPassReport {
    /// Own-actor scopes considered (walked or not).
    pub scopes: u32,
    /// Entries whose membership grew — each one published.
    pub raised: u32,
    /// Per-scope failures, already labeled with their scope.
    pub errors: Vec<String>,
}

/// One auto-in-set pass: for each own-actor scope, raise the scope's
/// seen-set watermark to the accounted frontier and publish when it grew.
///
/// `own_scopes` must be own-actor scopes only — the caller passes
/// [`crate::scope_set::derive_own_actor_scopes`]'s output, never member
/// scopes (a `conv` scope's items are browse content; watermarking them
/// would assert observations nobody made).
pub async fn auto_in_set_pass<B: StoreBackend, R: RpcRequester>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    own_scopes: &[ContentScope],
) -> SeenSetPassReport {
    let mut report = SeenSetPassReport::default();
    for scope in own_scopes {
        report.scopes += 1;
        let scope_text = scope.to_string();
        if let Err(e) = raise_one(store, plane, &scope_text, &mut report).await {
            report
                .errors
                .push(format!("seen-set ({scope_text}): {e:#}"));
        }
    }
    report
}

async fn raise_one<B: StoreBackend, R: RpcRequester>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    scope_text: &str,
    report: &mut SeenSetPassReport,
) -> anyhow::Result<()> {
    // The accounted prefix: the frontier's nest slot. Zero = never walked (or
    // nothing served yet) — nothing is in-set, nothing to raise.
    let frontier = store
        .frontier(scope_text)
        .await?
        .into_iter()
        .find(|(w, _)| *w == WriterId::NEST_SEQUENCER)
        .map_or(0, |(_, seq)| seq);
    if frontier == 0 {
        return Ok(());
    }

    // Read-modify-write over the replica's merged entry (the reconcile step
    // ran before this pass, so sibling raises are already folded in and a
    // covered raise below is a no-op — the echo-stop).
    let mut set = match store.state(KIND_SEEN_SET, scope_text).await? {
        Some(entry) => canonical_decode::<SeenScopeSet>(&entry.value)
            // Loud skip, never clobber: publishing a fresh set over an entry
            // this binary cannot read would shrink no fleet-wide membership
            // (the join is a union) but would hide a local bug.
            .map_err(|e| anyhow::anyhow!("stored entry does not decode: {e}"))?,
        None => SeenScopeSet::new(),
    };
    if !set.raise_watermark(WriterId::NEST_SEQUENCER.0, frontier) {
        return Ok(()); // already covered — nothing grew, nothing published
    }
    let value = canonical_encode(&set)?;
    plane
        .put(
            &ItemId {
                kind: KIND_SEEN_SET.to_string(),
                key: scope_text.to_string(),
            },
            value,
            // CrdtPerField carries no LwwStamp — the join itself orders.
            None,
        )
        .await?;
    report.raised += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::types::{ItemRef, JournalOp, JournalRow, StateEntry};
    use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
    use fauna_core::data::ContentHash;
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::account_state::ACCOUNT_STATE_SCOPE;

    use super::*;

    /// The pass never publishes over RPC in these tests (pull-only plane), so
    /// any request reaching the wire is itself the failure.
    struct NoNest;

    impl RpcRequester for NoNest {
        type Error = anyhow::Error;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> anyhow::Result<Reply>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            anyhow::bail!("pull-only put reached the wire ({kind}) — it must be local-only")
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        store: AccountStore<SqliteBackend>,
        schedule: AccountStateKeySchedule,
        writer_key: SigningKey,
        trust: crate::generation_tip::GenerationTrust,
        actor: [u8; 32],
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let kp = ActorKeypair::generate();
        let writer_key = SigningKey::from_bytes(&[7u8; 32]);
        let writer = WriterId(writer_key.verifying_key().to_bytes());
        let store = AccountStore::open(
            SqliteBackend::open(dir.path()).unwrap(),
            &kp.actor_id_hex(),
            writer,
        )
        .await
        .unwrap();
        Fixture {
            _dir: dir,
            store,
            schedule: AccountStateKeySchedule::derive(&BackupKey::derive(kp.secret_bytes())),
            writer_key,
            trust: crate::generation_tip::GenerationTrust {
                root: kp.actor_id(),
                prior: Vec::new(),
                trusted_holders: Default::default(),
            },
            actor: fauna_core::hex32::decode(&kp.actor_id_hex()).unwrap(),
        }
    }

    impl Fixture {
        fn plane(&self) -> AccountStatePlane<'_, SqliteBackend, NoNest> {
            AccountStatePlane::new_pull_only(
                &self.store,
                &NoNest,
                &self.schedule,
                &self.writer_key,
                &self.trust,
                ACCOUNT_STATE_SCOPE,
            )
            .expect("plane")
        }

        /// Put the store where a producer-less binary would have left it:
        /// rows accounted (ingest + frontier advance), no seen-set entry.
        async fn account_rows(&self, scope: &str, up_to: u64) {
            for seq in 1..=up_to {
                self.store
                    .ingest_row(&JournalRow {
                        writer: WriterId::NEST_SEQUENCER,
                        seq,
                        scope: scope.to_string(),
                        op: JournalOp::RecordAdded,
                        item: ItemRef::Cid(ContentHash::from_digest_dag_cbor([seq as u8; 32])),
                    })
                    .await
                    .expect("ingest");
                self.store
                    .advance_frontier(scope, &WriterId::NEST_SEQUENCER, seq)
                    .await
                    .expect("advance");
            }
        }
    }

    fn own_scope(kind: &str, actor: [u8; 32]) -> ContentScope {
        ContentScope::new(kind, actor).expect("scope")
    }

    /// The upgrade/healing path: a frontier accounted long before this
    /// producer existed gets its whole prefix into the seen-set on the first
    /// pass — the reason the pass reads the frontier, not a walk report.
    #[tokio::test]
    async fn a_pre_existing_frontier_is_healed_on_the_first_pass() {
        let fx = fixture().await;
        let mail = own_scope("mail", fx.actor);
        let post = own_scope("post", fx.actor);
        fx.account_rows(&mail.to_string(), 3).await;

        let report = auto_in_set_pass(&fx.store, &fx.plane(), &[mail.clone(), post.clone()]).await;
        assert_eq!(report.errors, Vec::<String>::new());
        assert_eq!(report.scopes, 2);
        assert_eq!(report.raised, 1, "only the walked scope raised");

        let entry = fx
            .store
            .state(KIND_SEEN_SET, &mail.to_string())
            .await
            .expect("state")
            .expect("entry");
        let set: SeenScopeSet = canonical_decode(&entry.value).expect("decode");
        assert_eq!(set.watermark_of(&WriterId::NEST_SEQUENCER.0), 3);
        assert!(set.refs.is_empty());
        // A never-walked scope (frontier 0) asserts nothing — no entry.
        assert!(
            fx.store
                .state(KIND_SEEN_SET, &post.to_string())
                .await
                .expect("state")
                .is_none(),
            "zero frontier: nothing is in-set, nothing written"
        );
    }

    /// Per-scope isolation, and the never-clobber rule: a stored entry this
    /// binary cannot decode is reported and left byte-identical, while the
    /// other scopes' raises proceed.
    #[tokio::test]
    async fn an_undecodable_entry_is_skipped_loudly_and_never_clobbered() {
        let fx = fixture().await;
        let mail = own_scope("mail", fx.actor);
        let post = own_scope("post", fx.actor);
        fx.account_rows(&mail.to_string(), 1).await;
        fx.account_rows(&post.to_string(), 2).await;

        let garbage = b"not dag-cbor".to_vec();
        fx.store
            .put_state(StateEntry {
                kind: KIND_SEEN_SET.to_string(),
                key: mail.to_string(),
                scope: ACCOUNT_STATE_SCOPE.to_string(),
                value: garbage.clone(),
                merge_meta: None,
                entry_version: 0,
                tombstone: false,
            })
            .await
            .expect("poison");

        let report = auto_in_set_pass(&fx.store, &fx.plane(), &[mail.clone(), post.clone()]).await;
        assert_eq!(report.raised, 1, "the healthy scope still raised");
        assert_eq!(report.errors.len(), 1, "one loud per-scope failure");
        assert!(
            report.errors[0].contains(&mail.to_string()),
            "the failure names its scope: {:?}",
            report.errors[0]
        );

        let poisoned = fx
            .store
            .state(KIND_SEEN_SET, &mail.to_string())
            .await
            .expect("state")
            .expect("entry");
        assert_eq!(poisoned.value, garbage, "never clobbered");
        let healthy = fx
            .store
            .state(KIND_SEEN_SET, &post.to_string())
            .await
            .expect("state")
            .expect("entry");
        let set: SeenScopeSet = canonical_decode(&healthy.value).expect("decode");
        assert_eq!(set.watermark_of(&WriterId::NEST_SEQUENCER.0), 2);
    }
}
