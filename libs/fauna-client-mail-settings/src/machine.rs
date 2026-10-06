//! State-machine driver. Owns the typed snapshot the per-app UI
//! renders, the action dispatch, and the wiring to the four
//! injected seams.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_protocol::MaybeSendSync;
use fauna_protocol::discovery::capability::SPAM_MODEL_SEALED_AT_REST;
use rand::RngCore;
use serde_bytes::ByteBuf;
use zeroize::Zeroizing;

use fauna_client_bridges::HolderInfo;
use fauna_client_capabilities::{
    DEFAULT_GRANT_WINDOW_SECS, bounded_mail_rotation_heal_keys, grant_log, mint_grant,
};
use fauna_client_config::SuccessionLedgerStore;
use fauna_core::data::{MailConfig, MailCredential, MsekFingerprint, Timestamp};
use fauna_core::grant_event::{GrantEvent, GrantEventScope};
use fauna_core::identity::ActorId;
use fauna_core::mail_rows::{MailRows, MailStateRow};
use fauna_core::secret::{SecretArray32, SecretString};
use fauna_core::succession_ledger::SuccessionLedger;
use fauna_mail::spam::model_write::{
    ModelWriteOp, SealedWriteDispatch, apply_and_reseal_hybrid, dispatch_for, encode_history_delta,
    seal_history_blob_hybrid, unwrap_sealed_history_bytes, unwrap_sealed_history_delta,
};
use fauna_mail::spam::{SpamLabel, SpamModel};
use fauna_mls::wrapped_blob::{
    GrantWindow, MAIL_EPOCH_PUBLISH_HORIZON, MLKEM768_DECAPS_KEY_LEN, MlsSnapshotBlob,
    SNAPSHOT_GRACE_KEYPAIRS, ScopeTuple, StandingMailKeypair, SubmissionToken, WrappedMsekBlob,
    WrappedSubmissionTokenBlob, XWingPublicKey, build_mls_snapshot_plaintext,
    derive_mail_epoch_root, derive_recipient_epoch_hpke_keypair,
    derive_recipient_epoch_xwing_keypair, derive_recipient_hpke_keypair,
    derive_recipient_xwing_keypair, derive_standing_mail_keypairs, mail_sealing_epoch_of,
    seal_spam_model_copy,
};
use fauna_protocol::bridge_routing::{
    EpochSealKey, SpamHistoryOp, SpamLabel as WireSpamLabel, TrainingSource as WireTrainingSource,
};
use fauna_protocol::wrapped_blob::{HolderSealTarget, PutSpamModelOutcome, SpamModelHolderCopy};

use crate::credential::{Credential, derive_credential_id};
use crate::error::{DispatchError, NestError, SignerError};
use crate::rotation;
use crate::state::{
    MailCredentialSummary, MailSettingsAction, MailSettingsSnapshot, MuaInstructions,
    PendingRotationStatus, SettingsStatus,
};
use crate::wrap;

/// Default `SubmissionToken.expires_at` window. Spec § Submission
/// token: 30 days. The background refresher (token_refresh.rs)
/// re-mints every 7 days.
pub const SUBMISSION_TOKEN_LIFETIME_SECS: u64 = 30 * 86_400;
/// Default per-token send policy. Matches the conformance test in
/// `fauna-mls::wrapped_blob::seal_tests::fresh_signed_token`. Per-
/// credential overrides live on the policy surface (out of scope
/// for Phase A).
pub const DEFAULT_MAX_RECIPIENTS: u32 = 100;
pub const DEFAULT_MAX_MESSAGES_PER_DAY: u32 = 1000;

/// The seam-level projection of a `fauna.bridges.fetch_spam_model` reply the
/// [`NestClient::fetch_spam_model`] read returns — the sealed model blob plus the
/// two additive deployment-baseline **write-signal** fields the nest volunteers
/// (`mail-spam.md` § Wire shapes, piece (b)). Widened from the bare
/// `Option<Vec<u8>>` so [`MailSettingsMachine::apply_spam_model_write`] can attach
/// a holder copy in the same round trip (no second fetch): a reply with
/// both write-signal fields at their defaults (no holder enrolled) degrades to the
/// bit-only, no-copy path.
#[derive(Debug, Clone, Default)]
pub struct FetchedSpamModel {
    /// The model as the nest returns it — a bare inner `wrapped_blob` sealed to
    /// the actor. `None` ⇒ the actor is untrained and no deployment baseline is
    /// published. With [`Self::stored_sealed`] `false` a present blob is the
    /// nest's read-time **cold-start seed** (the published baseline sealed on
    /// read onto a fresh model), never the user's own model.
    pub blob: Option<Vec<u8>>,
    /// `true` iff [`Self::blob`] is the actor's **stored** model (always sealed
    /// at rest). `false` with a blob ⇒ the blob is the cold-start seed, which a
    /// write must NOT train on or persist (the fold is read-time only —
    /// `mail-spam.md` § Cold start Path 2): every write position starts from a
    /// fresh model instead.
    pub stored_sealed: bool,
    /// `true` iff this actor is opted into the deployment baseline — the
    /// re-seal-on-every-write copy-attach signal (only meaningful with a
    /// [`Self::holder_seal_target`]).
    pub contribute_baseline: bool,
    /// The box's aggregation-holder seal target, when a holder is enrolled — the
    /// nest volunteers its own content-processor holder so a non-admin contributor
    /// seals the copy without enumerating the service-user roster. `None` ⇒ no
    /// holder ⇒ attach no copy.
    pub holder_seal_target: Option<HolderSealTarget>,
}

/// The actor's own **stored** sealed model out of a fetch, or `None` — the one
/// place every write position reads the "train on what?" answer. A missing
/// model and the nest's read-time cold-start seed (`stored_sealed == false`)
/// both read as `None` ⇒ start from a fresh model: the seed is the deployment
/// baseline folded on read, never the user's training, so it must not be
/// trained on, persisted, or contributed (`mail-spam.md` § Cold start Path 2).
fn stored_model(fetched: &FetchedSpamModel) -> Option<&[u8]> {
    if fetched.stored_sealed {
        fetched.blob.as_deref()
    } else {
        None
    }
}

/// WS-RPC seam to nest. The shared `rpc_glue` seam impl (the one every
/// native app + the web SPA share) implements this over the WS-RPC client
/// surface and the typed request/reply pairs in `libs/fauna-protocol::wrapped_blob`.
/// The owner's WebDAV served state as their folder-key custody calls it
/// (ruling (7)(b)(ii) rule (2), `writer-signed-change-records.md`) — what
/// [`NestClient::serves_any_webdav_set`] folds the owner-scoped
/// `fauna.folders.list` rows through instead of their `webdav_enabled` flag.
/// The custody fold lives with the custody
/// (`fauna_client_folders::CustodyServedSets`, which this
/// crate cannot depend on: that crate sits above it), so this seam is the
/// whole of it here.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait WebdavServedSets: MaybeSendSync {
    /// Whether custody serves ≥1 of the owner's `rows`. An unreadable
    /// custody answers `false`.
    async fn serves_any(&self, rows: Vec<fauna_protocol::folders::FolderSummary>) -> bool;
}

// Dual `async_trait` arm + `MaybeSendSync` supertrait so the one seam serves
// native + wasm (see `fauna_protocol::MaybeSendSync`); the wasm `rpc_glue` arm
// wraps the `Rc`-based, `!Send` `WsRpcClient`. Mirrors the admin `…Nest` traits.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait NestClient: MaybeSendSync {
    async fn provision_wrapped_mls_blob(&self, blob: WrappedMsekBlob) -> Result<(), NestError>;
    async fn provision_mls_snapshot_blob(&self, blob: MlsSnapshotBlob) -> Result<(), NestError>;
    async fn provision_wrapped_submission_token(
        &self,
        blob: WrappedSubmissionTokenBlob,
    ) -> Result<(), NestError>;
    async fn revoke_wrapped_mls_blob(
        &self,
        actor_id: [u8; 32],
        credential_id: String,
    ) -> Result<(), NestError>;
    async fn revoke_wrapped_submission_token(
        &self,
        actor_id: [u8; 32],
        credential_id: String,
    ) -> Result<(), NestError>;
    /// Register the actor's standing recipient-mail X25519 pubkey
    /// (`fauna.bridges.provision_recipient_mls_pubkey`). The MTA seals
    /// inbound mail to it; the user self-registers their own (the
    /// handler enforces `target == caller`). Idempotent atomic replace.
    ///
    /// `mlkem_ek` is the recipient's post-quantum **ML-KEM-768 encapsulation
    /// key** (1184 B, MSEK-derived — `derive_recipient_xwing_keypair(msek)
    /// .public.mlkem_encaps_key()`). Required, as the wire field is — no
    /// capability token gates the publication
    /// (`architecture/security/post-quantum.md` § Capability negotiation, the
    /// 2026-09-24 ruling).
    ///
    /// `epoch_keys` is the content-sealing-epoch publication horizon
    /// (design 2026-07-18 § 3), likewise always sent by the machine
    /// (`architecture/encryption-at-rest.md` § Content-sealing epochs).
    async fn provision_recipient_mls_pubkey(
        &self,
        actor_id: [u8; 32],
        pubkey: [u8; 32],
        mlkem_ek: Vec<u8>,
        epoch_keys: Option<Vec<EpochSealKey>>,
    ) -> Result<(), NestError>;

    /// Whether the home nest advertises capability `token` in its
    /// `fauna.nest.info` reply (`fauna_protocol::discovery::capability`).
    /// The degrade contract is the capability module's: a nest that does not
    /// advertise the token — or omits the whole `capabilities` field — reads as
    /// `false`.
    async fn nest_supports(&self, token: &str) -> Result<bool, NestError>;

    /// Flip the deployment-wide mail subsystem on/off
    /// (`fauna.bridges.set_mail_enabled`). This is Admin-class on the nest, so a
    /// non-admin actor's call is rejected server-side — which is exactly how
    /// [`MailSettingsMachine::enable_mail`] scopes design A: it calls this
    /// best-effort and swallows the `Rejected` (the non-admin no-op).
    async fn set_mail_enabled(&self, enabled: bool) -> Result<(), NestError>;

    /// Flip whether this nest serves the *calling* actor's mailbox over
    /// IMAP/CalDAV to external MUAs (`fauna.bridges.set_mail_serving_enabled`).
    /// User-class and **caller-scoped** on the nest: the request carries only
    /// `enabled` (the handler keys on the authenticated actor), so there is no
    /// "set another user's flag" path — the admin's audit view is read-only by
    /// design. Default on / absent ⇒ on. See
    /// `docs/goal/architecture/nest/deployment-home-with-public-relay.md`
    /// § MUA reach.
    async fn set_mail_serving_enabled(&self, enabled: bool) -> Result<(), NestError>;

    /// Read the calling actor's own IMAP/CalDAV-serving flag
    /// (`fauna.bridges.get_mail_serving_enabled`). Caller-scoped: the read is
    /// for the authenticated actor (the request's `actor_id` is sent empty —
    /// the nest forces a `User` caller to its own id regardless). A never-set
    /// actor reads back `true` (default-on). The Admin-scoped audit read of
    /// *another* actor's flag is a separate (admin) surface, not this trait.
    async fn get_mail_serving_enabled(&self) -> Result<bool, NestError>;

    /// Read the deployment's effective CalDAV listener port
    /// (`fauna.bridges.get_caldav_port`). `User | Admin` and **not**
    /// caller-scoped — the port is a nest-wide singleton (the admin's
    /// `set_caldav_port` choice, default `DEFAULT_CALDAV_PORT` = 8443). The
    /// mail-settings page reads it to display the CalDAV connection detail for a
    /// third-party calendar app on a **local-target** box, where the MDA binds it
    /// directly (`caldav-server.md` § Network exposure — Desktop / IP). The read
    /// is best-effort: the caller treats a failure (a transport error) as "keep
    /// the for-node-URL default" rather than failing the whole hydrate.
    async fn get_caldav_port(&self) -> Result<u16, NestError>;

    /// Whether this actor serves **≥1 folder over WebDAV** — the owner-scoped
    /// `fauna.folders.list` rows folded through the owner's folder-key
    /// custody: an owned row custody calls served (ruling (7)(b)(ii) rule (2),
    /// `writer-signed-change-records.md`), never the rows' `webdav_enabled`
    /// flag, which a nest could set on a set the owner never served.
    ///
    /// This is the **per-actor** WebDAV signal (`webdav-server.md` §
    /// Implementation status), *not* the deployment-wide `webdav_enabled`
    /// toggle — which defaults ON for a real-domain box and is `Admin`-only to
    /// read. See [`MailSettingsSnapshot::serves_webdav_set`].
    ///
    /// Best-effort: the caller
    /// treats a hard failure (a transport error) as "not serving" rather than failing the whole
    /// hydrate — the same best-effort contract as [`Self::get_caldav_port`].
    async fn serves_any_webdav_set(&self) -> Result<bool, NestError>;

    /// Fetch the caller's own per-user spam model (`fauna.bridges.fetch_spam_model`)
    /// as the nest returns it — a bare inner `wrapped_blob` sealed to the actor
    /// (`mail-spam.md` § Encrypted-mode interaction). `None` when the actor is
    /// untrained (cold start). The read half of the client model-write loop
    /// ([`MailSettingsMachine::apply_spam_model_write`]): the machine unwraps this
    /// under the MSEK-derived recipient secret, applies the op, and re-seals.
    /// Caller-scoped by the nest (`target == caller`), so `actor_id` is the
    /// caller's own.
    ///
    /// Returns the sealed blob **plus** the two additive deployment-baseline
    /// write-signal fields ([`FetchedSpamModel`]): `contribute_baseline` and the
    /// nest-volunteered `holder_seal_target`, so the write orchestrator can attach
    /// the holder copy in the same round trip (`mail-spam.md` § Wire shapes,
    /// piece (b)).
    async fn fetch_spam_model(&self, actor_id: [u8; 32]) -> Result<FetchedSpamModel, NestError>;

    /// Write the whole re-sealed spam model back opaque
    /// (`fauna.bridges.put_spam_model`) — the write twin of
    /// [`Self::fetch_spam_model`]. `sealed_model` is the client-sealed
    /// `wrapped_blob` bytes ([`fauna_mail::spam::model_write::ReSealedModel::sealed_model`]);
    /// the nest stores it verbatim, never decoding it. `sample_count` is advisory
    /// display metadata only (never trusted for security — the blob is opaque).
    /// Caller-scoped by construction (no `actor_id` field — the connection's actor
    /// is the subject).
    ///
    /// `history_op` rides the same kind so the model re-seal and its training-history
    /// mutation commit **atomically** (build-item 3 write side): a client-path train
    /// carries [`SpamHistoryOp::Insert`] (its client-sealed subject + delta), a
    /// client-side undo carries [`SpamHistoryOp::Delete`], `None` is a model-only
    /// write (a moderation-queue social train, or the initial holder-copy
    /// re-seal).
    ///
    /// `holder_copy` rides the same kind, atomically with the model re-seal: when
    /// the writing actor is opted into the deployment baseline and the nest
    /// volunteered a `holder_seal_target`, the orchestrator seals a copy of the
    /// post-mutation model to that holder and passes it here so the nest replaces
    /// the stored `(actor, holder)` copy in the same transaction (the
    /// re-seal-on-every-write rule, `mail-spam.md` § Encrypted-mode interaction).
    /// `None` ⇒ the actor is opted out or no holder is enrolled — the stored copy
    /// (if any) is left untouched.
    ///
    /// Returns the nest's [`PutSpamModelOutcome`]: `Written`, or
    /// `DuplicateSignal` when the one-lesson rule (`mail-spam.md` § 3) rejected
    /// an `Insert` that repeats the actor's newest recorded lesson — then nothing
    /// was written and the stored model is unchanged.
    async fn put_spam_model(
        &self,
        sealed_model: Vec<u8>,
        sample_count: u32,
        history_op: Option<SpamHistoryOp>,
        holder_copy: Option<SpamModelHolderCopy>,
    ) -> Result<PutSpamModelOutcome, NestError>;

    /// `fauna.capabilities.mint` — deposit the client-built, HPKE-sealed
    /// [`fauna_client_capabilities::mint_grant`] `GrantBlob` (canonical bytes).
    /// The nest stores it opaque, keyed `(owner, grant_id)`, and never opens it.
    /// The `mail-spam` contribute toggle mints a keyless `content.read{spam-model}`
    /// grant to the box's aggregation holder (`mail-spam.md` § Encrypted-mode
    /// interaction, piece (b)); the copy travels separately via `put_spam_model`.
    async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), NestError>;

    /// `fauna.capabilities.revoke` — delete the `(owner, grant_id)` row; the
    /// holder's next fetch returns nothing and the grant goes dark (revocation is
    /// terminal — the nest also deletes the paired spam-model holder copy). The
    /// contribute toggle-OFF path revokes the standing baseline-contribution grant.
    async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), NestError>;

    /// `fauna.capabilities.renew` — move an existing grant's window to
    /// `[new_epoch_start, new_epoch_end]` and/or append fresh per-`(scope,
    /// epoch)` key wraps to it, keyed `(owner, grant_id)`. The nest stores each
    /// appended wrap opaque, replacing an existing same-`(scope, epoch)` wrap
    /// whose bytes differ (the B3c cross-root replace-heal), and prunes the
    /// per-epoch wraps below the window start (the retention ruling). The end
    /// never shrinks and the start never moves back (the nest refuses both —
    /// narrowing is revocation's job).
    ///
    /// The rotation-heal driver ([`MailSettingsMachine::heal_outstanding_bounded_grants`])
    /// calls this once per outstanding bounded mail grant after an MSEK rotation,
    /// with the grant's **recorded** window unchanged (a pure key refresh) and
    /// `appended_keys` re-wrapping that whole window under the post-rotation
    /// generation set, so the holder heals across the hard-revoked lineage
    /// without the owner re-minting — the retained epoch set is the log's
    /// window at both drivers. `appended_keys` are canonical
    /// [`fauna_mls::wrapped_blob::WrappedScopeKey`] bytes.
    async fn renew_grant(
        &self,
        grant_id: [u8; 16],
        new_epoch_start: u64,
        new_epoch_end: u64,
        appended_keys: Vec<Vec<u8>>,
    ) -> Result<(), NestError>;

    /// Enumerate the home nest's approved **content-processor holders** — the
    /// grant seal targets (`fauna.bridges.list_service_users` filtered to
    /// content-reading roles, then each one's `fetch_bridge_pubkey`). The
    /// rotation-heal driver matches an outstanding bounded grant's log-recorded
    /// holder pubkey against this **live roster** to recover the holder's current
    /// X25519 (+ optional ML-KEM) seal target — never storing that key material in
    /// the grant log (`ui/nests.md:163`: holder metadata is recomputed from the
    /// roster, so a lying nest merely fails to decrypt). A holder absent from the
    /// roster is skipped by the driver (no classical-blind re-wrap).
    async fn content_processor_holders(&self) -> Result<Vec<HolderInfo>, NestError>;

    /// `fauna.posts.get` → the post's trainable text (`Post::body_text`, the
    /// same extraction every spam train uses) — the body read
    /// [`MailSettingsMachine::train_moderation_correction`]'s model half trains
    /// on. Rides the nest's quarantine-aware read gate, so a post the caller may
    /// not read is an `Err`. `Ok(None)` ⇒ the reply carried no decodable post.
    async fn fetch_post_body_text(&self, content_id: &str) -> Result<Option<String>, NestError>;

    /// `fauna.moderation.train{content_id, verdict}` — the **nest half** of a
    /// moderation-queue training correction: the read gate (a taken-down /
    /// quarantined / absent post is `not_found`) and the report capture
    /// (`report-sharing.md` § Report capture). It trains nothing — the model
    /// rests sealed, so the model half is the client's sealed write.
    /// `verdict` is the wire string (`"spam"` / `"ham"`).
    async fn moderation_train(&self, content_id: &str, verdict: &str) -> Result<(), NestError>;
}

