//! ATProto delegated-authoring cert minting — the client half of D10
//! (`docs/goal/behavior/atproto-pds-full.md` § D10 → *Mint ceremony*).
//!
//! An external ATProto app posts through the account's **authoring sub-key
//! `K`**, which the nest holds and unwraps per write. What authorizes `K` is a
//! `DeviceAuthorization` cert signed by the account's **identity key** — and
//! the identity key is client-only, so the cert can only be minted here.
//! That is the whole reason this function exists client-side rather than as a
//! nest convenience: a nest that could mint its own authorization would be
//! self-granting, and D10's fail-closed chain would prove nothing.
//!
//! The ceremony is three calls, in this order:
//!
//! 1. `fauna.bridges.atproto.fetch_authoring_key` → `k_pub` (mint-on-read,
//!    first-write-wins — the nest generates `K` on the first fetch and every
//!    later fetch returns that same key).
//! 2. [`build_authoring_delegation_cert`] — **here**, offline, under the
//!    identity key.
//! 3. `fauna.bridges.atproto.provision_authoring_delegation` → the nest runs
//!    the five-check verify (`bins/fauna-nest/src/atproto_authoring_key.rs`
//!    `verify_delegation_cert`) and stores the cert beside `K`.
//!
//! Shared Rust rather than per-app (priority #2): all 7 apps mint the
//! identical cert, and the five checks it must satisfy are exactly the kind of
//! detail that drifts when six apps each re-implement it.

use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
use fauna_core::encoding::{
    EmbedAsBytes, canonical_decode, canonical_encode, decode_signed_bytes, sign_envelope,
    verify_envelope,
};
use fauna_core::identity::{ActorId, ActorKeypair};

/// How long a freshly minted authoring delegation lasts: ~90 days, in
/// **seconds**.
///
/// A hard-coded Rust constant with **no configuration surface and no
/// user-facing chooser** — the shipped capability-grant shape
/// (`docs/goal/ui/nests.md` § Expiry / renewal, whose
/// `fauna_client_capabilities::DEFAULT_GRANT_WINDOW_SECS` is this same 90 days
/// for the same reason). `docs/goal/principles.md` requires capability grants
/// be *time-bounded*; it does not require the user to pick the bound, and no
/// surface in Fauna renders a duration chooser today
/// (`view_model::MintOptionModel` offers use-case + holder, never a duration).
/// So a delegation is finite by construction rather than by a decision the
/// user has to make correctly.
pub const DELEGATION_WINDOW_SECS: u64 = 90 * 24 * 60 * 60;

/// A delegation reads **"expiring soon"** once it is within this of its
/// expiry — ~14 days, matching `view_model::RENEW_AHEAD_SECS`.
///
/// The point is that a lapse is never a *surprise*: the status row warns
/// before external apps stop being able to post, so a lapsed delegation reads
/// as "authorization expired — re-authorize here" rather than as silent
/// feature loss (the bar `nests.md` § Expiry / renewal sets for grant rows).
pub const DELEGATION_RENEW_AHEAD_SECS: u64 = 14 * 24 * 60 * 60;

/// A stored delegation's liveness relative to `now` — the states the ATProto
/// page's delegation row renders.
///
/// Deliberately the same three-state shape as
/// `fauna_client_capabilities::view_model::GrantLiveness`, minus its
/// `AutoRenewing` variant: that one marks a grant the *minting client* renews
/// in the background, and nothing re-mints a D10 delegation today (re-minting
/// needs the identity key, so it can only happen while the user's client is
/// running and unlocked). It is a separate type rather than a shared one
/// because the two live in different domains and different units — that one
/// folds content-grant events in **seconds**, this one reads a
/// `DeviceAuthorization` in **microseconds** — and sharing it would pull
/// `fauna-mls` into this deliberately thin, wasm-clean crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationLiveness {
    /// Inside its window, comfortably ahead of the renew-ahead threshold.
    Active,
    /// Within [`DELEGATION_RENEW_AHEAD_SECS`] of expiry — warn the user that
    /// external apps will stop being able to post unless they re-authorize.
    ExpiringSoon,
    /// `now >= expires_at`. External apps can no longer author: the nest's
    /// authoring-time wall-clock gate refuses with the D6 *fauna-surface*
    /// sub-type (D10 § Expiry, point (b)). Re-authorizing is the recovery,
    /// and the row stays visible so there is something to act on.
    Expired,
    /// The cert carries no `expires_at` at all — nothing this client minted,
    /// since [`build_authoring_delegation_cert`] is always called with a
    /// window, but representable because the wire allows it and an older or
    /// third-party-minted cert may lack one. Renders as a standing grant.
    NeverExpires,
}

