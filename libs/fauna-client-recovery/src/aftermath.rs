//! The post-succession **aftermath** — the ordered pass a successor's first
//! authenticated session runs, and the sibling half of [`crate::ceremony`]
//! (`succession-aftermath.md` § Re-key scope; `succession-aftermath.md`
//! § Re-key scope → the `BackupKey` corpus row: "started at first successor
//! sign-in, surfaced with progress, resumed until complete").
//!
//! **Why this is shared rather than per-app.** The legs themselves already live
//! in shared crates — `regrant_nest_backup_key`, `remint_capability_grants`, `rekey_drafts_after_succession`,
//! `burn_mail_after_succession`. What was *not* shared until this module was
//! the thing that actually carries the safety property: **the order they run
//! in, and which of them are barriers**. That ordering was written once, in
//! `apps/fauna-tui/src/session.rs`, and every reason for it lived in that
//! file's comments — so the second app to drive the aftermath would have had to
//! re-derive them, and the seventh would have had six chances to get one wrong
//! (priorities #1/#2/#4). The reasons are now here, where every app reads the
//! same copy.
//!
//! **What is NOT here.** Four legs do not run in this pass on any app, and
//! deliberately: the `__mls` re-seal (leg 3) is a barrier *inside*
//! `MlsStateSync::load`, ahead of the conversations plane's own restore rather
//! than a post-auth hook; the file-corpus re-seal (leg 5) does not run in the
//! app process at all — it is the sync agent's; and the capability-grant
//! re-mint (leg 4) and the mail burn (leg 6) read rows in the successor's
//! account store — the succession ledger, and the mail custody
//! (`fauna.state.mail`) — so they run in the post-store-ready pass
//! ([`crate::ledger_aftermath`]). All four report into the same progress
//! surface from where their work happens.
//!
//! **Every leg is best-effort and none is fatal to the session.** The account
//! is already the successor's; refusing a sign-in over a pass that can retry at
//! the next one would be strictly worse than a plane that is briefly still
//! owed. Each leg's own idempotence is what makes that safe — the corpus is its
//! own progress record, which is also why there is no progress state at rest.

use fauna_client_config::{BackupKey, BackupRegrantProgress, SuccessionTime};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_protocol::{RpcErrorClass, RpcRequester};

/// What a ceremony knows and the aftermath cannot re-derive — the input to the
/// two **silent** raises (the unattested-member roster and the inherited-filter
/// marks), rebuilt from the durable park ([`PendingCeremony`]) by the
/// post-store-ready pass ([`crate::ledger_aftermath::run_ledger_aftermath`]).
///
/// Absent on a device that did not run the ceremony: both raises are facts
/// about a specific ceremony, and "which succession raised this" must never be
/// answered by "whichever predecessor key happens to open something"
/// (`fauna_client_config::raise_succession_member_reviews`' module doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AftermathCeremony {
    /// The identity this account succeeded *from* in the ceremony that raised
    /// the roster below.
    pub raising_predecessor: ActorId,
    /// The people the group sweep can vouch for nothing about. Empty is
    /// meaningful (a sweep that found nobody), and the raise no-ops on it.
    pub review_roster: Vec<ActorId>,
    /// The nest-recorded time the succession applied — the bound the inherited
    /// filters are classified against. `None` is honest: the reply that carries
    /// it does not arrive on the reconcile arm.
    pub succession_time: Option<SuccessionTime>,
}

