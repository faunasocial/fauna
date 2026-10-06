//! Wire format primitives for wrapped blobs: error types, AAD binding,
//! version constants. Concrete blob structs are added in subsequent
//! tasks; this module holds shared infrastructure.

use crate::wrapped_blob::kdf::{ARGON2_VERSION_13, Argon2idParams, HkdfSha256Params, KdfParams};
use fauna_cbor::Value as CborValue;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use thiserror::Error;

/// Current blob format version. Bumped only for incompatible AAD or
/// AEAD changes; algorithm-agility within a version is handled by the
/// per-blob `kdf` / `hpke` descriptors.
pub const BLOB_FORMAT_VERSION: u8 = 1;

/// Errors raised when sealing a blob.
#[derive(Debug, Error)]
pub enum WrapError {
    #[error("KDF derivation failed: {0}")]
    KdfFailed(String),
    #[error("AEAD encryption failed: {0}")]
    AeadFailed(String),
    #[error("HPKE seal failed: {0}")]
    HpkeFailed(String),
    #[error("CBOR encode failed: {0}")]
    CborEncode(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

/// Errors raised when unwrapping a blob. AEAD failure is the
/// authentication signal at the bridge — distinguish it from format
/// errors so callers can map AEAD-fail to "BAD" responses.
#[derive(Debug, Error)]
pub enum UnwrapError {
    /// AEAD decrypt-and-verify failed. At a bridge: this is "wrong
    /// credential" / "blob tampered" / "AAD mismatch". Treated as the
    /// authentication-failure signal.
    #[error("AEAD verify failed")]
    AeadFailed,
    /// HPKE open failed: wrong recipient secret or tampered ciphertext.
    #[error("HPKE open failed")]
    HpkeFailed,
    /// Format-level error: bad CBOR, unknown kind, missing field, wrong
    /// field length.
    #[error("invalid blob format: {0}")]
    InvalidFormat(String),
    /// The blob carries a format version this build does not speak — a
    /// later build's blob, not a corrupt one. Read from the stamp BEFORE
    /// the strict decode ([`check_stamp_before_decode`]), so a newer body
    /// shape never reads as corruption.
    #[error("unsupported blob format version: {0}")]
    UnsupportedVersion(u8),
    /// KDF derivation failed (parameter out of range, etc.).
    #[error("KDF derivation failed: {0}")]
    KdfFailed(String),
    /// Submission-token signature verify failed.
    #[error("submission-token signature verify failed")]
    SignatureFailed,
    /// An R14 (account-data-plane.md § The ratified decisions) generation wrap opened, but the recovered key's commitment does
    /// not match the mint it claims to belong to — wrap substitution, detected
    /// at the unwrapping device (`owner-key-material.md` § The schedule build
    /// design → *Key↔id binding*). Distinct from [`Self::HpkeFailed`] on
    /// purpose: HPKE failure is a wrong key/tamper/binding mismatch, this is a
    /// *well-formed* wrap carrying the wrong generation's key.
    #[error("unwrapped generation key does not match its mint's commitment")]
    CommitmentMismatch,
    /// An ATProto identity blob opened, but the keys inside are not the ones the
    /// identity row publishes — a whole blob served for the wrong identity, or an
    /// identity row that records no published keys to bind to. Distinct from
    /// [`Self::HpkeFailed`] for [`Self::CommitmentMismatch`]'s reason: the blob
    /// is *well-formed* and genuinely sealed to this bridge; it is the wrong one.
    #[error("unsealed identity keys are not the identity's published keys: {0}")]
    PublishedKeyMismatch(String),
}

/// Format version field. Decoded from the CBOR `"v"` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatVersion(pub u8);

impl FormatVersion {
    pub fn current() -> Self {
        Self(BLOB_FORMAT_VERSION)
    }

    /// Decoders MUST reject unknown versions per spec § Format versioning.
    pub fn check_supported(self) -> Result<(), UnwrapError> {
        if self.0 == BLOB_FORMAT_VERSION {
            Ok(())
        } else {
            Err(UnwrapError::UnsupportedVersion(self.0))
        }
    }
}

/// Read a blob's format stamp — its `"v"` key — and refuse a version this
/// build does not speak, BEFORE the blob's strict decode (`transport.md` §
/// Schema and forward-compat discipline → *Rule 3 in full*, the `ladder`
/// ground: the stamp is read before the strict decode, or the refusal reads
/// as corruption). Every blob is a CBOR map carrying `"v"`; the body's own
/// shape is the version's to define, so a later version's body — a new
/// [`SerKdfParams`] variant, a reshaped field — would fail the strict decode
/// as a CBOR error and be reported as a damaged blob. Decoding only the stamp
/// ignores every other key, whatever it holds.
///
/// Bytes whose stamp cannot be read at all are left to the strict decode,
/// which reports them as the malformed input they are; the decoders still
/// check the version after it too, which is then a no-op.
///
/// # Errors
///
/// [`UnwrapError::UnsupportedVersion`] for a stamp naming another version.
pub fn check_stamp_before_decode(bytes: &[u8]) -> Result<(), UnwrapError> {
    #[derive(Deserialize)]
    struct Stamp {
        #[serde(rename = "v")]
        version: u8,
    }
    match fauna_cbor::decode_strict::<Stamp>(bytes) {
        Ok(stamp) => FormatVersion(stamp.version).check_supported(),
        Err(_) => Ok(()),
    }
}

/// Canonical AAD binding for a blob. Constructed from the blob's
/// `(version, kind, ix)` tuple, encoded as canonical DAG-CBOR. Bound
/// at AEAD time so that tampering with any of the three or
/// substituting a blob across slots fails AEAD verify.
#[derive(Debug, Clone)]
pub struct AadBinding {
    inner: CborValue,
}

impl AadBinding {
    /// AAD for a wrapped-MSEK blob, indexed by `(actor_id, credential_id)`.
    pub fn for_wrapped_msek(actor_id: &[u8; 32], credential_id: &str) -> Self {
        Self {
            inner: build_aad(
                "wrapped-msek",
                &[
                    CborValue::Bytes(actor_id.to_vec()),
                    CborValue::String(credential_id.into()),
                ],
            ),
        }
    }

    /// AAD for an MLS-state snapshot blob, indexed by `(actor_id,)`.
    pub fn for_mls_snapshot(actor_id: &[u8; 32]) -> Self {
        Self {
            inner: build_aad("mls-snapshot", &[CborValue::Bytes(actor_id.to_vec())]),
        }
    }

    /// AAD for a **seed-escrow** blob, indexed by `(actor_id,)`.
    ///
    /// The escrow blob is the identity seed HPKE-sealed to the RecoveryKey's
    /// derived X25519 public key and stored opaque on the home nest, so the
    /// phrase alone can recover an account after total device loss
    /// (`docs/goal/behavior/identity-succession.md` § Seed escrow, which
    /// specifies exactly this `(kind = "seed-escrow", actor_id)` binding).
    ///
    /// The distinct `"seed-escrow"` kind tag is what stops an escrow ciphertext
    /// from ever being opened as an `mls-snapshot` (or vice-versa): these two
    /// are indexed identically, by `(actor_id,)` alone, so **only** the kind tag
    /// separates their AAD. That matters more here than for the other kinds —
    /// the escrow plaintext is the identity seed itself, the most valuable
    /// secret in the system.
    pub fn for_seed_escrow(actor_id: &[u8; 32]) -> Self {
        Self {
            inner: build_aad("seed-escrow", &[CborValue::Bytes(actor_id.to_vec())]),
        }
    }

    /// AAD for the **predecessor section** of a seed-escrow blob
    /// (`identity-succession.md` § Seed escrow: the successor's blob carries
    /// the predecessor seed(s) until the corpus re-seal completes). Bound to
    /// the *successor's* actor id — the blob owner — so a section lifted from
    /// one account's blob never opens under another's. The distinct
    /// `"seed-escrow-pred"` kind tag domain-separates it from the primary
    /// seal, so a predecessor ciphertext can never be opened as the account's
    /// own seed, nor vice versa, even at a wrong-shape call site.
    pub fn for_seed_escrow_predecessors(actor_id: &[u8; 32]) -> Self {
        Self {
            inner: build_aad("seed-escrow-pred", &[CborValue::Bytes(actor_id.to_vec())]),
        }
    }

    /// AAD for a WebDAV served-set key blob, indexed by `(actor_id,)`. The
    /// per-actor `WebdavKeysBlob` (WebDAV files server, `webdav-server.md`
    /// § Key model) is the MSEK-sealed sibling of the MLS snapshot; the distinct
    /// `"webdav-keys"` kind tag domain-separates it so a `webdav-keys` ciphertext
    /// can never be opened as an `mls-snapshot` (or vice-versa) even if a future
    /// bug surfaced a wrong-shape decrypt site.
    pub fn for_webdav_keys(actor_id: &[u8; 32]) -> Self {
        Self {
            inner: build_aad("webdav-keys", &[CborValue::Bytes(actor_id.to_vec())]),
        }
    }

    /// AAD for a wrapped-submission-token blob, indexed by
    /// `(actor_id, credential_id)`.
    pub fn for_submission_token(actor_id: &[u8; 32], credential_id: &str) -> Self {
        Self {
            inner: build_aad(
                "submission-token",
                &[
                    CborValue::Bytes(actor_id.to_vec()),
                    CborValue::String(credential_id.into()),
                ],
            ),
        }
    }

    /// AAD (and HPKE info) for an R14 **generation wrap** — one generation's
    /// key X-Wing-sealed to one device's KEM public key, indexed by
    /// `(generation_id, target_device)`. One binding serves both carriage
    /// vehicles — the mint entry's inline member wraps and the
    /// `fauna.state.generation-wrap` top-up rows — because the sealed claim is
    /// identical in both: *this generation's key, for this device*. The
    /// binding is what stops a wrap lifted from another generation (or another
    /// device's slot) from opening: substitution fails AEAD before the key
    /// commitment check even runs (`generation_wraps` owns both checks).
    pub fn for_generation_wrap(generation_id: &[u8; 32], target_device: &[u8; 32]) -> Self {
        Self {
            inner: build_aad(
                "generation-wrap",
                &[
                    CborValue::Bytes(generation_id.to_vec()),
                    CborValue::Bytes(target_device.to_vec()),
                ],
            ),
        }
    }

    /// AAD (and HPKE info) for an R14 **generation escrow wrap** — one
    /// generation's key X-Wing-sealed to a published escrow target, indexed by
    /// `(generation_id, target_key)` where `target_key` is the
    /// `fauna.state.escrow-target` row's logical key (`identity/<actor-id-hex>`
    /// for the identity-derived target; additive holder rows bind under their own
    /// keys). The distinct `"generation-escrow"` kind tag domain-separates an
    /// escrow ciphertext from every device wrap, mirroring the
    /// seed-escrow-vs-mls-snapshot precedent above: the plaintext is the same
    /// shape, so only the tag separates the AADs.
    pub fn for_generation_escrow(generation_id: &[u8; 32], target_key: &str) -> Self {
        Self {
            inner: build_aad(
                "generation-escrow",
                &[
                    CborValue::Bytes(generation_id.to_vec()),
                    CborValue::String(target_key.into()),
                ],
            ),
        }
    }

    /// AAD (and HPKE info) for a T20 **group generation wrap** — one storage
    /// group's generation key X-Wing-sealed to one roster entry's reception
    /// public key, indexed by `(group_generation_id, entry_id)`. The entry id
    /// (never the member actor) is the slot: reception keys hang off roster
    /// entries, so a re-admitted member's fresh entry is a fresh slot. One
    /// binding serves both carriage vehicles (inline mint wraps and
    /// `fauna.group.generation-wrap` top-up rows), and the distinct kind tag
    /// domain-separates every group wrap from every R14 device wrap — the
    /// same ids could not collide anyway (different id contexts), but the
    /// AAD does not rely on that.
    pub fn for_group_generation_wrap(group_generation_id: &[u8; 32], entry_id: &[u8; 32]) -> Self {
        Self {
            inner: build_aad(
                "group-generation-wrap",
                &[
                    CborValue::Bytes(group_generation_id.to_vec()),
                    CborValue::Bytes(entry_id.to_vec()),
                ],
            ),
        }
    }

    /// AAD (and HPKE info) for a T20 **group admission bundle** — the
    /// scope's machinery root + retained generation bundle X-Wing-sealed to
    /// a joiner's reception public key at admission, indexed by
    /// `(scope_id, entry_id)`: the bundle is minted for one roster cell of
    /// one scope, so a bundle lifted from another scope (or another
    /// member's admission) fails AEAD before any commitment check runs.
    /// Distinct kind tag from the per-generation wraps above — same
    /// plaintext *family*, different claim ("this scope's whole machinery
    /// reach" vs "this one generation's key").
    pub fn for_group_admission_bundle(scope_id: &[u8; 32], entry_id: &[u8; 32]) -> Self {
        Self {
            inner: build_aad(
                "group-admission-bundle",
                &[
                    CborValue::Bytes(scope_id.to_vec()),
                    CborValue::Bytes(entry_id.to_vec()),
                ],
            ),
        }
    }

    /// AAD (used as HPKE info) for a TLS cert blob, indexed by
    /// `(bridge_role, bridge_id, domain)`.
    pub fn for_tls_cert(bridge_role: &str, bridge_id: &str, domain: &str) -> Self {
        Self {
            inner: build_aad(
                "tls-cert",
                &[
                    CborValue::String(bridge_role.into()),
                    CborValue::String(bridge_id.into()),
                    CborValue::String(domain.into()),
                ],
            ),
        }
    }

    /// AAD (used as HPKE info) for a per-recipient inbound-mail-record
    /// envelope. No index parameters — recipient-targeting is already
    /// covered by HPKE's KEM (the encapsulated key is targeted at the
    /// recipient's pubkey), so the AAD only carries the kind tag for
    /// cross-shape domain separation against the other wrapped blob
    /// kinds (a `mail-record` ciphertext can never be opened as a
    /// `tls-cert` / `mls-snapshot`, even if a future bug
    /// surfaced a wrong-shape decrypt site).
    pub fn for_mail_record() -> Self {
        Self {
            inner: build_aad("mail-record", &[]),
        }
    }

    /// AAD (used as HPKE info) for a capability-grant wrapped scope key,
    /// indexed by the full scope tuple `(owner_actor_id, class, kind, tier,
    /// epoch)`.
    ///
    /// This *generalizes* the capability-scope taxonomy's stated `(class,
    /// kind)` binding to the full tuple — the safer 1000-session default,
    /// matching the `for_tls_cert`/`for_wrapped_msek` precedent of
    /// binding **every** index component. Binding all five closes three
    /// substitution paths a `(class, kind)`-only AAD would leave open: a
    /// `tier-3` post key passed as `tier-1`, a stale `epoch e` key passed as
    /// `epoch e+1` (the crux of epoch-sealed expiry actually biting), and
    /// owner X's blob confused for owner Y's. `kind`/`tier`/`epoch` are
    /// optional (a keyless `content.label-write` has no kind; only `post`
    /// carries a tier; a master-key grant has no epoch) and encode as CBOR
    /// `null` when absent, so `Some(x)` and `None` produce distinct AAD.
    ///
    /// Refutable (per the taxonomy's own discipline): a session that shows a
    /// narrower binding suffices may tighten it, stating why the closed paths
    /// stay closed. Design spec § Phase 2 Step 2 § 2.2.
    ///
    /// `set` (the folder-scope set-name qualifier, 2026-07-12) joins the
    /// index **only when present** — a set-less tuple produces the exact
    /// 5-element AAD bytes minted before the field existed, so every at-rest
    /// grant keeps opening (within-major additive compat). A 5- and a
    /// 6-element `ix` list cannot collide under canonical CBOR (distinct
    /// length prefixes), so `set: Some(x)` is still domain-separated from
    /// every set-less binding, and set A's wrap fails AEAD-open when
    /// presented for set B.
    ///
    /// `factor` (the per-labeler license qualifier, 2026-09-27) joins the same way, as a **7th** element: when present, the
    /// `set` slot is always emitted (as `null` if absent) so the list length
    /// alone tells a factor-bearing binding from a set-bearing one, and a
    /// factor-less tuple still produces the pre-existing 5- or 6-element
    /// bytes. This is what makes the license unforgeable by the store: the
    /// wrap that opens the owner's mail for labeler A fails AEAD-open when a
    /// nest presents it as licensing labeler B, and a factor-less (built-in
    /// scanner) wrap fails when presented as licensing any labeler at all.
    pub fn for_capability(
        owner_actor_id: &[u8; 32],
        class: &str,
        kind: Option<&str>,
        tier: Option<&str>,
        epoch: Option<u64>,
        set: Option<&str>,
        factor: Option<&str>,
    ) -> Self {
        let mut ix = vec![
            CborValue::Bytes(owner_actor_id.to_vec()),
            CborValue::String(class.into()),
            kind.map_or(CborValue::Null, |k| CborValue::String(k.into())),
            tier.map_or(CborValue::Null, |t| CborValue::String(t.into())),
            epoch.map_or(CborValue::Null, |e| CborValue::Integer(e.into())),
        ];
        match (set, factor) {
            (Some(s), None) => ix.push(CborValue::String(s.into())),
            (set, Some(f)) => {
                ix.push(set.map_or(CborValue::Null, |s| CborValue::String(s.into())));
                ix.push(CborValue::String(f.into()));
            }
            (None, None) => {}
        }
        Self {
            inner: build_aad("capability-grant", &ix),
        }
    }

    /// AAD (used as HPKE info) for a spam-model deployment-baseline **holder
    /// copy** ([`SpamModelCopyBlob`]), indexed by the contributing owner's
    /// actor id. Binding the owner closes the substitution path where actor
    /// X's copy is presented (by a compromised nest) as actor Y's — the
    /// holder attributes each merged contribution to an owner for the
    /// k-anonymity count, so mis-attribution must fail AEAD-open rather than
    /// silently double-count. The kind tag domain-separates the copy from
    /// every other wrapped-blob shape (a holder-copy ciphertext can never be
    /// opened as a `tls-cert` / `mail-record` / capability blob).
    /// `mail-spam.md` § Encrypted-mode interaction (ratified 2026-07-13).
    pub fn for_spam_model_copy(owner_actor_id: &[u8; 32]) -> Self {
        Self {
            inner: build_aad(
                "spam-model-copy",
                &[CborValue::Bytes(owner_actor_id.to_vec())],
            ),
        }
    }

    /// AAD (used as HPKE info) for a mailbox-export session key
    /// ([`ExportSessionKeyBlob`]), indexed by the exporting user's own actor
    /// id. `mail-export.md` § Key material.
    ///
    /// **Why the actor and not the session.** The wrapped key travels to the
    /// nest *inside* `start_export_session`, and the session id is minted by
    /// the nest in that same call's reply — so at seal time there is no
    /// session id to bind. The gap is closed one layer up rather than left
    /// open: every frame of the blob authenticates its own `session_id`
    /// (§ Blob shape on disk), so a nest that moved the wrapped key of one of
    /// actor A's sessions onto another of A's rows produces a download whose *first
    /// frame* fails AEAD — the substitution is caught, just at the frame
    /// instead of the key. Binding the actor still closes the cross-user path
    /// (user X's session key can never be opened as user Y's), which the
    /// frame AAD does not cover.
    ///
    /// The kind tag domain-separates it from every other wrapped-blob shape:
    /// an `export-session-key` ciphertext can never be opened as a
    /// `mail-record`, and a mail record can never be opened as a session key
    /// — which matters here because both are sealed to the *same* recipient
    /// key material (the user's MSEK-derived standing pair).
    pub fn for_export_session_key(actor_id: &[u8; 32]) -> Self {
        Self {
            inner: build_aad("export-session-key", &[CborValue::Bytes(actor_id.to_vec())]),
        }
    }

    /// AAD (used as HPKE info) for a per-user ATProto identity-key blob
    /// ([`AtprotoIdentityBlob`]), indexed by the owning user's actor id. The
    /// kind tag domain-separates it from every other wrapped-blob shape; the
    /// actor index closes user-substitution (user X's sealed signing key can
    /// never be opened as user Y's, even by the legitimate recipient bridge).
    /// `atproto-pds-bridge.md` § State & data shape.
    pub fn for_atproto_identity(actor_id: &[u8; 32]) -> Self {
        Self {
            inner: build_aad("atproto-identity", &[CborValue::Bytes(actor_id.to_vec())]),
        }
    }

    /// AAD (used as HPKE info) for the bridge-wide ATProto session-token
    /// secret blob ([`AtprotoSessionSecretBlob`]), indexed by
    /// `(bridge_role, bridge_id)` — the bridge-scoped keying of
    /// [`Self::for_tls_cert`] minus the domain (the secret signs session
    /// tokens for every domain the PDS serves). The kind tag
    /// domain-separates it from every other wrapped-blob shape; the index
    /// closes bridge-substitution (one deployment's PDS secret can never be
    /// opened under another bridge identity's fetch, even by a legitimate
    /// recipient). `atproto-pds-full.md` § Key material inventory.
    pub fn for_atproto_session_secret(bridge_role: &str, bridge_id: &str) -> Self {
        Self {
            inner: build_aad(
                "atproto-session-secret",
                &[
                    CborValue::String(bridge_role.into()),
                    CborValue::String(bridge_id.into()),
                ],
            ),
        }
    }

    // `atproto-as-key` was the domain tag of the retired bridge-held OAuth
    // authorization-server signing key. Never reuse it for a new blob shape:
    // ciphertexts sealed under it may still exist in a bridge's memory or logs.

    /// Encode the AAD to canonical DAG-CBOR bytes.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        fauna_cbor::encode_canonical(&self.inner)
            .expect("CBOR encode of fixed AAD shape cannot fail")
    }
}

fn build_aad(kind: &'static str, ix: &[CborValue]) -> CborValue {
    // `fauna_cbor::encode_canonical` sorts map keys length-first then
    // bytewise (RFC 8949 § 4.2.1 / dag-cbor), so the wire order is
    // (v, ix, kind) regardless of construction order here. For our keys:
    //   "v"    → 0x61 0x76                  (2 bytes)
    //   "ix"   → 0x62 0x69 0x78             (3 bytes)
    //   "kind" → 0x64 0x6b 0x69 0x6e 0x64   (5 bytes)
    // Both seal and unseal sides (incl. the Go `internal/dagcbor` and
    // Swift ports) MUST produce identical AAD bytes; the canonical
    // encoder guarantees that. Locked by `aad_wrapped_msek_golden_bytes`.
    CborValue::Map(std::collections::BTreeMap::from([
        (
            "v".to_string(),
            CborValue::Integer(BLOB_FORMAT_VERSION.into()),
        ),
        ("ix".to_string(), CborValue::List(ix.to_vec())),
        ("kind".to_string(), CborValue::String(kind.to_string())),
    ]))
}

/// Per-credential MLS-key blob. Plaintext is a 32-byte MSEK.
///
/// On the wire as canonical DAG-CBOR per spec § Wire format and CDDL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedMsekBlob {
    /// Format version. Encoded as map key `"v"`.
    #[serde(rename = "v")]
    pub version: u8,
    /// Discriminator. Always `"wrapped-msek"`.
    #[serde(rename = "kind")]
    pub kind: String,
    /// `[actor_id (32B), credential_id (utf-8)]`.
    #[serde(rename = "ix")]
    pub index: WrappedMsekIndex,
    /// KDF parameters (argon2id Interactive default for PLAIN; HKDF for OAUTHBEARER).
    #[serde(rename = "kdf")]
    pub kdf: SerKdfParams,
    /// 16-byte KDF salt.
    #[serde(rename = "salt")]
    pub salt: ByteBuf,
    /// 12-byte AEAD nonce.
    #[serde(rename = "nonce")]
    pub nonce: ByteBuf,
    /// AEAD ciphertext (includes 16-byte Poly1305 tag).
    #[serde(rename = "ct")]
    pub ciphertext: ByteBuf,
}

