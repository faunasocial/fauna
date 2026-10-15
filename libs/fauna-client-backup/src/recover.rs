//! The **lived-in recovery driver** — the one place the recovery gesture's run
//! is put in order and its result is read, for every app.
//!
//! Owner: `docs/goal/architecture/segment-backup-protocol.md` § Client-device
//! custodian (pull) → *Restore* → *Recovery into the lived-in nest that
//! regressed* (what one run does, its idempotence, its post-recovery duties);
//! the gesture and its result view: `docs/goal/ui/backups.md` § Audit-alert
//! surface, the fifth reason.
//!
//! # What this module owns, and what it does not
//!
//! The leg that moves a destination's copy back onto the regressed source —
//! list, fetch, land, `fauna.backup.custody.recover`, then the duties — is
//! [`RecoveryLeg`], a seam: its one body is [`crate::recover_leg::RecoveryDelivery`]
//! (beside this module, wasm-clean, so web runs the same leg), whose `Send`
//! impl natively is the sync engine's over its concrete transport, exactly as
//! the re-seed's delivery leg is a seam beside [`crate::reseed::run_reseed`].
//! What is shared here is
//! everything a shell would otherwise re-derive: **which sets** a destination's
//! notice stands for ([`regressed_sets`]), the order they run in, how a reply
//! or a refusal reads as a per-set outcome, the one bit a UI may render as
//! "recovered" ([`RecoveryOutcome::is_whole`]), and the result view's text
//! ([`result_lines`]).
//!
//! # Crash-safety and retry
//!
//! The verb is idempotent by construction (held implies filed), and the leg is
//! resumable by content addressing, so re-running from the top is the only
//! retry this driver offers — the re-seed's posture. A transport fault stops
//! the run ([`RecoveryError::Transport`]); every set before it is reported as
//! it ended. Nothing here deletes anything on the source; the leg's
//! destination tombstones are retained `T` and reversible.

use async_trait::async_trait;
use fauna_protocol::{MaybeSendSync, RpcError};

use crate::audit::DestinationAuditRecord;
use crate::reseed::SetRefusal;

/// What `fauna.backup.custody.recover` counted, as the result view reads it —
/// the wire reply's counts, without its open extension map, so an outcome
/// compares by value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveredCounts {
    /// Records the source did not hold, now appended and live.
    pub recovered: u64,
    /// Records the delivered copy holds that the source already held.
    pub already_held: u64,
    /// Recovered records filed where the newest delivered journal put them.
    pub filed: u64,
    /// Recovered mail no delivered journal names, landed in the inbox unseen.
    pub inboxed: u64,
    /// Calendar or contacts records no journal names — not appended.
    pub unplaceable: u64,
}

impl From<&fauna_protocol::backup::CustodyRecoverReply> for RecoveredCounts {
    fn from(reply: &fauna_protocol::backup::CustodyRecoverReply) -> Self {
        Self {
            recovered: reply.recovered,
            already_held: reply.already_held,
            filed: reply.filed,
            inboxed: reply.inboxed,
            unplaceable: reply.unplaceable,
        }
    }
}

/// What the leg reports for one set.
#[derive(Debug, Clone, PartialEq)]
pub enum LegAnswer {
    /// The source's verb replied. `duties` is whether the post-recovery duties
    /// (clearing this device's notice record, retiring the copy's unnamed
    /// paths at the destination) completed after that whole reply — the
    /// records are back either way.
    Replied {
        reply: fauna_protocol::backup::CustodyRecoverReply,
        duties: Result<(), String>,
    },
    /// The source refused the verb, by its wire error.
    Refused(RpcError),
    /// Delivery, or the connection, failed before a reply — the run stops.
    Failed(String),
}

/// The recovery leg for one destination: every generation its copy holds for
/// `set_name`, landed on the regressed source, then the verb, then the duties.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait RecoveryLeg: MaybeSendSync {
    async fn recover_set(&self, set_name: &str) -> LegAnswer;
}

