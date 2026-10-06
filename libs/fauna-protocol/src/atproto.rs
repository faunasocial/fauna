//! ATProto identity helpers — handle derivation and `did:key` encoding.
//!
//! The host-direction ATProto PDS derives a user's ATProto handle from their
//! Fauna handle (`alice@example.com` → `alice.example.com`, derived at READ
//! time, never frozen at mint — `docs/goal/behavior/atproto-pds-bridge.md`
//! § Identity) and encodes identity public keys as `did:key` strings for PLC
//! operations and DID documents. Both are pure, WASM-safe protocol facts shared
//! by the nest (DNS record matrix, `fauna.bridges.atproto.*` kinds) and, from
//! S4 on, the apps (enable-UX handle preview) — the same single-source
//! contract as [`crate::handle::validate_handle`].
//!
//! The `<handle>.<domain>` namespace is shared with per-user web hosting, so
//! derivation reuses the same reserved-label consumption gate
//! ([`fauna_core::web::is_reserved_subdomain_label`]): a handle equal to an
//! infra label (`mail`, `relay`, …) exists (the admin-claim ceremony may mint
//! one) but never derives an ATProto handle, exactly as it never serves a web
//! subdomain. The real-domain gate (`is_public_dns_name`) stays at the call
//! sites — clients may preview a derivation before the nest-side gate applies.
//!
//! Spec references (verified 2026-07-20): ATProto cryptography spec (did:key =
//! `z` + base58btc(multicodec ++ compressed SEC1 point); p256 multicodec
//! `0x1200` → varint `[0x80, 0x24]`; secp256k1 `0xE7` → varint `[0xE7, 0x01]`)
//! and did:plc spec v0.1 (rotation keys are p256/k256 did:key strings).

use fauna_core::web::{is_reserved_subdomain_label, normalize_dns_name};

use crate::handle::validate_handle;

/// How deep a user's Bluesky integration runs — the single ordered choice the
/// `atproto` page's depth selector writes (`docs/goal/ui/atproto.md` § Goal).
///
/// The ordering is load-bearing, not cosmetic: a jump across several rungs
/// composes the per-rung effects into one transition, and the *direction* of a
/// change selects which half of § Transition semantics applies (upward = mint /
/// reactivate / open the login plane; downward = suspend / deactivate, always
/// reversibly). The two hosted rungs share one Fauna-minted DID
/// ([`Self::is_hosted`]), and they are alternatives to [`Self::Linked`]'s
/// external account — never both at once (§ Don't do these, the one-backing
/// rule the transition kind enforces).
///
/// The wire strings are persisted in nest's
/// `atproto_account_settings.integration_level` and cross
/// `fauna.bridges.atproto.set_integration_level`, so they are at-rest + wire
/// compatibility surface: additive only, never renamed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntegrationLevel {
    /// No ATProto presence at all. The ratified default — publishing to a
    /// public third-party network is explicit opt-in consent.
    #[default]
    Off,
    /// Read/interact/cross-post through an existing *external* Bluesky account
    /// (the consume-side link); the identity lives on someone else's PDS.
    Linked,
    /// This nest mints and holds the DID; public posts project one-way and the
    /// network can read/follow/interact.
    HostedVisible,
    /// Additionally, third-party ATProto apps log in against this nest — the
    /// same hosted DID, with the login plane open.
    HostedFull,
}

impl IntegrationLevel {
    /// The persisted/wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Linked => "linked",
            Self::HostedVisible => "hosted_visible",
            Self::HostedFull => "hosted_full",
        }
    }

    /// Parse a persisted/wire spelling. `None` for anything unrecognized — an
    /// unknown level must stay *representable as a refusal* rather than
    /// silently degrading to `Off`, which would read as a user-invisible
    /// step-down of their integration.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "off" => Some(Self::Off),
            "linked" => Some(Self::Linked),
            "hosted_visible" => Some(Self::HostedVisible),
            "hosted_full" => Some(Self::HostedFull),
            _ => None,
        }
    }

    /// Whether this level is backed by a *Fauna-hosted* DID — the question the
    /// transition handler branches on (mint/reactivate vs. deactivate) and the
    /// one-backing rule keys off.
    pub fn is_hosted(self) -> bool {
        matches!(self, Self::HostedVisible | Self::HostedFull)
    }
}

