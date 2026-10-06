//! In-memory fake `NestApi` for machine-lifecycle tests.
//!
//! Each method has a `Mutex<Option<Result<_, _>>>` slot that callers
//! can pre-populate via `set_*_response`. If the slot is `None`, the
//! method returns a benign default suitable for "happy path" tests
//! that don't care about the response shape.

#![cfg(any(test, debug_assertions, feature = "test-helpers"))]

use std::sync::Mutex;

use super::types::*;

/// The identity the fake stands in for — what its `submit_nat_mode` binds the
/// commit to (the real impl reads it off the connection; the fake models no
/// connection). 64-hex, so it is a well-formed `nest_id` on the wire.
pub const FAKE_NEST_ID: &str = "fafafafafafafafafafafafafafafafafafafafafafafafafafafafafafafafa";

#[derive(Debug, Default)]
pub struct FakeNestApi {
    pub(crate) probe_setup_status_response: Mutex<Option<Result<SetupStatus, ProbeError>>>,
    pub(crate) claim_admin_response: Mutex<Option<Result<ClaimAdminResponse, ClaimAdminError>>>,
    pub(crate) submit_invite_request_response:
        Mutex<Option<Result<InviteRequestResponse, InviteRequestError>>>,
    pub(crate) recheck_invite_request_response:
        Mutex<Option<Result<InviteRequestResponse, InviteRequestError>>>,
    pub(crate) cancel_invite_request_response: Mutex<Option<Result<(), InviteRequestError>>>,
    pub(crate) verify_invite_code_response:
        Mutex<Option<Result<InviteCodeVerification, InviteCodeError>>>,
    pub(crate) register_response: Mutex<Option<Result<RegisterResponse, RegisterError>>>,
    pub(crate) submit_nat_mode_response: Mutex<Option<Result<(), NatModeError>>>,
    /// The handle-check silent-sign-in outcome. Unlike the `Result` slots, this
    /// is a plain `SilentChallengeOutcome` (it carries its own failure buckets,
    /// no `Err`). Default `NotRegistered` mirrors the `claimed: false`
    /// setup-status default — a fresh, unclaimed nest the actor isn't on yet.
    pub(crate) silent_challenge_response: Mutex<Option<SilentChallengeOutcome>>,
    /// Each `secret_hex` passed to `silent_challenge`, in call order. The
    /// handle-check's silent sign-in is the wizard's *authentication* moment,
    /// so which of the machine's two identity slots reaches it is the whole
    /// question `canonical_secret` answers — and it is invisible from the
    /// outcome alone (a wrong-but-valid key just reads as `NotRegistered`).
    /// Recording it is what makes the precedence rule testable.
    pub(crate) silent_challenge_secrets: Mutex<Vec<String>>,
    pub(crate) calls: Mutex<Vec<String>>,
    /// `(handle, mail_domain)` captured from each `claim_admin` call, in order —
    /// so a test can assert the machine threaded the chosen handle's local part
    /// (and its domain) into the claim. The handle is required (a `String`); the
    /// mail_domain stays optional.
    pub(crate) claim_admin_args: Mutex<Vec<(String, Option<String>)>>,
    /// Each claim code passed to `claim_admin`, in call order — so a test can
    /// assert a Retry re-claims with the code the box was BUILT with rather
    /// than a fresh one (`onboarding.md` § 6 *The pending-provision slot*).
    pub(crate) claim_admin_codes: Mutex<Vec<String>>,
    /// Each `NatModeBody` passed to `submit_nat_mode`, in call order — so a
    /// test can assert the signed fields (mode / actor_id / timestamp /
    /// signature) the machine built for the commit.
    pub(crate) submit_nat_mode_bodies: Mutex<Vec<NatModeBody>>,
    /// Each `RegisterBody` passed to `register`, in call order — the twin of
    /// `claim_admin_args` for the *other* handle-bearing wire call. The claim
    /// path's local-part threading was pinned by a test from the start; the
    /// register path's was not, and it shipped sending the whole
    /// `alice@nest.example` as the handle (which the nest's `validate_handle`
    /// rejects). Assert on the recorded body so that cannot recur.
    pub(crate) register_bodies: Mutex<Vec<RegisterBody>>,
    pub(crate) submit_invite_request_bodies: Mutex<Vec<InviteRequestBody>>,
    pub(crate) age_nonce_response: Mutex<Option<Result<AgeNonce, AgeNonceError>>>,
    pub(crate) restore_escrowed_seed_response:
        Mutex<Option<Result<RestoredIdentity, RestoreSeedError>>>,
    /// Each `(base_url, recovery_secret_hex, actor_id_hex, handle)` the machine
    /// passed to `restore_escrowed_seed`, in call order. The account halves are
    /// what the restore's correctness turns on — the kit's actor id must reach
    /// the wire when the payload carried one, and the account field's handle
    /// must be the bare local part (the nest stores handles without `@domain`,
    /// the same trap the register path already shipped once).
    pub(crate) restore_escrowed_seed_args: Mutex<Vec<RestoreSeedArgs>>,
    /// `(method_name, base_url)` for EVERY call to EVERY method, in call
    /// order — the general twin of `calls` (which drops the URL) and of
    /// `restore_escrowed_seed_args` (which threads it for one method only).
    /// Exists because every other method silently discarded the `base_url`
    /// it was dialed with, so no test in this crate could tell "the machine
    /// dialed the box it just provisioned" from "the machine dialed the
    /// empty string". A separate field rather than widening
    /// `calls` itself: `calls()` and its ~20 existing callers stay exactly
    /// as they read today.
    pub(crate) base_url_calls: Mutex<Vec<(String, String)>>,
}

