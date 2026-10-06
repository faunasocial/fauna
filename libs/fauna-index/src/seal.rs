//! Wire-format AEAD encryption for fauna-index.
//!
//! Two formats live here:
//!
//! * **Segment format** (`seal_segment_bytes` / `open_segment_bytes`): wraps a
//!   plaintext byte stream (typically `Index::seal_to_bytes` output) under a
//!   per-segment data key, which is itself wrapped under the user's
//!   [`IndexMasterKey`] in the header. Master-key rotation
//!   ([`rewrap_segment_master_key`]) rewrites only the 48-byte wrapped data
//!   key — the body is untouched.
//!
//! * **Master-direct format** (`seal_under_master` / `open_under_master`): a
//!   simpler envelope used for small, frequently rewritten files like
//!   `manifest.idx` and `classifier-ledger.idx` (Plan 3). The body is
//!   AEAD-sealed directly under the master key; rotation is just a re-encrypt.
//!
//! AEAD primitive: XChaCha20-Poly1305 with 24-byte random nonces (per spec
//! D10). Algorithm byte 0x00; future schemes get new bytes without changing
//! the layout.
//!
//! ## The 8-byte header prefix — versioned, and bound as AAD
//!
//! Both framings open with the **same** 8-byte prefix:
//!
//! ```text
//! 0..4  magic              b"FXSG" (segment) | b"FXMG" (master-direct)
//! 4     format_version     the writer's format
//! 5     min_reader_version the oldest reader that may touch this blob
//! 6     algorithm          0x00 = XChaCha20-Poly1305
//! 7     reserved           0x00
//! ```
//!
//! Byte 5 is the § 2.2 two-number scheme's reader floor (`crate::version`),
//! spent out of what used to be two reserved zero bytes. It is what lets a
//! reader tell a **newer-but-additive** blob (read it — I2 backward-compat)
//! from a **newer-and-breaking** one (refuse honestly with the typed
//! [`IndexError::Incompatible`], and touch nothing — I1). The old exact-match
//! `version != VERSION` reject could not tell those apart, and answered both
//! with an `IndexError::Crypto` a caller could not distinguish from "corrupt".
//!
//! **The prefix is the AEAD associated data** for every encryption in both
//! framings. This reverses the v1 design's "why no AAD" call, and the reversal
//! is caused by the versioning above:
//!
//! * v1's concrete objection was that binding the *nonce* or the *wrapped data
//!   key* would make [`rewrap_segment_master_key`] (which mutates exactly those)
//!   invalidate the body tag, destroying the cheap-rewrap property. That does not
//!   apply to a **static** prefix: rewrap copies bytes 0..8 verbatim, so both tags
//!   still verify and rewrap stays O(header).
//! * v1 also judged static-field AAD to buy nothing. That was true when there was
//!   **one** version and an intolerant reader — there was nothing to roll back
//!   *to*. Making readers tolerate a *range* of versions is precisely what creates
//!   a version-rollback surface: without AAD, anyone who can rewrite the at-rest
//!   bytes could relabel an authentic newer blob as an older version and have an
//!   older reader decrypt its body under the wrong semantics. Binding the prefix
//!   pins each body to the format that produced it.
//!
//! This is affordable only because **nothing has ever written this format to disk**
//! (see `crate::version`) — after the first real write it would be a breaking change
//! to user data forever.

use crate::key::{IndexMasterKey, IndexSegmentKey};
use crate::types::IndexError;
use crate::version::{
    CURRENT_INDEX_FORMAT_VERSION, IndexFormatStamp, MIN_READER_INDEX_FORMAT_VERSION,
};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
};

// ---- shared header-prefix constants ----------------------------------------

const ALG_XCHACHA20_POLY1305: u8 = 0x00;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const DATA_KEY_LEN: usize = 32;
const WRAPPED_DK_LEN: usize = DATA_KEY_LEN + TAG_LEN; // 48

// Offsets inside the 8-byte prefix both framings share.
const OFF_MAGIC: usize = 0;
const OFF_VERSION: usize = 4;
const OFF_MIN_READER: usize = 5;
const OFF_ALG: usize = 6;
const OFF_RESERVED: usize = 7;
/// The AAD region, and the fixed prefix of both framings.
const HEADER_PREFIX_LEN: usize = 8;

// ---- segment-format constants ----------------------------------------------

