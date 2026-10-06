//! Terminal did:plc identity retirement — the client-signed PLC tombstone
//! (S5 slice 5b of `atproto-pds-bridge.md` § Disable & revocation layer 2).
//!
//! This is the one act in the whole Bluesky subsystem that **destroys** rather
//! than deactivates. Stepping down deactivates; "Delete my Bluesky presence"
//! sweeps the projected records but keeps the identity ("still reversible in
//! identity terms"); the tombstone published here ends the DID itself, and no
//! part of it can be undone once PLC's contest window closes.
//!
//! ## Why this lives on the client — and what the box can and cannot do
//!
//! A PLC tombstone must be signed by a rotation key **listed** in the
//! operation it chains to, and [`select_signer`] below states the consequence
//! honestly: membership, not position — any listed key may sign the next op.
//! The ratified custody split (`atproto-pds-bridge.md` § State & data shape)
//! puts the user's key at `rotationKeys[0]`, SENIOR to the bridge's junior
//! key at index 1 — but the bridge's key is *listed*, so a compromised bridge
//! could sign a tombstone the directory would accept (the 2026-08-02 finding
//! (c)). What the custody split actually delivers is not box-incapability but
//! the user's **contest position** (the senior key out-ranks the junior one
//! inside PLC's 72 h recovery window) plus **attribution**
//! ([`tombstone_signed_by_held_key`] — a tombstone no held key signed raises
//! the custody alarm instead of reading as the user's own retirement).
//!
//! This module is the *product's* only tombstone path, and it lives on the
//! client because the **user's** consent and the user's senior key live here:
//! the op is built, signed and submitted against the client's own HTTPS
//! connection to the directory — the same no-nest-in-the-middle property
//! S4-C's [`crate::genesis_verify`] relies on — and only when the nest's
//! durable intent agrees with the client-authored consent record
//!.
//!
//! ## Ordering: the sweep runs first, always — decided off ONE read
//!
//! [`converge_retirement`] observes the delete-presence sweep finished before
//! anything is signed or submitted. The reason is not that the sweep would
//! break — the bridge's sweep signs commits with a key it already holds and
//! never resolves the DID — but that a **tombstoned DID stops resolving**,
//! and a relay or AppView that cannot resolve a DID cannot verify that DID's
//! commits. Retiring first would therefore silently strand the delete
//! tombstones: our repo would be gone and the network's copies would stay,
//! which is the precise failure the delete flow exists to prevent. It is S5
//! slice 5a's own rule one level up — do the destructive work *through* the
//! protocol while the protocol can still hear you, and change the terminal
//! state last.
//!
//! The whole step is one function reading the directory's log **once**
//! because its two questions are coupled: the standing head that answers
//! "already retired" (a `plc_tombstone`) is precisely the head that has no
//! `atproto_pds` service left to probe. Asked as two independent calls, an
//! already-published tombstone made the sweep probe answer "cannot tell"
//! forever, so a client that crashed between submit and report could never
//! re-report — the wedge an independent security review. [`retirement_step`] pins the coupling as a pure decision.
//!
//! ## Idempotency, because a client can crash mid-act
//!
//! Submission is not the durable record of anything; the directory is. So the
//! head is always re-read first, and a log whose standing head is *already* a
//! tombstone reports [`RetirementProgress::AlreadyRetired`] rather than an
//! error — that is what lets a client that crashed between submitting and
//! reporting converge on the truth instead of double-submitting or, worse,
//! telling the user the retirement failed when it succeeded.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use fauna_core::data::AtprotoRotationKey;
use p256::ecdsa::SigningKey;
use p256::ecdsa::signature::Signer as _;
use serde::{Deserialize, Serialize};

use crate::genesis_verify::{
    VerifyFailure, fetch_audit_log, parse_audit_log, plc_directory_base_url,
};

/// The `type` of a did:plc v0.1 tombstone operation. Distinct from the
/// `plc_operation` every other op in this system carries — a tombstone has its
/// own type, not a flag on the ordinary shape.
pub const OP_TYPE_TOMBSTONE: &str = "plc_tombstone";

/// A signed `plc_tombstone`, and the exact JSON body submitted to the
/// directory.
///
/// The op carries **only** these three fields — no rotation keys, no
/// verification methods, no services, no `alsoKnownAs`. That is the spec shape,
/// and it is also the point: a tombstone asserts nothing about the identity
/// except that it ends here.
///
/// Field order is the canonical dag-cbor order (see [`unsigned_tombstone_cbor`]);
/// the JSON body's order is irrelevant to the directory, but keeping one order
/// in one place means the signed bytes and the submitted bytes are read off the
/// same declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TombstoneOp {
    /// CID of the operation this tombstone chains to — the directory's own
    /// standing head, never a locally rebuilt view of it.
    pub prev: String,
    #[serde(rename = "type")]
    pub op_type: String,
    /// base64url-no-pad ECDSA-SHA256 signature, low-S, fixed-size `r‖s`, over
    /// [`unsigned_tombstone_cbor`].
    pub sig: String,
}

