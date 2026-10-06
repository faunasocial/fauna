//! Auto-generated bridge passwords (IMAP / SMTP / CalDAV PLAIN credential).
//!
//! On an **encrypted** nest the at-rest security of a mailbox/calendar that has
//! the bridge enabled is bounded by the brute-force resistance of this password
//! (it Argon2id-wraps the actor's MLS capability — see
//! `docs/goal/behavior/mail-credentials.md` § Auto-generated bridge password).
//! So the client generates a high-entropy random password BY DEFAULT, the same
//! way Proton Bridge does, rather than letting the user pick a weak one. The
//! "Auto-generate" toggle is on by default on every app; turning it off on an
//! encrypted nest surfaces a warning (`warn_manual_password`).
//!
//! Charset is `a-zA-Z0-9` only (no special characters) so the password pastes
//! cleanly into any MUA's account field without escaping/quoting surprises.
//!
//! This module is also the single source for the OAUTHBEARER credential token
//! (`generate_bridge_token`) — the high-entropy bearer secret the recommended
//! credential kind uses — so the PLAIN password and the bearer token are minted
//! the same way on every app (was per-app: linux `fresh_token_string`,
//! windows `FreshTokenHex`, web).

use fauna_core::localized::LocalizedText;
use fauna_core::secret::SecretString;
use rand::RngCore;

/// `a-zA-Z0-9` — 62 symbols, ~5.954 bits each.
const CHARSET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Default length. 24 × log2(62) ≈ **142.9 bits** of entropy — comfortably past
/// the 128-bit bar, while still short enough to paste. (Argon2id wrapping then
/// makes each offline guess expensive on top of this.)
pub const BRIDGE_PASSWORD_LEN: usize = 24;

/// Generate the default-length random bridge password.
pub fn generate_bridge_password() -> SecretString {
    generate_bridge_password_len(BRIDGE_PASSWORD_LEN)
}

/// Generate a random `a-zA-Z0-9` password of `len` characters from the OS CSPRNG.
///
/// Uses rejection sampling so the charset distribution is unbiased (a naïve
/// `byte % 62` favours the first `256 % 62 = 8` symbols). Mirrors the crate's
/// existing CSPRNG use (`machine.rs` MSEK generation, `rand::thread_rng()`).
pub fn generate_bridge_password_len(len: usize) -> SecretString {
    // Largest multiple of 62 that fits in a byte; reject anything at/above it.
    const LIMIT: u8 = 62 * 4; // 248
    let mut rng = rand::thread_rng();
    let mut out = String::with_capacity(len);
    let mut buf = [0u8; 64];
    while out.len() < len {
        rng.fill_bytes(&mut buf);
        for &b in buf.iter() {
            if out.len() == len {
                break;
            }
            if b < LIMIT {
                out.push(CHARSET[(b % 62) as usize] as char);
            }
        }
    }
    SecretString::new(out)
}

/// Number of random bytes in an OAUTHBEARER credential token. 32 bytes = 256 bits,
/// far past the ≥128-bit bar the bridge's HKDF-AUTH path assumes — per
/// `docs/goal/behavior/mail-credentials.md` § OAUTHBEARER ("Fresh random 32 bytes").
pub const BRIDGE_TOKEN_BYTES: usize = 32;

/// Mint a fresh OAUTHBEARER credential token as a lowercase-hex string
/// (`BRIDGE_TOKEN_BYTES` random bytes → twice as many hex chars) from the OS
/// CSPRNG. The credential secret is this string's UTF-8 bytes, so the MUA pastes
/// exactly what the bridge HKDF-unwraps with (`mail-credentials.md` § OAUTHBEARER).
/// Displayed once on the credential-add screen for the user to copy; never
/// persisted in a recoverable form. The single source for the per-app token
/// mints this replaces — linux `fresh_token_string`, windows `FreshTokenHex`, web
/// (which mis-sized at 24 bytes) — so all apps agree on the spec'd 32-byte shape.
pub fn generate_bridge_token() -> SecretString {
    let mut bytes = [0u8; BRIDGE_TOKEN_BYTES];
    rand::thread_rng().fill_bytes(&mut bytes);
    SecretString::new(fauna_core::format::hex_full(&bytes))
}