/// The mail custody seam — the **shared** `fauna_client_config::MailStore`
/// (`fauna.state.mail`), re-exported here so every `crate::machine::MailStore`
/// reference resolves.
pub use fauna_client_config::MailStore;

/// User's Ed25519 signing-key seam. The shared `rpc_glue` seam impl holds the
/// actual `SigningKey`; the state machine asks it to sign a fully-
/// populated (but `user_sig`-placeholder-filled) `SubmissionToken`.
// `MaybeSendSync` (not `Send + Sync`) so the wasm seam impl can hold a
// `!Send` transport, like the async seams above. Synchronous, so no
// `async_trait` arm.
pub trait IdentitySigner: MaybeSendSync {
    fn sign_submission_token(&self, token: SubmissionToken)
    -> Result<SubmissionToken, SignerError>;
    /// Sign a capability grant-event (`Mint`/`Revoke`) with the user's identity
    /// key — the same seam-path split as [`Self::sign_submission_token`] (the raw
    /// `SigningKey` never crosses the FFI/wasm boundary; the `rpc_glue` impl holds
    /// it). Consumed by the `mail-spam` contribute-toggle's mint/revoke path
    /// (`grant_log::build_{mint,revoke}_event` → this → `grant_log::append_signed`).
    fn sign_grant_event(&self, event: GrantEvent) -> Result<GrantEvent, SignerError>;
}

/// The MSEK-derived recipient key material [`MailSettingsMachine::apply_spam_model_write`]
/// re-seals under. Crate-internal — never crosses the UniFFI/wasm/UI boundary
/// (only the resulting opaque sealed blob does).
struct ReSealKeyMaterial {
    /// X25519 recipient secret — unwraps the fetched sealed model.
    x25519_secret: [u8; 32],
    /// ML-KEM-768 decapsulation key — unwraps a hybrid (X-Wing) blob.
    mlkem_dk: Vec<u8>,
    /// The actor's own X-Wing public key — the hybrid reseal target.
    xwing_pubkey: XWingPublicKey,
}

/// The result of a client model-write ([`MailSettingsMachine::apply_spam_model_write`]).
/// The surface that dispatched it (the moderation-queue `train-correction-button`,
/// the `mail-spam` settings undo) branches on this: `Sealed` means the
/// model was re-sealed + written back client-side; `ServerPath` means no sealed
/// write was possible and the model half was skipped (there is no server-side
/// model write to fall back to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpamModelWriteOutcome {
    /// The model was mutated + re-sealed client-side and written back opaque via
    /// `put_spam_model`. `delta` is the forward n-gram set of a `Train` (empty for
    /// `Undo`/`ModelSync`) — the caller persists it as a new training-history row
    /// (leg 1c, once the sealed history-row write lands). `sample_count` is the
    /// post-mutation training-document count for the settings display.
    Sealed {
        sample_count: u32,
        delta: BTreeSet<String>,
    },
    /// No sealed write is possible: the nest hasn't advertised the sealed-at-rest
    /// handling ([`SPAM_MODEL_SEALED_AT_REST`] — cannot happen on a current
    /// nest), **or** the actor has no MSEK (mail not enabled, so there is no
    /// recipient key to seal a model to and no per-user model at all). Nothing
    /// was written; the caller skips the model half and must NOT fall back to a
    /// server-side train (none exists — the model rests sealed).
    ServerPath,
    /// The nest rejected the whole sealed write under the one-lesson rule
    /// (`mail-spam.md` § 3): the actor's newest recorded lesson for that message
    /// already carries this label, so nothing was written and the stored model is
    /// unchanged — the sealed path was taken (no server-path fallback), it just
    /// had nothing new to teach. `sample_count` is the count still on record
    /// (pre-mutation). Only a `Train` with a history `Insert` can meet this.
    Duplicate { sample_count: u32 },
}

/// The optional training-history mutation a client-path model write commits
/// **atomically** with the model re-seal via [`MailSettingsMachine::apply_spam_model_write`]
/// (build-item 3 write side, option (a); `mail-spam.md` § Wire shapes
/// `put_spam_model` `history_op`).
///
/// - [`Insert`](Self::Insert) — a client-path **train**. The machine seals
///   `subject` + the event's forward n-gram delta to the actor's **own** recipient
///   key (the same classical/hybrid suite as the model) and ships them as
///   [`SpamHistoryOp::Insert`], so the audit row is nest-opaque like the model.
/// - [`Delete`](Self::Delete) — a client-side **undo** of a sealed row. The inverse
///   delta is already applied by the [`ModelWriteOp::Undo`] on the same write, so
///   this carries only the `history_id` to remove the consumed row atomically.
///
/// A moderation-queue *social* train passes `None` (no history variant) — the
/// training-history list is the mail surface's audit (`{subject} · {mailbox}`);
/// the moderation correction's own record is the nest-side report capture of
/// `fauna.moderation.train`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryWrite {
    /// A client-path train: insert one sealed audit row.
    Insert {
        /// The trained message's stable 32-byte content/placement id (stored
        /// opaque for the row's reference).
        message_id: Vec<u8>,
        /// The mailbox the message was in at train time (`INBOX` / `Junk` / …) —
        /// plaintext metadata, returned separately so a client renders
        /// `{unwrapped subject} · {mailbox}`.
        mailbox: String,
        /// The trained message's subject, **sealed to the actor's own key** by the
        /// machine (the caller passes plaintext; it never leaves as plaintext).
        subject: String,
        /// Where the training signal came from.
        source: WireTrainingSource,
    },
    /// A client-side undo: delete one of the caller's own history rows by id.
    Delete {
        /// The 16-byte history-event id from a prior `list_spam_training_history`.
        history_id: Vec<u8>,
    },
}

/// The sealed spam-model write capability the `mail-spam` page's undo needs but
/// that lives on [`MailSettingsMachine`] (which holds the MSEK-derived reseal key
/// and the nest capability signals). A **narrow** seam so
/// [`crate::spam::MailSpamMachine`] can drive a client-side sealed-row undo —
/// unwrap the row's sealed delta → [`ModelWriteOp::Undo`] → atomic model-write +
/// `history_op: Delete` — without depending on the whole settings object, and so
/// the undo is unit-testable with a spy writer. [`MailSettingsMachine`] is the
/// production impl; the per-app glue injects it (built from the same actor
/// keypair, so it derives the same MSEK). The method names are deliberately
/// distinct from [`MailSettingsMachine`]'s inherent ones so the forwarding impl
/// isn't ambiguous.
// Dual `async_trait` arm + `MaybeSendSync` supertrait (native + wasm), like
// `NestClient`.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait SealedModelWriter: MaybeSendSync {
    /// Unwrap a client-written row's sealed `model_delta_applied` to its forward
    /// n-gram set — `None` ⇒ mail not enabled (no key ⇒ no client-side undo).
    /// Forwards to [`MailSettingsMachine::unwrap_sealed_history_delta`].
    async fn unwrap_history_delta(
        &self,
        sealed_delta: &[u8],
    ) -> Result<Option<BTreeSet<String>>, DispatchError>;
    /// Unwrap a client-written row's sealed **subject** to its plaintext string,
    /// for the training-history display (`{subject} · {mailbox}`). `None` ⇒ mail
    /// not enabled. Forwards to [`MailSettingsMachine::unwrap_sealed_history_subject`].
    async fn unwrap_history_subject(
        &self,
        sealed_subject: &[u8],
    ) -> Result<Option<String>, DispatchError>;
    /// Apply a model write (+ optional atomic history mutation) — the reseal loop.
    /// Forwards to [`MailSettingsMachine::apply_spam_model_write`].
    async fn write_spam_model(
        &self,
        op: ModelWriteOp,
        history: Option<HistoryWrite>,
    ) -> Result<SpamModelWriteOutcome, DispatchError>;

    /// Deployment-baseline **opt-in** write path (`mail-spam` contribute toggle ON,
    /// `mail-spam.md` § Encrypted-mode interaction): after the nest bit is set (so
    /// b1-nest volunteers the box's aggregation holder in `fetch_spam_model`), seal
    /// the initial holder copy of the current model and mint the keyless
    /// `content.read{spam-model}` grant. Holder-absent ⇒ bit-only (nothing to seal
    /// or grant to). The [`MailSettingsMachine`] impl holds the actor keypair
    /// (copy seal), the `SuccessionLedgerStore` (grant log), and the mint/revoke transport.
    async fn attach_and_mint_baseline_grant(&self) -> Result<(), DispatchError>;

    /// Deployment-baseline **opt-out** write path (contribute toggle OFF): revoke
    /// the standing `content.read{spam-model}` grant (the nest then deletes the
    /// stored copy) and append the signed `Revoke` to the grant log. Idempotent —
    /// a no-op when no baseline grant is currently open.
    async fn revoke_baseline_grant(&self) -> Result<(), DispatchError>;
}

/// The mailbox-export wizard's key custody (`mail-export.md` § Key material),
/// composed into `build_mail_export_machine` the way `build_mail_spam_machine`
/// composes the [`SealedModelWriter`]: this machine already holds the
/// mail custody the MSEK lives in and the actor id the session key is
/// bound to, so the MSEK never leaves shared Rust and no app glue touches it.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl crate::export::MailExportKeyCustody for MailSettingsMachine {
    async fn mint_export_session_key(
        &self,
    ) -> Result<crate::export::MintedExportSessionKey, DispatchError> {
        let mail = self.mail.load().await?;
        let msek = mail.msek.as_ref().ok_or_else(export_needs_mail)?;
        crate::export::mint_export_session_key_for(msek, &self.actor_id)
    }

    async fn export_record_opener(
        &self,
    ) -> Result<Arc<dyn crate::export::MailRecordOpening>, DispatchError> {
        let mail = self.mail.load().await?;
        let msek = mail.msek.as_ref().ok_or_else(export_needs_mail)?;
        // Current generation first, then the grace generations a rotation
        // retained — the exact history `standing_mail_keypairs` reads, so the
        // export can open whatever the user's inbox can.
        let history: Vec<[u8; 32]> = std::iter::once(**msek)
            .chain(
                mail.prior_mseks
                    .iter()
                    .take(SNAPSHOT_GRACE_KEYPAIRS - 1)
                    .map(|prior| **prior),
            )
            .collect();
        Ok(Arc::new(
            crate::export::StandingKeyRecordOpener::from_msek_history(&history),
        ))
    }

    async fn unwrap_export_session_key(
        &self,
        wrapped: &[u8],
    ) -> Result<zeroize::Zeroizing<[u8; 32]>, DispatchError> {
        let mail = self.mail.load().await?;
        let msek = mail.msek.as_ref().ok_or_else(export_needs_mail)?;
        // The COMPLETE standing set, exactly as `export_record_opener` builds
        // it: an export sealed under generation n-1 must still open from a
        // client that has since rotated to n (`unseal_export_session_key`'s own
        // doc). A current-generation-only unwrap would hand such a client a row
        // it can see, a blob it can download, and no way to read it.
        let history: Vec<[u8; 32]> = std::iter::once(**msek)
            .chain(
                mail.prior_mseks
                    .iter()
                    .take(SNAPSHOT_GRACE_KEYPAIRS - 1)
                    .map(|prior| **prior),
            )
            .collect();
        let standing = fauna_mls::wrapped_blob::derive_standing_mail_keypairs(&history);
        let blob = fauna_mls::wrapped_blob::ExportSessionKeyBlob::from_canonical_bytes(wrapped)
            .map_err(|e| {
                DispatchError::InvalidState(format!("export session key blob is unreadable: {e}"))
            })?;
        let key = fauna_mls::wrapped_blob::unseal_export_session_key(&blob, &standing)
            .map_err(|e| DispatchError::InvalidState(format!("unwrap export session key: {e}")))?;
        Ok(zeroize::Zeroizing::new(key))
    }
}

/// No MSEK means no mailbox: nothing to wrap a session key to and nothing to
/// open records with. An export of mail that does not exist is refused, not
/// degraded.
fn export_needs_mail() -> DispatchError {
    DispatchError::InvalidState(
        "mail is not enabled for this account, so there is no mail to export".into(),
    )
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl SealedModelWriter for MailSettingsMachine {
    async fn unwrap_history_delta(
        &self,
        sealed_delta: &[u8],
    ) -> Result<Option<BTreeSet<String>>, DispatchError> {
        self.unwrap_sealed_history_delta(sealed_delta).await
    }
    async fn unwrap_history_subject(
        &self,
        sealed_subject: &[u8],
    ) -> Result<Option<String>, DispatchError> {
        self.unwrap_sealed_history_subject(sealed_subject).await
    }
    async fn write_spam_model(
        &self,
        op: ModelWriteOp,
        history: Option<HistoryWrite>,
    ) -> Result<SpamModelWriteOutcome, DispatchError> {
        self.apply_spam_model_write(op, history).await
    }
    async fn attach_and_mint_baseline_grant(&self) -> Result<(), DispatchError> {
        // b1-nest volunteers `holder_seal_target` ONLY once the opt-in bit is set,
        // so the seam calls this AFTER `set_baseline_contribution(true)` — one fetch
        // learns the holder + the current model.
        let fetched = self.nest.fetch_spam_model(self.actor_id).await?;
        let Some(holder) = fetched.holder_seal_target.clone() else {
            // No holder enrolled: bit-only,
            // nothing to seal a copy to or mint a grant for.
            return Ok(());
        };
        // (1) initial copy of the current model (skipped when untrained / mail-off /
        // the cold-start baseline, nothing stored — the copy then rides the first train's write).
        // Only the actor's STORED model is contributed — never the cold-start seed
        // (the deployment baseline folded on read), which is not user training.
        self.attach_initial_holder_copy(&holder, stored_model(&fetched))
            .await?;
        // (2) mint + log the keyless `content.read{spam-model}` grant to that holder.
        self.mint_baseline_grant(&holder).await
    }
    async fn revoke_baseline_grant(&self) -> Result<(), DispatchError> {
        let ledger = self.ledger.load().await?;
        // The one currently-open baseline grant (content.read{spam-model}); none ⇒
        // idempotent no-op (an opt-in with no holder enrolled (bit-only), or an already-off row).
        let Some(grant) = grant_log::current_grants(&ledger)
            .into_iter()
            .find(|g| is_spam_model_grant(&g.scope))
        else {
            return Ok(());
        };
        let grant_id: [u8; 16] = grant.grant_id.as_slice().try_into().map_err(|_| {
            DispatchError::InvalidState("baseline grant_id must be 16 bytes".into())
        })?;
        let holder: [u8; 32] = grant.holder.as_slice().try_into().map_err(|_| {
            DispatchError::InvalidState("baseline grant holder must be 32 bytes".into())
        })?;
        // Nest deletion first, then the log records the revoke (mirrors
        // `LinkedNestsMachine::revoke_grant_action`); a crash between leaves the
        // grant dark nest-side and re-derivable from a fresh toggle.
        self.nest.revoke_grant(grant_id).await?;
        let now = Timestamp::now_secs() as u64;
        let unsigned = grant_log::build_revoke_event(grant_id, holder, now);
        let signed = self.signer.sign_grant_event(unsigned)?;
        self.ledger
            .merge(SuccessionLedger::events_replica(
                ActorId(self.actor_id),
                vec![signed],
            ))
            .await?;
        Ok(())
    }
}

/// A folded grant's scope names the deployment-baseline contribution
/// (`content.read{spam-model}`) — the keyless kind the contribute toggle mints
/// (`mail-spam.md` § Encrypted-mode interaction). Used to find the one open grant
/// to revoke on opt-out.
fn is_spam_model_grant(scope: &[GrantEventScope]) -> bool {
    scope.iter().any(|s| {
        s.class == ScopeTuple::CLASS_CONTENT_READ
            && s.kind.as_deref() == Some(ScopeTuple::KIND_SPAM_MODEL)
    })
}

/// FFI-safe scalar result of the composed
/// [`MailSettingsMachine::train_spam_model_client`] façade —
/// [`SpamModelWriteOutcome`] flattened for the UniFFI/wasm boundary (the
/// `BTreeSet` forward delta doesn't cross it; once the sealed history-row write
/// lands the machine persists the delta itself via `put_spam_model`'s
/// `history_op`, so no consumer ever needs it FFI-side).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SpamModelClientWrite {
    /// True ⇒ the sealed client-side path handled the write — the model was
    /// mutated, re-sealed and written back ([`SpamModelWriteOutcome::Sealed`]),
    /// or the nest kept it unchanged under the one-lesson rule
    /// ([`SpamModelWriteOutcome::Duplicate`]); false ⇒
    /// [`SpamModelWriteOutcome::ServerPath`] — no sealed write was possible
    /// (no MSEK, or a nest without the sealed-at-rest token) and the model half
    /// was skipped; there is no server-side model write to fall back to.
    pub sealed: bool,
    /// The training-document count on record after the call, for the settings
    /// display (`0` when `sealed == false`).
    pub sample_count: u32,
}

