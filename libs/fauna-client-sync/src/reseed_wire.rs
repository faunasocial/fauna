//! The re-seed ceremony across the app ↔ agent seam.
//!
//! On a desktop the sync agent owns the custodian store, so it runs the shared
//! driver (`fauna_client_backup::reseed::run_reseed`) and the app only renders
//! the result (`docs/goal/behavior/backup-destinations.md` § Re-seed → *Where
//! the ceremony runs*). The driver's types cannot cross the IPC as they are
//! (`fauna-ipc` is a leaf crate the Windows shell extension links), so they travel as
//! [`fauna_ipc::sync::CustodianReseedState`]. Both directions of that
//! projection live here, one module, so the round trip is pinned in one place
//! and the app renders exactly what the driver decided.

use fauna_client_backup::reseed::{
    DeliveredCorpus, DeliveredSet, ReseedError, ReseedOutcome, SetAxis, SetOutcome, SetRefusal,
    SetResult,
};
use fauna_ipc::sync::{
    CustodianReseedReport, CustodianReseedSet, CustodianReseedSetOutcome, CustodianReseedState,
};

/// The re-seed job as the app sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReseedJob {
    /// No job since the agent started.
    Idle,
    /// A job is running.
    Running,
    /// The ceremony reached its end.
    Finished(ReseedOutcome),
    /// The ceremony stopped early; re-running is safe. `phase` is one of
    /// `store`, `grant`, `delivery`, `transport` — or [`UNKNOWN_PHASE`], for a
    /// state a newer agent names that this app cannot read.
    Failed { phase: String, detail: String },
}

/// The `phase` of the [`ReseedJob::Failed`] an unreadable job state reads as
/// (`CustodianReseedState::Unknown`): a state a newer agent names is never
/// read as running, so the wait ends instead of polling a job it cannot follow
/// (`transport.md` § Rule 3 in full).
pub const UNKNOWN_PHASE: &str = "unknown";

/// The refusal code an unreadable per-set outcome reads as
/// (`CustodianReseedSetOutcome::Unknown`): nothing is claimed for that set.
pub const UNKNOWN_OUTCOME_CODE: &str = "unknown-outcome";

/// Agent side: the driver's result → the wire.
pub fn state_from_driver(outcome: Result<ReseedOutcome, ReseedError>) -> CustodianReseedState {
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(e) => {
            let phase = match &e {
                ReseedError::Grant(_) => "grant",
                ReseedError::Delivery(_) => "delivery",
                ReseedError::Transport { .. } => "transport",
            };
            return CustodianReseedState::Failed {
                phase: phase.into(),
                detail: e.to_string(),
            };
        }
    };
    CustodianReseedState::Finished(CustodianReseedReport {
        sets: outcome
            .sets
            .into_iter()
            .map(|s| CustodianReseedSet {
                set_name: s.set.set_name,
                folder: s.set.axis == SetAxis::Folder,
                folder_display_name: s.set.folder_display_name,
                outcome: match s.outcome {
                    SetOutcome::Materialized { segments, records } => {
                        CustodianReseedSetOutcome::Materialized { segments, records }
                    }
                    SetOutcome::AlreadyLive => CustodianReseedSetOutcome::AlreadyLive,
                    SetOutcome::Refused { refusal, detail } => CustodianReseedSetOutcome::Refused {
                        code: refusal.code().to_string(),
                        detail,
                    },
                },
            })
            .collect(),
        sidecarless_segments: outcome.delivered.sidecarless_segments,
        folder_paths_without_seal: outcome.delivered.folder_paths_without_seal,
        plaintext_bytes: outcome.delivered.plaintext_bytes,
    })
}

