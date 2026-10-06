//! The web-paywall **capability URL** token (`monetization.md` § Pillar 2
//! Gate/Q2–Q3; `web-content-hosting.md` § Paywalled serving).
//!
//! A short-lived, signed, self-contained bearer credential a visitor presents
//! **explicitly per request** (URL-carried `?token=…`) to read one paywalled
//! rendered page. Verified **statelessly**: the token is the `ShareToken`
//! pattern — sign-over-CID, embed-as-bytes, base64url — signed by the
//! **web-serve holder's own Ed25519 identity** and verified against that same
//! key, so the check is nest-owned code in the serving layer's own category
//! (no user code, no DB lookup, no per-request render). It is never a cookie
//! (invariant 2: non-ambient — a hostile sibling subdomain cannot ride it),
//! and a leaked token reads one rendered post for minutes, never account
//! control (short TTL + `(owner, path)` scope + free re-mint).

use fauna_core::data::Timestamp;
use fauna_core::encoding::{self, Signed};
use fauna_core::identity::{ActorId, ActorKeypair};
use serde::{Deserialize, Serialize};

/// Token lifetime. A hard-coded constant (no operator exists to tune it):
/// long enough to click through from a payment/claim flow, short enough that
/// a referrer/history-leaked URL goes stale in minutes. Re-minting is free.
pub const WEB_PAYWALL_TOKEN_TTL_SECS: u64 = 600;

/// The signed content of a web-paywall capability token. The Ed25519
/// signature ships in the sign-over-CID envelope alongside these bytes
/// (embed-as-bytes), exactly like `ShareToken`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebPaywallToken {
    /// The web-serve holder identity that minted (signed) this token. Bound
    /// into the signature; the verifier REQUIRES it to equal its own holder
    /// key — without that equality check anyone could self-sign a "valid"
    /// token under their own key.
    pub holder: ActorId,
    /// The creator whose paywalled page this token opens.
    pub owner: ActorId,
    /// The exact rendered path the token is scoped to (e.g.
    /// `post/my-slug.html` — the `web_rendered_sealed` row key).
    pub path: String,
    /// Expiry, unix seconds.
    pub expires: u64,
}

impl Signed for WebPaywallToken {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.holder.0
    }
}

impl WebPaywallToken {
    /// Mint a token for `(owner, path)` signed by the holder keypair, valid
    /// for [`WEB_PAYWALL_TOKEN_TTL_SECS`]. Returns the base64url form (the
    /// `?token=` value) and its expiry.
    pub fn mint(
        holder_keypair: &ActorKeypair,
        owner: [u8; 32],
        path: &str,
    ) -> anyhow::Result<(String, u64)> {
        use base64::Engine as _;
        let expires = Timestamp::now_secs() as u64 + WEB_PAYWALL_TOKEN_TTL_SECS;
        let token = WebPaywallToken {
            holder: holder_keypair.actor_id(),
            owner: ActorId(owner),
            path: path.to_string(),
            expires,
        };
        let bytes = encoding::sign_and_pack(holder_keypair, &token)?;
        Ok((
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes),
            expires,
        ))
    }

    /// Like [`mint`](Self::mint) but with an explicit `expires` instead of
    /// stamping `now + TTL` — the only way to produce an **expired** token.
    /// `mint` always mints a fresh 10-minute token, so a tier_3 test can't
    /// reach the expiry branch through the production RPC without waiting out
    /// the real TTL; this is exposed only to unit tests and the
    /// `test-hooks`-gated `web_paywall_test_hook` (never the production
    /// binary).
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn mint_with_expiry(
        holder_keypair: &ActorKeypair,
        owner: [u8; 32],
        path: &str,
        expires: u64,
    ) -> anyhow::Result<String> {
        use base64::Engine as _;
        let token = WebPaywallToken {
            holder: holder_keypair.actor_id(),
            owner: ActorId(owner),
            path: path.to_string(),
            expires,
        };
        let bytes = encoding::sign_and_pack(holder_keypair, &token)?;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    }

    /// Verify a base64url token against the expected holder key, owner, path,
    /// and the clock. `None` on ANY failure — malformed, bad signature, a
    /// signer other than this nest's web-serve holder (the anti-forgery
    /// check), scope mismatch, or expiry; the caller falls back to the teaser
    /// (never an error page — `monetization.md` § Pillar 2).
    pub fn verify(
        token_b64: &str,
        expected_holder: &[u8; 32],
        owner: &[u8; 32],
        path: &str,
    ) -> Option<()> {
        let token: WebPaywallToken = encoding::decode_and_verify_base64url(token_b64).ok()?;
        (token.holder.0 == *expected_holder
            && token.owner.0 == *owner
            && token.path == path
            && (Timestamp::now_secs() as u64) < token.expires)
            .then_some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder_kp() -> ActorKeypair {
        ActorKeypair::from_secret([9u8; 32])
    }

    #[test]
    fn mint_verify_roundtrip_and_scope_checks() {
        let kp = holder_kp();
        let holder_pk = kp.actor_id().0;
        let owner = [3u8; 32];
        let (tok, expires) = WebPaywallToken::mint(&kp, owner, "post/premium.html").unwrap();
        assert!(expires > Timestamp::now_secs() as u64);

        // Valid: exact holder + owner + path.
        assert!(WebPaywallToken::verify(&tok, &holder_pk, &owner, "post/premium.html").is_some());
        // Wrong path / wrong owner / wrong expected holder: all deny.
        assert!(WebPaywallToken::verify(&tok, &holder_pk, &owner, "post/other.html").is_none());
        assert!(
            WebPaywallToken::verify(&tok, &holder_pk, &[4u8; 32], "post/premium.html").is_none()
        );
        assert!(
            WebPaywallToken::verify(&tok, &[0u8; 32], &owner, "post/premium.html").is_none(),
            "a token signed by a different key must not verify against our holder"
        );
        // Garbage is a clean deny.
        assert!(WebPaywallToken::verify("not-a-token", &holder_pk, &owner, "x").is_none());
    }

    #[test]
    fn forged_token_from_another_keypair_is_rejected() {
        // An attacker mints a perfectly well-formed token under THEIR OWN
        // keypair naming themselves as holder — the signature verifies against
        // the embedded key, so the equality check against OUR holder key is
        // what rejects it.
        let attacker = ActorKeypair::from_secret([7u8; 32]);
        let our_holder = holder_kp().actor_id().0;
        let owner = [3u8; 32];
        let (forged, _) = WebPaywallToken::mint(&attacker, owner, "post/premium.html").unwrap();
        assert!(
            WebPaywallToken::verify(&forged, &our_holder, &owner, "post/premium.html").is_none()
        );
    }

    #[test]
    fn expired_token_is_rejected() {
        let kp = holder_kp();
        let holder_pk = kp.actor_id().0;
        let owner = [3u8; 32];
        // mint always stamps now+TTL, so an expired token needs the explicit-
        // expires constructor (1 = 1970, always in the past).
        let b64 = WebPaywallToken::mint_with_expiry(&kp, owner, "post/premium.html", 1).unwrap();
        assert!(WebPaywallToken::verify(&b64, &holder_pk, &owner, "post/premium.html").is_none());
    }
}
