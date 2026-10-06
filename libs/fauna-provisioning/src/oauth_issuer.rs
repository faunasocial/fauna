//! The nest's OAuth **issuer signing key** — ES256 keygen and the JOSE
//! primitives its key set needs (feature `oauth-issuer`), plus the
//! **token-lifetime and key-retirement-horizon constants**, which are ungated.
//!
//! `docs/goal/architecture/key-material-hierarchy.md` § Audience: deployment
//! infrastructure → *Issuer signing key* rules that the nest mints a fresh
//! random ES256 keypair, seals its private half under a domain-separated
//! sibling of the nest-internal key-encryption key, and serves the public half
//! at `/oauth/jwks` as a key **set** with `kid`s.
//!
//! # Why this is its own feature and not part of `atproto-seal`
//!
//! The issuer is TP5's whole point:
//! `docs/goal/behavior/authorization-server.md` § The issuer rules that the
//! authorization server "is up whenever the nest is up" and that custody moves
//! precisely so "the issuer no longer depends on an optional bridge being
//! enrolled and approved" (`key-material-hierarchy.md` § *Issuer signing key* →
//! *Audience justification*). The nest happens to enable `atproto-seal`
//! unconditionally today, so putting the mint there would compile in every
//! flavor — but it would do so **by accident of one Cargo line**, and the
//! property the goal doc states is structural. A separate feature makes the
//! independence something the build system holds rather than something a future
//! edit can quietly remove.
//!
//! The RFC 7638 thumbprint lives here for the same reason and is re-exported
//! from [`crate::atproto`], where it was first written: it is a JOSE primitive
//! over a P-256 key, not an ATProto one, and the Go bridge's `ecThumbprint`
//! parity fixtures keep resolving through the re-export unchanged.
//!
//! # Why the constants are NOT behind that feature
//!
//! [`ACCESS_TOKEN_LIFETIME_SECS`] and the horizon derived from it are policy
//! integers: nothing reads them with a curve. Their consumers include builds
//! that mint no keys at all — the Go bridge reaches them through fauna-ffi,
//! which every app binding links — so gating them would drag `p256` into those
//! artifacts to read two numbers. The items that genuinely need the curve carry
//! `#[cfg(feature = "oauth-issuer")]` one at a time instead.

// The key plane below is `oauth-issuer`-gated item by item, while the module
// itself is not: the horizon constants are policy integers no curve is needed
// to read (`crate::lib`'s note at the `pub mod` line says why).
#[cfg(feature = "oauth-issuer")]
use rand::rngs::OsRng;

#[cfg(feature = "oauth-issuer")]
use crate::error::ProvisionError;

/// A freshly minted issuer signing key, ready for the nest to seal and store.
///
/// The secret is the raw 32-byte P-256 scalar rather than PKCS#8 DER, because
/// the nest wraps it through `nest_kek::wrap_32` — the fixed-size construction
/// every 32-byte member of the key-encryption-key family shares. PKCS#8 would
/// only earn its self-description if the secret rode an HPKE-sealed blob to
/// another process, which this key never does.
#[cfg(feature = "oauth-issuer")]
#[derive(Clone)]
pub struct MintedIssuerKey {
    /// The raw P-256 private scalar, big-endian — `wrap_32`'s input.
    pub secret_scalar: [u8; 32],
    /// RFC 7638 JWK thumbprint of the public half: this key's `kid`, stamped
    /// into every token it signs and advertised in the JWKS.
    pub kid: String,
    /// Base64url (no padding) X coordinate of the public half.
    pub x: String,
    /// Base64url (no padding) Y coordinate of the public half.
    pub y: String,
}

