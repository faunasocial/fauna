//! Log inclusion — the fetch-layer companion that makes *publication* checkable
//! rather than assumed.
//!
//! Owner doc: `docs/goal/behavior/region-blocking.md` § The transparency log
//! (strengthened 2026-08-11, ratified-refutable at build time; built 2026-09-09).
//! A signature binds an artifact to an authority and says nothing about whether
//! it was ever published; the section's CT property — *log inclusion is what
//! makes a policy effective* — is delivered by **four pieces of evidence** a
//! consumer checks itself, and this module is that checker:
//!
//! 1. **Inclusion proof** — the log's head commit and the git object chain
//!    (commit → trees → blob) proving the artifact's exact bytes sit at their
//!    path in that head. A bounded hash walk over **SHA-256 git objects**; no
//!    git dependency here, only [`ObjectId::of_object`].
//! 2. **Head monotonicity** — the consumer persists the last head it accepted
//!    ([`AnchorState::last_accepted`]); a new head must *descend* from it, shown
//!    by a bounded parent-chain walk over the served commit objects. The log
//!    is **linear** (owner doc § Repository layout and mirrors): a commit
//!    naming a second parent is refused wherever it is parsed, so a fork can
//!    never be merged back into the line every consumer follows.
//! 3. **The compiled-in anchor** — [`COMPILED_IN_LOG_ANCHOR`], the log head a
//!    build shipped with, so a fresh consumer's first accept already descends
//!    from a known-honest head. **`None` today: the log is unpublished, so this
//!    is the pre-log era**, and [`AnchorState::is_pre_log`] is what makes that
//!    era a *stated* rule rather than an implied one (see [`admit_artifact`]).
//! 4. **Witnessed checkpoints** — the served head is a [`Checkpoint`] cosigned
//!    by a quorum of the [`WitnessRoster`]. The roster is **empty at version 0**,
//!    like the registry, and for the same reason: enrolling a witness is an
//!    administrative act that has not happened. The consequence is deliberate
//!    and stated in code: **evidence can never be accepted under an empty
//!    roster** ([`InclusionRejection::NoWitnessRoster`]), because the section
//!    makes the quorum the go-live bar — it arrives *before* the property is
//!    relied on, never as a retrofit.
//!
//! ## The witness consistency contract is enforced on both sides
//!
//! What a witness signs is a consistency claim, not an observation: *each
//! witness persists the last checkpoint it cosigned, and cosigns a new one only
//! if it descends from that one*. A stateless fetch-and-sign witness would
//! cosign — with no compromise at all — a history rewritten after the anchor,
//! which is exactly what fresh installs cannot detect on their own. So the
//! witness half of the contract lives here too, network-free, as
//! [`cosign_checkpoint`] over a persisted [`WitnessState`]: a witness fed a
//! non-descendant checkpoint **refuses to cosign**, and the future witness
//! binary is a thin shell over this function rather than a second reading of
//! the contract. The consumer half is [`check_inclusion`]'s descendance walk
//! plus the quorum count.
//!
//! ## What is here and what is not
//!
//! Everything here is network-free and WASM-usable on purpose (the module note
//! on the parent): fetching the bundle is the consumer's — the nest for the
//! feature plane today (`region_tier.rs`), the app for the content plane when
//! that plane's fetch is built. The evidence is a **companion beside the
//! envelope, never a field inside it**: a logged blob cannot contain the hash of
//! the commit that contains it, and an authority signs before publication. The
//! log serves the artifact at [`log_path`] (a tracked blob) and its evidence at
//! [`log_evidence_path`] (regenerated per head, beside it).

use std::collections::{BTreeMap, BTreeSet};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_bytes::ByteBuf;
use sha2::{Digest, Sha256};

use super::{MAX_KEY_ID_BYTES, PolicyArtifact, RegionCode};

/// The identity a checkpoint names. A cosignature over a checkpoint from some
/// *other* log — a test log, a fork — must not count here, however honest its
/// witnesses, so the consumer refuses any checkpoint naming a different log.
pub const REGION_LOG_ID: &str = "log.fauna.social";

/// The log head this build shipped with (§ The transparency log — *the
/// compiled-in anchor*).
///
/// **`None` is the pre-log era, and that is the true state, not a placeholder:**
/// the log is not yet published (an administrative act, like enrolling an
/// authority). When it is, the then-current head lands here beside the
/// compiled-in registry snapshot, in the same revision — and from that build on
/// an artifact with no evidence is refused ([`InclusionRejection::EvidenceRequired`]).
pub const COMPILED_IN_LOG_ANCHOR: Option<ObjectId> = None;

/// Domain separator under the checkpoint bytes a witness signs, so a witness
/// key can never be tricked into cosigning some other structure that happens
/// to encode the same way.
pub const CHECKPOINT_SIGNING_DOMAIN: &[u8] = b"fauna-region-log-checkpoint-v1\0";

/// Longest served commit object accepted. A commit is a few header lines and a
/// message; 64 KiB is orders of magnitude past any real one.
pub const MAX_COMMIT_BYTES: usize = 64 * 1024;

/// Longest served tree object accepted. The `regions/` tree could one day hold
/// every ISO 3166 row at ~50 bytes an entry; a mebibyte is room for far more.
pub const MAX_TREE_BYTES: usize = 1024 * 1024;

/// Deepest commit chain a bundle may carry — the descendance walk's bound. A
/// consumer offline longer than this many log commits walks deeper via ranged
/// fetches (the one qualified edge § The transparency log names); the ordinary
/// bundle stays far under it.
pub const MAX_CHAIN_COMMITS: usize = 4096;

/// Deepest artifact path accepted (`regions/<region>/<kind>.cbor` is three).
pub const MAX_PATH_DEPTH: usize = 8;

/// Most cosignatures examined on one checkpoint; a roster is a handful.
pub const MAX_COSIGNATURES: usize = 64;

/// Longest `log_id` decoded before it is compared to anything.
pub const MAX_LOG_ID_BYTES: usize = 128;

const OBJECT_ID_LEN: usize = 32;
const ED25519_PUBLIC_KEY_LEN: usize = 32;
const ED25519_SIGNATURE_LEN: usize = 64;

/// The path an artifact sits at in the log — one home for the layout the fetch
/// URL, the inclusion proof and the log's own publisher all agree on.
pub fn log_path(region: &RegionCode, payload_kind: &str) -> String {
    format!("regions/{}/{payload_kind}.cbor", region.as_str())
}

/// Where the log serves an artifact's inclusion evidence: **beside** the
/// artifact, not inside the tree (it names the head that contains the artifact,
/// so it cannot itself be a tracked blob).
pub fn log_evidence_path(region: &RegionCode, payload_kind: &str) -> String {
    format!("regions/{}/{payload_kind}.evidence.cbor", region.as_str())
}