/// Index for a wrapped-MSEK blob. CBOR-encoded as a 2-tuple.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedMsekIndex(
    /// actor_id
    #[serde(with = "serde_bytes")]
    pub Vec<u8>,
    /// credential_id
    pub String,
);

/// Wire-side serialization of `KdfParams`. Internally tagged by the
/// `"alg"` discriminant per the CDDL.
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `ladder` ground: a new variant raises the format
/// version the reader checks before it decodes, so there is no unknown arm). A
/// new variant is an edit to `tools/check-additive-evolution/enum_ledger.txt`,
/// made in the same change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "alg")]
pub enum SerKdfParams {
    #[serde(rename = "argon2id")]
    Argon2id {
        /// Argon2 algorithm version (`0x13` for v1.3).
        #[serde(rename = "ver")]
        argon_version: u8,
        m: u32,
        t: u32,
        p: u32,
    },
    #[serde(rename = "hkdf-sha256")]
    HkdfSha256 {},
}

impl WrappedMsekBlob {
    /// Encode to canonical DAG-CBOR bytes.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on a CBOR encoding failure
    /// (practically unreachable for the fixed wire shape).
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode and validate a wrapped-MSEK blob from canonical CBOR.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for any of: bad CBOR,
    /// unknown version, wrong `kind` discriminator, wrong byte length
    /// for `actor_id` (must be 32), `salt` (must be 16), or `nonce`
    /// (must equal `AEAD_NONCE_LEN`).
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        check_stamp_before_decode(bytes)?;
        let blob: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))?;
        FormatVersion(blob.version).check_supported()?;
        if blob.kind != "wrapped-msek" {
            return Err(UnwrapError::InvalidFormat(format!(
                "expected kind=wrapped-msek, got {}",
                blob.kind
            )));
        }
        if blob.index.0.len() != 32 {
            return Err(UnwrapError::InvalidFormat(format!(
                "actor_id must be 32 bytes, got {}",
                blob.index.0.len()
            )));
        }
        if blob.salt.len() != 16 {
            return Err(UnwrapError::InvalidFormat(format!(
                "salt must be 16 bytes, got {}",
                blob.salt.len()
            )));
        }
        if blob.nonce.len() != crate::wrapped_blob::aead::AEAD_NONCE_LEN {
            return Err(UnwrapError::InvalidFormat(format!(
                "nonce must be {} bytes, got {}",
                crate::wrapped_blob::aead::AEAD_NONCE_LEN,
                blob.nonce.len()
            )));
        }
        Ok(blob)
    }
}

