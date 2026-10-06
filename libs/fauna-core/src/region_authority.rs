//! The region/authority plumbing — the curated registry, the signed artifact
//! envelope, and what verifying one actually checks.
//!
//! Owner doc: `docs/goal/behavior/region-blocking.md` § The region/authority
//! plumbing (resolved 2026-08-10, refutable at build time). It is designed
//! **once and serves two planes** — that doc's own content blocking, and
//! `docs/goal/architecture/dynamic-features.md` § The region tier, the
//! feature-gating tier 2 — so this module is shared Rust rather than nest code:
//! the two planes must not grow two envelopes, and apps read the same registry a
//! nest does (a store build's storefront-bound posture).
//!
//! **Network-free on purpose.** This module decodes and verifies; fetching from
//! the transparency log is the consumer's, which is what keeps these types
//! usable from the web SPA's WASM build. § The transparency log's acceptance
//! rule is not something a signature can carry: it is held at the **ingress**
//! (fetch only from the log) *and* by the section's ratified strengthening
//! (2026-08-11 — a checkable inclusion proof, head monotonicity, the
//! compiled-in anchor and witnessed checkpoints), whose checker is the
//! [`inclusion`] submodule — network-free, so every consumer — nest and app —
//! verifies inclusion itself. [`verify_artifact`] answers *who signed this*;
//! [`admit_artifact`] answers *was it published*, and the two are checked in
//! that order by every consumer.
//!
//! ## The registry ships empty, and that is its correct content
//!
//! Enrolling an authority is *"an administrative act by the Fauna organization,
//! done in the open"* (§ The region registry) and it has not happened: no
//! jurisdiction has published a Fauna policy artifact. So
//! [`compiled_in_registry`] is empty at version 0 — not a placeholder, the true
//! state — and it composes exactly right: every artifact fails
//! [`ArtifactRejection::UnknownRegion`], no region document is ever ingested,
//! and a deployment runs tiers 1/3/4/5, which is precisely the ratified *"a
//! deployment no region claims"* arm of dynamic-features.md § Fail posture.
//!
//! When the first authority is enrolled, the curation lands as an ordinary
//! revision of this file — its diff being the transparent artifact § The region
//! registry asks for. A YAML→generated-Rust pipeline on the provider-catalog
//! precedent (`i18n/providers.yaml`) is the shape to adopt once the table has
//! more than a handful of rows; it is deliberately not built for zero.

use std::collections::BTreeMap;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::feature_gate::FeaturePolicy;

pub mod inclusion;

pub use inclusion::{
    AnchorState, COMPILED_IN_LOG_ANCHOR, Checkpoint, Cosignature, InclusionEvidence,
    InclusionRejection, ObjectId, REGION_LOG_ID, Witness, WitnessRefusal, WitnessRoster,
    WitnessState, admit_artifact, check_inclusion, compiled_in_witness_roster, cosign_checkpoint,
    log_evidence_path, log_path,
};

/// The payload kind carrying this doc's sibling plane — a content-blocking
/// policy document (region-blocking.md § Publication and signing).
///
/// Decoded by [`VerifiedArtifact::content_policy`] into
/// [`crate::region_policy::ContentPolicyDocument`]; an artifact of this kind is
/// *recognised and skipped* by the feature plane's accessor rather than mistaken
/// for a feature policy, and vice versa.
pub const PAYLOAD_KIND_CONTENT_POLICY: &str = "content-policy";

/// The payload kind carrying a region's feature-policy document — the
/// quota-grammar map this plane consumes
/// (`dynamic-features.md` § The policy shape).
pub const PAYLOAD_KIND_FEATURE_POLICY: &str = "feature-policy";

/// The region refresh cadence, in seconds — how often a consumer re-asks for a
/// region's published artifacts (§ Publication and signing: *"a bounded cadence
/// sized as a tier-1-style constant"*). One constant for every consumer: the
/// nest's log refresh and an app's relay refresh run the same clock
/// (§ The content plane → *How an app obtains its region's policy*).
///
/// Six hours: a policy change is a legislative act with a lead time measured in
/// weeks, so the cadence is sized for *staleness detection* rather than
/// propagation speed — four attempts a day makes an unreachable channel visible
/// within a day while costing one small request per consumer.
pub const REFRESH_INTERVAL_SECS: u64 = 6 * 60 * 60;

/// How long a region channel may go unreached before its transparency surface
/// (the nest's admin surface, an app's settings surface) warns.
///
/// Deliberately several cadences, not one: a single missed attempt is a blip,
/// and a warning that fires on blips is one a reader learns to dismiss. Nothing
/// changes when it trips — the last-known-good document keeps binding — which is
/// § Fail posture's *"a warning, not an outage"*.
pub const STALE_AFTER_SECS: u64 = 3 * 24 * 60 * 60;

/// The envelope's size bound (§ Publication and signing: *"dag-cbor,
/// size-bounded"*) — a denial-of-service ceiling covering **every** payload
/// kind, generous because the content plane's payload may bundle a scorer
/// artifact.
///
/// It is deliberately not this plane's bound: a feature-policy document is three
/// small structs, so its consumer applies the much tighter
/// [`MAX_FEATURE_POLICY_PAYLOAD_BYTES`] on top. One ceiling per concern — the
/// envelope's stops a fetch from eating memory, the payload's stops a nonsense
/// document from being treated as a policy.
pub const MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// This plane's own bound on a decoded feature-policy payload. A document naming
/// every registry member with every dimension set is a few hundred bytes; 64 KiB
/// is room for two orders of magnitude of future growth and still refuses a
/// payload that is obviously not one.
pub const MAX_FEATURE_POLICY_PAYLOAD_BYTES: usize = 64 * 1024;

/// Longest accepted `key_id`, checked before the registry lookup so an
/// attacker-supplied identifier is bounded before it is used for anything.
pub const MAX_KEY_ID_BYTES: usize = 64;