/// A git object id in the repository's **SHA-256** object format — 32 bytes.
///
/// The object hash is security-load-bearing (§ The transparency log: git's
/// default SHA-1 is not acceptable for a chain that *is* the property), which
/// is why the type is fixed-width and validated on decode rather than a
/// free-form byte string.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId([u8; OBJECT_ID_LEN]);

impl ObjectId {
    pub const LEN: usize = OBJECT_ID_LEN;

    pub const fn from_bytes(bytes: [u8; OBJECT_ID_LEN]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; OBJECT_ID_LEN] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse the 64-hex-digit form git prints.
    pub fn parse_hex(text: &str) -> Option<Self> {
        if text.len() != OBJECT_ID_LEN * 2 {
            return None;
        }
        let bytes = hex::decode(text).ok()?;
        let mut id = [0u8; OBJECT_ID_LEN];
        id.copy_from_slice(&bytes);
        Some(Self(id))
    }

    /// Git's object hash: `SHA-256("<kind> <len>\0" ‖ content)`.
    ///
    /// The one definition every proof step uses; pinned against git's own
    /// published empty-object ids in the tests.
    pub fn of_object(kind: &str, content: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(kind.as_bytes());
        hasher.update(b" ");
        hasher.update(content.len().to_string().as_bytes());
        hasher.update([0u8]);
        hasher.update(content);
        Self(hasher.finalize().into())
    }

    pub fn of_blob(content: &[u8]) -> Self {
        Self::of_object("blob", content)
    }

    pub fn of_tree(content: &[u8]) -> Self {
        Self::of_object("tree", content)
    }

    pub fn of_commit(content: &[u8]) -> Self {
        Self::of_object("commit", content)
    }
}

impl std::fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ObjectId({})", self.to_hex())
    }
}

impl std::fmt::Display for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for ObjectId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for ObjectId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = ByteBuf::deserialize(deserializer)?;
        let bytes: [u8; OBJECT_ID_LEN] = bytes.into_vec().try_into().map_err(|v: Vec<u8>| {
            serde::de::Error::invalid_length(v.len(), &"a 32-byte SHA-256 git object id")
        })?;
        Ok(Self(bytes))
    }
}

/// The consistency claim a witness cosigns: *this head is the current head of
/// this log*. What makes the claim bind is the witness's own memory
/// ([`WitnessState`]) — see the module note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Which log this is a checkpoint of — [`REGION_LOG_ID`] for the one a
    /// consumer accepts.
    pub log_id: String,
    /// The head commit the log had when this checkpoint was cut.
    pub head: ObjectId,
    /// Forward-compat catch-all: a newer log may add fields; they round-trip
    /// untouched and stay under the cosignature.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, fauna_cbor::Value>,
}

/// One witness's signature over a [`Checkpoint`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cosignature {
    /// Which roster member signed.
    pub witness_key_id: String,
    /// Ed25519 over [`checkpoint_signing_input`].
    #[serde(with = "serde_bytes")]
    pub sig: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, fauna_cbor::Value>,
}

/// The evidence the log serves beside an artifact — the four pieces of § The
/// transparency log in one decodable companion.
///
/// Object contents are carried **without** git's `"<kind> <len>\0"` header;
/// the checker prepends it when hashing ([`ObjectId::of_object`]), so a served
/// object can only ever be hashed as the kind the proof step expects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InclusionEvidence {
    /// The head commit this evidence proves inclusion in.
    pub head: ObjectId,
    /// Commit objects, **head first**, each subsequent one a parent of the one
    /// before, reaching back at least as far as the consumer's persisted head.
    pub chain: Vec<ByteBuf>,
    /// Tree objects from the root down to the artifact's parent directory —
    /// one per path component of [`log_path`].
    pub trees: Vec<ByteBuf>,
    /// The witnessed checkpoint naming `head`.
    pub checkpoint: Checkpoint,
    /// The witnesses' cosignatures over `checkpoint`.
    pub cosignatures: Vec<Cosignature>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, fauna_cbor::Value>,
}

/// One enrolled witness. `retired_at` makes rotation a roster revision rather
/// than a deletion, exactly as `AuthorityKey` does for the registry — but a
/// retired witness's cosignatures **stop counting** (a checkpoint has no era
/// to check against, and a retired key is retired because it should sign
/// nothing further).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Witness {
    pub key_id: String,
    #[serde(with = "serde_bytes")]
    pub public_key: Vec<u8>,
    pub enrolled_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<u64>,
}

/// The witness roster — an administrative enrollment act like the registry's
/// own, shipped compiled in and refreshed over the same channel.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WitnessRoster {
    #[serde(default)]
    pub version: u64,
    /// How many **distinct, current** witnesses must cosign a checkpoint.
    /// `0` — the compiled-in state — means no checkpoint can be accepted at all.
    #[serde(default)]
    pub quorum: u32,
    #[serde(default)]
    pub witnesses: Vec<Witness>,
}

impl WitnessRoster {
    /// A current (un-retired) witness by key id.
    pub fn current(&self, key_id: &str) -> Option<&Witness> {
        self.witnesses
            .iter()
            .find(|w| w.key_id == key_id && w.retired_at.is_none())
    }

    /// Whether any checkpoint could satisfy this roster at all.
    pub fn has_quorum(&self) -> bool {
        self.quorum > 0
            && self
                .witnesses
                .iter()
                .filter(|w| w.retired_at.is_none())
                .count()
                >= self.quorum as usize
    }
}

/// The compiled-in roster: **empty, quorum 0, at version 0** — no witness has
/// been enrolled. See the module note on why that refuses every piece of
/// evidence rather than waving it through.
pub fn compiled_in_witness_roster() -> WitnessRoster {
    WitnessRoster::default()
}

/// What a consumer knows about the log's history: the head its build shipped
/// with and the last head it accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorState {
    /// [`COMPILED_IN_LOG_ANCHOR`] as this build has it.
    pub compiled_in: Option<ObjectId>,
    /// The last head this consumer accepted — persisted by the consumer, since
    /// § The transparency log's monotonicity is a property of *its* history.
    pub last_accepted: Option<ObjectId>,
}

impl AnchorState {
    /// This build's anchor with nothing accepted yet — the fresh-install state.
    pub fn compiled_in() -> Self {
        Self {
            compiled_in: COMPILED_IN_LOG_ANCHOR,
            last_accepted: None,
        }
    }

    /// This build's anchor plus what the consumer persisted.
    pub fn with_last_accepted(last_accepted: Option<ObjectId>) -> Self {
        Self {
            compiled_in: COMPILED_IN_LOG_ANCHOR,
            last_accepted,
        }
    }