/// [`AftermathCeremony`] **parked durably** in the account registry, and
/// rebuilt by the successor's post-store-ready pass.
///
/// **Why a park at all.** The ceremony and the raises are two halves of one
/// act, but never two halves of one moment: the ceremony runs pre-switch, as
/// the predecessor, and the raises write the successor's account store, which
/// does not exist until the successor's runtime reaches store-ready — and on
/// web the switch is a full document swap besides. So the post-ceremony fold
/// parks this per **successor** in the registry beside its
/// `record_succession` ([`Self::park`]) — the single durable decision point
/// — and the post-store-ready pass at every store-ready is the reconcile that
/// drains it, clearing the slot only once every put of both raises landed
/// (`succession-aftermath.md` § Re-key scope → *Adjudicating what the
/// aftermath carries across*, the 2026-09-30 paragraph). A crash or a door
/// refusal anywhere in between leaves it parked, so a raise is owed, never
/// lost; the sweep retry re-parks its roster the same way
/// ([`Self::repark_retried_roster`]).
///
/// Hex rather than `ActorId` on the wire-ish side because this crosses a
/// string-typed store; [`Self::into_ceremony`] is the only way back, and it is
/// deliberately lossy in one direction (see its doc).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingCeremony {
    /// 64-hex of the identity the account succeeded *from*.
    pub raising_predecessor: String,
    /// 64-hex of each person the sweep can vouch for nothing about. Empty is
    /// meaningful — a sweep that found nobody — and must survive the round trip
    /// as an empty list rather than collapsing the whole context away.
    pub review_roster: Vec<String>,
    /// The nest's `succeeded_at` in **epoch seconds**, exactly as the ceremony
    /// received it.
    ///
    /// ⚠ **Seconds, never the derived millisecond bound.**
    /// [`SuccessionTime::from_nest_seconds`] rounds *up* to the end of the
    /// second, and the asymmetry is load-bearing (rounding down under-marks, in
    /// the hiding direction — `filter_marks.rs`). Parking the source value
    /// keeps that rule in the one place that owns it; parking `as_millis()` and
    /// re-wrapping would silently route around it.
    pub succession_seconds: Option<i64>,
}

impl PendingCeremony {
    /// Capture what the ceremony that just ran knows.
    pub fn new(
        raising_predecessor: &ActorId,
        review_roster: &[ActorId],
        succession_seconds: Option<i64>,
    ) -> Self {
        Self {
            raising_predecessor: raising_predecessor.to_hex(),
            review_roster: review_roster.iter().map(|a| a.to_hex()).collect(),
            succession_seconds,
        }
    }

    /// Park this context for `successor_actor_hex` — what every post-ceremony
    /// fold calls immediately before its `record_succession`. Replaces any
    /// earlier park for that successor. Keyed on the **successor** so a switch
    /// to an unrelated account cannot consume a roster raised for a different
    /// identity — the attribution rule applied to the store itself.
    pub fn park(
        &self,
        registry: &fauna_client_accounts::AccountRegistry,
        successor_actor_hex: &str,
    ) {
        match self.to_json() {
            Ok(json) => registry.park_aftermath_ceremony(successor_actor_hex, &json),
            // Unreachable for a struct of strings and an integer; logged rather
            // than panicked because it sits inside a ceremony that already
            // landed.
            Err(e) => tracing::error!(
                error = %e,
                "parking the aftermath ceremony failed to encode — the member and filter \
                 raises of this succession will not run"
            ),
        }
    }

    /// The context parked for `successor_actor_hex`, if any. A payload that no
    /// longer decodes is `Some(Err(()))` — the caller clears it, since no later
    /// store-ready could consume it either.
    pub fn parked(
        registry: &fauna_client_accounts::AccountRegistry,
        successor_actor_hex: &str,
    ) -> Option<Result<Self, ()>> {
        registry
            .pending_aftermath_ceremony(successor_actor_hex)
            .map(|raw| Self::from_json(&raw).ok_or(()))
    }

