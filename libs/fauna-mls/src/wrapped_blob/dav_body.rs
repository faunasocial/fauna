//! The **DAV resource-body seal** — one owner for the scheme CardDAV cards and
//! CalDAV events (and the Fauna extension sidecar riding an event) are stored
//! under at rest.
//!
//! A body is sealed as a single [`MailRecordEnvelope`] to the actor's *own*
//! `credential_id="default"` recipient key derived from `msek`
//! (the `fauna.state.mail` MSEK) — exactly what the MDA's Go side does on a DAV PUT
//! (`internal/mda/{carddav,caldav}/put.go` → `mailfauna.EncryptToRecipient` /
//! `EncryptToRecipientHybrid`). Same MSEK ⇒ same recipient keypair, so bytes
//! written by a Fauna app and by the MDA are interchangeable in both directions.
//! Classical (`DHKEM(X25519,HKDF-SHA256)`) and post-quantum X-Wing
//! (ML-KEM-768 ∥ X25519) seals both exist; the envelope self-describes its
//! suite, so [`unseal_dav_body`] opens either and a body rides the same
//! post-quantum migration as a mail record (`post-quantum.md` § Capability
//! negotiation picks the seal side).
//!
//! ⚠ **Why it lives here and not in a DAV crate.** Until 2026-08-23 it lived in
//! *both*: `fauna_client_carddav::{seal_card_body, seal_card_body_xwing,
//! unseal_card_body}` and `fauna_client_caldav::{seal_event_body,
//! seal_event_body_xwing, unseal_event_body}` were three byte-identical pairs,
//! plus two byte-identical `SealError` enums — six functions and two types for
//! one scheme, in two crates that both already depended on this module for
//! every primitive they called. Neither crate could own it without the other
//! depending on it, and the identity is not a coincidence to be maintained by
//! hand: the whole contract is that a card body and an event body are sealed the
//! *same* way as a mail record, by the same key, so that the Go MDA needs one
//! implementation rather than three. The two crates keep their domain-named
//! aliases; the scheme has one home.
//!
//! Found by the containment arm of the dev-fleet near-duplicate-function
//! scanner (`seal_card_body` ↔ `seal_event_body`, cont 0.413).

use zeroize::Zeroizing;

use super::{
    MailRecordEnvelope, mls_snapshot_plaintext::derive_recipient_hpke_keypair,
    mls_snapshot_plaintext::derive_recipient_xwing_keypair, seal_to_recipient,
    seal_to_recipient_xwing, unseal_mail_record_hybrid,
};

/// Seal/unseal failure for the DAV resource-body crypto layer.
///
/// ⚠ **One rendered spelling, deliberately, and it is the one behavioural
/// change in the 2026-08-23 unification** — the two predecessor enums rendered
/// `"seal card body: …"` and `"seal event body: …"`, and this renders
/// `"seal DAV body: …"` for both. Taken rather than kept because keeping the
/// distinction means keeping two types plus two `From` impls to move between
/// them, which is the shape this lift exists to delete; and because nothing
/// reads the text — the noun is recoverable from the call site in every case
/// (grepped: no test, app, e2e fixture or i18n key matches either old string;
/// neither enum is `uniffi(flat_error)`, so no app renders it either).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealError {
    /// HPKE-seal or CBOR-encode failure (practically unreachable for valid input).
    Seal(String),
    /// Malformed envelope bytes, wrong `kind`, or HPKE-open failure (wrong key /
    /// tampered ciphertext).
    Unseal(String),
}

impl core::fmt::Display for SealError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Seal(m) => write!(f, "seal DAV body: {m}"),
            Self::Unseal(m) => write!(f, "unseal DAV body: {m}"),
        }
    }
}

impl std::error::Error for SealError {}

/// Seal a DAV resource body (a vCard, a VEVENT, a sidecar) with the classical
/// suite, producing the `encrypted_body` wire bytes.
pub fn seal_dav_body(plaintext: &[u8], msek: &[u8; 32]) -> Result<Vec<u8>, SealError> {
    let (_secret, pubkey) = derive_recipient_hpke_keypair(msek);
    let envelope =
        seal_to_recipient(plaintext, &pubkey).map_err(|e| SealError::Seal(e.to_string()))?;
    envelope
        .to_canonical_bytes()
        .map_err(|e| SealError::Seal(e.to_string()))
}

/// Post-quantum (X-Wing) sibling of [`seal_dav_body`]. Derives the actor's own
/// X-Wing keypair from `msek` and seals to it; because the key is derived
/// locally it is always FIPS-203 valid, so there is no untrusted-published-ek
/// seal error to degrade from — an error here is a genuine bug and is surfaced
/// rather than silently downgraded.
pub fn seal_dav_body_xwing(plaintext: &[u8], msek: &[u8; 32]) -> Result<Vec<u8>, SealError> {
    let pubkey = derive_recipient_xwing_keypair(msek).public;
    let envelope =
        seal_to_recipient_xwing(plaintext, &pubkey).map_err(|e| SealError::Seal(e.to_string()))?;
    envelope
        .to_canonical_bytes()
        .map_err(|e| SealError::Seal(e.to_string()))
}