    /// **The pre-log era**: no compiled-in anchor and nothing ever accepted.
    /// The only state in which an artifact without evidence is admitted
    /// ([`admit_artifact`]).
    pub fn is_pre_log(&self) -> bool {
        self.compiled_in.is_none() && self.last_accepted.is_none()
    }

    /// The head a new one must descend from: what this consumer last accepted,
    /// else what its build shipped with.
    ///
    /// The persisted head wins when both exist because it was itself accepted
    /// as a descendant of the anchor of the build that accepted it; requiring
    /// descendance from an *older* compiled-in anchor as well would force every
    /// bundle to carry history back to the oldest build in the fleet.
    pub fn trusted_head(&self) -> Option<ObjectId> {
        self.last_accepted.or(self.compiled_in)
    }
}

/// Why evidence — or its absence — was refused. Every arm is a refusal to
/// *apply* the artifact; the anchor is unchanged on every one of them.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InclusionRejection {
    /// No evidence was served and this consumer is past the pre-log era.
    #[error("no inclusion evidence, and this consumer is past the pre-log anchor")]
    EvidenceRequired,
    #[error("the commit chain is empty")]
    EmptyChain,
    #[error("the commit chain has {0} commits, over the {MAX_CHAIN_COMMITS} bound")]
    ChainTooLong(usize),
    #[error("a served {kind} object is {len} bytes, over its bound")]
    ObjectTooLarge { kind: &'static str, len: usize },
    #[error("{0} trees served, over the {MAX_PATH_DEPTH} path-depth bound")]
    PathTooDeep(usize),
    #[error("{0} cosignatures served, over the {MAX_COSIGNATURES} bound")]
    TooManyCosignatures(usize),
    #[error("checkpoint names log {0:?}, not {REGION_LOG_ID:?}")]
    WrongLog(String),
    #[error("the served head is {claimed} but the first chain commit hashes to {computed}")]
    HeadMismatch {
        claimed: ObjectId,
        computed: ObjectId,
    },
    #[error("the checkpoint names head {checkpoint} but the evidence proves {evidence}")]
    CheckpointHeadMismatch {
        checkpoint: ObjectId,
        evidence: ObjectId,
    },
    #[error("malformed commit object: {0}")]
    MalformedCommit(String),
    #[error("malformed tree object: {0}")]
    MalformedTree(String),
    #[error("inclusion proof broken at {path:?}: {why}")]
    ProofBroken { path: String, why: String },
    #[error("the artifact's bytes are not the blob at {path:?} in head {head}")]
    ArtifactNotAtPath { path: String, head: ObjectId },
    #[error("chain commit {index} is not a parent of the commit before it")]
    ChainBroken { index: usize },
    /// The log is linear — one branch, every commit exactly one parent — so a
    /// commit naming a second parent is refused before any walk through it:
    /// a merge would let a fork be reconciled back into the public line, and
    /// "descends from" would then be satisfiable by naming the trusted head
    /// as *any* parent (owner doc § Repository layout and mirrors).
    #[error("chain commit {index} has a second parent; the log is linear and a merge is refused")]
    MergeCommit { index: usize },
    #[error("head {head} does not descend from the trusted head {trusted}")]
    NotDescendant { head: ObjectId, trusted: ObjectId },
    /// The roster can satisfy no checkpoint — the compiled-in state today.
    #[error("no witness roster is enrolled, so no checkpoint can be accepted")]
    NoWitnessRoster,
    #[error("checkpoint cosigned by {have} current witnesses, quorum is {needed}")]
    NoWitnessQuorum { needed: u32, have: u32 },
    #[error("canonical encode failed: {0}")]
    Encode(String),
}

/// Why a witness declined to cosign — the consistency contract, witness side.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WitnessRefusal {
    #[error("checkpoint names log {0:?}, not {REGION_LOG_ID:?}")]
    WrongLog(String),
    /// The checkpoint does not descend from the last one this witness
    /// cosigned (or its chain is unusable) — the contract's whole point.
    #[error("refusing to cosign: {0}")]
    NotDescendant(InclusionRejection),
    #[error("canonical encode failed: {0}")]
    Encode(String),
}

/// What a witness remembers between checkpoints: the last head it cosigned.
/// **The memory is the contract** — a witness that forgets it is the stateless
/// witness the module note explains would cosign a rewritten history.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WitnessState {
    pub last_cosigned: Option<ObjectId>,
}

/// The bytes a cosignature covers: the domain separator, then the checkpoint's
/// canonical dag-cbor. One function for the witness and the consumer, so the
/// two can never drift about what was signed.
pub fn checkpoint_signing_input(checkpoint: &Checkpoint) -> Result<Vec<u8>, InclusionRejection> {
    let body = fauna_cbor::encode_canonical(checkpoint)
        .map_err(|e| InclusionRejection::Encode(e.to_string()))?;
    let mut input = Vec::with_capacity(CHECKPOINT_SIGNING_DOMAIN.len() + body.len());
    input.extend_from_slice(CHECKPOINT_SIGNING_DOMAIN);
    input.extend_from_slice(&body);
    Ok(input)
}

/// Cosign a checkpoint **as a witness** — the consistency contract, enforced.
///
/// Refuses unless `checkpoint.head` is proven, by `chain`, to descend from the
/// last head this witness cosigned; a fresh witness (`last_cosigned: None`)
/// cosigns its first checkpoint unconditionally and remembers it. Returns the
/// cosignature and the state to persist **before** the cosignature is
/// released — a witness that signs first and persists second can be made to
/// forget by a crash between the two.
///
/// Fauna's future witness binary is a shell over this function; it exists here
/// so the contract has one reading and one test.
///
/// # Errors
///
/// [`WitnessRefusal::NotDescendant`] carrying the chain finding, or
/// [`WitnessRefusal::WrongLog`].
pub fn cosign_checkpoint(
    state: &WitnessState,
    checkpoint: &Checkpoint,
    chain: &[ByteBuf],
    witness_key_id: &str,
    signing: &SigningKey,
) -> Result<(Cosignature, WitnessState), WitnessRefusal> {
    if checkpoint.log_id != REGION_LOG_ID {
        return Err(WitnessRefusal::WrongLog(checkpoint.log_id.clone()));
    }
    let head = chain_head(chain, checkpoint.head).map_err(WitnessRefusal::NotDescendant)?;
    if let Some(last) = state.last_cosigned {
        descends_from(chain, head, last).map_err(WitnessRefusal::NotDescendant)?;
    }
    let input =
        checkpoint_signing_input(checkpoint).map_err(|e| WitnessRefusal::Encode(e.to_string()))?;
    let cosignature = Cosignature {
        witness_key_id: witness_key_id.to_string(),
        sig: signing.sign(&input).to_bytes().to_vec(),
        extra: BTreeMap::new(),
    };
    Ok((
        cosignature,
        WitnessState {
            last_cosigned: Some(head),
        },
    ))
}