const SEGMENT_MAGIC: [u8; 4] = *b"FXSG";

const OFF_HEADER_NONCE: usize = HEADER_PREFIX_LEN;
const OFF_WRAPPED_DK: usize = OFF_HEADER_NONCE + NONCE_LEN; // 32
const OFF_BODY_NONCE: usize = OFF_WRAPPED_DK + WRAPPED_DK_LEN; // 80
const OFF_BODY: usize = OFF_BODY_NONCE + NONCE_LEN; // 104
const SEGMENT_HEADER_LEN: usize = OFF_BODY; // 104

// ---- master-direct format constants ----------------------------------------

const MASTER_DIRECT_MAGIC: [u8; 4] = *b"FXMG";

const MD_OFF_NONCE: usize = HEADER_PREFIX_LEN;
const MD_OFF_BODY: usize = MD_OFF_NONCE + NONCE_LEN; // 32
const MASTER_DIRECT_HEADER_LEN: usize = MD_OFF_BODY; // 32

/// This build's 8-byte prefix for `magic`.
fn header_prefix(magic: &[u8; 4]) -> [u8; HEADER_PREFIX_LEN] {
    let mut p = [0u8; HEADER_PREFIX_LEN];
    p[OFF_MAGIC..OFF_MAGIC + 4].copy_from_slice(magic);
    p[OFF_VERSION] = CURRENT_INDEX_FORMAT_VERSION;
    p[OFF_MIN_READER] = MIN_READER_INDEX_FORMAT_VERSION;
    p[OFF_ALG] = ALG_XCHACHA20_POLY1305;
    p[OFF_RESERVED] = 0;
    p
}

/// Read a sealed blob's version stamp from its plaintext prefix, without keys
/// and without opening anything. `None` when the bytes are not either framing.
///
/// The stamp bytes sit in the AAD-bound prefix precisely so a reader can make
/// version decisions pre-decryption; this is the API form of that property.
/// Consumer: the resume/read paths' pre-v3 retirement gate (`content-index.md`
/// § Where the index is built → the 2026-08-10 carrier ruling) — a segment
/// stamped below [`CURRENT_INDEX_FORMAT_VERSION`] predates the `secondary_id`
/// schema field and must be tombstoned or skipped, never fed to
/// `open_multi_segment` beside a current one (single-schema requirement).
pub fn peek_sealed_stamp(bytes: &[u8]) -> Option<IndexFormatStamp> {
    if bytes.len() < HEADER_PREFIX_LEN {
        return None;
    }
    let magic = &bytes[OFF_MAGIC..OFF_MAGIC + 4];
    if magic != SEGMENT_MAGIC && magic != MASTER_DIRECT_MAGIC {
        return None;
    }
    Some(IndexFormatStamp::from_raw(
        bytes[OFF_VERSION],
        bytes[OFF_MIN_READER],
    ))
}

/// Validate one framing's prefix and return the § 2.2 verdict's `Ok`/`Err`.
///
/// Order matters: magic first (so a wrong-format blob is diagnosed as such rather
/// than by length), then length, then the **version verdict** — which runs before
/// any decryption, so a blob this build must not touch is never even opened.
fn check_header(
    bytes: &[u8],
    magic: &[u8; 4],
    what: &str,
    min_len: usize,
) -> Result<(), IndexError> {
    if bytes.len() < OFF_MAGIC + 4 || bytes[OFF_MAGIC..OFF_MAGIC + 4] != *magic {
        return Err(IndexError::Crypto(format!("bad {what} magic")));
    }
    if bytes.len() < min_len {
        return Err(IndexError::Crypto(format!(
            "sealed {what} too short ({} bytes, need at least {min_len})",
            bytes.len(),
        )));
    }
    // The version gate, before anything else reads the body. A newer-breaking blob
    // refuses with the typed `Incompatible` — never the generic `Crypto` a caller
    // could mistake for "corrupt, recreate it".
    IndexFormatStamp::from_raw(bytes[OFF_VERSION], bytes[OFF_MIN_READER]).check()?;
    if bytes[OFF_ALG] != ALG_XCHACHA20_POLY1305 {
        return Err(IndexError::Crypto(format!(
            "unsupported algorithm byte {}",
            bytes[OFF_ALG]
        )));
    }
    if bytes[OFF_RESERVED] != 0 {
        return Err(IndexError::Crypto("non-zero reserved byte".into()));
    }
    Ok(())
}

