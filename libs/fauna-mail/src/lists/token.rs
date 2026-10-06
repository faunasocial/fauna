//! Deployment-wide RFC 8058 one-click-unsubscribe token derivation
//! (`docs/goal/behavior/mail-mass-mailing.md` § Token format).
//!
//! Pure — no tokio/DNS/parser, WASM-safe (sibling of `srs`/`aliases`): the
//! nest derives a member's token at subscribe time + at send time, and
//! verifies a claimed token at the HTTPS / mailto unsubscribe handlers, all
//! over the one shared impl (priority #2).

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Bytes of HMAC-SHA-256 output kept in the token (`mail-mass-mailing.md`
/// § Token format: 24 bytes = 192 bits, base64url → 32 chars, no padding).
pub const UNSUBSCRIBE_TOKEN_BYTES: usize = 24;

/// Deployment-wide one-click-unsubscribe secret length (`§ Token format`:
/// "a 32-byte secret in nest state ... server-managed"). Nest-held plaintext —
/// the same class + lifecycle as the SRS secret (a deployment-wide mail HMAC
/// key), nest-generated + auto-seeded, never wrapped to the MTA (the MTA never
/// needs it: unsubscribe handlers resolve a token by the cached
/// `mail_list_members.one_click_unsubscribe_token` index, not by re-deriving).
pub const UNSUBSCRIBE_SECRET_BYTES: usize = 32;

/// Derives + verifies the per-subscription one-click-unsubscribe token.
///
/// Holds the 32-byte deployment secret (one secret deployment-wide, not
/// per-list — `§ Token format`). The token for a subscription is
/// `base64url(HMAC_SHA256(secret, list_id || recipient_address)[:24])` and is
/// **deterministic** (same inputs → same token). The caller passes the
/// *canonical* (lower-cased, stored) recipient address so the token matches
/// the `mail_list_members.one_click_unsubscribe_token` cache and the DB
/// `UNIQUE(list_id, recipient_address)` key — normalization is the caller's
/// (DB-layer's) job, so this helper hashes exactly what it is given and stays
/// free of email-normalization policy.
#[derive(Clone)]
pub struct UnsubscribeTokenGenerator {
    secret: [u8; UNSUBSCRIBE_SECRET_BYTES],
}

impl UnsubscribeTokenGenerator {
    /// Wrap the unwrapped 32-byte deployment secret.
    pub fn new(secret: [u8; UNSUBSCRIBE_SECRET_BYTES]) -> Self {
        Self { secret }
    }

    /// `base64url(HMAC_SHA256(secret, list_id || recipient_address)[:24])`.
    ///
    /// `list_id` is the 16-byte list UUID; being fixed-width, its concatenation
    /// with the variable-length `recipient_address` is unambiguous without a
    /// separator (the first 16 bytes are always the list_id) — matching
    /// `§ Token format`'s `concat(list_id, recipient_address)`.
    pub fn token_for(&self, list_id: &[u8; 16], recipient_address: &str) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts any key length");
        mac.update(list_id);
        mac.update(recipient_address.as_bytes());
        let full = mac.finalize().into_bytes();
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&full[..UNSUBSCRIBE_TOKEN_BYTES])
    }

    /// Re-derive the expected token from `(list_id, recipient_address)` and
    /// constant-time compare it against `claimed_token`. A bit-flip in the
    /// token (or a wrong secret / list / address) yields `false`.
    pub fn verify_token(
        &self,
        list_id: &[u8; 16],
        recipient_address: &str,
        claimed_token: &str,
    ) -> bool {
        let expected = self.token_for(list_id, recipient_address);
        fauna_core::secret::constant_time_eq(expected.as_bytes(), claimed_token.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: [u8; 32] = [7u8; 32];
    const LIST_A: [u8; 16] = [1u8; 16];
    const LIST_B: [u8; 16] = [2u8; 16];

    fn generator() -> UnsubscribeTokenGenerator {
        UnsubscribeTokenGenerator::new(SECRET)
    }

    #[test]
    fn token_is_deterministic() {
        let g = generator();
        assert_eq!(
            g.token_for(&LIST_A, "alice@example.com"),
            g.token_for(&LIST_A, "alice@example.com"),
        );
    }

    #[test]
    fn token_varies_by_recipient() {
        let g = generator();
        assert_ne!(
            g.token_for(&LIST_A, "alice@example.com"),
            g.token_for(&LIST_A, "bob@example.com"),
        );
    }

    #[test]
    fn token_varies_by_list() {
        let g = generator();
        assert_ne!(
            g.token_for(&LIST_A, "alice@example.com"),
            g.token_for(&LIST_B, "alice@example.com"),
        );
    }

    #[test]
    fn token_varies_by_secret() {
        let a = UnsubscribeTokenGenerator::new([1u8; 32]);
        let b = UnsubscribeTokenGenerator::new([2u8; 32]);
        assert_ne!(
            a.token_for(&LIST_A, "alice@example.com"),
            b.token_for(&LIST_A, "alice@example.com"),
        );
    }

    #[test]
    fn token_is_32_char_url_safe_base64() {
        let t = generator().token_for(&LIST_A, "alice@example.com");
        // 24 bytes → 32 base64url chars, no padding.
        assert_eq!(t.len(), 32, "token = {t}");
        assert!(
            t.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "token must be url-safe (no +/=): {t}"
        );
    }

    #[test]
    fn token_matches_independent_hmac() {
        // Structural check: pin that the generator does exactly
        // base64url(HMAC_SHA256(secret, list_id || addr)[:24]), computed here
        // independently of the production helper.
        let addr = "alice@example.com";
        let mut mac = HmacSha256::new_from_slice(&SECRET).unwrap();
        mac.update(&LIST_A);
        mac.update(addr.as_bytes());
        let full = mac.finalize().into_bytes();
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(&full[..UNSUBSCRIBE_TOKEN_BYTES]);
        assert_eq!(generator().token_for(&LIST_A, addr), expected);
    }

    #[test]
    fn verify_accepts_correct_token() {
        let g = generator();
        let t = g.token_for(&LIST_A, "alice@example.com");
        assert!(g.verify_token(&LIST_A, "alice@example.com", &t));
    }

    #[test]
    fn verify_rejects_bit_flip() {
        let g = generator();
        let t = g.token_for(&LIST_A, "alice@example.com");
        let mut flipped: Vec<char> = t.chars().collect();
        // Flip the first char to a different url-safe char.
        flipped[0] = if flipped[0] == 'A' { 'B' } else { 'A' };
        let flipped: String = flipped.into_iter().collect();
        assert_ne!(flipped, t);
        assert!(!g.verify_token(&LIST_A, "alice@example.com", &flipped));
    }

    #[test]
    fn verify_rejects_wrong_recipient_and_garbage() {
        let g = generator();
        let t = g.token_for(&LIST_A, "alice@example.com");
        assert!(!g.verify_token(&LIST_A, "bob@example.com", &t));
        assert!(!g.verify_token(&LIST_A, "alice@example.com", ""));
        assert!(!g.verify_token(&LIST_A, "alice@example.com", "not-a-real-token"));
    }
}