/// Admit an artifact with or without evidence — **the pre-log rule, stated**.
///
/// - Evidence present → [`check_inclusion`]; failing evidence refuses the
///   artifact, whatever era the consumer is in.
/// - No evidence → accepted **only while the anchor is the compiled-in empty
///   one** ([`AnchorState::is_pre_log`]) and the anchor is returned unchanged;
///   past it, [`InclusionRejection::EvidenceRequired`].
///
/// A consumer whose compiled-in anchor is `None` and that has never accepted a
/// head is exactly a build from before the log was published talking to a log
/// that does not exist yet; that is today's every deployment, and it is the
/// only shape in which "no evidence" is not a stripped bundle.
pub fn admit_artifact(
    artifact: &PolicyArtifact,
    evidence: Option<&InclusionEvidence>,
    anchor: &AnchorState,
    roster: &WitnessRoster,
) -> Result<AnchorState, InclusionRejection> {
    match evidence {
        Some(evidence) => check_inclusion(artifact, evidence, anchor, roster),
        None if anchor.is_pre_log() => Ok(anchor.clone()),
        None => Err(InclusionRejection::EvidenceRequired),
    }
}

/// Check the four pieces of evidence for one artifact, returning the anchor
/// to persist on success (the served head becomes `last_accepted`).
///
/// Order, cheapest first: bounds; log identity; the head hash; the inclusion
/// proof (a hash walk); descendance from the trusted head (a hash walk); the
/// witness quorum last, since it is the only step that verifies signatures.
///
/// # Errors
///
/// One [`InclusionRejection`] per failed check. The anchor passed in is never
/// modified — a refusal leaves the consumer exactly where it was.
pub fn check_inclusion(
    artifact: &PolicyArtifact,
    evidence: &InclusionEvidence,
    anchor: &AnchorState,
    roster: &WitnessRoster,
) -> Result<AnchorState, InclusionRejection> {
    check_bounds(evidence)?;
    if evidence.checkpoint.log_id != REGION_LOG_ID {
        return Err(InclusionRejection::WrongLog(
            evidence.checkpoint.log_id.clone(),
        ));
    }

    let head = chain_head(&evidence.chain, evidence.head)?;
    if evidence.checkpoint.head != head {
        return Err(InclusionRejection::CheckpointHeadMismatch {
            checkpoint: evidence.checkpoint.head,
            evidence: head,
        });
    }

    let path = log_path(&artifact.region, &artifact.payload_kind);
    let artifact_bytes = fauna_cbor::encode_canonical(artifact)
        .map_err(|e| InclusionRejection::Encode(e.to_string()))?;
    prove_blob_at_path(
        &evidence.chain[0],
        &evidence.trees,
        &path,
        ObjectId::of_blob(&artifact_bytes),
        head,
    )?;

    if let Some(trusted) = anchor.trusted_head() {
        descends_from(&evidence.chain, head, trusted)?;
    }

    check_quorum(&evidence.checkpoint, &evidence.cosignatures, roster)?;

    Ok(AnchorState {
        compiled_in: anchor.compiled_in,
        last_accepted: Some(head),
    })
}

fn check_bounds(evidence: &InclusionEvidence) -> Result<(), InclusionRejection> {
    if evidence.chain.is_empty() {
        return Err(InclusionRejection::EmptyChain);
    }
    if evidence.chain.len() > MAX_CHAIN_COMMITS {
        return Err(InclusionRejection::ChainTooLong(evidence.chain.len()));
    }
    if let Some(commit) = evidence.chain.iter().find(|c| c.len() > MAX_COMMIT_BYTES) {
        return Err(InclusionRejection::ObjectTooLarge {
            kind: "commit",
            len: commit.len(),
        });
    }
    if evidence.trees.len() > MAX_PATH_DEPTH {
        return Err(InclusionRejection::PathTooDeep(evidence.trees.len()));
    }
    if let Some(tree) = evidence.trees.iter().find(|t| t.len() > MAX_TREE_BYTES) {
        return Err(InclusionRejection::ObjectTooLarge {
            kind: "tree",
            len: tree.len(),
        });
    }
    if evidence.cosignatures.len() > MAX_COSIGNATURES {
        return Err(InclusionRejection::TooManyCosignatures(
            evidence.cosignatures.len(),
        ));
    }
    if evidence.checkpoint.log_id.len() > MAX_LOG_ID_BYTES {
        return Err(InclusionRejection::WrongLog(
            evidence.checkpoint.log_id.chars().take(16).collect(),
        ));
    }
    Ok(())
}

/// The chain's first commit must hash to the claimed head.
fn chain_head(chain: &[ByteBuf], claimed: ObjectId) -> Result<ObjectId, InclusionRejection> {
    let first = chain.first().ok_or(InclusionRejection::EmptyChain)?;
    if chain.len() > MAX_CHAIN_COMMITS {
        return Err(InclusionRejection::ChainTooLong(chain.len()));
    }
    let computed = ObjectId::of_commit(first);
    if computed != claimed {
        return Err(InclusionRejection::HeadMismatch { claimed, computed });
    }
    Ok(computed)
}

/// Walk `chain` from `head` until `trusted` is met, checking at each step that
/// the next served commit is *the* parent of the current one — the log is
/// linear, and [`parse_commit`] has already refused any commit with a second
/// parent, so `parents` holds at most one id here. Meeting `trusted` at step
/// 0 — the head *is* the trusted head — is a re-fetch of the same state, and
/// descends trivially.
fn descends_from(
    chain: &[ByteBuf],
    head: ObjectId,
    trusted: ObjectId,
) -> Result<(), InclusionRejection> {
    let mut current = head;
    for (index, commit) in chain.iter().enumerate() {
        if index > 0 && ObjectId::of_commit(commit) != current {
            return Err(InclusionRejection::ChainBroken { index });
        }
        if current == trusted {
            return Ok(());
        }
        let header = parse_commit(commit, index)?;
        match chain.get(index + 1) {
            Some(next) => {
                let next_id = ObjectId::of_commit(next);
                if !header.parents.contains(&next_id) {
                    return Err(InclusionRejection::ChainBroken { index: index + 1 });
                }
                current = next_id;
            }
            None => break,
        }
    }
    Err(InclusionRejection::NotDescendant { head, trusted })
}