/// How long an OAuth access token this issuer mints stays valid, in seconds.
///
/// **This constant's single owner.** It was a Go literal
/// (`internal/atprotopds/oauth_token.go`'s `OAuthAccessTokenLifetime`) while
/// the authorization server was bridge-hosted, and
/// `docs/goal/behavior/authorization-server.md` § As built recorded the
/// consequence: the key-retirement horizon had to be derived bridge-side
/// "because the lifetime constant lives bridge-side and nest must not duplicate
/// it". Nest custody of the key (§ The issuer) makes the nest a second
/// consumer, so the number moved *up* into shared Rust rather than being copied
/// *across* — a nest-side copy is exactly the drift that ruling was avoiding.
/// Since the bridge's own authorization server retired, the nest is its only
/// reader.
///
/// **Why 15 minutes and not 30.** OAuth 2.1 caps access tokens at 15 minutes
/// where the authorization server cannot revoke an individual access token, and
/// under 30 where it can. This issuer ships the grant *row* and `/oauth/revoke`
/// (RFC 7009) revokes a grant *family*, but there is still no per-call grant
/// check, so a token minted here genuinely cannot be killed before it expires
/// and the stricter cap is the honest one. Widening to under 30 minutes is
/// additive for the slice that adds that check, and must not happen before it.
pub const ACCESS_TOKEN_LIFETIME_SECS: u64 = 15 * 60;

/// How long ONE refresh token this issuer mints stays valid, in seconds — 180
/// days, the cap OAuth 2.1 sets for a confidential client's individual refresh
/// token.
///
/// **This constant's single owner**, joining the access lifetime above for the
/// same reason it moved: the token mint had two implementations during the
/// issuer's move onto the nest, and two literals is how they come to disagree
/// about how long a client stays signed in.
///
/// It bounds one *token*, not a *session*: a confidential client's session is
/// unlimited while each of its refresh tokens is bounded, because rotation
/// re-mints. A public client's session is additionally capped absolutely by
/// [`PUBLIC_SESSION_LIFETIME_SECS`], which is the shorter of the two in
/// practice.
pub const REFRESH_TOKEN_LIFETIME_SECS: u64 = 180 * 24 * 60 * 60;

/// The ABSOLUTE deadline on a **public** client's grant, in seconds — 14 days
/// from consent, carried across every rotation so refreshing cannot extend it.
///
/// **This constant's single owner**, for the reason above. The Go bridge reads
/// it through `fauna_ffi::oauth_public_session_lifetime_secs`.
///
/// A public client holds no credential of its own — PKCE and DPoP are all that
/// bind its tokens — so its session is time-boxed where a confidential
/// client's is not. The deadline rides the refresh token as its `sexp` claim,
/// and a rotation *clamps* the new token's expiry to it rather than ignoring
/// it: clamping is what keeps a public client's last refresh token from
/// outliving the session it belongs to.
pub const PUBLIC_SESSION_LIFETIME_SECS: u64 = 14 * 24 * 60 * 60;

// A public client's whole session must not outlast one of its refresh tokens'
// own cap, or the clamp above would be the only thing bounding it and the two
// numbers would be describing different systems. Pinned at COMPILE time beside
// the horizon relation below, for the same reason.
const _: () = assert!(PUBLIC_SESSION_LIFETIME_SECS < REFRESH_TOKEN_LIFETIME_SECS);

/// The pad between an access token's expiry and dropping the key that signed
/// it, in seconds.
///
/// A verifier reads the JWKS through a cache, so a key can still be *needed*
/// for a little after the last token it signed expires — and the cost of the
/// two errors is wildly asymmetric: padding too long leaves a retired public
/// key served (it signs nothing; a public key is not a secret), while padding
/// too short breaks live tokens at exactly the clients an admin was told
/// rotation was safe for.
pub const KEY_ROTATION_PICKUP_GRACE_SECS: u64 = 5 * 60;

/// How long a retired issuer key must stay in the served key set before it may
/// be dropped, in seconds — the "access-token horizon" of
/// `docs/goal/architecture/key-material-hierarchy.md` § Audience: deployment
/// infrastructure → *Issuer signing key*.
///
/// Derived from the two constants above rather than written out, so the sum can
/// never disagree with its parts: a slice that widens the token lifetime widens
/// the horizon with it, which is the invariant — a key must outlive everything
/// it signed.
#[must_use]
pub const fn issuer_key_retirement_horizon_secs() -> u64 {
    ACCESS_TOKEN_LIFETIME_SECS + KEY_ROTATION_PICKUP_GRACE_SECS
}