/// Resolve the PLAIN-form password field's autogenerate-vs-manual decision at a
/// **settled toggle edge** (the kind-selector landing on PLAIN, or the
/// auto-generate toggle flipping) — `Some` a freshly-minted secret when both
/// `kind` is [`crate::state::CredentialKind::Plain`] and `autogenerate` is on;
/// `None` to clear the field for manual entry, including whenever `kind` is
/// `OAuthBearer` (the PLAIN password field doesn't apply there, regardless of
/// `autogenerate`'s stale value from a prior PLAIN visit).
///
/// **Call this ONLY at a settled edge, never again at submit** — the caller
/// stores the returned value and reads it back verbatim to build the
/// credential, so the secret persisted is byte-identical to the one the user
/// copied. Every one of the 6 clients had hand-rolled this exact two-input
/// decision independently; centralizing it doesn't change any client's
/// behavior, but makes the sequencing bug an apple 2026-07-13 build hit
/// structurally impossible to reintroduce — that bug re-minted a fresh,
/// different password at submit because the edge-mint and the submit path
/// called [`generate_bridge_password`] independently instead of sharing one
/// resolved value. See `docs/goal/behavior/mail-credentials.md` § Auto-generated
/// bridge password.
pub fn resolve_autogenerated_password(
    kind: crate::state::CredentialKind,
    autogenerate: bool,
) -> Option<SecretString> {
    if kind == crate::state::CredentialKind::Plain && autogenerate {
        Some(generate_bridge_password())
    } else {
        None
    }
}

/// Whether to warn the user that a manually-typed password weakens at-rest.
///
/// True only when auto-generate is OFF **and** the nest is encrypted: on a
/// plaintext nest the data already rests unencrypted, so a weak password
/// adds no incremental at-rest exposure and no warning is shown.
pub fn warn_manual_password(auto_generate: bool, nest_encrypted: bool) -> bool {
    !auto_generate && nest_encrypted
}

/// Character count at/above which a manually-typed password reads "Fair".
pub const PASSWORD_FAIR_MIN_LEN: usize = 8;
/// Character count at/above which a manually-typed password reads "Strong".
pub const PASSWORD_STRONG_MIN_LEN: usize = 16;