fn check_segment_header(bytes: &[u8]) -> Result<(), IndexError> {
    check_header(
        bytes,
        &SEGMENT_MAGIC,
        "segment",
        SEGMENT_HEADER_LEN + TAG_LEN,
    )
}

fn check_master_direct_header(bytes: &[u8]) -> Result<(), IndexError> {
    check_header(
        bytes,
        &MASTER_DIRECT_MAGIC,
        "master-direct",
        MASTER_DIRECT_HEADER_LEN + TAG_LEN,
    )
}

fn xchacha(key: &[u8; 32]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new_from_slice(key)
        .expect("32-byte key is always valid for XChaCha20Poly1305")
}

fn fresh_nonce() -> XNonce {
    XChaCha20Poly1305::generate_nonce(&mut OsRng)
}

fn fresh_data_key() -> [u8; 32] {
    // 32 fresh random bytes from the OS CSRNG, used as a per-segment data key.
    use chacha20poly1305::aead::rand_core::RngCore;
    let mut k = [0u8; 32];
    OsRng.fill_bytes(&mut k);
    k
}

/// Seal `plaintext` as a segment blob under `master` — the wrap key for every
/// **master-class** kind (conversation / post / file / contact / draft / media).
///
/// Generates a fresh per-segment data key + two random 24-byte nonces, then
/// emits the wire layout documented at the module level. Both encryptions bind
/// the 8-byte header prefix as AAD.
pub fn seal_segment_bytes(
    plaintext: &[u8],
    master: &IndexMasterKey,
) -> Result<Vec<u8>, IndexError> {
    seal_segment_raw(plaintext, master.as_bytes(), header_prefix(&SEGMENT_MAGIC))
}

/// [`seal_segment_bytes`]'s **mail/calendar** twin: identical framing, but the
/// per-segment data key wraps under the MSEK-derived [`IndexSegmentKey`] — the
/// S0-ratified per-kind key split (`content-index.md` § Encryption posture).
pub fn seal_segment_bytes_mailcal(
    plaintext: &[u8],
    key: &IndexSegmentKey,
) -> Result<Vec<u8>, IndexError> {
    seal_segment_raw(plaintext, key.as_bytes(), header_prefix(&SEGMENT_MAGIC))
}

/// [`seal_segment_bytes`] over a caller-supplied prefix. Test-only: production always
/// seals with *this* build's stamp. Tests use it to forge the blob a **future** build
/// would write, which is the only way to prove the tolerant read actually tolerates.
#[cfg(test)]
fn seal_segment_bytes_with_prefix(
    plaintext: &[u8],
    master: &IndexMasterKey,
    prefix: [u8; HEADER_PREFIX_LEN],
) -> Result<Vec<u8>, IndexError> {
    seal_segment_raw(plaintext, master.as_bytes(), prefix)
}