/// Encode `val` as canonical dag-CBOR and seal it with the classical suite in
/// one step — the write-side counterpart of [`unseal_dav_body_typed`], and the
/// encode+seal pairing every typed DAV writer repeated (`seal_calendar_metadata`,
/// `seal_addressbook_metadata`, `seal_fauna_ext`; found by the containment arm
/// of the dev-fleet near-duplicate-function scanner, cont 0.448 caldav↔carddav).
pub fn seal_dav_body_typed<T: serde::Serialize>(
    val: &T,
    msek: &[u8; 32],
) -> Result<Vec<u8>, SealError> {
    let bytes = fauna_cbor::encode_canonical(val).map_err(|e| SealError::Seal(e.to_string()))?;
    seal_dav_body(&bytes, msek)
}

/// [`seal_dav_body_typed`], but sealing with the post-quantum X-Wing suite —
/// the write-side counterpart consumed by `seal_fauna_ext_xwing`.
pub fn seal_dav_body_typed_xwing<T: serde::Serialize>(
    val: &T,
    msek: &[u8; 32],
) -> Result<Vec<u8>, SealError> {
    let bytes = fauna_cbor::encode_canonical(val).map_err(|e| SealError::Seal(e.to_string()))?;
    seal_dav_body_xwing(&bytes, msek)
}

/// The actor's own DAV recipient keypair, derived once from `msek` and reused
/// across every body opened under it — the fix for the finding that
/// [`unseal_dav_body`] paid a fresh ML-KEM-768 keygen (plus the classical HPKE
/// derive) on *every call*, even though both are fully deterministic in the
/// 32-byte `msek` : a 200-row CardDAV/CalDAV list was 200
/// keygens to render once. A batch-opening caller (`list_calendars`,
/// `query_cards`, `query_events`, …) derives one of these and calls
/// [`Self::unseal`] per item instead of [`unseal_dav_body`] per item.
///
/// Threaded down from the caller rather than memoized behind a global cache —
/// this holds secret key material, and a process-lifetime `static` would need
/// its own zeroize-on-evict story for no benefit a short-lived, explicitly-
/// scoped value doesn't already give for free. The X-Wing secret half already
/// zeroizes on drop (`fauna_pq_kem::XWingSecretKey: ZeroizeOnDrop`); the
/// classical X25519 secret is `Zeroizing`-wrapped here to match (PQ-2's
/// discipline — `fauna_pq_kem`'s own secrets follow the same rule).
pub struct DavRecipientKeys {
    hpke_secret: Zeroizing<[u8; 32]>,
    xwing: fauna_pq_kem::XWingKeyPair,
}

impl DavRecipientKeys {
    /// Derive both halves once — the one ML-KEM-768 keygen (and classical HPKE
    /// derive) a whole batch of DAV bodies under this `msek` now pays, instead
    /// of one per body.
    pub fn derive(msek: &[u8; 32]) -> Self {
        #[cfg(test)]
        tests::DERIVE_CALLS.with(|c| c.set(c.get() + 1));
        let (secret, _pubkey) = derive_recipient_hpke_keypair(msek);
        Self {
            hpke_secret: Zeroizing::new(secret),
            xwing: derive_recipient_xwing_keypair(msek),
        }
    }

    /// Open one `encrypted_body` with the keys derived at construction — no
    /// re-derivation. Suite-dispatching, same as [`unseal_dav_body`]: the
    /// envelope names its own suite, so this serves the classical and X-Wing
    /// paths alike (the classical seal is still written live, e.g. the
    /// caldav index hint via `seal_event_body`).
    pub fn unseal(&self, encrypted_body: &[u8]) -> Result<Vec<u8>, SealError> {
        let mlkem_dk = *self.xwing.secret.mlkem_decaps_key();
        let envelope = MailRecordEnvelope::from_canonical_bytes(encrypted_body)
            .map_err(|e| SealError::Unseal(e.to_string()))?;
        unseal_mail_record_hybrid(&envelope, &self.hpke_secret, &mlkem_dk)
            .map_err(|e| SealError::Unseal(e.to_string()))
    }
}

/// Open an `encrypted_body` written by either seal above **or by the MDA** —
/// both derive the same recipient keypair from `msek`. Suite-dispatching: the
/// envelope names its own suite, so one reader serves the classical and X-Wing
/// paths (the classical seal is still written live, e.g. the caldav index
/// hint via `seal_event_body`).
///
/// A single-shot convenience over [`DavRecipientKeys`] — opening more than one
/// body under the same `msek`? Derive one [`DavRecipientKeys`] and call
/// [`DavRecipientKeys::unseal`] per item instead of calling this in a loop,
/// or every item pays its own keygen again .
pub fn unseal_dav_body(encrypted_body: &[u8], msek: &[u8; 32]) -> Result<Vec<u8>, SealError> {
    DavRecipientKeys::derive(msek).unseal(encrypted_body)
}