    /// **The sweep retry's re-park.** A retried sweep (`retry_group_sweep`'s
    /// `Swept(report)`) re-reports its roster; this writes it into the same
    /// slot under the same raising predecessor, as the UNION by person with
    /// whatever is still parked, keeping a parked stamp — so the next
    /// store-ready drains the ceremony's roster and a retry's through one
    /// path. A park naming a different predecessor is replaced (logged): the
    /// retry is the nearest hop's, and the stale one could not be attested.
    pub fn repark_retried_roster(
        registry: &fauna_client_accounts::AccountRegistry,
        successor_actor_hex: &str,
        raising_predecessor: &ActorId,
        roster: &[ActorId],
    ) {
        let mut next = Self::new(raising_predecessor, roster, None);
        match Self::parked(registry, successor_actor_hex) {
            Some(Ok(existing)) if existing.raising_predecessor == next.raising_predecessor => {
                for person in existing.review_roster {
                    if !next.review_roster.contains(&person) {
                        next.review_roster.push(person);
                    }
                }
                next.succession_seconds = existing.succession_seconds;
            }
            Some(Ok(existing)) => tracing::warn!(
                parked = %existing.raising_predecessor,
                retried = %next.raising_predecessor,
                "a retried sweep replaces a parked ceremony raised by a different predecessor"
            ),
            Some(Err(())) | None => {}
        }
        next.park(registry, successor_actor_hex);
    }

    /// Encode for a string-typed store.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Decode. `None` on anything unreadable — a store this size is not worth an
    /// error path.
    pub fn from_json(raw: &str) -> Option<Self> {
        serde_json::from_str(raw).ok()
    }

    /// Rebuild the context the pass consumes.
    ///
    /// **`None` when the raising predecessor does not parse, and that is a
    /// refusal rather than a degradation.** "Which succession raised this" must
    /// never be answered by a guess — the whole reason the roster is attributed
    /// to a specific ceremony instead of "whichever predecessor key happens to
    /// open the blob". Voiding the context raises nothing; attributing a real
    /// roster to the wrong ceremony would be at rest and permanent.
    ///
    /// **An unparseable roster entry is skipped, not fatal** — the opposite
    /// call, for the opposite reason: one dropped correspondent is one missing
    /// review, and refusing the whole context over it would drop the rest of a
    /// perfectly good roster too.
    pub fn into_ceremony(self) -> Option<AftermathCeremony> {
        let raising_predecessor = fauna_core::hex32::decode(&self.raising_predecessor)
            .map(ActorId)
            .map_err(|e| {
                tracing::warn!(
                    error = %e,
                    "the parked ceremony's raising predecessor did not parse — the review raises \
                     are voided rather than attributed to a guess"
                );
            })
            .ok()?;
        let review_roster = self
            .review_roster
            .iter()
            .filter_map(|hex| {
                fauna_core::hex32::decode(hex).map(ActorId).ok().or_else(|| {
                    tracing::warn!(
                        actor = %hex,
                        "a parked roster entry did not parse — that person is skipped, the rest \
                         of the roster still raises"
                    );
                    None
                })
            })
            .collect();
        Some(AftermathCeremony {
            raising_predecessor,
            review_roster,
            succession_time: self
                .succession_seconds
                .map(SuccessionTime::from_nest_seconds),
        })
    }
}

/// Everything the pass needs that is neither the transport nor the sink.
pub struct AftermathInputs {
    /// This session's own identity seed — the successor's.
    pub owner_secret: [u8; 32],
    /// The `BackupKey`s of the retired identities this device can actually
    /// open — the keys leg 7 tries the drafts under.
    ///
    /// ⚠ **Resolve this with the shared registry walk, never a hand-rolled
    /// filter** — `AccountRegistry::predecessor_backup_keys_by_actor`. A dropped
    /// row reads as "no key opens it" forever, and that is exactly the silent
    /// failure priority #2 exists to prevent.
    ///
    /// An *empty* list is meaningful rather than a bug: on a device the user
    /// did not succeed from, the pass still runs so it can report the drafts
    /// as owed by another device instead of doing nothing silently.
    pub predecessor_keys: Vec<BackupKey>,
}