impl DelegationLiveness {
    /// The stable wire spelling clients carry on their page snapshots. Kept
    /// here beside the variants so the string set cannot drift from the enum.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::ExpiringSoon => "expiring_soon",
            Self::Expired => "expired",
            Self::NeverExpires => "never_expires",
        }
    }
}

/// Classify a delegation's expiry against `now`. Both arguments are
/// **[`Timestamp`], i.e. epoch MICROSECONDS** — the unit `DeviceAuthorization`
/// stores and the unit the nest's own gate compares in. The thresholds above
/// are in seconds and are scaled here, once, rather than at each call site:
/// mixing the two units is the exact bug that made the nest accept expired
/// certs until 2026-07-29 (D10 § Expiry, the ⚠ note).
pub fn delegation_liveness(expires_at: Option<Timestamp>, now: Timestamp) -> DelegationLiveness {
    let Some(exp) = expires_at else {
        return DelegationLiveness::NeverExpires;
    };
    if now.0 >= exp.0 {
        return DelegationLiveness::Expired;
    }
    let remaining_micros = exp.0.saturating_sub(now.0);
    if remaining_micros <= DELEGATION_RENEW_AHEAD_SECS.saturating_mul(1_000_000) {
        DelegationLiveness::ExpiringSoon
    } else {
        DelegationLiveness::Active
    }
}

/// What the page needs to render about the account's current delegation,
/// recovered from the stored cert bytes rather than from anything the nest
/// asserts about them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationSummary {
    /// The sub-key this delegation authorizes.
    pub device_key: [u8; 32],
    /// The capabilities granted — a subset of [`AUTHORING_CAPABILITIES`].
    pub capabilities: Vec<Capability>,
    /// When the user authorized it.
    pub created_at: Timestamp,
    /// When it lapses; `None` for a standing grant.
    pub expires_at: Option<Timestamp>,
}

/// Why a stored delegation cert could not be trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationParseError {
    /// The bytes are not a well-formed embed-as-bytes `DeviceAuthorization`.
    Malformed(String),
    /// The envelope does not verify under the account's identity key, or the
    /// cert names a different account as grantor. Either way **this account
    /// did not sign it** — the one check that makes the read trustworthy
    /// rather than a restatement of whatever the nest chose to serve.
    NotSignedByThisAccount,
}

impl std::fmt::Display for DelegationParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "the stored delegation cert is malformed: {e}"),
            Self::NotSignedByThisAccount => {
                f.write_str("the stored delegation cert was not signed by this account")
            }
        }
    }
}

impl std::error::Error for DelegationParseError {}

/// Decode a stored delegation cert and **verify this account signed it**.
///
/// The nest serves the cert bytes verbatim (`fetch_authoring_delegation`), so
/// this is where they become trustworthy: the envelope must verify under
/// `identity`'s own key and name `identity` as grantor. A nest that swapped in
/// a cert authorizing some other sub-key is therefore detectable on the client
/// — which matters because that cert is exactly what would let a substituted
/// `K` author posts as this user.
pub fn parse_delegation_cert(
    cert_bytes: &[u8],
    identity: &ActorId,
) -> Result<DelegationSummary, DelegationParseError> {
    let wire: EmbedAsBytes = canonical_decode(cert_bytes)
        .map_err(|e| DelegationParseError::Malformed(format!("{e:?}")))?;
    let (bytes, env) = wire
        .into_signed()
        .map_err(|e| DelegationParseError::Malformed(format!("{e:?}")))?;
    let cert: DeviceAuthorization = decode_signed_bytes(&bytes)
        .map_err(|e| DelegationParseError::Malformed(format!("{e:?}")))?;
    if cert.actor_id != *identity {
        return Err(DelegationParseError::NotSignedByThisAccount);
    }
    verify_envelope(&cert, &bytes, &env)
        .map_err(|_| DelegationParseError::NotSignedByThisAccount)?;
    Ok(DelegationSummary {
        device_key: cert.device_key,
        capabilities: cert.capabilities,
        created_at: cert.created_at,
        expires_at: cert.expires_at,
    })
}