/// MLS-state snapshot blob. Plaintext is the user's serialized
/// **read-only** MLS state. Sealed under MSEK (not credential-derived).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlsSnapshotBlob {
    #[serde(rename = "v")]
    pub version: u8,
    #[serde(rename = "kind")]
    pub kind: String,
    #[serde(rename = "ix")]
    pub index: MlsSnapshotIndex,
    #[serde(rename = "nonce")]
    pub nonce: ByteBuf,
    #[serde(rename = "ct")]
    pub ciphertext: ByteBuf,
}

/// Index for an MLS-state snapshot blob. CBOR-encoded as a
/// 1-element array containing the 32-byte `actor_id`, per the
/// CDDL `"ix": [actor_id]`.
///
/// Implemented with hand-rolled `Serialize` / `Deserialize` because
/// serde treats single-field tuple structs as transparent (newtype),
/// which would emit a bare bstr instead of the spec-mandated
/// 1-element array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MlsSnapshotIndex(pub Vec<u8>);

impl serde::Serialize for MlsSnapshotIndex {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = ser.serialize_seq(Some(1))?;
        seq.serialize_element(serde_bytes::Bytes::new(&self.0))?;
        seq.end()
    }
}

impl<'de> serde::Deserialize<'de> for MlsSnapshotIndex {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let v: Vec<serde_bytes::ByteBuf> = serde::Deserialize::deserialize(de)?;
        if v.len() != 1 {
            return Err(serde::de::Error::custom(format!(
                "ix must have exactly 1 element, got {}",
                v.len()
            )));
        }
        Ok(MlsSnapshotIndex(v.into_iter().next().unwrap().into_vec()))
    }
}

impl MlsSnapshotBlob {
    /// Encode to canonical DAG-CBOR bytes.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on a CBOR encoding failure
    /// (practically unreachable for the fixed wire shape).
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode and validate from canonical CBOR.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR, unknown
    /// version, wrong `kind`, or wrong field length.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        check_stamp_before_decode(bytes)?;
        let blob: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))?;
        FormatVersion(blob.version).check_supported()?;
        if blob.kind != "mls-snapshot" {
            return Err(UnwrapError::InvalidFormat(format!(
                "expected kind=mls-snapshot, got {}",
                blob.kind
            )));
        }
        if blob.index.0.len() != 32 {
            return Err(UnwrapError::InvalidFormat(
                "actor_id must be 32 bytes".into(),
            ));
        }
        if blob.nonce.len() != crate::wrapped_blob::aead::AEAD_NONCE_LEN {
            return Err(UnwrapError::InvalidFormat(format!(
                "nonce must be {} bytes, got {}",
                crate::wrapped_blob::aead::AEAD_NONCE_LEN,
                blob.nonce.len()
            )));
        }
        Ok(blob)
    }
}

/// **Seed-escrow blob** — the identity seed HPKE-sealed to the RecoveryKey's
/// derived X25519 public half, resting opaque on the home nest
/// (`docs/goal/behavior/identity-succession.md` § Seed escrow).
///
/// The third member of the `(actor_id,)`-indexed family, beside
/// [`MlsSnapshotBlob`] and [`WebdavKeysBlob`] — but the only one sealed to a key
/// the nest *never* sees any half of, and the only one whose plaintext is the
/// account itself. Its whole purpose is that a phrase holder with no device and
/// no session can recover the seed; the nest is a bit-store on that path
/// (`identity-succession.md:104` — enforcer and distributor, never authorizer).
///
/// The container lives in shared Rust so all 7 apps seal and open the same
/// bytes: a per-app framing here would be a recovery blob that only the
/// client that wrote it can read, discovered at the worst possible moment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedEscrowBlob {
    #[serde(rename = "v")]
    pub version: u8,
    #[serde(rename = "kind")]
    pub kind: String,
    #[serde(rename = "ix")]
    pub index: SeedEscrowIndex,
    #[serde(rename = "hpke")]
    pub hpke: HpkeWire,
    /// The **predecessor section** (ratified 2026-08-03,
    /// `identity-succession.md` § Seed escrow): a second HPKE seal — same
    /// recipient key, its own [`AadBinding::for_seed_escrow_predecessors`]
    /// binding — whose plaintext is the canonical encoding of a
    /// [`PredecessorSeedList`]. Present only on a successor's blob during the
    /// corpus re-seal window; dropped again at the next blob write once the
    /// re-seal completes. **Additive by construction**: `skip_serializing_if`
    /// keeps a predecessor-less blob byte-identical to the pre-field
    /// encoding, and an older decoder ignores the key — its restore recovers
    /// the account and merely misses the predecessor corpus, the ratified
    /// degradation. Deliberately a *sibling seal*, not a v2 plaintext: the
    /// primary plaintext stays exactly the 32-byte seed every shipped client
    /// enforces, so no written-down phrase ever meets a blob it cannot open.
    #[serde(rename = "pred", default, skip_serializing_if = "Option::is_none")]
    pub pred: Option<HpkeWire>,
}

/// The predecessor section's plaintext: `(actor_id, seed)` pairs in chain
/// order, oldest first. A separate wire struct with `ByteBuf` fields (both
/// ride as byte strings, like every raw-byte field) rather than the public
/// API's fixed arrays. No `Debug` derive — the seed field is an
/// identity secret (the same custody rule every kit-bearing type follows).
#[derive(Clone, Serialize, Deserialize)]
pub struct PredecessorSeedEntry {
    #[serde(rename = "a")]
    pub actor_id: ByteBuf,
    #[serde(rename = "s")]
    pub seed: ByteBuf,
}

/// The list the predecessor section seals — its own named type so the
/// canonical encoding has one owner.
#[derive(Clone, Serialize, Deserialize)]
pub struct PredecessorSeedList(pub Vec<PredecessorSeedEntry>);

/// [`SeedEscrowBlob`] index: the owning actor id (32 bytes), AAD-bound via
/// [`AadBinding::for_seed_escrow`].
///
/// Derive-based (a bare bstr on the wire) like [`AtprotoIdentityIndex`] and
/// [`SpamModelCopyIndex`], not the hand-rolled 1-element-array form
/// [`MlsSnapshotIndex`] carries: that array shape is the frozen CDDL of the
/// MSEK-sealed family, and seed escrow is a new HPKE container with no legacy
/// wire to match. The load-bearing binding is the AAD either way.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedEscrowIndex(pub ByteBuf /* actor_id */);

impl SeedEscrowBlob {
    /// The `kind` tag every seed-escrow container carries — the same string the
    /// AAD is built from, so a mismatch between container and binding is a
    /// decode error rather than an AEAD failure with no explanation.
    pub const KIND: &'static str = "seed-escrow";

    /// Encode to canonical DAG-CBOR bytes — what `fauna.recovery.escrow.put`
    /// stores and `fauna.recovery.escrow.fetch` returns, opaque to the nest.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on a CBOR encoding failure (practically
    /// unreachable for the fixed wire shape).
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode and validate from canonical CBOR.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR, an unsupported
    /// version, a wrong `kind`, or a malformed actor id.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        check_stamp_before_decode(bytes)?;
        let blob: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))?;
        FormatVersion(blob.version).check_supported()?;
        if blob.kind != Self::KIND {
            return Err(UnwrapError::InvalidFormat(format!(
                "expected kind={}, got {}",
                Self::KIND,
                blob.kind
            )));
        }
        if blob.index.0.len() != 32 {
            return Err(UnwrapError::InvalidFormat(
                "actor_id must be 32 bytes".into(),
            ));
        }
        Ok(blob)
    }
}

/// WebDAV served-set key blob. Plaintext is the actor's
/// [`crate::wrapped_blob::webdav_keys_plaintext::WebdavKeysPlaintext`] — the
/// per-served-set content keys the MDA needs to serve WebDAV. Sealed under MSEK
/// (not credential-derived), the exact sibling of [`MlsSnapshotBlob`]
/// (`webdav-server.md` § Key model, `key-material-hierarchy.md` § Path B-sibling-3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebdavKeysBlob {
    #[serde(rename = "v")]
    pub version: u8,
    #[serde(rename = "kind")]
    pub kind: String,
    #[serde(rename = "ix")]
    pub index: WebdavKeysIndex,
    #[serde(rename = "nonce")]
    pub nonce: ByteBuf,
    #[serde(rename = "ct")]
    pub ciphertext: ByteBuf,
}

/// Index for a WebDAV served-set key blob. CBOR-encoded as a 1-element array
/// containing the 32-byte `actor_id`, per the CDDL `"ix": [actor_id]` — the
/// exact shape of [`MlsSnapshotIndex`]. Hand-rolled serde for the same reason
/// (serde would flatten a single-field tuple struct to a bare bstr).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebdavKeysIndex(pub Vec<u8>);

impl serde::Serialize for WebdavKeysIndex {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = ser.serialize_seq(Some(1))?;
        seq.serialize_element(serde_bytes::Bytes::new(&self.0))?;
        seq.end()
    }
}

impl<'de> serde::Deserialize<'de> for WebdavKeysIndex {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let v: Vec<serde_bytes::ByteBuf> = serde::Deserialize::deserialize(de)?;
        if v.len() != 1 {
            return Err(serde::de::Error::custom(format!(
                "ix must have exactly 1 element, got {}",
                v.len()
            )));
        }
        Ok(WebdavKeysIndex(v.into_iter().next().unwrap().into_vec()))
    }
}

impl WebdavKeysBlob {
    /// Encode to canonical DAG-CBOR bytes.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on a CBOR encoding failure
    /// (practically unreachable for the fixed wire shape).
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode and validate from canonical CBOR.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR, unknown
    /// version, wrong `kind`, or wrong field length.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        check_stamp_before_decode(bytes)?;
        let blob: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))?;
        FormatVersion(blob.version).check_supported()?;
        if blob.kind != "webdav-keys" {
            return Err(UnwrapError::InvalidFormat(format!(
                "expected kind=webdav-keys, got {}",
                blob.kind
            )));
        }
        if blob.index.0.len() != 32 {
            return Err(UnwrapError::InvalidFormat(
                "actor_id must be 32 bytes".into(),
            ));
        }
        if blob.nonce.len() != crate::wrapped_blob::aead::AEAD_NONCE_LEN {
            return Err(UnwrapError::InvalidFormat(format!(
                "nonce must be {} bytes, got {}",
                crate::wrapped_blob::aead::AEAD_NONCE_LEN,
                blob.nonce.len()
            )));
        }
        Ok(blob)
    }
}

/// Per-credential submission-token blob. AEAD-sealed under a
/// credential-derived key; plaintext is a canonically-encoded
/// SubmissionToken.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedSubmissionTokenBlob {
    #[serde(rename = "v")]
    pub version: u8,
    #[serde(rename = "kind")]
    pub kind: String,
    #[serde(rename = "ix")]
    pub index: WrappedMsekIndex, // (actor_id, credential_id) — same shape
    #[serde(rename = "kdf")]
    pub kdf: SerKdfParams,
    #[serde(rename = "salt")]
    pub salt: ByteBuf,
    #[serde(rename = "nonce")]
    pub nonce: ByteBuf,
    #[serde(rename = "ct")]
    pub ciphertext: ByteBuf,
}

impl WrappedSubmissionTokenBlob {
    /// Encode to canonical DAG-CBOR bytes.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode and validate.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR, unknown
    /// version, wrong `kind`, or any wrong field length.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        check_stamp_before_decode(bytes)?;
        let blob: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))?;
        FormatVersion(blob.version).check_supported()?;
        if blob.kind != "submission-token" {
            return Err(UnwrapError::InvalidFormat(format!(
                "expected kind=submission-token, got {}",
                blob.kind
            )));
        }
        if blob.index.0.len() != 32 {
            return Err(UnwrapError::InvalidFormat(
                "actor_id must be 32 bytes".into(),
            ));
        }
        if blob.salt.len() != 16 {
            return Err(UnwrapError::InvalidFormat(format!(
                "salt must be 16 bytes, got {}",
                blob.salt.len()
            )));
        }
        if blob.nonce.len() != crate::wrapped_blob::aead::AEAD_NONCE_LEN {
            return Err(UnwrapError::InvalidFormat(format!(
                "nonce must be {} bytes, got {}",
                crate::wrapped_blob::aead::AEAD_NONCE_LEN,
                blob.nonce.len()
            )));
        }
        Ok(blob)
    }
}