/// The unsigned op — the shape whose canonical dag-cbor the rotation key signs.
///
/// Declared `prev` before `type` because that **is** the canonical dag-cbor map
/// order for these two keys (equal length, so length-first ordering falls
/// through to bytewise: `prev` < `type`). `encode_canonical` sorts regardless;
/// the declaration matching the wire order is a courtesy to the next reader,
/// pinned by `unsigned_cbor_is_canonical_and_byte_stable`.
#[derive(Serialize)]
struct UnsignedTombstoneOp<'a> {
    prev: &'a str,
    #[serde(rename = "type")]
    op_type: &'a str,
}

/// Where one converge pass got to. One type for the whole step — the sweep
/// question and the retire question are answered off ONE read of the
/// directory's log, never as two independently-answerable calls: an
/// already-published tombstone is what makes the old two-call shape
/// contradictory (the head that means "already retired" is the head that has
/// no PDS service to probe), and modelling them separately is how that state
/// deadlocked as an eternal quiet-retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetirementProgress {
    /// This call published the tombstone. The identity is retired.
    Retired {
        /// The op this tombstone chained to, for the record nest keeps.
        prev_cid: String,
        /// The held key that signed — the one the standing head listed. The
        /// caller burns it against this DID
        /// (`AtprotoRotationKey::published_for_dids`), so a later mint can
        /// never reuse it and publicly link the fresh DID to this retired
        /// one.
        signed_with_did_key: String,
    },
    /// The directory's standing head is already a tombstone — a previous
    /// attempt (or another of the user's devices) got there first, possibly
    /// this same client crashing between submit and report. A success, not an
    /// error, decided **before** any PDS probe: a tombstone op declares no
    /// services, so there is nothing to probe and nothing left to wait for.
    ///
    /// Carries no CID on purpose: this client chained nothing, and
    /// `RecordTombstoneRequest::prev_cid` already rules that an empty value is
    /// exactly what this case reports.
    AlreadyRetired,
    /// The DID's own PDS still serves the repo: the delete-presence sweep has
    /// not finished, so nothing was submitted. Retry on a later convergence.
    SweepStillRunning,
}