/// `(base_url, recovery_secret_hex, actor_id_hex, handle)` as the machine
/// passed them.
pub type RestoreSeedArgs = (String, String, Option<String>, Option<String>);

impl FakeNestApi {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_probe_setup_status_response(&self, r: Result<SetupStatus, ProbeError>) {
        *self.probe_setup_status_response.lock().unwrap() = Some(r);
    }
    pub fn set_claim_admin_response(&self, r: Result<ClaimAdminResponse, ClaimAdminError>) {
        *self.claim_admin_response.lock().unwrap() = Some(r);
    }
    pub fn set_submit_invite_request_response(
        &self,
        r: Result<InviteRequestResponse, InviteRequestError>,
    ) {
        *self.submit_invite_request_response.lock().unwrap() = Some(r);
    }
    pub fn set_recheck_invite_request_response(
        &self,
        r: Result<InviteRequestResponse, InviteRequestError>,
    ) {
        *self.recheck_invite_request_response.lock().unwrap() = Some(r);
    }
    pub fn set_cancel_invite_request_response(&self, r: Result<(), InviteRequestError>) {
        *self.cancel_invite_request_response.lock().unwrap() = Some(r);
    }
    pub fn set_verify_invite_code_response(
        &self,
        r: Result<InviteCodeVerification, InviteCodeError>,
    ) {
        *self.verify_invite_code_response.lock().unwrap() = Some(r);
    }
    pub fn set_age_nonce_response(&self, r: Result<AgeNonce, AgeNonceError>) {
        *self.age_nonce_response.lock().unwrap() = Some(r);
    }
    pub fn set_register_response(&self, r: Result<RegisterResponse, RegisterError>) {
        *self.register_response.lock().unwrap() = Some(r);
    }
    pub fn set_submit_nat_mode_response(&self, r: Result<(), NatModeError>) {
        *self.submit_nat_mode_response.lock().unwrap() = Some(r);
    }
    pub fn set_silent_challenge_response(&self, r: SilentChallengeOutcome) {
        *self.silent_challenge_response.lock().unwrap() = Some(r);
    }
    pub fn set_restore_escrowed_seed_response(
        &self,
        r: Result<RestoredIdentity, RestoreSeedError>,
    ) {
        *self.restore_escrowed_seed_response.lock().unwrap() = Some(r);
    }

    /// The ordinary success fixture: a seed with no predecessor section, which
    /// is what every pre-succession kit's blob carries. Tests exercising the
    /// succession re-seal window build a [`RestoredIdentity`] by hand.
    pub fn set_restored_seed(&self, seed_hex: &str) {
        self.set_restore_escrowed_seed_response(Ok(RestoredIdentity {
            seed_hex: seed_hex.to_string(),
            predecessors: Vec::new(),
            predecessors_unreadable: None,
        }));
    }

    /// Returns the methods that have been called, in order. Useful for
    /// asserting on call sequence in lifecycle tests.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// The `(handle, mail_domain)` pairs passed to `claim_admin`, in call order.
    pub fn claim_admin_args(&self) -> Vec<(String, Option<String>)> {
        self.claim_admin_args.lock().unwrap().clone()
    }