/// How far into the future an `issued_at` may sit before the artifact is
/// refused.
///
/// Not load-bearing for ordering — `sequence` orders artifacts, not time — but
/// the transparency read *displays* this timestamp, and a nonsense one should
/// not reach a user's screen. A week is far past any plausible clock drift
/// between a nest and an authority, so an ordinary skew never trips it.
pub const MAX_ISSUED_AT_SKEW_SECS: u64 = 7 * 24 * 60 * 60;

const ED25519_PUBLIC_KEY_LEN: usize = 32;
const ED25519_SIGNATURE_LEN: usize = 64;

/// A region code — ISO 3166 basis, with supra-national authorities (the EU)
/// representable as their own rows (§ The region registry).
///
/// Validated on construction **and on decode**, so a hostile artifact cannot
/// carry a megabyte-long region code into a database key. Case is not
/// normalised — an artifact spelling its region in lower case is refused rather
/// than folded, because a normalising decoder makes two spellings of one region
/// both storable and the store's key ambiguous.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RegionCode(String);

/// Why a string is not a well-formed [`RegionCode`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "malformed region code: expected 2–8 uppercase ASCII alphanumerics, \
     or the ISO 3166-2 subdivision form `CC-SSS`, got {0:?}"
)]
pub struct MalformedRegionCode(String);

impl RegionCode {
    /// Shortest accepted code (ISO 3166-1 alpha-2, and the EU row).
    pub const MIN: usize = 2;
    /// Longest accepted code — headroom for an exceptional or supra-national
    /// row without leaving the key space bounded only by the wire.
    pub const MAX: usize = 8;

    /// The country part of an ISO 3166-2 subdivision code, in characters.
    const SUBDIVISION_COUNTRY_LEN: usize = 2;
    /// The widest subdivision part ISO 3166-2 assigns.
    const SUBDIVISION_MAX: usize = 3;

    /// Parse and validate.
    ///
    /// Two accepted spellings:
    ///
    /// - **2–8 uppercase ASCII alphanumerics** — ISO 3166-1 alpha-2 and the
    ///   supra-national rows (`EU`), unchanged since this type was built.
    /// - **The ISO 3166-2 subdivision form `CC-SSS`** — exactly one hyphen,
    ///   an alpha-2 country part before it and a 1–3 character subdivision part
    ///   after it (§ Regions compose along the registry's parent chain).
    ///
    /// Case is still not normalised in either spelling — a normalising decoder
    /// makes two spellings of one region both storable and the store's key
    /// ambiguous. An older consumer refuses the subdivision spelling as
    /// malformed and applies nothing for that row, which is exactly what it did
    /// before the spelling widened.
    ///
    /// # Errors
    ///
    /// [`MalformedRegionCode`] if the string matches neither spelling.
    pub fn parse(raw: impl Into<String>) -> Result<Self, MalformedRegionCode> {
        let raw = raw.into();
        if Self::is_well_formed(&raw) {
            Ok(Self(raw))
        } else {
            Err(MalformedRegionCode(raw))
        }
    }

    fn is_well_formed(raw: &str) -> bool {
        match raw.split_once('-') {
            Some((country, subdivision)) => {
                country.len() == Self::SUBDIVISION_COUNTRY_LEN
                    && (1..=Self::SUBDIVISION_MAX).contains(&subdivision.len())
                    && Self::is_plain(country)
                    && Self::is_plain(subdivision)
            }
            None => (Self::MIN..=Self::MAX).contains(&raw.len()) && Self::is_plain(raw),
        }
    }

    /// Uppercase ASCII alphanumerics only, and at least one character.
    ///
    /// `split_once` splits on the *first* hyphen, so a second hyphen lands in
    /// the subdivision part and is refused here — `CC-SSS` means one hyphen.
    fn is_plain(part: &str) -> bool {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RegionCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for RegionCode {
    type Error = MalformedRegionCode;
    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(raw)
    }
}

impl From<RegionCode> for String {
    fn from(code: RegionCode) -> Self {
        code.0
    }
}

/// One signing key an authority has been enrolled with.
///
/// **`retired_at` is what makes rotation a registry revision rather than a
/// deletion** (§ The region registry: *"an artifact stays attributable to the
/// key that was valid in its era"*). A retired key keeps verifying the artifacts
/// it issued while it was current and can issue no new ones — so history stays
/// checkable across a rotation, which is the property a permanent public log
/// needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityKey {
    /// Registry-assigned, stable for this key's lifetime.
    pub key_id: String,
    /// The Ed25519 public key.
    #[serde(with = "serde_bytes")]
    pub public_key: Vec<u8>,
    /// When the Fauna organization enrolled it, seconds since the Unix epoch.
    pub enrolled_at: u64,
    /// When it was rotated out; absent while current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<u64>,
}

impl AuthorityKey {
    /// Whether this key was the authority's current key at `at`.
    ///
    /// Half-open: `enrolled_at` counts, `retired_at` does not, so two keys
    /// whose eras abut cannot both be valid for one instant.
    pub fn valid_at(&self, at: u64) -> bool {
        at >= self.enrolled_at && self.retired_at.is_none_or(|retired| at < retired)
    }
}

/// One region's row in the curated catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionEntry {
    pub region: RegionCode,
    /// The administering authority, as the transparency surfaces name it.
    pub authority_name: String,
    /// The authority's official domain — the channel through which curation
    /// verified the key belongs to it.
    pub official_domain: String,
    /// The region this one sits inside, if any — a subdivision's country, a
    /// country's supra-national body (§ Regions compose along the registry's
    /// parent chain).
    ///
    /// A device is in several regions at once and each may administer its own
    /// policy, so the app applies **every** policy on the ancestor chain,
    /// strictest-wins in the same fold. `#[serde(default)]` keeps a snapshot
    /// published before this field existed decodable, and a row with no parent
    /// (the top of a chain, and every row today) simply carries `None`.
    #[serde(default)]
    pub parent: Option<RegionCode>,
    /// Every key ever enrolled for this region, current and retired.
    pub keys: Vec<AuthorityKey>,
}