impl From<KdfParams> for SerKdfParams {
    fn from(p: KdfParams) -> Self {
        match p {
            KdfParams::Argon2id(a) => SerKdfParams::Argon2id {
                argon_version: ARGON2_VERSION_13,
                m: a.m,
                t: a.t,
                p: a.p,
            },
            KdfParams::HkdfSha256(_) => SerKdfParams::HkdfSha256 {},
        }
    }
}

impl TryFrom<&SerKdfParams> for KdfParams {
    type Error = UnwrapError;

    fn try_from(p: &SerKdfParams) -> Result<Self, Self::Error> {
        match p {
            SerKdfParams::Argon2id {
                argon_version,
                m,
                t,
                p,
            } => {
                if *argon_version != ARGON2_VERSION_13 {
                    return Err(UnwrapError::InvalidFormat(format!(
                        "unsupported argon2 version: 0x{argon_version:02x}"
                    )));
                }
                Ok(KdfParams::Argon2id(Argon2idParams {
                    m: *m,
                    t: *t,
                    p: *p,
                }))
            }
            SerKdfParams::HkdfSha256 {} => Ok(KdfParams::HkdfSha256(HkdfSha256Params)),
        }
    }
}

/// Wire shape for HPKE-protected blobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HpkeWire {
    /// Cipher suite IDs.
    #[serde(rename = "ks")]
    pub kem_suite: KemSuite,
    /// HPKE encapsulated key (X25519 ephemeral pubkey, 32 bytes).
    #[serde(rename = "enc")]
    pub enc: ByteBuf,
    /// HPKE-AEAD ciphertext (includes Poly1305 tag).
    #[serde(rename = "ct")]
    pub ciphertext: ByteBuf,
}

/// One atomic capability class from the capability-scope taxonomy — the unit
/// of `scope` a capability grant declares and each `WrappedScopeKey` serves.
///
/// `class` ∈ `content.read` | `content.label-write` | `index.write` |
/// `index.read`; `kind` ∈ `mail` | `calendar` | `post` | `folder` |
/// `spam-model` (`None` only for the keyless `content.label-write`;
/// `spam-model` is itself a **keyless** read kind — the artifact travels as a
/// sealed-to-holder copy instead of a key, `key-material-hierarchy.md` rule #7
/// resolution, ratified 2026-07-13); `tier` is present iff `kind == "post"`
/// (the period tier); `set` is present iff `kind == "folder"` (the file
/// set's address — the lowercase hex of its `set_name_hash`, never the
/// plaintext name, [`ScopeTuple::folder_set_qualifier`]; the web-paywall
/// folder scope, `mls-group-key-material.md` § M2 third distribution
/// channel). Design spec § Phase 2 Step 2 § 2.1.
///
/// `set` is additive (2026-07-12): it is omitted from the canonical encoding
/// when absent, so every pre-folder tuple's wire bytes — and therefore every
/// existing grant's AAD — are unchanged, and an old 3-key tuple decodes with
/// `set: None`.
///
/// `factor` is additive the same way (2026-09-27): the
/// **bus factor this tuple's license is confined to**. On a `content.read`
/// tuple, a wrap carrying `factor: Some("labeler:<hex>")` opens the kind only
/// to compute that labeler's score — the per-labeler grant
/// `content-moderation-and-ranking.md` § Tier-3 ratifies, made real in the
/// one place a compromised store cannot rewrite (the wrap's AAD binds it,
/// [`AadBinding::for_capability`]); on a `content.label-write` tuple it
/// licenses writing exactly that factor. `None` is the built-in perimeter
/// factors (the scanners the MDA re-runs) — today's "read and filter my
/// mail" grant, whose wraps therefore license **no** community labeler. A
/// holder matches the factor exactly in both directions: a labeler's wrap
/// never serves a built-in factor either.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeTuple {
    #[serde(rename = "class")]
    pub class: String,
    #[serde(rename = "kind")]
    pub kind: Option<String>,
    #[serde(rename = "tier")]
    pub tier: Option<String>,
    #[serde(rename = "set", default, skip_serializing_if = "Option::is_none")]
    pub set: Option<String>,
    #[serde(rename = "factor", default, skip_serializing_if = "Option::is_none")]
    pub factor: Option<String>,
}

impl ScopeTuple {
    /// Read `content.read{mail|calendar|post:tier}` — reaches the minimal
    /// opening key for one kind (§ taxonomy table).
    pub const CLASS_CONTENT_READ: &'static str = "content.read";
    /// Append a score/label to the metadata bus — keyless (§ taxonomy table).
    pub const CLASS_CONTENT_LABEL_WRITE: &'static str = "content.label-write";
    /// Write a sealed index segment for one kind — keyed by that kind's
    /// index-segment key (§ taxonomy table).
    pub const CLASS_INDEX_WRITE: &'static str = "index.write";
    /// Query the sealed index for one kind (§ taxonomy table).
    pub const CLASS_INDEX_READ: &'static str = "index.read";
    /// The W8 (account-data-plane.md § Workstreams) custody grant — **keyless always** (`derive_scope_payload` →
    /// `None`, like `content.label-write`): the nest row is the authorization +
    /// audit record the nest's revocation store answers from, never a key
    /// conveyance. The admission *witness* is the owner-signed
    /// `fauna_core::custody_grant::CustodyGrant` envelope, a separate
    /// artifact; the scope vocabulary here is
    /// `account-data-plane.md` § Replica posture → *The custody grant +
    /// ceremony*'s two forms, mapped by
    /// `fauna_client_capabilities::custody_grants`. `holder` on a custody
    /// row carries the custodian's device-principal **Ed25519** key (its
    /// peer-plane NodeId) — legitimate because no HPKE wrap ever targets it
    /// (`wrapped_keys` is empty by construction).
    pub const CLASS_CUSTODY: &'static str = "custody";
    /// The oracle's class (TP11, `key-material-hierarchy.md` § Audience:
    /// deployment infrastructure → *The oracle*) — **keyless always**, like
    /// `content.label-write`: the tuple is the user-minted audit + revocation
    /// record a third-party principal's identity-bearing operations are
    /// authorized against; the key stays with its first-party custodian. The
    /// tuple's `kind` is the operation class's name
    /// (`fauna_core::identity_op::IdentityOpClass::name`); the vocabulary, the
    /// sovereign deny list and the admission predicate are that module's.
    pub const CLASS_IDENTITY_OP: &'static str = fauna_core::identity_op::CLASS;
    /// A third-party principal's write authority over one `ext.*` kind
    /// (`third-party-kinds.md` § Principal write authority) — **keyless
    /// always**: the kind's delegable pair is one symmetric unit, carried by
    /// the `content.read` tuple beside it, so this tuple wraps nothing. Its
    /// `kind` is the full `ext.*` string and its `factor` the holder's
    /// attested writer key (`writer:<hex>`,
    /// `fauna_core::grant_event::writer_factor`) — the license confined to
    /// that key, the event-log twin folding it into `kind` as every factor
    /// is. Constructed by [`Self::content_write`].
    pub const CLASS_CONTENT_WRITE: &'static str = fauna_core::grant_event::CLASS_CONTENT_WRITE;
    /// A third-party principal's write-only ingress into one folder
    /// (`file-sync.md` § Third-party deposit ingress) — **keyless always**:
    /// the nest seals what the holder posts to the owner's recipient key, so
    /// the tuple wraps nothing and is the audit + revocation record the
    /// deposit door re-resolves at every deposit. Its `set` is the folder's
    /// row id in decimal; no `kind`, `tier` or `factor`. Constructed by
    /// [`Self::folder_deposit`].
    pub const CLASS_DEPOSIT: &'static str = fauna_core::grant_event::CLASS_DEPOSIT;

    /// `kind` value for the custody `Account` form (the owner's whole
    /// single-principal scope set, current and future — one tuple, no `set`).
    pub const KIND_CUSTODY_ACCOUNT: &'static str = "account";
    /// `kind` value for one explicit custody scope entry — the tuple's `set`
    /// carries the canonical scope string (`fauna_protocol::scope`
    /// vocabulary); the event-log twin carries it in `tier` (that type has
    /// no `set` field and is frozen).
    pub const KIND_CUSTODY_SCOPE: &'static str = "scope";

    /// `kind` value for mail content / the mail index.
    pub const KIND_MAIL: &'static str = "mail";
    /// `kind` value for calendar content / the calendar index.
    pub const KIND_CALENDAR: &'static str = "calendar";
    /// `kind` value for audience-restricted-post content / the post index.
    pub const KIND_POST: &'static str = "post";
    /// `kind` value for a `web`-mode folder's content keys (the web-paywall
    /// folder scope — `behavior/monetization.md` § Pillar 2). The tuple's
    /// `set` field names the set; the payload regime is one [`WrappedScopeKey`]
    /// per content-key generation with `epoch` = the generation `version`
    /// (`mls-group-key-material.md` § M2 third distribution channel).
    pub const KIND_FOLDER: &'static str = "folder";

    /// The `set` qualifier of a `content.read{folder}` tuple: the lowercase
    /// hex of the set's `set_name_hash` (`path-sealing.md` § the set-name
    /// plane). The grant rests on the nest, and the holder matches it against
    /// the folder row's `name_hash`, so the plaintext set name appears in
    /// neither — a sealed set's row holds none to compare against.
    pub fn folder_set_qualifier(set_name_hash: &[u8]) -> String {
        hex::encode(set_name_hash)
    }
    /// `kind` value for the per-user spam model's deployment-baseline
    /// contribution — a **keyless** read scope (`derive_scope_payload` →
    /// `None`, like `content.label-write`): the grant is the audit/revocation
    /// record and the publish-worklist authorization; the contributed model
    /// travels as a [`SpamModelCopyBlob`] sealed to the holder's own pubkey,
    /// never as a wrapped key of the user's. No `WrappedScopeKey` may ever
    /// carry this kind — the model's only opening key is the recipient-mail
    /// secret, which a grant must not wrap for a narrower purpose
    /// (`key-material-hierarchy.md` rule #7 + § Don't do these;
    /// `mail-spam.md` § Encrypted-mode interaction, ratified 2026-07-13).
    pub const KIND_SPAM_MODEL: &'static str = "spam-model";

    /// The unbounded (master-key) `content.read{mail}` tuple — five call
    /// sites across `fauna-mls`/`fauna-client-capabilities`/
    /// `fauna-capability-holder` hand-copied this exact literal before this
    /// constructor existed. Callers needing the **bounded** log-side marker
    /// use `fauna_client_capabilities::grant_log::bounded_mail_event_scope`
    /// instead — a different type (`GrantEventScope`, `tier` set to
    /// [`fauna_core::grant_event::GRANT_SCOPE_TIER_BOUNDED`]).
    #[must_use]
    pub fn mail() -> Self {
        Self {
            class: Self::CLASS_CONTENT_READ.to_string(),
            kind: Some(Self::KIND_MAIL.to_string()),
            tier: None,
            set: None,
            factor: None,
        }
    }

    /// The `content.read` tuple over one `ext.*` kind — its wrap carries the
    /// kind's delegable pair (`third-party-kinds.md` § Principal write
    /// authority: tuples are per kind, `kind` the full string).
    #[must_use]
    pub fn ext_kind_read(kind: &str) -> Self {
        Self {
            class: Self::CLASS_CONTENT_READ.to_string(),
            kind: Some(kind.to_string()),
            tier: None,
            set: None,
            factor: None,
        }
    }

    /// The keyless `content.write` tuple over one `ext.*` kind, confined to
    /// `writer` (see [`Self::CLASS_CONTENT_WRITE`]).
    #[must_use]
    pub fn content_write(kind: &str, writer: &[u8; 32]) -> Self {
        Self {
            class: Self::CLASS_CONTENT_WRITE.to_string(),
            kind: Some(kind.to_string()),
            tier: None,
            set: None,
            factor: Some(fauna_core::grant_event::writer_factor(writer)),
        }
    }

    /// The keyless `deposit` tuple over the folder whose row id is
    /// `folder_id` (see [`Self::CLASS_DEPOSIT`]).
    #[must_use]
    pub fn folder_deposit(folder_id: i64) -> Self {
        Self {
            class: Self::CLASS_DEPOSIT.to_string(),
            kind: None,
            tier: None,
            set: Some(folder_id.to_string()),
            factor: None,
        }
    }