/// What a user already has, which decides how a level change is realized —
/// notably whether entering a hosted level mints a new DID or reactivates the
/// one they kept from a previous step-down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelContext {
    /// An active consume-side link to an external Bluesky account.
    pub has_external_link: bool,
    /// A **restorable** Fauna-minted identity exists — active, deactivated, or
    /// presence-deleted. Layer-2 step-down and the presence sweep both retain
    /// the DID, so those still count: re-entry restores that same DID rather
    /// than minting a second one.
    ///
    /// A *retired* identity (the terminal PLC tombstone) does **not** count. Its
    /// DID no longer resolves anywhere, so there is nothing to restore, and the
    /// honest answer for re-entry is a fresh mint — a retirement ends an
    /// identity, not the user's ability to have one. Nest archives the retired
    /// row on that transition, so the destroyed DID stays a stored fact.
    pub has_identity: bool,
}

/// The effects one level change implies — the single description that nest
/// *executes* and the client's transition card *describes*.
///
/// Both sides reading one plan is the point: a card that promised something
/// nest did not do (or omitted something it did) is the failure mode a
/// confirm-before-anything-happens UX cannot tolerate, and two independent
/// transition tables would drift the moment either side grew a rung.
///
/// A jump across several rungs composes the per-rung effects into one plan and
/// one confirm (`docs/goal/ui/atproto.md` § Transition semantics).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransitionPlan {
    /// Drop the consume-side link (the one-backing rule). The external account
    /// itself is untouched — it keeps existing on its own PDS.
    pub unlinks_external: bool,
    /// Mint a brand-new Fauna-hosted DID. Requires the mint parameters.
    pub mints_identity: bool,
    /// Restore the retained DID from a previous step-down — same identity, no
    /// new mint, and the DID-method choice is a fact rather than a question.
    pub reactivates_identity: bool,
    /// Layer-2 deactivation: projection stops, the repo is no longer served,
    /// `#account` is announced. **Reversible** — the DID and sealed keys are
    /// retained (`atproto-pds-bridge.md` § Disable & revocation layer 2).
    pub deactivates_identity: bool,
    /// Let third-party ATProto apps log in as this identity. The plane stays
    /// inert until a credential is minted or a grant approved.
    pub opens_login_plane: bool,
    /// Suspend the external-app plane: live sessions revoked, grants
    /// suspended. Credential and grant rows are KEPT and individually
    /// revocable, so stepping back up restores usability.
    pub suspends_login_plane: bool,
}

impl TransitionPlan {
    /// The effects of moving `from` → `to` given what the user already has.
    pub fn for_move(from: IntegrationLevel, to: IntegrationLevel, ctx: LevelContext) -> Self {
        if from == to {
            return Self::default();
        }
        Self {
            // Entering a hosted level drops any external link; so does leaving
            // Linked downward, which is what "unlink the external account"
            // means at that rung.
            unlinks_external: ctx.has_external_link
                && (to.is_hosted() || (from == IntegrationLevel::Linked && to < from)),
            mints_identity: to.is_hosted() && !from.is_hosted() && !ctx.has_identity,
            reactivates_identity: to.is_hosted() && !from.is_hosted() && ctx.has_identity,
            deactivates_identity: from.is_hosted() && !to.is_hosted(),
            opens_login_plane: to == IntegrationLevel::HostedFull,
            // Leaving the top rung suspends the plane — whether stepping one
            // rung down to visible or dropping off the ladder entirely.
            suspends_login_plane: from == IntegrationLevel::HostedFull
                && to != IntegrationLevel::HostedFull,
        }
    }

