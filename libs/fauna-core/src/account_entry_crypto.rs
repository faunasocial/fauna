//! The **sealed class-2 entry** of the account-data plane — the form T14 froze
//! (`docs/goal/architecture/account-data-plane.md` § The class-2 entry form,
//! frozen 2026-08-10; key schedule owner:
//! `docs/goal/architecture/owner-key-material.md` § Path A-sibling-2).
//!
//! A class-2 item is *mutable account state* — a settings key, a relationship,
//! a read marker, a seen-set entry, a device endpoint. This module is how one
//! writer's version of such an item rests on every replica's sealed plane and
//! travels on the wire to every peer, **uniformly**: reading replica, nest and
//! key-less custodian all hold the identical bytes, so posture never appears on
//! the wire (R7 (account-data-plane.md § The ratified decisions)) and a custodian can hold and relay every item with zero read
//! reach.
//!
//! # The form
//!
//! ```text
//! v1: [form version: 1 byte] [nonce: 12 bytes] [ChaCha20-Poly1305 ciphertext+tag]
//! v2: [form version: 1 byte] [generation id: 32 bytes, cleartext] [nonce] [ciphertext+tag]
//! ```
//!
//! sealed under `entry_key(kind)` ([`crate::crypto::AccountStateKeySchedule`]),
//! with **AAD = the canonical dag-cbor encoding of the entry's plane
//! coordinates** `{form_version, writer_id, writer_seq, scope, item_key}` —
//! v2 (the generation-sealed form, R14) additionally binds the cleartext
//! generation id ([`SEALED_ENTRY_V2`]).
//!
//! The AAD binding is what makes relay trust unnecessary for integrity, and it
//! is worth being precise about *which* attacks it closes, because the plane
//! deliberately relies on it instead of on honest relays:
//!
//! - **Splice** — no relay, the nest included, can move one entry's ciphertext
//!   under another entry's coordinates: the coordinates are in the tag.
//! - **Replay-as-newer** — an old ciphertext cannot pose as a newer row,
//!   because its `writer_seq` is bound in.
//! - **Forged tombstones** — a deletion is an ordinary sealed entry whose
//!   payload carries the tombstone marker, so a relay cannot fabricate one that
//!   opens. (The journal row's *cleartext* `op = tombstone` exists only so
//!   key-less custodians can apply tombstone retention.)
//!
//! What it deliberately does **not** close: fabricating non-class-2 journal
//! rows (a fake `record-added`) is admission-plane territory — the W8 (account-data-plane.md § Workstreams) gate's
//! hardening, not this form.
//!
//! # The in-seal writer signature (R13)
//!
//! The AEAD key is **symmetric**, so everything above protects readers against
//! *relays* and nothing against *key holders*: any position able to open a kind
//! is equally able to seal one, and a capability grant would therefore convey
//! write authority along with read. The sealed payload closes that by carrying
//! the writing device's Ed25519 signature over the entry's canonical payload
//! bytes and its AAD coordinates, verified on every open ([`open_entry`]).
//!
//! - **Separates read capability from write capability**, which is what makes
//!   delegable-rung granting safe to be generous with.
//! - **Durable attribution**: an entry's author is provable long after the row
//!   left the writing device.
//! - **No per-kind opt-out.** A discretionary "signed?" flag would re-create the
//!   classification hazard the audience ladder exists to kill; a kind whose write
//!   rate makes signatures matter is mis-homed on this plane.
//!
//! The signer is the entry's own `writer_id` — the device principal's public key,
//! already bound into the AAD — so there is deliberately **no separate signer
//! field**: no second copy to disagree with the first, and key substitution is
//! unrepresentable rather than merely checked.
//!
//! What this layer verifies is that the *stated writer* signed these exact bytes
//! at these exact coordinates. Whether that writer is a device the account ever
//! authorized is the **authoring-chain** check — a `DeviceAuthorization` covering
//! the device key, root-signed by the ActorId, with succession-crossing
//! verification (`../behavior/devices.md` § Device-signed authoring). That check
//! needs the account's device registry, which this plane does not yet carry; it
//! is named as a distinct layer above the walk rather than smuggled in here,
//! because a signature check that silently answered a *different* question would
//! be worse than an absent one.
//!
//! # Nonce mode
//!
//! Random, never derived, and that is mandatory rather than incidental: an
//! entry's *value* is mutable under a fixed item key, which is exactly the case
//! the shipped sealed-label law reserves `seal_random` for
//! ([`crate::path_crypto`] module docs). A salt-derived nonce here would reuse
//! a (key, nonce) pair across differing plaintexts — the classic AEAD
//! catastrophe.
//!
//! # Evolution
//!
//! This is both a wire form and an at-rest form, so changing it after W2 ships
//! is a migration. Evolution is additive-everywhere along the leading
//! form-version byte (`version-compatibility.md`); nothing may add fields to
//! the AAD or the sealed payload outside that axis.

use crate::crypto::AccountStateKindKeys;
use crate::encoding::{canonical_decode, canonical_encode};
use anyhow::{Result, bail};
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
};
// Only the SIGNING half is needed here: verification goes through the shared
// strict primitive `crate::identity::verify_detached`, never a locally built
// `VerifyingKey` + permissive `verify`.
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

/// The only entry-form version this build writes.
///
/// Still `1` after the R13 signature amendment, deliberately: the amendment
/// landed while the form had **no consumer** (nothing seals production entries),
/// so the signature is birth-shape for generation 0 rather than a migration.
/// Bumping here would imply an unsigned v1 exists in the world; none does.
pub const SEALED_ENTRY_V1: u8 = 1;

/// The **generation-sealed** entry form (R14 build step 2 —
/// `account-data-plane.md` § The class-2 entry form → *Form v2*).
///
/// Identical to v1 except the envelope carries the sealing generation's
/// 32-byte content-derived id **in cleartext** between the version byte and
/// the nonce, bound into the AAD. Cleartext deliberately (charter § Replica
/// posture, the custody-floor amendment): the id is what makes a reader's
/// key choice a lookup instead of a trial walk across every retained
/// generation — the accepted floor cost is that a custodian sees the
/// account's rotation cadence.
///
/// **v1 stays the gen-0 form forever**: this build writes v2 only when
/// sealing under a generation ≥ 1 (no production writer does until the
/// schedule's mint step lands; the writer-door gate enforces that ordering).
pub const SEALED_ENTRY_V2: u8 = 2;

/// Length of the v2 envelope's cleartext generation id.
const GENERATION_ID_LEN: usize = 32;

/// Domain-separation tag for the in-seal writer signature.
///
/// Follows the codebase convention (`lan_cert.rs`'s `LAN_CERT_SIG_CONTEXT`,
/// `key-material-hierarchy.md` § Architectural rules #8):
/// `b"fauna.<context>.v<n>\0"`, the trailing NUL making the tag self-delimiting
/// so no tag is a prefix of another and it cannot blend into the bytes after it.
/// Without it, a device signature made here could in principle be replayed into
/// another context that signs bare canonical bytes under the same device key.
const ENTRY_WRITER_SIG_CONTEXT: &[u8] = b"fauna.account-state.entry.writer-sig.v1\0";