/// The capabilities a D10 authoring delegation may carry, in the order the
/// hosted-enable ceremony grants them.
///
/// This is the client-side twin of the nest's `is_authoring_capability` gate
/// (check 4, *scope minimalism*): a cert naming anything outside this set is
/// refused at `provision_authoring_delegation`, so minting one would only
/// produce a round-trip the user cannot complete. `Post` covers the write
/// surface F2.2 serves today; `UpdateProfile` is what F2.3's `putRecord`
/// profile arm will ride.
pub const AUTHORING_CAPABILITIES: [Capability; 2] = [Capability::Post, Capability::UpdateProfile];

/// Errors minting a delegation cert. Deliberately small — every input is
/// client-supplied and checkable before any signing happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationCertError {
    /// `capabilities` was empty, or named something outside
    /// [`AUTHORING_CAPABILITIES`]. Either way the nest's check 4 would refuse
    /// the cert, so it is refused here rather than minted to be rejected.
    UnsupportedCapabilities,
    /// `expires_at` is at or before `created_at` — a cert that is already
    /// expired the instant it is signed.
    AlreadyExpired,
    /// Canonical encoding or signing failed (a `fauna-core` fault, not a
    /// caller error).
    Encoding(String),
}

impl std::fmt::Display for DelegationCertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedCapabilities => f.write_str(
                "an authoring delegation must grant at least one of Post / UpdateProfile, and nothing else",
            ),
            Self::AlreadyExpired => {
                f.write_str("the delegation would expire at or before it was created")
            }
            Self::Encoding(e) => write!(f, "encoding the delegation cert failed: {e}"),
        }
    }
}

impl std::error::Error for DelegationCertError {}