    /// Is this exactly the `deposit` tuple over `folder_id`? Whole-tuple
    /// equality with [`Self::folder_deposit`], so a tuple carrying any other
    /// qualifier, or a non-canonical spelling of the id, admits nothing.
    #[must_use]
    pub fn is_folder_deposit_for(&self, folder_id: i64) -> bool {
        *self == Self::folder_deposit(folder_id)
    }

    /// The `content.read{folder, set}` tuple over the set whose
    /// `set_name_hash` is `set_name_hash` — the folder read twin (the paywall
    /// web-serve grant and a principal's `fauna:folder:read` grant alike;
    /// `webdav-server.md` § Key model → *A principal's read*).
    #[must_use]
    pub fn folder_read(set_name_hash: &[u8]) -> Self {
        Self {
            class: Self::CLASS_CONTENT_READ.to_string(),
            kind: Some(Self::KIND_FOLDER.to_string()),
            tier: None,
            set: Some(Self::folder_set_qualifier(set_name_hash)),
            factor: None,
        }
    }

    /// Is this exactly the folder read tuple over `set_name_hash`?
    /// Whole-tuple equality with [`Self::folder_read`].
    #[must_use]
    pub fn is_folder_read_for(&self, set_name_hash: &[u8]) -> bool {
        *self == Self::folder_read(set_name_hash)
    }

    /// The **per-labeler** mail-read tuple — [`Self::mail`] confined to one
    /// bus factor (`labeler:<hex>`, `fauna_core::scoring::labeler_factor`):
    /// its wraps open the owner's mail only for computing that labeler's
    /// score. The read half of the grant a labeler subscription over sealed
    /// mail mints.
    #[must_use]
    pub fn mail_for_factor(factor: &str) -> Self {
        Self {
            factor: Some(factor.to_string()),
            ..Self::mail()
        }
    }

    /// The keyless `content.label-write` tuple: `factor: None` licenses the
    /// built-in perimeter factors (the composed MDA role); `Some(f)`
    /// licenses writing exactly the factor `f`.
    #[must_use]
    pub fn label_write(factor: Option<&str>) -> Self {
        Self {
            class: Self::CLASS_CONTENT_LABEL_WRITE.to_string(),
            kind: None,
            tier: None,
            set: None,
            factor: factor.map(str::to_string),
        }
    }

    /// Whether this tuple's license covers `factor` — exact in both
    /// directions (`None` ⇔ the built-in factors, `Some` ⇔ that one factor),
    /// the one predicate every position that gates on a factor shares: the
    /// holder's key selection, the nest's worklist and `submit_scores` authz.
    #[must_use]
    pub fn licenses_factor(&self, factor: Option<&str>) -> bool {
        self.factor.as_deref() == factor
    }
}

/// Reconstruct a custody grant's declared scope set from its nest-blob
/// [`ScopeTuple`]s — the ONE owner of this direction (W8.6 pin N4; the
/// mint-side `fauna_client_capabilities::custody_grants::custody_tuples`
/// is the forward twin and round-trip-pins against this). Consumed by the
/// nest custody door, which re-derives the admission verdict from the LIVE
/// row on every request. `None` when the tuple list is not a custody
/// grant's at all.
///
/// Tolerance mirrors the event-log twin: an `account` entry wins over any
/// stray `scope` entries (the `Account` form is a superset, so widening on
/// a malformed mix is never a narrower verdict), and a `scope` entry with
/// no `set` string is dropped rather than invented.
pub fn custody_scope_set_from_tuples(
    scope: &[ScopeTuple],
) -> Option<fauna_core::custody_grant::CustodyScopeSet> {
    use fauna_core::custody_grant::CustodyScopeSet;
    let custody: Vec<&ScopeTuple> = scope
        .iter()
        .filter(|t| t.class == ScopeTuple::CLASS_CUSTODY)
        .collect();
    if custody.is_empty() {
        return None;
    }
    if custody
        .iter()
        .any(|t| t.kind.as_deref() == Some(ScopeTuple::KIND_CUSTODY_ACCOUNT))
    {
        return Some(CustodyScopeSet::Account);
    }
    Some(CustodyScopeSet::Scopes(
        custody
            .iter()
            .filter(|t| t.kind.as_deref() == Some(ScopeTuple::KIND_CUSTODY_SCOPE))
            .filter_map(|t| t.set.clone())
            .collect(),
    ))
}

/// Is `grant`'s authorization window open at `now_epoch_secs`? **Both**
/// bounds, which is the whole point of this helper existing.
///
/// Finding (2026-08-16): `GrantWindow` is `[epoch_start,
/// epoch_end]` and both halves are first-class — `record_mint` signs
/// `window_start` into the owner's own grant log, and the custody handshake
/// refuses a row whose window has not opened. But the nest's
/// `capability_grants` table stores only `epoch_end`, so every
/// storage-level filter (`fetch_capability_grants_for_holder` and friends)
/// can express "not expired" and *cannot* express "already started". Each
/// consumer that re-derives authorization from a decoded blob must therefore
/// check the start bound itself, and before this helper existed exactly one
/// of them did: a grant post-dated by a month authorized pulls today.
///
/// So: any code that decodes a [`GrantBlob`] to make an authorization
/// decision calls this, rather than trusting the row it came from to have
/// been window-filtered. Fails **closed** on a clock at or before the epoch.
pub fn grant_window_is_open(grant: &GrantBlob, now_epoch_secs: i64) -> bool {
    let Ok(now) = u64::try_from(now_epoch_secs) else {
        return false; // a pre-epoch clock authorizes nothing
    };
    grant.window.0 <= now && now <= grant.window.1
}

/// An HPKE-wrapped minimal key-subset serving exactly one [`ScopeTuple`],
/// sealed to a capability grant's **holder** — an enrolled bridge
/// service-user X25519 pubkey (the MDA / a scorer / an FTS-indexer), **never**
/// the actor's identity key. One `WrappedScopeKey` per key-bearing scope tuple
/// × epoch it opens; a keyless `content.label-write` tuple appears in a grant's
/// `scope` but has no `WrappedScopeKey`. Design spec § Phase 2 Step 2 § 2.1.
///
/// The wrapped plaintext is the **minimal derived content key** for one kind
/// (the recipient-mail HPKE secret, a tier `period_key`, or an index-segment
/// key) — never MSEK, `epoch_secret`, `BackupKey`, the index master, or the
/// identity seed (`key-material-hierarchy.md` rule #7). So a holder that opens
/// this can *read one content kind*; it cannot impersonate the owner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedScopeKey {
    /// Which scope tuple this key serves.
    #[serde(rename = "scope")]
    pub scope: ScopeTuple,
    /// `None` = master-key (the standing base key, every content kind today);
    /// `Some(e)` = the per-epoch key for window `e` (a future epoch-sealed
    /// kind). Bound into the seal's AAD (§ 2.2), so a stale epoch key cannot
    /// be presented for a newer window.
    #[serde(rename = "epoch")]
    pub epoch: Option<u64>,
    /// `{ks, enc, ct}` — HPKE-sealed to the holder, AAD-bound via
    /// [`AadBinding::for_capability`].
    #[serde(rename = "hpke")]
    pub hpke: HpkeWire,
}

impl WrappedScopeKey {
    /// Encode to canonical DAG-CBOR — the wire form each `appended_keys` entry
    /// a `fauna.capabilities.renew` carries takes, and the element form of a
    /// [`GrantBlob`]'s `wrapped_keys`.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode and validate a canonical-CBOR wrapped scope key — the nest's
    /// `fauna.capabilities.renew` handler parses each client-appended key this
    /// way before folding it into the stored grant.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR or a wrong HPKE `enc`
    /// length for the declared suite (the same enc-length guard [`GrantBlob`]
    /// applies to each of its wrapped keys).
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        let wk: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))?;
        if let Some(expected) = wk.hpke.kem_suite.expected_enc_len()
            && wk.hpke.enc.len() != expected
        {
            return Err(UnwrapError::InvalidFormat(format!(
                "hpke enc must be {expected} bytes for this suite, got {}",
                wk.hpke.enc.len()
            )));
        }
        Ok(wk)
    }
}

/// The authorization window of a capability grant — `[start, end]` as
/// **Unix seconds**, ALWAYS, in BOTH regimes (master-key today and
/// epoch-sealed later) — never epoch *indices* (design § Revision history
/// 2026-07-06). The landed nest expiry filter compares
/// `epoch_end >= now_epoch_secs()` (`bins/fauna-nest/src/db/capability_grants.rs`),
/// so a session that put an epoch index here would make every grant read
/// as instantly expired (index ≪ unix-now). For a master-key grant (every
/// content kind today) the window is the advisory settings-page bound
/// (§ Phase 2 Step 1). Once a kind adopts a rotating content-sealing epoch
/// (Phase 3 for mail), the window STAYS unix-seconds; the per-epoch index
/// set the grant authorizes is derived by time-intersection at mint/renew
/// and carried per-key in [`WrappedScopeKey`]'s `epoch` field, not in `window`.
/// Serializes as a CBOR 2-array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantWindow(
    pub u64, /* start_unix_secs */
    pub u64, /* end_unix_secs */
);

/// Index of a capability grant: `(owner_actor_id, grant_id)` — the storage
/// key and revocation handle. Serializes as a CBOR 2-tuple of byte strings,
/// mirroring [`WrappedMsekIndex`]. `owner_actor_id` is 32 bytes; `grant_id`
/// is a 16-byte opaque identifier the minting client chooses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantIndex(
    #[serde(with = "serde_bytes")] pub Vec<u8>, /* owner_actor_id (32) */
    #[serde(with = "serde_bytes")] pub Vec<u8>, /* grant_id (16) */
);

/// A user-minted capability grant: one **holder** (a bridge service-user)
/// plus a set of scope tuples, each key-bearing tuple carrying an HPKE-wrapped
/// minimal key-subset. Canonical dag-cbor, templated on [`TlsCertBlob`] — the
/// `{v, kind, ix, …}` shape (design § Phase 2 Step 2 § 2.1).
///
/// The mint (the user's client, holding the content root off-box) derives each
/// wrapped payload from material only it holds, HPKE-seals it to `holder`, and
/// never hands the nest a standing key — the nest stores ciphertext it cannot
/// open (`encryption-at-rest.md` § nest holds no content key;
/// `key-material-hierarchy.md` rule #4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantBlob {
    /// Format version (`"v"`). Decoders reject unknown versions.
    #[serde(rename = "v")]
    pub version: u8,
    /// Discriminator (`"kind"`). Always `"capability-grant"`.
    #[serde(rename = "kind")]
    pub kind: String,
    /// `(owner_actor_id, grant_id)` — storage key + revocation handle.
    #[serde(rename = "ix")]
    pub index: GrantIndex,
    /// The wrap target: a bridge service-user X25519 pubkey (32 bytes), from
    /// `bridge_service_users.x25519_pubkey`. **Not** the actor identity.
    #[serde(rename = "holder")]
    pub holder: ByteBuf,
    /// The authorization window `[epoch_start, epoch_end]`.
    #[serde(rename = "window")]
    pub window: GrantWindow,
    /// The DECLARED scope — a set of scope tuples (auditable; drives the
    /// settings page). A keyless `content.label-write` tuple appears here but
    /// not in `wrapped_keys`.
    #[serde(rename = "scope")]
    pub scope: Vec<ScopeTuple>,
    /// One [`WrappedScopeKey`] per KEY-BEARING scope tuple × epoch it opens.
    #[serde(rename = "wrapped_keys")]
    pub wrapped_keys: Vec<WrappedScopeKey>,
}

impl GrantBlob {
    /// The blob discriminator.
    pub const KIND: &'static str = "capability-grant";

    /// Encode to canonical DAG-CBOR.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode and validate a capability-grant blob from canonical CBOR.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR, an unknown version,
    /// a wrong `kind` discriminator, or a wrong HPKE `enc` length on any
    /// wrapped key.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        check_stamp_before_decode(bytes)?;
        let blob: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))?;
        FormatVersion(blob.version).check_supported()?;
        if blob.kind != Self::KIND {
            return Err(UnwrapError::InvalidFormat(format!(
                "expected kind={}, got {}",
                Self::KIND,
                blob.kind
            )));
        }
        for wk in &blob.wrapped_keys {
            if let Some(expected) = wk.hpke.kem_suite.expected_enc_len()
                && wk.hpke.enc.len() != expected
            {
                return Err(UnwrapError::InvalidFormat(format!(
                    "hpke enc must be {expected} bytes for this suite, got {}",
                    wk.hpke.enc.len()
                )));
            }
        }
        Ok(blob)
    }
}

/// HPKE cipher suite identifiers per RFC 9180 § 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KemSuite {
    pub kem: u16,
    pub kdf: u16,
    pub aead: u16,
}