    /// The claim codes passed to `claim_admin`, in call order.
    pub fn claim_admin_codes(&self) -> Vec<String> {
        self.claim_admin_codes.lock().unwrap().clone()
    }

    /// The bodies passed to `submit_nat_mode`, in call order.
    pub fn submit_nat_mode_bodies(&self) -> Vec<NatModeBody> {
        self.submit_nat_mode_bodies.lock().unwrap().clone()
    }

    /// The bodies passed to `register`, in call order.
    pub fn register_bodies(&self) -> Vec<RegisterBody> {
        self.register_bodies.lock().unwrap().clone()
    }
    pub fn submit_invite_request_bodies(&self) -> Vec<InviteRequestBody> {
        self.submit_invite_request_bodies.lock().unwrap().clone()
    }

    /// The `secret_hex` values passed to `silent_challenge`, in call order.
    pub fn silent_challenge_secrets(&self) -> Vec<String> {
        self.silent_challenge_secrets.lock().unwrap().clone()
    }

    /// The `(base_url, recovery_secret_hex, actor_id_hex, handle)` tuples
    /// passed to `restore_escrowed_seed`, in call order.
    pub fn restore_escrowed_seed_args(&self) -> Vec<RestoreSeedArgs> {
        self.restore_escrowed_seed_args.lock().unwrap().clone()
    }

    /// The `(method_name, base_url)` pair for every call to every method, in
    /// call order — what a test asserts on to tell "dialed the box the run
    /// just provisioned" from "dialed the empty string" or a stale cached
    /// URL. `calls()` alone cannot: it drops the URL entirely.
    pub fn base_url_calls(&self) -> Vec<(String, String)> {
        self.base_url_calls.lock().unwrap().clone()
    }
}

use async_trait::async_trait;

