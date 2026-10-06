//! An in-memory stand-in for one nest's recovery plane.
//!
//! Deliberately **faithful, not permissive**: it runs the same
//! `fauna_core::recovery` verification the real handlers run
//! (`SignedRecoveryKeyRegistration::verify`, `verify_seed_alone`,
//! `EscrowChallenge::verify`, `verify_succession_against_chain`), enforces the
//! same single-use nonces, and reproduces the lifecycle rule that makes the
//! client's escrow re-put load-bearing — **the escrow row is deleted on any
//! registration that changes the registered pubkey, and inside the succession
//! transaction** (`identity-succession.md:51`).
//!
//! That fidelity is the whole point. A fake that merely records calls would
//! green-light a ceremony that signs the wrong bytes, orders its calls wrongly,
//! or skips the re-put — which are exactly the mistakes these tests exist to
//! catch.

// Shared by more than one test binary, each of which uses part of it.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Mutex;

use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_core::identity::ActorId;
use fauna_core::recovery::{
    ChainHead, EscrowChallenge, RECOVERY_REPLACE_GRACE_SECS, ReplacementVeto,
    SignedIdentitySuccession, SignedRecoveryKeyRegistration, verify_succession_against_chain,
};
use fauna_protocol::recovery as wire;
use fauna_protocol::{ByteBuf, RpcError, RpcErrorClass, RpcRequester};

/// The error type the fake refuses with — carries a real [`RpcError`] so the
/// crate's taxonomy mapping is exercised through its production path
/// (`RpcErrorClass::as_rpc_error`), not a test-only shortcut.
#[derive(Debug)]
pub struct FakeError(pub RpcError);

impl core::fmt::Display for FakeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0.code)
    }
}

impl RpcErrorClass for FakeError {
    fn is_rejection(&self) -> bool {
        true
    }
    fn as_rpc_error(&self) -> Option<&RpcError> {
        Some(&self.0)
    }
}

fn refuse<T>(code: &str) -> Result<T, FakeError> {
    Err(FakeError(RpcError::new(code, code)))
}

#[derive(Default)]
struct Account {
    /// Registration chain, oldest first, as the verbatim submitted bytes.
    chain: Vec<Vec<u8>>,
    /// The opaque escrow blob, when one rests.
    escrow: Option<Vec<u8>>,
    /// A parked seed-alone replacement.
    pending: Option<wire::ReplacementPendingInfo>,
    /// Set once this identity has been succeeded.
    succeeded_by: Option<[u8; 32]>,
    /// Succession statements from this identity forward, oldest first.
    successions: Vec<Vec<u8>>,
}

#[derive(Default)]
struct State {
    accounts: HashMap<[u8; 32], Account>,
    /// Live single-use nonces, per pool. The two pools are disjoint exactly as
    /// the nest's are, so a veto nonce can never redeem an escrow fetch.
    escrow_nonces: Vec<([u8; 32], [u8; 32])>,
    replacement_nonces: Vec<([u8; 32], [u8; 32])>,
    nonce_counter: u8,
}

/// One nest's recovery plane, in memory.
#[derive(Default)]
pub struct FakeNest {
    state: Mutex<State>,
    /// The actor whose connection this is, for the USER-class kinds that take
    /// the account from the session rather than the wire.
    session: Mutex<Option<[u8; 32]>>,
    /// When set, `escrow.put` refuses with this code — the injected failure the
    /// "the kit still comes back" test needs.
    escrow_put_fails: Mutex<Option<String>>,
    /// Kinds this nest pretends never to have heard of — how a nest answers
    /// a kind it does not serve, for the refusal arms.
    unknown_kinds: Mutex<Vec<String>>,
    /// The account's pairing rows, as `fauna.pair.list` serves them — the
    /// linked nests a clause-(c) gesture fans out to.
    pairings: Mutex<Vec<fauna_protocol::pair::PairingRow>>,
}

impl FakeNest {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sign this connection in as `actor` (the USER-class kinds need it).
    pub fn signed_in_as(self, actor: &ActorId) -> Self {
        *self.session.lock().unwrap() = Some(actor.0);
        self
    }

    /// Make every subsequent `escrow.put` refuse.
    pub fn fail_escrow_put(&self, code: &str) {
        *self.escrow_put_fails.lock().unwrap() = Some(code.to_string());
    }