// A retired key must outlive everything it signed — the one relation between
// these three numbers that is not a preference. Pinned at COMPILE time, like
// the reader/writer version floors elsewhere in the workspace: a slice that
// widened the token lifetime past the horizon should fail to build, not fail a
// test somebody could read as flaky.
const _: () = assert!(issuer_key_retirement_horizon_secs() > ACCESS_TOKEN_LIFETIME_SECS);

// OAuth 2.1 caps an access token at 15 minutes while the authorization server
// cannot revoke an individual one, which is this issuer's state until the grant
// registry becomes a revocation mechanism. Widening past that is a deliberate
// edit HERE, with the doc comment above rewritten to say what made it legal.
const _: () = assert!(ACCESS_TOKEN_LIFETIME_SECS <= 15 * 60);

/// Mint a fresh ES256 issuer signing key.
///
/// # Errors
///
/// Returns `ProvisionError::Other` if the generated key's public half is not a
/// well-formed uncompressed point — impossible for a key this function just
/// generated, surfaced as an error rather than a panic because this runs inside
/// the nest's request path.
#[cfg(feature = "oauth-issuer")]
pub fn mint_issuer_key() -> Result<MintedIssuerKey, ProvisionError> {
    let signing_key = p256::ecdsa::SigningKey::random(&mut OsRng);
    let (x, y) = public_coordinates(signing_key.verifying_key())?;
    Ok(MintedIssuerKey {
        secret_scalar: signing_key.to_bytes().into(),
        kid: rfc7638_p256_thumbprint(signing_key.verifying_key()),
        x,
        y,
    })
}

/// The base64url coordinates of a P-256 public key, as JWK `x`/`y`.
///
/// Fallible where [`rfc7638_p256_thumbprint`] panics — not because this is
/// reached from a JWKS read path (it isn't: `/oauth/jwks` serves the `x`/`y`
/// strings stored at mint time verbatim, never re-deriving them), but because
/// its only **production** caller is [`mint_issuer_key`], and an encoding
/// failure there — a P-256 point with no affine coordinates, which a
/// well-formed random key never produces but this function cannot rule out by
/// type — must surface as an error the caller can report rather than panic
/// mid-mint. (Two `#[cfg(test)]` fixture-key helpers call it too.)
#[cfg(feature = "oauth-issuer")]
pub fn public_coordinates(
    verifying_key: &p256::ecdsa::VerifyingKey,
) -> Result<(String, String), ProvisionError> {
    use base64::Engine as _;

    let point = verifying_key.to_encoded_point(false);
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let x = point
        .x()
        .ok_or_else(|| ProvisionError::Other("P-256 point has no x coordinate".into()))?;
    let y = point
        .y()
        .ok_or_else(|| ProvisionError::Other("P-256 point has no y coordinate".into()))?;
    Ok((b64.encode(x), b64.encode(y)))
}