/// What happened to one set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoverySetOutcome {
    /// The source took back `counts.recovered > 0` records.
    Recovered { counts: RecoveredCounts },
    /// The copy held nothing the source lacks — on a re-run, the set an
    /// earlier run finished.
    NothingToRecover { counts: RecoveredCounts },
    /// Refused for a reason the owner can act on — the same catalogue, and the
    /// same next steps, as the re-seed's materialize.
    Refused { refusal: SetRefusal, detail: String },
}

/// One set's line in the run's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverySetResult {
    pub set_name: String,
    pub outcome: RecoverySetOutcome,
    /// A post-recovery duty that did not complete, and why. The records are
    /// back; the notice may stand until a re-run finishes the duty.
    pub duties_unfinished: Option<String>,
}

/// The run's result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryOutcome {
    /// One entry per set, in run order. Empty when no notice stood.
    pub sets: Vec<RecoverySetResult>,
}

impl RecoveryOutcome {
    /// `true` when every set's lost records are back, every one was placed,
    /// and every duty completed — the one bit a UI may render as "recovered".
    /// A run with nothing to recover is not whole: it recovered nothing.
    pub fn is_whole(&self) -> bool {
        !self.sets.is_empty()
            && self.sets.iter().all(|s| {
                s.duties_unfinished.is_none()
                    && match &s.outcome {
                        RecoverySetOutcome::Recovered { counts }
                        | RecoverySetOutcome::NothingToRecover { counts } => {
                            counts.unplaceable == 0
                        }
                        RecoverySetOutcome::Refused { .. } => false,
                    }
            })
    }

    /// Records made live again by this run, across every set.
    pub fn records_recovered(&self) -> u64 {
        self.sets
            .iter()
            .map(|s| match &s.outcome {
                RecoverySetOutcome::Recovered { counts } => counts.recovered,
                _ => 0,
            })
            .sum()
    }
}

/// Why the run stopped before its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryError {
    /// Delivery or the connection failed on `set_name`; its result is unknown.
    /// Re-run: the leg resumes by content address and the verb recovers
    /// nothing twice.
    Transport { set_name: String, detail: String },
    /// The store end only ([`run_store_recovery`]): the owner's own nest did
    /// not take the `NestBackupKey` grant the verb opens the delivered custody
    /// under, so nothing was landed. Re-run: the grant is idempotent.
    Grant(String),
}

impl std::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport { set_name, detail } => {
                write!(f, "recovering {set_name} did not complete: {detail}")
            }
            Self::Grant(detail) => {
                write!(
                    f,
                    "handing your nest its backup key did not complete: {detail}"
                )
            }
        }
    }
}

impl std::error::Error for RecoveryError {}

/// The reserved sets an accepted source regression stands for on this
/// destination's record — what one recovery run covers, in a stable order.
///
/// The record's keys are ledger keys (`<set>/<mirror path>`,
/// [`crate::audit::ledger_key`]); a regressed kind's content ledger and its
/// journal's both name the one set, which runs once. Read straight from the
/// standing record, so the sets a run covers are exactly the ones the notice
/// is about.
pub fn regressed_sets(record: &DestinationAuditRecord) -> Vec<String> {
    sets_of_ledger_keys(record.state.accepted_regressions.keys().map(String::as_str))
}

/// The reserved sets a list of ledger keys (`<set>/<mirror path>`) stands for,
/// once each, in a stable order — [`regressed_sets`]' reading, shared with the
/// store end, whose record is the custodian store's own `source_regressions`
/// keyed the same way (`segment-backup-protocol.md` § Client-device custodian
/// (pull) → *Restore* → *Recovery from this device's own store*).
pub fn sets_of_ledger_keys<'a>(keys: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut sets: Vec<String> = keys
        .into_iter()
        .filter_map(|key| key.split('/').next())
        .filter(|set| !set.is_empty())
        .map(str::to_string)
        .collect();
    sets.sort();
    sets.dedup();
    sets
}

