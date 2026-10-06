//! In-memory fakes for the four injected seams. Gated behind the
//! `test-helpers` feature; lit up automatically by the crate's own
//! integration tests via the self dev-dependency in `Cargo.toml`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ed25519_dalek::SigningKey;
use fauna_client_bridges::HolderInfo;
use fauna_mls::wrapped_blob::{
    MlsSnapshotBlob, SubmissionToken, WrappedMsekBlob, WrappedSubmissionTokenBlob,
};

use fauna_core::grant_event::GrantEvent;

use crate::error::{NestError, SignerError};
use crate::machine::{FetchedSpamModel, IdentitySigner, NestClient};
use fauna_protocol::bridge_routing::EpochSealKey;
use fauna_protocol::wrapped_blob::{HolderSealTarget, PutSpamModelOutcome, SpamModelHolderCopy};

/// Records every nest call so tests can assert the right blobs
/// flowed in the right order. Cheap-clone via `Arc<Mutex<...>>`.
#[derive(Default, Clone)]
pub struct FakeNestClient {
    inner: Arc<Mutex<FakeNestState>>,
}

/// One `provision_recipient_mls_pubkey` call recorded by the fake:
/// `(actor_id, recipient_x25519_pubkey, mlkem_ek, epoch_keys)`. The machine
/// always publishes the third (the 1184-B ML-KEM-768 ek, S3d leg A — required
/// on the wire) and the fourth (the content-sealing-epoch horizon, B3b), so
/// the fourth is `Some` on every record it produces; it stays `Option` because
/// the wire field is.
pub type RecipientPubkeyRecord = ([u8; 32], [u8; 32], Vec<u8>, Option<Vec<EpochSealKey>>);

/// One recorded `renew_grant` call: `(grant_id, new_epoch_start,
/// new_epoch_end, appended_keys)`.
pub type RenewedGrant = ([u8; 16], u64, u64, Vec<Vec<u8>>);