    /// Make this nest answer `fauna.protocol.unknown_kind` for `kind` — a
    /// nest that does not serve it. The router refuses before any handler
    /// runs, so this models the real refusal exactly.
    pub fn forget_kind(&self, kind: &str) {
        self.unknown_kinds.lock().unwrap().push(kind.to_string());
    }

    /// Add a pairing row naming the nest `nest_id`, linked at `nest_url`.
    pub fn pair_with(&self, nest_id: [u8; 32], nest_url: &str) {
        self.pairings
            .lock()
            .unwrap()
            .push(fauna_protocol::pair::PairingRow {
                private_nest_id: ByteBuf::from(nest_id.to_vec()),
                capabilities: Vec::new(),
                expires_at: None,
                created_at: 0,
                label: None,
                nest_url: Some(nest_url.to_string()),
                extra: Default::default(),
            });
    }

    /// The escrow blob currently at rest, if any.
    pub fn escrow_blob(&self, actor: &ActorId) -> Option<Vec<u8>> {
        self.state
            .lock()
            .unwrap()
            .accounts
            .get(&actor.0)
            .and_then(|a| a.escrow.clone())
    }

    /// Replace the resting blob's bytes without going through `escrow.put` —
    /// how a test models a hostile or faulty nest serving corrupted bytes back.
    /// The row is opaque to the real nest too, so nothing here is bypassed.
    pub fn overwrite_escrow_blob(&self, actor: &ActorId, blob: Vec<u8>) {
        self.state
            .lock()
            .unwrap()
            .accounts
            .get_mut(&actor.0)
            .expect("account exists")
            .escrow = Some(blob);
    }

    /// Drop the resting row while leaving the registration chain intact — the
    /// `RegisteredNoEscrow` state a failed `escrow.put` leaves behind, and the
    /// one a nest reaches on its own by the lifecycle rule.
    pub fn clear_escrow_blob(&self, actor: &ActorId) {
        self.state
            .lock()
            .unwrap()
            .accounts
            .get_mut(&actor.0)
            .expect("account exists")
            .escrow = None;
    }

    /// The registered chain head, if any.
    pub fn head(&self, actor: &ActorId) -> Option<ChainHead> {
        let state = self.state.lock().unwrap();
        head_of(state.accounts.get(&actor.0)?)
    }

    /// Number of successions recorded from this identity forward — what a probe
    /// asks to learn whether the transaction committed while the client was
    /// being told it had not.
    pub fn succession_count(&self, actor: &ActorId) -> usize {
        self.state
            .lock()
            .unwrap()
            .accounts
            .get(&actor.0)
            .map(|a| a.successions.len())
            .unwrap_or(0)
    }

    /// Number of registrations on the chain.
    pub fn chain_len(&self, actor: &ActorId) -> usize {
        self.state
            .lock()
            .unwrap()
            .accounts
            .get(&actor.0)
            .map(|a| a.chain.len())
            .unwrap_or(0)
    }

    /// The parked seed-alone replacement, if a window is open.
    pub fn pending(&self, actor: &ActorId) -> Option<wire::ReplacementPendingInfo> {
        self.state
            .lock()
            .unwrap()
            .accounts
            .get(&actor.0)
            .and_then(|a| a.pending.clone())
    }

    /// The registration chain as served: the verbatim records, oldest first.
    pub fn chain(&self, actor: &ActorId) -> Vec<Vec<u8>> {
        self.state
            .lock()
            .unwrap()
            .accounts
            .get(&actor.0)
            .map(|a| a.chain.clone())
            .unwrap_or_default()
    }

    /// Land a seed-alone replacement as the nest itself does once its window
    /// passes uncontested: verified under the seed-alone rule against the
    /// current head and appended, with no `registration.submit` involved —
    /// the one way a link without a prior-key signature enters a chain.
    pub fn land_seed_alone(&self, actor: &ActorId, record: Vec<u8>) {
        let signed: SignedRecoveryKeyRegistration =
            canonical_decode(&record).expect("a landing record decodes");
        let mut state = self.state.lock().unwrap();
        let account = state.accounts.get_mut(&actor.0).expect("account exists");
        let head = head_of(account).expect("a seed-alone replacement needs a head");
        signed
            .verify_seed_alone(&head)
            .expect("the landing record verifies seed-alone");
        account.chain.push(record);
        account.escrow = None;
        account.pending = None;
    }

