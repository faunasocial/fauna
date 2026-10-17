//! The recovery from this device's own store across the app ↔ agent seam.
//!
//! On a desktop the sync agent owns the custodian store, so it runs the leg
//! (`fauna_sync_engine::recovery::recover_from_own_store`, the shared driver
//! `fauna_client_backup::recover::run_store_recovery`) and the app only renders
//! the result (`docs/goal/architecture/segment-backup-protocol.md`
//! § Client-device custodian (pull) → *Restore* → *Recovery from this device's
//! own store*, question (2)). The re-seed's carrier ([`crate::reseed_wire`])
//! over the recovery's outcome: the driver's types cross the IPC as
//! [`fauna_ipc::sync::CustodianRecoverState`], both directions of that
//! projection live here, and the app's wait is [`await_agent_recover`] — so no
//! shell re-derives the end state.

use fauna_client_backup::recover::{
    RecoveredCounts, RecoveryError, RecoveryOutcome, RecoverySetOutcome, RecoverySetResult,
};
use fauna_client_backup::reseed::SetRefusal;
use fauna_ipc::sync::{
    CustodianRecoverReport, CustodianRecoverSet, CustodianRecoverSetOutcome, CustodianRecoverState,
    CustodianRecoveredCounts,
};

pub use crate::reseed_wire::{
    AgentReseedStop as AgentRecoverStop, UNKNOWN_OUTCOME_CODE, UNKNOWN_PHASE,
};

/// The recovery job as the app sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoverJob {
    /// No job since the agent started.
    Idle,
    /// A job is running.
    Running,
    /// The shared driver reached its end.
    Finished(RecoveryOutcome),
    /// The run stopped early; re-running is safe. `phase` is one of `store`,
    /// `grant`, `transport` — or [`UNKNOWN_PHASE`], for a state a newer agent
    /// names that this app cannot read.
    Failed { phase: String, detail: String },
}

fn counts_to_wire(c: RecoveredCounts) -> CustodianRecoveredCounts {
    CustodianRecoveredCounts {
        recovered: c.recovered,
        already_held: c.already_held,
        filed: c.filed,
        inboxed: c.inboxed,
        unplaceable: c.unplaceable,
    }
}

fn counts_from_wire(c: CustodianRecoveredCounts) -> RecoveredCounts {
    RecoveredCounts {
        recovered: c.recovered,
        already_held: c.already_held,
        filed: c.filed,
        inboxed: c.inboxed,
        unplaceable: c.unplaceable,
    }
}

/// Agent side: the driver's result → the wire.
pub fn state_from_driver(outcome: Result<RecoveryOutcome, RecoveryError>) -> CustodianRecoverState {
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(e) => {
            let phase = match &e {
                RecoveryError::Grant(_) => "grant",
                RecoveryError::Transport { .. } => "transport",
            };
            return CustodianRecoverState::Failed {
                phase: phase.into(),
                detail: e.to_string(),
            };
        }
    };
    CustodianRecoverState::Finished(CustodianRecoverReport {
        sets: outcome
            .sets
            .into_iter()
            .map(|s| CustodianRecoverSet {
                set_name: s.set_name,
                outcome: match s.outcome {
                    RecoverySetOutcome::Recovered { counts } => {
                        CustodianRecoverSetOutcome::Recovered {
                            counts: counts_to_wire(counts),
                        }
                    }
                    RecoverySetOutcome::NothingToRecover { counts } => {
                        CustodianRecoverSetOutcome::NothingToRecover {
                            counts: counts_to_wire(counts),
                        }
                    }
                    RecoverySetOutcome::Refused { refusal, detail } => {
                        CustodianRecoverSetOutcome::Refused {
                            code: refusal.code().to_string(),
                            detail,
                        }
                    }
                },
                duties_unfinished: s.duties_unfinished,
            })
            .collect(),
    })
}