/// The inclusion proof: head commit → root tree → … → the blob at `path`.
fn prove_blob_at_path(
    head_commit: &[u8],
    trees: &[ByteBuf],
    path: &str,
    blob: ObjectId,
    head: ObjectId,
) -> Result<(), InclusionRejection> {
    let components: Vec<&str> = path.split('/').collect();
    if components.len() != trees.len() {
        return Err(InclusionRejection::ProofBroken {
            path: path.to_string(),
            why: format!(
                "{} trees served for a {}-component path",
                trees.len(),
                components.len()
            ),
        });
    }
    let header = parse_commit(head_commit, 0)?;
    let mut expected_tree = header.tree;
    for (depth, (tree, component)) in trees.iter().zip(&components).enumerate() {
        let computed = ObjectId::of_tree(tree);
        if computed != expected_tree {
            return Err(InclusionRejection::ProofBroken {
                path: path.to_string(),
                why: format!(
                    "tree at depth {depth} hashes to {computed}, expected {expected_tree}"
                ),
            });
        }
        let entry =
            tree_entry(tree, component)?.ok_or_else(|| InclusionRejection::ProofBroken {
                path: path.to_string(),
                why: format!("no entry {component:?} at depth {depth}"),
            })?;
        let is_leaf = depth + 1 == components.len();
        match (is_leaf, entry.kind) {
            (false, TreeEntryKind::Directory) => expected_tree = entry.id,
            (true, TreeEntryKind::File) => {
                if entry.id != blob {
                    return Err(InclusionRejection::ArtifactNotAtPath {
                        path: path.to_string(),
                        head,
                    });
                }
            }
            (_, kind) => {
                return Err(InclusionRejection::ProofBroken {
                    path: path.to_string(),
                    why: format!("entry {component:?} at depth {depth} is a {kind:?}"),
                });
            }
        }
    }
    Ok(())
}

/// The quorum count: distinct, current roster members whose signature over the
/// checkpoint verifies. Unknown, retired, duplicate and invalid cosignatures
/// are skipped, never fatal — a hostile mirror appending junk cosignatures must
/// not be able to turn a witnessed checkpoint into a refused one.
fn check_quorum(
    checkpoint: &Checkpoint,
    cosignatures: &[Cosignature],
    roster: &WitnessRoster,
) -> Result<(), InclusionRejection> {
    if !roster.has_quorum() {
        return Err(InclusionRejection::NoWitnessRoster);
    }
    let input = checkpoint_signing_input(checkpoint)?;
    let mut counted: BTreeSet<&str> = BTreeSet::new();
    for cosignature in cosignatures {
        if cosignature.witness_key_id.len() > MAX_KEY_ID_BYTES
            || cosignature.sig.len() != ED25519_SIGNATURE_LEN
        {
            continue;
        }
        let Some(witness) = roster.current(&cosignature.witness_key_id) else {
            continue;
        };
        if witness.public_key.len() != ED25519_PUBLIC_KEY_LEN {
            continue;
        }
        let mut pk = [0u8; ED25519_PUBLIC_KEY_LEN];
        pk.copy_from_slice(&witness.public_key);
        let mut sig = [0u8; ED25519_SIGNATURE_LEN];
        sig.copy_from_slice(&cosignature.sig);
        let Ok(verifying) = VerifyingKey::from_bytes(&pk) else {
            continue;
        };
        if verifying
            .verify_strict(&input, &Signature::from_bytes(&sig))
            .is_ok()
        {
            counted.insert(witness.key_id.as_str());
        }
    }
    let have = counted.len() as u32;
    if have < roster.quorum {
        return Err(InclusionRejection::NoWitnessQuorum {
            needed: roster.quorum,
            have,
        });
    }
    Ok(())
}

struct CommitHeader {
    tree: ObjectId,
    parents: Vec<ObjectId>,
}