    /// HOSTILE-NEST KNOB: serve only the first `keep` links of the chain from
    /// now on — the rewind/truncation a dishonest chain server performs, which
    /// `resolve_successor`'s `known_head` exists to refuse.
    pub fn truncate_chain(&self, actor: &ActorId, keep: usize) {
        let mut state = self.state.lock().unwrap();
        if let Some(account) = state.accounts.get_mut(&actor.0) {
            account.chain.truncate(keep);
        }
    }

    /// HOSTILE-NEST KNOB: park a succession statement WITHOUT the verification
    /// `succession.submit` runs — a dishonest nest serves what it likes; only
    /// the consumer's own verification stands between it and a takeover. Also
    /// marks the identity succeeded, since the lookup walk follows that mark.
    pub fn plant_succession(&self, actor: &ActorId, statement_bytes: Vec<u8>) {
        let signed: SignedIdentitySuccession =
            canonical_decode(&statement_bytes).expect("a planted statement still decodes");
        let mut state = self.state.lock().unwrap();
        let account = state.accounts.entry(actor.0).or_default();
        account.succeeded_by = Some(signed.statement.new_actor_id.0);
        account.successions.push(statement_bytes);
    }
}

fn head_of(account: &Account) -> Option<ChainHead> {
    let last = account.chain.last()?;
    let signed: SignedRecoveryKeyRegistration = canonical_decode(last).ok()?;
    Some(ChainHead::new(
        signed.registration.recovery_pubkey,
        signed.registration.seq,
    ))
}

fn decoded_chain(account: &Account) -> Vec<SignedRecoveryKeyRegistration> {
    account
        .chain
        .iter()
        .filter_map(|b| canonical_decode(b).ok())
        .collect()
}

fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs_or_zero()
}

impl RpcRequester for FakeNest {
    type Error = FakeError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let payload_bytes = fauna_protocol::encode_canonical(&payload).unwrap();
        let reply_bytes = self.dispatch(kind, &payload_bytes)?;
        Ok(fauna_protocol::decode_strict(&reply_bytes).unwrap())
    }
}

/// A [`FakeNest`] one of whose replies never arrives — the probe shape.
///
/// The finding that motivated it: the nest commits a succession *before* it
/// encodes its reply (`record_succession`'s `tx.commit()` precedes
/// `encode_reply`), so a dropped websocket in that gap returns an error to a
/// client whose account has **already moved**. Nothing about that is
/// reproducible with a refusal — a refusal means the nest decided *not* to act —
/// so this wrapper does the one thing a refusal cannot: let the handler run to
/// completion, then throw the answer away.
///
/// The error it raises is deliberately **transport class** (`is_rejection() ==
/// false`, `as_rpc_error() == None`), because that is what a real dropped
/// connection looks like to `RecoveryError::from_transport`, and because a
/// client that could tell the two apart would not need the fix.
pub struct LossyNest {
    inner: FakeNest,
    /// The kind whose next reply is dropped, armed one call at a time.
    lose_next: Mutex<Option<&'static str>>,
}

/// [`LossyNest`]'s error: a pass-through refusal, or a lost reply.
#[derive(Debug)]
pub enum LossyError {
    /// The wrapped nest refused; carried verbatim so the taxonomy mapping is
    /// exercised exactly as it would be without the wrapper.
    Refused(FakeError),
    /// The call was handled and its reply discarded. Indistinguishable, from the
    /// client, from a request that never arrived — which is the whole point.
    ReplyLost,
}

impl core::fmt::Display for LossyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Refused(inner) => write!(f, "{inner}"),
            Self::ReplyLost => write!(f, "connection closed before the reply arrived"),
        }
    }
}

impl RpcErrorClass for LossyError {
    fn is_rejection(&self) -> bool {
        match self {
            Self::Refused(inner) => inner.is_rejection(),
            Self::ReplyLost => false,
        }
    }
    fn as_rpc_error(&self) -> Option<&RpcError> {
        match self {
            Self::Refused(inner) => inner.as_rpc_error(),
            Self::ReplyLost => None,
        }
    }
}