struct Inner {
    snapshot: MailSettingsSnapshot,
}

/// The state machine. Single instance per actor on each Fauna
/// app.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MailSettingsMachine {
    actor_id: [u8; 32],
    nest: Arc<dyn NestClient>,
    /// The grant-event log (`fauna.state.succession-ledger`) the baseline
    /// grant and the rotation heal record their signed events on — the
    /// host's account-store handle.
    ledger: Arc<dyn SuccessionLedgerStore>,
    /// The account's mail custody (`fauna.state.mail`) — the MSEK, its grace
    /// window, the credentials, the flags. Plane-only: nothing else
    /// backs this machine.
    mail: Arc<dyn MailStore>,
    signer: Arc<dyn IdentitySigner>,
    mua: MuaInstructions,
    inner: Mutex<Inner>,
}

impl MailSettingsMachine {
    pub fn new(
        actor_id: [u8; 32],
        nest: Arc<dyn NestClient>,
        ledger: Arc<dyn SuccessionLedgerStore>,
        mail: Arc<dyn MailStore>,
        signer: Arc<dyn IdentitySigner>,
        mua: MuaInstructions,
    ) -> Self {
        // The never-enabled shape lives on `MailSettingsSnapshot::default()`
        // (which documents why `serving_enabled` defaults on); only the resolved
        // MUA instructions are ours to supply.
        let snapshot = MailSettingsSnapshot {
            mua: mua.clone(),
            ..Default::default()
        };
        Self {
            actor_id,
            nest,
            ledger,
            mail,
            signer,
            mua,
            inner: Mutex::new(Inner { snapshot }),
        }
    }

    /// The actor's id. Not part of the UniFFI surface — `[u8; 32]` is not
    /// a UniFFI-representable return type and the per-app UI has no need
    /// for it (it built the machine with the keypair). Used by the
    /// crate-internal rotation path and native (linux) callers.
    pub fn actor_id(&self) -> [u8; 32] {
        self.actor_id
    }

    /// The recipient HPKE secret for inbound-mail decrypt — the secret half of
    /// `derive_recipient_hpke_keypair(msek)` (the nest only ever holds the public
    /// half). `None` when mail isn't enabled (no MSEK provisioned yet).
    ///
    /// The conversations SMTP receive rail's source opens each sealed inbound
    /// record under this secret (`fauna_mail::open_inbound_record`,
    /// `docs/goal/behavior/smtp-server.md` § Inbound client receive). Shared on
    /// the machine so every app derives it identically from the synced
    /// mail custody's MSEK rather than re-rolling the KDF in per-app glue
    /// (priority #2). Native (linux) derives it inline in `mail_sink.rs` today;
    /// web/UniFFI consume this read instead of reaching for the raw MSEK.
    pub async fn recipient_hpke_secret(&self) -> Result<Option<[u8; 32]>, DispatchError> {
        let mail = self.mail.load().await?;
        Ok(mail.msek.map(|msek| derive_recipient_hpke_keypair(&msek).0))
    }

    /// The recipient **ML-KEM-768 decapsulation key** (2400 bytes) for opening
    /// post-quantum (X-Wing) inbound mail — the ML-KEM half of
    /// `derive_recipient_xwing_keypair(msek).secret` (the X25519 half is
    /// [`Self::recipient_hpke_secret`], reused). `None` when mail isn't enabled.
    ///
    /// The web/UniFFI receive rail threads this alongside the HPKE secret into
    /// `fauna_mail::open_inbound_record_hybrid` so it reads both classical and
    /// hybrid mail (post-quantum slice S3d, `architecture/security/post-quantum.md`).
    /// Native (linux / fauna-client-conversations) derives it inline from the same
    /// MSEK; this read keeps the KDF in shared Rust for web/UniFFI (priority #2) and
    /// the MSEK never leaves Rust.
    pub async fn recipient_mlkem_decaps_key(&self) -> Result<Option<Vec<u8>>, DispatchError> {
        let mail = self.mail.load().await?;
        Ok(mail.msek.map(|msek| {
            derive_recipient_xwing_keypair(&msek)
                .secret
                .mlkem_decaps_key()
                .to_vec()
        }))
    }

    /// The account's complete **standing** recipient-mail key set — the current
    /// MSEK's keypair first, then one per prior grace generation
    /// (the custody's `prior_mseks`, capped so the total is
    /// [`SNAPSHOT_GRACE_KEYPAIRS`]) — from the ONE shared derivation the MDA's
    /// snapshot is built from (`derive_standing_mail_keypairs`), so what a
    /// client can open is exactly what the MDA can (`owner-key-material.md`
    /// § Path B-sibling-2). Empty when mail isn't enabled. The web receive rail
    /// threads this into `WasmConversationsManager::setRecipientKeypairs`; the
    /// single-generation readers above remain for the spam-model seal, which
    /// is always to the current generation. The MSEK never leaves Rust.
    ///
    /// The client's **mail-epoch roots** for the content-sealing-epochs opener
    /// chain (design § 4/§ 5), as a flat `Vec<u8>` of `N × 32` bytes: root `[0]`
    /// = the current MSEK's [`derive_mail_epoch_root`], then one per prior grace
    /// generation (the custody's `prior_mseks`, capped so the total is
    /// [`SNAPSHOT_GRACE_KEYPAIRS`]) — exactly the set the shared snapshot builder
    /// carries as `mail_epoch_grace_roots`. Empty when mail isn't enabled (no
    /// MSEK). The web/UniFFI receive rail threads this into the epoch opener
    /// (`fauna_mail::open_inbound_record_epoch_hybrid`) so a client reads mail
    /// sealed under a mail epoch key once the write flip is thrown; native
    /// (`fauna-client-conversations`) derives the same set inline. Shared here so
    /// the KDF stays in Rust and the MSEK never leaves it (priority #2). Returned
    /// flat (not `Vec<[u8; 32]>`) so the wasm boundary is one `Uint8Array`.
    ///
    /// **Custody note.** [`derive_mail_epoch_root`] hands each root out as
    /// `Zeroizing` (`key-material-hierarchy.md` § Path B-sibling-2 → *Custody of
    /// the derived bytes*), and each wrapper drops — zeroizing — as soon as its
    /// bytes are copied into `out`. `out` itself is a bare `Vec<u8>` because the
    /// value crosses the wasm/UniFFI boundary, where a Rust custody wrapper
    /// cannot follow: that is the documented end of the custody window, not an
    /// oversight. What crosses is still strictly generation-scoped mail read
    /// material (rule #7) — never the MSEK itself.
    pub async fn standing_mail_keypairs(&self) -> Result<Vec<StandingMailKeypair>, DispatchError> {
        let mail = self.mail.load().await?;
        let Some(msek) = mail.msek.as_ref() else {
            return Ok(Vec::new());
        };
        let mseks: Vec<[u8; 32]> = std::iter::once(**msek)
            .chain(
                mail.prior_mseks
                    .iter()
                    .take(SNAPSHOT_GRACE_KEYPAIRS - 1)
                    .map(|prior| **prior),
            )
            .collect();
        Ok(derive_standing_mail_keypairs(&mseks))
    }

    /// The account's own X-Wing recipient **public** key — what a client seals
    /// its own copy of a bridged message to (`ui/conversations.md` § Where
    /// logic lives → *The `Bridged` adapter*: the Sent row). `None` when mail
    /// isn't enabled. The same derivation the publish path uses, so a row
    /// sealed here opens under [`Self::standing_mail_keypairs`]`[0]`. Public
    /// material: it may cross the wasm boundary freely.
    pub async fn recipient_public_key(&self) -> Result<Option<XWingPublicKey>, DispatchError> {
        let mail = self.mail.load().await?;
        Ok(mail
            .msek
            .as_ref()
            .map(|msek| derive_recipient_xwing_keypair(msek).public))
    }

    pub async fn mail_epoch_roots(&self) -> Result<Vec<u8>, DispatchError> {
        let mail = self.mail.load().await?;
        let Some(msek) = mail.msek.as_ref() else {
            return Ok(Vec::new());
        };
        let mut out = Vec::with_capacity(SNAPSHOT_GRACE_KEYPAIRS * 32);
        out.extend_from_slice(derive_mail_epoch_root(msek).as_slice());
        for prior in mail.prior_mseks.iter().take(SNAPSHOT_GRACE_KEYPAIRS - 1) {
            out.extend_from_slice(derive_mail_epoch_root(prior).as_slice());
        }
        Ok(out)
    }

    /// Build + seal + provision the read-side MLS snapshot for an MSEK history
    /// (`mseks[0]` = current generation, the rest grace, deduped; the builder
    /// caps at [`SNAPSHOT_GRACE_KEYPAIRS`]). Sealed under `mseks[0]` — the key
    /// the MDA unwraps it with.
    ///
    /// **This is the dk half of the recipient keypair**, and the *only* route
    /// by which it reaches the nest. Its ek half rides
    /// `provision_recipient_mls_pubkey`. `LeafInitKeypair::mlkem_dk` declares
    /// the invariant that binds them — an absent `mdk` is safe only because
    /// "hybrid mail is only sealed once the recipient has *published* the
    /// matching ek …, which the same client transaction that (re)writes this
    /// snapshot performs". So every path that publishes an ek calls this in the
    /// same transaction, and all three (first-enable, rotation, connect-time
    /// refresh) share this one recipe rather than open-coding it (priority #3).
    /// Break the pairing and the MDA advertises a decapsulation key it does not
    /// hold: everything it then seals is unopenable by the session that sealed
    /// it — silently, since the CalDAV read path logs and skips the collection.
    pub(crate) async fn provision_snapshot_for(
        &self,
        mseks: &[[u8; 32]],
    ) -> Result<(), DispatchError> {
        let Some(current) = mseks.first() else {
            return Ok(());
        };
        let mut deduped: Vec<[u8; 32]> = Vec::with_capacity(mseks.len());
        for k in mseks {
            if !deduped.contains(k) {
                deduped.push(*k);
            }
        }
        // ⚠ **OFF the caller's stack, on purpose** — this line derives an X-Wing
        // (ML-KEM-768 ∥ X25519) keypair per grace MSEK, and it is the exact leaf
        // a macOS crash died in on 2026-08-23: `SIGBUS` on the guard page of a
        // **544 KB** Swift cooperative-pool thread, which is what polls a
        // `fauna-ffi` async export (`async_runtime = "tokio"` enters the tokio
        // *context*, it does not move the polling to a tokio worker). The rule
        // and both remedies are owned by
        // `docs/goal/architecture/apps/native-async-execution.md` — read it there.
        //
        // `spawn_blocking` rather than that doc's usual `Box::pin`: the cost here
        // is a SYNCHRONOUS callee's frames, and moving a future to the heap does
        // not change how much stack a synchronous callee needs. The blocking pool
        // gives ~2 MB, and CPU-bound keygen belongs off an async worker anyway —
        // so this is the right place on both axes.
        //
        // At the LEAF deliberately, not at the FFI export: every caller of this
        // recipe is covered at once — first-enable, rotation, and the
        // connect-time refresh. That last one is why this is not theoretical.
        // `FaunaClient.refreshMailEpochSchedule` reaches here from a bare
        // `Task {}` (no actor isolation ⇒ the small pool) on **every**
        // authenticated start; it survived only because its frame chain sat a
        // few frames shallower than the crash's did — a margin nobody measures.
        // (wasm arm: no tokio and no blocking pool exist on wasm32 — this
        // crate's tokio dep is native-gated — and the 544 KB-cooperative-pool
        // hazard above is a native-FFI stack shape; the browser main stack
        // takes the synchronous derivation directly.)
        #[cfg(not(target_arch = "wasm32"))]
        let snapshot_state = tokio::task::spawn_blocking(move || {
            build_mls_snapshot_plaintext(&deduped).to_canonical_bytes()
        })
        .await
        .map_err(|e| DispatchError::Wrap(format!("mls snapshot plaintext task failed: {e}")))??;
        #[cfg(target_arch = "wasm32")]
        let snapshot_state = build_mls_snapshot_plaintext(&deduped).to_canonical_bytes()?;
        let snapshot_blob =
            wrap::seal_snapshot_under_msek(&snapshot_state, &self.actor_id, current)?;
        self.nest.provision_mls_snapshot_blob(snapshot_blob).await?;
        Ok(())
    }

    /// The MSEK history to snapshot for the CURRENT generation: `msek` first,
    /// then the grace generations, exactly the set [`Self::mail_epoch_roots`]
    /// derives its roots from. Empty when mail isn't enabled.
    /// Copies out of custody deliberately: the result feeds
    /// [`Self::provision_snapshot_for`], which derives epoch roots from it and
    /// drops it — the transient mint-argument boundary the *Carrier shape* rule
    /// declares out of scope, not a carrier.
    fn snapshot_mseks(mail: &MailConfig) -> Vec<[u8; 32]> {
        let Some(msek) = mail.msek.as_ref() else {
            return Vec::new();
        };
        let mut mseks = vec![msek.to_array()];
        mseks.extend(mail.prior_mseks.iter().map(SecretArray32::to_array));
        mseks
    }

    /// The recipient **ML-KEM-768 encapsulation key** (1184 B) to publish
    /// alongside the X25519 recipient-mail pubkey — unconditionally: no
    /// capability token gates it (`architecture/security/post-quantum.md`
    /// § Capability negotiation, the 2026-09-24 ruling). Shared by
    /// [`Self::provision_mailbox_blobs`], the rotation loop and
    /// [`Self::refresh_epoch_schedule`] so the publish paths can never drift
    /// (priority #3). The ek is the ML-KEM half of
    /// `derive_recipient_xwing_keypair(msek)` — the X25519 half is the existing
    /// `derive_recipient_hpke_keypair` key, reused (not minted).
    pub(crate) fn recipient_mlkem_ek_to_publish(msek: &[u8; 32]) -> Vec<u8> {
        derive_recipient_xwing_keypair(msek)
            .public
            .mlkem_encaps_key()
            .to_vec()
    }

    /// The content-sealing-epoch schedule to publish alongside the standing
    /// recipient pubkey — the horizon `[e_now, e_now + MAIL_EPOCH_PUBLISH_HORIZON]`
    /// of per-epoch public halves (design 2026-07-18 § 3), MSEK-derived and
    /// published unconditionally (`architecture/encryption-at-rest.md`
    /// § Content-sealing epochs). Every epoch entry carries **both** halves —
    /// never a mix of hybrid and classical-only rows in one publication
    /// (INFO-B: a half-empty publication would silently downgrade a hybrid
    /// recipient's new mail to classical-only). Shared by
    /// [`Self::provision_mailbox_blobs`] + the rotation loop +
    /// [`Self::refresh_epoch_schedule`] so the publish paths can never drift
    /// (priority #3).
    pub(crate) fn epoch_seal_keys_to_publish(msek: &[u8; 32]) -> Vec<EpochSealKey> {
        let e_now = mail_sealing_epoch_of(Timestamp::now_secs() as u64);
        (e_now..=e_now + MAIL_EPOCH_PUBLISH_HORIZON)
            .map(|epoch| {
                let (_, mls_pubkey) = derive_recipient_epoch_hpke_keypair(msek, epoch);
                let mlkem_ek = derive_recipient_epoch_xwing_keypair(msek, epoch)
                    .public
                    .mlkem_encaps_key()
                    .to_vec();
                EpochSealKey {
                    epoch,
                    mls_pubkey: ByteBuf::from(mls_pubkey.to_vec()),
                    mlkem_ek: ByteBuf::from(mlkem_ek),
                }
            })
            .collect()
    }