/// App side: the wire → the job.
///
/// The delivered-set list is rebuilt from the per-set results, which is
/// lossless: the driver records exactly one result per delivered set, an
/// unnamed folder included.
pub fn job_from_state(state: CustodianReseedState) -> ReseedJob {
    let report = match state {
        CustodianReseedState::Idle => return ReseedJob::Idle,
        CustodianReseedState::Running => return ReseedJob::Running,
        CustodianReseedState::Failed { phase, detail } => {
            return ReseedJob::Failed { phase, detail };
        }
        CustodianReseedState::Finished(report) => report,
        CustodianReseedState::Unknown(_) => {
            return ReseedJob::Failed {
                phase: UNKNOWN_PHASE.into(),
                detail: "the backup service reported a restore state this version cannot read; \
                         update the app, then run the restore again"
                    .into(),
            };
        }
    };
    let sets: Vec<SetResult> = report
        .sets
        .into_iter()
        .map(|s| SetResult {
            set: DeliveredSet {
                set_name: s.set_name,
                axis: if s.folder {
                    SetAxis::Folder
                } else {
                    SetAxis::Segment
                },
                folder_display_name: s.folder_display_name,
                folder_label: None,
            },
            outcome: match s.outcome {
                CustodianReseedSetOutcome::Materialized { segments, records } => {
                    SetOutcome::Materialized { segments, records }
                }
                CustodianReseedSetOutcome::AlreadyLive => SetOutcome::AlreadyLive,
                CustodianReseedSetOutcome::Refused { code, detail } => SetOutcome::Refused {
                    refusal: SetRefusal::from_code(&code),
                    detail,
                },
                CustodianReseedSetOutcome::Unknown(_) => SetOutcome::Refused {
                    refusal: SetRefusal::from_code(UNKNOWN_OUTCOME_CODE),
                    detail: "the backup service reported an outcome this version cannot read"
                        .into(),
                },
            },
        })
        .collect();
    ReseedJob::Finished(ReseedOutcome {
        delivered: DeliveredCorpus {
            sets: sets.iter().map(|s| s.set.clone()).collect(),
            sidecarless_segments: report.sidecarless_segments,
            folder_paths_without_seal: report.folder_paths_without_seal,
            plaintext_bytes: report.plaintext_bytes,
        },
        sets,
    })
}

/// The agent calls the app side of a desktop re-seed makes: read the store's
/// folder names, start the job, then read how it is going.
/// [`crate::agent::SyncAgentProvisioner`] is the production source; a test
/// hands [`await_agent_reseed`] a scripted one.
pub trait AgentReseedJob: Sync {
    /// The covered-folder display names the agent's store learned
    /// (`GetCustodianFolderNames`) — the target pre-create's input.
    fn folder_names(&self)
    -> impl std::future::Future<Output = Result<Vec<String>, String>> + Send;

    /// Start the ceremony (`ReseedCustodianStore`); answers the job's state at
    /// once, `Running` in the ordinary case.
    fn start(
        &self,
        nest_backup_key: [u8; 32],
    ) -> impl std::future::Future<Output = Result<ReseedJob, String>> + Send;

    /// Read the job's state (`GetCustodianReseed`).
    fn status(&self) -> impl std::future::Future<Output = Result<ReseedJob, String>> + Send;
}

impl<R, B> AgentReseedJob for crate::agent::SyncAgentProvisioner<R, B>
where
    R: fauna_protocol::RpcRequester + Clone + Send + Sync + 'static,
    R::Error: std::fmt::Display,
    B: crate::agent::ProvisioningBearerSource + 'static,
{
    async fn folder_names(&self) -> Result<Vec<String>, String> {
        self.custodian_folder_names()
            .await
            .map_err(|e| e.to_string())
    }

    async fn start(&self, nest_backup_key: [u8; 32]) -> Result<ReseedJob, String> {
        self.reseed_custodian_store(nest_backup_key)
            .await
            .map_err(|e| e.to_string())
    }

    async fn status(&self) -> Result<ReseedJob, String> {
        self.custodian_reseed().await.map_err(|e| e.to_string())
    }
}

/// Why a desktop re-seed ended without the driver's outcome. Every arm is safe
/// to re-run: the driver resumes each phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentReseedStop {
    /// The agent could not be asked, at the start or mid-poll.
    Unreachable(String),
    /// The ceremony itself stopped, in `phase`.
    Failed { phase: String, detail: String },
    /// The agent answered `Idle` mid-job: it restarted and forgot the job.
    Restarted,
}

impl std::fmt::Display for AgentReseedStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(detail) | Self::Failed { detail, .. } => f.write_str(detail),
            Self::Restarted => f.write_str("the backup service restarted during the restore"),
        }
    }
}

