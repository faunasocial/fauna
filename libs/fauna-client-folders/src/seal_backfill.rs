//! The S8 **seal-backfill sweep** — the one session-start pass every app
//! runs to catch up rows a prior (unwired or keyless) session left
//! plaintext-only: D1 over the folder plane, then D3 over each owned set's
//! snapshot tags (`docs/goal/behavior/path-sealing.md` § Implementation status
//! today).
//!
//! # Why the sweep lives here and not in each app
//!
//! The two passes themselves are shared already
//! ([`FoldersClient::backfill_sealed_fields`] + [`SnapshotsClient::backfill_tag_seals`]),
//! but the **sequencing around them** — D1 first, then the roster, skip
//! `role == "member"`, D3 per remaining set, every step independently
//! best-effort — was hand-written once per client: the since-removed headless
//! daemon, windows' `SealBackfillSweep.cs`, apple's `FaunaClient.runSealBackfill`,
//! android's `MailEnableGlueVM.runSealBackfill`. Four copies of one policy,
//! two of which are the sort of thing that silently drifts (the member skip is
//! a *correctness* rule, not a nicety — see below). It is written once here
//! instead (priority #2/#4), generic over the transport like every other seam
//! in this crate: native `Arc<NestClient>`, wasm `WsRpcClient`.
//!
//! # The two rules the sequencing carries
//!
//! - **Owner-run only.** A member's stamp is one the S9 flip's
//!   scrub cannot attribute to the owner or creator, so a member-side backfill
//!   would stamp seals the flip then disowns. Rows whose `role` is `"member"`
//!   are skipped; the owner's own client converges the set.
//! - **Best-effort throughout, never fatal.** An unreachable nest, a refused
//!   update, one bad set — each is counted and the pass moves on, because the
//!   whole sweep reruns at the next session start. Nothing here returns `Err`:
//!   a caller wires it fire-and-forget at its post-auth hook and reads the
//!   report only to log.
//!
//! What stays *out* of the sequencing is the fail-closed custody logic: whether
//! a given row may seal at all is decided inside the two passes, from the row's
//! own bound-ness and the custody's resolve. This module never inspects keys.

use fauna_client_snapshots::{SnapshotsClient, TagSealBackfillReport};
use fauna_protocol::RpcRequester;

use crate::{FoldersClient, SealBackfillReport};

/// Report of one [`sweep_with`] pass. All-zero/`None`-free = converged with no
/// trouble, the steady state after the first pass on a given nest.
///
/// Failures are *fields*, not an `Err`: the sweep is best-effort by contract,
/// and its callers log rather than branch. The two error strings carry the
/// transport's own `Display` text (never a set name — S7).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SealBackfillSweepReport {
    /// The D1 folder-plane pass, or `None` when it could not run at all
    /// (see [`Self::fields_error`]).
    pub fields: Option<SealBackfillReport>,
    /// Why the D1 pass could not run (nest unreachable at session start is the
    /// ordinary case).
    pub fields_error: Option<String>,
    /// The D3 snapshot-tag pass, **summed over every set swept**.
    pub tags: TagSealBackfillReport,
    /// Owned sets the D3 pass visited.
    pub sets_swept: usize,
    /// Sets skipped because this actor is a *member*, not the owner.
    pub member_sets_skipped: usize,
    /// Sets whose D3 call failed outright (transport fault) — skipped, the
    /// sweep reruns at the next start. Distinct from
    /// [`TagSealBackfillReport::stamp_failures`], which counts per-snapshot
    /// stamp submissions inside a set that *was* reached.
    pub set_failures: usize,
    /// Why the roster read before the D3 loop failed, when it did (no set was
    /// swept in that case).
    pub roster_error: Option<String>,
}