/// Where each leg reports. One method per progress projection, called with
/// `Running` before the leg and its settled/failed value after — the same two
/// edges every app's surface renders.
///
/// **AFIT, not `async_trait`.** The one async method has to be `Send` on a
/// `tokio::spawn`ed native task and `!Send` in a browser; an `async fn` in
/// trait is `Send` exactly when the implementor's body is, which is the same
/// reason [`RpcRequester`] is AFIT. The cost is that this trait is not
/// dyn-safe, so the driver is generic over it.
pub trait AftermathSink {
    /// Leg 2 — the `NestBackupKey` re-grant.
    fn backup_regrant(&mut self, progress: BackupRegrantProgress);
    /// Leg 4 — the capability-grant re-mint, reported by the post-store-ready
    /// pass ([`crate::ledger_aftermath`]), never by this one.
    fn grant_remint(&mut self, progress: fauna_client_capabilities::GrantRemintProgress);
    /// Leg 7 — the `__drafts` re-seal.
    fn drafts_reseal(&mut self, progress: fauna_client_drafts::DraftsResealProgress);
    /// Leg 6 — the mail burn. Fired by the post-store-ready pass
    /// ([`crate::ledger_aftermath`]), never by this one.
    fn mail_burn(&mut self, progress: fauna_client_mail_settings::MailBurnProgress);

    /// Leg 8 — the subscriber-tier period-key rotation.
    ///
    /// ⚠ **Defaulted, unlike its five siblings, and only because of build
    /// order.** Every app should render this line — the arm that says a nest
    /// is too old to rotate reports an exposure that is still open, and a
    /// silent leg is exactly the "clean aftermath" the user must not be shown.
    /// tui is the lead app for a new UI feature and takes it first; the default keeps the other six
    /// compiling until their batched trickle-down overrides it, rather than
    /// seeding six empty `match` arms that each have to be found again later.
    fn period_rotation(
        &mut self,
        progress: fauna_client_subscriptions::orchestration::PeriodRotationProgress,
    ) {
        let _ = progress;
    }

    /// Fired once, at the end of the post-store-ready pass
    /// ([`crate::ledger_aftermath::run_ledger_aftermath`]), once the silent
    /// raises have had their turn on the succession ledger.
    ///
    /// It exists so an app that renders the review surfaces can re-read them
    /// there and nowhere else: a successor's very first session then shows the
    /// marks the ceremony it just ran produced. An app with no such surface
    /// leaves the default no-op.
    ///
    /// ⚠ **Unconditional — it fires even when a raise was refused.** The
    /// surfaces render items raised by *earlier* ceremonies too, and those are
    /// already at rest; a refused put this pass must not also blank a flagged
    /// person's mark.
    fn config_stage_settled(&mut self) -> impl core::future::Future<Output = ()> {
        async {}
    }
}

/// Run the aftermath: leg 7, the `__drafts` re-seal — the one leg the
/// post-auth hook still owns.
///
/// Leg 1, the `__config` re-seal, retired with the blob rail at closure step
/// (6) (`config-dissolution.md` § The `__config` dissolution schedule → *The
/// closure order*): a successor's inherited settings reach it on the account
/// plane, through its own walk's carry (`succession-aftermath.md` § Re-key
/// scope). Every result reaches the app through `sink`.
pub async fn run_succession_aftermath<R, S>(nest: R, inputs: AftermathInputs, sink: &mut S)
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass + core::fmt::Display,
    S: AftermathSink,
{
    let AftermathInputs {
        owner_secret,
        predecessor_keys,
    } = inputs;

    tracing::info!(
        predecessors = predecessor_keys.len(),
        "running the post-succession aftermath"
    );

    // The two silent raises (the member roster and the inherited-filter
    // marks) do NOT run here: they write the succession ledger's rows, which
    // live in the successor's account store, and this pass runs before (or
    // unordered with) it on every host. They run in the post-store-ready pass
    // (`crate::ledger_aftermath`), off the ceremony's durable park.

    // Leg 2 (the `NestBackupKey` re-grant) does not run here either: the
    // destination list it reconciles from is a `fauna.state.backup` row in the
    // successor's account store, so it runs in the post-store-ready pass
    // (`crate::ledger_aftermath`), off the bound box's list.

    // ── Leg 7: the `__drafts` re-seal ─────────────────────────────────────
    //
    // Every compose surface saves half-written text to its own rail sealed
    // under the owner's `BackupKey`. Until this runs the successor's composers
    // come up empty and `DraftsClient::load` hard-errors, so the drafts are
    // **stuck, not corrupt** — and unreachable to the thief, since no
    // credential authenticates as the owner.
    //
    // Leg 6, the only leg that takes something away, runs after every
    // restoring leg by construction: it is the post-store-ready pass's last.
    sink.drafts_reseal(fauna_client_drafts::DraftsResealProgress::Running);
    let drafts_keypair = ActorKeypair::from_secret(owner_secret);
    let drafts = fauna_client_drafts::DraftsClient::new(nest.clone(), &drafts_keypair);
    let drafts_progress = match fauna_client_drafts::rekey_drafts_after_succession(
        &drafts,
        &predecessor_keys,
    )
    .await
    {
        Ok(outcome) => {
            tracing::info!(?outcome, "the __drafts re-seal pass settled");
            fauna_client_drafts::DraftsResealProgress::Settled(outcome)
        }
        // Best-effort and not fatal, like every sibling leg: the drafts are
        // locked rather than lost, and the pass is safe to re-run at every
        // sign-in — the corpus is its own progress record.
        Err(e) => {
            tracing::warn!(error = %e, "the __drafts re-seal pass failed; it retries next sign-in");
            fauna_client_drafts::DraftsResealProgress::Failed(format!("{e}"))
        }
    };
    sink.drafts_reseal(drafts_progress);
}