/// The two header lines the walk needs, from a commit object's content: the
/// `tree` line (always first) and the `parent` line — at most one, because the
/// log is linear: a second `parent` line is [`InclusionRejection::MergeCommit`]
/// at `index` (the commit's position in the served chain), refused here so
/// that every reader of a commit — the proof walk over the head and the
/// descendance walk on both the consumer and witness side — holds the same
/// rule. Everything else — author, committer, `gpgsig` and its continuation
/// lines, the message — is skipped, never interpreted.
fn parse_commit(content: &[u8], index: usize) -> Result<CommitHeader, InclusionRejection> {
    let malformed = |why: &str| InclusionRejection::MalformedCommit(why.to_string());
    let mut tree = None;
    let mut parents = Vec::new();
    for line in content.split(|&b| b == b'\n') {
        if line.is_empty() {
            break; // the header ends at the first blank line; the message follows
        }
        if line[0] == b' ' {
            continue; // a continuation line of a multi-line header (gpgsig)
        }
        let Some(space) = line.iter().position(|&b| b == b' ') else {
            return Err(malformed("header line without a space"));
        };
        let (key, value) = (&line[..space], &line[space + 1..]);
        let id = |what: &str| {
            std::str::from_utf8(value)
                .ok()
                .and_then(ObjectId::parse_hex)
                .ok_or_else(|| malformed(&format!("{what} line is not a SHA-256 object id")))
        };
        match key {
            b"tree" if tree.is_none() => tree = Some(id("tree")?),
            b"tree" => return Err(malformed("two tree lines")),
            b"parent" if parents.is_empty() => parents.push(id("parent")?),
            b"parent" => return Err(InclusionRejection::MergeCommit { index }),
            _ => {}
        }
    }
    Ok(CommitHeader {
        tree: tree.ok_or_else(|| malformed("no tree line"))?,
        parents,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeEntryKind {
    Directory,
    File,
    /// A symlink, submodule, or a mode this checker does not walk through.
    Other,
}

struct TreeEntry {
    kind: TreeEntryKind,
    id: ObjectId,
}

/// Find `name` in a tree object's content (`<mode> <name>\0<32-byte id>`,
/// repeated). Order is not relied on — git sorts entries, but the proof holds
/// whether or not a served tree is sorted, since the tree's hash is checked.
fn tree_entry(content: &[u8], name: &str) -> Result<Option<TreeEntry>, InclusionRejection> {
    let malformed = |why: &str| InclusionRejection::MalformedTree(why.to_string());
    let mut rest = content;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .position(|&b| b == b' ')
            .ok_or_else(|| malformed("entry without a mode"))?;
        let mode = &rest[..space];
        let after_mode = &rest[space + 1..];
        let nul = after_mode
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| malformed("entry name is not NUL-terminated"))?;
        let entry_name = &after_mode[..nul];
        let id_bytes = after_mode
            .get(nul + 1..nul + 1 + OBJECT_ID_LEN)
            .ok_or_else(|| malformed("entry id is short"))?;
        rest = &after_mode[nul + 1 + OBJECT_ID_LEN..];
        if entry_name == name.as_bytes() {
            let kind = match mode {
                b"40000" => TreeEntryKind::Directory,
                b"100644" | b"100755" => TreeEntryKind::File,
                _ => TreeEntryKind::Other,
            };
            let mut id = [0u8; OBJECT_ID_LEN];
            id.copy_from_slice(id_bytes);
            return Ok(Some(TreeEntry {
                kind,
                id: ObjectId::from_bytes(id),
            }));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::super::{PAYLOAD_KIND_FEATURE_POLICY, sign_artifact};
    use super::*;

    /// git's own SHA-256 empty-blob and empty-tree ids
    /// (`git hash-object -t blob /dev/null` under `--object-format=sha256`).
    #[test]
    fn object_ids_match_gits_sha256_object_format() {
        assert_eq!(
            ObjectId::of_blob(b"").to_hex(),
            "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813"
        );
        assert_eq!(
            ObjectId::of_tree(b"").to_hex(),
            "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321"
        );
    }

    #[test]
    fn an_object_id_round_trips_and_refuses_the_wrong_width() {
        let id = ObjectId::of_blob(b"x");
        let bytes = fauna_cbor::encode_canonical(&id).unwrap();
        let back: ObjectId = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(back, id);
        let short = fauna_cbor::encode_canonical(&ByteBuf::from(vec![1u8; 20])).unwrap();
        assert!(fauna_cbor::decode_strict::<ObjectId>(&short).is_err());
        assert_eq!(ObjectId::parse_hex(&id.to_hex()), Some(id));
    }

    // ── a synthetic SHA-256 object graph, built the way git would ──────────

    fn region() -> RegionCode {
        RegionCode::parse("NO").unwrap()
    }

    fn authority() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn artifact(sequence: u64) -> PolicyArtifact {
        sign_artifact(
            PolicyArtifact {
                region: region(),
                key_id: "k1".into(),
                sequence,
                issued_at: 1_000,
                payload_kind: PAYLOAD_KIND_FEATURE_POLICY.into(),
                payload: fauna_cbor::encode_canonical(
                    &std::collections::BTreeMap::<String, u64>::new(),
                )
                .unwrap(),
                sig: Vec::new(),
            },
            &authority(),
        )
        .unwrap()
    }

    fn tree_content(entries: &[(&str, &str, ObjectId)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (mode, name, id) in entries {
            out.extend_from_slice(mode.as_bytes());
            out.push(b' ');
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            out.extend_from_slice(id.as_bytes());
        }
        out
    }

    fn commit_content(tree: ObjectId, parents: &[ObjectId], message: &str) -> Vec<u8> {
        let mut out = format!("tree {tree}\n");
        for parent in parents {
            out.push_str(&format!("parent {parent}\n"));
        }
        out.push_str("author Log <log@example> 1 +0000\n");
        out.push_str("committer Log <log@example> 1 +0000\n");
        out.push_str("gpgsig -----BEGIN-----\n iQEz\n -----END-----\n");
        out.push('\n');
        out.push_str(message);
        out.into_bytes()
    }

    /// One log commit holding `artifact` at its path: returns the commit's
    /// content plus the three trees root → leaf-parent.
    fn log_commit(
        artifact: &PolicyArtifact,
        parents: &[ObjectId],
        message: &str,
    ) -> (Vec<u8>, Vec<ByteBuf>) {
        let blob = ObjectId::of_blob(&fauna_cbor::encode_canonical(artifact).unwrap());
        let leaf = tree_content(&[
            ("100644", "feature-policy.cbor", blob),
            ("100644", "content-policy.cbor", ObjectId::of_blob(b"other")),
        ]);
        let region_dir = tree_content(&[
            ("40000", "DK", ObjectId::of_tree(b"")),
            ("40000", "NO", ObjectId::of_tree(&leaf)),
        ]);
        let root = tree_content(&[
            ("100644", "README.md", ObjectId::of_blob(b"# log")),
            ("40000", "regions", ObjectId::of_tree(&region_dir)),
        ]);
        let commit = commit_content(ObjectId::of_tree(&root), parents, message);
        (
            commit,
            vec![
                ByteBuf::from(root),
                ByteBuf::from(region_dir),
                ByteBuf::from(leaf),
            ],
        )
    }

    fn witness(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn roster(quorum: u32, seeds: &[u8]) -> WitnessRoster {
        WitnessRoster {
            version: 1,
            quorum,
            witnesses: seeds
                .iter()
                .map(|&seed| Witness {
                    key_id: format!("w{seed}"),
                    public_key: witness(seed).verifying_key().to_bytes().to_vec(),
                    enrolled_at: 1,
                    retired_at: None,
                })
                .collect(),
        }
    }

    fn checkpoint(head: ObjectId) -> Checkpoint {
        Checkpoint {
            log_id: REGION_LOG_ID.into(),
            head,
            extra: BTreeMap::new(),
        }
    }

    /// Fresh witnesses (no memory yet) cosign `head` over `chain`.
    fn cosign_fresh(head: ObjectId, chain: &[ByteBuf], seeds: &[u8]) -> Vec<Cosignature> {
        seeds
            .iter()
            .map(|&seed| {
                cosign_checkpoint(
                    &WitnessState::default(),
                    &checkpoint(head),
                    chain,
                    &format!("w{seed}"),
                    &witness(seed),
                )
                .unwrap()
                .0
            })
            .collect()
    }

    /// A two-commit log: genesis (holding sequence 1) then a child holding
    /// sequence 2. Returns (genesis id, child id, evidence for the child).
    fn two_commit_log() -> (ObjectId, ObjectId, InclusionEvidence) {
        let (genesis, _) = log_commit(&artifact(1), &[], "genesis");
        let genesis_id = ObjectId::of_commit(&genesis);
        let (child, trees) = log_commit(&artifact(2), &[genesis_id], "publish seq 2");
        let head = ObjectId::of_commit(&child);
        let chain = vec![ByteBuf::from(child), ByteBuf::from(genesis)];
        let cosignatures = cosign_fresh(head, &chain, &[1, 2]);
        (
            genesis_id,
            head,
            InclusionEvidence {
                head,
                chain,
                trees,
                checkpoint: checkpoint(head),
                cosignatures,
                extra: BTreeMap::new(),
            },
        )
    }

    fn anchored_at(head: ObjectId) -> AnchorState {
        AnchorState {
            compiled_in: None,
            last_accepted: Some(head),
        }
    }

    // ── the row's five failing-tests-first ──────────────────────────────────

    #[test]
    fn a_genuine_artifact_with_a_genuine_chain_accepts_and_advances_the_anchor() {
        let (genesis, head, evidence) = two_commit_log();
        let anchor = anchored_at(genesis);
        let next = check_inclusion(&artifact(2), &evidence, &anchor, &roster(2, &[1, 2])).unwrap();
        assert_eq!(next.last_accepted, Some(head));
        assert_eq!(next.compiled_in, None);
        // A re-fetch at the same head is a descendant of itself.
        let same = check_inclusion(&artifact(2), &evidence, &next, &roster(2, &[1, 2])).unwrap();
        assert_eq!(same, next);
    }

    /// RED-VERIFIED: with the `descends_from` call in `check_inclusion` removed,
    /// this test fails on the first assertion.
    #[test]
    fn a_forged_non_descendant_head_is_refused_and_the_anchor_is_unchanged() {
        let (_genesis, head, evidence) = two_commit_log();
        // The victim's persisted head is a commit the served chain never reaches.
        let (elsewhere, _) = log_commit(&artifact(1), &[], "the honest history");
        let trusted = ObjectId::of_commit(&elsewhere);
        let anchor = anchored_at(trusted);
        let outcome = check_inclusion(&artifact(2), &evidence, &anchor, &roster(2, &[1, 2]));
        assert_eq!(
            outcome,
            Err(InclusionRejection::NotDescendant { head, trusted })
        );
        assert_eq!(anchor, anchored_at(trusted), "a refusal moves nothing");
    }

    #[test]
    fn a_checkpoint_short_of_quorum_is_refused() {
        let (genesis, _head, mut evidence) = two_commit_log();
        evidence.cosignatures.truncate(1);
        let outcome = check_inclusion(
            &artifact(2),
            &evidence,
            &anchored_at(genesis),
            &roster(2, &[1, 2]),
        );
        assert_eq!(
            outcome,
            Err(InclusionRejection::NoWitnessQuorum { needed: 2, have: 1 })
        );
    }

    /// The consistency contract, witness side:
    /// a witness that remembers cosigning
    /// one history refuses a checkpoint that does not descend from it.
    #[test]
    fn a_witness_refuses_to_cosign_a_checkpoint_that_does_not_descend_from_its_last() {
        let (genesis, head, evidence) = two_commit_log();
        let (elsewhere, _) = log_commit(&artifact(9), &[], "a rewritten history");
        let remembered = WitnessState {
            last_cosigned: Some(ObjectId::of_commit(&elsewhere)),
        };
        let refused = cosign_checkpoint(
            &remembered,
            &checkpoint(head),
            &evidence.chain,
            "w1",
            &witness(1),
        );
        assert!(
            matches!(
                refused,
                Err(WitnessRefusal::NotDescendant(
                    InclusionRejection::NotDescendant { .. }
                ))
            ),
            "{refused:?}"
        );
        // The same witness, remembering the genesis instead, cosigns — and
        // its state advances to the head it just vouched for.
        let (_, state) = cosign_checkpoint(
            &WitnessState {
                last_cosigned: Some(genesis),
            },
            &checkpoint(head),
            &evidence.chain,
            "w1",
            &witness(1),
        )
        .unwrap();
        assert_eq!(state.last_cosigned, Some(head));
    }

    /// The contract, consumer side: a cosignature that is not a current roster
    /// member's does not count, and a checkpoint naming another log is refused.
    #[test]
    fn a_cosignature_outside_the_roster_does_not_count() {
        let (genesis, head, mut evidence) = two_commit_log();
        // Witness 2 is retired: its (genuine) cosignature no longer counts —
        // and the roster still has a reachable quorum (1 and 3), so this is a
        // short checkpoint, not an unusable roster.
        let mut retired = roster(2, &[1, 2, 3]);
        retired.witnesses[1].retired_at = Some(2);
        assert_eq!(
            check_inclusion(&artifact(2), &evidence, &anchored_at(genesis), &retired),
            Err(InclusionRejection::NoWitnessQuorum { needed: 2, have: 1 })
        );
        // A duplicate of witness 1's cosignature is one witness, not two.
        evidence.cosignatures[1] = evidence.cosignatures[0].clone();
        assert_eq!(
            check_inclusion(
                &artifact(2),
                &evidence,
                &anchored_at(genesis),
                &roster(2, &[1, 2])
            ),
            Err(InclusionRejection::NoWitnessQuorum { needed: 2, have: 1 })
        );
        // A checkpoint of some other log, however well witnessed.
        let foreign = Checkpoint {
            log_id: "log.example".into(),
            ..checkpoint(head)
        };
        evidence.cosignatures = [1u8, 2]
            .iter()
            .map(|&seed| {
                let input = checkpoint_signing_input(&foreign).unwrap();
                Cosignature {
                    witness_key_id: format!("w{seed}"),
                    sig: witness(seed).sign(&input).to_bytes().to_vec(),
                    extra: BTreeMap::new(),
                }
            })
            .collect();
        evidence.checkpoint = foreign;
        assert_eq!(
            check_inclusion(
                &artifact(2),
                &evidence,
                &anchored_at(genesis),
                &roster(2, &[1, 2])
            ),
            Err(InclusionRejection::WrongLog("log.example".into()))
        );
    }

    #[test]
    fn an_evidence_less_artifact_is_admitted_only_in_the_pre_log_era() {
        let pre_log = AnchorState::compiled_in();
        assert!(pre_log.is_pre_log(), "today's build ships no anchor");
        assert_eq!(
            admit_artifact(&artifact(1), None, &pre_log, &compiled_in_witness_roster()),
            Ok(pre_log.clone())
        );
        let (_genesis, head, _) = two_commit_log();
        let past = anchored_at(head);
        assert_eq!(
            admit_artifact(&artifact(1), None, &past, &compiled_in_witness_roster()),
            Err(InclusionRejection::EvidenceRequired)
        );
        let shipped_anchor = AnchorState {
            compiled_in: Some(head),
            last_accepted: None,
        };
        assert_eq!(
            admit_artifact(
                &artifact(1),
                None,
                &shipped_anchor,
                &compiled_in_witness_roster()
            ),
            Err(InclusionRejection::EvidenceRequired)
        );
    }

    // ── the remaining pieces ────────────────────────────────────────────────

    /// Evidence served under the compiled-in (empty) roster is refused even in
    /// the pre-log era: the quorum is the go-live bar, never waived.
    #[test]
    fn evidence_under_the_empty_roster_is_refused_rather_than_waved_through() {
        let (_genesis, _head, evidence) = two_commit_log();
        assert_eq!(
            admit_artifact(
                &artifact(2),
                Some(&evidence),
                &AnchorState::compiled_in(),
                &compiled_in_witness_roster()
            ),
            Err(InclusionRejection::NoWitnessRoster)
        );
    }

    #[test]
    fn a_fresh_consumer_with_no_anchor_accepts_a_witnessed_head() {
        let (_genesis, head, evidence) = two_commit_log();
        let next = check_inclusion(
            &artifact(2),
            &evidence,
            &AnchorState::compiled_in(),
            &roster(2, &[1, 2]),
        )
        .unwrap();
        assert_eq!(next.last_accepted, Some(head));
    }

    #[test]
    fn an_artifact_whose_bytes_are_not_at_its_path_is_refused() {
        let (genesis, head, evidence) = two_commit_log();
        // Same region, same kind, different bytes: a genuinely signed artifact
        // the log never committed.
        let never_logged = artifact(3);
        assert_eq!(
            check_inclusion(
                &never_logged,
                &evidence,
                &anchored_at(genesis),
                &roster(2, &[1, 2])
            ),
            Err(InclusionRejection::ArtifactNotAtPath {
                path: "regions/NO/feature-policy.cbor".into(),
                head,
            })
        );
        // Another region's artifact against this region's proof: the path
        // derives from the artifact, so the walk looks for `DK` and finds an
        // empty tree.
        let mut elsewhere = artifact(2);
        elsewhere.region = RegionCode::parse("DK").unwrap();
        assert!(matches!(
            check_inclusion(
                &elsewhere,
                &evidence,
                &anchored_at(genesis),
                &roster(2, &[1, 2])
            ),
            Err(InclusionRejection::ProofBroken { .. })
        ));
    }

    #[test]
    fn a_tampered_chain_is_refused() {
        let (genesis, head, evidence) = two_commit_log();
        let good = roster(2, &[1, 2]);
        // The head claimed does not match the commit served.
        let mut wrong_head = evidence.clone();
        wrong_head.head = genesis;
        assert!(matches!(
            check_inclusion(&artifact(2), &wrong_head, &anchored_at(genesis), &good),
            Err(InclusionRejection::HeadMismatch { .. })
        ));
        // The checkpoint vouches for a different head than the proof shows.
        let mut wrong_checkpoint = evidence.clone();
        wrong_checkpoint.checkpoint.head = genesis;
        assert_eq!(
            check_inclusion(
                &artifact(2),
                &wrong_checkpoint,
                &anchored_at(genesis),
                &good
            ),
            Err(InclusionRejection::CheckpointHeadMismatch {
                checkpoint: genesis,
                evidence: head,
            })
        );
        // A chain whose second commit is not a parent of the first.
        let (stranger, _) = log_commit(&artifact(1), &[], "unrelated");
        let mut broken = evidence.clone();
        broken.chain[1] = ByteBuf::from(stranger);
        assert_eq!(
            check_inclusion(&artifact(2), &broken, &anchored_at(genesis), &good),
            Err(InclusionRejection::ChainBroken { index: 1 })
        );
        // A chain that is only the head, for a consumer anchored earlier.
        let mut short = evidence.clone();
        short.chain.truncate(1);
        assert_eq!(
            check_inclusion(&artifact(2), &short, &anchored_at(genesis), &good),
            Err(InclusionRejection::NotDescendant {
                head,
                trusted: genesis
            })
        );
    }

    /// The log is linear (§ Repository layout and mirrors), and the checker
    /// holds it to that: a head whose *second* parent is the trusted head is
    /// refused outright, on the consumer side and the witness side alike.
    /// This pin asserted the opposite until 2026-09-11: a merge descending
    /// through either parent — under which an operator holding a split
    /// witness quorum could heal a victim-targeted fork by merging it back,
    /// and head monotonicity would reduce to "the operator named your head
    /// somewhere", which the operator can always arrange.
    #[test]
    fn a_merge_commit_is_refused_because_the_log_is_linear() {
        let (genesis, _) = log_commit(&artifact(1), &[], "genesis");
        let genesis_id = ObjectId::of_commit(&genesis);
        let (side, _) = log_commit(&artifact(1), &[], "a side branch");
        let side_id = ObjectId::of_commit(&side);
        let (merge, trees) = log_commit(&artifact(2), &[side_id, genesis_id], "merge");
        let head = ObjectId::of_commit(&merge);
        let chain = vec![ByteBuf::from(merge), ByteBuf::from(genesis)];
        let evidence = InclusionEvidence {
            head,
            cosignatures: cosign_fresh(head, &chain, &[1, 2]),
            chain: chain.clone(),
            trees,
            checkpoint: checkpoint(head),
            extra: BTreeMap::new(),
        };
        assert_eq!(
            check_inclusion(
                &artifact(2),
                &evidence,
                &anchored_at(genesis_id),
                &roster(2, &[1, 2]),
            ),
            Err(InclusionRejection::MergeCommit { index: 0 })
        );
        // The witness half of the contract refuses the same head for the
        // same reason: a merge is never a descent, whichever parent it names.
        let refused = cosign_checkpoint(
            &WitnessState {
                last_cosigned: Some(genesis_id),
            },
            &checkpoint(head),
            &chain,
            "w1",
            &witness(1),
        );
        assert!(
            matches!(
                refused,
                Err(WitnessRefusal::NotDescendant(
                    InclusionRejection::MergeCommit { index: 0 }
                ))
            ),
            "{refused:?}"
        );
    }

    #[test]
    fn bounds_are_checked_before_anything_is_hashed() {
        let (genesis, _head, evidence) = two_commit_log();
        let good = roster(2, &[1, 2]);
        let mut empty = evidence.clone();
        empty.chain.clear();
        assert_eq!(
            check_inclusion(&artifact(2), &empty, &anchored_at(genesis), &good),
            Err(InclusionRejection::EmptyChain)
        );
        let mut huge = evidence.clone();
        huge.chain[0] = ByteBuf::from(vec![b'x'; MAX_COMMIT_BYTES + 1]);
        assert_eq!(
            check_inclusion(&artifact(2), &huge, &anchored_at(genesis), &good),
            Err(InclusionRejection::ObjectTooLarge {
                kind: "commit",
                len: MAX_COMMIT_BYTES + 1
            })
        );
        let mut deep = evidence.clone();
        deep.trees.resize(MAX_PATH_DEPTH + 1, ByteBuf::new());
        assert_eq!(
            check_inclusion(&artifact(2), &deep, &anchored_at(genesis), &good),
            Err(InclusionRejection::PathTooDeep(MAX_PATH_DEPTH + 1))
        );
    }

    #[test]
    fn evidence_round_trips_through_dag_cbor_with_unknown_fields_kept() {
        let (_genesis, _head, mut evidence) = two_commit_log();
        evidence
            .extra
            .insert("window".into(), fauna_cbor::Value::Integer(7));
        let bytes = fauna_cbor::encode_canonical(&evidence).unwrap();
        let back: InclusionEvidence = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(back, evidence);
    }

    #[test]
    fn the_log_paths_agree_with_the_fetch_layout() {
        assert_eq!(
            log_path(&region(), PAYLOAD_KIND_FEATURE_POLICY),
            "regions/NO/feature-policy.cbor"
        );
        assert_eq!(
            log_evidence_path(&region(), PAYLOAD_KIND_FEATURE_POLICY),
            "regions/NO/feature-policy.evidence.cbor"
        );
    }
}