impl SealBackfillSweepReport {
    /// True when the pass stamped something or hit trouble — i.e. when it is
    /// worth a log line at all. The converged steady state is silent.
    pub fn is_noteworthy(&self) -> bool {
        self.fields_error.is_some()
            || self.roster_error.is_some()
            || self.set_failures > 0
            || self.tags.stamped > 0
            || self.tags.stamp_failures > 0
            || self.fields.as_ref().is_some_and(|f| {
                f.stamped() > 0 || f.update_failures > 0 || f.identity_mismatch > 0
            })
    }
}

/// Run the sweep against two already-built clients — **the sequencing seam**,
/// which is what makes the policy unit-testable without a nest or a resolver.
///
/// Both clients must carry the caller's label custody
/// ([`FoldersClient::with_label_custody`] / [`SnapshotsClient::with_label_custody`]);
/// [`run_sweep`] is the wiring that builds them that way. A custody-less pair
/// is not an error — it simply stamps nothing (fail closed), which is the
/// deliberate behavior for an unkeyed session.
pub async fn sweep_with<R: RpcRequester + Clone>(
    folders: &FoldersClient<R>,
    snapshots: &SnapshotsClient<R>,
) -> SealBackfillSweepReport {
    let mut report = SealBackfillSweepReport::default();

    match folders.backfill_sealed_fields().await {
        Ok(r) => report.fields = Some(r),
        Err(e) => report.fields_error = Some(e.to_string()),
    }

    // The roster read is deliberately a second read rather than a reuse of
    // D1's: that pass reads the RAW wire list (its predicates must see the
    // resting truth, not `list`'s rendered one), and a `role` is all this loop
    // needs. A failure here degrades identically — next start retries.
    let reply = match folders.list().await {
        Ok(reply) => reply,
        Err(e) => {
            report.roster_error = Some(e.to_string());
            return report;
        }
    };

    for row in &reply.folders {
        if row.role.as_deref() == Some("member") {
            report.member_sets_skipped += 1;
            continue;
        }
        match snapshots.backfill_tag_seals(&row.name).await {
            Ok(r) => {
                report.sets_swept += 1;
                report.tags.stamped += r.stamped;
                report.tags.unsealable += r.unsealable;
                report.tags.stamp_failures += r.stamp_failures;
            }
            // Per-set, not fatal: one unreachable set never costs the rest.
            Err(_) => report.set_failures += 1,
        }
    }

    report
}

#[cfg(feature = "mls")]
pub use wired::{resolver_backed_custody, run_sweep};

#[cfg(feature = "mls")]
mod wired {
    use std::sync::Arc;

    use fauna_client_snapshots::SnapshotsClient;
    use fauna_core::crypto::BackupKey;
    use fauna_core::folder_keys::FolderKeyResolver;
    use fauna_core::identity::ActorKeypair;
    use fauna_core::label_custody::LabelCustody;
    use fauna_protocol::RpcRequester;

    use super::{SealBackfillSweepReport, sweep_with};
    use crate::{FolderKeyReader, FoldersClient, NestFolderKeyResolver};

    /// The resolver-backed label custody every keyed client seals with: the
    /// shared-set resolver **and** the owner key, both arms.
    ///
    /// This is the one constructor because the alternative — hand-assembling it
    /// per call site — is exactly how a site reverts to
    /// [`LabelCustody::owner_only`], which is: a bound set's
    /// fields silently seal under the owner root where no roster member can
    /// follow, and it looks like a graceful degrade. The resolver arm is what
    /// makes a bound-but-unresolvable set fail *closed* instead.
    ///
    /// The `NestFolderKeyResolver<R>: FolderKeyResolver` bound is what carries
    /// the transport-generic property: the trait is implemented for the native
    /// `Arc<NestClient>` and the wasm `WsRpcClient` (`custody_ingest.rs`), so
    /// this compiles for exactly the transports that have a resolver and for no
    /// others.
    pub fn resolver_backed_custody<R>(
        nest: R,
        keypair: &ActorKeypair,
        folder_keys: Arc<dyn FolderKeyReader>,
    ) -> LabelCustody
    where
        R: RpcRequester + Clone,
        NestFolderKeyResolver<R>: FolderKeyResolver + 'static,
    {
        LabelCustody::new(
            Some(Arc::new(NestFolderKeyResolver::new(nest, folder_keys))),
            Some(BackupKey::derive(keypair.secret_bytes())),
        )
    }