    /// All the MSEK-derived recipient key material the client model-write loop
    /// needs to **re-seal** the spam model to the actor's own key — the secret to
    /// unwrap the fetched blob and the X-Wing public key to re-seal under.
    /// `None` when mail isn't enabled (no MSEK), which is the social-only edge:
    /// there is no per-user seal key, so [`Self::apply_spam_model_write`] skips
    /// the model write ([`SpamModelWriteOutcome::ServerPath`]). Derived in this one
    /// place so the KDF stays single-sourced (priority #2); the MSEK never leaves
    /// Rust — only the sealed opaque blob crosses the wire.
    async fn recipient_reseal_material(&self) -> Result<Option<ReSealKeyMaterial>, DispatchError> {
        let mail = self.mail.load().await?;
        Ok(mail.msek.map(|msek| {
            let (x25519_secret, _) = derive_recipient_hpke_keypair(&msek);
            let xwing = derive_recipient_xwing_keypair(&msek);
            // Owned copies so the derived keypairs don't outlive this closure.
            let mlkem_dk = xwing.secret.mlkem_decaps_key().to_vec();
            ReSealKeyMaterial {
                x25519_secret,
                mlkem_dk,
                xwing_pubkey: xwing.public,
            }
        }))
    }

    /// Apply one [`ModelWriteOp`] to the caller's per-user spam model on the
    /// **client** — the tier-1 model-write half of the content-moderation/ranking
    /// frame's first sealing slice (`docs/goal/architecture/content-moderation-and-ranking.md`
    /// § Sealing tier-1; co-design tracked internally).
    ///
    /// When the per-user model is sealed at rest the nest cannot read-mutate-write
    /// it in-process, so the mutation runs here (the client holds the user's key):
    /// fetch the sealed model → unwrap under the MSEK-derived recipient secret →
    /// apply `op` → re-seal the whole model to the actor's own key → write it back
    /// opaque via `fauna.bridges.put_spam_model`.
    ///
    /// The path is chosen by **feature-presence capability negotiation**
    /// ([`dispatch_for`], co-design § Revision history 2026-07-06 C/D + G), reading
    /// two nest signals: [`SPAM_MODEL_SEALED_AT_REST`] gates the sealed-write path
    /// (absent ⇒ [`SpamModelWriteOutcome::ServerPath`] — a blind re-seal against a
    /// still-seals-on-read nest would double-seal into unreadability =
    /// **user-data loss**). The reseal is always the X-Wing suite: the client
    /// always publishes its ML-KEM ek, so the nest's `seal_recipient_blob` gate
    /// (recipient ek published) seals the same suite. `ServerPath` means no
    /// sealed write is possible (the nest lacks the token, or the actor has no
    /// MSEK): the model half is simply skipped — there is no server-side model
    /// write to fall back to (the model rests sealed, `mail-spam.md`
    /// § Encrypted-mode interaction).
    pub async fn apply_spam_model_write(
        &self,
        op: ModelWriteOp,
        history: Option<HistoryWrite>,
    ) -> Result<SpamModelWriteOutcome, DispatchError> {
        let sealed_supported = self.nest.nest_supports(SPAM_MODEL_SEALED_AT_REST).await?;
        let material = self.recipient_reseal_material().await?;

        match dispatch_for(sealed_supported, material.is_some()) {
            SealedWriteDispatch::ServerPath => return Ok(SpamModelWriteOutcome::ServerPath),
            SealedWriteDispatch::Sealed => {}
        }
        // `dispatch_for` returns `Sealed` only when `material.is_some()`.
        let material = material.expect("Sealed dispatch ⇒ reseal material present");
        let mlkem_dk: &[u8; MLKEM768_DECAPS_KEY_LEN] =
            material.mlkem_dk.as_slice().try_into().map_err(|_| {
                DispatchError::InvalidState(format!(
                    "recipient ML-KEM decaps key must be {MLKEM768_DECAPS_KEY_LEN} bytes"
                ))
            })?;

        // Fetch the current sealed model, plus the deployment-baseline write
        // signal the nest volunteers (§ Wire shapes): whether this actor is opted
        // in, and the aggregation-holder seal target to attach a fresh copy to on
        // this write. Only a STORED model is trained on: no model, or the nest's
        // read-time cold-start seed (`stored_sealed == false`), starts from a
        // fresh model — persisting the seed would bake the deployment baseline
        // into the user's own model (`mail-spam.md` § Cold start Path 2).
        let fetched = self.nest.fetch_spam_model(self.actor_id).await?;

        let resealed = apply_and_reseal_hybrid(
            stored_model(&fetched),
            &material.x25519_secret,
            mlkem_dk,
            &material.xwing_pubkey,
            &op,
        )
        .map_err(|e| DispatchError::Wrap(format!("client spam-model re-seal: {e}")))?;

        // Deployment-baseline re-seal-on-every-write: while this actor is opted in
        // and the nest volunteered a holder, seal a copy of the **post-mutation
        // plaintext** model (already in hand — `resealed.model_bytes`, no redundant
        // unseal) to the aggregation holder and attach it atomically with the model
        // write, so the holder-readable copy never lags the model (`mail-spam.md`
        // § Encrypted-mode interaction). `holder_seal_target: None` (no holder enrolled)
        // ⇒ no copy, and the plaintext-row merge still counts a
        // server-written model.
        let holder_copy = self.seal_baseline_holder_copy(
            fetched.contribute_baseline,
            fetched.holder_seal_target.as_ref(),
            &resealed.model_bytes,
        )?;

        // Compose the atomic training-history mutation (build-item 3 write side).
        // Sealing subject + delta uses the **same suite** the model rode, so the
        // audit row shares the model's key custody (nest-opaque either way).
        let seal_blob = |bytes: &[u8]| -> Result<Vec<u8>, DispatchError> {
            seal_history_blob_hybrid(bytes, &material.xwing_pubkey)
                .map_err(|e| DispatchError::Wrap(format!("seal spam-history blob: {e}")))
        };
        let history_op = match history {
            None => None,
            Some(HistoryWrite::Delete { history_id }) => Some(SpamHistoryOp::Delete { history_id }),
            Some(HistoryWrite::Insert {
                message_id,
                mailbox,
                subject,
                source,
            }) => {
                // An Insert is intrinsically a train event — its label is the
                // `Train`'s. A mismatched op is a caller bug.
                let label = match &op {
                    ModelWriteOp::Train { label, .. } => match label {
                        SpamLabel::Spam => WireSpamLabel::Spam,
                        SpamLabel::Ham => WireSpamLabel::Ham,
                    },
                    _ => {
                        return Err(DispatchError::InvalidState(
                            "HistoryWrite::Insert requires a Train op".into(),
                        ));
                    }
                };
                Some(SpamHistoryOp::Insert {
                    message_id,
                    mailbox,
                    sealed_subject: seal_blob(subject.as_bytes())?,
                    sealed_delta: seal_blob(&encode_history_delta(&resealed.delta))?,
                    label,
                    source,
                })
            }
        };

        match self
            .nest
            .put_spam_model(
                resealed.sealed_model,
                resealed.sample_count,
                history_op,
                holder_copy,
            )
            .await?
        {
            PutSpamModelOutcome::Written => Ok(SpamModelWriteOutcome::Sealed {
                sample_count: resealed.sample_count,
                delta: resealed.delta,
            }),
            // The one-lesson rule: the nest kept the stored model as it was, so
            // the mutation we sealed is not the model on record — report the
            // count that is. An outcome a newer nest added reads the same way,
            // as not written: the mutation is discarded, never assumed stored.
            PutSpamModelOutcome::DuplicateSignal | PutSpamModelOutcome::Unknown => {
                Ok(SpamModelWriteOutcome::Duplicate {
                    sample_count: resealed.sample_count_before,
                })
            }
        }
    }

    /// Seal a deployment-baseline **holder copy** of the just-written model when
    /// this actor is opted in and the nest volunteered an aggregation-holder seal
    /// target — the shared copy-attach step both [`Self::apply_spam_model_write`]
    /// (every sealed write) and the toggle-ON seam (the initial copy) run.
    ///
    /// `model_bytes` is the **post-mutation plaintext** `SpamModel` (the writer
    /// holds it unwrapped mid-write, so no redundant unseal). The seal binds this
    /// actor as the owner into the copy's AAD (`seal_spam_model_copy`), and selects
    /// the X-Wing suite iff the holder published an ML-KEM ek (PQ-4b). Returns
    /// `None` when opted out or no holder is enrolled — the caller then attaches no
    /// copy (`mail-spam.md` § Encrypted-mode interaction).
    fn seal_baseline_holder_copy(
        &self,
        contribute_baseline: bool,
        holder_seal_target: Option<&HolderSealTarget>,
        model_bytes: &[u8],
    ) -> Result<Option<SpamModelHolderCopy>, DispatchError> {
        let Some(target) = holder_seal_target.filter(|_| contribute_baseline) else {
            return Ok(None);
        };
        let holder_x25519: [u8; 32] = target.x25519_pubkey.as_slice().try_into().map_err(|_| {
            DispatchError::InvalidState(
                "fetch_spam_model holder_seal_target.x25519_pubkey must be 32 bytes".into(),
            )
        })?;
        let copy_blob = seal_spam_model_copy(
            model_bytes,
            &self.actor_id,
            &holder_x25519,
            target.mlkem_ek.as_ref().map(|ek| ek.as_slice()),
        )
        .map_err(|e| DispatchError::Wrap(format!("seal spam-model holder copy: {e}")))?;
        let sealed_copy = copy_blob
            .to_canonical_bytes()
            .map_err(|e| DispatchError::Wrap(format!("encode spam-model holder copy: {e}")))?;
        Ok(Some(SpamModelHolderCopy {
            holder_pubkey: target.x25519_pubkey.clone(),
            sealed_copy,
            extra: Default::default(),
        }))
    }

    /// Re-seal the caller's **current** model unchanged and attach a fresh
    /// deployment-baseline holder copy — the *initial-copy* step the toggle-ON seam
    /// runs so a publish right after opt-in already counts this contributor (the
    /// every-write step is [`Self::apply_spam_model_write`]'s). No-ops (writes
    /// nothing) when:
    /// - the actor is **untrained** (`blob` empty — the caller passes `None` for
    ///   the cold-start seed too, which is not the user's model) — there is no
    ///   model to contribute yet; the copy attaches on the first train's write
    ///   instead;
    /// - **mail is off** (no MSEK) — no per-user seal key; or
    /// - the nest hasn't advertised sealed-at-rest handling — no sealed write is
    ///   possible (the guard `apply_spam_model_write` also enforces).
    async fn attach_initial_holder_copy(
        &self,
        holder: &HolderSealTarget,
        blob: Option<&[u8]>,
    ) -> Result<(), DispatchError> {
        let Some(blob) = blob.filter(|b| !b.is_empty()) else {
            return Ok(());
        };
        let sealed_supported = self.nest.nest_supports(SPAM_MODEL_SEALED_AT_REST).await?;
        let material = self.recipient_reseal_material().await?;
        match dispatch_for(sealed_supported, material.is_some()) {
            SealedWriteDispatch::ServerPath => return Ok(()),
            SealedWriteDispatch::Sealed => {}
        }
        let material = material.expect("Sealed dispatch ⇒ reseal material present");
        // A no-op `ModelSync` re-seals the CURRENT model unchanged (an empty `other`
        // never wins the adopt-if-larger compare, `apply_model_write_op`), yielding
        // the post-mutation plaintext bytes to seal the copy from without a second
        // unseal — the same reseal + suite path `apply_spam_model_write` takes, but
        // decoupled from a training mutation and the mint.
        let noop = ModelWriteOp::ModelSync {
            other: SpamModel::new(),
        };
        let dk: &[u8; MLKEM768_DECAPS_KEY_LEN] =
            material.mlkem_dk.as_slice().try_into().map_err(|_| {
                DispatchError::InvalidState(format!(
                    "recipient ML-KEM decaps key must be {MLKEM768_DECAPS_KEY_LEN} bytes"
                ))
            })?;
        let resealed = apply_and_reseal_hybrid(
            Some(blob),
            &material.x25519_secret,
            dk,
            &material.xwing_pubkey,
            &noop,
        )
        .map_err(|e| {
            DispatchError::Wrap(format!("client spam-model re-seal (initial copy): {e}"))
        })?;
        // `contribute_baseline = true` by construction here (the toggle set the bit
        // before this runs), so the copy is always sealed to `holder`.
        let holder_copy =
            self.seal_baseline_holder_copy(true, Some(holder), &resealed.model_bytes)?;
        // `history_op: None` ⇒ the one-lesson rule cannot fire (it gates only an
        // `Insert`), so the outcome is `Written` by construction.
        let _written = self
            .nest
            .put_spam_model(
                resealed.sealed_model,
                resealed.sample_count,
                None,
                holder_copy,
            )
            .await?;
        Ok(())
    }