/// Whether `record`'s fifth-reason banner carries
/// `backup-audit-recover-button` at `now` — the one rule every app's button
/// asks (`ui/backups.md` § Audit-alert surface, the fifth reason, and its
/// client-device arm).
///
/// Two conditions, both read, never re-derived: an accepted source regression
/// stands on the record (the shared [`DestinationAuditRecord::alert_reasons`],
/// never a deadline check of the shell's own), and `destination` has a leg this
/// device can run ([`recover_leg`]) — a **nest**, which the network leg reaches
/// from any device, or **this device's own** client-device row, the one whose
/// custodian assignment names `this_device_id`
/// ([`fauna_core::data::custodian_assignment_for`]), whose copy sits in this
/// device's own store. Another device's client-device row is never offered
/// here: its copy is not on this device. A shell that is never a custodian
/// (web) passes an empty id, which matches nothing.
/// `destination` must be the row `record` is about; a row for another
/// destination is never offered.
pub fn recover_offered(
    record: &DestinationAuditRecord,
    destination: &fauna_core::data::BackupDestination,
    this_device_id: &str,
    now: i64,
) -> bool {
    record.state.destination_id == destination.destination_id
        && recover_leg(destination, this_device_id).is_some()
        && record.alert_reasons(now).iter().any(|r| {
            matches!(
                r,
                fauna_core::format::BackupAuditAlertReason::SourceRegressed { .. }
            )
        })
}

/// Which recovery leg the confirm of `backup-audit-recover-button` runs for a
/// row — the row's kind decides it, never the shell
/// (`segment-backup-protocol.md` § Client-device custodian (pull) → *Restore*,
/// *Recovery into the lived-in nest that regressed* and *Recovery from this
/// device's own store*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverLeg {
    /// A nest destination: the network leg, run in the app over the owner's
    /// own connections (`fauna_sync_engine::recovery::recover_from_destination`
    /// natively, `fauna-wasm`'s `backupRecoverFromDestination` on web).
    Destination,
    /// This device's own custodian row: the store end, run by the process that
    /// owns the store — the sync agent on a desktop
    /// (`fauna_client_sync::recover_wire::await_agent_recover`), the app on a
    /// phone ([`run_store_recovery`]).
    OwnStore,
}

/// The leg `destination` recovers through on the device whose sync id is
/// `this_device_id`, or `None` when no leg on this device reaches its copy —
/// another device's client-device row, a row this build cannot drive.
pub fn recover_leg(
    destination: &fauna_core::data::BackupDestination,
    this_device_id: &str,
) -> Option<RecoverLeg> {
    match destination.kind_view() {
        fauna_core::data::DestinationKind::Nest { .. } => Some(RecoverLeg::Destination),
        fauna_core::data::DestinationKind::ClientDevice { .. } => {
            fauna_core::data::custodian_assignment_for(
                std::iter::once(destination.custodian_row()),
                this_device_id,
            )
            .map(|_| RecoverLeg::OwnStore)
        }
        fauna_core::data::DestinationKind::Inert { .. } => None,
    }
}

/// The destination ids among `records` that [`recover_offered`] answers yes
/// for, in record order — each record matched to its row in `destinations` by
/// id (a destination's coverage rows share its id and its kind).
pub fn recover_targets(
    records: &[DestinationAuditRecord],
    destinations: &[fauna_core::data::BackupDestination],
    this_device_id: &str,
    now: i64,
) -> Vec<String> {
    records
        .iter()
        .filter(|record| {
            destinations
                .iter()
                .find(|d| d.destination_id == record.state.destination_id)
                .is_some_and(|d| recover_offered(record, d, this_device_id, now))
        })
        .map(|record| record.state.destination_id.clone())
        .collect()
}