/// App side: the wire → the job.
pub fn job_from_state(state: CustodianRecoverState) -> RecoverJob {
    let report = match state {
        CustodianRecoverState::Idle => return RecoverJob::Idle,
        CustodianRecoverState::Running => return RecoverJob::Running,
        CustodianRecoverState::Failed { phase, detail } => {
            return RecoverJob::Failed { phase, detail };
        }
        CustodianRecoverState::Finished(report) => report,
        CustodianRecoverState::Unknown(_) => {
            return RecoverJob::Failed {
                phase: UNKNOWN_PHASE.into(),
                detail: "the backup service reported a recovery state this version cannot \
                         read; update the app, then run the recovery again"
                    .into(),
            };
        }
    };
    RecoverJob::Finished(RecoveryOutcome {
        sets: report
            .sets
            .into_iter()
            .map(|s| RecoverySetResult {
                set_name: s.set_name,
                outcome: match s.outcome {
                    CustodianRecoverSetOutcome::Recovered { counts } => {
                        RecoverySetOutcome::Recovered {
                            counts: counts_from_wire(counts),
                        }
                    }
                    CustodianRecoverSetOutcome::NothingToRecover { counts } => {
                        RecoverySetOutcome::NothingToRecover {
                            counts: counts_from_wire(counts),
                        }
                    }
                    CustodianRecoverSetOutcome::Refused { code, detail } => {
                        RecoverySetOutcome::Refused {
                            refusal: SetRefusal::from_code(&code),
                            detail,
                        }
                    }
                    CustodianRecoverSetOutcome::Unknown(_) => RecoverySetOutcome::Refused {
                        refusal: SetRefusal::from_code(UNKNOWN_OUTCOME_CODE),
                        detail: "the backup service reported an outcome this version cannot read"
                            .into(),
                    },
                },
                duties_unfinished: s.duties_unfinished,
            })
            .collect(),
    })
}

/// The agent calls the app side of a desktop recovery makes: start the job,
/// then read how it is going. [`crate::agent::SyncAgentProvisioner`] is the
/// production source; a test hands [`await_agent_recover`] a scripted one.
pub trait AgentRecoverJob: Sync {
    /// Start the run (`RecoverCustodianStore`); answers the job's state at
    /// once, `Running` in the ordinary case.
    fn start(
        &self,
        nest_backup_key: [u8; 32],
    ) -> impl std::future::Future<Output = Result<RecoverJob, String>> + Send;

    /// Read the job's state (`GetCustodianRecover`).
    fn status(&self) -> impl std::future::Future<Output = Result<RecoverJob, String>> + Send;
}

impl<R, B> AgentRecoverJob for crate::agent::SyncAgentProvisioner<R, B>
where
    R: fauna_protocol::RpcRequester + Clone + Send + Sync + 'static,
    R::Error: std::fmt::Display,
    B: crate::agent::ProvisioningBearerSource + 'static,
{
    async fn start(&self, nest_backup_key: [u8; 32]) -> Result<RecoverJob, String> {
        self.recover_custodian_store(nest_backup_key)
            .await
            .map_err(|e| e.to_string())
    }

    async fn status(&self) -> Result<RecoverJob, String> {
        self.custodian_recover().await.map_err(|e| e.to_string())
    }
}

/// How often [`await_agent_recover`] reads the job — the re-seed's pace.
pub const AGENT_RECOVER_POLL: std::time::Duration = crate::reseed_wire::AGENT_RESEED_POLL;

