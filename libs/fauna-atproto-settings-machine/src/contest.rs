//! The recovery-fork contest seam — the client half of the 72 h senior-key
//! remedy (`atproto-pds-bridge.md` § State & data shape, the recovery-fork
//! contest).
//!
//! Same shape and same reason as [`crate::custody`] and [`crate::retirement`]:
//! the machine runs the act on status convergence, and the seam exists so
//! machine unit tests drive the flow without HTTP. The production impl is a
//! thin call into [`fauna_client_atproto::recovery_fork`], which reads the PLC
//! directory and submits the fork over the *client's own* connections — no
//! nest in the path, before or after (decision 9), which is what keeps the
//! remedy reachable when a hostile box denies the identity.
//!
//! **Two methods, and the split is not the one `retirement` rejected.** There,
//! two seam methods let a fake hold two answers the real directory cannot
//! produce simultaneously. Here [`ContestActor::plan`] and
//! [`ContestActor::converge`] read the same log for the same question, and the
//! fake below models ONE log state that both answer from — so the
//! contradictory pair stays unrepresentable. They are separate because they
//! serve different callers: the snapshot needs a plan on every convergence
//! (the card renders off it), while converging must happen only when a consent
//! covers the violation. Collapsing them would make rendering the card an act.

pub use fauna_client_atproto::recovery_fork::{
    ContestEligibility, ContestPlan, ContestProgress, ForkError, Violation,
};

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait ContestActor: fauna_core::MaybeSendSync {
    /// Read the DID's published log and plan a contest from it — pure
    /// reporting, **nothing is signed**. Drives the ceremony card's state.
    async fn plan(
        &self,
        did: String,
        held_did_keys: Vec<String>,
        now_unix_secs: u64,
    ) -> Result<ContestPlan, ForkError>;

    /// Re-read the log and, if the user's recorded consent still covers its
    /// current first standing violation, build, sign and submit the fork. See
    /// [`fauna_client_atproto::recovery_fork::converge_contest`].
    ///
    /// The key material crosses this seam because signing *is* what the seam
    /// does — the same carve-out [`crate::retirement`] makes, and for the same
    /// reason it is deliberately not on the nest seam.
    async fn converge(
        &self,
        did: String,
        held_keys: Vec<fauna_core::data::AtprotoRotationKey>,
        intents: Vec<fauna_core::data::AtprotoContestIntent>,
        now_unix_secs: u64,
    ) -> Result<ContestProgress, ForkError>;
}

/// Production impl: the client-direct read of, and submit to, the public PLC
/// directory.
#[derive(Debug, Default)]
pub struct DirectoryContestActor;

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl ContestActor for DirectoryContestActor {
    async fn plan(
        &self,
        did: String,
        held_did_keys: Vec<String>,
        now_unix_secs: u64,
    ) -> Result<ContestPlan, ForkError> {
        let body = fauna_client_atproto::genesis_verify::fetch_audit_log(
            &fauna_client_atproto::recovery_fork::directory_base_url(),
            &did,
        )
        .await?;
        fauna_client_atproto::recovery_fork::contest_plan(
            &body,
            &did,
            &held_did_keys,
            now_unix_secs,
        )
    }

    async fn converge(
        &self,
        did: String,
        held_keys: Vec<fauna_core::data::AtprotoRotationKey>,
        intents: Vec<fauna_core::data::AtprotoContestIntent>,
        now_unix_secs: u64,
    ) -> Result<ContestProgress, ForkError> {
        fauna_client_atproto::recovery_fork::converge_contest(
            &fauna_client_atproto::recovery_fork::directory_base_url(),
            &did,
            &held_keys,
            &intents,
            now_unix_secs,
        )
        .await
    }
}

/// Test actor modelling ONE directory log — the coupling is the point, exactly
/// as in [`crate::retirement`]. A successful submit nullifies the contested
/// suffix in the modelled log, so a second pass plans `NoViolation` off the
/// same state and the "contested but still alarming" pair cannot be scripted.
#[cfg(any(test, feature = "test-helpers"))]
pub struct FakeContestActor {
    state: std::sync::Mutex<FakeContestState>,
}

/// The modelled log state one step reads.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeContestLog {
    /// Custody holds — nothing to contest.
    Clean,
    /// A standing violation this ring can fork away from.
    Contestable {
        contested_op_cid: String,
        fork_point_cid: String,
    },
    /// A standing violation with no remedy (genesis, or a closed window).
    NotContestable(ContestEligibility),
    /// The directory cannot be read: every step is the quiet-retry class.
    Unreachable,
}

#[cfg(any(test, feature = "test-helpers"))]
struct FakeContestState {
    log: FakeContestLog,
    /// When set, a submit attempt fails with this message and the modelled log
    /// stays as it was.
    submit_fails: Option<String>,
    converge_calls: Vec<FakeContestCall>,
    plan_calls: u32,
}