impl LossyNest {
    pub fn new(inner: FakeNest) -> Self {
        Self {
            inner,
            lose_next: Mutex::new(None),
        }
    }

    /// Drop the reply to the next call of `kind` — **after** the nest has
    /// handled it and committed whatever it commits.
    pub fn lose_reply_to(&self, kind: &'static str) {
        *self.lose_next.lock().unwrap() = Some(kind);
    }

    /// The nest underneath, for the state assertions a probe makes about what
    /// really landed while the client was being told it failed.
    pub fn nest(&self) -> &FakeNest {
        &self.inner
    }
}

impl RpcRequester for LossyNest {
    type Error = LossyError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        // Call through FIRST and unconditionally: the defect only exists because
        // the nest's work is already done by the time the reply is lost.
        let handled = self.inner.request::<Req, Reply>(kind, payload).await;
        let armed = {
            let mut slot = self.lose_next.lock().unwrap();
            if *slot == Some(kind) {
                *slot = None;
                true
            } else {
                false
            }
        };
        match handled {
            Ok(_reply) if armed => Err(LossyError::ReplyLost),
            Ok(reply) => Ok(reply),
            Err(refusal) => Err(LossyError::Refused(refusal)),
        }
    }
}

impl FakeNest {
    fn dispatch(&self, kind: &'static str, payload: &[u8]) -> Result<Vec<u8>, FakeError> {
        // Before anything else, exactly where the real router checks it: a kind
        // this nest does not serve never reaches a handler.
        if self.unknown_kinds.lock().unwrap().iter().any(|k| k == kind) {
            return refuse("fauna.protocol.unknown_kind");
        }

        let mut state = self.state.lock().unwrap();
        let session = *self.session.lock().unwrap();

        match kind {
            "fauna.recovery.registration.submit" => {
                let req: wire::RegistrationSubmitRequest =
                    fauna_protocol::decode_strict(payload).unwrap();
                let actor = session.expect("registration.submit is USER class");
                let signed: SignedRecoveryKeyRegistration =
                    canonical_decode(req.registration.as_ref()).unwrap();
                // The record must name the authenticated actor.
                if signed.registration.actor_id.0 != actor {
                    return refuse("fauna.auth.forbidden");
                }
                let account = state.accounts.entry(actor).or_default();
                if account.succeeded_by.is_some() {
                    return superseded(account);
                }
                let prior = head_of(account);
                if signed.verify(prior.as_ref()).is_err() {
                    return refuse("fauna.recovery.signature_failed");
                }
                let changed_pubkey =
                    prior.is_none_or(|h| h.recovery_pubkey != signed.registration.recovery_pubkey);
                account.chain.push(req.registration.to_vec());
                // The lifecycle rule the client's re-put exists to answer.
                if changed_pubkey {
                    account.escrow = None;
                    account.pending = None;
                }
                encode(&wire::RegistrationSubmitReply {
                    seq: signed.registration.seq,
                    ..Default::default()
                })
            }

            "fauna.recovery.registration.chain" => {
                let req: wire::RegistrationChainRequest =
                    fauna_protocol::decode_strict(payload).unwrap();
                let actor = actor32(&req.actor_id);
                let registrations = state
                    .accounts
                    .get(&actor)
                    .map(|a| a.chain.iter().cloned().map(ByteBuf::from).collect())
                    .unwrap_or_default();
                encode(&wire::RegistrationChainReply {
                    registrations,
                    ..Default::default()
                })
            }

            "fauna.recovery.escrow.put" => {
                if let Some(code) = self.escrow_put_fails.lock().unwrap().as_deref() {
                    return refuse(code);
                }
                let req: wire::EscrowPutRequest = fauna_protocol::decode_strict(payload).unwrap();
                let actor = session.expect("escrow.put is USER class");
                let account = state.accounts.entry(actor).or_default();
                account.escrow = Some(req.blob.to_vec());
                encode(&wire::EscrowPutReply {
                    updated_at: now_secs(),
                    ..Default::default()
                })
            }

            // The signed-in presence read. USER class like `put`, and carrying
            // no actor field — the account comes from the session, which is
            // what keeps it from being a cross-account oracle.
            "fauna.recovery.escrow.status" => {
                let _req: wire::EscrowStatusRequest =
                    fauna_protocol::decode_strict(payload).unwrap();
                let actor = session.expect("escrow.status is USER class");
                let present = state
                    .accounts
                    .get(&actor)
                    .and_then(|a| a.escrow.as_ref())
                    .is_some();
                encode(&wire::EscrowStatusReply {
                    present,
                    updated_at: present.then(now_secs),
                    ..Default::default()
                })
            }

            "fauna.recovery.escrow.challenge" => {
                let req: wire::EscrowChallengeRequest =
                    fauna_protocol::decode_strict(payload).unwrap();
                let actor = actor32(&req.actor_id);
                let nonce = state.mint_nonce();
                state.escrow_nonces.push((actor, nonce));
                encode(&wire::EscrowChallengeReply {
                    nonce: ByteBuf::from(nonce.to_vec()),
                    expires_at: now_secs() as u64 + 300,
                    ..Default::default()
                })
            }

            "fauna.recovery.escrow.fetch" => {
                let req: wire::EscrowFetchRequest = fauna_protocol::decode_strict(payload).unwrap();
                let actor = actor32(&req.actor_id);
                let nonce = actor32(&req.nonce);
                // Single-use: consumed before verification, exactly as the nest
                // does, so a failed attempt cannot be retried with the same one.
                let pos = state
                    .escrow_nonces
                    .iter()
                    .position(|(a, n)| *a == actor && *n == nonce);
                let Some(pos) = pos else {
                    return refuse("fauna.recovery.invalid_nonce");
                };
                state.escrow_nonces.remove(pos);

                let Some(account) = state.accounts.get(&actor) else {
                    return refuse("fauna.recovery.not_registered");
                };
                if account.succeeded_by.is_some() {
                    return superseded(account);
                }
                let Some(head) = head_of(account) else {
                    return refuse("fauna.recovery.not_registered");
                };
                let challenge = EscrowChallenge::new(ActorId(actor), nonce);
                if challenge
                    .verify(&head.recovery_pubkey, req.signature.as_ref())
                    .is_err()
                {
                    return refuse("fauna.recovery.signature_failed");
                }
                let Some(blob) = account.escrow.clone() else {
                    return refuse("fauna.recovery.no_escrow");
                };
                encode(&wire::EscrowFetchReply {
                    blob: ByteBuf::from(blob),
                    ..Default::default()
                })
            }

            "fauna.recovery.replacement.request" => {
                let req: wire::ReplacementRequestRequest =
                    fauna_protocol::decode_strict(payload).unwrap();
                let actor = session.expect("replacement.request is USER class");
                let signed: SignedRecoveryKeyRegistration =
                    canonical_decode(req.registration.as_ref()).unwrap();
                let account = state.accounts.entry(actor).or_default();
                let Some(head) = head_of(account) else {
                    return refuse("fauna.recovery.not_registered");
                };
                // The dedicated seed-alone arm — the strict chain rule refuses
                // this record by design, which is what stops it landing
                // windowless.
                if signed.verify_seed_alone(&head).is_err() {
                    return refuse("fauna.recovery.signature_failed");
                }
                let lands_at = now_secs() + RECOVERY_REPLACE_GRACE_SECS as i64;
                account.pending = Some(wire::ReplacementPendingInfo {
                    new_recovery_pubkey: ByteBuf::from(
                        signed.registration.recovery_pubkey.to_vec(),
                    ),
                    requested_at: now_secs(),
                    lands_at,
                    ..Default::default()
                });
                encode(&wire::ReplacementRequestReply {
                    lands_at,
                    ..Default::default()
                })
            }

            "fauna.recovery.replacement.status" => {
                let actor = session.expect("replacement.status is USER class");
                let pending = state.accounts.get(&actor).and_then(|a| a.pending.clone());
                encode(&wire::ReplacementStatusReply {
                    pending,
                    ..Default::default()
                })
            }

            "fauna.recovery.replacement.challenge" => {
                let req: wire::ReplacementChallengeRequest =
                    fauna_protocol::decode_strict(payload).unwrap();
                let actor = actor32(&req.actor_id);
                let nonce = state.mint_nonce();
                state.replacement_nonces.push((actor, nonce));
                encode(&wire::ReplacementChallengeReply {
                    nonce: ByteBuf::from(nonce.to_vec()),
                    expires_at: now_secs() as u64 + 300,
                    ..Default::default()
                })
            }

            "fauna.recovery.replacement.veto" => {
                let req: wire::ReplacementVetoRequest =
                    fauna_protocol::decode_strict(payload).unwrap();
                let actor = actor32(&req.actor_id);
                let nonce = actor32(&req.nonce);
                let pos = state
                    .replacement_nonces
                    .iter()
                    .position(|(a, n)| *a == actor && *n == nonce);
                let Some(pos) = pos else {
                    return refuse("fauna.recovery.invalid_nonce");
                };
                state.replacement_nonces.remove(pos);

                let Some(account) = state.accounts.get_mut(&actor) else {
                    return refuse("fauna.recovery.not_registered");
                };
                let Some(head) = head_of(account) else {
                    return refuse("fauna.recovery.not_registered");
                };
                let veto = ReplacementVeto::new(ActorId(actor), nonce);
                if veto
                    .verify(&head.recovery_pubkey, req.signature.as_ref())
                    .is_err()
                {
                    return refuse("fauna.recovery.signature_failed");
                }
                let cancelled = account.pending.take().is_some();
                encode(&wire::ReplacementVetoReply {
                    cancelled,
                    ..Default::default()
                })
            }

            "fauna.recovery.succession.submit" => {
                let req: wire::SuccessionSubmitRequest =
                    fauna_protocol::decode_strict(payload).unwrap();
                let signed: SignedIdentitySuccession =
                    canonical_decode(req.statement.as_ref()).unwrap();
                let old = signed.statement.old_actor_id.0;
                let Some(account) = state.accounts.get(&old) else {
                    return refuse("fauna.recovery.not_registered");
                };
                if account.succeeded_by.is_some() {
                    return refuse("fauna.recovery.already_succeeded");
                }
                let chain = decoded_chain(account);
                if verify_succession_against_chain(&signed, &chain, None).is_err() {
                    return refuse("fauna.recovery.signature_failed");
                }
                let new_actor = signed.statement.new_actor_id.0;
                let account = state.accounts.get_mut(&old).unwrap();
                account.succeeded_by = Some(new_actor);
                account.successions.push(req.statement.to_vec());
                // Deleted inside the succession transaction — the old kit
                // retires with the old identity.
                account.escrow = None;
                account.pending = None;
                state.accounts.entry(new_actor).or_default();
                encode(&wire::SuccessionSubmitReply {
                    new_actor_id: ByteBuf::from(new_actor.to_vec()),
                    succeeded_at: now_secs(),
                    ..Default::default()
                })
            }

            "fauna.recovery.succession.lookup" => {
                let req: wire::SuccessionLookupRequest =
                    fauna_protocol::decode_strict(payload).unwrap();
                let mut statements = Vec::new();
                let mut current = actor32(&req.actor_id);
                // Walk forward to the terminal successor, as the real lookup does.
                while let Some(account) = state.accounts.get(&current) {
                    let Some(next) = account.succeeded_by else {
                        break;
                    };
                    statements.extend(account.successions.iter().cloned().map(ByteBuf::from));
                    current = next;
                }
                encode(&wire::SuccessionLookupReply {
                    statements,
                    ..Default::default()
                })
            }

            "fauna.pair.list" => {
                if session.is_none() {
                    return refuse("fauna.auth.forbidden");
                }
                encode(&fauna_protocol::pair::PairListReply {
                    pairings: self.pairings.lock().unwrap().clone(),
                    forward_queue: Default::default(),
                    extra: Default::default(),
                })
            }

            other => panic!("FakeNest got an unexpected kind: {other}"),
        }
    }
}

impl State {
    fn mint_nonce(&mut self) -> [u8; 32] {
        self.nonce_counter = self.nonce_counter.wrapping_add(1);
        let mut nonce = [0u8; 32];
        nonce[0] = self.nonce_counter;
        nonce
    }
}

fn superseded<T>(account: &Account) -> Result<T, FakeError> {
    Err(FakeError(RpcError::superseded(
        &account.succeeded_by.expect("caller checked"),
    )))
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, FakeError> {
    Ok(canonical_encode(value).unwrap())
}

fn actor32(bytes: &ByteBuf) -> [u8; 32] {
    bytes.as_ref().try_into().expect("32-byte id")
}
