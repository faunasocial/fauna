//! ATProto app-credential minting — the D3 rung-1 *minted capability
//! credentials* (`docs/goal/behavior/atproto-pds-full.md` § Detailed design →
//! F1 detail).
//!
//! Credentials are **generated, never user-chosen** (the no-passwords
//! invariant bans user-chosen memorized secrets; minted machine credentials
//! for an external protocol surface are the sanctioned class — mail's
//! generated bridge password is the shipped precedent, and
//! `fauna-client-mail-settings::password_gen` is the pattern this module
//! mirrors). The shape is the ecosystem's app-password format
//! `xxxx-xxxx-xxxx-xxxx` — 16 symbols from a 32-symbol lowercase alphabet
//! (RFC 4648 base32, lowercased) ≈ 80 bits — fixed by third-party login
//! boxes, not by us; it rides Argon2id plus the bridge's rate-limit stack.
//!
//! Custody split (D3): the minting client keeps the recoverable copy in
//! `fauna.state.atproto`; the nest stores only the **PHC-serialized Argon2id
//! verifier** computed here, client-side, at mint — the nest never sees the
//! secret, even at provisioning. The bridge fetches verifier rows and
//! verifies at `createSession` (Go side; the PHC string carries its own
//! parameters, so any conformant Argon2id implementation verifies old
//! credentials forever).

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use fauna_core::secret::SecretString;
use rand::RngCore;

/// RFC 4648 base32 alphabet, lowercased — the ecosystem's app-password
/// symbol set. 32 symbols = 5 bits each; 16 symbols ≈ 80 bits.
const CHARSET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Four dash-joined groups of four symbols: `xxxx-xxxx-xxxx-xxxx`.
pub const APP_CREDENTIAL_GROUPS: usize = 4;
/// Symbols per group.
pub const APP_CREDENTIAL_GROUP_LEN: usize = 4;

/// Argon2id parameters for a freshly-minted credential's verifier — the
/// same cost surface as mail's PLAIN KDF (`mail-credentials.md` § KDF
/// choice: m = 64 MiB, t = 2, p = 1). Old verifiers keep verifying under
/// their own PHC-embedded parameters if these ever change.
fn verifier_params() -> Params {
    Params::new(65_536, 2, 1, None).expect("static Argon2id params are valid")
}

/// Generate a fresh app credential: `xxxx-xxxx-xxxx-xxxx`, 16 symbols from
/// the 32-symbol alphabet, OS CSPRNG. A 32-symbol alphabet divides the byte
/// range evenly (256 = 8 × 32), so `byte & 0x1f` is exactly unbiased — the
/// modulo-bias rejection loop `password_gen.rs` needs for its 62-symbol
/// charset has nothing to reject here.
pub fn generate_app_credential() -> SecretString {
    let mut bytes = [0u8; APP_CREDENTIAL_GROUPS * APP_CREDENTIAL_GROUP_LEN];
    rand::thread_rng().fill_bytes(&mut bytes);
    let mut out = String::with_capacity(bytes.len() + APP_CREDENTIAL_GROUPS - 1);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && i % APP_CREDENTIAL_GROUP_LEN == 0 {
            out.push('-');
        }
        out.push(CHARSET[(b & 0x1f) as usize] as char);
    }
    SecretString::new(out)
}

/// Compute the PHC-serialized Argon2id verifier for a credential secret —
/// run **client-side at mint**; the resulting string (parameters + salt +
/// hash, self-describing) is what `provision_app_credential` sends to the
/// nest. The secret's exact bytes (dashes included) are the password input:
/// the login box transmits what the user pasted, verbatim.
pub fn compute_app_credential_verifier(secret: &SecretString) -> String {
    let salt = SaltString::generate(&mut rand::rngs::OsRng);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, verifier_params())
        .hash_password(secret.as_str().as_bytes(), &salt)
        .expect("Argon2id hashing of a generated credential cannot fail")
        .to_string()
}