impl RegionEntry {
    pub fn key(&self, key_id: &str) -> Option<&AuthorityKey> {
        self.keys.iter().find(|k| k.key_id == key_id)
    }
}

/// The Fauna-curated, signed, versioned catalog (§ The region registry).
///
/// **An owned, decodable value rather than a `&'static` table**, because § The
/// region registry has apps and nests ship a compiled-in *snapshot* and then
/// **refresh it over the same channel as policies** — a refreshable catalog
/// cannot be a compile-time-only shape.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RegionRegistry {
    /// Monotonic across curation revisions; `0` is the empty compiled-in
    /// snapshot that has never been refreshed.
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub regions: Vec<RegionEntry>,
}

/// Why a declared region has no resolvable ancestor chain.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegionChainError {
    /// A row's parent links back into the chain. A defect in a Fauna-curated,
    /// signed catalog — refused loudly rather than silently applying whichever
    /// prefix the walk happened to reach before it noticed.
    #[error("region registry has a parent cycle at {at}")]
    Cycle { at: RegionCode },
    /// The chain exceeds [`RegionRegistry::MAX_CHAIN_DEPTH`] without repeating a
    /// row — not a cycle, but not a plausible hierarchy of government either.
    #[error("region chain from {from} is deeper than {max} rows")]
    TooDeep { from: RegionCode, max: usize },
}

impl RegionRegistry {
    /// The deepest ancestor chain the catalog may express.
    ///
    /// Subdivision → country → supra-national is three; the headroom is for a
    /// hierarchy this design cannot foresee, and the bound exists so a
    /// malformed catalog cannot make a walk unbounded.
    pub const MAX_CHAIN_DEPTH: usize = 8;

    pub fn region(&self, region: &RegionCode) -> Option<&RegionEntry> {
        self.regions.iter().find(|r| &r.region == region)
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// The regions whose policies bind a device that declares `region`, most
    /// specific first (§ Regions compose along the registry's parent chain).
    ///
    /// The declared region always leads the chain, **including when the catalog
    /// does not carry it**: the declaration is a fact about the device, not a
    /// claim about the catalog, and asking for an unenrolled region's (absent)
    /// document is the fresh-subject arm of § Fail posture rather than an error.
    ///
    /// # Errors
    ///
    /// [`RegionChainError`] when the catalog's `parent` links form a cycle or
    /// run deeper than [`Self::MAX_CHAIN_DEPTH`].
    pub fn chain(&self, region: &RegionCode) -> Result<Vec<RegionCode>, RegionChainError> {
        let mut chain = vec![region.clone()];
        let mut cursor = region.clone();
        loop {
            let Some(parent) = self.region(&cursor).and_then(|e| e.parent.clone()) else {
                return Ok(chain);
            };
            if chain.contains(&parent) {
                return Err(RegionChainError::Cycle { at: parent });
            }
            if chain.len() >= Self::MAX_CHAIN_DEPTH {
                return Err(RegionChainError::TooDeep {
                    from: region.clone(),
                    max: Self::MAX_CHAIN_DEPTH,
                });
            }
            chain.push(parent.clone());
            cursor = parent;
        }
    }
}

/// The compiled-in registry snapshot.
///
/// **Empty at version 0 today** — see the module note on why that is the true
/// state rather than a placeholder, and what enrolling the first authority looks
/// like.
pub fn compiled_in_registry() -> RegionRegistry {
    RegionRegistry::default()
}

/// A region's feature-policy document: one authored [`FeaturePolicy`] per gated
/// feature, keyed by the feature's **stable key**
/// (`fauna_core::feature_gate::GatedFeature::as_str`).
///
/// Structurally identical to the guardian tier's sub-document
/// (`fauna_protocol::features::GuardianFeaturePolicies`) and for the same
/// reason: a document written by a *newer* publisher may name a feature this
/// build has never heard of, and keying by the stable string lets such an entry
/// round-trip untouched instead of failing the whole document.
pub type RegionFeaturePolicies = BTreeMap<String, FeaturePolicy>;

/// The signed envelope an authority publishes (§ Publication and signing).
///
/// One envelope, two payload kinds — one per plane. The kind is a **stable
/// string rather than an enum** on purpose: a newer authority publishing a third
/// payload kind must not make the envelope undecodable to an older reader. It
/// must decode, verify, and simply not be consumed by a plane that does not know
/// it — the same additive discipline the string-keyed feature documents use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyArtifact {
    /// The region this artifact binds.
    pub region: RegionCode,
    /// Which of the region's enrolled keys signed it.
    pub key_id: String,
    /// Monotonic per (region, payload kind). What makes last-known-good
    /// un-rollbackable: an older artifact is refused, never re-applied.
    pub sequence: u64,
    /// When the authority issued it, seconds since the Unix epoch.
    pub issued_at: u64,
    /// [`PAYLOAD_KIND_FEATURE_POLICY`], [`PAYLOAD_KIND_CONTENT_POLICY`], or a
    /// kind a newer publisher minted.
    pub payload_kind: String,
    /// The kind's own dag-cbor document.
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
    /// Ed25519 signature by the named key over this struct's canonical dag-cbor
    /// with `sig` zeroed — the placeholder pattern
    /// [`crate::grant_event::GrantEvent`] uses.
    #[serde(with = "serde_bytes")]
    pub sig: Vec<u8>,
}

