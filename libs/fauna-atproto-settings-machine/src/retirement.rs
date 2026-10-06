//! The terminal-retirement seam — the client half of S5 slice 5b
//! (`atproto-pds-bridge.md` § Disable & revocation layer 2).
//!
//! Same shape and same reason as [`crate::custody`]: the machine runs the act
//! on status convergence, and the seam exists so machine unit tests drive the
//! flow without HTTP. The production impl is a thin call into
//! [`fauna_client_atproto::tombstone`], which talks to the PLC directory and to
//! the DID's own PDS over the *client's* connections — no nest in the middle,
//! the same property the S4-C seniority check relies on.
//!
//! **One method, deliberately.** This seam used to be two ("has the sweep
//! finished?" / "retire"), on the theory that splitting them let a test assert
//! the ordering. What the split actually did was let the fake hold two
//! independent answers the real directory cannot produce — `RepoGone` beside
//! `AlreadyRetired` — while in production the one log that answers "already
//! retired" (a tombstone head, which declares no services) made the sweep
//! probe answer "cannot tell" forever: `AlreadyRetired` was unreachable, and a
//! report that failed after a successful submit wedged the nest row for good. One method backed
//! by one read of the log makes the contradictory pair unrepresentable, in the
//! fake as in production; the ordering lives in
//! [`fauna_client_atproto::tombstone::converge_retirement`] and is pinned
//! there ([`fauna_client_atproto::tombstone::retirement_step`]) plus by the
//! two-pass machine test over the coupled fake.

pub use fauna_client_atproto::tombstone::{RetirementProgress, TombstoneError};

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait TombstoneActor: fauna_core::MaybeSendSync {
    /// Run one whole retirement converge step off one read of the DID's
    /// published log: already retired → report so; sweep unfinished → wait;
    /// sweep finished → sign with whichever held key the standing head lists
    /// and submit. See
    /// [`fauna_client_atproto::tombstone::converge_retirement`].
    ///
    /// The key material crosses this seam because signing *is* what the seam
    /// does — unlike the nest-API seam, where a key would have no business.
    /// There is exactly one production impl, in this crate, calling one
    /// shared-Rust function; the other impl is test-only.
    async fn converge(
        &self,
        did: String,
        held_keys: Vec<fauna_core::data::AtprotoRotationKey>,
    ) -> Result<RetirementProgress, TombstoneError>;
}

/// Production impl: the client-direct reads of the public PLC directory and of
/// the DID's own PDS.
#[derive(Debug, Default)]
pub struct DirectoryTombstoneActor;

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl TombstoneActor for DirectoryTombstoneActor {
    async fn converge(
        &self,
        did: String,
        held_keys: Vec<fauna_core::data::AtprotoRotationKey>,
    ) -> Result<RetirementProgress, TombstoneError> {
        fauna_client_atproto::tombstone::converge_retirement(
            &fauna_client_atproto::tombstone::directory_base_url(),
            &did,
            &held_keys,
        )
        .await
    }
}

/// Test actor modelling ONE directory log state — the coupling is the point.
/// A successful submit flips the modelled log to `Tombstoned`, so a second
/// pass answers `AlreadyRetired` off the same state, exactly as the real
/// directory would; the impossible "sweep finished AND already retired as two
/// independent knobs" cannot be scripted.
#[cfg(any(test, feature = "test-helpers"))]
pub struct FakeTombstoneActor {
    state: std::sync::Mutex<FakeActorState>,
}

/// The modelled directory/PDS state one converge step reads.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeLogState {
    /// The standing head is a live op; the PDS answers the sweep probe.
    Live {
        /// Whether the DID's PDS answers `RepoNotFound` (sweep finished).
        sweep_finished: bool,
    },
    /// The standing head is already a tombstone.
    Tombstoned,
    /// The directory/PDS cannot be read at all (network down): every step is
    /// the quiet-retry error class.
    Unreachable,
}

#[cfg(any(test, feature = "test-helpers"))]
struct FakeActorState {
    log: FakeLogState,
    /// When set, a submit attempt fails with this message instead of
    /// publishing (the log state then stays `Live`).
    submit_fails: Option<String>,
    calls: Vec<FakeActorCall>,
}

/// One recorded seam call, carrying the ring it was handed so a test can
/// assert the machine passed the *stored ring* — every held key, scalars
/// intact — rather than anything else.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Debug, Clone, PartialEq)]
pub struct FakeActorCall {
    pub did: String,
    pub held_keys: Vec<fauna_core::data::AtprotoRotationKey>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl FakeTombstoneActor {
    /// Defaults to "live head, sweep finished, submit succeeds" — the happy
    /// path; tests that pin a branch script the log state explicitly.
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            state: std::sync::Mutex::new(FakeActorState {
                log: FakeLogState::Live {
                    sweep_finished: true,
                },
                submit_fails: None,
                calls: Vec::new(),
            }),
        })
    }

    pub fn set_log(&self, log: FakeLogState) {
        self.state.lock().unwrap().log = log;
    }

    pub fn set_submit_fails(&self, message: Option<String>) {
        self.state.lock().unwrap().submit_fails = message;
    }

    pub fn calls(&self) -> Vec<FakeActorCall> {
        self.state.lock().unwrap().calls.clone()
    }
}

#[cfg(any(test, feature = "test-helpers"))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl TombstoneActor for FakeTombstoneActor {
    async fn converge(
        &self,
        did: String,
        held_keys: Vec<fauna_core::data::AtprotoRotationKey>,
    ) -> Result<RetirementProgress, TombstoneError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(FakeActorCall {
            did,
            held_keys: held_keys.clone(),
        });
        match s.log.clone() {
            FakeLogState::Unreachable => {
                Err(TombstoneError::Probe("fake: directory unreachable".into()))
            }
            FakeLogState::Tombstoned => Ok(RetirementProgress::AlreadyRetired),
            FakeLogState::Live { sweep_finished } => {
                if !sweep_finished {
                    return Ok(RetirementProgress::SweepStillRunning);
                }
                if let Some(msg) = s.submit_fails.clone() {
                    return Err(TombstoneError::Submit(msg));
                }
                // The submit PUBLISHED: the modelled log is now a tombstone,
                // whatever happens to the report — the coupling under test.
                s.log = FakeLogState::Tombstoned;
                Ok(RetirementProgress::Retired {
                    prev_cid: "bafyhead".into(),
                    signed_with_did_key: held_keys
                        .first()
                        .map(|k| k.pubkey_did_key.clone())
                        .unwrap_or_default(),
                })
            }
        }
    }
}