/// Nonce length for ChaCha20-Poly1305.
const NONCE_LEN: usize = 12;

/// Poly1305 tag length.
const TAG_LEN: usize = 16;

/// The entry's **plane coordinates** minus the item key — the cleartext floor a
/// key-less custodian legitimately sees, and (with the item key and the form
/// version) exactly what the AEAD binds.
///
/// Deliberately *not* an owned struct with an item-key field: on the seal side
/// the item key is **derived**, never supplied, so a caller cannot produce an
/// entry whose outer routing key disagrees with its sealed payload. See
/// [`seal_entry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryCoordinates<'a> {
    /// The authoring writer — a device principal's public key, or the nest's.
    pub writer_id: [u8; 32],
    /// The authoring writer's log sequence number for this entry. Gapless in
    /// the writer's own journal only: on the feed it is spent across scopes
    /// and collapsed per item, so a gap there is legal and does not reveal a
    /// withheld row (`account-sync-plane.md` § The class-2 entry form).
    pub writer_seq: u64,
    /// The scope whose feed carries this entry (the account-state scope, in the
    /// charter's partition).
    pub scope: &'a str,
}

/// The sealed payload: what only a reading replica ever sees.
///
/// Canonical dag-cbor inside the envelope. Everything here — the true kind and
/// logical key, the merge metadata, the value, the tombstone marker — is
/// invisible to every key-less position, which sees only the custody floor
/// (scope, writer id/seq, op discriminator, blinded item key, size, timing).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryPlaintext {
    /// The true kind. Cross-checked on open against the keys the entry opened
    /// under — see [`open_entry`].
    pub kind: String,
    /// The true logical key. Cross-checked on open by recomputing the blind.
    pub key: String,
    /// The kind's merge-policy metadata — an LWW stamp, per-field versions, or
    /// a CAS base echo (`account-data-plane.md` § Merge-policy seam). Opaque
    /// bytes at this layer: the merge seam interprets them at reading
    /// replicas, this module only carries them inside the seal.
    pub merge_meta: Option<ByteBuf>,
    /// The opaque canonical value bytes.
    pub value: ByteBuf,
    /// The tombstone marker. A class-2 deletion is an ordinary sealed entry
    /// with this set — never a bare cleartext row.
    pub tombstone: bool,
}

/// A sealed entry together with the routing key it must be filed under.
///
/// Returned as a pair on purpose: the two are derived together and a caller
/// that could pick them independently could file an entry under a key that does
/// not match its contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedEntry {
    /// The blinded item key — the opaque 32-byte routing key, dropping into the
    /// feed row's existing routing-key slot.
    pub item_key: [u8; 32],
    /// The envelope bytes: v1 `[form version][nonce][ciphertext+tag]`; v2
    /// `[form version][generation id (32, cleartext)][nonce][ciphertext+tag]`.
    pub envelope: Vec<u8>,
}

/// The AAD struct — canonical dag-cbor of the entry's five plane coordinates.
///
/// A struct rather than a hand-rolled concatenation because the charter
/// specifies "the canonical encoding of the entry's plane coordinates", and
/// canonical dag-cbor is this codebase's one canonical encoding: it is
/// length-delimited and field-tagged, so no two distinct coordinate tuples can
/// encode to the same bytes (a concatenation of a variable-length `scope` with
/// fixed fields would need its own length discipline to promise that).
#[derive(Serialize)]
struct EntryAad<'a> {
    form_version: u8,
    #[serde(with = "serde_bytes")]
    item_key: &'a [u8],
    scope: &'a str,
    #[serde(with = "serde_bytes")]
    writer_id: &'a [u8],
    writer_seq: u64,
}

fn entry_aad(coords: &EntryCoordinates<'_>, item_key: &[u8; 32]) -> Result<Vec<u8>> {
    canonical_encode(&EntryAad {
        form_version: SEALED_ENTRY_V1,
        item_key,
        scope: coords.scope,
        writer_id: &coords.writer_id,
        writer_seq: coords.writer_seq,
    })
    .map_err(Into::into)
}

/// The v2 AAD — the v1 coordinates plus the sealing generation's id. A
/// separate struct rather than an optional field: the two forms' AADs must
/// never be able to encode to the same bytes, and distinct field sets under
/// distinct `form_version` values make that structural.
#[derive(Serialize)]
struct EntryAadV2<'a> {
    form_version: u8,
    #[serde(with = "serde_bytes")]
    generation_id: &'a [u8],
    #[serde(with = "serde_bytes")]
    item_key: &'a [u8],
    scope: &'a str,
    #[serde(with = "serde_bytes")]
    writer_id: &'a [u8],
    writer_seq: u64,
}

fn entry_aad_v2(
    coords: &EntryCoordinates<'_>,
    item_key: &[u8; 32],
    generation_id: &[u8; 32],
) -> Result<Vec<u8>> {
    canonical_encode(&EntryAadV2 {
        form_version: SEALED_ENTRY_V2,
        generation_id,
        item_key,
        scope: coords.scope,
        writer_id: &coords.writer_id,
        writer_seq: coords.writer_seq,
    })
    .map_err(Into::into)
}

/// Whether `envelope` has the **v1** (gen-0) form's shape: its version byte,
/// and room for the nonce and the tag. The cleartext floor a key-less position
/// can check — a nest door admitting a third-party principal's row, whose
/// `ext.*` kinds are gen-0 by construction (`third-party-kinds.md` § The kinds
/// vocabulary) — and nothing more: it says nothing about whether the entry
/// opens, which only a key holder can learn.
pub fn has_v1_envelope_shape(envelope: &[u8]) -> bool {
    envelope.first() == Some(&SEALED_ENTRY_V1) && envelope.len() >= 1 + NONCE_LEN + TAG_LEN
}

/// The cleartext generation id of a **v2** envelope, `None` for v1 (a gen-0
/// entry has no generation id) or for bytes too short to carry one.
///
/// Advisory routing only — the reader uses it to pick which retained
/// generation's key schedule to open under (a lookup, not a trial walk);
/// the AEAD tag remains the arbiter, and a lying id simply fails the open.
pub fn peek_generation_id(envelope: &[u8]) -> Option<[u8; 32]> {
    if envelope.first() != Some(&SEALED_ENTRY_V2) || envelope.len() < 1 + GENERATION_ID_LEN {
        return None;
    }
    envelope[1..1 + GENERATION_ID_LEN].try_into().ok()
}