/// Why an artifact was refused.
///
/// Every arm is a refusal to *apply* it. There is no arm that applies an
/// artifact with a warning: § Fail posture's direction is that a policy stays in
/// force until **replaced**, so a refused artifact leaves the last accepted one
/// exactly where it was.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactRejection {
    /// The payload is larger than [`MAX_PAYLOAD_BYTES`].
    #[error("payload is {0} bytes, over the {MAX_PAYLOAD_BYTES}-byte envelope bound")]
    PayloadTooLarge(usize),
    /// `key_id` is longer than [`MAX_KEY_ID_BYTES`].
    #[error("key id is {0} bytes, over the {MAX_KEY_ID_BYTES}-byte bound")]
    KeyIdTooLong(usize),
    /// No row in the registry claims this region — the state an **empty**
    /// registry puts every artifact in.
    #[error("no enrolled authority for region {0}")]
    UnknownRegion(RegionCode),
    /// The region is enrolled, but not with this key.
    #[error("region {region} has no enrolled key {key_id:?}")]
    UnknownKey { region: RegionCode, key_id: String },
    /// The key exists but was not the authority's current key when the artifact
    /// was issued — enrolled later, or already rotated out.
    #[error("key {key_id:?} was not valid for region {region} at {issued_at}")]
    KeyOutsideEra {
        region: RegionCode,
        key_id: String,
        issued_at: u64,
    },
    /// `issued_at` is implausibly far ahead of the verifier's clock.
    #[error("issued_at {issued_at} is more than {MAX_ISSUED_AT_SKEW_SECS}s ahead of now ({now})")]
    IssuedInFuture { issued_at: u64, now: u64 },
    /// `sequence` does not advance past the last artifact accepted for this
    /// (region, payload kind) — a replay, or a rollback attempt.
    #[error("sequence {sequence} does not advance past the accepted {accepted}")]
    ReplayedSequence { sequence: u64, accepted: u64 },
    /// The public key or signature had the wrong length, or the signature did
    /// not verify against the canonical bytes.
    #[error("signature verification failed")]
    SignatureFailed,
    /// The canonical re-encode of the signing input failed (unreachable for this
    /// float-free shape).
    #[error("canonical encode: {0}")]
    Encode(String),
}

/// Why a verified artifact's payload could not be read as this plane's document.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PayloadError {
    /// The artifact carries a different plane's document, or one this build does
    /// not know. Not a fault: it is the additive case the string kind exists for.
    #[error("artifact carries payload kind {0:?}, which is not the one asked for")]
    WrongKind(String),
    /// Over the asked-for plane's payload bound
    /// ([`MAX_FEATURE_POLICY_PAYLOAD_BYTES`] or
    /// [`crate::region_policy::MAX_CONTENT_POLICY_PAYLOAD_BYTES`]).
    #[error("payload is {0} bytes, over the plane's bound")]
    TooLarge(usize),
    /// The payload is not a decodable document of the asked-for plane.
    #[error("decode payload: {0}")]
    Decode(String),
}

/// A [`PolicyArtifact`] that has passed [`verify_artifact`].
///
/// **The payload is reachable only through this type**, and this type is
/// constructible only by verification. That is deliberate rather than tidy: the
/// sibling plane's own build lesson is that *a pin against a caller obligation
/// cannot assemble the call* — an API where `decode` and `verify` are two
/// independent calls has a silently-permissive shape one forgotten line away.
/// Here forgetting is not representable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArtifact {
    artifact: PolicyArtifact,
    authority_name: String,
}

impl VerifiedArtifact {
    pub fn artifact(&self) -> &PolicyArtifact {
        &self.artifact
    }

    /// The administering authority's name, as the transparency surfaces show it
    /// ("limited by …"). Read from the registry at verification time, never from
    /// the artifact — an authority does not get to name itself.
    pub fn authority_name(&self) -> &str {
        &self.authority_name
    }

    pub fn region(&self) -> &RegionCode {
        &self.artifact.region
    }

    pub fn sequence(&self) -> u64 {
        self.artifact.sequence
    }

    pub fn issued_at(&self) -> u64 {
        self.artifact.issued_at
    }

    pub fn payload_kind(&self) -> &str {
        &self.artifact.payload_kind
    }

    /// Decode the payload as this plane's feature-policy document.
    ///
    /// # Errors
    ///
    /// [`PayloadError::WrongKind`] when the artifact carries the content plane's
    /// document (or a kind a newer publisher minted) — which a caller treats as
    /// "not mine", never as a fault.
    pub fn feature_policies(&self) -> Result<RegionFeaturePolicies, PayloadError> {
        if self.artifact.payload_kind != PAYLOAD_KIND_FEATURE_POLICY {
            return Err(PayloadError::WrongKind(self.artifact.payload_kind.clone()));
        }
        if self.artifact.payload.len() > MAX_FEATURE_POLICY_PAYLOAD_BYTES {
            return Err(PayloadError::TooLarge(self.artifact.payload.len()));
        }
        fauna_cbor::decode_strict(&self.artifact.payload)
            .map_err(|e| PayloadError::Decode(format!("{e:?}")))
    }

    /// Decode the payload as the **content plane's** policy document
    /// (`region-blocking.md` § The content plane → *The policy document*).
    ///
    /// The twin of [`Self::feature_policies`], and — like it — the only door to
    /// its plane's payload: [`crate::region_policy`] deliberately exposes no
    /// `decode`, so a document that was never verified is unrepresentable
    /// rather than merely discouraged.
    ///
    /// A decoded document is not yet an *applicable* one — ask
    /// [`crate::region_policy::ContentPolicyDocument::status`], which is where
    /// the inert-version and malformed-structure rules live.
    ///
    /// # Errors
    ///
    /// [`PayloadError::WrongKind`] when the artifact carries the feature plane's
    /// document (or a kind a newer publisher minted) — which a caller treats as
    /// "not mine", never as a fault.
    pub fn content_policy(
        &self,
    ) -> Result<crate::region_policy::ContentPolicyDocument, PayloadError> {
        if self.artifact.payload_kind != PAYLOAD_KIND_CONTENT_POLICY {
            return Err(PayloadError::WrongKind(self.artifact.payload_kind.clone()));
        }
        if self.artifact.payload.len() > crate::region_policy::MAX_CONTENT_POLICY_PAYLOAD_BYTES {
            return Err(PayloadError::TooLarge(self.artifact.payload.len()));
        }
        fauna_cbor::decode_strict(&self.artifact.payload)
            .map_err(|e| PayloadError::Decode(format!("{e:?}")))
    }
}