#[derive(Default)]
pub struct FakeNestState {
    pub provision_wrapped_mls: Vec<WrappedMsekBlob>,
    pub provision_mls_snapshot: Vec<MlsSnapshotBlob>,
    pub provision_submission_token: Vec<WrappedSubmissionTokenBlob>,
    pub revoke_wrapped_mls: Vec<([u8; 32], String)>,
    pub revoke_submission_token: Vec<([u8; 32], String)>,
    /// `(actor_id, recipient_x25519_pubkey, mlkem_ek, epoch_keys)` tuples
    /// registered via `provision_recipient_mls_pubkey` — see
    /// [`RecipientPubkeyRecord`].
    pub provision_recipient_pubkey: Vec<RecipientPubkeyRecord>,
    /// The capability tokens this fake nest "advertises" in `fauna.nest.info`
    /// (consumed by `nest_supports`). Empty by default ⇒ `nest_supports` is
    /// `false` for everything (e.g. `spam-model-sealed-at-rest` off ⇒ the
    /// spam-model write takes the server path).
    pub advertised_capabilities: Vec<String>,
    /// Every `set_mail_enabled` call's argument, in order. Lets tests assert
    /// that enabling mail also flips the deployment subsystem (design A).
    pub set_mail_enabled: Vec<bool>,
    /// Every `set_mail_serving_enabled` call's argument, in order. Lets tests
    /// assert the per-actor IMAP/CalDAV-serving toggle fired.
    pub set_mail_serving_enabled: Vec<bool>,
    /// The per-actor serving flag the fake reads back. `None` ⇒ never set ⇒
    /// `get_mail_serving_enabled` returns `true` — mirroring the nest's
    /// `actor_mail_serving` default-on (`.unwrap_or(true)`). A `set` records
    /// the call *and* updates this so a subsequent `get`/`hydrate` round-trips.
    pub mail_serving_enabled: Option<bool>,
    /// If set, the matching provision call fails. Decremented on
    /// each call; the next call after it reaches zero succeeds.
    /// Use to simulate a mid-rotation transient nest failure.
    pub fail_provision_wrapped_mls_for_n_calls: u32,
    /// If set, `set_mail_enabled` returns this error instead of recording the
    /// call. `Rejected` simulates a non-admin actor (design A's server-enforced
    /// scoping); `Transient` simulates a connectivity failure.
    pub fail_set_mail_enabled_with: Option<NestError>,
    /// The effective CalDAV port the fake reads back via `get_caldav_port`.
    /// `None` ⇒ unset ⇒ returns `DEFAULT_CALDAV_PORT`, mirroring the nest's
    /// `.unwrap_or(DEFAULT_CALDAV_PORT)` for the admin-set port singleton.
    pub caldav_port: Option<u16>,
    /// If set, `get_caldav_port` returns this error — simulates a failed
    /// read of `get_caldav_port` (the machine must
    /// degrade to the for-node-URL default, not fail hydrate).
    pub fail_get_caldav_port_with: Option<NestError>,
    /// What `serves_any_webdav_set` reports — the per-actor "serves ≥1 folder
    /// over WebDAV" fold of `fauna.folders.list`. Default `false` (no served
    /// set), the state of a fresh actor.
    pub serves_any_webdav_set: bool,
    /// If set, `serves_any_webdav_set` returns this error — simulates a failed
    /// read. The machine must degrade to
    /// "not serving", not fail hydrate.
    pub fail_serves_any_webdav_set_with: Option<NestError>,
    /// The stored per-user spam model — `put_spam_model` overwrites it,
    /// `fetch_spam_model` returns it verbatim (the sealed-at-rest nest's opaque
    /// store). `None` ⇒ untrained (cold start). A test seeds an initial sealed
    /// blob to exercise the read-mutate-write round-trip.
    pub spam_model: Option<Vec<u8>>,
    /// `true` ⇒ the seeded [`Self::spam_model`] plays the nest's read-time
    /// **cold-start seed** (`fetch_spam_model` answers `stored_sealed: false`),
    /// not a stored model. Default `false`: a seeded blob is the actor's stored
    /// model. Cleared by every `put_spam_model` (the write stores a real model).
    pub spam_model_is_cold_start_seed: bool,
    /// The post bodies `fetch_post_body_text` serves, keyed by `content_id`. A
    /// missing key answers `Err(Rejected)` (the read gate's `not_found`).
    pub post_bodies: std::collections::BTreeMap<String, String>,
    /// Every `fetch_post_body_text` call's `content_id`, in order.
    pub post_body_fetches: Vec<String>,
    /// Every `moderation_train` call's `(content_id, verdict)`, in order.
    pub moderation_trains: Vec<(String, String)>,
    /// If set, `moderation_train` records the call and returns this error.
    pub fail_moderation_train_with: Option<NestError>,
    /// The advisory `sample_count` from the most recent `put_spam_model`, so a
    /// test can assert the write carried the post-mutation document count.
    pub put_spam_model_sample_count: Option<u32>,
    /// The `history_op` from the most recent `put_spam_model` (the atomic
    /// train-INSERT / undo-DELETE audit-row mutation), so a test can assert a
    /// client-side undo rode a `Delete` and a social train rode `None`.
    pub put_spam_model_history_op: Option<Option<fauna_protocol::bridge_routing::SpamHistoryOp>>,
    /// The deployment-baseline write signal `fetch_spam_model` volunteers (piece
    /// (b)): whether the actor is opted in. A test seeds `true` (with a
    /// [`Self::holder_seal_target`]) to exercise the copy-attach path.
    pub contribute_baseline: bool,
    /// The aggregation-holder seal target `fetch_spam_model` volunteers. `None` ⇒
    /// no holder enrolled ⇒ the write attaches no copy.
    pub holder_seal_target: Option<HolderSealTarget>,
    /// The `holder_copy` from the most recent `put_spam_model`, so a test can
    /// assert an opted-in write attached a copy sealed to the volunteered holder
    /// and an opted-out write attached `None`.
    pub put_spam_model_holder_copy: Option<Option<SpamModelHolderCopy>>,
    /// The outcome the next `put_spam_model` answers. Default `Written`; a test
    /// seeds `DuplicateSignal` to play the nest's one-lesson rule
    /// (`mail-spam.md` § 3) — the fake then keeps `spam_model` UNCHANGED, as
    /// the nest keeps its row.
    pub put_spam_model_outcome: PutSpamModelOutcome,
    /// Every `mint_grant` call's canonical `GrantBlob` bytes, in order — a test
    /// asserts the contribute toggle-ON minted the keyless `content.read{spam-model}`
    /// grant to the volunteered holder.
    pub minted_grants: Vec<Vec<u8>>,
    /// Every `revoke_grant` call's 16-byte grant id, in order — a test asserts the
    /// toggle-OFF path revoked the standing baseline grant.
    pub revoked_grants: Vec<[u8; 16]>,
    /// Every `renew_grant` call ([`RenewedGrant`]), in
    /// order — the rotation-heal driver's output. A test asserts exactly one renew
    /// per outstanding bounded grant, with the equal window end and the
    /// post-rotation cross-generation key wraps. Mirrors [`Self::minted_grants`].
    pub renewed_grants: Vec<RenewedGrant>,
    /// The content-processor holder roster this fake nest serves via
    /// `content_processor_holders` — the rotation-heal driver's discovery seam.
    /// Empty by default (no holders enrolled) ⇒ the driver heals nothing. A test
    /// seeds the holder its seeded bounded grant was minted to.
    pub content_processor_holders: Vec<HolderInfo>,
    /// If set, `renew_grant` returns this error instead of recording the call —
    /// lets a test assert a failed heal-renew never fails the rotation
    /// (best-effort/log-only posture).
    pub fail_renew_grant_with: Option<NestError>,
    /// If set, `mint_grant` returns this error instead of accepting the deposit
    /// — lets a test assert the record-then-deposit order
    /// (`grant_log::UndepositedGrant`): a refused deposit must find the `Mint`
    /// event already durable, never the reverse.
    pub fail_mint_grant_with: Option<NestError>,
    /// If set, `content_processor_holders` returns this error instead of the
    /// roster — lets a test assert an undiscoverable roster skips the heal without
    /// failing the rotation.
    pub fail_content_processor_holders_with: Option<NestError>,
}