impl KemSuite {
    /// The classical cipher suite: DHKEM(X25519, HKDF-SHA-256) (`0x0020`) /
    /// HKDF-SHA-256 (`0x0001`) / ChaCha20-Poly1305 (`0x0003`). The only suite a
    /// current build can seal or open; every blob sealed today carries it.
    pub const STANDARD: Self = Self {
        kem: 0x0020,
        kdf: 0x0001,
        aead: 0x0003,
    };

    /// Whether this descriptor is the classical [`Self::STANDARD`] suite.
    ///
    /// The crypto-agility contract (goal `architecture/security/post-quantum.md`
    /// § 7.1): unseal **dispatches** on the self-describing per-blob `ks` instead
    /// of asserting a single constant. A classical blob takes today's X25519 HPKE
    /// path; a non-classical (e.g. [`FAUNA_KEM_XWING`] hybrid) blob is opened by a
    /// later build (slice S2+) and, until then, fails with a *typed* format error
    /// rather than a silent AEAD mis-decrypt.
    pub const fn is_standard(&self) -> bool {
        self.kem == Self::STANDARD.kem
            && self.kdf == Self::STANDARD.kdf
            && self.aead == Self::STANDARD.aead
    }

    /// The expected on-wire HPKE `enc` (encapsulation) length for this suite, or
    /// `None` for an unrecognized suite.
    ///
    /// Classical is the 32-byte X25519 ephemeral pubkey; **X-Wing**
    /// ([`FAUNA_KEM_XWING`]) is the 1120-byte hybrid ciphertext. A `None` (unknown
    /// suite) keeps `from_canonical_bytes` **decode-lenient** — the shape decodes
    /// and the open-time dispatch (`hpke_open_dispatch`) returns the typed
    /// unknown-suite / unsupported error, so an additive future suite never fails
    /// to *decode*, only to *open*.
    #[must_use]
    pub fn expected_enc_len(&self) -> Option<usize> {
        if self.is_standard() {
            Some(crate::wrapped_blob::envelope::HPKE_ENC_LEN)
        } else if self.kem == FAUNA_KEM_XWING {
            Some(fauna_pq_kem::XWING_CIPHERTEXT_LEN)
        } else {
            None
        }
    }
}

/// Fauna-private HPKE KEM identifier for the **X-Wing** hybrid suite
/// (ML-KEM-768 ∥ X25519, `draft-connolly-cfrg-xwing-kem`), carried in
/// [`KemSuite::kem`].
///
/// X-Wing has no IANA-assigned HPKE KEM id yet, so a Fauna-private value in the
/// high 16-bit space is used until/unless one is assigned. This is sound because
/// every HPKE blob is **self-describing** about its suite ([`HpkeWire::kem_suite`]),
/// so only Fauna seals and opens these and the exact id is a private convention,
/// not a wire-compat constant. Because nothing is sealed with X-Wing until slice
/// S2 ships, the value can still be repointed at an IANA assignment at zero
/// migration cost before then.
///
/// S1 only **registers** the id so the unseal dispatcher can name the hybrid arm;
/// the actual X-Wing KEM is implemented in slice S2.
pub const FAUNA_KEM_XWING: u16 = 0xFC00;

/// A **mailbox-export session key**: the client-minted 256-bit key every
/// frame of one export blob is sealed under, HPKE-sealed by the user's own
/// client to the user's own MSEK-derived recipient key
/// (`mail-export.md` § Key material).
///
/// It rests in `export_sessions.blob_decryption_key_wrapped_for_actor`, which
/// is the whole reason it is wrapped rather than merely kept: the nest stores
/// it so that **any** of the user's clients — not just the one that ran the
/// wizard — can fetch it, open it, and decrypt the download. The nest never
/// sees the unwrapped key (§ Don't do these: "Don't unwrap the per-session key
/// on nest, ever").
///
/// Sealed under the post-quantum **X-Wing** suite unconditionally, with no
/// classical degrade arm. Every other `seal_*` here has one because its
/// recipient is a *different* principal whose published key set the sealer
/// cannot choose; here the sealer and the opener are the same user's clients
/// over the same MSEK, so the hybrid half is always derivable and a degrade
/// would only weaken a blob that unlocks an entire mail history. The opener
/// still handles a classical entry, because a standing keypair parsed from a
/// pre-hybrid snapshot carries no ML-KEM half.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportSessionKeyBlob {
    #[serde(rename = "v")]
    pub version: u8,
    #[serde(rename = "kind")]
    pub kind: String,
    #[serde(rename = "ix")]
    pub index: ExportSessionKeyIndex,
    #[serde(rename = "hpke")]
    pub hpke: HpkeWire,
}

/// [`ExportSessionKeyBlob`] index: the exporting user's actor id (32 bytes),
/// AAD-bound via [`AadBinding::for_export_session_key`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportSessionKeyIndex(pub ByteBuf /* exporting actor_id */);

impl ExportSessionKeyBlob {
    /// The `kind` tag every export session key carries.
    pub const KIND: &'static str = "export-session-key";

    /// Encode to canonical DAG-CBOR.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode and validate.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR, unknown version,
    /// wrong `kind`, a non-32-byte actor index, or a wrong HPKE enc length.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        check_stamp_before_decode(bytes)?;
        let blob: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))?;
        FormatVersion(blob.version).check_supported()?;
        if blob.kind != Self::KIND {
            return Err(UnwrapError::InvalidFormat(format!(
                "expected kind={}, got {}",
                Self::KIND,
                blob.kind
            )));
        }
        if blob.index.0.len() != 32 {
            return Err(UnwrapError::InvalidFormat(format!(
                "exporting actor_id index must be 32 bytes, got {}",
                blob.index.0.len()
            )));
        }
        if let Some(expected) = blob.hpke.kem_suite.expected_enc_len()
            && blob.hpke.enc.len() != expected
        {
            return Err(UnwrapError::InvalidFormat(format!(
                "hpke enc must be {expected} bytes for this suite, got {}",
                blob.hpke.enc.len()
            )));
        }
        Ok(blob)
    }
}

/// A spam-model deployment-baseline **holder copy**: an opt-in contributor's
/// per-user `SpamModel` bytes, HPKE-sealed by the contributor's own
/// client/agent to the **aggregation holder's** pubkey (the enrolled
/// mail-bridge content-processor service user). Rests nest-opaque beside the
/// contributor's `spam_models` row; served to the holder during a
/// `publish_spam_baseline` drain run only while the paired **keyless**
/// `content.read{spam-model}` grant stands. The holder opens it with its own
/// service-user key — no key of the user's is ever conveyed
/// (`key-material-hierarchy.md` § Audience: deployment infrastructure →
/// Spam-baseline holder copy; `mail-spam.md` § Encrypted-mode interaction).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpamModelCopyBlob {
    #[serde(rename = "v")]
    pub version: u8,
    #[serde(rename = "kind")]
    pub kind: String,
    #[serde(rename = "ix")]
    pub index: SpamModelCopyIndex,
    #[serde(rename = "hpke")]
    pub hpke: HpkeWire,
}

/// [`SpamModelCopyBlob`] index: the contributing owner's actor id (32 bytes),
/// AAD-bound via [`AadBinding::for_spam_model_copy`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpamModelCopyIndex(pub ByteBuf /* owner actor_id */);

impl SpamModelCopyBlob {
    /// The `kind` tag every holder copy carries.
    pub const KIND: &'static str = "spam-model-copy";

    /// Encode to canonical DAG-CBOR.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode and validate.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR, unknown version,
    /// wrong `kind`, a non-32-byte owner index, or a wrong HPKE enc length.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        check_stamp_before_decode(bytes)?;
        let blob: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))?;
        FormatVersion(blob.version).check_supported()?;
        if blob.kind != Self::KIND {
            return Err(UnwrapError::InvalidFormat(format!(
                "expected kind={}, got {}",
                Self::KIND,
                blob.kind
            )));
        }
        if blob.index.0.len() != 32 {
            return Err(UnwrapError::InvalidFormat(format!(
                "owner actor_id index must be 32 bytes, got {}",
                blob.index.0.len()
            )));
        }
        if let Some(expected) = blob.hpke.kem_suite.expected_enc_len()
            && blob.hpke.enc.len() != expected
        {
            return Err(UnwrapError::InvalidFormat(format!(
                "hpke enc must be {expected} bytes for this suite, got {}",
                blob.hpke.enc.len()
            )));
        }
        Ok(blob)
    }
}

/// Per-user ATProto identity-key blob: the bridge-custodied half of the
/// ratified key-custody split (`atproto-pds-bridge.md` § State & data shape) —
/// the repo signing key + the bridge's junior rotation key, sealed by nest to
/// the `atproto.pds` bridge's attested X25519 exactly like the TLS cert blob.
/// The user-custodied senior rotation key is NOT here (it lives in the user's
/// client credential store and never touches the nest in plaintext).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtprotoIdentityBlob {
    #[serde(rename = "v")]
    pub version: u8,
    #[serde(rename = "kind")]
    pub kind: String,
    #[serde(rename = "ix")]
    pub index: AtprotoIdentityIndex,
    #[serde(rename = "hpke")]
    pub hpke: HpkeWire,
}

/// [`AtprotoIdentityBlob`] index: the owning user's actor id (32 bytes),
/// AAD-bound via [`AadBinding::for_atproto_identity`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtprotoIdentityIndex(pub ByteBuf /* actor_id */);

/// ATProto identity-key bundle plaintext: raw 32-byte scalars for the two
/// bridge-custodied K-256 keys, zeroed on drop. Curves ride as explicit tags
/// (`"k256"`) beside each scalar so the Go bridge reconstructs keys without
/// guessing, and the did:key pubkey strings are carried for self-description
/// (they are also stored nest-side as plaintext data — the blob is the only
/// home of the SECRETS, not of the pubkeys).
#[derive(Debug, Clone, Serialize, Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct AtprotoIdentityKeyBundle {
    /// The owning user's actor id (32 bytes).
    #[zeroize(skip)]
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Repo-commit signing key: raw scalar.
    #[serde(with = "serde_bytes")]
    pub signing_priv: Vec<u8>,
    /// Curve tag for `signing_priv` (currently always `"k256"`).
    #[zeroize(skip)]
    pub signing_curve: String,
    /// The signing key's public half as a `did:key:z…` string.
    #[zeroize(skip)]
    pub signing_pub_did_key: String,
    /// The bridge's junior PLC rotation key: raw scalar.
    #[serde(with = "serde_bytes")]
    pub rotation_priv: Vec<u8>,
    /// Curve tag for `rotation_priv` (currently always `"k256"`).
    #[zeroize(skip)]
    pub rotation_curve: String,
    /// The bridge rotation key's public half as a `did:key:z…` string.
    #[zeroize(skip)]
    pub rotation_pub_did_key: String,
    #[zeroize(skip)]
    pub issued_at: u64,
}

/// What a caller opening an [`AtprotoIdentityBlob`] expects to find inside: the
/// two `did:key` strings the nest's identity row records as this identity's
/// PUBLISHED bridge-custodied keys (`atproto_identities.signing_pub` /
/// `.bridge_rotation_pub`, served beside the blob by
/// `fauna.bridges.atproto.fetch_identity_key_blob`).
///
/// The binding is to the keys and deliberately NOT to an actor id. The blob's
/// two actor ids are mint-time provenance: the identity row and its blob move
/// together to a successor while the sealed bytes cannot be re-sealed, so after
/// a succession they name an ancestor for the life of the DID — an actor
/// comparison would strand every succeeded account. The published keys are what
/// the DID document lists, and they travel with the row across any number of
/// hops (`atproto-pds-bridge.md` § State & data shape).
#[derive(Debug, Clone, Copy)]
pub struct AtprotoIdentityPublishedKeys<'a> {
    /// The repo signing key's public half as a `did:key:z…` string.
    pub signing_pub_did_key: &'a str,
    /// The bridge junior rotation key's public half as a `did:key:z…` string.
    pub rotation_pub_did_key: &'a str,
}

impl AtprotoIdentityBlob {
    /// The `kind` tag every ATProto identity blob carries.
    pub const KIND: &'static str = "atproto-identity";

    /// Encode to canonical DAG-CBOR.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode and validate.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR, unknown version,
    /// wrong `kind`, a non-32-byte actor index, or a wrong HPKE enc length.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        check_stamp_before_decode(bytes)?;
        let blob: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))?;
        FormatVersion(blob.version).check_supported()?;
        if blob.kind != Self::KIND {
            return Err(UnwrapError::InvalidFormat(format!(
                "expected kind={}, got {}",
                Self::KIND,
                blob.kind
            )));
        }
        if blob.index.0.len() != 32 {
            return Err(UnwrapError::InvalidFormat(format!(
                "actor_id index must be 32 bytes, got {}",
                blob.index.0.len()
            )));
        }
        if let Some(expected) = blob.hpke.kem_suite.expected_enc_len()
            && blob.hpke.enc.len() != expected
        {
            return Err(UnwrapError::InvalidFormat(format!(
                "hpke enc must be {expected} bytes for this suite, got {}",
                blob.hpke.enc.len()
            )));
        }
        Ok(blob)
    }
}