/// The bytes an artifact's signature covers: its canonical dag-cbor with `sig`
/// zeroed.
///
/// One function, used by both [`sign_artifact`] and [`verify_artifact`], so the
/// two can never drift about what was signed.
fn signing_input(artifact: &PolicyArtifact) -> Result<Vec<u8>, ArtifactRejection> {
    let mut placeholder = artifact.clone();
    placeholder.sig = vec![0u8; ED25519_SIGNATURE_LEN];
    fauna_cbor::encode_canonical(&placeholder).map_err(|e| ArtifactRejection::Encode(e.to_string()))
}

/// Sign an artifact with an authority's key.
///
/// **Fauna never calls this in production** — Fauna does not administer a region
/// and holds no authority key. It exists so that the signing input is defined in
/// exactly one place (see [`signing_input`]), which is what an authority's own
/// tooling, a fixture generator, and every test need in order to agree with
/// [`verify_artifact`] rather than approximate it.
///
/// # Errors
///
/// [`ArtifactRejection::Encode`] if the canonical encode fails (unreachable for
/// this float-free shape).
pub fn sign_artifact(
    mut artifact: PolicyArtifact,
    signing_key: &SigningKey,
) -> Result<PolicyArtifact, ArtifactRejection> {
    let bytes = signing_input(&artifact)?;
    artifact.sig = signing_key.sign(&bytes).to_bytes().to_vec();
    Ok(artifact)
}