    /// Nothing happens — the level is already in force, or the move is the one
    /// effect-free rung (Off → Linked, which merely reveals the link form).
    pub fn is_effect_free(&self) -> bool {
        *self == Self::default()
    }

    /// Whether the transition card must be shown and confirmed before anything
    /// happens. Only the effect-free move applies straight from the selector.
    pub fn needs_confirmation(&self) -> bool {
        !self.is_effect_free()
    }

    /// Always false: **no** level change destroys an identity. Terminal
    /// destruction lives only inside "Delete my Bluesky presence" as an
    /// explicit opt-in, never as a step-down (§ Don't do these). This exists so
    /// the guarantee is a *test assertion* rather than a comment — a future
    /// rung that quietly wired destruction into a downward move would have to
    /// change this line to pass.
    pub fn destroys_identity(&self) -> bool {
        false
    }
}

/// Maximum total length of a DNS hostname (and thus an ATProto handle).
const MAX_HANDLE_LEN: usize = 253;

/// Why a Fauna handle + domain pair derives no ATProto handle.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AtprotoHandleError {
    /// The Fauna handle itself is malformed (pre-`validate_handle` input, e.g.
    /// an admin-claim handle from before validation, or raw client input).
    #[error("invalid handle: {0}")]
    InvalidHandle(&'static str),
    /// The handle equals a reserved infra subdomain label (`mail`, `relay`, …).
    #[error("handle is a reserved subdomain label")]
    ReservedLabel,
    /// The domain is empty or not a plausible DNS name.
    #[error("invalid domain")]
    InvalidDomain,
    /// `<handle>.<domain>` exceeds the 253-char DNS hostname bound.
    #[error("derived handle exceeds the DNS hostname length bound")]
    TooLong,
}

/// Derive the ATProto handle for a Fauna handle on `domain`:
/// `alice` + `example.com` → `alice.example.com`, lowercase.
///
/// Callers must pass the *current* handle + handle-domain on every read — the
/// derivation is never persisted (derived-at-read; a rename re-derives).
pub fn derive_atproto_handle(handle: &str, domain: &str) -> Result<String, AtprotoHandleError> {
    validate_handle(handle).map_err(AtprotoHandleError::InvalidHandle)?;
    if is_reserved_subdomain_label(handle) {
        return Err(AtprotoHandleError::ReservedLabel);
    }
    let domain = normalize_dns_name(domain);
    if domain.is_empty()
        || !domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
    {
        return Err(AtprotoHandleError::InvalidDomain);
    }
    let derived = format!("{handle}.{domain}");
    if derived.len() > MAX_HANDLE_LEN {
        return Err(AtprotoHandleError::TooLong);
    }
    Ok(derived)
}

/// The `_atproto.<handle>` TXT record (name, value) that verifies an ATProto
/// handle → DID binding. The DNS record matrix wraps this into a `DnsRecord`.
pub fn handle_verification_txt(atproto_handle: &str, did: &str) -> (String, String) {
    (format!("_atproto.{atproto_handle}"), format!("did={did}"))
}

/// The `did:web` DID for an ATProto handle (the opt-in method — domain-bound,
/// no recovery; `atproto-pds-bridge.md` § DID method).
pub fn derive_did_web(atproto_handle: &str) -> String {
    format!("did:web:{atproto_handle}")
}

/// The two curves ATProto blesses for identity keys. Rotation keys must be one
/// of these per the did:plc spec; repo signing keys conventionally are too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DidKeyCurve {
    /// NIST P-256 (multicodec `0x1200`).
    P256,
    /// secp256k1 / K-256 (multicodec `0xE7`).
    K256,
}

impl DidKeyCurve {
    /// The unsigned-varint multicodec prefix for the compressed public key.
    fn multicodec_varint(self) -> [u8; 2] {
        match self {
            DidKeyCurve::P256 => [0x80, 0x24],
            DidKeyCurve::K256 => [0xE7, 0x01],
        }
    }
}