#[cfg(test)]
mod tests {
    //! What these pin is the **order**, which is the only thing this module
    //! adds over the legs it calls — each leg's own behaviour is tested where
    //! it lives, against its own fakes.
    //!
    //! The test drives the pass over a drafts plane with **nothing at rest**
    //! (`blob: None`), the one settled outcome the leg short-circuits on
    //! without a round trip: the leg still runs — it reports — which is
    //! exactly what the sequence assertion reads.

    use super::*;
    use fauna_client_testkit::{RecordingRequester, block_on};
    use std::sync::Arc;

    /// One entry per `AftermathSink` call, in call order — the assertion
    /// surface. The leg's *value* is flattened to a short label: what these
    /// tests are about is which leg reported and when, never the copy (which
    /// each projection's own `status_line` tests own).
    #[derive(Default)]
    struct Log(Vec<String>);

    impl AftermathSink for Log {
        fn backup_regrant(&mut self, progress: BackupRegrantProgress) {
            self.0.push(format!("backup:{}", label(&progress)));
        }
        fn grant_remint(&mut self, progress: fauna_client_capabilities::GrantRemintProgress) {
            self.0.push(format!("remint:{}", label(&progress)));
        }
        fn drafts_reseal(&mut self, progress: fauna_client_drafts::DraftsResealProgress) {
            self.0.push(format!("drafts:{}", label(&progress)));
        }
        fn mail_burn(&mut self, progress: fauna_client_mail_settings::MailBurnProgress) {
            self.0.push(format!("mail:{}", label(&progress)));
        }
        async fn config_stage_settled(&mut self) {
            self.0.push("config-stage-settled".into());
        }
    }