/// Why a retirement could not be completed.
///
/// Every variant is a *retry* class except [`TombstoneError::SeniorKeyNotListed`],
/// which is terminal for this key: the published log does not list it, so no
/// number of retries will make the directory accept a signature from it.
#[derive(Debug, thiserror::Error)]
pub enum TombstoneError {
    /// The directory could not be read (network, HTTP status, unparseable body,
    /// no standing op). Quiet-retry class, exactly as for the seniority check.
    #[error("could not read the operation log: {0}")]
    Directory(#[from] VerifyFailure),
    /// A standing entry carried no `cid`, so there is nothing to chain to.
    /// A misbehaving directory, not a user-reachable state.
    #[error("the standing operation log head carries no cid")]
    HeadWithoutCid,
    /// The stored senior rotation key is not among the head op's
    /// `rotationKeys`, so the directory would reject a signature from it. This
    /// is the same condition S4-C's seniority check alarms on, caught here
    /// before anything is submitted so the user is never told an identity was
    /// retired when the directory refused it.
    #[error("this client's rotation key is not listed on the published operation")]
    SeniorKeyNotListed {
        /// What the head op does list, for the diagnostic.
        published: Vec<String>,
    },
    /// The stored scalar did not parse as a P-256 secret key.
    #[error("the stored rotation key is not a valid P-256 scalar")]
    BadRotationKey,
    /// Canonical dag-cbor encoding failed — a programming error, not a
    /// reachable state (the op is two short strings).
    #[error("could not encode the tombstone operation: {0}")]
    Encode(String),
    /// The directory refused the submission, or was unreachable during it.
    #[error("the directory refused the tombstone: {0}")]
    Submit(String),
    /// The PDS could not be asked whether the sweep has finished. Quiet-retry
    /// class: an unanswered question is never a licence to retire early.
    #[error("could not read the repo's status from its PDS: {0}")]
    Probe(String),
}

/// The canonical dag-cbor of the unsigned op — the exact byte string the
/// rotation key signs.
///
/// dag-cbor, not JSON: the directory validates the signature against this
/// encoding, and JSON is only ever the HTTP body. Same split the Go bridge's
/// `PlcOperation.UnsignedCBOR` makes for the ops it signs.
pub fn unsigned_tombstone_cbor(prev_cid: &str) -> Result<Vec<u8>, TombstoneError> {
    let op = UnsignedTombstoneOp {
        prev: prev_cid,
        op_type: OP_TYPE_TOMBSTONE,
    };
    fauna_protocol::encode_canonical(&op)
        .map(|b| b.to_vec())
        .map_err(|e| TombstoneError::Encode(e.to_string()))
}

/// Build and sign a tombstone chaining to `prev_cid`, using the user's senior
/// rotation key scalar.
///
/// The signature is ECDSA-SHA256 over the canonical dag-cbor, **normalized to
/// low-S**, serialized as the fixed-size `r‖s` pair and base64url-no-pad
/// encoded. Low-S is not optional: ATProto's crypto rules require it of both
/// blessed curves, and RustCrypto's P-256 signer does not normalize on its own
/// (unlike its secp256k1 signer, where the ecosystem forced the default) — so a
/// signature that skipped this step would verify locally and be rejected by the
/// directory roughly half the time.
pub fn sign_tombstone(
    prev_cid: &str,
    senior_secret_scalar: &[u8; 32],
) -> Result<TombstoneOp, TombstoneError> {
    let signing =
        SigningKey::from_slice(senior_secret_scalar).map_err(|_| TombstoneError::BadRotationKey)?;
    let msg = unsigned_tombstone_cbor(prev_cid)?;
    let sig: p256::ecdsa::Signature = signing.sign(&msg);
    let sig = sig.normalize_s().unwrap_or(sig);
    Ok(TombstoneOp {
        prev: prev_cid.to_string(),
        op_type: OP_TYPE_TOMBSTONE.to_string(),
        sig: B64URL.encode(sig.to_bytes()),
    })
}

/// Does a published tombstone's signature verify against one of the client's
/// held rotation keys — i.e. is this retirement **our own act** (this device
/// or a sibling), as opposed to one signed by some *other* key the op it
/// chains to happened to list (the bridge's junior key foremost)?
///
/// This is the attribution the silent-retirement reading rests on: a tombstone
/// in the public log is self-evidence of *a* retirement, but only a signature
/// from the user's own ring is evidence the USER retired it. The check needs
/// no key list from the previous op — the question is "is it ours", not "whose
/// is it" — and every legitimate retirement in this system was signed by a
/// ring key through [`sign_tombstone`], whose exact canonical-cbor + low-S +
/// `r‖s`/base64url shape is what is re-verified here. Ring keys are P-256 by
/// construction; a k256-signed tombstone (the curve the bridge's junior key
/// uses) simply fails to verify, which is the correct answer.
///
/// Anything unverifiable — a missing `prev` or `sig`, an undecodable held key,
/// a signature over different bytes — answers **false**, and false is the loud
/// direction (the caller alarms): an unattributable terminal act on an
/// identity this client protects must never read as the ordinary end of its
/// life.
pub fn tombstone_signed_by_held_key(
    prev_cid: &str,
    sig_b64: &str,
    held_did_keys: &[String],
) -> bool {
    use p256::ecdsa::signature::Verifier as _;
    if prev_cid.is_empty() || sig_b64.is_empty() {
        return false;
    }
    let Ok(msg) = unsigned_tombstone_cbor(prev_cid) else {
        return false;
    };
    let Ok(sig_bytes) = B64URL.decode(sig_b64) else {
        return false;
    };
    let Ok(sig) = p256::ecdsa::Signature::from_slice(&sig_bytes) else {
        return false;
    };
    held_did_keys.iter().any(|held| {
        let Ok((fauna_protocol::atproto::DidKeyCurve::P256, point)) =
            fauna_protocol::atproto::decode_did_key(held)
        else {
            return false;
        };
        let Ok(vk) = p256::ecdsa::VerifyingKey::from_sec1_bytes(&point) else {
            return false;
        };
        vk.verify(&msg, &sig).is_ok()
    })
}

/// The directory's standing log head: what a next op must chain to, and what it
/// says about who may sign that op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandingHead {
    /// CID of the last non-nullified operation.
    pub cid: String,
    /// Its `type` — [`OP_TYPE_TOMBSTONE`] when the identity is already retired.
    pub op_type: String,
    /// Its `rotationKeys`, in published seniority order.
    pub rotation_keys: Vec<String>,
    /// The `atproto_pds` service endpoint this DID publishes — where the repo
    /// is hosted, per the DID's own published record rather than per anything
    /// the box asserts. Empty when the op declares no such service (a did:plc
    /// this system did not mint, or one already moved off this PDS).
    pub pds_endpoint: String,
}

impl StandingHead {
    /// Whether this identity is already retired.
    pub fn is_tombstoned(&self) -> bool {
        self.op_type == OP_TYPE_TOMBSTONE
    }
}

/// The service id every ATProto PLC operation publishes its PDS under.
const SERVICE_ID_ATPROTO_PDS: &str = "atproto_pds";

/// Read the standing head out of a raw `/{did}/log/audit` body. Pure — the unit
/// under test.
///
/// Nullified entries are skipped, exactly as the Go bridge's `FetchLastOp`
/// does: an op a senior key contested inside PLC's window is no longer part of
/// the chain, so chaining `prev` to one would be rejected.
pub fn standing_head(audit_json: &[u8]) -> Result<StandingHead, TombstoneError> {
    let entries = parse_audit_log(audit_json)?;
    let head = entries
        .iter()
        .rev()
        .find(|e| !e.nullified)
        .ok_or(VerifyFailure::NoStandingOps)?;
    if head.cid.is_empty() {
        return Err(TombstoneError::HeadWithoutCid);
    }
    Ok(StandingHead {
        cid: head.cid.clone(),
        op_type: head.operation.op_type.clone(),
        rotation_keys: head.operation.rotation_keys.clone(),
        pds_endpoint: head
            .operation
            .services
            .get(SERVICE_ID_ATPROTO_PDS)
            .map(|s| s.endpoint.clone())
            .unwrap_or_default(),
    })
}