impl FakeNestClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, FakeNestState> {
        self.inner.lock().unwrap()
    }
}

#[async_trait]
impl NestClient for FakeNestClient {
    async fn provision_wrapped_mls_blob(&self, blob: WrappedMsekBlob) -> Result<(), NestError> {
        let mut s = self.inner.lock().unwrap();
        if s.fail_provision_wrapped_mls_for_n_calls > 0 {
            s.fail_provision_wrapped_mls_for_n_calls -= 1;
            return Err(NestError::Transient("simulated provision failure".into()));
        }
        s.provision_wrapped_mls.push(blob);
        Ok(())
    }
    async fn provision_mls_snapshot_blob(&self, blob: MlsSnapshotBlob) -> Result<(), NestError> {
        self.inner.lock().unwrap().provision_mls_snapshot.push(blob);
        Ok(())
    }
    async fn provision_wrapped_submission_token(
        &self,
        blob: WrappedSubmissionTokenBlob,
    ) -> Result<(), NestError> {
        self.inner
            .lock()
            .unwrap()
            .provision_submission_token
            .push(blob);
        Ok(())
    }
    async fn revoke_wrapped_mls_blob(
        &self,
        actor_id: [u8; 32],
        credential_id: String,
    ) -> Result<(), NestError> {
        self.inner
            .lock()
            .unwrap()
            .revoke_wrapped_mls
            .push((actor_id, credential_id));
        Ok(())
    }
    async fn revoke_wrapped_submission_token(
        &self,
        actor_id: [u8; 32],
        credential_id: String,
    ) -> Result<(), NestError> {
        self.inner
            .lock()
            .unwrap()
            .revoke_submission_token
            .push((actor_id, credential_id));
        Ok(())
    }
    async fn provision_recipient_mls_pubkey(
        &self,
        actor_id: [u8; 32],
        pubkey: [u8; 32],
        mlkem_ek: Vec<u8>,
        epoch_keys: Option<Vec<EpochSealKey>>,
    ) -> Result<(), NestError> {
        self.inner
            .lock()
            .unwrap()
            .provision_recipient_pubkey
            .push((actor_id, pubkey, mlkem_ek, epoch_keys));
        Ok(())
    }
    async fn nest_supports(&self, token: &str) -> Result<bool, NestError> {
        Ok(fauna_protocol::discovery::capability::supports(
            &self.inner.lock().unwrap().advertised_capabilities,
            token,
        ))
    }
    async fn set_mail_enabled(&self, enabled: bool) -> Result<(), NestError> {
        let mut s = self.inner.lock().unwrap();
        if let Some(err) = s.fail_set_mail_enabled_with.clone() {
            return Err(err);
        }
        s.set_mail_enabled.push(enabled);
        Ok(())
    }
    async fn set_mail_serving_enabled(&self, enabled: bool) -> Result<(), NestError> {
        let mut s = self.inner.lock().unwrap();
        s.set_mail_serving_enabled.push(enabled);
        s.mail_serving_enabled = Some(enabled);
        Ok(())
    }
    async fn get_mail_serving_enabled(&self) -> Result<bool, NestError> {
        // None ⇒ default-on, mirroring the nest's `.unwrap_or(true)`.
        Ok(self
            .inner
            .lock()
            .unwrap()
            .mail_serving_enabled
            .unwrap_or(true))
    }
    async fn get_caldav_port(&self) -> Result<u16, NestError> {
        let s = self.inner.lock().unwrap();
        if let Some(err) = s.fail_get_caldav_port_with.clone() {
            return Err(err);
        }
        // None ⇒ the shared default, mirroring the nest's `.unwrap_or(DEFAULT)`.
        Ok(s.caldav_port
            .unwrap_or(fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT))
    }
    async fn serves_any_webdav_set(&self) -> Result<bool, NestError> {
        let s = self.inner.lock().unwrap();
        if let Some(err) = s.fail_serves_any_webdav_set_with.clone() {
            return Err(err);
        }
        Ok(s.serves_any_webdav_set)
    }
    async fn fetch_spam_model(&self, _actor_id: [u8; 32]) -> Result<FetchedSpamModel, NestError> {
        let s = self.inner.lock().unwrap();
        Ok(FetchedSpamModel {
            blob: s.spam_model.clone(),
            stored_sealed: s.spam_model.is_some() && !s.spam_model_is_cold_start_seed,
            contribute_baseline: s.contribute_baseline,
            holder_seal_target: s.holder_seal_target.clone(),
        })
    }
    async fn put_spam_model(
        &self,
        sealed_model: Vec<u8>,
        sample_count: u32,
        history_op: Option<fauna_protocol::bridge_routing::SpamHistoryOp>,
        holder_copy: Option<SpamModelHolderCopy>,
    ) -> Result<PutSpamModelOutcome, NestError> {
        let mut s = self.inner.lock().unwrap();
        let outcome = s.put_spam_model_outcome;
        if outcome == PutSpamModelOutcome::DuplicateSignal {
            // The nest wrote nothing — not the model, not the row, not the copy.
            return Ok(outcome);
        }
        s.spam_model = Some(sealed_model);
        s.spam_model_is_cold_start_seed = false;
        s.put_spam_model_sample_count = Some(sample_count);
        s.put_spam_model_history_op = Some(history_op);
        s.put_spam_model_holder_copy = Some(holder_copy);
        Ok(outcome)
    }
    async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), NestError> {
        let mut s = self.inner.lock().unwrap();
        if let Some(e) = s.fail_mint_grant_with.clone() {
            return Err(e);
        }
        s.minted_grants.push(grant_blob);
        Ok(())
    }
    async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), NestError> {
        self.inner.lock().unwrap().revoked_grants.push(grant_id);
        Ok(())
    }
    async fn renew_grant(
        &self,
        grant_id: [u8; 16],
        new_epoch_start: u64,
        new_epoch_end: u64,
        appended_keys: Vec<Vec<u8>>,
    ) -> Result<(), NestError> {
        let mut s = self.inner.lock().unwrap();
        if let Some(err) = s.fail_renew_grant_with.clone() {
            return Err(err);
        }
        s.renewed_grants
            .push((grant_id, new_epoch_start, new_epoch_end, appended_keys));
        Ok(())
    }
    async fn content_processor_holders(&self) -> Result<Vec<HolderInfo>, NestError> {
        let s = self.inner.lock().unwrap();
        if let Some(err) = s.fail_content_processor_holders_with.clone() {
            return Err(err);
        }
        Ok(s.content_processor_holders.clone())
    }
    async fn fetch_post_body_text(&self, content_id: &str) -> Result<Option<String>, NestError> {
        let mut s = self.inner.lock().unwrap();
        s.post_body_fetches.push(content_id.to_string());
        match s.post_bodies.get(content_id) {
            Some(body) => Ok(Some(body.clone())),
            None => Err(NestError::Rejected(format!(
                "fauna.posts.not_found: {content_id}"
            ))),
        }
    }
    async fn moderation_train(&self, content_id: &str, verdict: &str) -> Result<(), NestError> {
        let mut s = self.inner.lock().unwrap();
        s.moderation_trains
            .push((content_id.to_string(), verdict.to_string()));
        match s.fail_moderation_train_with.clone() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

/// The grant-event log double — the shared
/// [`FakeSuccessionLedgerStore`], what the machine's ledger half (the grant
/// log, the owner chain) reads and writes. The mail custody is not in it: that
/// is [`FakeMailStore`], the account-store door's semantics over the
/// `fauna.state.mail` rows, whose peer-write hooks
/// (`FakeMailStore::join_peer_state`, `FakeMailStore::after_next_write`) are
/// what the rotation finalize's cross-device revert tests inject through.
pub use fauna_client_config::test_helpers::FakeSuccessionLedgerStore;

pub use fauna_client_config::test_helpers::FakeMailStore;

/// Wraps a deterministic Ed25519 signing key. Tests can assert
/// post-sign that the embedded signature verifies against the
/// matching public key.
#[derive(Clone)]
pub struct FakeSigner {
    key: SigningKey,
}

impl FakeSigner {
    pub fn new(seed: [u8; 32]) -> Self {
        Self {
            key: SigningKey::from_bytes(&seed),
        }
    }

    pub fn verifying_key(&self) -> ed25519_dalek::VerifyingKey {
        self.key.verifying_key()
    }
}

impl IdentitySigner for FakeSigner {
    fn sign_submission_token(
        &self,
        token: SubmissionToken,
    ) -> Result<SubmissionToken, SignerError> {
        token
            .sign(&self.key)
            .map_err(|e| SignerError::Sign(e.to_string()))
    }
    fn sign_grant_event(&self, event: GrantEvent) -> Result<GrantEvent, SignerError> {
        event
            .sign(&self.key)
            .map_err(|e| SignerError::GrantEventSign(e.to_string()))
    }
}

/// A fresh, nothing-filled-in-yet `MailImportSnapshot` — both native apps'
/// mail-import wizard tests built this exact shape by hand (`fauna-linux`'s
/// always wanted `ImportStep::Confirm`; `fauna-tui`'s parameterized the step).
/// Deliberately distinct from `MailImportSnapshot`'s own internal `empty()`
/// constructor: that one seeds `host`/`port` from the Gmail provider preset
/// for production's benefit, where a fresh wizard should already show a
/// working default; a blank `host` here keeps these tests' widget-text
/// assertions simple, matching what both hand-rolled doubles already
/// asserted against.
pub fn a_mail_import_snapshot(step: crate::ImportStep) -> crate::MailImportSnapshot {
    crate::MailImportSnapshot {
        step,
        source_kind: crate::ImportSourceKind::Gmail,
        host: String::new(),
        port: 993,
        tls_mode: crate::ImportTlsMode::Implicit,
        username: String::new(),
        password: fauna_core::secret::SecretString::default(),
        mailboxes: Vec::new(),
        date_from: String::new(),
        max_size_bytes: 50 * 1024 * 1024,
        session_state: None,
        imported_count: 0,
        skipped_count: 0,
        errored_count: 0,
        total_count: 0,
        error_log: Vec::new(),
        status: crate::ImportStatus::Idle,
        error: None,
    }
}