impl AtprotoIdentityKeyBundle {
    /// Encode to canonical DAG-CBOR.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode from canonical CBOR.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` on bad CBOR.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))
    }
}

/// Bridge-wide ATProto session-token secret blob: the HS256 secret the
/// `atproto.pds` bridge signs access/refresh JWTs with, minted nest-side on
/// first fetch (provision-on-read) and sealed to the
/// bridge's attested X25519 exactly like the TLS cert blob — nest stores
/// opaque ciphertext and discards the plaintext after sealing.
/// `atproto-pds-full.md` § Key material inventory (bridge-wide row, F1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtprotoSessionSecretBlob {
    #[serde(rename = "v")]
    pub version: u8,
    #[serde(rename = "kind")]
    pub kind: String,
    #[serde(rename = "ix")]
    pub index: AtprotoSessionSecretIndex,
    #[serde(rename = "hpke")]
    pub hpke: HpkeWire,
}

/// [`AtprotoSessionSecretBlob`] index: `(bridge_role, bridge_id)`, AAD-bound
/// via [`AadBinding::for_atproto_session_secret`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtprotoSessionSecretIndex(
    pub String, /* bridge_role */
    pub String, /* bridge_id */
);

/// Session-token secret plaintext: the raw 32-byte HMAC-SHA-256 key, zeroed
/// on drop. A CBOR map (not bare bytes) so future additive fields — e.g. a
/// rotation generation — extend it without a format break (within-major
/// additive compat, `version-compatibility.md`).
#[derive(Debug, Clone, Serialize, Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct AtprotoSessionSecretBundle {
    /// The HS256 signing secret (32 bytes).
    #[serde(with = "serde_bytes")]
    pub secret: Vec<u8>,
    #[zeroize(skip)]
    pub issued_at: u64,
}

impl AtprotoSessionSecretBlob {
    /// The `kind` tag every ATProto session-secret blob carries.
    pub const KIND: &'static str = "atproto-session-secret";

    /// Encode to canonical DAG-CBOR.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode and validate.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR, unknown version,
    /// wrong `kind`, or a wrong HPKE enc length.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        check_stamp_before_decode(bytes)?;
        let blob: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))?;
        FormatVersion(blob.version).check_supported()?;
        if blob.kind != Self::KIND {
            return Err(UnwrapError::InvalidFormat(format!(
                "expected kind={}, got {}",
                Self::KIND,
                blob.kind
            )));
        }
        if let Some(expected) = blob.hpke.kem_suite.expected_enc_len()
            && blob.hpke.enc.len() != expected
        {
            return Err(UnwrapError::InvalidFormat(format!(
                "hpke enc must be {expected} bytes for this suite, got {}",
                blob.hpke.enc.len()
            )));
        }
        Ok(blob)
    }
}

impl AtprotoSessionSecretBundle {
    /// Encode to canonical DAG-CBOR.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode from canonical CBOR.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` on bad CBOR.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))
    }
}

/// Per-domain TLS cert blob. Wrapped under a specific bridge's
/// X25519 pubkey (a deployment with N bridges has N TLS blobs per
/// domain).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsCertBlob {
    #[serde(rename = "v")]
    pub version: u8,
    #[serde(rename = "kind")]
    pub kind: String,
    #[serde(rename = "ix")]
    pub index: TlsCertIndex,
    #[serde(rename = "hpke")]
    pub hpke: HpkeWire,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsCertIndex(
    pub String, // bridge_role: "mta" or "mda"
    pub String, // bridge_id
    pub String, // domain
);

/// TLS cert + private-key bundle plaintext. priv_key zeroes on drop
/// per spec § 978; the cert chain and metadata don't.
#[derive(Debug, Clone, Serialize, Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct TlsCertBundle {
    #[zeroize(skip)]
    #[serde(with = "serde_bytes")]
    pub cert_chain: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub priv_key: Vec<u8>,
    #[zeroize(skip)]
    pub expires_at: u64,
    #[zeroize(skip)]
    pub issued_at: u64,
}

impl TlsCertBlob {
    /// Encode to canonical DAG-CBOR.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode and validate.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR, unknown
    /// version, wrong `kind`, or wrong HPKE enc length.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        check_stamp_before_decode(bytes)?;
        let blob: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))?;
        FormatVersion(blob.version).check_supported()?;
        if blob.kind != "tls-cert" {
            return Err(UnwrapError::InvalidFormat(format!(
                "expected kind=tls-cert, got {}",
                blob.kind
            )));
        }
        if let Some(expected) = blob.hpke.kem_suite.expected_enc_len()
            && blob.hpke.enc.len() != expected
        {
            return Err(UnwrapError::InvalidFormat(format!(
                "hpke enc must be {expected} bytes for this suite, got {}",
                blob.hpke.enc.len()
            )));
        }
        Ok(blob)
    }
}

impl TlsCertBundle {
    /// Encode to canonical DAG-CBOR.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode from canonical CBOR.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` on bad CBOR.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))
    }
}

/// Per-recipient inbound-mail-record envelope. The MTA bridge HPKE-Seals
/// (a) the raw RFC 5322 message bytes to the recipient's MLS pubkey and
/// (b) the canonical-token-set index hint to the recipient's index
/// pubkey; both produce one of these envelopes, opaque to nest, opened
/// later by the recipient's MDA / client with the matching X25519 secret.
///
/// The envelope carries no `ix` field — recipient-targeting is encoded
/// in the HPKE encapsulation (the encapsulated key is targeted at the
/// recipient's pubkey), and the same envelope shape serves both body
/// and index-hint kinds because the two encrypts go to different keys.
/// `kind` is always `"mail-record"`; we keep it as a discriminator so a
/// future at-rest replay reader can sanity-check the wire shape before
/// trying to open.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailRecordEnvelope {
    #[serde(rename = "v")]
    pub version: u8,
    #[serde(rename = "kind")]
    pub kind: String,
    #[serde(rename = "hpke")]
    pub hpke: HpkeWire,
}

impl MailRecordEnvelope {
    /// Encode to canonical DAG-CBOR.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode and validate.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR, unknown
    /// version, wrong `kind`, or wrong HPKE enc length.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        check_stamp_before_decode(bytes)?;
        let blob: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))?;
        FormatVersion(blob.version).check_supported()?;
        if blob.kind != "mail-record" {
            return Err(UnwrapError::InvalidFormat(format!(
                "expected kind=mail-record, got {}",
                blob.kind
            )));
        }
        if let Some(expected) = blob.hpke.kem_suite.expected_enc_len()
            && blob.hpke.enc.len() != expected
        {
            return Err(UnwrapError::InvalidFormat(format!(
                "hpke enc must be {expected} bytes for this suite, got {}",
                blob.hpke.enc.len()
            )));
        }
        Ok(blob)
    }
}

/// Bytes **proven** to be a sealed [`MailRecordEnvelope`] — the only currency
/// a content-segment ingest chokepoint accepts for record payloads (bodies and
/// index hints alike; both are sealed with the same envelope shape by every
/// producer — Go `EncryptToRecipientHybrid`, client `seal_event_body`, nest
/// `seal_recipient_blob`).
///
/// This is the structural form of the at-rest seal invariant
/// (`docs/goal/architecture/encryption-at-rest.md` § Plaintext ceiling per
/// mode): an ingest site *cannot spell* an unsealed append, because the append
/// signature demands this type and its only constructor is [`verify`], the
/// strict envelope decode. "Six ingest sites, one forgot the seal" — the shape
/// of both the `import_message`
/// and the cal/card caller-convention gap it generalized from — becomes a
/// compile error at new sites and a typed wire-edge rejection at existing
/// ones, instead of silent plaintext in a backup-eligible segment.
///
/// Deliberately NOT provided in production builds: an unchecked constructor.
/// Paths that carry bytes *already at rest* verbatim (the relay's
/// `append_sealed_record`, the S4 in-place reseal, file-level
/// compaction/restore) operate below this record-level boundary on
/// pre-encoded outer envelopes and never mint this type. The one sanctioned
/// exception is `test-helpers`-gated
/// [`carried_at_rest_unchecked`](SealedRecordBytes::carried_at_rest_unchecked),
/// for fixtures and the tier_3 raw-inject hook that seed arbitrary bytes
/// already at rest — compiled out of production builds.
///
/// [`verify`]: SealedRecordBytes::verify
#[derive(Clone)]
pub struct SealedRecordBytes(Vec<u8>);