/// The recovery from **this device's own custodian store**, put in order: the
/// `NestBackupKey` grant to the owner's own nest first — the verb opens the
/// delivered custody under it, and a nest whose only destination is this
/// device may hold none (the re-seed's phase 1 verb, idempotent: a grant
/// replaces) — then [`run_recovery`] over the store end's leg.
///
/// Nothing standing (`sets` empty) asks nothing, the grant included. The one
/// order every host of the store runs — the desktop agent's job and the
/// phone's in-process call — so neither re-derives it.
pub async fn run_store_recovery<R, L>(
    source: &crate::BackupClient<R>,
    nest_backup_key: Vec<u8>,
    leg: &L,
    sets: &[String],
) -> Result<RecoveryOutcome, RecoveryError>
where
    R: fauna_protocol::RpcRequester,
    L: RecoveryLeg + ?Sized,
{
    if sets.is_empty() {
        return Ok(RecoveryOutcome::default());
    }
    let grant = source
        .nest_key_grant(nest_backup_key)
        .await
        .map_err(|e| RecoveryError::Grant(e.to_string()))?;
    if !grant.ok {
        return Err(RecoveryError::Grant(
            "your nest did not store the backup key".into(),
        ));
    }
    run_recovery(leg, sets).await
}

/// Run the recovery for `sets` through `leg`, one set at a time.
pub async fn run_recovery<L>(leg: &L, sets: &[String]) -> Result<RecoveryOutcome, RecoveryError>
where
    L: RecoveryLeg + ?Sized,
{
    let mut out = RecoveryOutcome::default();
    for set_name in sets {
        let (outcome, duties_unfinished) = match leg.recover_set(set_name).await {
            LegAnswer::Replied { reply, duties } => {
                let counts = RecoveredCounts::from(&reply);
                let outcome = if counts.recovered > 0 {
                    RecoverySetOutcome::Recovered { counts }
                } else {
                    RecoverySetOutcome::NothingToRecover { counts }
                };
                (outcome, duties.err())
            }
            LegAnswer::Refused(rpc) => (
                RecoverySetOutcome::Refused {
                    refusal: SetRefusal::from_code(&rpc.code),
                    detail: rpc.detail_or_code(),
                },
                None,
            ),
            LegAnswer::Failed(detail) => {
                return Err(RecoveryError::Transport {
                    set_name: set_name.clone(),
                    detail,
                });
            }
        };
        out.sets.push(RecoverySetResult {
            set_name: set_name.clone(),
            outcome,
            duties_unfinished,
        });
    }
    Ok(out)
}