/// What the AEAD actually encrypts: the canonical [`EntryPlaintext`] bytes plus
/// the writing device's signature over them (R13).
///
/// Two levels rather than a `signature` field on [`EntryPlaintext`], because a
/// signature *inside* the bytes it signs is circular. The inner bytes are kept
/// verbatim (not re-encoded from a decoded struct) so a verifier signs and checks
/// the identical byte string — canonical encoding makes that redundant, and
/// carrying the bytes makes it true by construction rather than by assumption.
#[derive(Serialize, Deserialize)]
struct SignedEntryPayload {
    /// Canonical dag-cbor of the [`EntryPlaintext`].
    payload: ByteBuf,
    /// Ed25519 signature over [`writer_sig_message`], by the entry's
    /// `writer_id`. Fixed 64 bytes; length-checked on open.
    signature: ByteBuf,
}

/// The exact bytes the writer signs: `tag ‖ aad ‖ payload`.
///
/// Concatenation is unambiguous here without extra length discipline, because
/// the tag is a fixed NUL-terminated constant and both `aad` and `payload` are
/// complete canonical dag-cbor values — self-delimiting by construction, so no
/// two distinct `(aad, payload)` pairs produce the same message.
fn writer_sig_message(aad: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(ENTRY_WRITER_SIG_CONTEXT.len() + aad.len() + payload.len());
    m.extend_from_slice(ENTRY_WRITER_SIG_CONTEXT);
    m.extend_from_slice(aad);
    m.extend_from_slice(payload);
    m
}

/// Seal one class-2 entry, deriving its item key from the payload's own logical
/// key so the outer routing key and the sealed contents cannot disagree.
///
/// `writer` is the authoring **device** signing key (R13). Its public half must
/// be the `coords.writer_id` the entry is filed under: the signature and the
/// coordinates name one writer, so there is nothing for a verifier to reconcile.
///
/// # Errors
/// A kind/key mismatch, a `writer` whose public half is not `coords.writer_id`,
/// canonical-encoding failure, or an AEAD failure (unreachable for the shapes we
/// seal, but never swallowed).
pub fn seal_entry(
    keys: &AccountStateKindKeys,
    coords: &EntryCoordinates<'_>,
    plaintext: &EntryPlaintext,
    writer: &SigningKey,
) -> Result<SealedEntry> {
    if plaintext.kind != keys.kind() {
        bail!(
            "cannot seal a `{}` entry under `{}` keys — the sealed kind and the key's kind must agree",
            plaintext.kind,
            keys.kind()
        );
    }
    if writer.verifying_key().to_bytes() != coords.writer_id {
        bail!(
            "refusing to seal: the signing device is not the entry's writer_id — a signature \
             naming a different writer than the coordinates would fail on every reader"
        );
    }
    let item_key = keys.item_key(plaintext.key.as_bytes());
    let envelope = seal_with_item_key(keys, coords, &item_key, None, plaintext, writer)?;
    Ok(SealedEntry { item_key, envelope })
}

/// Seal one class-2 entry in the **v2 generation-sealed form**
/// ([`SEALED_ENTRY_V2`]): same contract as [`seal_entry`], plus the sealing
/// generation's 32-byte id, carried cleartext in the envelope and bound into
/// the AAD.
///
/// `keys` must be the **per-generation** kind keys — the schedule derived
/// from that generation's key under the fleet-only context pair
/// (`owner-key-material.md` § The schedule build design; the derivation lands
/// with the mint step). Sealing a v2 entry under gen-0 keys would be a
/// category error this function cannot detect — the mint machinery is what
/// hands out matched (id, schedule) pairs, and the key↔id commitment is
/// verified where generation keys are unwrapped, never here.
pub fn seal_entry_v2(
    keys: &AccountStateKindKeys,
    coords: &EntryCoordinates<'_>,
    generation_id: &[u8; 32],
    plaintext: &EntryPlaintext,
    writer: &SigningKey,
) -> Result<SealedEntry> {
    if plaintext.kind != keys.kind() {
        bail!(
            "cannot seal a `{}` entry under `{}` keys — the sealed kind and the key's kind must agree",
            plaintext.kind,
            keys.kind()
        );
    }
    if writer.verifying_key().to_bytes() != coords.writer_id {
        bail!(
            "refusing to seal: the signing device is not the entry's writer_id — a signature \
             naming a different writer than the coordinates would fail on every reader"
        );
    }
    let item_key = keys.item_key(plaintext.key.as_bytes());
    let envelope = seal_with_item_key(
        keys,
        coords,
        &item_key,
        Some(generation_id),
        plaintext,
        writer,
    )?;
    Ok(SealedEntry { item_key, envelope })
}

/// The exact length of the envelope [`seal_entry`] — or, when
/// `generation_sealed`, [`seal_entry_v2`] — produces for `plaintext`, computed
/// from the encodings alone: no key, no signature, no AEAD.
///
/// The frame is fixed (the form byte, the v2 generation id, the nonce, the
/// Poly1305 tag) and the writer signature is always
/// [`ed25519_dalek::SIGNATURE_LENGTH`] bytes, so the one variable is the
/// canonical encoding of the signed payload — which depends on the
/// plaintext's bytes and on nothing the seal draws at random. That is what
/// lets a writer refuse an entry a nest's per-entry cap would refuse *before*
/// the entry is assigned a writer seq.
///
/// # Errors
/// Canonical-encoding failure — the one the seal itself would hit.
pub fn sealed_envelope_len(plaintext: &EntryPlaintext, generation_sealed: bool) -> Result<usize> {
    let inner = canonical_encode(plaintext)?;
    let payload = canonical_encode(&SignedEntryPayload {
        payload: ByteBuf::from(inner),
        signature: ByteBuf::from(vec![0u8; ed25519_dalek::SIGNATURE_LENGTH]),
    })?;
    let header_len = if generation_sealed {
        1 + GENERATION_ID_LEN
    } else {
        1
    };
    Ok(header_len + NONCE_LEN + payload.len() + TAG_LEN)
}

/// The raw seal, with the item key supplied rather than derived.
///
/// Private: the only production caller is [`seal_entry`], which derives the key
/// from the payload. Tests use it to forge the mismatch [`open_entry`]'s
/// cross-check exists to catch — an attack a *holder of this kind's grant*
/// could otherwise mount, since it needs no key the attacker lacks.
fn seal_with_item_key(
    keys: &AccountStateKindKeys,
    coords: &EntryCoordinates<'_>,
    item_key: &[u8; 32],
    generation_id: Option<&[u8; 32]>,
    plaintext: &EntryPlaintext,
    writer: &SigningKey,
) -> Result<Vec<u8>> {
    let inner = canonical_encode(plaintext)?;
    let aad = match generation_id {
        None => entry_aad(coords, item_key)?,
        Some(generation) => entry_aad_v2(coords, item_key, generation)?,
    };
    let signature = writer.sign(&writer_sig_message(&aad, &inner));
    let payload = canonical_encode(&SignedEntryPayload {
        payload: ByteBuf::from(inner),
        signature: ByteBuf::from(signature.to_bytes().to_vec()),
    })?;
    let cipher = ChaCha20Poly1305::new_from_slice(keys.entry_key()).expect("32-byte key is valid");
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: &payload,
                aad: &aad,
            },
        )
        .map_err(|e| anyhow::anyhow!("account-state entry sealing failed: {e}"))?;

    let header_len = match generation_id {
        None => 1,
        Some(_) => 1 + GENERATION_ID_LEN,
    };
    let mut envelope = Vec::with_capacity(header_len + NONCE_LEN + ct.len());
    match generation_id {
        None => envelope.push(SEALED_ENTRY_V1),
        Some(generation) => {
            envelope.push(SEALED_ENTRY_V2);
            envelope.extend_from_slice(generation);
        }
    }
    envelope.extend_from_slice(nonce.as_slice());
    envelope.extend_from_slice(&ct);
    Ok(envelope)
}