    /// The whole session-start sweep from a transport + the actor's keypair —
    /// what a client's post-auth hook calls.
    ///
    /// Builds both clients over one resolver-backed custody
    /// ([`resolver_backed_custody`]) and runs [`sweep_with`]. Never fails; see
    /// the module docs for the best-effort contract.
    pub async fn run_sweep<R>(
        nest: R,
        keypair: &ActorKeypair,
        folder_keys: Arc<dyn FolderKeyReader>,
    ) -> SealBackfillSweepReport
    where
        R: RpcRequester + Clone,
        NestFolderKeyResolver<R>: FolderKeyResolver + 'static,
    {
        let custody = resolver_backed_custody(nest.clone(), keypair, folder_keys);
        let folders = FoldersClient::new(nest.clone()).with_label_custody(custody.clone());
        let snapshots = SnapshotsClient::new(nest).with_label_custody(custody);
        sweep_with(&folders, &snapshots).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use fauna_protocol::folders::{FolderSummary, FoldersListReply};

    /// A requester serving the four kinds a sweep touches, recording every
    /// call in order, with per-kind failure injection.
    ///
    /// Not [`fauna_client_testkit::RecordingRequester`]: that double keeps only
    /// the LAST call and is `Infallible`, and a sweep is *precisely* an
    /// assertion about call ORDER under partial failure.
    struct SweepRequester {
        sets: Vec<FolderSummary>,
        snapshots: Vec<fauna_protocol::filesync::SnapshotSummaryRow>,
        calls: std::sync::Mutex<Vec<(&'static str, Vec<u8>)>>,
        /// Fail `fauna.folders.list` from this call index on (0-based over
        /// that kind alone) — D1 reads it first, the D3 roster second.
        fail_list_from: Option<usize>,
        /// Fail `fauna.filesync.snapshot.list` for this set name.
        fail_snapshots_for: Option<&'static str>,
    }

    impl SweepRequester {
        fn new(sets: Vec<FolderSummary>) -> Self {
            Self {
                sets,
                snapshots: Vec::new(),
                calls: std::sync::Mutex::new(Vec::new()),
                fail_list_from: None,
                fail_snapshots_for: None,
            }
        }

        fn kinds(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().iter().map(|(k, _)| *k).collect()
        }

        /// The `folder` argument of every `fauna.filesync.snapshot.list`, in
        /// order — i.e. which sets the D3 loop actually reached.
        /// The roster names each snapshot read addressed (by hash — the funnel
        /// takes the plaintext off the wire).
        fn swept_sets(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| *k == "fauna.filesync.snapshot.list")
                .filter_map(|(_, b)| {
                    let req = fauna_protocol::decode_strict::<
                        fauna_protocol::filesync::SnapshotListRequest,
                    >(b)
                    .ok()?;
                    self.sets
                        .iter()
                        .find(|s| fauna_protocol::folders::SetAddressed::addresses(&req, &s.name))
                        .map(|s| s.name.clone())
                })
                .collect()
        }
    }

    impl RpcRequester for SweepRequester {
        type Error = String;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            self.calls.lock().unwrap().push((kind, bytes.to_vec()));
            let reply = match kind {
                "fauna.folders.list" => {
                    let seen = self
                        .calls
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|(k, _)| *k == "fauna.folders.list")
                        .count()
                        - 1;
                    if self.fail_list_from.is_some_and(|from| seen >= from) {
                        return Err("list unreachable".into());
                    }
                    fauna_protocol::encode_canonical(&FoldersListReply {
                        folders: self.sets.clone(),
                        extra: Default::default(),
                    })
                }
                "fauna.folders.update" => {
                    fauna_protocol::encode_canonical(&crate::folders::FolderUpdateReply {
                        ok: true,
                        extra: Default::default(),
                    })
                }
                "fauna.filesync.snapshot.list" => {
                    let req: fauna_protocol::filesync::SnapshotListRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode snapshot list");
                    if self.fail_snapshots_for.is_some_and(|name| {
                        fauna_protocol::folders::SetAddressed::addresses(&req, name)
                    }) {
                        return Err("snapshot list unreachable".into());
                    }
                    fauna_protocol::encode_canonical(&fauna_protocol::filesync::SnapshotListReply {
                        rows: self.snapshots.clone(),
                        extra: Default::default(),
                    })
                }
                "fauna.filesync.snapshot.stamp_labels" => fauna_protocol::encode_canonical(
                    &fauna_protocol::filesync::SnapshotStampLabelsReply {
                        ok: true,
                        extra: Default::default(),
                    },
                ),
                other => panic!("SweepRequester: unhandled kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    fn set(name: &str, role: Option<&str>) -> FolderSummary {
        FolderSummary {
            id: 1,
            name: name.into(),
            role: role.map(str::to_string),
            ..Default::default()
        }
    }

    fn custody() -> fauna_core::label_custody::LabelCustody {
        fauna_core::label_custody::LabelCustody::owner_only(
            fauna_core::crypto::BackupKey::from_bytes([7u8; 32]),
        )
    }

    fn clients(
        req: std::sync::Arc<SweepRequester>,
    ) -> (
        FoldersClient<std::sync::Arc<SweepRequester>>,
        SnapshotsClient<std::sync::Arc<SweepRequester>>,
    ) {
        (
            FoldersClient::new(req.clone()).with_label_custody(custody()),
            SnapshotsClient::new(req).with_label_custody(custody()),
        )
    }

    /// D1 runs first and exactly once; the D3 loop then visits every OWNED set
    /// and never the member one (a member's stamp is one the S9
    /// scrub disowns).
    ///
    /// Mutation: drop the `role == "member"` skip → `swept_sets` gains
    /// `"shared-with-me"` and `member_sets_skipped` falls to 0.
    #[test]
    fn sweep_runs_d1_then_d3_per_owned_set_and_skips_member_sets() {
        let req = std::sync::Arc::new(SweepRequester::new(vec![
            set("docs", Some("owner")),
            set("shared-with-me", Some("member")),
            // No role at all is the owner-scoped `fauna.folders.list`
            // contract's own default — swept, not skipped.
            set("photos", None),
        ]));
        let (folders, snapshots) = clients(req.clone());

        let report = block_on(sweep_with(&folders, &snapshots));

        assert_eq!(
            req.kinds().first(),
            Some(&"fauna.folders.list"),
            "D1's own raw list read must come first — the sweep is D1 THEN D3"
        );
        assert_eq!(
            req.swept_sets(),
            vec!["docs".to_string(), "photos".to_string()],
            "every owned set is swept, in roster order, and the member set never is"
        );
        assert_eq!(report.sets_swept, 2);
        assert_eq!(report.member_sets_skipped, 1);
        assert!(report.fields.is_some(), "the D1 pass ran");
        assert_eq!(report.set_failures, 0);
        assert_eq!(report.roster_error, None);
    }

    /// One set's D3 call failing costs that set only — the rest of the roster
    /// is still swept, and the failure is counted rather than raised.
    ///
    /// Mutation: propagate the per-set `Err` instead of counting it → `photos`
    /// never appears in `swept_sets`.
    #[test]
    fn a_failing_set_does_not_abort_the_rest_of_the_sweep() {
        let mut inner = SweepRequester::new(vec![set("docs", Some("owner")), set("photos", None)]);
        inner.fail_snapshots_for = Some("docs");
        let req = std::sync::Arc::new(inner);
        let (folders, snapshots) = clients(req.clone());

        let report = block_on(sweep_with(&folders, &snapshots));

        assert_eq!(report.set_failures, 1);
        assert_eq!(report.sets_swept, 1, "the second set was still swept");
        assert_eq!(
            req.swept_sets(),
            vec!["docs".to_string(), "photos".to_string()],
            "the failing set was attempted and the next one still ran"
        );
    }

    /// The D1 pass's result survives a roster read that fails *after* it: the
    /// sweep reports both halves independently, because they retry
    /// independently at the next session start.
    ///
    /// Mutation: return early on the D1 error without recording `fields` →
    /// this pin reds on the `fields.is_some()` assertion.
    #[test]
    fn a_roster_failure_after_d1_keeps_the_d1_result_and_sweeps_nothing() {
        let mut inner = SweepRequester::new(vec![set("docs", Some("owner"))]);
        // D1 reads the list first (index 0) and succeeds; the D3 roster read
        // (index 1) is the one that fails.
        inner.fail_list_from = Some(1);
        let req = std::sync::Arc::new(inner);
        let (folders, snapshots) = clients(req.clone());

        let report = block_on(sweep_with(&folders, &snapshots));

        assert!(report.fields.is_some(), "the D1 pass ran and is reported");
        assert_eq!(report.fields_error, None);
        assert!(report.roster_error.is_some(), "the roster failure is named");
        assert_eq!(report.sets_swept, 0);
        assert!(
            req.swept_sets().is_empty(),
            "no set is swept when the roster read fails"
        );
        assert!(
            report.is_noteworthy(),
            "a failed roster read is worth a log"
        );
    }

    /// **The pin, moved to the one place the shape is now decided.**
    /// `resolver_backed_custody` is the single constructor every app's sweep
    /// builds through ([`run_sweep`] takes a transport + keypair, never a
    /// custody), so a client physically cannot hand its sweep an owner-only
    /// custody the way a hand-assembled call site could — and this asserts the
    /// one constructor carries BOTH arms.
    ///
    /// Why both: the **resolver** so a bound set's fields seal under the M2
    /// generation its roster can open — owner-only there is a
    /// silent seal where no member can follow — and the **owner key** so an
    /// unbound set still seals at all (the positive-control arm; a resolver-only
    /// custody would stamp nothing and the sweep would look converged).
    ///
    /// Mutation: return `LabelCustody::owner_only(..)` from the constructor →
    /// `has_resolver()` is false → exactly this pin reds. No connection is made:
    /// `NestClient::new` only constructs, as in `fauna-ffi`'s twin pin.
    #[cfg(all(feature = "mls", not(target_arch = "wasm32")))]
    #[test]
    fn the_sweeps_one_custody_constructor_carries_both_arms() {
        let keypair = fauna_core::identity::ActorKeypair::from_secret([7u8; 32]);
        let nest = fauna_client::NestClient::new(
            "wss://unreachable.invalid".into(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        );

        let custody = super::resolver_backed_custody(
            nest,
            &keypair,
            std::sync::Arc::new(crate::MemoryFolderKeyStore::default()),
        );

        assert_eq!(
            (custody.has_resolver(), custody.has_owner_key()),
            (true, true),
            "the sweep seals through this custody on every app, web included: \
             without the resolver a bound set seals under the owner root (unopenable by its roster), without the owner key an unbound set \
             seals not at all"
        );
    }

    /// An unreachable nest at session start is the ordinary case, and it is not
    /// an error: both halves report their own failure and the caller carries on.
    #[test]
    fn an_unreachable_nest_reports_both_halves_without_failing() {
        let mut inner = SweepRequester::new(vec![set("docs", Some("owner"))]);
        inner.fail_list_from = Some(0);
        let req = std::sync::Arc::new(inner);
        let (folders, snapshots) = clients(req);

        let report = block_on(sweep_with(&folders, &snapshots));

        assert!(report.fields.is_none());
        assert!(report.fields_error.is_some());
        assert!(report.roster_error.is_some());
        assert_eq!(report.sets_swept, 0);
    }
}