/// The one real segment-seal implementation — both public wrap-key types
/// delegate here. `wrap` is whichever 32-byte key the kind's class dictates.
fn seal_segment_raw(
    plaintext: &[u8],
    wrap: &[u8; 32],
    prefix: [u8; HEADER_PREFIX_LEN],
) -> Result<Vec<u8>, IndexError> {
    let data_key: zeroize::Zeroizing<[u8; DATA_KEY_LEN]> =
        zeroize::Zeroizing::new(fresh_data_key());
    let header_nonce = fresh_nonce();
    let body_nonce = fresh_nonce();

    let wrap_cipher = xchacha(wrap);
    let wrapped_dk = wrap_cipher
        .encrypt(
            &header_nonce,
            Payload {
                msg: data_key.as_slice(),
                aad: &prefix,
            },
        )
        .map_err(|e| IndexError::Crypto(format!("wrap data key: {e}")))?;
    debug_assert_eq!(wrapped_dk.len(), WRAPPED_DK_LEN);

    let body_cipher = xchacha(&data_key);
    let body = body_cipher
        .encrypt(
            &body_nonce,
            Payload {
                msg: plaintext,
                aad: &prefix,
            },
        )
        .map_err(|e| IndexError::Crypto(format!("encrypt body: {e}")))?;

    let mut out = Vec::with_capacity(SEGMENT_HEADER_LEN + body.len());
    out.extend_from_slice(&prefix);
    out.extend_from_slice(header_nonce.as_slice());
    out.extend_from_slice(&wrapped_dk);
    out.extend_from_slice(body_nonce.as_slice());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Inverse of [`seal_segment_bytes`]. Verifies the framing (including the version
/// verdict), unwraps the data key under `master`, then decrypts the body.
pub fn open_segment_bytes(sealed: &[u8], master: &IndexMasterKey) -> Result<Vec<u8>, IndexError> {
    open_segment_raw(sealed, master.as_bytes())
}

/// Inverse of [`seal_segment_bytes_mailcal`] — the mail/calendar twin of
/// [`open_segment_bytes`].
pub fn open_segment_bytes_mailcal(
    sealed: &[u8],
    key: &IndexSegmentKey,
) -> Result<Vec<u8>, IndexError> {
    open_segment_raw(sealed, key.as_bytes())
}

fn open_segment_raw(sealed: &[u8], wrap: &[u8; 32]) -> Result<Vec<u8>, IndexError> {
    check_segment_header(sealed)?;
    let aad = &sealed[..HEADER_PREFIX_LEN];
    let header_nonce = XNonce::from_slice(&sealed[OFF_HEADER_NONCE..OFF_HEADER_NONCE + NONCE_LEN]);
    let wrapped_dk = &sealed[OFF_WRAPPED_DK..OFF_WRAPPED_DK + WRAPPED_DK_LEN];
    let body_nonce = XNonce::from_slice(&sealed[OFF_BODY_NONCE..OFF_BODY_NONCE + NONCE_LEN]);
    let body = &sealed[OFF_BODY..];

    let wrap_cipher = xchacha(wrap);
    let data_key_vec: zeroize::Zeroizing<Vec<u8>> = wrap_cipher
        .decrypt(
            header_nonce,
            Payload {
                msg: wrapped_dk,
                aad,
            },
        )
        .map_err(|_| {
            IndexError::Crypto("unwrap data key failed (wrong wrap key or tampered header)".into())
        })?
        .into();
    if data_key_vec.len() != DATA_KEY_LEN {
        return Err(IndexError::Crypto(format!(
            "unwrapped data key has wrong length {}",
            data_key_vec.len()
        )));
    }
    let mut data_key: zeroize::Zeroizing<[u8; DATA_KEY_LEN]> =
        zeroize::Zeroizing::new([0u8; DATA_KEY_LEN]);
    data_key.copy_from_slice(&data_key_vec);

    let body_cipher = xchacha(&data_key);
    let plaintext = body_cipher
        .decrypt(body_nonce, Payload { msg: body, aad })
        .map_err(|_| IndexError::Crypto("decrypt body failed (tampered body)".into()))?;

    Ok(plaintext)
}

/// Master-key rotation: re-encrypt the wrapped data key under `new_master`
/// without re-encrypting the body.
///
/// The body's AEAD tag does not depend on the master key (the wrapped data
/// key is the only thing the master touches), so the *cryptographic* work is
/// O(header) — about a hundred bytes regardless of segment size. The returned
/// `Vec` is full-size because the body bytes are copied verbatim; fauna-sync
/// can ship only the rewrapped 80-byte header region over the wire (Plan 3
/// will wire that up).
///
/// The 8-byte prefix is copied **verbatim**, not regenerated: it is the AAD both
/// tags were computed over, so rewriting it would invalidate the body we are not
/// re-encrypting. That verbatim copy also means a rotation performed by an older
/// build never restamps a newer segment's version down (§ 2.2) — the "do not
/// restamp down" rule holds here for free.
///
/// Returns a fresh blob with new `header_nonce` + new `wrapped_dk` and the
/// original prefix + `body_nonce` + `body` byte-identical to the input.
pub fn rewrap_segment_master_key(
    sealed: &[u8],
    old_master: &IndexMasterKey,
    new_master: &IndexMasterKey,
) -> Result<Vec<u8>, IndexError> {
    rewrap_segment_raw(sealed, old_master.as_bytes(), new_master.as_bytes())
}

/// [`rewrap_segment_master_key`]'s mail/calendar twin — the MSEK-rotation rewrap
/// pass over mail/calendar segment headers (`key-material-hierarchy.md`
/// § Path B-sibling-4: the rotating client converges every segment to the new
/// generation's key; O(header), bodies untouched).
pub fn rewrap_segment_mailcal_key(
    sealed: &[u8],
    old_key: &IndexSegmentKey,
    new_key: &IndexSegmentKey,
) -> Result<Vec<u8>, IndexError> {
    rewrap_segment_raw(sealed, old_key.as_bytes(), new_key.as_bytes())
}

fn rewrap_segment_raw(
    sealed: &[u8],
    old_wrap: &[u8; 32],
    new_wrap: &[u8; 32],
) -> Result<Vec<u8>, IndexError> {
    check_segment_header(sealed)?;
    let prefix = &sealed[..HEADER_PREFIX_LEN];
    let old_header_nonce =
        XNonce::from_slice(&sealed[OFF_HEADER_NONCE..OFF_HEADER_NONCE + NONCE_LEN]);
    let wrapped_dk = &sealed[OFF_WRAPPED_DK..OFF_WRAPPED_DK + WRAPPED_DK_LEN];

    // Unwrap under old key.
    let old_cipher = xchacha(old_wrap);
    let data_key_vec: zeroize::Zeroizing<Vec<u8>> = old_cipher
        .decrypt(
            old_header_nonce,
            Payload {
                msg: wrapped_dk,
                aad: prefix,
            },
        )
        .map_err(|_| {
            IndexError::Crypto(
                "unwrap data key failed (wrong old wrap key or tampered header)".into(),
            )
        })?
        .into();
    if data_key_vec.len() != DATA_KEY_LEN {
        return Err(IndexError::Crypto(format!(
            "unwrapped data key has wrong length {}",
            data_key_vec.len()
        )));
    }

    // Re-wrap under new key with a fresh nonce, over the same (verbatim) prefix.
    let new_header_nonce = fresh_nonce();
    let new_cipher = xchacha(new_wrap);
    let new_wrapped_dk = new_cipher
        .encrypt(
            &new_header_nonce,
            Payload {
                msg: data_key_vec.as_slice(),
                aad: prefix,
            },
        )
        .map_err(|e| IndexError::Crypto(format!("re-wrap data key: {e}")))?;
    debug_assert_eq!(new_wrapped_dk.len(), WRAPPED_DK_LEN);

    let mut out = Vec::with_capacity(sealed.len());
    out.extend_from_slice(prefix);
    out.extend_from_slice(new_header_nonce.as_slice());
    out.extend_from_slice(&new_wrapped_dk);
    out.extend_from_slice(&sealed[OFF_BODY_NONCE..]);
    Ok(out)
}

/// AEAD-encrypt `plaintext` directly under `master`. Used for small files
/// (`manifest.idx`, `classifier-ledger.idx`) where rotation is just a
/// rewrite.
pub fn seal_under_master(plaintext: &[u8], master: &IndexMasterKey) -> Result<Vec<u8>, IndexError> {
    seal_direct_raw(
        plaintext,
        master.as_bytes(),
        header_prefix(&MASTER_DIRECT_MAGIC),
    )
}

/// [`seal_under_master`]'s mail/calendar twin: the same direct framing (same
/// `FXMG` magic — the framing describes the envelope, not the key), sealed
/// under the [`IndexSegmentKey`]. Used for `manifest-mailcal.idx`.
pub fn seal_under_mailcal_key(
    plaintext: &[u8],
    key: &IndexSegmentKey,
) -> Result<Vec<u8>, IndexError> {
    seal_direct_raw(
        plaintext,
        key.as_bytes(),
        header_prefix(&MASTER_DIRECT_MAGIC),
    )
}

/// [`seal_under_master`] over a caller-supplied prefix — see
/// [`seal_segment_bytes_with_prefix`].
#[cfg(test)]
fn seal_under_master_with_prefix(
    plaintext: &[u8],
    master: &IndexMasterKey,
    prefix: [u8; HEADER_PREFIX_LEN],
) -> Result<Vec<u8>, IndexError> {
    seal_direct_raw(plaintext, master.as_bytes(), prefix)
}

fn seal_direct_raw(
    plaintext: &[u8],
    wrap: &[u8; 32],
    prefix: [u8; HEADER_PREFIX_LEN],
) -> Result<Vec<u8>, IndexError> {
    let nonce = fresh_nonce();
    let cipher = xchacha(wrap);
    let body = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: &prefix,
            },
        )
        .map_err(|e| IndexError::Crypto(format!("encrypt master-direct body: {e}")))?;

    let mut out = Vec::with_capacity(MASTER_DIRECT_HEADER_LEN + body.len());
    out.extend_from_slice(&prefix);
    out.extend_from_slice(nonce.as_slice());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Inverse of [`seal_under_master`].
pub fn open_under_master(sealed: &[u8], master: &IndexMasterKey) -> Result<Vec<u8>, IndexError> {
    open_direct_raw(sealed, master.as_bytes())
}

/// Inverse of [`seal_under_mailcal_key`].
pub fn open_under_mailcal_key(sealed: &[u8], key: &IndexSegmentKey) -> Result<Vec<u8>, IndexError> {
    open_direct_raw(sealed, key.as_bytes())
}

fn open_direct_raw(sealed: &[u8], wrap: &[u8; 32]) -> Result<Vec<u8>, IndexError> {
    check_master_direct_header(sealed)?;
    let aad = &sealed[..HEADER_PREFIX_LEN];
    let nonce = XNonce::from_slice(&sealed[MD_OFF_NONCE..MD_OFF_NONCE + NONCE_LEN]);
    let body = &sealed[MD_OFF_BODY..];
    let cipher = xchacha(wrap);
    cipher
        .decrypt(nonce, Payload { msg: body, aad })
        .map_err(|_| {
            IndexError::Crypto(
                "decrypt master-direct body failed (wrong wrap key or tampered)".into(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> IndexMasterKey {
        IndexMasterKey::from_bytes([b; 32])
    }

    /// A prefix as a **future** build would write it.
    fn future_prefix(magic: &[u8; 4], format_version: u8, min_reader_version: u8) -> [u8; 8] {
        let mut p = header_prefix(magic);
        p[OFF_VERSION] = format_version;
        p[OFF_MIN_READER] = min_reader_version;
        p
    }

    /// **The property the whole scheme exists for.** A future build bumps the format
    /// additively — new `format_version`, reader floor untouched — and *this* build must
    /// still open its segments (I2 backward-compat). If this fails, the two-number
    /// scheme is decoration.
    ///
    /// It also pins the half the AAD binding could plausibly have broken: the prefix is
    /// associated data, so a *legitimate* version bump must still verify, because the
    /// future writer computed its tags over that same prefix. Only *tampering* with the
    /// prefix after the fact breaks the tag (see `flipped_version_byte_is_caught_as_tampering`).
    #[test]
    fn newer_additive_segment_opens_under_this_build() {
        let master = key(7);
        let plaintext = b"written by a build from the future".to_vec();
        let sealed = seal_segment_bytes_with_prefix(
            &plaintext,
            &master,
            future_prefix(&SEGMENT_MAGIC, 9, MIN_READER_INDEX_FORMAT_VERSION),
        )
        .expect("a future build seals its segment");

        let opened = open_segment_bytes(&sealed, &master)
            .expect("a newer-ADDITIVE segment must still open (I2 backward-compat)");
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn newer_additive_master_direct_opens_under_this_build() {
        let master = key(8);
        let plaintext = b"a future manifest".to_vec();
        let sealed = seal_under_master_with_prefix(
            &plaintext,
            &master,
            future_prefix(&MASTER_DIRECT_MAGIC, 9, MIN_READER_INDEX_FORMAT_VERSION),
        )
        .expect("a future build seals its manifest");

        assert_eq!(
            open_under_master(&sealed, &master).expect("newer-additive must open"),
            plaintext
        );
    }

    /// The other half: a future build that raised the reader **floor** past us wrote a
    /// blob we must not touch. Refuse with the typed `Incompatible` — never the generic
    /// `Crypto` a caller could mistake for "corrupt, recreate it" — and refuse *before*
    /// any decryption is attempted.
    #[test]
    fn newer_breaking_segment_refuses_with_the_typed_error() {
        // Relative to the shipped constant, so a format bump keeps this blob
        // genuinely in the future (a hardcoded 4 went stale at the v3 bump).
        let breaking = CURRENT_INDEX_FORMAT_VERSION + 1;
        let master = key(9);
        let sealed = seal_segment_bytes_with_prefix(
            b"a shape this build cannot parse",
            &master,
            future_prefix(&SEGMENT_MAGIC, breaking, breaking),
        )
        .expect("a future build seals its segment");

        let err =
            open_segment_bytes(&sealed, &master).expect_err("a newer-BREAKING segment must refuse");
        assert!(
            matches!(
                err,
                IndexError::Incompatible {
                    file_v,
                    file_min,
                    bin_v: CURRENT_INDEX_FORMAT_VERSION,
                } if file_v == breaking && file_min == breaking
            ),
            "must be the typed Incompatible, not a generic Crypto: {err}"
        );
    }

    #[test]
    fn newer_breaking_master_direct_refuses_with_the_typed_error() {
        // Relative to the shipped constant, like the segment twin above (a hardcoded
        // 4 went stale at the v4 bump).
        let breaking = CURRENT_INDEX_FORMAT_VERSION + 1;
        let master = key(10);
        let sealed = seal_under_master_with_prefix(
            b"a future manifest",
            &master,
            future_prefix(&MASTER_DIRECT_MAGIC, breaking, breaking),
        )
        .expect("seal");

        let err =
            open_under_master(&sealed, &master).expect_err("a newer-BREAKING blob must refuse");
        assert!(
            matches!(err, IndexError::Incompatible { .. }),
            "must be the typed Incompatible, not a generic Crypto: {err}"
        );
    }

    /// The AAD binding, stated as a property: the header prefix is authenticated, so an
    /// attacker who can rewrite at-rest bytes cannot relabel an authentic blob's version
    /// and have us decrypt its body under the wrong semantics (the version-rollback
    /// surface that tolerant readers create). Before the AAD, these bytes were
    /// unauthenticated.
    #[test]
    fn flipped_version_byte_is_caught_as_tampering() {
        let master = key(11);
        let mut sealed = seal_segment_bytes(b"intact body", &master).expect("seal");
        sealed[OFF_VERSION] = 9; // relabel v1 -> v9; floor untouched, so the verdict passes...

        let err = open_segment_bytes(&sealed, &master)
            .expect_err("a relabelled version byte must not open");
        // ...and the AEAD tag, computed over the *original* prefix, catches it.
        assert!(
            format!("{err}").contains("unwrap data key failed"),
            "the AAD must catch a relabelled prefix: {err}"
        );
    }

    /// Rewrap copies the prefix verbatim, so a rotation performed by an older build
    /// never restamps a newer segment's version down (§ 2.2) — and both tags, computed
    /// over that prefix, still verify afterwards.
    #[test]
    fn rewrap_preserves_a_newer_segments_stamp_and_body() {
        let old = key(12);
        let new = key(13);
        let plaintext = b"a future segment being rotated by an older build".to_vec();
        let sealed = seal_segment_bytes_with_prefix(
            &plaintext,
            &old,
            future_prefix(&SEGMENT_MAGIC, 9, MIN_READER_INDEX_FORMAT_VERSION),
        )
        .expect("seal");

        let rewrapped = rewrap_segment_master_key(&sealed, &old, &new).expect("rewrap");

        assert_eq!(
            rewrapped[..HEADER_PREFIX_LEN],
            sealed[..HEADER_PREFIX_LEN],
            "the prefix — and so the v9 stamp — must survive verbatim, never restamped down"
        );
        assert_eq!(rewrapped[OFF_VERSION], 9);
        assert_eq!(
            open_segment_bytes(&rewrapped, &new).expect("rewrapped blob still opens"),
            plaintext
        );
    }

    /// A blob whose reader-floor byte is `0` — the reserved zero a writer predating
    /// the scheme left there — is refused before any decryption, never read as a
    /// baseline. The pre-scheme normalization was retired by the compat-remnant sweep
    /// (`version-compatibility.md` § Dimension 2, program 4).
    #[test]
    fn a_zero_reader_floor_byte_is_refused() {
        let master = key(14);
        let plaintext = b"stamped with the old reserved zero".to_vec();
        let sealed = seal_segment_bytes_with_prefix(
            &plaintext,
            &master,
            future_prefix(&SEGMENT_MAGIC, 1, 0), // min_reader = 0: the old reserved zero
        )
        .expect("seal");

        assert!(
            matches!(
                open_segment_bytes(&sealed, &master),
                Err(IndexError::SchemaMismatch(_))
            ),
            "an unstamped blob must be refused"
        );
    }
}