/// Open a sealed entry at its stated coordinates, fail-closed.
///
/// `item_key` and `coords` both come from the feed/journal row carrying the
/// entry; the AEAD tag is what proves the relay did not alter either.
///
/// Three checks run *after* a successful decrypt, as defense in depth over the
/// AAD — they catch a malicious **sealer** holding this kind's grant, which the
/// AAD alone cannot, since such a sealer can compute any AAD it likes:
///
/// 1. the writer signature (R13) must verify under `coords.writer_id` over these
///    exact payload bytes at these exact coordinates,
/// 2. the payload's kind must equal the kind these keys are for, and
/// 3. recomputing the blind over the payload's logical key must reproduce the
///    outer item key.
///
/// Check 1 is what separates read capability from write capability: without it a
/// grant for kind K conveys the power to *author* K, since the AEAD key is
/// symmetric. Checks 2–3 stop a grant for K minting an entry whose payload claims
/// kind K′, which a reader would otherwise merge as K′ state it never granted.
///
/// **Not checked here:** whether `coords.writer_id` is a device the account
/// authorized. That is the authoring-chain question (module docs § The in-seal
/// writer signature) and it needs the device registry; this function answers only
/// "the stated writer signed exactly this".
///
/// # Errors
/// Unknown form version, a truncated envelope, an AEAD failure (wrong key,
/// wrong coordinates, or tampering), a non-canonical payload, a malformed or
/// failing signature, or either cross-check failing.
pub fn open_entry(
    keys: &AccountStateKindKeys,
    coords: &EntryCoordinates<'_>,
    item_key: &[u8; 32],
    envelope: &[u8],
) -> Result<EntryPlaintext> {
    const MIN_LEN: usize = 1 + NONCE_LEN + TAG_LEN;
    if envelope.len() < MIN_LEN {
        bail!(
            "sealed account-state entry too short: {} bytes (minimum {MIN_LEN})",
            envelope.len()
        );
    }
    // Per-form header parse: the AAD and the body offset are the only things
    // the forms disagree about — everything after the decrypt is shared.
    let (aad, body) = match envelope[0] {
        SEALED_ENTRY_V1 => (entry_aad(coords, item_key)?, &envelope[1..]),
        SEALED_ENTRY_V2 => {
            const MIN_LEN_V2: usize = 1 + GENERATION_ID_LEN + NONCE_LEN + TAG_LEN;
            if envelope.len() < MIN_LEN_V2 {
                bail!(
                    "sealed account-state v2 entry too short: {} bytes (minimum {MIN_LEN_V2})",
                    envelope.len()
                );
            }
            let generation_id: [u8; GENERATION_ID_LEN] = envelope[1..1 + GENERATION_ID_LEN]
                .try_into()
                .expect("length guaranteed by the MIN_LEN_V2 check");
            (
                entry_aad_v2(coords, item_key, &generation_id)?,
                &envelope[1 + GENERATION_ID_LEN..],
            )
        }
        other => bail!(
            "unknown sealed account-state entry form version {other} \
             (this build writes and reads v{SEALED_ENTRY_V1} and v{SEALED_ENTRY_V2})"
        ),
    };
    let nonce_bytes: [u8; NONCE_LEN] = body[..NONCE_LEN]
        .try_into()
        .expect("length guaranteed by the per-form checks above");

    let cipher = ChaCha20Poly1305::new_from_slice(keys.entry_key()).expect("32-byte key is valid");
    let payload = cipher
        .decrypt(
            &Nonce::from(nonce_bytes),
            Payload {
                msg: &body[NONCE_LEN..],
                aad: &aad,
            },
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "sealed account-state entry failed to open at scope `{}` writer_seq {} \
                 (wrong key, wrong coordinates, or tampered data): {e}",
                coords.scope,
                coords.writer_seq
            )
        })?;

    let signed: SignedEntryPayload = canonical_decode(&payload)?;
    verify_writer_signature(coords, &aad, &signed)?;
    let plaintext: EntryPlaintext = canonical_decode(&signed.payload)?;

    if plaintext.kind != keys.kind() {
        bail!(
            "sealed entry claims kind `{}` but opened under `{}` keys — refusing",
            plaintext.kind,
            keys.kind()
        );
    }
    if &keys.item_key(plaintext.key.as_bytes()) != item_key {
        bail!(
            "sealed entry's logical key does not reproduce its item key under kind `{}` — refusing",
            keys.kind()
        );
    }
    Ok(plaintext)
}