/// Why a did:key string (or key bytes) failed to encode/decode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DidKeyError {
    /// A compressed SEC1 point is exactly 33 bytes starting `0x02`/`0x03`.
    #[error("public key is not a 33-byte compressed SEC1 point")]
    NotCompressedPoint,
    /// Missing `did:key:`/`z` prefixes or invalid base58btc payload.
    #[error("malformed did:key string")]
    Malformed,
    /// The multicodec prefix is neither p256-pub nor secp256k1-pub.
    #[error("unsupported did:key multicodec")]
    UnsupportedCodec,
}

fn check_compressed(compressed: &[u8]) -> Result<(), DidKeyError> {
    if compressed.len() != 33 || !matches!(compressed[0], 0x02 | 0x03) {
        return Err(DidKeyError::NotCompressedPoint);
    }
    Ok(())
}

/// Multibase (`z` + base58btc) encoding of `multicodec ++ compressed point` —
/// the `publicKeyMultibase` form DID documents carry.
pub fn encode_multikey(curve: DidKeyCurve, compressed: &[u8]) -> Result<String, DidKeyError> {
    check_compressed(compressed)?;
    let mut bytes = Vec::with_capacity(2 + compressed.len());
    bytes.extend_from_slice(&curve.multicodec_varint());
    bytes.extend_from_slice(compressed);
    Ok(format!("z{}", bs58::encode(bytes).into_string()))
}

/// The full `did:key:z…` string for a compressed public key.
pub fn encode_did_key(curve: DidKeyCurve, compressed: &[u8]) -> Result<String, DidKeyError> {
    Ok(format!("did:key:{}", encode_multikey(curve, compressed)?))
}