    /// `Running` / `settled` / `failed` off any leg's `Debug` — every progress
    /// enum in the family has the same three-arm shape, so one helper reads all
    /// five without naming five types.
    fn label(progress: &impl core::fmt::Debug) -> &'static str {
        let rendered = format!("{progress:?}");
        if rendered.starts_with("Running") {
            "running"
        } else if rendered.starts_with("Failed") {
            "failed"
        } else {
            "settled"
        }
    }

    /// A predecessor key that opens nothing here — the pass needs a non-empty
    /// list to be worth running, and the test below never reaches an unseal.
    fn inputs() -> AftermathInputs {
        AftermathInputs {
            owner_secret: [3u8; 32],
            predecessor_keys: vec![fauna_client_config::backup_key_from_seed(&[7u8; 32])],
        }
    }

    /// The drafts plane answers "nothing at rest".
    fn empty_planes(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.drafts.get" => {
                fauna_protocol::encode_canonical(&fauna_protocol::drafts::GetDraftsReply {
                    blob: None,
                    extra: Default::default(),
                })
            }
            other => panic!("the aftermath sent an unexpected kind: {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn the_legs_report_in_the_order_the_ordering_rules_require() {
        let nest = Arc::new(RecordingRequester::new(empty_planes));
        let mut log = Log::default();

        block_on(run_succession_aftermath(
            Arc::clone(&nest),
            inputs(),
            &mut log,
        ));

        assert_eq!(
            log.0,
            vec!["drafts:running", "drafts:settled"],
            "leg 7 is this pass's one leg; leg 2's re-grant, leg 4's re-mint \
             and the review read-back are the post-store-ready pass's"
        );
    }

    // -- The parked ceremony context -----------------------------------------
    //
    // These pin the failure semantics rather than the happy path: every one of
    // them is a case where the obvious implementation loses a roster silently
    // or attributes one to the wrong ceremony.

    fn actor(byte: u8) -> ActorId {
        ActorId([byte; 32])
    }

    #[test]
    fn a_parked_ceremony_round_trips_through_the_store() {
        let parked = PendingCeremony::new(&actor(1), &[actor(2), actor(3)], Some(1_700_000_000));
        let restored = PendingCeremony::from_json(&parked.to_json().expect("encodes"))
            .expect("decodes")
            .into_ceremony()
            .expect("a parseable predecessor yields a context");

        assert_eq!(restored.raising_predecessor, actor(1));
        assert_eq!(restored.review_roster, vec![actor(2), actor(3)]);
        // Rebuilt through the shared constructor, so the round-UP rule applies
        // - parking the derived millis instead would have routed around it.
        assert_eq!(
            restored.succession_time,
            Some(SuccessionTime::from_nest_seconds(1_700_000_000)),
            "the bound must be rebuilt from the parked SECONDS"
        );
    }

    #[test]
    fn an_empty_roster_survives_as_an_empty_roster() {
        // A sweep that found nobody is a real answer. Collapsing it to "no
        // context" would make an honest all-clear indistinguishable from a
        // ceremony whose context was lost.
        let restored = PendingCeremony::from_json(
            &PendingCeremony::new(&actor(1), &[], None)
                .to_json()
                .expect("encodes"),
        )
        .expect("decodes")
        .into_ceremony()
        .expect("an empty roster is still a context");

        assert!(restored.review_roster.is_empty());
        assert_eq!(restored.succession_time, None);
    }

    #[test]
    fn an_unparseable_raising_predecessor_voids_the_whole_context() {
        // Attribution is never guessed: better to raise nothing than to
        // attribute a real roster to the wrong ceremony, which would be at
        // rest and permanent.
        let mut parked = PendingCeremony::new(&actor(1), &[actor(2)], Some(5));
        parked.raising_predecessor = "not-hex".into();

        assert!(
            parked.into_ceremony().is_none(),
            "a context whose raising ceremony cannot be named must not raise at all"
        );
    }

    #[test]
    fn an_unparseable_roster_entry_is_skipped_and_the_rest_still_raise() {
        // The opposite call from the one above, for the opposite reason: one
        // dropped correspondent is one missing review, and refusing the whole
        // context over it would drop a perfectly good roster too.
        let mut parked = PendingCeremony::new(&actor(1), &[actor(2), actor(3)], None);
        parked.review_roster[0] = "zz".into();

        let restored = parked.into_ceremony().expect("the context still stands");
        assert_eq!(
            restored.review_roster,
            vec![actor(3)],
            "the parseable half of the roster must survive a bad entry"
        );
    }

    #[test]
    fn an_unreadable_payload_decodes_to_none_rather_than_panicking() {
        // The slot can hold anything (a stale schema, a truncated write). Every failure here has the same
        // remedy, so none of them is worth an error path - but none may panic
        // inside the sign-in path either.
        assert!(PendingCeremony::from_json("").is_none());
        assert!(PendingCeremony::from_json("{").is_none());
        assert!(PendingCeremony::from_json("[]").is_none());
        assert!(PendingCeremony::from_json(r#"{"raising_predecessor":"aa"}"#).is_none());
    }
}