// Match the trait's per-target boxing (see `NestApi` in `mod.rs`) — the
// test-helpers wasm bundle compiles this impl too.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl super::NestApi for FakeNestApi {
    async fn probe_setup_status(&self, base_url: &str) -> Result<SetupStatus, ProbeError> {
        self.calls.lock().unwrap().push("probe_setup_status".into());
        self.base_url_calls
            .lock()
            .unwrap()
            .push(("probe_setup_status".into(), base_url.to_string()));
        self.probe_setup_status_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Ok(SetupStatus::default()))
    }

    async fn silent_challenge(&self, base_url: &str, secret_hex: &str) -> SilentChallengeOutcome {
        self.calls.lock().unwrap().push("silent_challenge".into());
        self.base_url_calls
            .lock()
            .unwrap()
            .push(("silent_challenge".into(), base_url.to_string()));
        self.silent_challenge_secrets
            .lock()
            .unwrap()
            .push(secret_hex.to_string());
        self.silent_challenge_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(SilentChallengeOutcome::NotRegistered)
    }

    async fn claim_admin(
        &self,
        base_url: &str,
        code: &str,
        _secret_hex: &str,
        handle: &str,
        mail_domain: Option<&str>,
    ) -> Result<ClaimAdminResponse, ClaimAdminError> {
        self.calls.lock().unwrap().push("claim_admin".into());
        self.base_url_calls
            .lock()
            .unwrap()
            .push(("claim_admin".into(), base_url.to_string()));
        self.claim_admin_codes
            .lock()
            .unwrap()
            .push(code.to_string());
        self.claim_admin_args
            .lock()
            .unwrap()
            .push((handle.to_string(), mail_domain.map(str::to_string)));
        self.claim_admin_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| {
                Ok(ClaimAdminResponse {
                    ok: true,
                    token: Some("tok".into()),
                    expires_at: Some(0),
                    domain: None,
                    deployment_seed: None,
                })
            })
    }

    async fn submit_invite_request(
        &self,
        base_url: &str,
        body: InviteRequestBody,
    ) -> Result<InviteRequestResponse, InviteRequestError> {
        self.calls
            .lock()
            .unwrap()
            .push("submit_invite_request".into());
        self.base_url_calls
            .lock()
            .unwrap()
            .push(("submit_invite_request".into(), base_url.to_string()));
        self.submit_invite_request_bodies.lock().unwrap().push(body);
        self.submit_invite_request_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| {
                Ok(InviteRequestResponse {
                    id: 1,
                    status: "pending".into(),
                    denial_reason: None,
                    quota: None,
                })
            })
    }

    async fn cancel_invite_request(
        &self,
        base_url: &str,
        _secret_hex: &str,
    ) -> Result<(), InviteRequestError> {
        self.calls
            .lock()
            .unwrap()
            .push("cancel_invite_request".into());
        self.base_url_calls
            .lock()
            .unwrap()
            .push(("cancel_invite_request".into(), base_url.to_string()));
        self.cancel_invite_request_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn recheck_invite_request(
        &self,
        base_url: &str,
        _actor_id_hex: &str,
    ) -> Result<InviteRequestResponse, InviteRequestError> {
        self.calls
            .lock()
            .unwrap()
            .push("recheck_invite_request".into());
        self.base_url_calls
            .lock()
            .unwrap()
            .push(("recheck_invite_request".into(), base_url.to_string()));
        self.recheck_invite_request_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| {
                Ok(InviteRequestResponse {
                    id: 1,
                    status: "pending".into(),
                    denial_reason: None,
                    quota: None,
                })
            })
    }

    async fn verify_invite_code(
        &self,
        base_url: &str,
        _code: &str,
    ) -> Result<InviteCodeVerification, InviteCodeError> {
        self.calls.lock().unwrap().push("verify_invite_code".into());
        self.base_url_calls
            .lock()
            .unwrap()
            .push(("verify_invite_code".into(), base_url.to_string()));
        self.verify_invite_code_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| {
                Ok(InviteCodeVerification {
                    invite_id: "inv-1".into(),
                    supervised_by: None,
                })
            })
    }

    async fn age_nonce(&self, base_url: &str) -> Result<AgeNonce, AgeNonceError> {
        self.calls.lock().unwrap().push("age_nonce".into());
        self.base_url_calls
            .lock()
            .unwrap()
            .push(("age_nonce".into(), base_url.to_string()));
        self.age_nonce_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| {
                // The default reply lists no platform: it models an unarmed
                // nest, which holds no verifier (verifies nothing). Tests
                // that want an attestation to ride set the list.
                Ok(AgeNonce {
                    nonce_hex: "ab".repeat(32),
                    expires_in_secs: 300,
                    attestation_platforms: Vec::new(),
                })
            })
    }

    async fn register(
        &self,
        base_url: &str,
        body: RegisterBody,
    ) -> Result<RegisterResponse, RegisterError> {
        self.calls.lock().unwrap().push("register".into());
        self.base_url_calls
            .lock()
            .unwrap()
            .push(("register".into(), base_url.to_string()));
        self.register_bodies.lock().unwrap().push(body);
        self.register_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Ok(RegisterResponse { ok: true }))
    }

    async fn submit_nat_mode(
        &self,
        base_url: &str,
        secret_hex: &str,
        mode: NodeMode,
    ) -> Result<(), NatModeError> {
        self.calls.lock().unwrap().push("submit_nat_mode".into());
        self.base_url_calls
            .lock()
            .unwrap()
            .push(("submit_nat_mode".into(), base_url.to_string()));
        // Build the body exactly as the production impl would, bound to the
        // fake's fixed identity (it models no connection to read one off),
        // so tests keep asserting the signed fields.
        let body = crate::machine::build_signed_nat_mode_body(secret_hex, mode, FAKE_NEST_ID)
            .ok_or(NatModeError::Invalid {
                reason: "invalid identity secret".into(),
            })?;
        self.submit_nat_mode_bodies.lock().unwrap().push(body);
        self.submit_nat_mode_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn restore_escrowed_seed(
        &self,
        base_url: &str,
        recovery_secret_hex: &str,
        actor_id_hex: Option<&str>,
        handle: Option<&str>,
    ) -> Result<RestoredIdentity, RestoreSeedError> {
        self.calls
            .lock()
            .unwrap()
            .push("restore_escrowed_seed".into());
        self.base_url_calls
            .lock()
            .unwrap()
            .push(("restore_escrowed_seed".into(), base_url.to_string()));
        self.restore_escrowed_seed_args.lock().unwrap().push((
            base_url.to_string(),
            recovery_secret_hex.to_string(),
            actor_id_hex.map(str::to_string),
            handle.map(str::to_string),
        ));
        self.restore_escrowed_seed_response
            .lock()
            .unwrap()
            .clone()
            // The benign default is the honest refusal a fresh nest gives: no
            // blob rests until a signed-in device has put one. A test that
            // wants a success fixtures one.
            .unwrap_or(Err(RestoreSeedError::NoEscrow))
    }
}