/// Open a `sealed` DAV body and strict-decode it as `T` in one step — the tail
/// every typed DAV reader repeats (`unseal_calendar_metadata`,
/// `unseal_addressbook_metadata`, `unseal_fauna_ext`; found by the containment
/// arm of the dev-fleet near-duplicate-function scanner, cont 0.690
/// caldav↔carddav). **Fail-closed on empty `sealed` bytes** — an empty
/// ciphertext needs no key to produce, so a reader that wants "empty means no
/// value stored yet" must opt in explicitly via
/// [`unseal_dav_body_typed_or_default`] rather than get it for free here
/// ( — a prior version of this fn defaulted on empty,
/// which turned the sidecar's fail-closed AEAD boundary fail-open).
pub fn unseal_dav_body_typed<T: serde::de::DeserializeOwned>(
    sealed: &[u8],
    keys: &DavRecipientKeys,
) -> Result<T, SealError> {
    let plaintext = keys.unseal(sealed)?;
    fauna_cbor::decode_strict(&plaintext).map_err(|e| SealError::Unseal(e.to_string()))
}

/// [`unseal_dav_body_typed`], but empty `sealed` bytes decode as `T::default()`
/// — "no metadata stored yet". For the two collection-metadata readers
/// (`unseal_calendar_metadata`, `unseal_addressbook_metadata`) whose
/// empty-means-unset default is deliberate and documented. **Not** for a
/// sidecar or any value where an empty ciphertext must not authenticate —
/// use [`unseal_dav_body_typed`] there.
pub fn unseal_dav_body_typed_or_default<T: serde::de::DeserializeOwned + Default>(
    sealed: &[u8],
    keys: &DavRecipientKeys,
) -> Result<T, SealError> {
    if sealed.is_empty() {
        return Ok(T::default());
    }
    unseal_dav_body_typed(sealed, keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const MSEK: [u8; 32] = [7u8; 32];

    thread_local! {
        /// Counts [`DavRecipientKeys::derive`] calls on THIS test's own thread —
        /// `cargo test` runs each `#[test]` fn on its own thread by default, so a
        /// thread-local (rather than a process-global atomic) keeps concurrently
        /// running sibling tests from perturbing this count. Test-only
        /// instrumentation (`#[cfg(test)]`), invisible to production code and to
        /// every other crate — the count-pin lives here, next to the type it
        /// counts, rather than as cross-crate instrumentation.
        pub(super) static DERIVE_CALLS: Cell<usize> = const { Cell::new(0) };
    }

    /// The whole point of [`DavRecipientKeys`] : N opens under one
    /// derived keypair pay exactly one keygen, not N — pinned by count, never by
    /// wall-clock (convention 14). The comparison arm proves the counter itself
    /// is live: calling the single-shot [`unseal_dav_body`] in a loop DOES
    /// re-derive every time, which is the exact per-item cost this type exists
    /// to eliminate for a batch caller.
    #[test]
    fn n_opens_under_one_derivation_cost_one_keygen() {
        let body = b"BEGIN:VCARD\r\nFN:Ada\r\nEND:VCARD\r\n";
        let sealed: Vec<Vec<u8>> = (0..5)
            .map(|_| seal_dav_body(body, &MSEK).unwrap())
            .collect();

        DERIVE_CALLS.with(|c| c.set(0));
        let keys = DavRecipientKeys::derive(&MSEK);
        for s in &sealed {
            assert_eq!(keys.unseal(s).expect("open"), body);
        }
        assert_eq!(
            DERIVE_CALLS.with(Cell::get),
            1,
            "opening 5 bodies under one DavRecipientKeys must derive exactly once"
        );

        DERIVE_CALLS.with(|c| c.set(0));
        for s in &sealed {
            assert_eq!(unseal_dav_body(s, &MSEK).expect("open"), body);
        }
        assert_eq!(
            DERIVE_CALLS.with(Cell::get),
            sealed.len(),
            "the single-shot convenience re-derives per call by design — this is \
             the cost a batch caller must avoid by switching to DavRecipientKeys"
        );
    }

    /// Both suites round-trip, and — the part that mattered when this was two
    /// copies — **one reader opens both**, which is what lets a classical body
    /// (still written live) and a hybrid body both open.
    #[test]
    fn both_suites_round_trip_through_one_reader() {
        let body = b"BEGIN:VCARD\r\nFN:Ada\r\nEND:VCARD\r\n";
        for sealed in [
            seal_dav_body(body, &MSEK).expect("classical seal"),
            seal_dav_body_xwing(body, &MSEK).expect("xwing seal"),
        ] {
            assert_eq!(unseal_dav_body(&sealed, &MSEK).expect("open"), body);
        }
    }

    /// A different MSEK derives a different recipient key, so the open fails
    /// rather than returning something. Pinned because "wrong key" and
    /// "tampered ciphertext" share the [`SealError::Unseal`] arm on purpose —
    /// the caller must not be able to tell them apart.
    #[test]
    fn a_wrong_msek_cannot_open_a_body() {
        let sealed = seal_dav_body(b"secret", &MSEK).expect("seal");
        let other = [9u8; 32];
        assert!(matches!(
            unseal_dav_body(&sealed, &other),
            Err(SealError::Unseal(_))
        ));
    }
}