/// Verify a candidate secret against a PHC-serialized verifier. The bridge's
/// production verify path is the Go implementation (`internal/auth`); this
/// Rust twin exists for tests (incl. the cross-language fixture pinning the
/// two implementations to the same PHC format) and client-side self-checks.
pub fn verify_app_credential(candidate: &str, phc_verifier: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc_verifier) else {
        return false;
    };
    Argon2::new(Algorithm::Argon2id, Version::V0x13, verifier_params())
        .verify_password(candidate.as_bytes(), &parsed)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Light parameters so tests don't pay 64 MiB per hash; production
    /// strings verify under their own PHC-embedded params regardless.
    fn light_phc(secret: &str) -> String {
        let salt = SaltString::generate(&mut rand::rngs::OsRng);
        Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            Params::new(4096, 1, 1, None).unwrap(),
        )
        .hash_password(secret.as_bytes(), &salt)
        .unwrap()
        .to_string()
    }

    #[test]
    fn credential_shape_is_four_dash_joined_groups() {
        let c = generate_app_credential();
        let s = c.as_str();
        assert_eq!(s.len(), 19); // 16 symbols + 3 dashes
        let groups: Vec<&str> = s.split('-').collect();
        assert_eq!(groups.len(), APP_CREDENTIAL_GROUPS);
        for g in groups {
            assert_eq!(g.len(), APP_CREDENTIAL_GROUP_LEN);
            assert!(
                g.bytes().all(|b| CHARSET.contains(&b)),
                "bad symbol in {s:?}"
            );
        }
    }

    #[test]
    fn charset_invariant_over_many_draws() {
        for _ in 0..200 {
            let c = generate_app_credential();
            for b in c.as_str().bytes() {
                assert!(b == b'-' || CHARSET.contains(&b), "non-charset byte {b}");
            }
        }
    }

    #[test]
    fn successive_credentials_differ() {
        // 80-bit secrets: a repeat means broken RNG wiring, not chance.
        assert_ne!(
            generate_app_credential().as_str(),
            generate_app_credential().as_str()
        );
    }

    #[test]
    fn verifier_round_trip() {
        let secret = generate_app_credential();
        let phc = light_phc(secret.as_str());
        assert!(verify_app_credential(secret.as_str(), &phc));
        assert!(!verify_app_credential("aaaa-bbbb-cccc-dddd", &phc));
    }

    #[test]
    fn verifier_rejects_garbage_phc() {
        assert!(!verify_app_credential("whatever", "not-a-phc-string"));
        assert!(!verify_app_credential("whatever", ""));
    }

    #[test]
    fn production_verifier_is_selfdescribing_argon2id_phc() {
        // One production-cost hash (64 MiB, t=2) — keep it to a single test.
        let secret = generate_app_credential();
        let phc = compute_app_credential_verifier(&secret);
        assert!(
            phc.starts_with("$argon2id$v=19$m=65536,t=2,p=1$"),
            "unexpected PHC prefix: {phc}"
        );
        assert!(verify_app_credential(secret.as_str(), &phc));
    }
}

#[cfg(test)]
mod cross_language_fixture {
    use super::*;

    /// The committed fixture the Go verifier test also consumes — pins the
    /// Rust minter and the Go `internal/auth` verifier to one PHC format.
    /// Path is relative to this crate's manifest; the fixture lives beside
    /// its Go consumer.
    const FIXTURE_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../bins/fauna-bridges/internal/auth/testdata/atproto_app_credential_phc.txt"
    );

    #[test]
    fn cross_language_fixture_verifies() {
        let raw = std::fs::read_to_string(FIXTURE_PATH)
            .expect("cross-language fixture file present beside the Go verifier");
        let mut lines = raw.lines().filter(|l| !l.starts_with('#') && !l.is_empty());
        let secret = lines.next().expect("fixture secret line");
        let phc = lines.next().expect("fixture PHC line");
        assert!(
            verify_app_credential(secret, phc),
            "Rust verifier rejects the committed cross-language fixture"
        );
        assert!(!verify_app_credential("aaaa-bbbb-cccc-dddd", phc));
    }
}