    /// Mint the keyless `content.read{spam-model}` deployment-baseline grant to the
    /// box's aggregation `holder` and record it in the signed grant log — the
    /// audit + revocation record and the authorization the publish worklist is gated on
    /// (`mail-spam.md` § Encrypted-mode interaction; the artifact travels as the
    /// separately-attached holder copy, this grant conveys **no** key material —
    /// `derive_scope_payload` returns `None` for the spam-model kind). The log
    /// records the mint **first**, and only a durable record releases the blob
    /// for deposit ([`grant_log::UndepositedGrant`] — the record-then-deposit
    /// rule, shared with `LinkedNestsMachine::mint`). ⚠ This comment used to say
    /// the opposite, having copied the pre-2026-08-14 order from that same site
    /// and cited it as canonical prior art; that is how finding reached
    /// a third call site, and why the ordering is now carried by a type rather
    /// than by prose. Holder source is swapped from the Admin-gated
    /// `discover_holders` to the fetch-reply's volunteered target — no
    /// `list_service_users`.
    async fn mint_baseline_grant(&self, holder: &HolderSealTarget) -> Result<(), DispatchError> {
        let holder_x25519: [u8; 32] = holder.x25519_pubkey.as_slice().try_into().map_err(|_| {
            DispatchError::InvalidState(
                "fetch_spam_model holder_seal_target.x25519_pubkey must be 32 bytes".into(),
            )
        })?;
        let now = Timestamp::now_secs() as u64;
        let window_end = now + DEFAULT_GRANT_WINDOW_SECS;
        // Random 16-byte id (NOT a counter — a counter would collide the nest's
        // `(owner, grant_id)` key across re-opt-ins; prior art
        // `client-pair`'s `new_grant_id`).
        let mut grant_id = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut grant_id);
        let scope = ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.into(),
            kind: Some(ScopeTuple::KIND_SPAM_MODEL.into()),
            tier: None,
            set: None,
            factor: None,
        };
        let blob = mint_grant(
            // Mail and spam-model scopes only — no period key is derived.
            None,
            // Keyless (`content.read{spam-model}`): no payload derives, so no
            // mail material is read.
            &MailConfig::default(),
            &self.actor_id,
            &grant_id,
            &holder_x25519,
            holder.mlkem_ek.as_ref().map(|ek| ek.as_slice()),
            GrantWindow(now, window_end),
            std::slice::from_ref(&scope),
        )
        .map_err(|e| DispatchError::Wrap(format!("mint spam-model grant: {e}")))?;
        let blob_bytes = blob
            .to_canonical_bytes()
            .map_err(|e| DispatchError::Wrap(format!("encode spam-model grant blob: {e}")))?;
        let pending = grant_log::UndepositedGrant::new(grant_id, blob_bytes);
        let event_scope = GrantEventScope {
            class: ScopeTuple::CLASS_CONTENT_READ.into(),
            kind: Some(ScopeTuple::KIND_SPAM_MODEL.into()),
            tier: None,
        };
        let unsigned = grant_log::build_mint_event(
            grant_id,
            holder_x25519,
            vec![event_scope],
            now,
            window_end,
            now,
        );
        let signed = self.signer.sign_grant_event(unsigned)?;
        let stored = self
            .ledger
            .merge(SuccessionLedger::events_replica(
                ActorId(self.actor_id),
                vec![signed],
            ))
            .await?;
        let blob_bytes = pending
            .release(&grant_log::RecordedGrants::from_stored(&stored))
            .map_err(|e| DispatchError::InvalidState(e.to_string()))?;
        self.nest.mint_grant(blob_bytes).await?;
        Ok(())
    }

    /// Best-effort heal of the actor's outstanding **bounded** mail grants after
    /// an MSEK rotation (design § 5 + the 2026-07-19 amendment; owner doc
    /// `encryption-at-rest.md` § Capability tiering). An MSEK hard-revoke resets
    /// the mail epoch lineage, so a bounded grant's per-epoch wraps (sealed under
    /// the retired root) stop covering content sealed under the new root. For each
    /// grant still in-window this re-wraps its whole window under the
    /// **post-rotation generation set** ([`bounded_mail_rotation_heal_keys`] reads
    /// the current + prior MSEKs from the mail custody) and drives one **equal-end**
    /// `fauna.capabilities.renew` (a pure key refresh — the nest replaces each
    /// differing `(scope, epoch)` wrap, B3c), so the holder heals across the
    /// hard-revoked lineage without the owner re-minting. A `Renew` event is
    /// recorded per healed grant (History forensics, `ui/nests.md`).
    ///
    /// **Log-only / best-effort:** every error path logs and continues — a heal
    /// failure NEVER fails the rotation (the grant heals at its next renew
    /// instead), matching the § 3 degradation posture. Called from
    /// [`crate::rotation`]'s `drive_rotation_loop` only after the MSEK swap is
    /// durably committed.
    ///
    /// The holder's seal target is **re-resolved from the live roster**
    /// ([`NestClient::content_processor_holders`]), never read from the grant log
    /// (`ui/nests.md:163` — holder metadata is recomputed, so a lying nest merely
    /// fails to decrypt). A holder absent from the roster is **skipped**, not
    /// healed classical-blind: a re-wrap without the holder's resolved ML-KEM ek
    /// could PQ-downgrade a hybrid grant's wraps via replace-on-append.
    pub(crate) async fn heal_outstanding_bounded_grants(&self) {
        let ledger = match self.ledger.load().await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "rotation heal: read grant log failed; bounded grants heal at next renew");
                return;
            }
        };
        let mail = match self.mail.load().await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "rotation heal: load mail custody failed; bounded grants heal at next renew");
                return;
            }
        };
        let now = Timestamp::now_secs().max(0) as u64;
        // Outstanding, still-in-window bounded mail grants. Enumerating first lets
        // the common path (no bounded grants — the pre-flip reality, since
        // `mint_bounded_mail_grant` has zero production callers) skip the holder
        // discovery RPC entirely.
        let healable: Vec<grant_log::CurrentGrant> = grant_log::current_grants(&ledger)
            .into_iter()
            .filter(|g| grant_log::is_bounded_mail_grant(&g.scope) && g.window_end >= now)
            .collect();
        if healable.is_empty() {
            return;
        }
        // Resolve the live holder roster once for all grants.
        let holders = match self.nest.content_processor_holders().await {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(error = %e, "rotation heal: holder discovery failed; bounded grants heal at next renew");
                return;
            }
        };
        let mut renewals = SuccessionLedger::events_replica(ActorId(self.actor_id), Vec::new());
        for g in healable {
            let Ok(grant_id): Result<[u8; 16], _> = g.grant_id.as_slice().try_into() else {
                tracing::warn!("rotation heal: skipping grant with a non-16-byte id");
                continue;
            };
            let Ok(holder_pk): Result<[u8; 32], _> = g.holder.as_slice().try_into() else {
                tracing::warn!("rotation heal: skipping grant with a non-32-byte holder");
                continue;
            };
            let Some(holder) = holders.iter().find(|h| h.pubkey == holder_pk) else {
                tracing::debug!(
                    "rotation heal: grant holder not in the live roster; heals at its next renew"
                );
                continue;
            };
            let window = GrantWindow(g.window_start, g.window_end);
            // A per-labeler grant heals per labeler: its factor rides every
            // re-wrapped key exactly as the mint's did, or the heal would
            // append a composed-read wrap to a grant the owner confined.
            let factor = grant_log::labeler_factor_of_grant(&g.scope);
            let wraps = match bounded_mail_rotation_heal_keys(
                &mail,
                &self.actor_id,
                &holder_pk,
                holder.mlkem_ek.as_deref(),
                &window,
                factor.as_deref(),
            ) {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!(error = %e, "rotation heal: re-wrap failed; grant heals at next renew");
                    continue;
                }
            };
            let appended: Result<Vec<Vec<u8>>, _> =
                wraps.iter().map(|w| w.to_canonical_bytes()).collect();
            let appended = match appended {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!(error = %e, "rotation heal: encode wraps failed; grant heals at next renew");
                    continue;
                }
            };
            // Equal-window renew — the recorded window is unchanged; only the
            // key wraps refresh.
            if let Err(e) = self
                .nest
                .renew_grant(grant_id, g.window_start, g.window_end, appended)
                .await
            {
                tracing::warn!(error = %e, "rotation heal: renew RPC failed; grant heals at next renew");
                continue;
            }
            // Record the Renew in the signed grant log (History), mirroring
            // `mint_baseline_grant`'s signer-seam path (raw key never crosses FFI).
            let unsigned = match grant_log::build_renew_event(
                &ledger,
                &grant_id,
                g.window_start,
                g.window_end,
                now,
            ) {
                Ok(ev) => ev,
                Err(e) => {
                    tracing::warn!(error = %e, "rotation heal: build renew event failed");
                    continue;
                }
            };
            let signed = match self.signer.sign_grant_event(unsigned) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "rotation heal: sign renew event failed");
                    continue;
                }
            };
            grant_log::append_signed(&mut renewals, signed);
        }
        // One ledger write for all healed grants. A failed persist is benign: the
        // nest already holds the healed wraps; only the History Renew rows are lost
        // (they reappear at the grant's next real renew).
        if !renewals.grant_events.is_empty()
            && let Err(e) = self.ledger.merge(renewals).await
        {
            tracing::warn!(error = %e, "rotation heal: persist renew log failed; History Renew rows deferred");
        }
    }

    /// Unwrap a client-written history row's sealed `model_delta_applied` back to
    /// its forward n-gram set — the read half a client-side **undo** runs before
    /// building [`ModelWriteOp::Undo`]. Keeps the MSEK-derived key material inside
    /// Rust (only the opaque sealed bytes ever cross a boundary): derives the
    /// recipient secret (+ hybrid ML-KEM decaps key when the actor is PQ-hybrid) and
    /// opens the blob via [`unwrap_sealed_history_delta`] (the hybrid opener reads
    /// either suite, so a since-migrated actor still opens a classically-sealed row).
    /// `None` when mail isn't enabled (no MSEK) — the actor then has no key to
    /// open the row with, so the undo cannot run.
    pub async fn unwrap_sealed_history_delta(
        &self,
        sealed_delta: &[u8],
    ) -> Result<Option<BTreeSet<String>>, DispatchError> {
        let Some(material) = self.recipient_reseal_material().await? else {
            return Ok(None);
        };
        // A malformed decaps key ⇒ fall back to the classical opener; it
        // still reads a classically-sealed row (the common case).
        let mlkem_dk: Option<&[u8; MLKEM768_DECAPS_KEY_LEN]> =
            material.mlkem_dk.as_slice().try_into().ok();
        unwrap_sealed_history_delta(sealed_delta, &material.x25519_secret, mlkem_dk)
            .map(Some)
            .map_err(|e| DispatchError::Wrap(format!("unwrap sealed history delta: {e}")))
    }

    /// Unwrap a client-written row's sealed **subject** to its plaintext string —
    /// the read half the `mail-spam` training-history display runs to render a
    /// sealed row as `{unwrapped subject} · {mailbox}` (the nest can't format it,
    /// so it degrades `message` to the mailbox alone). Opens under the same
    /// MSEK-derived recipient key as [`Self::unwrap_sealed_history_delta`] (the
    /// MSEK never leaves Rust); a non-UTF-8 payload is rendered lossily rather than
    /// failing the whole list. `None` when mail isn't enabled (no key).
    pub async fn unwrap_sealed_history_subject(
        &self,
        sealed_subject: &[u8],
    ) -> Result<Option<String>, DispatchError> {
        let Some(material) = self.recipient_reseal_material().await? else {
            return Ok(None);
        };
        let mlkem_dk: Option<&[u8; MLKEM768_DECAPS_KEY_LEN]> =
            material.mlkem_dk.as_slice().try_into().ok();
        let bytes = unwrap_sealed_history_bytes(sealed_subject, &material.x25519_secret, mlkem_dk)
            .map_err(|e| DispatchError::Wrap(format!("unwrap sealed history subject: {e}")))?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    // ─── EnableMail: turn email on (fresh, dormant-MSEK or DAV upgrade) ───

    async fn enable_mail(
        &self,
        display_name: String,
        credential: Credential,
    ) -> Result<(), DispatchError> {
        let rows = self.mail.load_rows().await?;
        let mail = rows.config();
        if mail.is_mail_enabled() {
            // The page offered Enable, so its snapshot predates what the
            // custody now holds — rows another device (or this device's
            // earlier session) wrote that the local store pulled in after the
            // page last hydrated. Refresh before refusing, so the page shows
            // the enabled mailbox rather than a toggle that keeps failing.
            // The refusal stands: a generated or typed secret must never be
            // reported as the mailbox's password when no credential holds it.
            let _ = self.refresh_snapshot().await;
            return Err(DispatchError::InvalidState(
                "mail already enabled — use AddCredential instead".into(),
            ));
        }

        self.set_status(SettingsStatus::Syncing);

        let upgrade = mail
            .live_credentials()
            .next()
            .filter(|_| mail.msek.is_some())
            .cloned();
        match upgrade {
            // ── Upgrade: a **DAV-only** mailbox already holds the shared MSEK
            //    + a live read credential (read-only recipe — no submission
            //    token). Enabling email is *additive*: reuse them and provision
            //    only the missing outbound submission token (one shared bridge
            //    password serves IMAP + SMTP + CalDAV — `caldav-server.md`
            //    § Independent enablement, "no separate CalDAV credential"),
            //    rather than minting a second mailbox that would force the user to
            //    reconfigure their MUA. The passed `credential`/`display_name` are
            //    unused — the user keeps the credential they already AUTH CalDAV
            //    with. ──
            Some(row) => {
                let credential_id = row.credential_id.clone();
                let existing = Credential::of_row(&row)?;
                let _ = (display_name, credential);
                self.provision_submission_token(&credential_id, &existing)
                    .await?;
            }
            // ── Fresh, or a **dormant** MSEK (one a teardown or a succession
            //    burn left with no live credential): make sure the account holds
            //    an MSEK, then mint the credential under it with the full
            //    recovery-safe blob set incl. the submission token (snapshot →
            //    recipient-pubkey → wrapped-MSEK → submission-token;
            //    `mail-credentials.md` § Partial-state-during-minting). ──
            None => {
                let (msek, mail) = self.ensure_msek(mail).await?;
                self.mint_credential_under(&msek, &mail, &rows, display_name, &credential, true)
                    .await?;
            }
        }
        let mail = self.mail.load().await?;
        self.mail
            .write_state(MailStateRow {
                mail_enabled: Some(true),
                ..MailStateRow::recreatable_of(&mail)
            })
            .await?;
        self.refresh_snapshot().await?;

        // Flip the deployment-wide mail subsystem on (design A). On a real
        //    deployment (Docker/systemd) the mail-bridge s6 services are gated
        //    on the `/data/imap-enabled` flag that `fauna.bridges.set_mail_enabled`
        //    writes; without this, enabling a mailbox provisions the user's keys
        //    but the bridge never boots — the production gap the tier_4
        //    `test_mail_client_ui_enable_docker` regression pins. The call is
        //    Admin-class on the nest, which scopes it for free: the admin (the
        //    first user) flips the subsystem on, while a non-admin enabling their
        //    own mailbox is rejected server-side (a no-op) — so we swallow
        //    `Rejected`. A `Transient` (connectivity) failure surfaces: it rides
        //    the same WS that just provisioned the mailbox, so the user should
        //    learn the enable didn't fully land.
        match self.nest.set_mail_enabled(true).await {
            Ok(()) | Err(NestError::Rejected(_)) => {}
            Err(e @ NestError::Transient(_)) => return Err(e.into()),
        }
        Ok(())
    }

    /// The account's MSEK, minting one when none rests: the fresh key is
    /// written to the state row **before** anything is provisioned under it
    /// (the MSEK is irrecoverable — `key-material-hierarchy.md` § Path B — so
    /// it persists first; a crash after this leaves a dormant MSEK the next
    /// enable reuses), then **read back**, and the key the row now holds is
    /// the one returned. Two devices enabling at once each write their own;
    /// the state row's join keeps one, and reading it back is what makes both
    /// provision under the winner rather than each under its own.
    async fn ensure_msek(
        &self,
        mail: MailConfig,
    ) -> Result<(SecretArray32, MailConfig), DispatchError> {
        if let Some(msek) = mail.msek.clone() {
            return Ok((msek, mail));
        }
        self.mail
            .write_state(MailStateRow {
                // `fresh_msek()` hands back `Zeroizing<[u8; 32]>`; `.into()`
                // MOVES it into custody (the *Carrier shape* rule).
                msek: Some(fresh_msek().into()),
                ..MailStateRow::recreatable_of(&mail)
            })
            .await?;
        let mail = self.mail.load().await?;
        let msek = mail.msek.clone().ok_or_else(|| {
            DispatchError::InvalidState("the new mail key did not persist — try again".into())
        })?;
        Ok((msek, mail))
    }

    /// Mint one credential under `msek`: provision the mailbox blob set for
    /// it (the snapshot over `mail`'s whole standing history, so a dormant
    /// MSEK's grace generations stay openable), then put its row — naming the
    /// generation it is wrapped under — under an id no row, revoked included,
    /// has ever held ([`MailRows::spent_credential_ids`]).
    async fn mint_credential_under(
        &self,
        msek: &SecretArray32,
        mail: &MailConfig,
        rows: &MailRows,
        display_name: String,
        credential: &Credential,
        with_submission_token: bool,
    ) -> Result<(), DispatchError> {
        let credential_id = derive_credential_id(&display_name, &rows.spent_credential_ids());
        let mut history = Self::snapshot_mseks(mail);
        if history.first() != Some(&msek.to_array()) {
            history.insert(0, msek.to_array());
        }
        self.provision_mailbox_blobs(
            msek,
            &history,
            &credential_id,
            credential,
            with_submission_token,
        )
        .await?;
        self.mail
            .put_credential(MailCredential {
                credential_id,
                display_name,
                kind: credential.kind().into(),
                secret: credential.as_bytes().to_vec().into(),
                created_at: Timestamp::now_secs() as u64,
                updated_at: Timestamp::default(),
                // The generation marker: this row's blob was just wrapped under
                // `msek` (`mail-credentials.md` § *The generation marker*).
                wrapped_under: Some(MsekFingerprint::of(msek)),
                revoked_at_unix: None,
                burned: None,
            })
            .await?;
        Ok(())
    }

    // ─── Enable{Caldav,Carddav}Mailbox: mint the shared mailbox key material
    //     for a DAV-only user (email never enabled) ───

    /// Provision the per-actor **mailbox key material** (the shared MSEK + the
    /// `default` read credential + the recovery-safe `bridge_*` blobs) for a user
    /// enabling **CalDAV** independently of email (`caldav-server.md`
    /// § Independent enablement — calendar-only is the privacy-maximal supported
    /// config). This is the calendar-side twin of [`Self::enable_mail`]; the MSEK
    /// is **shared** (one credential serves IMAP + SMTP + CalDAV + CardDAV,
    /// § Authentication — "no separate CalDAV credential"), so a user who later
    /// also enables email reuses this same MSEK.
    ///
    /// Two deliberate differences from [`Self::enable_mail`]:
    /// - **No submission token** — CalDAV needs no *outbound* mail capability, so
    ///   this uses the read-only recipe (`provision_mailbox_blobs(.., false)`, the
    ///   same one `provision_relay_mailbox` uses). The user provisions the
    ///   outbound submission token only if/when they enable email.
    /// - **No deployment toggle flip** — the Admin-class `set_caldav_enabled`
    ///   deployment toggle is owned by the [`BridgeApprovalMachine`]
    ///   (`SetCalDavEnabled`), which the onboarding launch glue fires separately;
    ///   this method is strictly the per-user MSEK mint.
    ///
    /// Minting the MSEK does **not** make the user email-reachable: an email route
    /// is a separate `account_aliases` row (an admin/onboarding concern), never a
    /// side effect of `enable_mail`/`enable_caldav_mailbox` — so a CalDAV-only user
    /// stays genuinely mailbox-less (the nest `resolve_recipient` → Reject the MDA
    /// auto-schedule gateway classifies as the sealed scheduling rail).
    ///
    /// **Idempotent:** a mailbox that already holds the MSEK and a live
    /// credential (email or a DAV protocol already enabled) is reduced to
    /// recording the per-actor flag — the existing shared mailbox already backs
    /// the calendar store.
    pub async fn enable_caldav_mailbox(
        &self,
        display_name: String,
        credential: Credential,
    ) -> Result<(), DispatchError> {
        self.enable_dav_mailbox(display_name, credential, DavProtocol::Caldav)
            .await
    }

    /// The **contacts** twin of [`Self::enable_caldav_mailbox`] — provision the
    /// shared mailbox key material for a user enabling **CardDAV** independently
    /// of email and calendar (`carddav-server.md` § Independent enablement). The
    /// address-book store (`bridge_carddav_*`) seals to the same MSEK-derived
    /// recipient keypair the calendar store uses, and the same `default` read
    /// credential AUTHs the CardDAV listener, so the recipe is identical — only
    /// the per-actor flag recorded differs. The Admin-class `set_carddav_enabled`
    /// deployment toggle is owned by [`BridgeApprovalMachine`]
    /// (`SetCardDavEnabled`), fired separately by the onboarding launch glue.
    pub async fn enable_carddav_mailbox(
        &self,
        display_name: String,
        credential: Credential,
    ) -> Result<(), DispatchError> {
        self.enable_dav_mailbox(display_name, credential, DavProtocol::Carddav)
            .await
    }

    /// Shared core of the two DAV-only mailbox enables — one recipe, one flag
    /// selector, so the calendar and contacts paths can never drift
    /// (`carddav-server.md` § Architectural rules: same concepts as CalDAV,
    /// never a parallel invention).
    async fn enable_dav_mailbox(
        &self,
        display_name: String,
        credential: Credential,
        protocol: DavProtocol,
    ) -> Result<(), DispatchError> {
        let rows = self.mail.load_rows().await?;
        let mail = rows.config();
        let provisioned = mail.msek.is_some() && mail.live_credentials().next().is_some();
        if provisioned && protocol.enabled_in(&mail) {
            // The shared mailbox is already provisioned and this protocol is
            // already on — the launch glue may call this unconditionally.
            return Ok(());
        }

        self.set_status(SettingsStatus::Syncing);
        if !provisioned {
            // Fresh, or a dormant MSEK with no live credential: the DAV store
            // seals under the account's MSEK (minted here only when none
            // rests), and its listener AUTHs a read credential — `enable_mail`'s
            // path, minus the submission token and the `set_mail_enabled`
            // deployment flip.
            let (msek, mail) = self.ensure_msek(mail).await?;
            self.mint_credential_under(&msek, &mail, &rows, display_name, &credential, false)
                .await?;
        }
        // Record that this protocol is now on (so a later "Disable mail"
        // preserves the shared MSEK it still needs — see
        // [`Self::disable_mail`]). An email flag never written reads as OFF:
        // a DAV-only mailbox must render "mail disabled" (`is_mail_enabled()`
        // keys on the flag, not on `msek.is_some()`).
        let mail = self.mail.load().await?;
        let mut state = MailStateRow::recreatable_of(&mail);
        protocol.set_enabled(&mut state);
        state.mail_enabled = Some(mail.mail_enabled.unwrap_or(false));
        self.mail.write_state(state).await?;
        self.refresh_snapshot().await?;
        Ok(())
    }

    /// Seal + provision the per-nest mailbox blobs for `msek` under `credential`
    /// against `self.nest`, in the recovery-safe order (snapshot →
    /// recipient-pubkey → wrapped-MSEK → [submission-token];
    /// `mail-credentials.md` § Partial-state-during-minting). Every step is an
    /// idempotent atomic-replace on the nest, so re-running it — a re-enable, or
    /// provisioning a *second* box the same user owns — overwrites rather than
    /// duplicates.
    ///
    /// `history` is the MSEK history the snapshot carries — `msek` first, then
    /// the grace generations ([`Self::snapshot_mseks`]) — so re-provisioning a
    /// mailbox that has rotated keeps its grace keypairs.
    ///
    /// `with_submission_token` is `true` for a box the user submits from (the
    /// primary / public relay box) and `false` for a relay/home box, which runs
    /// no MTA and so needs only the read credential
    /// (`deployment-home-with-public-relay.md` § Outbound mail — submission +
    /// DKIM + MX delivery all happen on the public box). Shared by the enables
    /// and `provision_relay_mailbox` (reused fleet MSEK) so the paths can never
    /// drift on the recipe.
    async fn provision_mailbox_blobs(
        &self,
        msek: &[u8; 32],
        history: &[[u8; 32]],
        credential_id: &str,
        credential: &Credential,
        with_submission_token: bool,
    ) -> Result<(), DispatchError> {
        // The read-side snapshot, sealed under `history[0]` (= `msek`).
        self.provision_snapshot_for(history).await?;

        // Register the actor's recipient-mail pubkey (derived from MSEK) *after*
        // the snapshot, so the matching secret is reachable before the MTA can
        // seal mail to the pubkey. The post-quantum ML-KEM ek and the
        // content-sealing-epoch schedule ride along, unconditionally.
        let (_, recipient_pubkey) = derive_recipient_hpke_keypair(msek);
        let mlkem_ek = Self::recipient_mlkem_ek_to_publish(msek);
        let epoch_keys = Self::epoch_seal_keys_to_publish(msek);
        self.nest
            .provision_recipient_mls_pubkey(
                self.actor_id,
                recipient_pubkey,
                mlkem_ek,
                Some(epoch_keys),
            )
            .await?;

        let msek_blob =
            wrap::seal_msek_under_credential(msek, &self.actor_id, credential_id, credential)?;
        self.nest.provision_wrapped_mls_blob(msek_blob).await?;

        if with_submission_token {
            self.provision_submission_token(credential_id, credential)
                .await?;
        }
        Ok(())
    }

    /// Mint + seal + provision the per-credential outbound **submission token** —
    /// the one blob the read-only recipe (`provision_mailbox_blobs(.., false)`)
    /// skips. Shared by `enable_mail` (fresh + the CalDAV→email upgrade),
    /// [`Self::add_credential`], and [`Self::provision_mailbox_blobs`] so the
    /// three paths can never drift on the seal recipe. Idempotent atomic-replace
    /// on the nest.
    async fn provision_submission_token(
        &self,
        credential_id: &str,
        credential: &Credential,
    ) -> Result<(), DispatchError> {
        let signed_token = self.mint_submission_token(credential_id)?;
        let token_blob = wrap::seal_token_under_credential(
            &signed_token,
            &self.actor_id,
            credential_id,
            credential,
        )?;
        self.nest
            .provision_wrapped_submission_token(token_blob)
            .await?;
        Ok(())
    }

    // ─── ProvisionRelayMailbox: provision the existing mailbox onto a *second*
    //     box the user owns (the home-with-public-relay home box), reusing the
    //     fleet MSEK so the relay's seal-to-recipient is decryptable there ───

    /// Reuse the account's MSEK + its first live credential to provision the
    /// mailbox onto `self.nest` (the newly-linked home box). No fresh MSEK — a
    /// different MSEK would silently break home-box decrypt (mail looks
    /// delivered but is unreadable). No submission token — the home box runs no
    /// MTA. Writes nothing to the mail custody: it already holds the MSEK and
    /// the credential, and this pushes `bridge_*` blobs to the peer nest, which
    /// don't federate. The mail custody is the ACCOUNT's (`fauna.state.mail`,
    /// fleet-wide across every box the user signs in to), so a fauna app
    /// connected to the home box reads the same MSEK — no per-box copy exists
    /// to write (`deployment-home-with-public-relay.md` § Pairing).
    async fn provision_relay_mailbox(&self) -> Result<(), DispatchError> {
        let mail = self.mail.load().await?;
        let msek = mail.msek.as_ref().ok_or_else(|| {
            DispatchError::InvalidState(
                "mail not enabled — enable mail on your primary box first".into(),
            )
        })?;
        // Reuse the default (first live) credential the user already configured
        // — the read credential the home box's MDA AEAD-unwraps at MUA-AUTH.
        // Minting a fresh credential here would force the user to reconfigure
        // their MUA.
        let row = mail.live_credentials().next().ok_or_else(|| {
            DispatchError::InvalidState(
                "mail enabled but no credential to provision — rotate or re-enable".into(),
            )
        })?;
        let credential_id = row.credential_id.clone();
        let credential = Credential::of_row(row)?;

        self.set_status(SettingsStatus::Syncing);

        // Read recipe only (no submission token — the home box never submits).
        self.provision_mailbox_blobs(
            msek,
            &Self::snapshot_mseks(&mail),
            &credential_id,
            &credential,
            false,
        )
        .await?;

        // Phase-3 D3 (`2026-07-07-phase-3-sealed-both-modes-design.md`): the
        // plaintext-MSEK deposit leg is RETIRED — relayed sealed mail is stored
        // verbatim in both modes and opened at the AUTH'd MDA session / this
        // client, so the plaintext MSEK is never transmitted to any box. (The
        // kind `provision_plaintext_msek` was removed by the 2026-09-24
        // compat-remnant sweep; this client no longer calls it.)

        // Reflect "enabled" on the (ephemeral, peer-bound) snapshot.
        self.refresh_snapshot().await?;

        // Flip the deployment-wide mail subsystem on so the home box's MDA boots
        // and serves the relayed mail over LAN IMAP. Mirrors `enable_mail` — on
        // the private NAT axis the `mta_should_run` gate keeps the MTA *down*, so
        // only the MDA comes up (`deployment-home-with-public-relay.md`
        // § Plaintext-mode behavior). Admin-class; the home-relay user is admin
        // on their own home box, so this normally succeeds — swallow `Rejected`
        // (a non-admin self-provision), surface `Transient`.
        match self.nest.set_mail_enabled(true).await {
            Ok(()) | Err(NestError::Rejected(_)) => {}
            Err(e @ NestError::Transient(_)) => return Err(e.into()),
        }
        Ok(())
    }

    // ─── AddCredential: new credential on already-enabled actor ───

    async fn add_credential(
        &self,
        display_name: String,
        credential: Credential,
    ) -> Result<(), DispatchError> {
        let rows = self.mail.load_rows().await?;
        let mail = rows.config();
        let msek = mail.msek.clone().ok_or_else(|| {
            DispatchError::InvalidState("mail not enabled — use EnableMail first".into())
        })?;

        self.set_status(SettingsStatus::Syncing);

        // A marked id is spent: a re-mint under a revoked row's key would join
        // as revoked (*Bounded rows* → *The mail plane*).
        let credential_id = derive_credential_id(&display_name, &rows.spent_credential_ids());

        let msek_blob =
            wrap::seal_msek_under_credential(&msek, &self.actor_id, &credential_id, &credential)?;
        self.nest.provision_wrapped_mls_blob(msek_blob).await?;

        self.provision_submission_token(&credential_id, &credential)
            .await?;

        self.mail
            .put_credential(MailCredential {
                credential_id,
                display_name,
                kind: credential.kind().into(),
                secret: credential.as_bytes().to_vec().into(),
                created_at: Timestamp::now_secs() as u64,
                updated_at: Timestamp::default(),
                // The generation marker: this row's blob was just wrapped under
                // `msek` (`mail-credentials.md` § *The generation marker*).
                wrapped_under: Some(MsekFingerprint::of(&msek)),
                revoked_at_unix: None,
                burned: None,
            })
            .await?;
        self.refresh_snapshot().await?;
        Ok(())
    }

    // ─── RevokeCredential: soft revoke; deletions on nest, MSEK unchanged ───

    async fn revoke_credential(&self, credential_id: String) -> Result<(), DispatchError> {
        let mail = self.mail.load().await?;
        if !mail
            .credentials
            .iter()
            .any(|c| c.credential_id == credential_id)
        {
            return Err(DispatchError::UnknownCredential(credential_id));
        }

        self.set_status(SettingsStatus::Syncing);
        self.revoke_row(credential_id).await?;
        self.refresh_snapshot().await?;
        Ok(())
    }

    /// Revoke one credential: both nest-side blobs deleted FIRST, then the
    /// row's soft-revoke marker (which clears its generation and empties its
    /// secret in the same write). In that order a crash between leaves a live
    /// row the user can revoke again, never a marked row whose blobs still
    /// rest unrecorded — and a racing re-wrap that re-provisions them is
    /// caught by the heal's dual ([`MailRows::delete_owed`]).
    async fn revoke_row(&self, credential_id: String) -> Result<(), DispatchError> {
        self.nest
            .revoke_wrapped_mls_blob(self.actor_id, credential_id.clone())
            .await?;
        self.nest
            .revoke_wrapped_submission_token(self.actor_id, credential_id.clone())
            .await?;
        self.mail.revoke(credential_id).await?;
        Ok(())
    }

    // ─── DisableMail: turn email off; tear the mailbox down when email is all
    //     it served ───

    /// Per `mail-settings.md` § Disable mail. Deliberately does **not** call the
    /// Admin-scoped, deployment-wide `set_mail_enabled(false)`: that flag
    /// boots/tears the box-level s6 mail subsystem (`/data/imap-enabled`),
    /// shared by every mailbox — one user disabling their own mail must not
    /// turn mail off for everyone. The asymmetry with `enable_mail` (which
    /// *does* flip the subsystem on, admin-gated) is intentional.
    ///
    /// **The MSEK outlives the teardown.** The mail custody's state row keeps
    /// its MSEK present-wins (the key is irrecoverable, so no write can clear
    /// it — `config-dissolution.md` § *The mail plane*), so an email-only
    /// disable revokes every credential and turns email off, and the MSEK and
    /// its grace window stay, dormant: a later enable mints a fresh credential
    /// under the same key, and mail sealed before the disable stays openable
    /// (`mail-credentials.md`, the Disable-mail row).
    async fn disable_mail(&self) -> Result<(), DispatchError> {
        let mail = self.mail.load().await?;
        if !mail.is_mail_enabled() {
            return Err(DispatchError::InvalidState(
                "mail not enabled — nothing to disable".into(),
            ));
        }

        self.set_status(SettingsStatus::Syncing);

        if mail.caldav_enabled || mail.carddav_enabled {
            // ── CalDAV/CardDAV still need the shared mailbox material, so
            //    disabling *email* must NOT revoke the read credentials — that
            //    AEAD-unwrap blob also authenticates the DAV listeners and both
            //    DAV stores seal under the MSEK (`caldav-server.md` /
            //    `carddav-server.md` § Independent enablement: "no separate DAV
            //    credential"). Revoke only the outbound **submission tokens**
            //    (no more sending) and flip email off. ──
            for c in mail.live_credentials() {
                self.nest
                    .revoke_wrapped_submission_token(self.actor_id, c.credential_id.clone())
                    .await?;
            }
            let mail = self.mail.load().await?;
            self.mail
                .write_state(MailStateRow {
                    mail_enabled: Some(false),
                    ..MailStateRow::recreatable_of(&mail)
                })
                .await?;
            self.refresh_snapshot().await?;
            return Ok(());
        }

        // ── Email only (no DAV): full teardown — revoke every credential the
        //    fold still shows (burned rows included: their deletes are
        //    idempotent), each through `revoke_row`, so a mid-loop transient
        //    nest failure leaves the already-revoked rows marked and a
        //    re-dispatched `DisableMail` resumes from the rest. ──
        for c in &mail.credentials {
            self.revoke_row(c.credential_id.clone()).await?;
        }
        let mail = self.mail.load().await?;
        self.mail
            .write_state(MailStateRow {
                mail_enabled: Some(false),
                pending_rotation: None,
                ..MailStateRow::recreatable_of(&mail)
            })
            .await?;
        self.refresh_snapshot().await?;
        Ok(())
    }

    async fn set_serving_enabled(&self, enabled: bool) -> Result<(), DispatchError> {
        // No `MailConfig` change (the flag lives on the nest, not in the synced
        // mail custody); confirm with the nest first, then reflect in the snapshot
        // — non-optimistic, matching the other dispatches, so a failed flip
        // leaves the toggle showing its prior state plus the error.
        self.set_status(SettingsStatus::Syncing);
        self.nest.set_mail_serving_enabled(enabled).await?;
        let mut inner = self.inner.lock().unwrap();
        inner.snapshot.serving_enabled = enabled;
        inner.snapshot.status = SettingsStatus::Idle;
        inner.snapshot.error = None;
        Ok(())
    }

    // ─── crate-internal helpers used by rotation.rs ───

    pub(crate) fn nest(&self) -> &Arc<dyn NestClient> {
        &self.nest
    }
    /// The account's mail custody — what the rotation and the succession burn
    /// read and write, and what a caller holding the machine (the aftermath's
    /// grant re-mint) folds.
    pub fn mail_store(&self) -> &Arc<dyn MailStore> {
        &self.mail
    }

    /// **The generation marker's heal and its dual** (`mail-credentials.md`
    /// § Rotation and recovery → *The generation marker*), run best-effort at
    /// every hydrate by whichever device holds the plane: each live row the
    /// heal owes ([`MailRows::heal_owed`] — unstamped, or naming a grace
    /// generation) is re-wrapped under the current MSEK and marked; each
    /// marked row still naming a generation ([`MailRows::delete_owed`]) gets
    /// both idempotent nest deletes re-issued and its generation cleared.
    /// Every failure logs and leaves the row owed for the next hydrate.
    /// Whether any row moved.
    async fn heal_generation_markers(&self, rows: &MailRows) -> bool {
        let mut moved = false;
        if let Some(msek) = rows.state.as_ref().and_then(|s| s.msek.clone()) {
            let fingerprint = MsekFingerprint::of(&msek);
            for c in rows.heal_owed() {
                let Some(credential) = Credential::from_stored(&c.kind, c.secret.to_vec()) else {
                    tracing::warn!(
                        credential_id = %c.credential_id,
                        "mail heal: a credential of a kind this build does not know; the row stays owed"
                    );
                    continue;
                };
                let blob = match wrap::seal_msek_under_credential(
                    &msek,
                    &self.actor_id,
                    &c.credential_id,
                    &credential,
                ) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(error = %e, "mail heal: re-wrap failed; the row stays owed");
                        continue;
                    }
                };
                if let Err(e) = self.nest.provision_wrapped_mls_blob(blob).await {
                    tracing::warn!(error = %e, "mail heal: provision failed; the row stays owed");
                    continue;
                }
                match self
                    .mail
                    .mark_wrapped(c.credential_id.clone(), fingerprint)
                    .await
                {
                    Ok(m) => moved |= m,
                    Err(e) => {
                        tracing::warn!(error = %e, "mail heal: marker write failed; the row stays owed");
                    }
                }
            }
        }
        for c in rows.delete_owed() {
            let deleted = async {
                self.nest
                    .revoke_wrapped_mls_blob(self.actor_id, c.credential_id.clone())
                    .await?;
                self.nest
                    .revoke_wrapped_submission_token(self.actor_id, c.credential_id.clone())
                    .await
            };
            if let Err(e) = deleted.await {
                tracing::warn!(error = %e, "mail heal: a marked row's deletes failed; it stays owed");
                continue;
            }
            match self
                .mail
                .put_credential(MailCredential {
                    wrapped_under: None,
                    ..c.clone()
                })
                .await
            {
                Ok(m) => moved |= m,
                Err(e) => {
                    tracing::warn!(error = %e, "mail heal: clearing a marked row's generation failed");
                }
            }
        }
        moved
    }

    /// Re-read the mail custody and re-snapshot from it — every mutation's
    /// last step, so the page renders what the rows now hold (another
    /// device's concurrent change included), not what this one wrote.
    pub(crate) async fn refresh_snapshot(&self) -> Result<(), DispatchError> {
        let rows = self.mail.load_rows().await?;
        self.set_snapshot_from(&rows);
        Ok(())
    }

    /// Re-read just the credential rows from the mail custody — no nest call
    /// and no generation heal, unlike [`Self::hydrate`]. For a surface that
    /// lists the app passwords without owning the mail page (the Connected
    /// apps roster re-reads on every visit and on every consent push). The
    /// nest-sourced snapshot fields keep the values the last `hydrate`
    /// fetched.
    pub async fn refresh_credentials(&self) -> Result<(), DispatchError> {
        self.refresh_snapshot().await
    }

    pub(crate) fn mint_submission_token(
        &self,
        credential_id: &str,
    ) -> Result<SubmissionToken, DispatchError> {
        let now = Timestamp::now_secs() as u64;
        let unsigned = SubmissionToken {
            actor_id: self.actor_id.to_vec(),
            credential_id: credential_id.to_string(),
            issued_at: now,
            expires_at: now.saturating_add(SUBMISSION_TOKEN_LIFETIME_SECS),
            max_recipients: DEFAULT_MAX_RECIPIENTS,
            max_messages_per_day: DEFAULT_MAX_MESSAGES_PER_DAY,
            // Placeholder; replaced inside `SubmissionToken::sign`.
            user_sig: ByteBuf::from(vec![0u8; fauna_mls::wrapped_blob::SIGNATURE_LEN]),
        };
        Ok(self.signer.sign_submission_token(unsigned)?)
    }

    pub(crate) fn set_snapshot_from(&self, rows: &MailRows) {
        let mail = rows.config();
        // The rotation's remaining credentials are derived from the rows'
        // generation markers under the sentinel's incoming MSEK
        // (`MailRows::owed_rewrap`), never stored.
        let remaining: Option<Vec<String>> = mail.pending_rotation.as_ref().map(|p| {
            rows.owed_rewrap(&p.new_msek)
                .into_iter()
                .map(|c| c.credential_id.clone())
                .collect()
        });
        // `serving_enabled`, `serves_webdav_set` and the displayed
        // `mua.caldav_port` are nest-sourced values NOT part of the synced
        // `MailConfig`, so carry the current values forward across a
        // config-driven re-snapshot (`hydrate` refreshes all three from the nest
        // separately). For a registrable-domain box the carried port is always
        // the canonical 443 (the admin port never overrides it —
        // `MuaInstructions::apply_admin_caldav_port`); for a local-target box it
        // is the last admin-set port the machine fetched.
        let (serving_enabled, serves_webdav_set, prior_caldav_port) = {
            let g = self.inner.lock().unwrap();
            (
                g.snapshot.serving_enabled,
                g.snapshot.serves_webdav_set,
                g.snapshot.mua.caldav_port,
            )
        };
        let mut mua = self.mua.clone();
        mua.apply_admin_caldav_port(prior_caldav_port);
        let snapshot = MailSettingsSnapshot {
            // "Mail enabled" = **email** on, NOT merely "has an MSEK" — a
            // CalDAV-only actor holds the shared MSEK but must render mail OFF
            // (`MailConfig::is_mail_enabled`; § Independent enablement).
            enabled: mail.is_mail_enabled(),
            // CalDAV (calendar) enablement, independent of email — drives the
            // page to render the shared credential-management section even when
            // email is off, so a CalDAV-only actor can obtain + manage its bridge
            // password (`mail-settings.md` § CalDAV-only mailbox).
            caldav_enabled: mail.caldav_enabled,
            // CardDAV (contacts) enablement, the contacts sibling — same
            // credential-section predicate role for an address-book-only actor
            // (`carddav-server.md` § Independent enablement).
            carddav_enabled: mail.carddav_enabled,
            serves_webdav_set,
            // The one place the credential-management reachability predicate is
            // evaluated — every app reads the field (priority #2/#4), so a
            // future DAV sibling widens it here, not in 6 UIs.
            credential_management_reachable: mail.is_mail_enabled()
                || mail.caldav_enabled
                || mail.carddav_enabled
                || serves_webdav_set,
            serving_enabled,
            // A credential of a kind this build does not name is kept in the
            // sealed config untouched and offered by no row here.
            credentials: mail
                .credentials
                .iter()
                .filter_map(|c| Some((c, crate::state::CredentialKind::of_stored(&c.kind)?)))
                .map(|(c, kind)| MailCredentialSummary {
                    credential_id: c.credential_id.clone(),
                    display_name: c.display_name.clone(),
                    kind,
                    created_at: c.created_at,
                    // Concrete per-credential MUA username (only `{handle}` left
                    // for the renderer), resolved in shared Rust from the mail
                    // domain + the default-bare rule.
                    mua_username: self.mua.username_for(&c.credential_id),
                    // A burned row still renders — it is the user's list of
                    // which mail apps to set up again — but it must never
                    // render as usable: the username below it works with no
                    // password that exists any more.
                    revoked: c.burned.is_some(),
                })
                .collect(),
            status: match &remaining {
                Some(ids) => SettingsStatus::RotationInProgress {
                    credentials_remaining: ids.len() as u64,
                },
                None => SettingsStatus::Idle,
            },
            pending_rotation: remaining.map(|credentials_remaining| PendingRotationStatus {
                credentials_remaining,
            }),
            mua,
            error: None,
        };
        self.inner.lock().unwrap().snapshot = snapshot;
    }

    pub(crate) fn set_status(&self, status: SettingsStatus) {
        let mut inner = self.inner.lock().unwrap();
        inner.snapshot.status = status;
        inner.snapshot.error = None;
    }

    pub(crate) fn set_error(&self, msg: String) {
        let mut inner = self.inner.lock().unwrap();
        crate::state::set_snapshot_error(&mut inner.snapshot.error, msg);
        // A failure ends the dispatch, not a rotation it interrupted: with the
        // sentinel still set the page keeps reading "rotation in progress" beside
        // its resume banner — the status a fresh hydrate derives from the stored
        // sentinel — never "All up to date" (`ui/mail-settings.md` § Status
        // indicator).
        inner.snapshot.status = match &inner.snapshot.pending_rotation {
            Some(pending) => SettingsStatus::RotationInProgress {
                credentials_remaining: pending.credentials_remaining.len() as u64,
            },
            None => SettingsStatus::Idle,
        };
    }
}