/// RFC-7638 JWK thumbprint of a P-256 verifying key — the `kid` stamped into
/// every token this key signs and advertised in its JWKS.
///
/// The hash input is the canonical JSON of exactly `{crv, kty, x, y}` in
/// lexicographic member order, coordinates fixed-width 32-byte zero-padded
/// base64url — the same recipe the PDS bridge's `ecThumbprint`
/// (`internal/atprotopds/dpop.go`) uses for a DPoP key's `jkt`, which must
/// equal the `cnf.jkt` the nest binds into a token; the Go side is pinned
/// against the Rust validator in `oauth_dpop_crossbinary_test.go`.
///
/// Moved here from `atproto` when the nest-held issuer needed it outside that
/// module's feature; `atproto` re-exports it, so every existing caller and the
/// Go parity chain resolve unchanged.
#[cfg(feature = "oauth-issuer")]
pub fn rfc7638_p256_thumbprint(verifying_key: &p256::ecdsa::VerifyingKey) -> String {
    use base64::Engine as _;
    use sha2::{Digest as _, Sha256};

    let point = verifying_key.to_encoded_point(false);
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let x = b64.encode(point.x().expect("uncompressed P-256 point has x"));
    let y = b64.encode(point.y().expect("uncompressed P-256 point has y"));
    let canonical = format!(r#"{{"crv":"P-256","kty":"EC","x":"{x}","y":"{y}"}}"#);
    b64.encode(Sha256::digest(canonical.as_bytes()))
}

/// The key plane's tests, in their own feature-gated module rather than four
/// `#[cfg(feature)]` attributes inside the module below — the shape
/// [`crate::dkim`]'s `seal_tests` already uses, and the one the feature-gated
/// test-coverage gate recognises. Per-test attributes made these invisible to
/// it, which would have left them compiled only by whatever else happens to
/// enable `oauth-issuer`.
#[cfg(all(test, feature = "oauth-issuer"))]
mod key_plane_tests {
    use super::*;

    #[test]
    fn a_minted_issuer_key_is_a_usable_es256_signer() {
        // The secret must survive the 32-byte round trip `nest_kek::wrap_32`
        // will put it through, and come back as the SAME key — not merely as
        // some valid key. A mint that returned a scalar unrelated to the `kid`
        // it advertised would pass every "is it well-formed" check and mint
        // tokens no verifier could match to the JWKS.
        let minted = mint_issuer_key().unwrap();
        let restored = p256::ecdsa::SigningKey::from_bytes(&minted.secret_scalar.into())
            .expect("the minted scalar is a valid P-256 key");
        assert_eq!(
            rfc7638_p256_thumbprint(restored.verifying_key()),
            minted.kid
        );
        let (x, y) = public_coordinates(restored.verifying_key()).unwrap();
        assert_eq!((x, y), (minted.x, minted.y));
    }

    #[test]
    fn two_mints_are_different_keys() {
        // Rotation adds a key to the set; a mint that returned a deterministic
        // key would make rotation a no-op that still reported success.
        let a = mint_issuer_key().unwrap();
        let b = mint_issuer_key().unwrap();
        assert_ne!(a.kid, b.kid);
        assert_ne!(a.secret_scalar, b.secret_scalar);
    }

    #[test]
    fn the_thumbprint_is_the_one_atproto_exports() {
        // The move must be a re-export, not a copy: two thumbprint functions
        // over one recipe is exactly the drift the doc comment says the shared
        // constant exists to prevent, and the Go parity fixtures pin only one
        // of them.
        let key = p256::ecdsa::SigningKey::random(&mut OsRng);
        assert_eq!(
            crate::atproto::rfc7638_p256_thumbprint(key.verifying_key()),
            rfc7638_p256_thumbprint(key.verifying_key()),
        );
    }

    #[test]
    fn the_kid_is_stable_for_one_key_and_base64url_unpadded() {
        // `kid` is a JOSE header value and a JSON member name in the JWKS; a
        // padded or standard-alphabet encoding would be legal base64 and wrong
        // here, and nothing downstream would notice until a client failed to
        // match a token to a key.
        let key = p256::ecdsa::SigningKey::random(&mut OsRng);
        let kid = rfc7638_p256_thumbprint(key.verifying_key());
        assert_eq!(kid, rfc7638_p256_thumbprint(key.verifying_key()));
        assert!(!kid.contains('='), "{kid}");
        assert!(!kid.contains('+') && !kid.contains('/'), "{kid}");
    }
}

/// The horizon constants' tests — ungated, like the constants themselves, so a
/// build that compiles no curve still checks the numbers it hands the Go bridge.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pad_past_expiry_is_the_pickup_grace_and_nothing_else() {
        // The ORDERING (horizon > lifetime) and the 15-minute cap are pinned at
        // compile time beside the constants — a test could not be more binding
        // than a build failure. What remains worth asserting here is the
        // *composition*: the horizon is those two numbers and no third one, so
        // a future slice that pads it with an unnamed extra has to come here and
        // name it.
        assert_eq!(
            issuer_key_retirement_horizon_secs() - ACCESS_TOKEN_LIFETIME_SECS,
            KEY_ROTATION_PICKUP_GRACE_SECS,
        );
    }
}