/// Advisory strength rating for a manually-typed bridge password, as a
/// [`LocalizedText`] each app resolves through its own i18n runtime (mirrors
/// [`crate::bridge_display_name`]). `< 8` chars → Weak, `< 16` → Fair, `≥ 16` →
/// Strong; an empty password returns `None` (the client clears the readout).
/// Length-only by design — the charset is fixed `a-zA-Z0-9` so length is the
/// entropy lever, and the readout is purely advisory (never gates submission).
/// Counts Unicode scalar values (`chars().count()`), so the rating is identical
/// across platforms whose native string length differs (Rust bytes vs UTF-16
/// units). Lifts the per-app meters linux/web/windows/android each hard-coded
/// onto one source of truth + one canonical threshold (priority #1/#2/#4: the
/// reference client linux's `< 16` Strong cutoff, replacing web's/android's
/// `< 12` drift; linux + web additionally drop their raw-English labels for the
/// shared `settings.mail.strength_{weak,fair,strong}` i18n keys).
/// `docs/goal/ui/mail-settings.md` (`mail-add-credential-password-strength-meter`).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn password_strength_label(password: &str) -> Option<LocalizedText> {
    match password.chars().count() {
        0 => None,
        n if n < PASSWORD_FAIR_MIN_LEN => Some(LocalizedText::key("settings.mail.strength_weak")),
        n if n < PASSWORD_STRONG_MIN_LEN => Some(LocalizedText::key("settings.mail.strength_fair")),
        _ => Some(LocalizedText::key("settings.mail.strength_strong")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_alnum_ascii(s: &str) -> bool {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() && b < 0x80)
    }

    #[test]
    fn default_length_and_charset() {
        let p = generate_bridge_password();
        assert_eq!(p.as_str().chars().count(), BRIDGE_PASSWORD_LEN);
        assert!(
            is_alnum_ascii(p.as_str()),
            "charset must be a-zA-Z0-9: {p:?}",
            p = p.as_str()
        );
    }

    #[test]
    fn custom_length_respected() {
        for len in [1usize, 8, 32, 100] {
            let p = generate_bridge_password_len(len);
            assert_eq!(p.as_str().chars().count(), len);
            assert!(is_alnum_ascii(p.as_str()));
        }
    }

    #[test]
    fn no_special_characters_ever() {
        // Many draws — assert the charset invariant holds across a large sample.
        for _ in 0..200 {
            let p = generate_bridge_password();
            assert!(
                p.as_str().bytes().all(|b| CHARSET.contains(&b)),
                "found a non-[a-zA-Z0-9] byte in {:?}",
                p.as_str()
            );
        }
    }

    #[test]
    fn successive_passwords_differ() {
        // Collision probability at 143 bits is negligible; a repeat means a
        // broken RNG wiring.
        let a = generate_bridge_password();
        let b = generate_bridge_password();
        assert_ne!(a.as_str(), b.as_str());
    }

    #[test]
    fn token_is_64_lowercase_hex_chars() {
        let t = generate_bridge_token();
        let s = t.as_str();
        assert_eq!(s.len(), BRIDGE_TOKEN_BYTES * 2); // 32 bytes → 64 hex chars
        assert!(
            s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "token must be lowercase hex: {s:?}"
        );
    }

    #[test]
    fn successive_tokens_differ() {
        // 256-bit tokens: a repeat means broken RNG wiring, not chance.
        let a = generate_bridge_token();
        let b = generate_bridge_token();
        assert_ne!(a.as_str(), b.as_str());
    }

    #[test]
    fn resolve_autogenerated_password_plain_and_autogen_mints() {
        let p = resolve_autogenerated_password(crate::state::CredentialKind::Plain, true);
        assert!(p.is_some());
        assert_eq!(p.unwrap().as_str().chars().count(), BRIDGE_PASSWORD_LEN);
    }

    #[test]
    fn resolve_autogenerated_password_plain_manual_clears() {
        assert!(
            resolve_autogenerated_password(crate::state::CredentialKind::Plain, false).is_none()
        );
    }

    #[test]
    fn resolve_autogenerated_password_oauthbearer_always_none() {
        // The PLAIN password field doesn't apply on OAUTHBEARER, regardless of
        // the auto-generate toggle's stale value from a prior PLAIN visit.
        assert!(
            resolve_autogenerated_password(crate::state::CredentialKind::OAuthBearer, true)
                .is_none()
        );
        assert!(
            resolve_autogenerated_password(crate::state::CredentialKind::OAuthBearer, false)
                .is_none()
        );
    }

    #[test]
    fn resolve_autogenerated_password_mints_a_fresh_value_each_call() {
        // Callers must call this only at a settled edge and store the result —
        // this test just pins that back-to-back calls aren't somehow memoized.
        let a = resolve_autogenerated_password(crate::state::CredentialKind::Plain, true).unwrap();
        let b = resolve_autogenerated_password(crate::state::CredentialKind::Plain, true).unwrap();
        assert_ne!(a.as_str(), b.as_str());
    }

    #[test]
    fn warn_only_when_manual_on_encrypted() {
        assert!(warn_manual_password(false, true)); // manual + encrypted → warn
        assert!(!warn_manual_password(true, true)); // auto + encrypted → no warn
        assert!(!warn_manual_password(false, false)); // manual + plaintext → no warn
        assert!(!warn_manual_password(true, false)); // auto + plaintext → no warn
    }

    #[test]
    fn strength_label_buckets() {
        assert_eq!(password_strength_label(""), None);
        let key = |p: &str| password_strength_label(p).unwrap().key;
        assert_eq!(key("short"), "settings.mail.strength_weak"); // 5 < 8
        assert_eq!(key("eightchr"), "settings.mail.strength_fair"); // 8 → fair
        assert_eq!(key("mediumlength1234"), "settings.mail.strength_strong"); // 16 → strong
        // A generated 24-char password reads Strong.
        assert_eq!(
            key(generate_bridge_password().as_str()),
            "settings.mail.strength_strong"
        );
    }
}