// FFI surface. Mirrors `BridgeApprovalMachine`'s export/private split: `new` +
// the private/`pub(crate)` helpers stay in the plain `impl` above; the per-app
// UI (windows/macos/ios/android via fauna-ffi, web via fauna-wasm, linux native)
// renders `snapshot()` and drives `hydrate()` / `dispatch()`.
#[cfg_attr(feature = "uniffi", uniffi::export)]
impl MailSettingsMachine {
    /// Read-only snapshot. Cheap clone.
    pub fn snapshot(&self) -> MailSettingsSnapshot {
        fauna_core::clone_locked(&self.inner, |i| &i.snapshot)
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl MailSettingsMachine {
    /// Pull the latest mail custody and refresh the in-memory
    /// snapshot. The per-app UI calls this once at page mount
    /// and whenever the account plane's mail rows change. Idempotent.
    pub async fn hydrate(&self) -> Result<(), DispatchError> {
        // Both reads first (either may fail → early-return without a partial
        // snapshot update): the synced mail custody and the per-actor
        // serving flag (a caller-scoped nest read; default-on for a never-set
        // actor). `serving_enabled` isn't in `MailConfig`, so it's set after
        // `set_snapshot_from` (which carries the prior value forward).
        let rows = self.mail.load_rows().await?;
        let serving_enabled = self.nest.get_mail_serving_enabled().await?;
        // Best-effort: on a transport error the displayed port falls back
        // to the for-node-URL default (443 / `DEFAULT_CALDAV_PORT`) rather than
        // failing the whole page hydrate.
        let caldav_port = self.nest.get_caldav_port().await.ok();
        // Best-effort for the same reason: a transport hiccup on this pure
        // read must
        // not fail the whole page hydrate — an unknown serve state reads as "not
        // serving", which merely hides the WebDAV URL row.
        let serves_webdav_set = self.nest.serves_any_webdav_set().await.unwrap_or(false);
        // Best-effort: the generation marker's heal and its delete dual — a
        // failure logs and leaves the rows owed for the next hydrate.
        let rows = match self.heal_generation_markers(&rows).await {
            true => self.mail.load_rows().await?,
            false => rows,
        };
        self.set_snapshot_from(&rows);
        {
            let mut inner = self.inner.lock().unwrap();
            inner.snapshot.serving_enabled = serving_enabled;
            inner.snapshot.serves_webdav_set = serves_webdav_set;
            // Recompute the predicate: `set_snapshot_from` folded in the
            // *carried-forward* serve state, which the fresh nest read may have
            // just changed (first-ever hydrate, or a set served since the last).
            inner.snapshot.credential_management_reachable = inner.snapshot.enabled
                || inner.snapshot.caldav_enabled
                || inner.snapshot.carddav_enabled
                || serves_webdav_set;
            if let Some(port) = caldav_port {
                inner.snapshot.mua.apply_admin_caldav_port(port);
            }
        }
        Ok(())
    }

    /// Slide the content-sealing-epoch publication horizon forward against
    /// the connected nest — `[e_now, e_now + MAIL_EPOCH_PUBLISH_HORIZON]`,
    /// idempotent upsert (design 2026-07-18 § 3: "refreshes the horizon on
    /// every connect"). Every app should call this once per successful
    /// (re)connect so the schedule keeps sliding forward while the user's
    /// device is online, rather than going stale past the horizon (§ 3
    /// step 2's honest degradation). A no-op when mail isn't enabled (no
    /// MSEK).
    pub async fn refresh_epoch_schedule(&self) -> Result<(), DispatchError> {
        let mail = self.mail.load().await?;
        let Some(msek) = mail.msek.as_ref() else {
            return Ok(());
        };
        let epoch_keys = Self::epoch_seal_keys_to_publish(msek);
        let (_, recipient_pubkey) = derive_recipient_hpke_keypair(msek);
        let mlkem_ek = Self::recipient_mlkem_ek_to_publish(msek);
        // Rewrite the snapshot BEFORE republishing the pubkey, the same order
        // first-enable and rotation use: the decapsulation key must be on the
        // nest before anything can seal to the encapsulation key. This is an
        // unconditional ordering invariant, by construction — without it, this
        // refresh publishes an ek whose dk the MDA does not hold, and every
        // collection the MDA subsequently seals becomes permanently unopenable
        // by the very session that sealed it. Unconditional rather than
        // conditional on a read-back: the blob is small, the write is an
        // idempotent atomic replace, and a pairing that holds by construction
        // is worth one extra write per connect.
        self.provision_snapshot_for(&Self::snapshot_mseks(&mail))
            .await?;
        self.nest
            .provision_recipient_mls_pubkey(
                self.actor_id,
                recipient_pubkey,
                mlkem_ek,
                Some(epoch_keys),
            )
            .await?;
        Ok(())
    }

    /// Recover a credential's secret (the PLAIN password or OAUTHBEARER token)
    /// from the account's **own** mail custody so the user can (re)configure a
    /// MUA without revoking + re-adding the credential. The secret is held
    /// client-side (the credential's `fauna.state.mail` row, sealed fleet-only
    /// — it must be, for unattended rotation), so this is a pure read. It is deliberately **not** in `MailSettingsSnapshot` (the passively
    /// rendered state never carries secrets — `state.rs`); this explicit,
    /// on-demand accessor is the only path that surfaces it, mirroring the
    /// shown-once reveal at credential-add time. Returns [`SecretString`]
    /// (zeroizing, redacted `Debug`) so the value stays in the disciplined type
    /// up to the per-app renderer that displays/copies it. `UnknownCredential`
    /// if no row matches `credential_id`.
    pub async fn reveal_credential_secret(
        &self,
        credential_id: String,
    ) -> Result<SecretString, DispatchError> {
        let mail = self.mail.load().await?;
        mail.credentials
            .iter()
            .find(|c| c.credential_id == credential_id)
            .map(|c| SecretString::new(String::from_utf8_lossy(c.secret.as_slice()).into_owned()))
            .ok_or(DispatchError::UnknownCredential(credential_id))
    }

    /// Enable mail for this actor with a **freshly generated** strong password,
    /// returning that password **once** so the caller surfaces it to the user
    /// (one-time reveal — the passively-rendered snapshot never carries
    /// secrets, mirroring `reveal_credential_secret`).
    ///
    /// This is the shared "auto-mint mailbox with a generated password" path.
    /// The onboarding auto-complete launch glue (`onboarding.md` § Enable-email
    /// at claim) and new-user auto-enable both
    /// call this instead of bare `set_mail_enabled`, so the box (and every new
    /// user) gets a working mailbox with no hand-entered password. It wraps the
    /// same `enable_mail` path as the manual mail-settings enable — same
    /// idempotency guard (errors if mail is already enabled, `onboarding.md:142`)
    /// — using a `Plain` credential whose secret is the generated password.
    pub async fn enable_mail_with_generated_password(
        &self,
        display_name: String,
    ) -> Result<SecretString, DispatchError> {
        let password = crate::password_gen::generate_bridge_password();
        let credential = Credential::Plain(Zeroizing::new(password.as_str().as_bytes().to_vec()));
        self.enable_mail(display_name, credential).await?;
        Ok(password)
    }

    /// Auto-enable mail for a freshly-registered user at their **first
    /// authenticated client setup**. Mints a mailbox with a generated password
    /// (returned **once** for the one-time reveal, via
    /// [`Self::enable_mail_with_generated_password`]) **iff** the deployment has
    /// mail on, the auto-enable-for-new-users policy is on, and the actor has no
    /// mailbox yet. Returns `Ok(None)` — *not* an error — when any gate fails
    /// (policy off, or a mailbox already exists), so the per-app launch glue
    /// can call it unconditionally on the non-admin first-setup path without
    /// special-casing the already-enabled actor.
    ///
    /// The two policy flags are passed in (the [`NestClient`] seam carries no
    /// `setup.status` read); the caller reads them from `fauna.setup.status`
    /// (`SetupStatusReply.{email_enabled, auto_enable_mail_for_new_users}`).
    /// `mail-credentials.md` § Auto-enable for new users.
    pub async fn auto_enable_mail_for_new_user(
        &self,
        deployment_mail_enabled: bool,
        auto_enable_policy: bool,
        display_name: String,
    ) -> Result<Option<SecretString>, DispatchError> {
        if !deployment_mail_enabled || !auto_enable_policy {
            return Ok(None);
        }
        // Idempotency gate (3): never re-mint an existing mailbox. The trigger
        // site (fresh-onboarding launch glue) already guarantees once-per-user;
        // this is the belt-and-suspenders check that also keeps the call a
        // graceful no-op rather than an `InvalidState` error.
        // An MSEK a teardown left dormant counts: the user disabled mail on
        // purpose, and a new-user auto-enable must not turn it back on.
        if self.mail.load().await?.msek.is_some() {
            return Ok(None);
        }
        Ok(Some(
            self.enable_mail_with_generated_password(display_name)
                .await?,
        ))
    }

    /// The "auto-mint a CalDAV mailbox with a generated password" path — the
    /// calendar twin of [`Self::enable_mail_with_generated_password`]. The
    /// per-app onboarding launch glue calls this (when the user enabled CalDAV
    /// but not email) so a CalDAV-only user gets the shared MSEK their calendar
    /// store seals under, with no hand-entered password. Uses a `Plain` credential
    /// whose secret is the generated bridge password (the same one a stock CalDAV
    /// client AUTHs with — § Authentication). Returns the password for the
    /// one-time reveal. Idempotent (delegates to [`Self::enable_caldav_mailbox`],
    /// a no-op if the shared MSEK already exists, so the launch glue can call it
    /// unconditionally on a CalDAV-enable).
    pub async fn enable_caldav_mailbox_with_generated_password(
        &self,
        display_name: String,
    ) -> Result<SecretString, DispatchError> {
        let password = crate::password_gen::generate_bridge_password();
        let credential = Credential::Plain(Zeroizing::new(password.as_str().as_bytes().to_vec()));
        self.enable_caldav_mailbox(display_name, credential).await?;
        Ok(password)
    }

    /// Enable a CalDAV mailbox under a **caller-provided** PLAIN bridge password —
    /// the password twin of [`Self::enable_caldav_mailbox_with_generated_password`].
    /// The e2e uses it to mint a CalDAV-only organizer mailbox whose bridge
    /// password the test knows, so a stock CalDAV client can AUTH with it (the
    /// generated-password variant drops the secret, leaving it reachable only via
    /// the mail-settings reveal UI, which a CalDAV-only actor doesn't render). It
    /// is also the shape a future "choose your own CalDAV password" UI would call.
    /// Idempotent (delegates to [`Self::enable_caldav_mailbox`], a no-op once the
    /// shared MSEK exists).
    pub async fn enable_caldav_mailbox_with_password(
        &self,
        display_name: String,
        password: String,
    ) -> Result<(), DispatchError> {
        let credential = Credential::Plain(Zeroizing::new(password.into_bytes()));
        self.enable_caldav_mailbox(display_name, credential).await
    }

    /// The "auto-mint a CardDAV mailbox with a generated password" path — the
    /// contacts twin of [`Self::enable_caldav_mailbox_with_generated_password`].
    /// The per-app onboarding launch glue calls this (when the user enabled
    /// CardDAV but neither email nor CalDAV) so a CardDAV-only user gets the
    /// shared MSEK their address-book store seals under, with no hand-entered
    /// password. Returns the password for the one-time reveal. Idempotent
    /// (delegates to [`Self::enable_carddav_mailbox`], reduced to recording the
    /// per-actor flag once the shared MSEK exists, so the launch glue can call
    /// it unconditionally on a CardDAV-enable).
    pub async fn enable_carddav_mailbox_with_generated_password(
        &self,
        display_name: String,
    ) -> Result<SecretString, DispatchError> {
        let password = crate::password_gen::generate_bridge_password();
        let credential = Credential::Plain(Zeroizing::new(password.as_str().as_bytes().to_vec()));
        self.enable_carddav_mailbox(display_name, credential)
            .await?;
        Ok(password)
    }

    /// Enable a CardDAV mailbox under a **caller-provided** PLAIN bridge
    /// password — the password twin of
    /// [`Self::enable_carddav_mailbox_with_generated_password`], mirroring the
    /// CalDAV pair (the e2e mints a CardDAV-only mailbox whose bridge password a
    /// stock CardDAV client AUTHs with). Idempotent (delegates to
    /// [`Self::enable_carddav_mailbox`]).
    pub async fn enable_carddav_mailbox_with_password(
        &self,
        display_name: String,
        password: String,
    ) -> Result<(), DispatchError> {
        let credential = Credential::Plain(Zeroizing::new(password.into_bytes()));
        self.enable_carddav_mailbox(display_name, credential).await
    }

    /// Whether a spam-model write would take the **sealed client-side** path —
    /// the cheap pre-check a surface runs *before* paying for a train-text
    /// fetch (e.g. `fauna.posts.get` for a moderation-queue row correction —
    /// [`Self::train_moderation_correction`] — where only the model half needs
    /// the plaintext body). Reads the same signals as
    /// [`Self::apply_spam_model_write`]'s dispatch: the
    /// [`SPAM_MODEL_SEALED_AT_REST`] feature-presence token (co-design
    /// § Revision history 2026-07-06 C/D) + MSEK presence (mail enabled). A
    /// `true` here can still race a concurrent `disable_mail` into a
    /// `ServerPath` outcome — callers branch on the write's outcome, this is
    /// only the fetch-avoidance hint.
    pub async fn sealed_spam_write_available(&self) -> Result<bool, DispatchError> {
        let sealed_supported = self.nest.nest_supports(SPAM_MODEL_SEALED_AT_REST).await?;
        let mail_enabled = self.mail.load().await?.msek.is_some();
        Ok(matches!(
            dispatch_for(sealed_supported, mail_enabled),
            SealedWriteDispatch::Sealed
        ))
    }

    /// Apply one **train** event to the caller's per-user spam model on the
    /// client — the FFI/wasm-safe façade over
    /// [`Self::apply_spam_model_write`]`(`[`ModelWriteOp::Train`]`)` for the
    /// per-app surfaces (the moderation-queue `train-correction-button`,
    /// mark-as-spam / not-spam): fetch sealed → unwrap → train on `text` →
    /// re-seal (hybrid-aware) → write back opaque via
    /// `fauna.bridges.put_spam_model`. `text` is the plaintext the client
    /// holds (a post-decrypt message body, or a body fetched by `content_id` —
    /// co-design § 3). Returns [`SpamModelClientWrite`]: `sealed == false` ⇒ no
    /// sealed write was possible and the model half was skipped. A
    /// moderation-queue *server* row's correction goes through
    /// [`Self::train_moderation_correction`] instead, which also runs the nest
    /// half (`fauna.moderation.train`).
    pub async fn train_spam_model_client(
        &self,
        text: String,
        is_spam: bool,
    ) -> Result<SpamModelClientWrite, DispatchError> {
        let label = if is_spam {
            SpamLabel::Spam
        } else {
            SpamLabel::Ham
        };
        // A moderation-queue / social train writes no audit row (its record is
        // the nest-side report capture of `fauna.moderation.train`). A
        // mail-surface train that *does* seal an `Insert` row uses a dedicated
        // façade carrying the message metadata (build-item 3 write side); this
        // façade stays model-only.
        match self
            .apply_spam_model_write(ModelWriteOp::Train { text, label }, None)
            .await?
        {
            SpamModelWriteOutcome::Sealed { sample_count, .. }
            | SpamModelWriteOutcome::Duplicate { sample_count } => Ok(SpamModelClientWrite {
                sealed: true,
                sample_count,
            }),
            SpamModelWriteOutcome::ServerPath => Ok(SpamModelClientWrite {
                sealed: false,
                sample_count: 0,
            }),
        }
    }

    /// Apply one **mail-surface train** event — the live [`HistoryWrite::Insert`]
    /// consumer, the mail-surface twin of [`Self::train_spam_model_client`]. A
    /// "mark as spam" / "mark as not spam" gesture on a conversation/mail message
    /// carries the message's `message_id` + `mailbox` + `subject` here so the
    /// client-side train **also seals an audit row** to the actor's own key
    /// ([`SpamHistoryOp::Insert`]) atomically with the model re-seal — unlike the
    /// model-only [`Self::train_spam_model_client`] (the moderation-queue *social*
    /// train, which writes no history row). The sealed row is what the `mail-spam`
    /// page's training-history list renders (`{subject} · {mailbox}`) and the
    /// per-row undo replays (client-side, since a sealed row is nest-opaque).
    ///
    /// - `text` — the trained message's plaintext body (the one plaintext position
    ///   in encrypted mode; the client already holds it post-decrypt).
    /// - `is_spam` — `true` ⇒ [`SpamLabel::Spam`], `false` ⇒ [`SpamLabel::Ham`].
    /// - `message_id` — the message's stable id, stored **opaque** as the row's
    ///   reference (never decoded nest-side).
    /// - `mailbox` — plaintext display metadata (`INBOX` / `Junk` / …). A
    ///   conversation message has no IMAP mailbox — the caller passes a sensible
    ///   default (`INBOX` for received content).
    /// - `subject` — the message subject, **sealed by the machine** to the actor's
    ///   own key (the caller passes plaintext; it never leaves as plaintext).
    ///   **When blank** (a received conversation message typically has no subject
    ///   line — `MessageSnapshot::subject_line` is `Some` only on a subject-change
    ///   message) the machine derives a body snippet ([`spam_subject_snippet`]) so
    ///   the sealed row still renders recognizably. This is the single shared home
    ///   for the fallback: every app passes the raw subject and none replicates
    ///   it (priorities #2/#4).
    ///
    /// The source is [`TrainingSource::ManualOther`](WireTrainingSource::ManualOther)
    /// — a first-party button, not an IMAP `\Junk` flag/move. Returns
    /// [`SpamModelClientWrite`]: `sealed == false` ⇒ no sealed write was possible
    /// (nest lacks [`SPAM_MODEL_SEALED_AT_REST`], or mail isn't enabled — then
    /// the actor has no per-user model at all) and nothing was written; there is
    /// no server-side train to fall back to.
    pub async fn train_spam_model_client_mail(
        &self,
        text: String,
        is_spam: bool,
        message_id: Vec<u8>,
        mailbox: String,
        subject: String,
    ) -> Result<SpamModelClientWrite, DispatchError> {
        let label = if is_spam {
            SpamLabel::Spam
        } else {
            SpamLabel::Ham
        };
        // A received conversation message usually carries no subject line, so the
        // caller may pass an empty subject; derive a body snippet here — the single
        // shared home for the fallback (priorities #2/#4), so no client shell
        // replicates it.
        let subject = if subject.trim().is_empty() {
            spam_subject_snippet(&text)
        } else {
            subject
        };
        match self
            .apply_spam_model_write(
                ModelWriteOp::Train { text, label },
                Some(HistoryWrite::Insert {
                    message_id,
                    mailbox,
                    subject,
                    source: WireTrainingSource::ManualOther,
                }),
            )
            .await?
        {
            // A `Duplicate` is the sealed path with nothing new to teach (the
            // one-lesson rule, `mail-spam.md` § 3): the caller must NOT fall
            // back to the server train, and the count on record is unchanged.
            SpamModelWriteOutcome::Sealed { sample_count, .. }
            | SpamModelWriteOutcome::Duplicate { sample_count } => Ok(SpamModelClientWrite {
                sealed: true,
                sample_count,
            }),
            SpamModelWriteOutcome::ServerPath => Ok(SpamModelClientWrite {
                sealed: false,
                sample_count: 0,
            }),
        }
    }

    /// The moderation-queue **training correction** for a *server* row (the
    /// `train-correction-button`; `content-moderation-and-ranking.md` § Sealing
    /// tier-1, `mail-spam.md` § Encrypted-mode interaction) — the ONE shared flow
    /// every app runs, in two halves:
    ///
    /// 1. **Model half** (client-side, only when a sealed write is possible —
    ///    [`Self::sealed_spam_write_available`]): `fauna.posts.get` →
    ///    `Post::body_text` → [`Self::train_spam_model_client`] (fetch sealed →
    ///    unwrap → train → re-seal → `put_spam_model`). A body-fetch miss or an
    ///    empty body skips this half — the nest half below reports the canonical
    ///    error for an unreadable post.
    /// 2. **Nest half**, ALWAYS: `fauna.moderation.train{content_id, verdict}` —
    ///    the read gate + the report capture (`report-sharing.md` § Report
    ///    capture). It trains nothing (the model rests sealed); its error is the
    ///    flow's error.
    ///
    /// `is_spam` picks the verdict (`"spam"` / `"ham"`; the queue's correction
    /// is `false` — "this was a false positive"). Returns the model half's
    /// [`SpamModelClientWrite`] (`sealed == false` ⇒ the model half was skipped).
    /// A model-half failure (sealed fetch/unwrap/put) does not stop the nest
    /// half — the correction is still reported — but is returned as the flow's
    /// error once the nest half succeeded, so the surface shows the train failed.
    pub async fn train_moderation_correction(
        &self,
        content_id: String,
        is_spam: bool,
    ) -> Result<SpamModelClientWrite, DispatchError> {
        let mut model_half = SpamModelClientWrite {
            sealed: false,
            sample_count: 0,
        };
        let mut model_err = None;
        // The pre-check avoids paying for the body fetch when no sealed write
        // is possible; a failed probe reads as "not possible" (the nest half
        // still runs and surfaces any real connectivity error).
        if self.sealed_spam_write_available().await.unwrap_or(false)
            && let Ok(Some(text)) = self.nest.fetch_post_body_text(&content_id).await
            && !text.is_empty()
        {
            match self.train_spam_model_client(text, is_spam).await {
                Ok(written) => model_half = written,
                Err(e) => model_err = Some(e),
            }
        }
        let verdict = if is_spam { "spam" } else { "ham" };
        self.nest.moderation_train(&content_id, verdict).await?;
        match model_err {
            Some(e) => Err(e),
            None => Ok(model_half),
        }
    }

    /// Single entry point for the per-app UI. Each action runs
    /// to completion (or sets an error on the snapshot) before
    /// returning.
    pub async fn dispatch(&self, action: MailSettingsAction) -> Result<(), DispatchError> {
        let result = match action {
            MailSettingsAction::EnableMail {
                display_name,
                kind,
                secret,
            } => {
                self.enable_mail(
                    display_name,
                    Credential::from_kind_bytes(kind, secret.to_vec()),
                )
                .await
            }
            MailSettingsAction::AddCredential {
                display_name,
                kind,
                secret,
            } => {
                self.add_credential(
                    display_name,
                    Credential::from_kind_bytes(kind, secret.to_vec()),
                )
                .await
            }
            MailSettingsAction::RevokeCredential { credential_id } => {
                self.revoke_credential(credential_id).await
            }
            MailSettingsAction::DisableMail => self.disable_mail().await,
            MailSettingsAction::StartRotation {
                excluded_credentials,
            } => rotation::start_rotation(self, &excluded_credentials).await,
            MailSettingsAction::ResumeRotation => rotation::resume_rotation(self).await,
            MailSettingsAction::SetServingEnabled { enabled } => {
                self.set_serving_enabled(enabled).await
            }
            MailSettingsAction::ProvisionRelayMailbox => self.provision_relay_mailbox().await,
        };
        if let Err(e) = &result {
            self.set_error(e.to_string());
        }
        result
    }
}

/// Which DAV protocol a DAV-only mailbox enable is recording — the single
/// axis on which [`MailSettingsMachine::enable_caldav_mailbox`] and
/// [`MailSettingsMachine::enable_carddav_mailbox`] differ (the per-actor
/// `MailConfig` flag). Private: the public surface is the two named methods.
#[derive(Debug, Clone, Copy)]
enum DavProtocol {
    Caldav,
    Carddav,
}

impl DavProtocol {
    fn enabled_in(self, mail: &MailConfig) -> bool {
        match self {
            Self::Caldav => mail.caldav_enabled,
            Self::Carddav => mail.carddav_enabled,
        }
    }

    fn set_enabled(self, state: &mut MailStateRow) {
        match self {
            Self::Caldav => state.caldav_enabled = true,
            Self::Carddav => state.carddav_enabled = true,
        }
    }
}

/// Mint a fresh 32-byte MSEK from the OS RNG.
///
/// The MSEK is the root the whole Path B tree hangs off, so it is the strongest
/// member of the three-way `fresh_*` minter family — with
/// `fauna_client_folders::fresh_content_key` and
/// `fauna_client_subscriptions::fresh_period_key`. All three now delegate to
/// [`fauna_core::secret::fresh_secret_32`], which owns the CSPRNG call and the
/// non-`Copy` *Carrier shape* rule they used to each restate
/// (`key-material-hierarchy.md` § Plaintext key lifetime on bridges).
pub(crate) fn fresh_msek() -> Zeroizing<[u8; 32]> {
    fauna_core::secret::fresh_secret_32()
}

/// Compile-time pin that [`fresh_msek`] keeps handing its output out non-`Copy`.
/// Reverting the return type to `[u8; 32]` fails the *build* here rather than
/// only wherever a call site happens to be strictly typed
/// (`key-material-hierarchy.md` § Carrier shape → *Pinned at compile time*).
const _FRESH_MSEK_IS_NOT_COPY: fn() -> Zeroizing<[u8; 32]> = fresh_msek;

/// A short, single-line subject stand-in for a mail-surface spam train whose
/// message carries no subject line — the common case, since a conversation
/// `MessageSnapshot` sets `subject_line` only on a subject-change message. The
/// first ~60 chars of the body, whitespace-collapsed and ellipsized when
/// truncated, so the sealed training-history row renders recognizably as
/// `{subject} · {mailbox}`. This is the **single shared home** for the fallback
/// ([`MailSettingsMachine::train_spam_model_client_mail`] applies it when the
/// caller passes a blank subject), so no per-app shell replicates it.
fn spam_subject_snippet(body: &str) -> String {
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut snippet: String = collapsed.chars().take(60).collect();
    if collapsed.chars().count() > 60 {
        snippet.push('…');
    }
    snippet
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeMailStore, FakeNestClient, FakeSigner};

    const ACTOR: [u8; 32] = [0x11; 32];
    const SIGNER_SEED: [u8; 32] = [0x22; 32];

    fn machine() -> MailSettingsMachine {
        MailSettingsMachine::new(
            ACTOR,
            Arc::new(FakeNestClient::new()),
            Arc::new(
                fauna_client_config::test_helpers::FakeSuccessionLedgerStore::empty(
                    fauna_core::identity::ActorId(ACTOR),
                ),
            ),
            Arc::new(FakeMailStore::empty()),
            Arc::new(FakeSigner::new(SIGNER_SEED)),
            MuaInstructions::placeholder(),
        )
    }

    // Pin the documented default send-policy values (spec § Submission token:
    // 30-day window, default 100 recipients / 1000 messages-per-day; the
    // `token_refresh` re-mint cadence is stated relative to the 30-day window).
    // A drift here silently re-policies every app's minted submission token.
    #[test]
    fn submission_token_policy_constants_match_spec() {
        assert_eq!(SUBMISSION_TOKEN_LIFETIME_SECS, 30 * 86_400);
        assert_eq!(DEFAULT_MAX_RECIPIENTS, 100);
        assert_eq!(DEFAULT_MAX_MESSAGES_PER_DAY, 1000);
    }

    // Pin the constant→field WIRING in `mint_submission_token` — a field-swap or
    // wrong-constant mutation the value-only test above would miss. The minted
    // token must carry the actor + the passed credential id, a 30-day expiry
    // measured from `issued_at` (both off the same `now`, so exact), and the
    // default send-policy caps. This is the MTA's per-message authorization, so
    // every mail enable / add-credential path (incl. the onboarding mint) relies
    // on it.
    #[test]
    fn mint_submission_token_wires_actor_credential_and_policy() {
        let m = machine();
        let token = m
            .mint_submission_token("cred-abc")
            .expect("mint submission token");

        assert_eq!(token.actor_id, ACTOR.to_vec());
        assert_eq!(token.credential_id, "cred-abc");
        assert_eq!(
            token.expires_at,
            token.issued_at + SUBMISSION_TOKEN_LIFETIME_SECS
        );
        assert_eq!(token.max_recipients, DEFAULT_MAX_RECIPIENTS);
        assert_eq!(token.max_messages_per_day, DEFAULT_MAX_MESSAGES_PER_DAY);
    }

    // The mail-surface train's subject fallback lives here (shared Rust), not in
    // any client shell: a blank subject derives a whitespace-collapsed, 60-char
    // ellipsized body snippet; a bounded body is used verbatim. A drift here would
    // change every app's sealed training-history row display.
    #[test]
    fn spam_subject_snippet_derives_a_bounded_body_stand_in() {
        assert_eq!(
            spam_subject_snippet("  Cheap   pills   here  "),
            "Cheap pills here"
        );
        assert_eq!(spam_subject_snippet(""), "");
        let long = "word ".repeat(40); // 199 chars collapsed — over the 60 cap
        let snip = spam_subject_snippet(&long);
        assert_eq!(snip.chars().count(), 61, "60 chars + the ellipsis");
        assert!(snip.ends_with('…'));
    }
}