/// One recorded converge call, carrying what the machine actually handed the
/// seam — the ring (scalars intact) and the consents.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Debug, Clone, PartialEq)]
pub struct FakeContestCall {
    pub did: String,
    pub held_keys: Vec<fauna_core::data::AtprotoRotationKey>,
    pub intents: Vec<fauna_core::data::AtprotoContestIntent>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl FakeContestActor {
    /// Defaults to a clean log — the overwhelmingly common state, and the one
    /// a test that is not about contests should see.
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            state: std::sync::Mutex::new(FakeContestState {
                log: FakeContestLog::Clean,
                submit_fails: None,
                converge_calls: Vec::new(),
                plan_calls: 0,
            }),
        })
    }

    pub fn set_log(&self, log: FakeContestLog) {
        self.state.lock().unwrap().log = log;
    }

    pub fn set_submit_fails(&self, message: Option<String>) {
        self.state.lock().unwrap().submit_fails = message;
    }

    pub fn converge_calls(&self) -> Vec<FakeContestCall> {
        self.state.lock().unwrap().converge_calls.clone()
    }

    pub fn plan_calls(&self) -> u32 {
        self.state.lock().unwrap().plan_calls
    }

    pub fn log(&self) -> FakeContestLog {
        self.state.lock().unwrap().log.clone()
    }

    fn plan_from(log: &FakeContestLog) -> Result<ContestPlan, ForkError> {
        match log {
            FakeContestLog::Unreachable => Err(ForkError::Directory(
                fauna_client_atproto::genesis_verify::VerifyFailure::Fetch(
                    "fake: directory unreachable".into(),
                ),
            )),
            FakeContestLog::Clean => Ok(ContestPlan::NoViolation),
            FakeContestLog::Contestable {
                contested_op_cid,
                fork_point_cid,
            } => Ok(ContestPlan::Violation(Box::new(Violation {
                contested_op_cid: contested_op_cid.clone(),
                reason: fauna_client_atproto::genesis_verify::MismatchReason::SeniorKeyDiffers {
                    standing_index: 1,
                    found: Some("did:key:zQ3shBoxJuniorKey".into()), // gitleaks:allow
                },
                deadline_unix: Some(u64::MAX),
                eligibility: ContestEligibility::Contestable(
                    fauna_client_atproto::recovery_fork::ForkPoint {
                        cid: fork_point_cid.clone(),
                        op: serde_json::json!({"type": "plc_operation"}),
                        signer_did_key: String::new(),
                        rotation_keys: Vec::new(),
                    },
                ),
            }))),
            FakeContestLog::NotContestable(eligibility) => {
                Ok(ContestPlan::Violation(Box::new(Violation {
                    contested_op_cid: "bafycontested".into(),
                    reason:
                        fauna_client_atproto::genesis_verify::MismatchReason::SeniorKeyDiffers {
                            standing_index: 0,
                            found: Some("did:key:zQ3shBoxJuniorKey".into()), // gitleaks:allow
                        },
                    deadline_unix: Some(0),
                    eligibility: eligibility.clone(),
                })))
            }
        }
    }
}

#[cfg(any(test, feature = "test-helpers"))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl ContestActor for FakeContestActor {
    async fn plan(
        &self,
        _did: String,
        _held_did_keys: Vec<String>,
        _now_unix_secs: u64,
    ) -> Result<ContestPlan, ForkError> {
        let mut s = self.state.lock().unwrap();
        s.plan_calls += 1;
        Self::plan_from(&s.log)
    }

    async fn converge(
        &self,
        did: String,
        held_keys: Vec<fauna_core::data::AtprotoRotationKey>,
        intents: Vec<fauna_core::data::AtprotoContestIntent>,
        _now_unix_secs: u64,
    ) -> Result<ContestProgress, ForkError> {
        let mut s = self.state.lock().unwrap();
        s.converge_calls.push(FakeContestCall {
            did: did.clone(),
            held_keys: held_keys.clone(),
            intents: intents.clone(),
        });
        let plan = Self::plan_from(&s.log)?;
        let violation = match plan {
            ContestPlan::NoViolation => return Ok(ContestProgress::NothingToContest),
            ContestPlan::Violation(v) => v,
        };
        // The consent gate, modelled exactly as the real converge applies it:
        // the pair `(did, contested_op_cid)` IS the scope, checked against the
        // FRESH read's violation.
        if !intents
            .iter()
            .any(|i| i.did == did && i.contested_op_cid == violation.contested_op_cid)
        {
            return Ok(ContestProgress::NoConsentForThisViolation);
        }
        let ContestEligibility::Contestable(fork_point) = violation.eligibility else {
            return Ok(ContestProgress::NotContestable(violation.eligibility));
        };
        if let Some(msg) = s.submit_fails.clone() {
            return Err(ForkError::Submit(msg));
        }
        // The submit LANDED: the contested suffix is nullified, so the modelled
        // log is clean from here — the coupling that makes a completed contest
        // converge to `NothingToContest` on the next pass, and a stale intent
        // inert forever.
        s.log = FakeContestLog::Clean;
        Ok(ContestProgress::Contested {
            fork_prev_cid: fork_point.cid,
            signed_with_did_key: held_keys
                .first()
                .map(|k| k.pubkey_did_key.clone())
                .unwrap_or_default(),
        })
    }
}