#[cfg(feature = "test-helpers")]
impl SealedRecordBytes {
    /// Wrap `bytes` WITHOUT the sealed-envelope proof — the spelling for
    /// *simulating bytes already at rest* (arbitrary raw bytes in
    /// a unit-test fixture, or the `--features test-hooks` tier_3 raw-inject
    /// hook that seeds one through the production ingest path). NEVER wire
    /// input: an ingest site reaching for this constructor is re-opening the
    /// hole [`verify`](SealedRecordBytes::verify) exists to close. Gated
    /// behind `test-helpers` so production builds cannot spell it.
    pub fn carried_at_rest_unchecked(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl SealedRecordBytes {
    /// Prove `bytes` decode as a sealed [`MailRecordEnvelope`] (canonical
    /// DAG-CBOR, `kind == "mail-record"`, supported version, suite-consistent
    /// `enc` length) and wrap them. The same strict decode as
    /// `is_sealed_mail_record` — reliable because text formats (RFC 5322,
    /// iCalendar, vCard) and the `<u32 BE len><bytes>` token-set
    /// serialization cannot parse as the exact `{v, kind, hpke}` map.
    ///
    /// # Errors
    ///
    /// `UnwrapError::InvalidFormat` when the bytes are anything else —
    /// including the empty slice.
    pub fn verify(bytes: Vec<u8>) -> Result<Self, UnwrapError> {
        MailRecordEnvelope::from_canonical_bytes(&bytes)?;
        Ok(Self(bytes))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Always `false` — the empty slice cannot pass [`verify`]. Provided for
    /// clippy's `len_without_is_empty` only.
    ///
    /// [`verify`]: SealedRecordBytes::verify
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl AsRef<[u8]> for SealedRecordBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl core::fmt::Debug for SealedRecordBytes {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Ciphertext — length is the only useful thing to print.
        write!(f, "SealedRecordBytes({} bytes)", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_is_one() {
        assert_eq!(FormatVersion::current().0, 1);
    }

    #[test]
    fn unknown_version_rejected() {
        let err = FormatVersion(99).check_supported().unwrap_err();
        assert!(matches!(err, UnwrapError::UnsupportedVersion(99)));
    }

    /// **The `ladder` ground at the blob** (`transport.md` § Schema and
    /// forward-compat discipline → *Rule 3 in full*): a later version's blob
    /// whose body this build cannot even decode — here a KDF selector no
    /// `SerKdfParams` variant names — reports the typed version refusal, not
    /// a CBOR error, because the stamp is read before the strict decode. The
    /// same body under THIS version is the malformed input it looks like.
    #[test]
    fn a_later_versions_blob_reads_as_unsupported_not_corrupt() {
        #[derive(Serialize)]
        struct LaterKdf {
            alg: &'static str,
            n: u32,
        }
        #[derive(Serialize)]
        struct LaterMsekBlob {
            v: u8,
            kind: &'static str,
            ix: WrappedMsekIndex,
            kdf: LaterKdf,
            salt: ByteBuf,
            nonce: ByteBuf,
            ct: ByteBuf,
        }
        let blob = |v: u8| {
            fauna_cbor::encode_canonical(&LaterMsekBlob {
                v,
                kind: "wrapped-msek",
                ix: WrappedMsekIndex(vec![0x01u8; 32], "cred-1".into()),
                kdf: LaterKdf {
                    alg: "scrypt",
                    n: 1 << 15,
                },
                salt: ByteBuf::from(vec![0xAAu8; 16]),
                nonce: ByteBuf::from(vec![0xBBu8; 12]),
                ct: ByteBuf::from(vec![0xCCu8; 48]),
            })
            .unwrap()
        };
        let later = BLOB_FORMAT_VERSION + 1;
        assert!(matches!(
            WrappedMsekBlob::from_canonical_bytes(&blob(later)).unwrap_err(),
            UnwrapError::UnsupportedVersion(v) if v == later
        ));
        assert!(matches!(
            WrappedMsekBlob::from_canonical_bytes(&blob(BLOB_FORMAT_VERSION)).unwrap_err(),
            UnwrapError::InvalidFormat(_)
        ));
        // Bytes with no readable stamp are still plain malformed input.
        assert!(matches!(check_stamp_before_decode(b"junk"), Ok(())));
        assert!(matches!(
            WrappedMsekBlob::from_canonical_bytes(b"junk").unwrap_err(),
            UnwrapError::InvalidFormat(_)
        ));
    }

    #[test]
    fn aad_for_different_kinds_differs() {
        let actor = [0u8; 32];
        let msek_aad = AadBinding::for_wrapped_msek(&actor, "x").canonical_bytes();
        let snap_aad = AadBinding::for_mls_snapshot(&actor).canonical_bytes();
        let tok_aad = AadBinding::for_submission_token(&actor, "x").canonical_bytes();
        assert_ne!(msek_aad, snap_aad);
        assert_ne!(msek_aad, tok_aad);
        assert_ne!(snap_aad, tok_aad);
    }

    #[test]
    fn aad_for_seed_escrow_is_separated_from_the_identically_indexed_kinds() {
        // `seed-escrow`, `mls-snapshot` and `webdav-keys` are all indexed by
        // `(actor_id,)` alone, so the kind tag is the ONLY thing separating
        // their AAD. The escrow plaintext is the identity seed itself
        // (`identity-succession.md` § Seed escrow), so a collision here would
        // let a wrong-shape decrypt site open the most valuable secret in the
        // system — pin the separation rather than trust the tag string.
        let actor = [0u8; 32];
        let escrow = AadBinding::for_seed_escrow(&actor).canonical_bytes();
        let snapshot = AadBinding::for_mls_snapshot(&actor).canonical_bytes();
        let webdav = AadBinding::for_webdav_keys(&actor).canonical_bytes();
        assert_ne!(escrow, snapshot);
        assert_ne!(escrow, webdav);

        // And it is actor-scoped: a different actor's escrow AAD differs, so an
        // escrow blob cannot be replayed across accounts.
        let other = AadBinding::for_seed_escrow(&[1u8; 32]).canonical_bytes();
        assert_ne!(escrow, other);
    }

    #[test]
    fn aad_for_tls_cert_distinguishes_role_id_and_domain() {
        let a = AadBinding::for_tls_cert("mta", "b1", "ex.com").canonical_bytes();
        let b = AadBinding::for_tls_cert("mda", "b1", "ex.com").canonical_bytes();
        let c = AadBinding::for_tls_cert("mta", "b2", "ex.com").canonical_bytes();
        let d = AadBinding::for_tls_cert("mta", "b1", "other.com").canonical_bytes();
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    /// Golden byte-stability lock (CBOR Layer 6, Domain L): the
    /// canonical-dag-cbor AAD bytes are bound into every wrapped-blob
    /// AEAD, so they MUST stay byte-identical (a) across the
    /// ciborium→fauna_cbor encoder migration and (b) for cross-language
    /// (Go `internal/dagcbor` / Swift) seal/unseal parity. Map keys in
    /// canonical (length-first-then-bytewise) order: v, ix, kind.
    #[test]
    fn aad_wrapped_msek_golden_bytes() {
        let actor = [0u8; 32];
        let got = AadBinding::for_wrapped_msek(&actor, "x").canonical_bytes();
        let mut want = vec![0xA3, 0x61, 0x76, 0x01, 0x62, 0x69, 0x78, 0x82, 0x58, 0x20];
        want.extend_from_slice(&[0u8; 32]); // actor_id bstr(32)
        want.extend_from_slice(&[0x61, 0x78]); // "x" (credential_id)
        want.extend_from_slice(&[0x64, 0x6b, 0x69, 0x6e, 0x64]); // "kind"
        want.push(0x6c); // text(12)
        want.extend_from_slice(b"wrapped-msek");
        assert_eq!(got, want, "AAD canonical bytes drifted");
    }

    #[test]
    fn wrapped_msek_blob_roundtrips_through_cbor() {
        let blob = WrappedMsekBlob {
            version: 1,
            kind: "wrapped-msek".into(),
            index: WrappedMsekIndex(vec![0x01u8; 32], "cred-1".into()),
            kdf: SerKdfParams::Argon2id {
                argon_version: ARGON2_VERSION_13,
                m: 65_536,
                t: 2,
                p: 1,
            },
            salt: ByteBuf::from(vec![0xAAu8; 16]),
            nonce: ByteBuf::from(vec![0xBBu8; 12]),
            ciphertext: ByteBuf::from(vec![0xCCu8; 48]),
        };
        let bytes = blob.to_canonical_bytes().unwrap();
        let decoded = WrappedMsekBlob::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.version, blob.version);
        assert_eq!(decoded.kind, blob.kind);
        assert_eq!(decoded.index.0, blob.index.0);
        assert_eq!(decoded.index.1, blob.index.1);
        assert_eq!(decoded.salt.as_ref(), blob.salt.as_ref());
        assert_eq!(decoded.nonce.as_ref(), blob.nonce.as_ref());
        assert_eq!(decoded.ciphertext.as_ref(), blob.ciphertext.as_ref());
    }

    #[test]
    fn decode_rejects_wrong_kind() {
        let blob = WrappedMsekBlob {
            version: 1,
            kind: "mls-snapshot".into(), // wrong
            index: WrappedMsekIndex(vec![0u8; 32], "x".into()),
            kdf: SerKdfParams::HkdfSha256 {},
            salt: ByteBuf::from(vec![0u8; 16]),
            nonce: ByteBuf::from(vec![0u8; 12]),
            ciphertext: ByteBuf::from(vec![0u8; 48]),
        };
        let bytes = blob.to_canonical_bytes().unwrap();
        let err = WrappedMsekBlob::from_canonical_bytes(&bytes).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    #[test]
    fn decode_rejects_bad_lengths() {
        let blob = WrappedMsekBlob {
            version: 1,
            kind: "wrapped-msek".into(),
            index: WrappedMsekIndex(vec![0u8; 16], "x".into()), // wrong actor_id len
            kdf: SerKdfParams::HkdfSha256 {},
            salt: ByteBuf::from(vec![0u8; 16]),
            nonce: ByteBuf::from(vec![0u8; 12]),
            ciphertext: ByteBuf::from(vec![0u8; 48]),
        };
        let bytes = blob.to_canonical_bytes().unwrap();
        let err = WrappedMsekBlob::from_canonical_bytes(&bytes).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    #[test]
    fn mls_snapshot_blob_roundtrips_through_cbor() {
        let blob = MlsSnapshotBlob {
            version: 1,
            kind: "mls-snapshot".into(),
            index: MlsSnapshotIndex(vec![0x77u8; 32]),
            nonce: ByteBuf::from(vec![0xCCu8; 12]),
            ciphertext: ByteBuf::from(vec![0xDDu8; 64]),
        };
        let bytes = blob.to_canonical_bytes().unwrap();
        let decoded = MlsSnapshotBlob::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.version, blob.version);
        assert_eq!(decoded.kind, blob.kind);
        assert_eq!(decoded.index.0, blob.index.0);
        assert_eq!(decoded.nonce.as_ref(), blob.nonce.as_ref());
        assert_eq!(decoded.ciphertext.as_ref(), blob.ciphertext.as_ref());
    }

    #[test]
    fn mls_snapshot_decode_rejects_wrong_kind() {
        let blob = MlsSnapshotBlob {
            version: 1,
            kind: "wrapped-msek".into(), // wrong
            index: MlsSnapshotIndex(vec![0u8; 32]),
            nonce: ByteBuf::from(vec![0u8; 12]),
            ciphertext: ByteBuf::from(vec![0u8; 64]),
        };
        let bytes = blob.to_canonical_bytes().unwrap();
        let err = MlsSnapshotBlob::from_canonical_bytes(&bytes).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    /// Regression: MlsSnapshotBlob's `ix` field MUST encode as a
    /// 1-element CBOR array (per spec § Wire format and CDDL),
    /// NOT as a transparent newtype around the bytes.
    ///
    /// Decode the canonical bytes and inspect the `ix` value's CBOR
    /// shape: it must be `Array([Bytes(...)])`, not `Bytes(...)`.
    #[test]
    fn mls_snapshot_ix_encodes_as_one_element_array() {
        let blob = MlsSnapshotBlob {
            version: 1,
            kind: "mls-snapshot".into(),
            index: MlsSnapshotIndex(vec![0x77u8; 32]),
            nonce: ByteBuf::from(vec![0u8; 12]),
            ciphertext: ByteBuf::from(vec![0u8; 16]),
        };
        let bytes = blob.to_canonical_bytes().unwrap();
        // Decode as a generic dag-cbor Value and pull out the "ix" field.
        let val: fauna_cbor::Value =
            fauna_cbor::decode_strict(&bytes).expect("must round-trip-decode");
        let fauna_cbor::Value::Map(entries) = val else {
            panic!("blob root must be a CBOR map");
        };
        let ix_value = entries.get("ix").expect("ix field must be present");
        match ix_value {
            fauna_cbor::Value::List(elements) => {
                assert_eq!(elements.len(), 1, "ix must have exactly 1 element");
                match &elements[0] {
                    fauna_cbor::Value::Bytes(actor) => {
                        assert_eq!(actor.len(), 32, "actor_id must be 32 bytes");
                    }
                    other => panic!("ix[0] must be a bstr, got {other:?}"),
                }
            }
            fauna_cbor::Value::Bytes(_) => {
                panic!(
                    "ix must be List([Bytes]) not Bytes — single-field tuple struct serialization regression!"
                );
            }
            other => panic!("ix must be a CBOR list, got {other:?}"),
        }
    }

    #[test]
    fn mls_snapshot_decode_rejects_bad_lengths() {
        // Wrong actor_id length.
        let blob = MlsSnapshotBlob {
            version: 1,
            kind: "mls-snapshot".into(),
            index: MlsSnapshotIndex(vec![0u8; 16]), // wrong, must be 32
            nonce: ByteBuf::from(vec![0u8; 12]),
            ciphertext: ByteBuf::from(vec![0u8; 16]),
        };
        let bytes = blob.to_canonical_bytes().unwrap();
        let err = MlsSnapshotBlob::from_canonical_bytes(&bytes).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));

        // Wrong nonce length.
        let blob = MlsSnapshotBlob {
            version: 1,
            kind: "mls-snapshot".into(),
            index: MlsSnapshotIndex(vec![0u8; 32]),
            nonce: ByteBuf::from(vec![0u8; 8]), // wrong, must be 12
            ciphertext: ByteBuf::from(vec![0u8; 16]),
        };
        let bytes = blob.to_canonical_bytes().unwrap();
        let err = MlsSnapshotBlob::from_canonical_bytes(&bytes).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    #[test]
    fn wrapped_submission_token_blob_roundtrips_through_cbor() {
        let blob = WrappedSubmissionTokenBlob {
            version: 1,
            kind: "submission-token".into(),
            index: WrappedMsekIndex(vec![0x12u8; 32], "cred-1".into()),
            kdf: SerKdfParams::HkdfSha256 {},
            salt: ByteBuf::from(vec![0x34u8; 16]),
            nonce: ByteBuf::from(vec![0x56u8; 12]),
            ciphertext: ByteBuf::from(vec![0x78u8; 200]),
        };
        let bytes = blob.to_canonical_bytes().unwrap();
        let decoded = WrappedSubmissionTokenBlob::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.version, blob.version);
        assert_eq!(decoded.kind, blob.kind);
        assert_eq!(decoded.index.0, blob.index.0);
        assert_eq!(decoded.index.1, blob.index.1);
    }

    #[test]
    fn tls_cert_blob_roundtrips_through_cbor() {
        let blob = TlsCertBlob {
            version: 1,
            kind: "tls-cert".into(),
            index: TlsCertIndex("mta".into(), "bridge-1".into(), "example.com".into()),
            hpke: HpkeWire {
                kem_suite: KemSuite::STANDARD,
                enc: ByteBuf::from(vec![0u8; 32]),
                ciphertext: ByteBuf::from(vec![0u8; 64]),
            },
        };
        let bytes = blob.to_canonical_bytes().unwrap();
        let decoded = TlsCertBlob::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.kind, blob.kind);
        assert_eq!(decoded.index.0, blob.index.0);
        assert_eq!(decoded.index.1, blob.index.1);
        assert_eq!(decoded.index.2, blob.index.2);
    }

    #[test]
    fn kem_suite_is_standard_discriminates() {
        assert!(KemSuite::STANDARD.is_standard());
        // The X-Wing hybrid id is NOT classical — the unseal dispatcher must
        // route it away from today's X25519 path.
        let xwing = KemSuite {
            kem: FAUNA_KEM_XWING,
            kdf: 0x0001,
            aead: 0x0003,
        };
        assert!(!xwing.is_standard());
        // An arbitrary unknown suite is also non-classical.
        let unknown = KemSuite {
            kem: 0x0099,
            kdf: 0x0001,
            aead: 0x0003,
        };
        assert!(!unknown.is_standard());
        assert_ne!(FAUNA_KEM_XWING, KemSuite::STANDARD.kem);
    }
}