/// The `backup-audit-recover-result` view's text: the verdict first, then one
/// line per set in run order, then one line per gap.
///
/// The verdict is [`RecoveryOutcome::is_whole`], never re-derived. Keys, not
/// strings, as [`crate::reseed::result_lines`]: resolve each line with
/// [`fauna_core::localized::LocalizedText::resolve_nested`], because the set
/// name and the remedy inside a line are themselves keys.
pub fn result_lines(outcome: &RecoveryOutcome) -> Vec<fauna_core::localized::LocalizedText> {
    use fauna_core::localized::LocalizedText;
    let verdict = if outcome.sets.is_empty() {
        "backups.recover_result_nothing"
    } else if outcome.is_whole() {
        "backups.recover_result_whole"
    } else {
        "backups.recover_result_incomplete"
    };
    let mut lines = vec![LocalizedText::key(verdict)];
    for set in &outcome.sets {
        let name = crate::reseed::segment_set_label(&set.set_name);
        lines.push(match &set.outcome {
            RecoverySetOutcome::Recovered { counts } => LocalizedText::key_args(
                "backups.recover_set_recovered",
                [
                    ("set", name.clone()),
                    ("count", counts.recovered.to_string()),
                ],
            ),
            RecoverySetOutcome::NothingToRecover { .. } => {
                LocalizedText::key_arg("backups.recover_set_nothing", "set", name.clone())
            }
            RecoverySetOutcome::Refused { refusal, .. } => LocalizedText::key_args(
                "backups.recover_set_refused",
                [
                    ("set", name.clone()),
                    ("remedy", refusal.remedy_key().to_string()),
                    ("code", refusal.code().to_string()),
                ],
            ),
        });
    }
    for set in &outcome.sets {
        let name = crate::reseed::segment_set_label(&set.set_name);
        if let RecoverySetOutcome::Recovered { counts }
        | RecoverySetOutcome::NothingToRecover { counts } = &set.outcome
            && counts.unplaceable > 0
        {
            lines.push(LocalizedText::key_args(
                "backups.recover_gap_unplaceable",
                [
                    ("set", name.clone()),
                    ("count", counts.unplaceable.to_string()),
                ],
            ));
        }
        if let Some(reason) = &set.duties_unfinished {
            lines.push(LocalizedText::key_args(
                "backups.recover_gap_duties",
                [("set", name), ("reason", reason.clone())],
            ));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{AcceptedRegression, ledger_key};
    use fauna_client_testkit::block_on;
    use fauna_protocol::backup::CustodyRecoverReply;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// A leg scripted per set, logging every set it was asked for.
    struct ScriptedLeg {
        answers: BTreeMap<String, LegAnswer>,
        asked: Mutex<Vec<String>>,
    }

    impl ScriptedLeg {
        fn new(answers: impl IntoIterator<Item = (&'static str, LegAnswer)>) -> Self {
            Self {
                answers: answers
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
                asked: Mutex::new(Vec::new()),
            }
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl RecoveryLeg for ScriptedLeg {
        async fn recover_set(&self, set_name: &str) -> LegAnswer {
            self.asked.lock().unwrap().push(set_name.to_string());
            self.answers[set_name].clone()
        }
    }

    fn replied(recovered: u64, unplaceable: u64, duties: Result<(), String>) -> LegAnswer {
        LegAnswer::Replied {
            reply: CustodyRecoverReply {
                recovered,
                already_held: 3,
                filed: recovered,
                unplaceable,
                ..Default::default()
            },
            duties,
        }
    }

    fn regression() -> AcceptedRegression {
        AcceptedRegression {
            pinned: 5,
            served: 3,
            observed_at: 100,
            floored_at: None,
            recoverable_until: None,
        }
    }

    fn resolve(lines: &[fauna_core::localized::LocalizedText]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.resolve_nested(fauna_i18n::strings::lookup))
            .collect()
    }

    #[test]
    fn the_sets_a_run_covers_are_the_ones_the_notice_stands_for_each_once() {
        let mut record = DestinationAuditRecord::never("dest-1");
        let scope = "ab".repeat(32);
        for (set, path) in [
            ("__mail", format!("{scope}/manifest.mail")),
            ("__mail", format!("{scope}/manifest.mail-placement")),
            ("__calendar", format!("{scope}/manifest.calendar")),
        ] {
            record
                .state
                .accepted_regressions
                .insert(ledger_key(set, &path), regression());
        }
        assert_eq!(regressed_sets(&record), vec!["__calendar", "__mail"]);
        assert!(regressed_sets(&DestinationAuditRecord::never("d")).is_empty());
    }

    #[test]
    fn a_whole_run_recovers_every_set_and_counts_what_came_back() {
        let leg = ScriptedLeg::new([
            ("__mail", replied(4, 0, Ok(()))),
            ("__post", replied(0, 0, Ok(()))),
        ]);
        let sets = vec!["__mail".to_string(), "__post".to_string()];
        let outcome = block_on(run_recovery(&leg, &sets)).expect("runs");
        assert_eq!(*leg.asked.lock().unwrap(), sets, "every set, in order");
        assert!(outcome.is_whole());
        assert_eq!(outcome.records_recovered(), 4);
        assert!(matches!(
            outcome.sets[1].outcome,
            RecoverySetOutcome::NothingToRecover { .. }
        ));
        assert_eq!(
            resolve(&result_lines(&outcome)),
            vec![
                "Everything your nest lost is back.",
                "Mail: 4 recovered",
                "Posts: nothing to recover",
            ]
        );
    }

    #[test]
    fn a_refusal_names_its_remedy_and_the_run_goes_on() {
        let leg = ScriptedLeg::new([
            (
                "__mail",
                LegAnswer::Refused(
                    RpcError::new(RpcError::CODE_BACKUP_CUSTODY_INCOMPLETE, "error")
                        .with_details_text("segment 3 has no sidecar"),
                ),
            ),
            ("__card", replied(2, 0, Ok(()))),
        ]);
        let sets = vec!["__mail".to_string(), "__card".to_string()];
        let outcome = block_on(run_recovery(&leg, &sets)).expect("runs");
        assert!(!outcome.is_whole());
        assert_eq!(
            outcome.sets[0].outcome,
            RecoverySetOutcome::Refused {
                refusal: SetRefusal::CustodyIncomplete,
                detail: "segment 3 has no sidecar".to_string(),
            }
        );
        let text = resolve(&result_lines(&outcome));
        assert_eq!(
            text[0],
            "The recovery is not complete yet. Run it again to finish what is missing."
        );
        assert!(text[1].starts_with("Mail: not recovered. "), "{text:?}");
        assert_eq!(text[2], "Contacts: 2 recovered");
    }

    #[test]
    fn a_gap_is_never_rendered_as_whole() {
        let leg = ScriptedLeg::new([
            ("__calendar", replied(5, 2, Ok(()))),
            (
                "__mail",
                replied(1, 0, Err("the destination was unreachable".into())),
            ),
        ]);
        let sets = vec!["__calendar".to_string(), "__mail".to_string()];
        let outcome = block_on(run_recovery(&leg, &sets)).expect("runs");
        assert!(
            !outcome.is_whole(),
            "unplaceable entries and an unfinished duty are gaps"
        );
        assert_eq!(
            outcome.sets[1].duties_unfinished.as_deref(),
            Some("the destination was unreachable")
        );
        let text = resolve(&result_lines(&outcome));
        assert_eq!(text.len(), 5, "{text:?}");
        assert!(
            text[3].contains("Calendar") && text[3].contains('2'),
            "{text:?}"
        );
        assert!(
            text[4].contains("the destination was unreachable"),
            "{text:?}"
        );
    }

    /// Every kind the sweep backs up is named to the owner in the recovery
    /// view too — never by its raw `__<kind>` set name.
    #[test]
    fn every_backed_up_kind_is_named_to_the_owner_never_by_its_set_name() {
        let sets: Vec<String> = fauna_protocol::segments::BACKED_UP_KINDS
            .iter()
            .map(|k| format!("__{k}"))
            .collect();
        let leg = ScriptedLeg {
            answers: sets
                .iter()
                .map(|s| (s.clone(), replied(2, 1, Ok(()))))
                .collect(),
            asked: Mutex::new(Vec::new()),
        };
        let outcome = block_on(run_recovery(&leg, &sets)).expect("runs");
        let text = resolve(&result_lines(&outcome));
        assert!(text.iter().all(|l| !l.contains("__")), "{text:?}");
    }

    #[test]
    fn a_transport_fault_stops_the_run_where_it_happened() {
        let leg = ScriptedLeg::new([
            ("__calendar", LegAnswer::Failed("connection reset".into())),
            ("__mail", replied(1, 0, Ok(()))),
        ]);
        let sets = vec!["__calendar".to_string(), "__mail".to_string()];
        let err = block_on(run_recovery(&leg, &sets)).expect_err("stops");
        assert_eq!(
            err,
            RecoveryError::Transport {
                set_name: "__calendar".to_string(),
                detail: "connection reset".to_string(),
            }
        );
        assert_eq!(*leg.asked.lock().unwrap(), vec!["__calendar"]);
    }

    #[test]
    fn no_standing_notice_is_nothing_to_recover_not_whole() {
        let leg = ScriptedLeg::new([]);
        let outcome = block_on(run_recovery(&leg, &[])).expect("runs");
        assert!(!outcome.is_whole());
        assert_eq!(
            resolve(&result_lines(&outcome)),
            vec!["There was nothing left to recover."]
        );
    }

    fn nest_row(id: &str) -> fauna_core::data::BackupDestination {
        fauna_core::data::BackupDestination {
            destination_id: id.into(),
            destination_nest_url: "https://backup.example".into(),
            ..Default::default()
        }
    }

    fn device_row(id: &str) -> fauna_core::data::BackupDestination {
        fauna_core::data::BackupDestination {
            destination_id: id.into(),
            kind: fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE.into(),
            custodian_device_id: Some("aa11".into()),
            ..Default::default()
        }
    }

    fn regressed(id: &str) -> DestinationAuditRecord {
        let mut record = DestinationAuditRecord::never(id);
        record
            .state
            .accepted_regressions
            .insert(ledger_key("__mail", "ab/manifest.mail"), regression());
        record
    }

    #[test]
    fn the_button_is_offered_on_a_nest_or_this_devices_own_row_with_a_standing_regression() {
        let now = 1_000;
        assert!(recover_offered(
            &regressed("n"),
            &nest_row("n"),
            "aa11",
            now
        ));
        assert!(
            recover_offered(&regressed("n"), &nest_row("n"), "", now),
            "a nest is reached from any device, a never-custodian one included"
        );
        assert!(
            recover_offered(&regressed("d"), &device_row("d"), "aa11", now),
            "this device's own custodian row recovers from its own store"
        );
        assert!(
            !recover_offered(&regressed("d"), &device_row("d"), "bb22", now),
            "another device's copy is not on this device"
        );
        assert!(
            !recover_offered(&regressed("d"), &device_row("d"), "", now),
            "a device with no sync id is no row's custodian"
        );
        assert!(
            !recover_offered(
                &DestinationAuditRecord::never("n"),
                &nest_row("n"),
                "aa11",
                now
            ),
            "no regression stands"
        );
        assert!(
            !recover_offered(
                &DestinationAuditRecord::never("d"),
                &device_row("d"),
                "aa11",
                now
            ),
            "no regression stands on this device's row"
        );
        assert!(
            !recover_offered(&regressed("n"), &nest_row("other"), "aa11", now),
            "another destination's row is never offered"
        );
        // A lapsed recovery window no longer stands — read from the shared
        // `alert_reasons`, never a deadline check here.
        let mut lapsed = regressed("n");
        for r in lapsed.state.accepted_regressions.values_mut() {
            r.recoverable_until = Some(now - 1);
        }
        assert!(!recover_offered(&lapsed, &nest_row("n"), "aa11", now));
    }

    #[test]
    fn the_leg_follows_the_rows_kind() {
        assert_eq!(
            recover_leg(&nest_row("n"), "aa11"),
            Some(RecoverLeg::Destination)
        );
        assert_eq!(
            recover_leg(&device_row("d"), "aa11"),
            Some(RecoverLeg::OwnStore)
        );
        assert_eq!(recover_leg(&device_row("d"), "bb22"), None);
        let inert = fauna_core::data::BackupDestination {
            destination_id: "x".into(),
            kind: "carrier-pigeon".into(),
            ..Default::default()
        };
        assert_eq!(recover_leg(&inert, "aa11"), None);
    }

    #[test]
    fn the_targets_are_the_offered_records_in_record_order() {
        let records = vec![
            regressed("n2"),
            regressed("d"),
            DestinationAuditRecord::never("n3"),
            regressed("n1"),
            regressed("gone"),
        ];
        let destinations = vec![
            nest_row("n1"),
            device_row("d"),
            nest_row("n2"),
            nest_row("n3"),
        ];
        assert_eq!(
            recover_targets(&records, &destinations, "aa11", 1_000),
            vec!["n2", "d", "n1"]
        );
        assert_eq!(
            recover_targets(&records, &destinations, "bb22", 1_000),
            vec!["n2", "n1"],
            "another device's row is not this device's to recover from"
        );
    }

    // ── the store end's order (`run_store_recovery`) ────────────────────────

    /// The owner's own nest as the store end's driver meets it: the grant
    /// alone, answered `ok` or not, logged into the leg's own log.
    struct GrantNest<'a> {
        ok: bool,
        log: &'a Mutex<Vec<String>>,
    }

    impl fauna_protocol::RpcRequester for &GrantNest<'_> {
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
            assert_eq!(
                kind,
                crate::KIND_NEST_KEY_GRANT,
                "the driver asks only the grant"
            );
            let bytes = fauna_protocol::encode_canonical(&payload).unwrap();
            let req: fauna_protocol::backup::NestKeyGrantRequest =
                fauna_protocol::decode_strict(&bytes).unwrap();
            self.log
                .lock()
                .unwrap()
                .push(format!("grant:{}", req.nest_backup_key.len()));
            let reply =
                fauna_protocol::encode_canonical(&fauna_protocol::backup::NestKeyGrantReply {
                    ok: self.ok,
                    ..Default::default()
                })
                .unwrap();
            Ok(fauna_protocol::decode_strict(&reply).unwrap())
        }
    }

    /// **The grant precedes the delivery** (*Recovery from this device's own
    /// store*, question (1)): the verb opens the delivered custody under the
    /// `NestBackupKey` grant, which a nest whose only destination is this
    /// device may never have held.
    #[test]
    fn the_store_end_grants_the_key_before_the_first_set() {
        let leg = ScriptedLeg::new([("__mail", replied(2, 0, Ok(())))]);
        let nest = GrantNest {
            ok: true,
            log: &leg.asked,
        };
        let out = block_on(run_store_recovery(
            &crate::BackupClient::new(&nest),
            vec![9; 32],
            &leg,
            &["__mail".to_string()],
        ))
        .unwrap();
        assert!(out.is_whole());
        assert_eq!(*leg.asked.lock().unwrap(), ["grant:32", "__mail"]);
    }

    /// A grant the nest refuses lands nothing: no set is asked for.
    #[test]
    fn a_refused_grant_stops_before_any_set() {
        let leg = ScriptedLeg::new([("__mail", replied(2, 0, Ok(())))]);
        let nest = GrantNest {
            ok: false,
            log: &leg.asked,
        };
        let err = block_on(run_store_recovery(
            &crate::BackupClient::new(&nest),
            vec![9; 32],
            &leg,
            &["__mail".to_string()],
        ))
        .unwrap_err();
        assert!(matches!(err, RecoveryError::Grant(_)), "got {err:?}");
        assert_eq!(*leg.asked.lock().unwrap(), ["grant:32"]);
    }

    /// Nothing standing asks nothing — not even the grant.
    #[test]
    fn nothing_standing_asks_nothing() {
        let leg = ScriptedLeg::new([]);
        let nest = GrantNest {
            ok: true,
            log: &leg.asked,
        };
        let out = block_on(run_store_recovery(
            &crate::BackupClient::new(&nest),
            vec![9; 32],
            &leg,
            &[],
        ))
        .unwrap();
        assert_eq!(out, RecoveryOutcome::default());
        assert!(leg.asked.lock().unwrap().is_empty());
    }

    /// The store's own record reads as the sets it stands for, once each: a
    /// kind's content ledger and its journal's name one set.
    #[test]
    fn ledger_keys_read_as_their_sets_once_each() {
        assert_eq!(
            sets_of_ledger_keys([
                "__mail/aa/manifest.mail",
                "__mail/aa/manifest.mail.placement",
                "__post/aa/manifest.post",
            ]),
            ["__mail", "__post"]
        );
    }
}
