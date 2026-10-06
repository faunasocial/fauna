//! The genesis-seniority custody check seam (S4-C,
//! `atproto-pds-bridge.md` § State & data shape).
//!
//! The machine runs the check after every status convergence; the seam exists
//! so machine unit tests program verdicts without HTTP. The production impl is
//! the client-direct PLC-directory fetch in
//! [`fauna_client_atproto::genesis_verify`] — no nest dependency by design.

pub use fauna_client_atproto::genesis_verify::{MismatchReason, SeniorityVerdict, VerifyFailure};

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait GenesisVerifier: fauna_core::MaybeSendSync {
    /// Resolve `did`'s standing op log from the public directory and verify
    /// every standing op's `rotationKeys[0]` is one of `held_did_keys` — the
    /// client's whole user-custodied ring, not a single chosen key, because
    /// which held key a DID publishes senior is that DID's own log's fact
    /// (fresh-key-per-mint leaves retired identities' burned keys in the
    /// ring beside the live one's).
    async fn verify(
        &self,
        did: String,
        held_did_keys: Vec<String>,
    ) -> Result<SeniorityVerdict, VerifyFailure>;
}

/// Production impl: the direct HTTPS read of the public PLC directory.
#[derive(Debug, Default)]
pub struct DirectoryGenesisVerifier;

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl GenesisVerifier for DirectoryGenesisVerifier {
    async fn verify(
        &self,
        did: String,
        held_did_keys: Vec<String>,
    ) -> Result<SeniorityVerdict, VerifyFailure> {
        fauna_client_atproto::genesis_verify::fetch_and_verify(
            &fauna_client_atproto::genesis_verify::plc_directory_base_url(),
            &did,
            &held_did_keys,
        )
        .await
    }
}

/// Test verifier: programmed verdict per call, plus a call recorder.
#[cfg(any(test, feature = "test-helpers"))]
pub struct FakeGenesisVerifier {
    state: std::sync::Mutex<FakeVerifierState>,
}

#[cfg(any(test, feature = "test-helpers"))]
struct FakeVerifierState {
    verdict: Result<SeniorityVerdict, VerifyFailure>,
    calls: Vec<(String, Vec<String>)>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl FakeGenesisVerifier {
    pub fn new(verdict: Result<SeniorityVerdict, VerifyFailure>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            state: std::sync::Mutex::new(FakeVerifierState {
                verdict,
                calls: Vec::new(),
            }),
        })
    }

    pub fn set_verdict(&self, verdict: Result<SeniorityVerdict, VerifyFailure>) {
        self.state.lock().unwrap().verdict = verdict;
    }

    /// `(did, held_did_keys)` per call, in order.
    pub fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.state.lock().unwrap().calls.clone()
    }
}

#[cfg(any(test, feature = "test-helpers"))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl GenesisVerifier for FakeGenesisVerifier {
    async fn verify(
        &self,
        did: String,
        held_did_keys: Vec<String>,
    ) -> Result<SeniorityVerdict, VerifyFailure> {
        let mut s = self.state.lock().unwrap();
        s.calls.push((did, held_did_keys));
        match &s.verdict {
            Ok(v) => Ok(v.clone()),
            // VerifyFailure isn't Clone (carries serde_json::Error); rebuild
            // the quiet-class marker the tests use.
            Err(e) => Err(VerifyFailure::Fetch(e.to_string())),
        }
    }
}