/// One whole retirement converge step: read the directory's standing head
/// **once** and let that one read answer every question — already retired?
/// sweep finished? which held key may sign? — then sign and submit only when
/// all three answers say go.
///
/// The single fetch is load-bearing, not an economy. The two questions the
/// old shape asked separately ("has the sweep finished?" and "is it already
/// retired?") are answered by the SAME log, and for an already-published
/// tombstone they are contradictory when asked separately: the tombstone head
/// declares no `atproto_pds` service, so a sweep probe routed through it can
/// only answer "cannot tell" — an eternal quiet-retry that made
/// `AlreadyRetired` unreachable and wedged the row nest-side the moment a
/// report failed after a successful submit. Deciding "already retired" from
/// the head, **before** any probe, is the fix; everything else keeps the
/// module's ordering rule (the sweep is observed finished before anything is
/// submitted).
///
/// Takes the whole held keyring rather than one chosen key because *which*
/// key may sign is the published log's fact, not a list position: after a
/// fresh-key re-mint the ring holds the live identity's key beside the
/// retired one's, and this DID's log names exactly the key that was published
/// for it.
///
/// **Terminal.** After this returns [`RetirementProgress::Retired`] the DID
/// stops resolving, its handle resolves to nothing, and no later operation
/// can revive it once PLC's contest window closes. Call it only from a flow
/// that has told the user so in those terms.
pub async fn converge_retirement(
    directory_base_url: &str,
    did: &str,
    held_keys: &[AtprotoRotationKey],
) -> Result<RetirementProgress, TombstoneError> {
    let body = fetch_audit_log(directory_base_url, did).await?;
    let head = standing_head(&body)?;
    let endpoint = match retirement_step(&head)? {
        StepPlan::AlreadyRetired => return Ok(RetirementProgress::AlreadyRetired),
        StepPlan::ProbeSweep { pds_endpoint } => pds_endpoint.to_string(),
    };
    match probe_sweep_complete(&endpoint, did).await? {
        SweepProbe::RepoPresent => return Ok(RetirementProgress::SweepStillRunning),
        SweepProbe::RepoGone => {}
    }
    let signer = select_signer(held_keys, &head)?;
    let op = sign_tombstone(&head.cid, &signer.secret_scalar.to_array())?;
    submit_tombstone(directory_base_url, did, &op).await?;
    Ok(RetirementProgress::Retired {
        prev_cid: head.cid,
        signed_with_did_key: signer.pubkey_did_key.clone(),
    })
}

/// What one look at the standing head decides. Pure — the unit under test.
#[derive(Debug, PartialEq, Eq)]
pub enum StepPlan<'a> {
    /// The head is already a tombstone: report success, probe nothing —
    /// a tombstone op declares no services, so there is nothing to ask and
    /// nothing to wait for. (Routing this state into the probe is exactly
    /// the deadlock this type exists to make unrepresentable.)
    AlreadyRetired,
    /// The head is a live op: ask its published PDS whether the sweep has
    /// finished before anything may be signed.
    ProbeSweep { pds_endpoint: &'a str },
}

/// Decide the step from the standing head. A live op that declares no
/// `atproto_pds` service leaves the sweep question unanswerable — quiet-retry,
/// never a licence to retire early (our mint flow always declares one, so
/// this is a misbehaving or foreign log, not a reachable product state).
pub fn retirement_step(head: &StandingHead) -> Result<StepPlan<'_>, TombstoneError> {
    if head.is_tombstoned() {
        return Ok(StepPlan::AlreadyRetired);
    }
    if head.pds_endpoint.is_empty() {
        return Err(TombstoneError::Probe(
            "the standing operation declares no atproto_pds service to ask".into(),
        ));
    }
    Ok(StepPlan::ProbeSweep {
        pds_endpoint: &head.pds_endpoint,
    })
}

/// Pick the held key the standing head lists — the only key the directory
/// would accept a next op from. Pure — the unit under test.
///
/// Membership, not position: any rotation key listed on the previous op may
/// sign the next one. Seniority decides who wins a *contest*, which a
/// tombstone of a live identity never enters — so requiring index 0 here
/// would refuse retirements the directory would happily accept. And the
/// *held ring's* order means nothing here: after a fresh-key re-mint the ring
/// holds several keys, and the one that may sign for THIS DID is whichever
/// its published log lists.
pub fn select_signer<'a>(
    held_keys: &'a [AtprotoRotationKey],
    head: &StandingHead,
) -> Result<&'a AtprotoRotationKey, TombstoneError> {
    held_keys
        .iter()
        .find(|k| head.rotation_keys.iter().any(|p| p == &k.pubkey_did_key))
        .ok_or_else(|| TombstoneError::SeniorKeyNotListed {
            published: head.rotation_keys.clone(),
        })
}