/// Decode a `did:key:z…` (or bare `z…` multikey) string back to its curve and
/// compressed point.
pub fn decode_did_key(s: &str) -> Result<(DidKeyCurve, [u8; 33]), DidKeyError> {
    let multikey = s.strip_prefix("did:key:").unwrap_or(s);
    let b58 = multikey.strip_prefix('z').ok_or(DidKeyError::Malformed)?;
    let bytes = bs58::decode(b58)
        .into_vec()
        .map_err(|_| DidKeyError::Malformed)?;
    let (curve, rest) = match bytes.as_slice() {
        [0x80, 0x24, rest @ ..] => (DidKeyCurve::P256, rest),
        [0xE7, 0x01, rest @ ..] => (DidKeyCurve::K256, rest),
        _ => return Err(DidKeyError::UnsupportedCodec),
    };
    check_compressed(rest)?;
    let mut out = [0u8; 33];
    out.copy_from_slice(rest);
    Ok((curve, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── integration level ────────────────────────────────────────────────────

    #[test]
    fn level_wire_strings_roundtrip() {
        // The four wire values are persisted in `atproto_account_settings
        // .integration_level` and cross the `set_integration_level` wire; a
        // rename here is an at-rest + wire break, so they are pinned literally.
        for (level, wire) in [
            (IntegrationLevel::Off, "off"),
            (IntegrationLevel::Linked, "linked"),
            (IntegrationLevel::HostedVisible, "hosted_visible"),
            (IntegrationLevel::HostedFull, "hosted_full"),
        ] {
            assert_eq!(level.as_str(), wire);
            assert_eq!(IntegrationLevel::from_wire(wire), Some(level));
        }
        assert_eq!(IntegrationLevel::from_wire("hosted"), None);
        assert_eq!(IntegrationLevel::from_wire(""), None);
    }

    #[test]
    fn levels_are_ordered_rungs() {
        // The ladder ordering is load-bearing: a multi-rung jump composes the
        // per-rung effects, and direction (up vs. down) selects which half of
        // the transition matrix applies (`ui/atproto.md` § Transition semantics).
        assert!(IntegrationLevel::Off < IntegrationLevel::Linked);
        assert!(IntegrationLevel::Linked < IntegrationLevel::HostedVisible);
        assert!(IntegrationLevel::HostedVisible < IntegrationLevel::HostedFull);
    }

    #[test]
    fn hosted_predicate_covers_exactly_the_two_hosted_rungs() {
        // "Is there a Fauna-hosted identity backing this level?" is the question
        // the transition handler branches on (mint/reactivate vs. deactivate),
        // and the one-backing rule keys off it.
        assert!(!IntegrationLevel::Off.is_hosted());
        assert!(!IntegrationLevel::Linked.is_hosted());
        assert!(IntegrationLevel::HostedVisible.is_hosted());
        assert!(IntegrationLevel::HostedFull.is_hosted());
    }

    // ── transition planning ──────────────────────────────────────────────────

    /// Context helper: nothing linked, nothing minted.
    fn fresh() -> LevelContext {
        LevelContext {
            has_external_link: false,
            has_identity: false,
        }
    }

    #[test]
    fn off_to_linked_is_the_one_effect_free_move() {
        // `ui/atproto.md` § Transition semantics: "No card needed — this is the
        // one effect-free transition; apply immediately on select."
        let plan =
            TransitionPlan::for_move(IntegrationLevel::Off, IntegrationLevel::Linked, fresh());
        assert!(plan.is_effect_free());
        assert!(!plan.needs_confirmation());
    }

    #[test]
    fn entering_hosted_mints_once_then_reactivates() {
        // First entry with no identity row mints; a later re-entry against a
        // retained (deactivated) identity reactivates the SAME DID and must
        // never mint a second one.
        let first = TransitionPlan::for_move(
            IntegrationLevel::Off,
            IntegrationLevel::HostedVisible,
            fresh(),
        );
        assert!(first.mints_identity);
        assert!(!first.reactivates_identity);

        let again = TransitionPlan::for_move(
            IntegrationLevel::Off,
            IntegrationLevel::HostedVisible,
            LevelContext {
                has_external_link: false,
                has_identity: true,
            },
        );
        assert!(!again.mints_identity, "an existing DID is never re-minted");
        assert!(again.reactivates_identity);
        assert!(again.needs_confirmation());
    }

    #[test]
    fn entering_hosted_unlinks_an_external_account_only_when_one_exists() {
        // The one-backing rule: no state may hold both an active consume-side
        // link and an active hosted identity.
        let with_link = TransitionPlan::for_move(
            IntegrationLevel::Linked,
            IntegrationLevel::HostedVisible,
            LevelContext {
                has_external_link: true,
                has_identity: false,
            },
        );
        assert!(with_link.unlinks_external);

        let without = TransitionPlan::for_move(
            IntegrationLevel::Off,
            IntegrationLevel::HostedVisible,
            fresh(),
        );
        assert!(!without.unlinks_external);
    }

    #[test]
    fn the_login_plane_opens_and_suspends_with_the_top_rung() {
        let ctx = LevelContext {
            has_external_link: false,
            has_identity: true,
        };
        let up = TransitionPlan::for_move(
            IntegrationLevel::HostedVisible,
            IntegrationLevel::HostedFull,
            ctx,
        );
        assert!(up.opens_login_plane);
        assert!(!up.suspends_login_plane);
        assert!(!up.deactivates_identity, "stepping UP destroys nothing");

        let down = TransitionPlan::for_move(
            IntegrationLevel::HostedFull,
            IntegrationLevel::HostedVisible,
            ctx,
        );
        assert!(down.suspends_login_plane);
        assert!(
            !down.deactivates_identity,
            "projection continues at hosted_visible"
        );
    }

    #[test]
    fn leaving_hosted_deactivates_reversibly_and_tears_the_plane_down() {
        // A multi-rung drop composes the per-rung effects into ONE transition:
        // full PDS → Off both suspends the plane and deactivates the identity.
        let ctx = LevelContext {
            has_external_link: false,
            has_identity: true,
        };
        for target in [IntegrationLevel::Linked, IntegrationLevel::Off] {
            let plan = TransitionPlan::for_move(IntegrationLevel::HostedFull, target, ctx);
            assert!(plan.deactivates_identity, "→ {target:?} must deactivate");
            assert!(plan.suspends_login_plane, "→ {target:?} must tear down");
            assert!(
                !plan.destroys_identity(),
                "step-down is REVERSIBLE — destruction is the separate action"
            );
            assert!(plan.needs_confirmation());
        }
    }

    #[test]
    fn leaving_linked_unlinks_and_nothing_else() {
        let plan = TransitionPlan::for_move(
            IntegrationLevel::Linked,
            IntegrationLevel::Off,
            LevelContext {
                has_external_link: true,
                has_identity: false,
            },
        );
        assert!(plan.unlinks_external);
        assert!(!plan.deactivates_identity);
        assert!(!plan.mints_identity);
    }

    #[test]
    fn a_move_to_the_current_level_is_an_idempotent_noop() {
        // The ratified contract is "idempotent on retry": re-confirming the
        // level already in force succeeds and does nothing.
        for level in [
            IntegrationLevel::Off,
            IntegrationLevel::Linked,
            IntegrationLevel::HostedVisible,
            IntegrationLevel::HostedFull,
        ] {
            let plan = TransitionPlan::for_move(
                level,
                level,
                LevelContext {
                    has_external_link: true,
                    has_identity: true,
                },
            );
            assert!(plan.is_effect_free(), "{level:?} → itself must be a no-op");
        }
    }

    #[test]
    fn mint_parameters_are_required_exactly_when_a_mint_happens() {
        assert!(
            TransitionPlan::for_move(IntegrationLevel::Off, IntegrationLevel::HostedFull, fresh())
                .mints_identity,
            "a two-rung jump into full PDS still mints"
        );
        assert!(
            !TransitionPlan::for_move(
                IntegrationLevel::HostedVisible,
                IntegrationLevel::Off,
                LevelContext {
                    has_external_link: false,
                    has_identity: true
                },
            )
            .mints_identity
        );
    }

    #[test]
    fn default_level_is_off() {
        // Default OFF is the ratified consent posture for publishing to a
        // public third-party network (`atproto-pds-bridge.md` § Enable UX).
        assert_eq!(IntegrationLevel::default(), IntegrationLevel::Off);
    }

    // ── handle derivation ────────────────────────────────────────────────────

    #[test]
    fn derives_the_plain_case() {
        assert_eq!(
            derive_atproto_handle("alice", "example.com"),
            Ok("alice.example.com".into())
        );
        // Trailing dot + case are normalized.
        assert_eq!(
            derive_atproto_handle("alice", "Example.COM."),
            Ok("alice.example.com".into())
        );
    }

    #[test]
    fn rejects_reserved_labels() {
        // Iterate the CONSTANT, never a hand-copied list: a hardcoded set passes
        // vacuously for any label added later (exactly how `pds` slipped past the
        // invariant when the `pds.*` SNI route landed).
        for h in fauna_core::web::RESERVED_SUBDOMAIN_LABELS {
            assert_eq!(
                derive_atproto_handle(h, "example.com"),
                Err(AtprotoHandleError::ReservedLabel),
                "{h:?} must not derive an ATProto handle"
            );
        }
    }

    #[test]
    fn rejects_malformed_handles() {
        // validate_handle already excludes dots/plus/underscore/unicode/@ —
        // the goal doc's sanitization worry is structurally impossible for
        // stored handles, but raw input still hits this path.
        for h in ["a.b", "a+b", "a_b", "Alice", "café", "ab"] {
            assert!(matches!(
                derive_atproto_handle(h, "example.com"),
                Err(AtprotoHandleError::InvalidHandle(_))
            ));
        }
    }

    #[test]
    fn rejects_bad_domains() {
        for d in ["", ".", "ex..com", "-bad.com", "bad-.com", "ex_ample.com"] {
            assert_eq!(
                derive_atproto_handle("alice", d),
                Err(AtprotoHandleError::InvalidDomain),
                "domain {d:?} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_overlong_derivation() {
        let label = "a".repeat(63);
        let domain = format!("{label}.{label}.{label}.{label}");
        assert_eq!(
            derive_atproto_handle("alice", &domain),
            Err(AtprotoHandleError::TooLong)
        );
    }

    #[test]
    fn verification_txt_shape() {
        assert_eq!(
            handle_verification_txt("alice.example.com", "did:plc:abc123"),
            (
                "_atproto.alice.example.com".into(),
                "did=did:plc:abc123".into()
            )
        );
    }

    #[test]
    fn did_web_shape() {
        assert_eq!(
            derive_did_web("alice.example.com"),
            "did:web:alice.example.com"
        );
    }

    // ── did:key ──────────────────────────────────────────────────────────────

    // The two example vectors from the ATProto cryptography spec.
    const P256_VECTOR: &str = "did:key:zDnaembgSGUhZULN2Caob4HLJPaxBh92N7rtH21TErzqf8HQo"; // gitleaks:allow
    const K256_VECTOR: &str = "did:key:zQ3shqwJEJyMBsBXCWyCBpUBMqxcon9oHB7mCvx4sSpMdLJwc"; // gitleaks:allow

    #[test]
    fn spec_vectors_roundtrip() {
        for (vector, curve) in [
            (P256_VECTOR, DidKeyCurve::P256),
            (K256_VECTOR, DidKeyCurve::K256),
        ] {
            let (got_curve, point) = decode_did_key(vector).expect("vector decodes");
            assert_eq!(got_curve, curve);
            assert!(matches!(point[0], 0x02 | 0x03));
            assert_eq!(encode_did_key(curve, &point).unwrap(), vector);
        }
    }

    #[test]
    fn characteristic_prefixes() {
        // The spec-documented visual prefixes: p256 → zDn…, k256 → zQ3s….
        let (_, p) = decode_did_key(P256_VECTOR).unwrap();
        let (_, k) = decode_did_key(K256_VECTOR).unwrap();
        assert!(
            encode_did_key(DidKeyCurve::P256, &p)
                .unwrap()
                .starts_with("did:key:zDn")
        );
        assert!(
            encode_did_key(DidKeyCurve::K256, &k)
                .unwrap()
                .starts_with("did:key:zQ3s")
        );
    }

    #[test]
    fn bare_multikey_decodes_and_reencodes() {
        let (curve, point) = decode_did_key(P256_VECTOR).unwrap();
        let multikey = encode_multikey(curve, &point).unwrap();
        assert!(multikey.starts_with('z'));
        assert_eq!(decode_did_key(&multikey).unwrap(), (curve, point));
        assert_eq!(format!("did:key:{multikey}"), P256_VECTOR);
    }

    #[test]
    fn rejects_bad_points_and_strings() {
        assert_eq!(
            encode_did_key(DidKeyCurve::P256, &[0u8; 33]),
            Err(DidKeyError::NotCompressedPoint)
        );
        assert_eq!(
            encode_did_key(DidKeyCurve::P256, &[2u8; 32]),
            Err(DidKeyError::NotCompressedPoint)
        );
        assert_eq!(decode_did_key("did:key:Qm"), Err(DidKeyError::Malformed));
        assert_eq!(decode_did_key("did:key:z0O"), Err(DidKeyError::Malformed));
        // ed25519 multicodec (0xED 0x01) is not an ATProto identity curve.
        let mut ed = vec![0xED, 0x01];
        ed.extend_from_slice(&[0x02; 33]);
        let ed_str = format!("did:key:z{}", bs58::encode(ed).into_string());
        assert_eq!(decode_did_key(&ed_str), Err(DidKeyError::UnsupportedCodec));
    }
}