/// Mint the identity-signed delegation cert authorizing the account's
/// authoring sub-key `K` to author on its behalf.
///
/// Returns the cert's **canonical embed-as-bytes wire** — exactly the bytes
/// `ProvisionAuthoringDelegationRequest.cert` carries, and exactly the bytes
/// the nest later re-embeds in each delegated post's
/// `EmbedAsBytes.signer_auth` so every reader can re-run the chain.
///
/// * `identity` — the account's identity keypair. Its public half becomes the
///   cert's `actor_id`, so the caller cannot mint a delegation for anyone else.
/// * `k_pub` — the 32-byte `k_pub` from `fetch_authoring_key`. Fetch it first:
///   the nest checks the cert names *that* key (check 3), so a cert minted
///   against a guessed or stale key is refused.
/// * `capabilities` — a non-empty subset of [`AUTHORING_CAPABILITIES`].
/// * `created_at` / `expires_at` — `Timestamp`, i.e. **epoch microseconds**
///   (`fauna_core::data::Timestamp`). Getting this unit wrong is not a cosmetic
///   slip: `verify_authoring_envelope` step 5 compares `expires_at` against the
///   signed post's own `created_at`, which is microseconds, so a cert minted in
///   milliseconds reads as having expired ~55 years ago and **every post it
///   authorizes is rejected at the ingest gate**. `None` mints a non-expiring
///   delegation, which stays revocable from the client at any time
///   (`revoke_authoring_delegation` destroys `K` itself).
pub fn build_authoring_delegation_cert(
    identity: &ActorKeypair,
    k_pub: [u8; 32],
    capabilities: &[Capability],
    created_at: Timestamp,
    expires_at: Option<Timestamp>,
) -> Result<Vec<u8>, DelegationCertError> {
    if capabilities.is_empty()
        || !capabilities
            .iter()
            .all(|c| AUTHORING_CAPABILITIES.contains(c))
    {
        return Err(DelegationCertError::UnsupportedCapabilities);
    }
    if let Some(exp) = expires_at
        && exp.0 <= created_at.0
    {
        return Err(DelegationCertError::AlreadyExpired);
    }

    let cert = DeviceAuthorization {
        actor_id: identity.actor_id(),
        device_key: k_pub,
        capabilities: capabilities.to_vec(),
        created_at,
        expires_at,
    };
    let (bytes, env) = sign_envelope(identity, &cert)
        .map_err(|e| DelegationCertError::Encoding(format!("{e:?}")))?;
    canonical_encode(&EmbedAsBytes::from_signed(bytes, env))
        .map_err(|e| DelegationCertError::Encoding(format!("{e:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::encoding::{canonical_decode, decode_signed_bytes, verify_envelope};

    fn identity() -> ActorKeypair {
        ActorKeypair::from_secret([9u8; 32])
    }

    /// The round-trip a nest-side `verify_delegation_cert` performs, reproduced
    /// here so this crate pins the wire shape without depending on the nest.
    fn parse(cert_bytes: &[u8]) -> DeviceAuthorization {
        let wire: EmbedAsBytes = canonical_decode(cert_bytes).expect("embed-as-bytes");
        let (bytes, env) = wire.into_signed().expect("split");
        let cert: DeviceAuthorization = decode_signed_bytes(&bytes).expect("decode");
        verify_envelope(&cert, &bytes, &env).expect("identity-signed");
        cert
    }

    #[test]
    fn mints_a_cert_the_five_nest_checks_accept() {
        let id = identity();
        let k_pub = [3u8; 32];
        let bytes = build_authoring_delegation_cert(
            &id,
            k_pub,
            &[Capability::Post],
            Timestamp(1_000),
            Some(Timestamp(9_000)),
        )
        .expect("mint");

        let cert = parse(&bytes);
        // 2. the grantor is the caller; 3. it names this account's own k_pub.
        assert_eq!(cert.actor_id, id.actor_id());
        assert_eq!(cert.device_key, k_pub);
        // 4. scope minimalism.
        assert_eq!(cert.capabilities, vec![Capability::Post]);
        // 5. not already expired.
        assert_eq!(cert.created_at, Timestamp(1_000));
        assert_eq!(cert.expires_at, Some(Timestamp(9_000)));
    }

    #[test]
    fn a_non_expiring_cert_carries_no_expiry() {
        let bytes = build_authoring_delegation_cert(
            &identity(),
            [4u8; 32],
            &AUTHORING_CAPABILITIES,
            Timestamp(1),
            None,
        )
        .expect("mint");
        let cert = parse(&bytes);
        assert_eq!(cert.expires_at, None);
        assert_eq!(cert.capabilities, AUTHORING_CAPABILITIES.to_vec());
    }

    #[test]
    fn refuses_capabilities_the_nest_would_reject() {
        for caps in [
            vec![],
            vec![Capability::All],
            vec![Capability::ManageSubscribers],
            // A supported capability does not launder an unsupported one.
            vec![Capability::Post, Capability::Follow],
        ] {
            assert_eq!(
                build_authoring_delegation_cert(&identity(), [5u8; 32], &caps, Timestamp(1), None),
                Err(DelegationCertError::UnsupportedCapabilities),
                "capabilities {caps:?} must be refused before signing",
            );
        }
    }

    #[test]
    fn refuses_a_cert_that_is_born_expired() {
        for exp in [Timestamp(500), Timestamp(1_000)] {
            assert_eq!(
                build_authoring_delegation_cert(
                    &identity(),
                    [6u8; 32],
                    &[Capability::Post],
                    Timestamp(1_000),
                    Some(exp),
                ),
                Err(DelegationCertError::AlreadyExpired),
            );
        }
    }

    /// The unit pin. `DELEGATION_RENEW_AHEAD_SECS` is in seconds and
    /// `Timestamp` is in microseconds; scaling by the wrong factor is the bug
    /// that made the nest's own expiry check dead until 2026-07-29, so the
    /// boundary is asserted at the exact microsecond rather than "roughly".
    #[test]
    fn liveness_thresholds_are_scaled_from_seconds_to_microseconds() {
        let renew_ahead_micros = DELEGATION_RENEW_AHEAD_SECS * 1_000_000;
        // A realistic epoch-microseconds instant (~Nov 2023): the thresholds
        // are months wide, so a toy timestamp would underflow before it could
        // exercise them.
        let exp = Timestamp(1_700_000_000_000_000);

        // Exactly at the threshold reads ExpiringSoon (inclusive), one
        // microsecond earlier is still Active.
        assert_eq!(
            delegation_liveness(Some(exp), Timestamp(exp.0 - renew_ahead_micros)),
            DelegationLiveness::ExpiringSoon,
        );
        assert_eq!(
            delegation_liveness(Some(exp), Timestamp(exp.0 - renew_ahead_micros - 1)),
            DelegationLiveness::Active,
        );

        // A full default window out is comfortably Active — this is what fails
        // loudly if the two constants are ever compared in mismatched units.
        let window_micros = DELEGATION_WINDOW_SECS * 1_000_000;
        assert_eq!(
            delegation_liveness(Some(exp), Timestamp(exp.0 - window_micros)),
            DelegationLiveness::Active,
        );
    }

    #[test]
    fn liveness_covers_expiry_and_the_standing_case() {
        let exp = Timestamp(1_700_000_000_000_000);
        // At expiry and past it both read Expired — `now >= expires_at`
        // matches the nest's own authoring-time gate.
        assert_eq!(
            delegation_liveness(Some(exp), exp),
            DelegationLiveness::Expired
        );
        assert_eq!(
            delegation_liveness(Some(exp), Timestamp(exp.0 + 1)),
            DelegationLiveness::Expired
        );
        assert_eq!(
            delegation_liveness(None, exp),
            DelegationLiveness::NeverExpires
        );
    }

    /// The round trip the status row actually performs: mint under the default
    /// window, then read it back the way the page will.
    #[test]
    fn a_minted_cert_parses_back_into_the_row_the_page_renders() {
        let id = identity();
        let k_pub = [3u8; 32];
        let created = Timestamp(1_700_000_000_000_000);
        let expires = Timestamp(created.0 + DELEGATION_WINDOW_SECS * 1_000_000);

        let bytes = build_authoring_delegation_cert(
            &id,
            k_pub,
            &AUTHORING_CAPABILITIES,
            created,
            Some(expires),
        )
        .expect("mint");

        let summary = parse_delegation_cert(&bytes, &id.actor_id()).expect("parse");
        assert_eq!(summary.device_key, k_pub);
        assert_eq!(summary.capabilities, AUTHORING_CAPABILITIES.to_vec());
        assert_eq!(summary.created_at, created);
        assert_eq!(summary.expires_at, Some(expires));
        // Freshly minted, so comfortably active.
        assert_eq!(
            delegation_liveness(summary.expires_at, created),
            DelegationLiveness::Active,
        );
    }

    /// The check that makes the status read trustworthy rather than a
    /// restatement of whatever the nest served: a cert some *other* identity
    /// signed is refused, even though it is perfectly well-formed and names
    /// this account's own `k_pub`.
    #[test]
    fn a_cert_this_account_did_not_sign_is_refused() {
        let mine = identity();
        let theirs = ActorKeypair::from_secret([77u8; 32]);
        let bytes = build_authoring_delegation_cert(
            &theirs,
            [3u8; 32],
            &[Capability::Post],
            Timestamp(1),
            None,
        )
        .expect("mint");

        assert_eq!(
            parse_delegation_cert(&bytes, &mine.actor_id()),
            Err(DelegationParseError::NotSignedByThisAccount),
        );
        // ...and the control arm: the same bytes parse for their real signer,
        // so the refusal above is about the binding, not a broken fixture.
        assert!(parse_delegation_cert(&bytes, &theirs.actor_id()).is_ok());
    }

    #[test]
    fn malformed_cert_bytes_are_refused_rather_than_panicking() {
        let id = identity();
        for bad in [vec![], vec![0xff; 8], b"not a cert at all".to_vec()] {
            assert!(matches!(
                parse_delegation_cert(&bad, &id.actor_id()),
                Err(DelegationParseError::Malformed(_))
            ));
        }
    }

    #[test]
    fn the_cert_binds_to_its_signer_not_to_a_claimed_actor_id() {
        // Minting under a different identity produces a different actor_id —
        // there is no parameter that lets a caller name someone else as the
        // grantor, which is what makes the nest's check 2 unfalsifiable.
        let a = build_authoring_delegation_cert(
            &ActorKeypair::from_secret([1u8; 32]),
            [7u8; 32],
            &[Capability::Post],
            Timestamp(1),
            None,
        )
        .expect("mint a");
        let b = build_authoring_delegation_cert(
            &ActorKeypair::from_secret([2u8; 32]),
            [7u8; 32],
            &[Capability::Post],
            Timestamp(1),
            None,
        )
        .expect("mint b");
        assert_ne!(parse(&a).actor_id, parse(&b).actor_id);
    }
}