/// What the PDS's own public read surface says about a DID's repo.
///
/// Deliberately three-valued: "gone", "still there", and "could not tell" are
/// three different answers, and collapsing the third into either of the others
/// would either retire early or never retire at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepProbe {
    /// The PDS holds no repo for this DID — `RepoNotFound`. The sweep has
    /// finished, or there was never anything to sweep (a never-projected
    /// identity answers the same way, which is the correct reading here: either
    /// way nothing is left to delete).
    RepoGone,
    /// The repo is still being served, so the sweep has not finished.
    RepoPresent,
}

/// Ask the DID's own published PDS whether the delete-presence sweep has
/// finished, by reading the surface a relay would read.
///
/// **This is a politeness ordering, not a security boundary, and the code says
/// so rather than overselling it.** A hostile or broken box could equally just
/// not sweep, or answer `RepoNotFound` while still serving; nothing here is a
/// proof. What the user is actually protected by is the tombstone itself, which
/// lands regardless of what this returns. The probe's job is narrower and worth
/// doing anyway: retiring *before* the deletes are published would strand them
/// (see the module docs), so it is worth asking.
///
/// The ask is **client-direct** — the endpoint comes from the DID's own PLC
/// log, and the request goes straight to it. The box is never in the middle of
/// a question about its own conduct; that is S4-C's property, and it is why the
/// endpoint is read from the published log rather than taken from a nest reply.
///
/// `did` is passed as a query parameter to a URL built from the log's endpoint,
/// so a malformed DID can at worst produce a request the PDS refuses.
pub async fn probe_sweep_complete(
    pds_endpoint: &str,
    did: &str,
) -> Result<SweepProbe, TombstoneError> {
    let url = format!(
        "{}/xrpc/com.atproto.sync.getRepoStatus",
        pds_endpoint.trim_end_matches('/')
    );
    let client = reqwest::Client::new();
    let req = client.get(&url).query(&[("did", did)]);
    #[cfg(not(target_arch = "wasm32"))]
    let req = req.timeout(std::time::Duration::from_secs(30));
    let resp = req
        .send()
        .await
        .map_err(|e| TombstoneError::Probe(e.to_string()))?;
    let status = resp.status();
    if status.is_success() {
        return Ok(SweepProbe::RepoPresent);
    }
    let body = resp
        .bytes()
        .await
        .map_err(|e| TombstoneError::Probe(e.to_string()))?;
    interpret_repo_status_error(status.as_u16(), &body)
}

/// Read a non-2xx `getRepoStatus` answer. Pure — the unit under test, split
/// from the I/O for the same reason [`crate::genesis_verify::verify_audit_log`]
/// is.
///
/// Keyed on the **lexicon's error name, not the HTTP status**: `getRepoStatus`
/// answers 400 both for a DID it serves no repo for and for a malformed
/// request, and only one of those means the sweep is done.
fn interpret_repo_status_error(status: u16, body: &[u8]) -> Result<SweepProbe, TombstoneError> {
    match serde_json::from_slice::<XrpcErrorBody>(body) {
        Ok(err) if err.error == XRPC_ERROR_REPO_NOT_FOUND => Ok(SweepProbe::RepoGone),
        // Any other named error, or a body that is not the XRPC error shape at
        // all, leaves the question unanswered — never a licence to retire.
        _ => Err(TombstoneError::Probe(format!(
            "the PDS answered HTTP {status} to getRepoStatus"
        ))),
    }
}

/// The XRPC error name a PDS returns for a DID it serves no repo for.
const XRPC_ERROR_REPO_NOT_FOUND: &str = "RepoNotFound";

/// The XRPC wire error shape (`{"error": …, "message": …}`); only the name is
/// read.
#[derive(Deserialize)]
struct XrpcErrorBody {
    #[serde(default)]
    error: String,
}

/// The default directory base URL, honouring the same test-only seam the
/// seniority check and the Go bridge honour.
pub fn directory_base_url() -> String {
    plc_directory_base_url()
}