/// Verify an artifact against the curated registry.
///
/// `now` is the verifier's clock in epoch seconds; `last_accepted_sequence` is
/// the sequence of the last artifact this verifier accepted for the same
/// (region, payload kind), or `None` if it has never accepted one.
///
/// ⚠ **Verification does not — and cannot — establish log inclusion.** § The
/// transparency log makes publication the thing that gives a policy effect, and
/// a signature binds an artifact to an authority while saying nothing about
/// whether it was ever published. Callers restrict their ingress to the log
/// **and** then call [`admit_artifact`] with the evidence the log served
/// beside the envelope (a fetch-layer companion, never an envelope field): that
/// is the check that makes inclusion *checkable* rather than assumed, and the
/// one that states the pre-log era's rule in code.
///
/// # Errors
///
/// One [`ArtifactRejection`] per failed check, in the order documented on the
/// module's design note: bounds, then registry lookup, then key era, then clock,
/// then sequence, then the signature. Cheap checks first, and the signature last
/// because it is the only expensive one.
pub fn verify_artifact(
    artifact: PolicyArtifact,
    registry: &RegionRegistry,
    now: u64,
    last_accepted_sequence: Option<u64>,
) -> Result<VerifiedArtifact, ArtifactRejection> {
    if artifact.payload.len() > MAX_PAYLOAD_BYTES {
        return Err(ArtifactRejection::PayloadTooLarge(artifact.payload.len()));
    }
    if artifact.key_id.len() > MAX_KEY_ID_BYTES {
        return Err(ArtifactRejection::KeyIdTooLong(artifact.key_id.len()));
    }

    let entry = registry
        .region(&artifact.region)
        .ok_or_else(|| ArtifactRejection::UnknownRegion(artifact.region.clone()))?;
    let key = entry
        .key(&artifact.key_id)
        .ok_or_else(|| ArtifactRejection::UnknownKey {
            region: artifact.region.clone(),
            key_id: artifact.key_id.clone(),
        })?;
    if !key.valid_at(artifact.issued_at) {
        return Err(ArtifactRejection::KeyOutsideEra {
            region: artifact.region.clone(),
            key_id: artifact.key_id.clone(),
            issued_at: artifact.issued_at,
        });
    }

    if artifact.issued_at > now.saturating_add(MAX_ISSUED_AT_SKEW_SECS) {
        return Err(ArtifactRejection::IssuedInFuture {
            issued_at: artifact.issued_at,
            now,
        });
    }

    if let Some(accepted) = last_accepted_sequence
        && artifact.sequence <= accepted
    {
        return Err(ArtifactRejection::ReplayedSequence {
            sequence: artifact.sequence,
            accepted,
        });
    }

    if key.public_key.len() != ED25519_PUBLIC_KEY_LEN || artifact.sig.len() != ED25519_SIGNATURE_LEN
    {
        return Err(ArtifactRejection::SignatureFailed);
    }
    let mut pk = [0u8; ED25519_PUBLIC_KEY_LEN];
    pk.copy_from_slice(&key.public_key);
    let mut sig = [0u8; ED25519_SIGNATURE_LEN];
    sig.copy_from_slice(&artifact.sig);
    let verifying_key =
        VerifyingKey::from_bytes(&pk).map_err(|_| ArtifactRejection::SignatureFailed)?;
    let bytes = signing_input(&artifact)?;
    verifying_key
        .verify_strict(&bytes, &Signature::from_bytes(&sig))
        .map_err(|_| ArtifactRejection::SignatureFailed)?;

    let authority_name = entry.authority_name.clone();
    Ok(VerifiedArtifact {
        artifact,
        authority_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature_gate::{Availability, GatedFeature, Window, WindowedBounds};

    const NOW: u64 = 1_800_000_000;
    const ENROLLED: u64 = 1_700_000_000;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn region() -> RegionCode {
        RegionCode::parse("NO").unwrap()
    }

    /// A registry enrolling one region with one current key.
    fn registry_with(signing: &SigningKey, retired_at: Option<u64>) -> RegionRegistry {
        RegionRegistry {
            version: 1,
            regions: vec![RegionEntry {
                region: region(),
                authority_name: "Test Authority".into(),
                official_domain: "authority.example".into(),
                parent: None,
                keys: vec![AuthorityKey {
                    key_id: "k1".into(),
                    public_key: signing.verifying_key().to_bytes().to_vec(),
                    enrolled_at: ENROLLED,
                    retired_at,
                }],
            }],
        }
    }

    fn feature_payload() -> Vec<u8> {
        let mut doc = RegionFeaturePolicies::new();
        doc.insert(
            GatedFeature::P2pShare.as_str().to_string(),
            FeaturePolicy {
                availability: Availability::Limit,
                counterparties: WindowedBounds::at(Window::Month, 3),
                ..Default::default()
            },
        );
        fauna_cbor::encode_canonical(&doc).unwrap()
    }

    fn artifact(signing: &SigningKey) -> PolicyArtifact {
        sign_artifact(
            PolicyArtifact {
                region: region(),
                key_id: "k1".into(),
                sequence: 7,
                issued_at: NOW - 60,
                payload_kind: PAYLOAD_KIND_FEATURE_POLICY.into(),
                payload: feature_payload(),
                sig: Vec::new(),
            },
            signing,
        )
        .unwrap()
    }

    /// A registry with a subdivision → country → supra-national chain, plus one
    /// unrelated row, so a chain walk cannot pass by picking the only entry.
    fn chained_registry() -> RegionRegistry {
        let entry = |code: &str, parent: Option<&str>| RegionEntry {
            region: RegionCode::parse(code).unwrap(),
            authority_name: format!("{code} authority"),
            official_domain: "authority.example".into(),
            parent: parent.map(|p| RegionCode::parse(p).unwrap()),
            keys: Vec::new(),
        };
        RegionRegistry {
            version: 1,
            regions: vec![
                entry("NO-03", Some("NO")),
                entry("NO", Some("EU")),
                entry("EU", None),
                entry("JP", None),
            ],
        }
    }

    #[test]
    fn a_subdivision_code_round_trips_and_an_alpha_2_still_does() {
        // § Regions compose along the registry's parent chain: `RegionCode`
        // admits the ISO 3166-2 `CC-SSS` form beside the alpha-2 and
        // supra-national rows it accepts today.
        for good in ["NO", "EU", "NO-03", "US-CA", "GB-ENG", "JP-13"] {
            let code = RegionCode::parse(good).unwrap_or_else(|e| panic!("{good}: {e}"));
            assert_eq!(code.as_str(), good);
            // Round-trips through the wire representation unchanged.
            let encoded = fauna_cbor::encode_canonical(&code).unwrap();
            let decoded: RegionCode = fauna_cbor::decode_strict(&encoded).unwrap();
            assert_eq!(decoded, code);
        }
    }

    #[test]
    fn a_malformed_subdivision_spelling_is_still_refused() {
        // The hyphen is admitted in exactly one shape. Everything else stays as
        // refused as it was before the spelling widened — an older consumer
        // refuses the new spelling and applies nothing for that row, and this
        // build must not become sloppier than that in exchange.
        for bad in [
            "no-03",   // lower case is never folded, only refused
            "NO-",     // no subdivision part
            "-03",     // no country part
            "N-03",    // country part is alpha-2, always
            "NOR-03",  // ditto
            "NO-0345", // subdivision part is 1–3 characters
            "NO-03-1", // one hyphen, not two
            "NO_03",   // the separator is the ISO one
        ] {
            assert!(
                RegionCode::parse(bad).is_err(),
                "{bad:?} must not parse as a region code"
            );
        }
    }

    #[test]
    fn a_chain_resolves_subdivision_then_country_then_supra_national() {
        let registry = chained_registry();
        let chain = registry
            .chain(&RegionCode::parse("NO-03").unwrap())
            .expect("a well-formed catalog has no cycle");
        assert_eq!(
            chain,
            vec![
                RegionCode::parse("NO-03").unwrap(),
                RegionCode::parse("NO").unwrap(),
                RegionCode::parse("EU").unwrap(),
            ],
            "most specific first — the app applies every policy on the chain"
        );

        // A row with no parent is its own whole chain.
        assert_eq!(
            registry.chain(&RegionCode::parse("JP").unwrap()).unwrap(),
            vec![RegionCode::parse("JP").unwrap()]
        );
    }

    #[test]
    fn a_region_the_registry_does_not_carry_is_its_own_whole_chain() {
        // The declared region is a fact about the device, not a claim about the
        // catalog. An unenrolled region simply has no ancestors we know of, and
        // asking for its (absent) document is the fresh-subject arm of § Fail
        // posture rather than an error.
        let registry = chained_registry();
        let declared = RegionCode::parse("ZZ").unwrap();
        assert_eq!(registry.chain(&declared).unwrap(), vec![declared]);
    }

    #[test]
    fn a_registry_cycle_is_refused() {
        // A cycle is a defect in a Fauna-curated, signed catalog. Refusing
        // loudly beats silently applying whichever prefix the walk happened to
        // reach before it noticed.
        let entry = |code: &str, parent: &str| RegionEntry {
            region: RegionCode::parse(code).unwrap(),
            authority_name: "a".into(),
            official_domain: "authority.example".into(),
            parent: Some(RegionCode::parse(parent).unwrap()),
            keys: Vec::new(),
        };
        let registry = RegionRegistry {
            version: 1,
            regions: vec![entry("AA", "BB"), entry("BB", "CC"), entry("CC", "AA")],
        };
        assert_eq!(
            registry.chain(&RegionCode::parse("AA").unwrap()),
            Err(RegionChainError::Cycle {
                at: RegionCode::parse("AA").unwrap()
            })
        );

        // A row that parents itself is the degenerate case of the same defect.
        let registry = RegionRegistry {
            version: 1,
            regions: vec![entry("AA", "AA")],
        };
        assert!(registry.chain(&RegionCode::parse("AA").unwrap()).is_err());
    }

    #[test]
    fn a_signed_artifact_from_an_enrolled_key_verifies_and_carries_its_document() {
        let signing = key(1);
        let verified = verify_artifact(
            artifact(&signing),
            &registry_with(&signing, None),
            NOW,
            None,
        )
        .unwrap();

        // The authority's name comes from the registry, not from the artifact.
        assert_eq!(verified.authority_name(), "Test Authority");
        assert_eq!(verified.sequence(), 7);

        let doc = verified.feature_policies().unwrap();
        let policy = doc.get(GatedFeature::P2pShare.as_str()).unwrap();
        assert_eq!(policy.availability, Availability::Limit);
        assert_eq!(policy.counterparties.get(Window::Month), Some(3));
    }

    /// The state every artifact is in today, and the reason the empty snapshot
    /// is the *correct* content: nothing is ingested, so a deployment runs
    /// tiers 1/3/4/5 (§ Fail posture, "a deployment no region claims").
    #[test]
    fn the_compiled_in_registry_is_empty_and_refuses_every_artifact() {
        let snapshot = compiled_in_registry();
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.version, 0);

        let signing = key(1);
        assert_eq!(
            verify_artifact(artifact(&signing), &snapshot, NOW, None),
            Err(ArtifactRejection::UnknownRegion(region()))
        );
    }

    #[test]
    fn an_artifact_for_an_unenrolled_region_is_refused() {
        let signing = key(1);
        let mut a = artifact(&signing);
        a.region = RegionCode::parse("SE").unwrap();
        let a = sign_artifact(a, &signing).unwrap();
        assert!(matches!(
            verify_artifact(a, &registry_with(&signing, None), NOW, None),
            Err(ArtifactRejection::UnknownRegion(_))
        ));
    }

    #[test]
    fn an_artifact_naming_an_unenrolled_key_is_refused() {
        let signing = key(1);
        let mut a = artifact(&signing);
        a.key_id = "k2".into();
        let a = sign_artifact(a, &signing).unwrap();
        assert!(matches!(
            verify_artifact(a, &registry_with(&signing, None), NOW, None),
            Err(ArtifactRejection::UnknownKey { .. })
        ));
    }

    /// A different key signing under an enrolled key's id: the id resolves, the
    /// signature does not.
    #[test]
    fn an_artifact_signed_by_an_impostor_is_refused() {
        let enrolled = key(1);
        let impostor = key(9);
        let a = artifact(&impostor);
        assert_eq!(
            verify_artifact(a, &registry_with(&enrolled, None), NOW, None),
            Err(ArtifactRejection::SignatureFailed)
        );
    }

    #[test]
    fn a_tampered_payload_is_refused() {
        let signing = key(1);
        let mut a = artifact(&signing);
        a.payload.push(0);
        assert_eq!(
            verify_artifact(a, &registry_with(&signing, None), NOW, None),
            Err(ArtifactRejection::SignatureFailed)
        );
    }

    /// The era check, in the direction that matters most: a key rotated out
    /// cannot issue anything new.
    #[test]
    fn a_retired_key_cannot_issue_after_its_era() {
        let signing = key(1);
        let retired_at = NOW - 3600;
        assert!(matches!(
            verify_artifact(
                artifact(&signing),
                &registry_with(&signing, Some(retired_at)),
                NOW,
                None
            ),
            Err(ArtifactRejection::KeyOutsideEra { .. })
        ));
    }

    /// …and the direction that makes rotation a *revision* rather than a
    /// deletion: the same retired key still verifies the artifacts it issued
    /// while it was current, so the public history stays checkable.
    #[test]
    fn a_retired_key_still_verifies_its_own_era() {
        let signing = key(1);
        let issued_at = ENROLLED + 100;
        let mut a = artifact(&signing);
        a.issued_at = issued_at;
        let a = sign_artifact(a, &signing).unwrap();
        let verified =
            verify_artifact(a, &registry_with(&signing, Some(issued_at + 1)), NOW, None).unwrap();
        assert_eq!(verified.issued_at(), issued_at);
    }

    #[test]
    fn a_key_cannot_issue_before_it_was_enrolled() {
        let signing = key(1);
        let mut a = artifact(&signing);
        a.issued_at = ENROLLED - 1;
        let a = sign_artifact(a, &signing).unwrap();
        assert!(matches!(
            verify_artifact(a, &registry_with(&signing, None), NOW, None),
            Err(ArtifactRejection::KeyOutsideEra { .. })
        ));
    }

    #[test]
    fn an_implausible_future_issue_time_is_refused() {
        let signing = key(1);
        let mut a = artifact(&signing);
        a.issued_at = NOW + MAX_ISSUED_AT_SKEW_SECS + 1;
        let a = sign_artifact(a, &signing).unwrap();
        assert!(matches!(
            verify_artifact(a, &registry_with(&signing, None), NOW, None),
            Err(ArtifactRejection::IssuedInFuture { .. })
        ));
    }

    /// Ordinary clock drift between a nest and an authority must not brick the
    /// tier — the skew allowance is what keeps the check from being an outage.
    #[test]
    fn a_small_clock_skew_is_tolerated() {
        let signing = key(1);
        let mut a = artifact(&signing);
        a.issued_at = NOW + 600;
        let a = sign_artifact(a, &signing).unwrap();
        assert!(verify_artifact(a, &registry_with(&signing, None), NOW, None).is_ok());
    }

    /// Last-known-good is only un-rollbackable if a replay is refused: an
    /// attacker replaying yesterday's looser policy must not relax a live bound.
    #[test]
    fn a_replayed_or_equal_sequence_is_refused() {
        let signing = key(1);
        let registry = registry_with(&signing, None);
        assert!(matches!(
            verify_artifact(artifact(&signing), &registry, NOW, Some(7)),
            Err(ArtifactRejection::ReplayedSequence {
                sequence: 7,
                accepted: 7
            })
        ));
        assert!(matches!(
            verify_artifact(artifact(&signing), &registry, NOW, Some(8)),
            Err(ArtifactRejection::ReplayedSequence { .. })
        ));
        assert!(verify_artifact(artifact(&signing), &registry, NOW, Some(6)).is_ok());
    }

    #[test]
    fn an_oversized_payload_is_refused_before_any_lookup() {
        let signing = key(1);
        let mut a = artifact(&signing);
        a.payload = vec![0u8; MAX_PAYLOAD_BYTES + 1];
        // Deliberately *not* re-signed: the bound must be refused on its own,
        // before the expensive check and before the registry is consulted.
        assert!(matches!(
            verify_artifact(a, &RegionRegistry::default(), NOW, None),
            Err(ArtifactRejection::PayloadTooLarge(_))
        ));
    }

    #[test]
    fn an_overlong_key_id_is_refused() {
        let signing = key(1);
        let mut a = artifact(&signing);
        a.key_id = "k".repeat(MAX_KEY_ID_BYTES + 1);
        assert!(matches!(
            verify_artifact(a, &registry_with(&signing, None), NOW, None),
            Err(ArtifactRejection::KeyIdTooLong(_))
        ));
    }

    /// The additive case the string payload kind exists for: an artifact of the
    /// *other* plane verifies fine and is simply not this plane's document.
    #[test]
    fn a_content_policy_artifact_verifies_but_is_not_a_feature_policy() {
        let signing = key(1);
        let mut a = artifact(&signing);
        a.payload_kind = PAYLOAD_KIND_CONTENT_POLICY.into();
        let a = sign_artifact(a, &signing).unwrap();
        let verified = verify_artifact(a, &registry_with(&signing, None), NOW, None).unwrap();
        assert!(matches!(
            verified.feature_policies(),
            Err(PayloadError::WrongKind(_))
        ));
    }

    /// The content plane's own door, and the reason it is a door: the document
    /// is reachable **only** through a verified artifact, so decoding an
    /// unverified one is unrepresentable rather than merely discouraged.
    #[test]
    fn a_content_policy_document_is_reachable_only_through_a_verified_artifact() {
        use crate::region_policy::{ContentPolicyDocument, GRAMMAR_VERSION, PolicyStatus};

        let signing = key(1);
        let document = ContentPolicyDocument {
            version: GRAMMAR_VERSION,
            rules: Vec::new(),
            scorers: Vec::new(),
            extra: Default::default(),
        };
        let mut a = artifact(&signing);
        a.payload_kind = PAYLOAD_KIND_CONTENT_POLICY.into();
        a.payload = fauna_cbor::encode_canonical(&document).unwrap();
        let a = sign_artifact(a, &signing).unwrap();

        let verified = verify_artifact(a, &registry_with(&signing, None), NOW, None).unwrap();
        let decoded = verified.content_policy().expect("this plane's document");
        assert_eq!(decoded, document);
        assert_eq!(decoded.status(&region()), PolicyStatus::Applied);

        // …and the sibling plane's accessor still says "not mine", never faults.
        assert!(matches!(
            verified.feature_policies(),
            Err(PayloadError::WrongKind(_))
        ));
    }

    /// The mirror of `a_content_policy_artifact_verifies_but_is_not_a_feature_policy`.
    #[test]
    fn a_feature_policy_artifact_verifies_but_is_not_a_content_policy() {
        let signing = key(1);
        let verified = verify_artifact(
            artifact(&signing),
            &registry_with(&signing, None),
            NOW,
            None,
        )
        .unwrap();
        assert!(matches!(
            verified.content_policy(),
            Err(PayloadError::WrongKind(_))
        ));
    }

    /// The same, for a kind minted after this build shipped — the reason the
    /// kind is a string and not an enum.
    #[test]
    fn an_unknown_future_payload_kind_still_decodes_and_verifies() {
        let signing = key(1);
        let mut a = artifact(&signing);
        a.payload_kind = "some-third-plane".into();
        let a = sign_artifact(a, &signing).unwrap();
        let bytes = fauna_cbor::encode_canonical(&a).unwrap();
        let back: PolicyArtifact = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(back, a);
        assert!(verify_artifact(back, &registry_with(&signing, None), NOW, None).is_ok());
    }

    #[test]
    fn the_envelope_round_trips_through_dag_cbor() {
        let signing = key(1);
        let a = artifact(&signing);
        let bytes = fauna_cbor::encode_canonical(&a).unwrap();
        let back: PolicyArtifact = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(back, a);
        assert!(verify_artifact(back, &registry_with(&signing, None), NOW, None).is_ok());
    }

    #[test]
    fn region_codes_are_validated_and_never_case_folded() {
        assert!(RegionCode::parse("NO").is_ok());
        assert!(RegionCode::parse("EU").is_ok());
        assert!(RegionCode::parse("no").is_err());
        assert!(RegionCode::parse("N").is_err());
        assert!(RegionCode::parse("N-O").is_err());
        assert!(RegionCode::parse("A".repeat(RegionCode::MAX + 1)).is_err());
    }

    /// The validation must hold on the *wire*, not only at `parse` — otherwise a
    /// hostile artifact carries an unbounded string into a database key.
    #[test]
    fn a_malformed_region_code_is_refused_at_decode() {
        let good = fauna_cbor::encode_canonical(&region()).unwrap();
        assert!(fauna_cbor::decode_strict::<RegionCode>(&good).is_ok());

        let hostile = fauna_cbor::encode_canonical(&"x".repeat(4096)).unwrap();
        assert!(fauna_cbor::decode_strict::<RegionCode>(&hostile).is_err());
    }
}