/// The app side of a desktop recovery from this device's own store: start the
/// agent's job and wait for its own terminal state. A start-then-poll job
/// because one agent round trip is capped well below a run's length.
///
/// `nest_backup_key` is the owner's seed-derived `NestBackupKey`; it crosses to
/// the agent in the start frame only, and the agent grants it to the owner's
/// nest before delivering. Every desktop shell (tui, linux, and the FFI face
/// windows and macOS call) runs through this one function.
pub async fn await_agent_recover<J>(
    agent: &J,
    nest_backup_key: [u8; 32],
    poll: std::time::Duration,
) -> Result<RecoveryOutcome, AgentRecoverStop>
where
    J: AgentRecoverJob + ?Sized,
{
    let mut job = agent
        .start(nest_backup_key)
        .await
        .map_err(AgentRecoverStop::Unreachable)?;
    while job == RecoverJob::Running {
        tokio::time::sleep(poll).await;
        job = agent
            .status()
            .await
            .map_err(AgentRecoverStop::Unreachable)?;
    }
    match job {
        RecoverJob::Finished(outcome) => Ok(outcome),
        RecoverJob::Failed { phase, detail } => Err(AgentRecoverStop::Failed { phase, detail }),
        // `Running` cannot reach here; `Idle` after a start is the restart case.
        RecoverJob::Idle | RecoverJob::Running => Err(AgentRecoverStop::Restarted),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted {
        first: Result<RecoverJob, String>,
        then: std::sync::Mutex<std::collections::VecDeque<Result<RecoverJob, String>>>,
        started_with: std::sync::Mutex<Option<[u8; 32]>>,
    }

    impl Scripted {
        fn new(first: Result<RecoverJob, String>, then: Vec<Result<RecoverJob, String>>) -> Self {
            Self {
                first,
                then: std::sync::Mutex::new(then.into()),
                started_with: std::sync::Mutex::new(None),
            }
        }
    }

    impl AgentRecoverJob for Scripted {
        async fn start(&self, key: [u8; 32]) -> Result<RecoverJob, String> {
            *self.started_with.lock().unwrap() = Some(key);
            self.first.clone()
        }
        async fn status(&self) -> Result<RecoverJob, String> {
            self.then
                .lock()
                .unwrap()
                .pop_front()
                .expect("read past the script: the wait did not stop on a terminal state")
        }
    }

    fn counts(recovered: u64) -> RecoveredCounts {
        RecoveredCounts {
            recovered,
            already_held: 2,
            filed: recovered,
            inboxed: 0,
            unplaceable: 0,
        }
    }

    fn whole() -> RecoveryOutcome {
        RecoveryOutcome {
            sets: vec![
                RecoverySetResult {
                    set_name: "__mail".into(),
                    outcome: RecoverySetOutcome::Recovered { counts: counts(4) },
                    duties_unfinished: None,
                },
                RecoverySetResult {
                    set_name: "__post".into(),
                    outcome: RecoverySetOutcome::NothingToRecover { counts: counts(0) },
                    duties_unfinished: Some("the store would not write".into()),
                },
                RecoverySetResult {
                    set_name: "__card".into(),
                    outcome: RecoverySetOutcome::Refused {
                        refusal: SetRefusal::from_code("fauna.backup.not_granted"),
                        detail: "no grant".into(),
                    },
                    duties_unfinished: None,
                },
            ],
        }
    }

    const NO_WAIT: std::time::Duration = std::time::Duration::ZERO;

    /// The round trip is lossless: what the agent's driver decided is what
    /// the app renders, every arm and the unfinished duty included.
    #[test]
    fn the_driver_outcome_round_trips_through_the_wire() {
        assert_eq!(
            job_from_state(state_from_driver(Ok(whole()))),
            RecoverJob::Finished(whole())
        );
    }

    #[test]
    fn a_driver_error_crosses_as_its_phase() {
        for (err, phase) in [
            (RecoveryError::Grant("refused".into()), "grant"),
            (
                RecoveryError::Transport {
                    set_name: "__mail".into(),
                    detail: "gone".into(),
                },
                "transport",
            ),
        ] {
            match job_from_state(state_from_driver(Err(err))) {
                RecoverJob::Failed { phase: got, .. } => assert_eq!(got, phase),
                other => panic!("expected a stop, got {other:?}"),
            }
        }
    }

    #[test]
    fn an_unknown_state_reads_as_a_stop_never_as_running() {
        let state = CustodianRecoverState::Unknown(fauna_core::carried::CarriedValue(
            fauna_protocol::Value::Null,
        ));
        assert!(matches!(
            job_from_state(state),
            RecoverJob::Failed { phase, .. } if phase == UNKNOWN_PHASE
        ));
    }

    #[tokio::test]
    async fn the_wait_ends_on_the_jobs_own_finished_state() {
        let agent = Scripted::new(
            Ok(RecoverJob::Running),
            vec![Ok(RecoverJob::Running), Ok(RecoverJob::Finished(whole()))],
        );
        assert_eq!(
            await_agent_recover(&agent, [7; 32], NO_WAIT).await,
            Ok(whole())
        );
        assert_eq!(*agent.started_with.lock().unwrap(), Some([7; 32]));
        assert!(agent.then.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_stopped_job_reports_its_phase_and_a_forgotten_one_the_restart() {
        let stopped = Scripted::new(
            Ok(RecoverJob::Running),
            vec![Ok(RecoverJob::Failed {
                phase: "grant".into(),
                detail: "no".into(),
            })],
        );
        assert_eq!(
            await_agent_recover(&stopped, [7; 32], NO_WAIT).await,
            Err(AgentRecoverStop::Failed {
                phase: "grant".into(),
                detail: "no".into()
            })
        );
        let forgot = Scripted::new(Ok(RecoverJob::Running), vec![Ok(RecoverJob::Idle)]);
        assert_eq!(
            await_agent_recover(&forgot, [7; 32], NO_WAIT).await,
            Err(AgentRecoverStop::Restarted)
        );
    }
}