/// `POST {base}/{did}` with the signed op as plain JSON — the directory's
/// submit format (dag-cbor is only ever the signing encoding). Mirrors the Go
/// bridge's `SubmitOperation`, including carrying the refusal body into the
/// error: a rejected tombstone's reason is the only diagnostic the user gets.
pub async fn submit_tombstone(
    directory_base_url: &str,
    did: &str,
    op: &TombstoneOp,
) -> Result<(), TombstoneError> {
    let body = serde_json::to_vec(op).map_err(|e| TombstoneError::Encode(e.to_string()))?;
    crate::directory_submit::post_plc_op(directory_base_url, did, body)
        .await
        .map_err(TombstoneError::Submit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier as _;

    const USER_KEY: &str = "did:key:zDnaeUserSeniorKey";
    const BOX_KEY: &str = "did:key:zQ3shBoxJuniorKey"; // gitleaks:allow
    const HEAD_CID: &str = "bafyreichhead";

    fn entry(
        cid: &str,
        op_type: &str,
        rotation_keys: &[&str],
        nullified: bool,
    ) -> serde_json::Value {
        crate::test_fixtures::plc_operation_entry_json(
            cid,
            op_type,
            rotation_keys,
            nullified,
            "2026-07-29T00:00:00Z",
        )
    }

    fn log(entries: &[serde_json::Value]) -> Vec<u8> {
        serde_json::to_vec(&entries).unwrap()
    }

    /// An entry whose op publishes a PDS service, as every op this system mints
    /// does (the `atproto_pds` entry pointing at the hosting PDS).
    fn entry_with_pds(cid: &str, endpoint: &str) -> serde_json::Value {
        let mut e = entry(cid, "plc_operation", &[USER_KEY, BOX_KEY], false);
        e["operation"]["services"] = serde_json::json!({
            "atproto_pds": {"type": "AtprotoPersonalDataServer", "endpoint": endpoint}
        });
        e
    }

    // ── The sweep probe ─────────────────────────────────────────────────────

    /// Where to ask comes from the DID's OWN published record, never from
    /// anything this box says — the same no-nest-in-the-middle property the
    /// seniority check has. A box asked to confirm its own sweep would be
    /// answering a question about its own conduct.
    #[test]
    fn the_pds_to_ask_is_read_off_the_published_operation() {
        let head = standing_head(&log(&[entry_with_pds(HEAD_CID, "https://pds.example.com")]))
            .expect("standing head");
        assert_eq!(head.pds_endpoint, "https://pds.example.com");
    }

    /// A did:plc this system did not mint (or one already moved off this PDS)
    /// declares no such service, and there is then nothing to ask. Unanswerable
    /// is a quiet retry, never a licence to retire.
    #[test]
    fn a_head_declaring_no_pds_leaves_the_question_unanswerable() {
        let head = standing_head(&log(&[entry(
            HEAD_CID,
            "plc_operation",
            &[USER_KEY],
            false,
        )]))
        .expect("standing head");
        assert!(head.pds_endpoint.is_empty());
    }

    /// The answer is keyed on the lexicon's error NAME, not the HTTP status:
    /// `getRepoStatus` answers 400 for a DID it serves no repo for AND for a
    /// malformed request, and reading the status alone would take the second
    /// for the first — retiring while the deletes were still unpublished.
    #[test]
    fn only_repo_not_found_means_the_sweep_finished() {
        assert_eq!(
            interpret_repo_status_error(400, br#"{"error":"RepoNotFound","message":"gone"}"#)
                .expect("a named RepoNotFound is an answer"),
            SweepProbe::RepoGone
        );
        for (status, body) in [
            (
                400u16,
                &br#"{"error":"InvalidRequest","message":"did is required"}"#[..],
            ),
            (
                400,
                br#"{"error":"RepoDeactivated","message":"stepped down"}"#,
            ),
            (500, br#"{"error":"InternalServerError","message":"boom"}"#),
            (502, b"<html>gateway</html>"),
            (429, b""),
        ] {
            assert!(
                interpret_repo_status_error(status, body).is_err(),
                "HTTP {status} {} must leave the question unanswered",
                String::from_utf8_lossy(body)
            );
        }
    }

    // ── The signed bytes ────────────────────────────────────────────────────

    /// The signing input is what the directory validates against, so pin it to
    /// the byte. A change here is a change to what every already-published
    /// signature means, and it cannot be caught by any round-trip test.
    #[test]
    fn unsigned_cbor_is_canonical_and_byte_stable() {
        let bytes = unsigned_tombstone_cbor("bafyabc").unwrap();
        #[rustfmt::skip]
        let want: Vec<u8> = [
            &[0xa2u8][..],                              // map, 2 pairs
            &[0x64], b"prev",                           // "prev"  (length-first: 4 < 13)
            &[0x67], b"bafyabc",                        // the cid, as a plain string
            &[0x64], b"type",                           // "type"
            &[0x6d], b"plc_tombstone",                  // the op type
        ]
        .concat();
        assert_eq!(bytes, want, "canonical dag-cbor of the unsigned tombstone");
    }

    /// `prev` before `type` is the canonical order, and the encoder must impose
    /// it rather than inherit the struct's declaration. Proven by encoding the
    /// same pair through a map declared the other way round.
    #[test]
    fn map_key_order_is_imposed_by_the_encoder_not_the_declaration() {
        #[derive(Serialize)]
        struct Reversed<'a> {
            #[serde(rename = "type")]
            op_type: &'a str,
            prev: &'a str,
        }
        let reversed = fauna_protocol::encode_canonical(&Reversed {
            op_type: OP_TYPE_TOMBSTONE,
            prev: "bafyabc",
        })
        .unwrap();
        assert_eq!(
            reversed.to_vec(),
            unsigned_tombstone_cbor("bafyabc").unwrap(),
            "canonical encoding must not depend on field declaration order"
        );
    }

    /// The signature verifies against the key's public half, over exactly the
    /// canonical dag-cbor — not over the JSON body, and not over the op with
    /// `sig` included.
    #[test]
    fn signature_verifies_over_the_unsigned_cbor() {
        let key = crate::rotation_key::generate_rotation_key(1_700_000_000);
        let scalar: [u8; 32] = *key.secret_scalar;
        let op = sign_tombstone(HEAD_CID, &scalar).unwrap();

        assert_eq!(op.op_type, OP_TYPE_TOMBSTONE);
        assert_eq!(op.prev, HEAD_CID);

        let raw = B64URL.decode(&op.sig).expect("sig is base64url-no-pad");
        assert_eq!(raw.len(), 64, "P1363 r‖s, never DER");
        let sig = p256::ecdsa::Signature::from_slice(&raw).unwrap();
        let signing = SigningKey::from_slice(&scalar).unwrap();
        let verifying = VerifyingKey::from(&signing);
        verifying
            .verify(&unsigned_tombstone_cbor(HEAD_CID).unwrap(), &sig)
            .expect("signature verifies over the canonical dag-cbor");
    }

    /// ATProto requires low-S. RustCrypto's P-256 signer does not normalize on
    /// its own, so assert the property directly rather than trusting the
    /// upstream default — the failure it guards against is a directory that
    /// rejects roughly half of all retirements.
    ///
    /// Self-proving: it also signs each input *without* normalizing and
    /// asserts that some of those come out high-S. Without that half the test
    /// would still pass if `normalize_s` became a no-op, or if this curve's
    /// signer started normalizing on its own and the call here were dropped —
    /// i.e. it would stop testing the thing it is named for.
    #[test]
    fn signature_is_low_s_and_would_not_be_without_normalizing() {
        let key = crate::rotation_key::generate_rotation_key(7);
        let scalar: [u8; 32] = *key.secret_scalar;
        let signing = SigningKey::from_slice(&scalar).unwrap();
        let mut unnormalized_high_s = 0;
        // Many distinct signing inputs: signing is deterministic (RFC 6979), so
        // roughly half of a decent sample lands high-S if left alone.
        for i in 0..64 {
            let prev = format!("bafycid{i}");
            let op = sign_tombstone(&prev, &scalar).unwrap();
            let raw = B64URL.decode(&op.sig).unwrap();
            let sig = p256::ecdsa::Signature::from_slice(&raw).unwrap();
            assert!(
                sig.normalize_s().is_none(),
                "signature {i} is high-S — the directory would reject it"
            );

            let raw_sig: p256::ecdsa::Signature =
                signing.sign(&unsigned_tombstone_cbor(&prev).unwrap());
            if raw_sig.normalize_s().is_some() {
                unnormalized_high_s += 1;
            }
        }
        assert!(
            unnormalized_high_s > 0,
            "no sampled signature was high-S before normalizing — this test \
             cannot distinguish a working normalization from a missing one"
        );
    }

    #[test]
    fn a_malformed_scalar_is_refused_before_anything_is_built() {
        let err = sign_tombstone(HEAD_CID, &[0u8; 32]).unwrap_err();
        assert!(matches!(err, TombstoneError::BadRotationKey), "got {err:?}");
    }

    // ── Reading the directory's head ────────────────────────────────────────

    #[test]
    fn head_is_the_last_standing_op() {
        let body = log(&[
            entry("bafygenesis", "plc_operation", &[USER_KEY, BOX_KEY], false),
            entry(HEAD_CID, "plc_operation", &[USER_KEY, BOX_KEY], false),
        ]);
        let head = standing_head(&body).unwrap();
        assert_eq!(head.cid, HEAD_CID);
        assert!(!head.is_tombstoned());
        assert_eq!(head.rotation_keys, vec![USER_KEY, BOX_KEY]);
    }

    /// A nullified op is not part of the chain, so chaining `prev` to it would
    /// be rejected — the head is the last op that still stands.
    #[test]
    fn nullified_entries_are_skipped_when_picking_the_head() {
        let body = log(&[
            entry("bafygenesis", "plc_operation", &[USER_KEY], false),
            entry("bafycontested", "plc_operation", &[BOX_KEY], true),
        ]);
        assert_eq!(standing_head(&body).unwrap().cid, "bafygenesis");
    }

    #[test]
    fn an_already_tombstoned_log_reports_itself_as_such() {
        let body = log(&[
            entry("bafygenesis", "plc_operation", &[USER_KEY], false),
            entry("bafytomb", OP_TYPE_TOMBSTONE, &[], false),
        ]);
        assert!(standing_head(&body).unwrap().is_tombstoned());
    }

    #[test]
    fn a_head_without_a_cid_is_refused_rather_than_chained_to_nothing() {
        let body = log(&[entry("", "plc_operation", &[USER_KEY], false)]);
        assert!(matches!(
            standing_head(&body).unwrap_err(),
            TombstoneError::HeadWithoutCid
        ));
    }

    // ── The step decision ───────────────────────────────────────────────────

    /// THE deadlock regression, pinned on the
    /// raw wire shape end to end: a published tombstone head declares no
    /// services, so a shape that asks "can I probe the sweep?" before "is it
    /// already retired?" answers "cannot tell" FOREVER — `AlreadyRetired`
    /// becomes unreachable and a report that failed after a successful submit
    /// can never be re-reported. The decision must read the head's own type
    /// first and never route a tombstone into the probe.
    #[test]
    fn a_tombstoned_serviceless_head_decides_already_retired_never_the_probe() {
        let body = log(&[
            entry("bafygenesis", "plc_operation", &[USER_KEY], false),
            entry("bafytomb", OP_TYPE_TOMBSTONE, &[], false),
        ]);
        let head = standing_head(&body).expect("standing head");
        assert!(
            head.pds_endpoint.is_empty(),
            "the premise: nothing to probe"
        );
        assert_eq!(
            retirement_step(&head).expect("a tombstone is an ANSWER, not an unanswerable"),
            StepPlan::AlreadyRetired
        );
    }

    /// The live arms around it: a live head with a PDS is probed there; a live
    /// head without one leaves the sweep question unanswerable (quiet-retry,
    /// never a licence to retire early).
    #[test]
    fn a_live_head_is_probed_at_its_published_pds_or_left_unanswerable() {
        let head = standing_head(&log(&[entry_with_pds(HEAD_CID, "https://pds.example.com")]))
            .expect("standing head");
        assert_eq!(
            retirement_step(&head).unwrap(),
            StepPlan::ProbeSweep {
                pds_endpoint: "https://pds.example.com"
            }
        );

        let head = standing_head(&log(&[entry(
            HEAD_CID,
            "plc_operation",
            &[USER_KEY],
            false,
        )]))
        .expect("standing head");
        assert!(matches!(
            retirement_step(&head).unwrap_err(),
            TombstoneError::Probe(_)
        ));
    }

    #[test]
    fn an_empty_log_is_a_quiet_directory_failure() {
        assert!(matches!(
            standing_head(b"[]").unwrap_err(),
            TombstoneError::Directory(VerifyFailure::NoStandingOps)
        ));
        assert!(matches!(
            standing_head(b"not json").unwrap_err(),
            TombstoneError::Directory(VerifyFailure::Parse(_))
        ));
    }

    // ── The submitted body ──────────────────────────────────────────────────

    /// The directory reads JSON with the spec's own key spelling; `op_type` must
    /// serialize as `type`, and no other field may appear.
    #[test]
    fn submitted_json_carries_exactly_the_three_spec_fields() {
        let op = TombstoneOp {
            prev: HEAD_CID.into(),
            op_type: OP_TYPE_TOMBSTONE.into(),
            sig: "c2ln".into(),
        };
        let v: serde_json::Value = serde_json::to_value(&op).unwrap();
        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["prev", "sig", "type"]);
        assert_eq!(obj["type"], OP_TYPE_TOMBSTONE);
        assert_eq!(obj["prev"], HEAD_CID);
    }

    /// After a fresh-key re-mint the ring holds the retired DID's key beside
    /// the live one's — the signer for THIS DID is whichever key its
    /// published head lists, never a ring position.
    #[test]
    fn select_signer_picks_the_head_listed_key_not_a_ring_position() {
        use fauna_core::data::AtprotoRotationKey;
        let key = |scalar: u8, pubkey: &str| AtprotoRotationKey {
            secret_scalar: [scalar; 32].into(),
            pubkey_did_key: pubkey.into(),
            created_at: u64::from(scalar),
            published_for_dids: Vec::new(),
        };
        let old = key(1, "did:key:zDnaeOldRetired");
        let fresh = key(2, "did:key:zDnaeFreshLive");
        let head = StandingHead {
            cid: HEAD_CID.into(),
            op_type: "plc_operation".into(),
            rotation_keys: vec!["did:key:zDnaeOldRetired".into(), "did:key:zQ3shBox".into()],
            pds_endpoint: String::new(),
        };

        // Ring order deliberately puts the fresh key first: selection must
        // still land on the key the head lists.
        let ring = [fresh.clone(), old.clone()];
        let signer = select_signer(&ring, &head).unwrap();
        assert_eq!(signer.pubkey_did_key, "did:key:zDnaeOldRetired");

        // No held key listed → refused with the published set, the terminal
        // (non-retry) class, before anything is signed or submitted.
        let ring = [fresh];
        let err = select_signer(&ring, &head).unwrap_err();
        match err {
            TombstoneError::SeniorKeyNotListed { published } => {
                assert_eq!(
                    published,
                    vec![
                        "did:key:zDnaeOldRetired".to_string(),
                        "did:key:zQ3shBox".to_string()
                    ]
                );
            }
            other => panic!("expected SeniorKeyNotListed, got {other:?}"),
        }
    }
}