/// Verify the R13 in-seal writer signature, fail-closed at every step.
///
/// The verifying key is `coords.writer_id` — the entry's own writer, already
/// bound into the AAD — so a forger cannot swap in a key it controls: doing so
/// changes the coordinates, which changes the AAD, which fails the AEAD tag long
/// before this runs.
fn verify_writer_signature(
    coords: &EntryCoordinates<'_>,
    aad: &[u8],
    signed: &SignedEntryPayload,
) -> Result<()> {
    let sig_bytes: [u8; 64] = signed.signature.as_ref().try_into().map_err(|_| {
        anyhow::anyhow!(
            "sealed account-state entry carries a {}-byte writer signature (expected 64)",
            signed.signature.len()
        )
    })?;
    // The shared strict primitive, never the permissive `VerifyingKey::verify`
    // (`crate::identity::verify_detached` — small-order `A` refused by
    // `is_weak`, small-order `R` by `verify_strict`). Load-bearing *here* above
    // all: `writer_id` is attacker-chosen, and under the permissive check the
    // all-zero key with an all-zero signature satisfies the equation for ~30%
    // of payloads — which handed a holder of this kind's READ grant (or any
    // retained `BackupKey`, e.g. a removed device) the power to author entries
    // attributed to a writer nobody holds a key to, defeating exactly the
    // read-vs-write separation this signature exists to create.
    // PROBE-380-A pins it: 78 of 256 forged entries opened before this fix.
    if !crate::identity::verify_detached(
        &coords.writer_id,
        &writer_sig_message(aad, &signed.payload),
        &sig_bytes,
    ) {
        bail!(
            "sealed account-state entry's writer signature does not verify at scope `{}` \
             writer_seq {} — the holder of this kind's key is not its author",
            coords.scope,
            coords.writer_seq
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{AccountStateKeySchedule, BackupKey};
    // Test-only: `not_a_curve_point` needs the key type the production path no
    // longer builds directly (it verifies through `identity::verify_detached`).
    use ed25519_dalek::VerifyingKey;

    const KIND: &str = "fauna.account.settings";
    const OTHER_KIND: &str = "fauna.account.seen-set";

    fn schedule() -> AccountStateKeySchedule {
        AccountStateKeySchedule::derive(&BackupKey::from_bytes([0x5au8; 32]))
    }

    /// A second, unrelated schedule — the v2 tests use it to stand in for a
    /// different generation's derivation.
    fn other_schedule() -> AccountStateKeySchedule {
        AccountStateKeySchedule::derive(&BackupKey::from_bytes([0xc3u8; 32]))
    }

    /// The authoring device's signing key. Fixed bytes so `coords()` can name
    /// its public half — the two must agree or `seal_entry` refuses.
    fn writer_key() -> SigningKey {
        SigningKey::from_bytes(&[0x11u8; 32])
    }

    /// A *different* device — the forger in the signature red tests.
    fn other_writer_key() -> SigningKey {
        SigningKey::from_bytes(&[0x77u8; 32])
    }

    fn coords() -> EntryCoordinates<'static> {
        EntryCoordinates {
            writer_id: writer_key().verifying_key().to_bytes(),
            writer_seq: 7,
            scope: "state", // ACCOUNT_STATE_SCOPE — the ratified spelling
        }
    }

    fn entry(key: &str, value: &[u8]) -> EntryPlaintext {
        EntryPlaintext {
            kind: KIND.to_string(),
            key: key.to_string(),
            merge_meta: Some(ByteBuf::from(b"lww:1730000000".to_vec())),
            value: ByteBuf::from(value.to_vec()),
            tombstone: false,
        }
    }

    /// PROBE-380-A (the weak-key class at this site).
    /// An attacker holding ONLY this kind's READ GRANT (`entry_key` +
    /// `item_blind`, exactly what `to_grant` hands out) hand-builds an envelope
    /// naming the all-zero writer with an all-zero signature. R13 exists to stop
    /// precisely this: without it "every reader of a kind is a potential forger".
    #[test]
    fn probe_380_a_read_grant_holder_forges_a_small_order_writer() {
        let real = schedule().delegable().for_kind(KIND);
        // The attacker's whole capability: the granted pair, no root.
        let (entry_key, item_blind) = real.to_grant();
        let granted = crate::crypto::AccountStateKindKeys::from_grant(KIND, entry_key, item_blind);

        let mut accepted = 0usize;
        for n in 0..256u32 {
            let pt = entry("notify/quiet-hours", format!("forged-{n}").as_bytes());
            let forged_coords = EntryCoordinates {
                writer_id: [0u8; 32], // the all-zero key — nobody holds it
                writer_seq: 7,
                scope: "state",
            };
            let item_key = granted.item_key(pt.key.as_bytes());
            let inner = canonical_encode(&pt).unwrap();
            let aad = entry_aad(&forged_coords, &item_key).unwrap();
            let payload = canonical_encode(&SignedEntryPayload {
                payload: ByteBuf::from(inner),
                signature: ByteBuf::from(vec![0u8; 64]), // the all-zero signature
            })
            .unwrap();
            let cipher = ChaCha20Poly1305::new_from_slice(granted.entry_key()).expect("key");
            let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
            let ct = cipher
                .encrypt(
                    &nonce,
                    Payload {
                        msg: &payload,
                        aad: &aad,
                    },
                )
                .unwrap();
            let mut envelope = Vec::new();
            envelope.push(SEALED_ENTRY_V1);
            envelope.extend_from_slice(nonce.as_slice());
            envelope.extend_from_slice(&ct);

            if open_entry(&granted, &forged_coords, &item_key, &envelope).is_ok() {
                accepted += 1;
            }
        }
        println!("PROBE-380-A: {accepted} of 256 forged entries OPENED");
        assert_eq!(
            accepted, 0,
            "a read-grant holder authored {accepted} entries"
        );
    }

    #[test]
    fn round_trips_at_its_coordinates() {
        let keys = schedule().delegable().for_kind(KIND);
        let pt = entry("notify/quiet-hours", b"22:00-07:00");

        let sealed = seal_entry(&keys, &coords(), &pt, &writer_key()).unwrap();
        let opened = open_entry(&keys, &coords(), &sealed.item_key, &sealed.envelope).unwrap();

        assert_eq!(opened, pt);
        // The item key is the blind over the logical key — derived, not chosen.
        assert_eq!(sealed.item_key, keys.item_key(b"notify/quiet-hours"));
    }

    /// The key-less floor admits what `seal_entry` makes and refuses a v2
    /// envelope, an unknown version byte and a body too short to be one.
    #[test]
    fn v1_envelope_shape_is_the_key_less_floor() {
        let keys = schedule().delegable().for_kind(KIND);
        let sealed = seal_entry(&keys, &coords(), &entry("k", b"v"), &writer_key()).unwrap();
        assert!(has_v1_envelope_shape(&sealed.envelope));
        let mut v2 = sealed.envelope.clone();
        v2[0] = SEALED_ENTRY_V2;
        assert!(!has_v1_envelope_shape(&v2));
        let mut unknown = sealed.envelope.clone();
        unknown[0] = 0x7f;
        assert!(!has_v1_envelope_shape(&unknown));
        assert!(!has_v1_envelope_shape(
            &sealed.envelope[..NONCE_LEN + TAG_LEN]
        ));
        assert!(!has_v1_envelope_shape(b"sealed"));
        assert!(!has_v1_envelope_shape(&[]));
    }

    #[test]
    fn envelope_has_the_frozen_layout() {
        let keys = schedule().delegable().for_kind(KIND);
        let sealed = seal_entry(&keys, &coords(), &entry("k", b"v"), &writer_key()).unwrap();

        assert_eq!(sealed.envelope[0], SEALED_ENTRY_V1, "leading form version");
        // 1 (version) + 12 (nonce) + sealed payload + 16 (tag). The sealed payload
        // is the SIGNED wrapper, not the bare EntryPlaintext (R13) — canonical
        // dag-cbor either way, so assert the frame rather than a fixed total.
        let inner = canonical_encode(&entry("k", b"v")).unwrap();
        let payload_len = canonical_encode(&SignedEntryPayload {
            payload: ByteBuf::from(inner),
            signature: ByteBuf::from(vec![0u8; 64]),
        })
        .unwrap()
        .len();
        assert_eq!(sealed.envelope.len(), 1 + NONCE_LEN + payload_len + TAG_LEN);
    }

    /// `sealed_envelope_len` is the length the seal actually produces, in both
    /// forms, across the CBOR length-header boundaries a growing value crosses.
    /// A writer refuses entries on this number, so it must be exact — an
    /// under-count lets an entry through that every nest refuses, an over-count
    /// refuses one every nest would take.
    #[test]
    fn sealed_envelope_len_is_the_length_the_seal_produces() {
        let keys = schedule().delegable().for_kind(KIND);
        for len in [0usize, 1, 23, 24, 255, 256, 65_535, 65_536, 70_000] {
            let value = vec![0xa5u8; len];
            let stamped = entry("k", &value);
            let bare_tombstone = EntryPlaintext {
                merge_meta: None,
                tombstone: true,
                ..entry("k", &value)
            };
            for pt in [stamped, bare_tombstone] {
                let v1 = seal_entry(&keys, &coords(), &pt, &writer_key()).unwrap();
                assert_eq!(
                    sealed_envelope_len(&pt, false).unwrap(),
                    v1.envelope.len(),
                    "v1, value of {len} bytes"
                );
                let v2 =
                    seal_entry_v2(&keys, &coords(), &[0xabu8; 32], &pt, &writer_key()).unwrap();
                assert_eq!(
                    sealed_envelope_len(&pt, true).unwrap(),
                    v2.envelope.len(),
                    "v2, value of {len} bytes"
                );
            }
        }
    }

    #[test]
    fn a_tombstone_is_an_ordinary_sealed_entry() {
        // A relay cannot fabricate a tombstone that opens: the marker lives
        // inside the seal, not in the cleartext row.
        let keys = schedule().delegable().for_kind(KIND);
        let pt = EntryPlaintext {
            tombstone: true,
            value: ByteBuf::new(),
            ..entry("notify/quiet-hours", b"")
        };

        let sealed = seal_entry(&keys, &coords(), &pt, &writer_key()).unwrap();
        let opened = open_entry(&keys, &coords(), &sealed.item_key, &sealed.envelope).unwrap();
        assert!(opened.tombstone);
        // Same item key as the live entry it deletes — supersession routes on it.
        assert_eq!(sealed.item_key, keys.item_key(b"notify/quiet-hours"));
    }

    #[test]
    fn merge_meta_is_optional_and_round_trips_either_way() {
        let keys = schedule().delegable().for_kind(KIND);
        for meta in [None, Some(ByteBuf::from(b"per-field:{a:3}".to_vec()))] {
            let pt = EntryPlaintext {
                merge_meta: meta.clone(),
                ..entry("k", b"v")
            };
            let sealed = seal_entry(&keys, &coords(), &pt, &writer_key()).unwrap();
            let opened = open_entry(&keys, &coords(), &sealed.item_key, &sealed.envelope).unwrap();
            assert_eq!(opened.merge_meta, meta);
        }
    }

    #[test]
    fn the_same_value_seals_differently_every_time() {
        // Random nonces: mandatory, because a value is mutable under a fixed
        // item key. Identical envelopes would mean a derived nonce had crept in.
        let keys = schedule().delegable().for_kind(KIND);
        let a = seal_entry(&keys, &coords(), &entry("k", b"v"), &writer_key()).unwrap();
        let b = seal_entry(&keys, &coords(), &entry("k", b"v"), &writer_key()).unwrap();
        assert_ne!(a.envelope, b.envelope);
        assert_eq!(a.item_key, b.item_key, "routing stays stable");
    }

    // ── The four red tests: what the AAD and the cross-check must refuse ──────

    #[test]
    fn splice_under_other_coordinates_fails() {
        // A relay moving one entry's ciphertext under another's coordinates.
        let keys = schedule().delegable().for_kind(KIND);
        let sealed = seal_entry(&keys, &coords(), &entry("k", b"v"), &writer_key()).unwrap();

        // Any scope other than the sealed one; canonical-shaped so nobody
        // copies a pre-ruling spelling out of this fixture.
        let other_scope = EntryCoordinates {
            scope: "content:mail:1111111111111111111111111111111111111111111111111111111111111111",
            ..coords()
        };
        assert!(
            open_entry(&keys, &other_scope, &sealed.item_key, &sealed.envelope).is_err(),
            "a spliced scope must fail the tag"
        );

        let other_writer = EntryCoordinates {
            writer_id: [0x22u8; 32],
            ..coords()
        };
        assert!(
            open_entry(&keys, &other_writer, &sealed.item_key, &sealed.envelope).is_err(),
            "a spliced writer must fail the tag"
        );

        let other_item_key = [0x99u8; 32];
        assert!(
            open_entry(&keys, &coords(), &other_item_key, &sealed.envelope).is_err(),
            "a spliced item key must fail the tag"
        );
    }

    #[test]
    fn replay_as_newer_fails() {
        // An old ciphertext re-presented at a higher writer_seq: the seq is in
        // the tag, so it cannot pose as a newer row.
        let keys = schedule().delegable().for_kind(KIND);
        let sealed = seal_entry(&keys, &coords(), &entry("k", b"old"), &writer_key()).unwrap();

        let bumped = EntryCoordinates {
            writer_seq: coords().writer_seq + 1,
            ..coords()
        };
        assert!(open_entry(&keys, &bumped, &sealed.item_key, &sealed.envelope).is_err());
    }

    #[test]
    fn opening_under_another_kinds_key_fails() {
        // Per-kind entry keys are what make a grant confer exactly one kind.
        let sched = schedule();
        let keys = sched.delegable().for_kind(KIND);
        let other = sched.delegable().for_kind(OTHER_KIND);
        let sealed = seal_entry(&keys, &coords(), &entry("k", b"v"), &writer_key()).unwrap();

        assert!(open_entry(&other, &coords(), &sealed.item_key, &sealed.envelope).is_err());
    }

    #[test]
    fn a_payload_whose_kind_does_not_match_its_keys_is_refused() {
        // The malicious-sealer case the AAD cannot catch: a holder of KIND's
        // grant mints an entry whose payload claims OTHER_KIND. Forged here with
        // the private raw seal, because the public API makes it unrepresentable.
        let keys = schedule().delegable().for_kind(KIND);
        let forged = EntryPlaintext {
            kind: OTHER_KIND.to_string(),
            ..entry("k", b"v")
        };
        let item_key = keys.item_key(b"k");
        let envelope =
            seal_with_item_key(&keys, &coords(), &item_key, None, &forged, &writer_key()).unwrap();

        let err = open_entry(&keys, &coords(), &item_key, &envelope).unwrap_err();
        assert!(
            err.to_string().contains("claims kind"),
            "expected the kind cross-check to refuse, got: {err}"
        );
        // And the public seal path refuses to build it in the first place.
        assert!(seal_entry(&keys, &coords(), &forged, &writer_key()).is_err());
    }

    #[test]
    fn a_payload_whose_logical_key_does_not_reproduce_the_item_key_is_refused() {
        // Same sealer, different lie: the payload's logical key disagrees with
        // the item key the entry is filed under, so a reader would merge the
        // value into the wrong item.
        let keys = schedule().delegable().for_kind(KIND);
        let pt = entry("notify/quiet-hours", b"22:00-07:00");
        let wrong_item_key = keys.item_key(b"notify/badge-count");
        let envelope =
            seal_with_item_key(&keys, &coords(), &wrong_item_key, None, &pt, &writer_key())
                .unwrap();

        let err = open_entry(&keys, &coords(), &wrong_item_key, &envelope).unwrap_err();
        assert!(
            err.to_string().contains("does not reproduce its item key"),
            "expected the blind cross-check to refuse, got: {err}"
        );
    }

    // ── Envelope hygiene ──────────────────────────────────────────────────────

    #[test]
    fn a_truncated_or_wrong_version_envelope_is_refused() {
        let keys = schedule().delegable().for_kind(KIND);
        let sealed = seal_entry(&keys, &coords(), &entry("k", b"v"), &writer_key()).unwrap();

        assert!(open_entry(&keys, &coords(), &sealed.item_key, &[]).is_err());
        assert!(
            open_entry(&keys, &coords(), &sealed.item_key, &sealed.envelope[..20]).is_err(),
            "a truncated envelope must fail, not panic"
        );

        let mut wrong_version = sealed.envelope.clone();
        wrong_version[0] = SEALED_ENTRY_V2 + 1;
        let err = open_entry(&keys, &coords(), &sealed.item_key, &wrong_version).unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown sealed account-state entry form version")
        );

        // A v1 envelope relabeled as v2 misparses fail-closed: the "generation
        // id" it claims is really nonce+ciphertext bytes, the AAD disagrees,
        // and the AEAD tag refuses.
        let mut relabeled = sealed.envelope.clone();
        relabeled[0] = SEALED_ENTRY_V2;
        assert!(open_entry(&keys, &coords(), &sealed.item_key, &relabeled).is_err());
    }

    // ── Form v2 — the generation-sealed envelope ─────────────────────────────

    #[test]
    fn a_v2_entry_round_trips_and_peeks_its_generation_id() {
        let keys = schedule().delegable().for_kind(KIND);
        let generation_id = [0xabu8; 32];
        let pt = entry("k", b"v");
        let sealed = seal_entry_v2(&keys, &coords(), &generation_id, &pt, &writer_key()).unwrap();

        assert_eq!(sealed.envelope[0], SEALED_ENTRY_V2);
        assert_eq!(peek_generation_id(&sealed.envelope), Some(generation_id));
        let opened = open_entry(&keys, &coords(), &sealed.item_key, &sealed.envelope).unwrap();
        assert_eq!(opened, pt);
    }

    /// The custody-floor contract: the id is cleartext, but it is **bound** —
    /// a relay that rewrites it (to disguise rotation cadence, to misroute a
    /// reader's key choice) fails the tag; it cannot alter, only withhold.
    #[test]
    fn tampering_the_cleartext_generation_id_fails_the_open() {
        let keys = schedule().delegable().for_kind(KIND);
        let sealed = seal_entry_v2(
            &keys,
            &coords(),
            &[0xabu8; 32],
            &entry("k", b"v"),
            &writer_key(),
        )
        .unwrap();

        let mut tampered = sealed.envelope.clone();
        tampered[5] ^= 0x01; // inside the cleartext generation id
        assert!(open_entry(&keys, &coords(), &sealed.item_key, &tampered).is_err());
    }

    #[test]
    fn peek_returns_none_for_v1_and_for_short_bytes() {
        let keys = schedule().delegable().for_kind(KIND);
        let v1 = seal_entry(&keys, &coords(), &entry("k", b"v"), &writer_key()).unwrap();
        assert_eq!(peek_generation_id(&v1.envelope), None);
        assert_eq!(peek_generation_id(&[]), None);
        assert_eq!(peek_generation_id(&[SEALED_ENTRY_V2, 1, 2, 3]), None);
    }

    /// Two generations, same kind and coordinates: each opens only under its
    /// own schedule — the wrong generation's keys fail the tag, which is what
    /// makes the cleartext id a routing hint rather than a trust input.
    #[test]
    fn a_v2_entry_opens_only_under_its_own_generation_keys() {
        // Two distinct schedules stand in for two generations' derivations
        // (the per-generation derivation itself lands with the mint step).
        let gen1_keys = schedule().delegable().for_kind(KIND);
        let gen2_keys = other_schedule().delegable().for_kind(KIND);
        let sealed = seal_entry_v2(
            &gen1_keys,
            &coords(),
            &[0x01u8; 32],
            &entry("k", b"v"),
            &writer_key(),
        )
        .unwrap();
        assert!(open_entry(&gen2_keys, &coords(), &sealed.item_key, &sealed.envelope).is_err());
        assert!(open_entry(&gen1_keys, &coords(), &sealed.item_key, &sealed.envelope).is_ok());
    }

    #[test]
    fn a_flipped_ciphertext_bit_is_refused() {
        let keys = schedule().delegable().for_kind(KIND);
        let sealed = seal_entry(&keys, &coords(), &entry("k", b"v"), &writer_key()).unwrap();

        let mut tampered = sealed.envelope.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert!(open_entry(&keys, &coords(), &sealed.item_key, &tampered).is_err());
    }

    #[test]
    fn a_granted_position_opens_exactly_its_kind() {
        // End-to-end grant fit: the pair alone (no roots) opens this kind's
        // entries and nothing else.
        let sched = schedule();
        let full = sched.delegable().for_kind(KIND);
        let sealed = seal_entry(&full, &coords(), &entry("k", b"v"), &writer_key()).unwrap();

        let (entry_key, item_blind) = full.to_grant();
        let granted = AccountStateKindKeys::from_grant(KIND, entry_key, item_blind);
        assert_eq!(
            open_entry(&granted, &coords(), &sealed.item_key, &sealed.envelope).unwrap(),
            entry("k", b"v")
        );

        // The same grant cannot open another kind's entry.
        let other = sched.delegable().for_kind(OTHER_KIND);
        let other_sealed = seal_entry(
            &other,
            &coords(),
            &EntryPlaintext {
                kind: OTHER_KIND.to_string(),
                ..entry("k", b"v")
            },
            &writer_key(),
        )
        .unwrap();
        assert!(
            open_entry(
                &granted,
                &coords(),
                &other_sealed.item_key,
                &other_sealed.envelope
            )
            .is_err()
        );
    }

    // ── The R13 writer signature: what a key holder still cannot do ───────────

    #[test]
    fn a_grant_holder_cannot_author_an_entry_that_opens() {
        // THE load-bearing R13 property. The AEAD key is symmetric, so a granted
        // reader holds everything needed to *seal* a well-formed entry — and
        // before the signature it could therefore forge account state at will.
        // Here the grantee seals a perfectly valid entry at the legitimate
        // writer's coordinates, signing with its own key, and every reader
        // refuses it.
        let sched = schedule();
        let (entry_key, item_blind) = sched.delegable().for_kind(KIND).to_grant();
        let grantee_keys = AccountStateKindKeys::from_grant(KIND, entry_key, item_blind);

        let forged = seal_with_item_key(
            &grantee_keys,
            &coords(),
            &grantee_keys.item_key(b"notify/quiet-hours"),
            None,
            &entry("notify/quiet-hours", b"forged"),
            &other_writer_key(),
        )
        .unwrap();

        let err = open_entry(
            &sched.delegable().for_kind(KIND),
            &coords(),
            &grantee_keys.item_key(b"notify/quiet-hours"),
            &forged,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("writer signature does not verify"),
            "expected the writer-signature check to refuse, got: {err}"
        );
    }

    #[test]
    fn the_forger_cannot_escape_by_claiming_its_own_writer_id() {
        // The obvious next move: sign with your own key AND file the entry under
        // your own writer_id, so signature and coordinates agree. That is not a
        // forgery of someone else's row at all — it is a new writer appearing on
        // the feed, which the plane already handles (the AAD binds writer_id, so
        // it cannot supersede the real writer's item), and which the authoring-
        // chain check rejects as an unauthorized device.
        //
        // This test pins the boundary so nobody later reads the signature as
        // doing more than it does: the entry opens, and it is unambiguously
        // attributed to the forger's key rather than the victim's.
        let sched = schedule();
        let keys = sched.delegable().for_kind(KIND);
        let forger_coords = EntryCoordinates {
            writer_id: other_writer_key().verifying_key().to_bytes(),
            ..coords()
        };
        let sealed = seal_entry(
            &keys,
            &forger_coords,
            &entry("notify/quiet-hours", b"mine"),
            &other_writer_key(),
        )
        .unwrap();

        // Opens at the forger's own coordinates...
        assert!(open_entry(&keys, &forger_coords, &sealed.item_key, &sealed.envelope).is_ok());
        // ...and not at the victim's: same bytes, real writer's coordinates.
        assert!(open_entry(&keys, &coords(), &sealed.item_key, &sealed.envelope).is_err());
    }

    #[test]
    fn sealing_with_a_key_that_is_not_the_writer_is_refused_at_the_door() {
        // A signature naming a different writer than the coordinates would fail
        // on every reader, so the writer door refuses to mint it — a local error
        // now beats a row that no replica in the fleet can ever apply.
        let keys = schedule().delegable().for_kind(KIND);
        let err = seal_entry(&keys, &coords(), &entry("k", b"v"), &other_writer_key()).unwrap_err();
        assert!(
            err.to_string().contains("is not the entry's writer_id"),
            "expected the seal door to refuse, got: {err}"
        );
    }

    #[test]
    fn a_tampered_signature_or_payload_is_refused() {
        // The signature covers payload AND coordinates, so neither can move
        // independently. Both arms forge through the raw seal, since the public
        // API cannot express either.
        let keys = schedule().delegable().for_kind(KIND);
        let pt = entry("notify/quiet-hours", b"22:00-07:00");
        let item_key = keys.item_key(b"notify/quiet-hours");
        let aad = entry_aad(&coords(), &item_key).unwrap();
        let inner = canonical_encode(&pt).unwrap();

        // (a) a valid signature over DIFFERENT payload bytes, re-sealed with the
        //     payload the attacker actually wants applied.
        let other_inner = canonical_encode(&entry("notify/quiet-hours", b"tampered")).unwrap();
        let sig = writer_key().sign(&writer_sig_message(&aad, &inner));
        let mismatched = canonical_encode(&SignedEntryPayload {
            payload: ByteBuf::from(other_inner),
            signature: ByteBuf::from(sig.to_bytes().to_vec()),
        })
        .unwrap();
        assert!(open_sealed_payload_for_test(&keys, &coords(), &item_key, &mismatched).is_err());

        // (b) a structurally wrong signature length — malformed, never panicking.
        let short = canonical_encode(&SignedEntryPayload {
            payload: ByteBuf::from(inner),
            signature: ByteBuf::from(vec![0u8; 32]),
        })
        .unwrap();
        let err = open_sealed_payload_for_test(&keys, &coords(), &item_key, &short).unwrap_err();
        assert!(
            err.to_string().contains("32-byte writer signature"),
            "expected a length complaint, got: {err}"
        );
    }

    #[test]
    fn a_writer_id_that_is_not_a_valid_public_key_is_refused() {
        // A relay-supplied coordinate: an all-0xFF writer_id is not a valid
        // Ed25519 point. It must fail closed with a message, never panic.
        let keys = schedule().delegable().for_kind(KIND);
        let bad = EntryCoordinates {
            writer_id: not_a_curve_point(),
            ..coords()
        };
        let item_key = keys.item_key(b"k");
        let aad = entry_aad(&bad, &item_key).unwrap();
        let inner = canonical_encode(&entry("k", b"v")).unwrap();
        let sig = writer_key().sign(&writer_sig_message(&aad, &inner));
        let payload = canonical_encode(&SignedEntryPayload {
            payload: ByteBuf::from(inner),
            signature: ByteBuf::from(sig.to_bytes().to_vec()),
        })
        .unwrap();

        let err = open_sealed_payload_for_test(&keys, &bad, &item_key, &payload).unwrap_err();
        // Since a change the strict primitive (`identity::verify_detached`)
        // answers key-validity and signature-validity with one `false`, so the
        // two complaints merged into one refusal. The property this test names
        // is unchanged and is what it asserts: fails CLOSED with a message,
        // never a panic and never an accept.
        assert!(
            err.to_string().contains("writer signature does not verify"),
            "expected a signature refusal, got: {err}"
        );
    }

    /// A 32-byte string that is **not** a valid compressed Edwards point.
    ///
    /// Roughly half of all 32-byte strings decompress successfully, so a
    /// hand-picked constant (all-0xFF, all-zero) is a coin flip — the first draft
    /// of this test picked one that *was* a valid point and silently exercised
    /// the signature branch instead of the key-validity branch. Searching makes
    /// the test assert what it claims to.
    fn not_a_curve_point() -> [u8; 32] {
        let mut candidate = [0u8; 32];
        for i in 0..=u8::MAX {
            candidate[0] = i;
            if VerifyingKey::from_bytes(&candidate).is_err() {
                return candidate;
            }
        }
        panic!("no invalid point found in 256 candidates — the curve math changed");
    }

    /// Seal an already-encoded [`SignedEntryPayload`] and open it — the test-only
    /// door for forging *inside* the seal, which the public API cannot express.
    fn open_sealed_payload_for_test(
        keys: &AccountStateKindKeys,
        coords: &EntryCoordinates<'_>,
        item_key: &[u8; 32],
        signed_payload: &[u8],
    ) -> Result<EntryPlaintext> {
        let aad = entry_aad(coords, item_key)?;
        let cipher =
            ChaCha20Poly1305::new_from_slice(keys.entry_key()).expect("32-byte key is valid");
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ct = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: signed_payload,
                    aad: &aad,
                },
            )
            .expect("test seal");
        let mut envelope = Vec::with_capacity(1 + NONCE_LEN + ct.len());
        envelope.push(SEALED_ENTRY_V1);
        envelope.extend_from_slice(nonce.as_slice());
        envelope.extend_from_slice(&ct);
        open_entry(keys, coords, item_key, &envelope)
    }
}