/// How often [`await_agent_reseed`] reads the job. It only paces the reads;
/// the wait itself ends on the job's own terminal state.
pub const AGENT_RESEED_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// The app side of a desktop re-seed: prepare the target sets, start the
/// agent's job and wait for it to end (`docs/goal/behavior/backup-destinations.md`
/// § Re-seed → *Where the ceremony runs*). A start-then-poll job because one
/// agent round trip is capped well below a ceremony's length.
///
/// **The target pre-create comes first** (`docs/goal/architecture/
/// writer-signed-change-records.md` ruling (7)(a)(i)): the app holds the seed,
/// so it creates each restored folder's target set — `prepare_targets`, given
/// the display names the agent's store learned; in production
/// `fauna_client_folders::prepare_reseed_targets_logged` — and the job then
/// signs every re-homed row under the nonce that create minted. A names read
/// the agent cannot answer prepares nothing and still starts: each folder set
/// comes back as the driver's typed refusal, which the owner reads.
///
/// The wait is on the job's own terminal state, read back from the agent, never
/// on a timer's say-so. `nest_backup_key` is the owner's seed-derived
/// `NestBackupKey`; it crosses to the agent in the start frame only. Every
/// desktop shell (tui, linux, and the FFI face windows and macOS call) runs
/// through this one function, so the order cannot drift per app.
pub async fn await_agent_reseed<J, P, Fut>(
    agent: &J,
    prepare_targets: P,
    nest_backup_key: [u8; 32],
    poll: std::time::Duration,
) -> Result<ReseedOutcome, AgentReseedStop>
where
    J: AgentReseedJob + ?Sized,
    P: FnOnce(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let names = agent.folder_names().await.unwrap_or_else(|e| {
        tracing::warn!(
            "re-seed: the agent's folder names are unreadable ({e}); no target is prepared"
        );
        Vec::new()
    });
    prepare_targets(names).await;
    let mut job = agent
        .start(nest_backup_key)
        .await
        .map_err(AgentReseedStop::Unreachable)?;
    while job == ReseedJob::Running {
        tokio::time::sleep(poll).await;
        job = agent.status().await.map_err(AgentReseedStop::Unreachable)?;
    }
    match job {
        ReseedJob::Finished(outcome) => Ok(outcome),
        ReseedJob::Failed { phase, detail } => Err(AgentReseedStop::Failed { phase, detail }),
        // `Running` cannot reach here; `Idle` after a start is the restart case.
        ReseedJob::Idle | ReseedJob::Running => Err(AgentReseedStop::Restarted),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An agent that answers the start with `first` and each status read with
    /// the next scripted state, recording how many reads it served.
    struct Scripted {
        names: Result<Vec<String>, String>,
        first: Result<ReseedJob, String>,
        then: std::sync::Mutex<std::collections::VecDeque<Result<ReseedJob, String>>>,
        started_with: std::sync::Mutex<Option<[u8; 32]>>,
        /// Every agent call and pre-create, in order.
        log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Scripted {
        fn new(first: Result<ReseedJob, String>, then: Vec<Result<ReseedJob, String>>) -> Self {
            Self {
                names: Ok(Vec::new()),
                first,
                then: std::sync::Mutex::new(then.into()),
                started_with: std::sync::Mutex::new(None),
                log: std::sync::Arc::default(),
            }
        }
        fn with_names(mut self, names: Result<Vec<String>, String>) -> Self {
            self.names = names;
            self
        }
        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
        fn reads_left(&self) -> usize {
            self.then.lock().unwrap().len()
        }
    }

    impl AgentReseedJob for Scripted {
        async fn folder_names(&self) -> Result<Vec<String>, String> {
            self.log.lock().unwrap().push("names".into());
            self.names.clone()
        }
        async fn start(&self, key: [u8; 32]) -> Result<ReseedJob, String> {
            self.log.lock().unwrap().push("start".into());
            *self.started_with.lock().unwrap() = Some(key);
            self.first.clone()
        }
        async fn status(&self) -> Result<ReseedJob, String> {
            self.then
                .lock()
                .unwrap()
                .pop_front()
                .expect("read past the script: the wait did not stop on a terminal state")
        }
    }

    fn whole() -> ReseedOutcome {
        let mail = set("__mail", SetAxis::Segment);
        ReseedOutcome {
            delivered: DeliveredCorpus {
                sets: vec![mail.clone()],
                ..DeliveredCorpus::default()
            },
            sets: vec![SetResult {
                set: mail,
                outcome: SetOutcome::Materialized {
                    segments: vec![0],
                    records: 1,
                },
            }],
        }
    }

    const NO_WAIT: std::time::Duration = std::time::Duration::ZERO;

    async fn no_targets(_: Vec<String>) {}

    /// The target pre-create (`writer-signed-change-records.md` ruling
    /// (7)(a)(i)) runs in the app, over the names the agent's store learned,
    /// and finishes before the agent's job starts — the job signs under the
    /// nonces the pre-create minted.
    #[tokio::test]
    async fn the_targets_are_prepared_from_the_agents_names_before_the_job_starts() {
        let agent = Scripted::new(Ok(ReseedJob::Finished(whole())), vec![])
            .with_names(Ok(vec!["Docs".into(), "Photos".into()]));
        let log = std::sync::Arc::clone(&agent.log);
        let prepare = move |names: Vec<String>| async move {
            log.lock()
                .unwrap()
                .push(format!("prepare {}", names.join(",")));
        };
        assert_eq!(
            await_agent_reseed(&agent, prepare, [7; 32], NO_WAIT).await,
            Ok(whole())
        );
        assert_eq!(agent.log(), ["names", "prepare Docs,Photos", "start"]);
    }

    /// An agent that cannot answer the names still runs
    /// the ceremony: nothing is pre-created, and each folder set comes back as
    /// the driver's typed refusal rather than a stop with nothing sent.
    #[tokio::test]
    async fn an_unanswered_names_read_prepares_nothing_and_still_starts() {
        let agent = Scripted::new(Ok(ReseedJob::Finished(whole())), vec![])
            .with_names(Err("unknown method".into()));
        let log = std::sync::Arc::clone(&agent.log);
        let prepare = move |names: Vec<String>| async move {
            log.lock().unwrap().push(format!("prepare {}", names.len()));
        };
        assert_eq!(
            await_agent_reseed(&agent, prepare, [7; 32], NO_WAIT).await,
            Ok(whole())
        );
        assert_eq!(agent.log(), ["names", "prepare 0", "start"]);
    }

    #[tokio::test]
    async fn the_wait_ends_on_the_jobs_own_finished_state() {
        let agent = Scripted::new(
            Ok(ReseedJob::Running),
            vec![Ok(ReseedJob::Running), Ok(ReseedJob::Finished(whole()))],
        );
        let got = await_agent_reseed(&agent, no_targets, [7; 32], NO_WAIT).await;
        assert_eq!(got, Ok(whole()));
        assert_eq!(
            agent.reads_left(),
            0,
            "every scripted read up to the verdict is made"
        );
        assert_eq!(
            *agent.started_with.lock().unwrap(),
            Some([7; 32]),
            "the key reaches the agent's start frame unchanged"
        );
    }

    #[tokio::test]
    async fn a_job_that_ends_at_once_is_never_polled() {
        let agent = Scripted::new(Ok(ReseedJob::Finished(whole())), vec![]);
        assert_eq!(
            await_agent_reseed(&agent, no_targets, [7; 32], NO_WAIT).await,
            Ok(whole())
        );
    }

    #[tokio::test]
    async fn a_stopped_job_reports_its_phase() {
        let agent = Scripted::new(
            Ok(ReseedJob::Running),
            vec![Ok(ReseedJob::Failed {
                phase: "grant".into(),
                detail: "refused".into(),
            })],
        );
        assert_eq!(
            await_agent_reseed(&agent, no_targets, [7; 32], NO_WAIT).await,
            Err(AgentReseedStop::Failed {
                phase: "grant".into(),
                detail: "refused".into()
            })
        );
    }

    /// An agent that restarted mid-job forgets it and answers `Idle`: reported
    /// as a stop, never waited on for ever.
    #[tokio::test]
    async fn an_agent_that_forgot_the_job_is_a_restart() {
        let agent = Scripted::new(Ok(ReseedJob::Running), vec![Ok(ReseedJob::Idle)]);
        let got = await_agent_reseed(&agent, no_targets, [7; 32], NO_WAIT).await;
        assert_eq!(got, Err(AgentReseedStop::Restarted));
        assert_eq!(
            got.unwrap_err().to_string(),
            "the backup service restarted during the restore"
        );
    }

    #[tokio::test]
    async fn an_unreachable_agent_stops_the_wait_at_either_read() {
        let at_start = Scripted::new(Err("no agent".into()), vec![]);
        assert_eq!(
            await_agent_reseed(&at_start, no_targets, [7; 32], NO_WAIT).await,
            Err(AgentReseedStop::Unreachable("no agent".into()))
        );
        let mid_poll = Scripted::new(Ok(ReseedJob::Running), vec![Err("pipe closed".into())]);
        assert_eq!(
            await_agent_reseed(&mid_poll, no_targets, [7; 32], NO_WAIT).await,
            Err(AgentReseedStop::Unreachable("pipe closed".into()))
        );
    }

    fn set(name: &str, axis: SetAxis) -> DeliveredSet {
        DeliveredSet {
            set_name: name.into(),
            axis,
            folder_display_name: None,
            folder_label: None,
        }
    }

    /// Every outcome shape survives the seam unchanged — the property the app's
    /// "restored" verdict rests on, since it reads `is_whole` on its side.
    #[test]
    fn a_driver_outcome_round_trips_through_the_wire() {
        let mail = set("__mail", SetAxis::Segment);
        let mut photos = set("__folder/ab/cd", SetAxis::Folder);
        photos.folder_display_name = Some("Photos".into());
        let unnamed = set("__folder/ab/ef", SetAxis::Folder);
        let outcome = ReseedOutcome {
            delivered: DeliveredCorpus {
                sets: vec![mail.clone(), photos.clone(), unnamed.clone()],
                sidecarless_segments: vec![3],
                folder_paths_without_seal: vec!["p".into()],
                plaintext_bytes: 42,
            },
            sets: vec![
                SetResult {
                    set: mail,
                    outcome: SetOutcome::Materialized {
                        segments: vec![0, 1],
                        records: 9,
                    },
                },
                SetResult {
                    set: photos,
                    outcome: SetOutcome::AlreadyLive,
                },
                SetResult {
                    set: unnamed,
                    outcome: SetOutcome::Refused {
                        refusal: SetRefusal::FolderUnnamed,
                        detail: "no name".into(),
                    },
                },
            ],
        };
        assert_eq!(
            job_from_state(state_from_driver(Ok(outcome.clone()))),
            ReseedJob::Finished(outcome)
        );
    }

    /// A value of `T` a newer agent wrote: a variant this build does not name,
    /// decoded through the real wire path (the twin is one extra variant).
    fn newer_agents_value<T: serde::de::DeserializeOwned>() -> T {
        #[derive(serde::Serialize)]
        enum Newer {
            AddedInANewerAgent { n: u32 },
        }
        let frame = fauna_ipc::encode_frame(&Newer::AddedInANewerAgent { n: 1 }).unwrap();
        fauna_ipc::decode_payload(&frame[4..]).expect("the unknown arm takes it")
    }

    /// tier_1: a job state a newer agent names reads as a stop, never as
    /// running — the poll loop ends at once rather than waiting on a job it
    /// cannot follow (`transport.md` § Rule 3 in full).
    #[tokio::test]
    async fn an_unknown_job_state_ends_the_wait_as_a_stop() {
        let unknown: CustodianReseedState = newer_agents_value();
        assert!(matches!(unknown, CustodianReseedState::Unknown(_)));
        let job = job_from_state(unknown);
        assert!(
            matches!(&job, ReseedJob::Failed { phase, .. } if phase == UNKNOWN_PHASE),
            "got {job:?}"
        );
        let agent = Scripted::new(Ok(ReseedJob::Running), vec![Ok(job)]);
        assert!(matches!(
            await_agent_reseed(&agent, no_targets, [7; 32], NO_WAIT).await,
            Err(AgentReseedStop::Failed { phase, .. }) if phase == UNKNOWN_PHASE
        ));
    }

    /// tier_1: a per-set outcome a newer agent names reads as a refusal —
    /// nothing is claimed for that set.
    #[test]
    fn an_unknown_set_outcome_reads_as_refused() {
        let state = CustodianReseedState::Finished(CustodianReseedReport {
            sets: vec![CustodianReseedSet {
                set_name: "s".into(),
                folder: false,
                folder_display_name: None,
                outcome: newer_agents_value(),
            }],
            ..Default::default()
        });
        let ReseedJob::Finished(outcome) = job_from_state(state) else {
            panic!("a finished report stays finished");
        };
        assert!(matches!(
            &outcome.sets[0].outcome,
            SetOutcome::Refused { refusal: SetRefusal::Other { code }, .. }
                if code == UNKNOWN_OUTCOME_CODE
        ));
    }

    #[test]
    fn a_stopped_ceremony_names_its_phase() {
        for (err, phase) in [
            (ReseedError::Grant("no".into()), "grant"),
            (ReseedError::Delivery("no".into()), "delivery"),
            (
                ReseedError::Transport {
                    set_name: "__mail".into(),
                    detail: "gone".into(),
                },
                "transport",
            ),
        ] {
            match job_from_state(state_from_driver(Err(err))) {
                ReseedJob::Failed { phase: got, .. } => assert_eq!(got, phase),
                other => panic!("expected Failed, got {other:?}"),
            }
        }
    }
}
