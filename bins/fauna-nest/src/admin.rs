//! Admin shared cores.
//!
//! The admin HTTP route handlers (the deprecated twins of the `fauna.admin.*`
//! WS-RPC kinds) were deleted once every
//! app moved onto WS-RPC; the WS-RPC handlers live in `admin_ws_handlers`.
//! Only the transport-agnostic core(s) those handlers still call remain here.

/// Mint a random, human-friendly invite code: 10 characters from an unambiguous
/// uppercase alphabet (no `0`/`O`/`1`/`I`/`L`). Used by the WS-RPC
/// `fauna.admin.invite_codes.create` handler when the admin supplies no code.
/// The byte→char modulo over a 31-char alphabet carries a negligible bias
/// (harmless for a non-cryptographic token); ~49 bits of entropy is ample, and a
/// rare collision surfaces as `fauna.admin.conflict` (the caller retries).
pub fn generate_invite_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789"; // 31 chars, unambiguous
    let mut bytes = [0u8; 10];
    getrandom::fill(&mut bytes).expect("getrandom failed");
    bytes
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect()
}

/// The BLAKE3 `derive_key` context for [`invite_code_audit_fingerprint`]'s key —
/// domain-separates it from the deployment seed's signing use and from every
/// other derivation (`cursor_seal`, `nest_kek`, …).
const INVITE_CODE_AUDIT_CONTEXT: &str = "fauna.nest.audit.invite_code.v1";

/// What the audit log stores in place of an invite code: a keyed, truncated
/// BLAKE3 fingerprint, `invite-fp:<16 hex>`.
///
/// An invite code is a bearer credential — `CacheDb::validate_invite_code`
/// redeems it on the string alone — and an audit row never stores a bearer
/// credential (see `db::admin::audit_on_conn`): the acting admin's rows ride
/// their own account export, which is retrievable with an eviction export
/// token, so a live code in `target` would be an enrolment capability that
/// outlives the admin's removal. The fingerprint keeps `invite.create` and
/// `invite.delete` of one code matchable in the admin audit view.
///
/// **Keyed, not a plain hash**, because the preimage space is small: an
/// admin-typed code can be a guessable word, and even a minted one is ~49 bits,
/// so an unkeyed digest would be an offline guessing oracle. The key is a
/// subkey of the durable deployment seed (the `cursor_seal` pattern, so no new
/// key material) that no export carries. Not stable across a deployment-key
/// rotation: a code minted before one and deleted after it fingerprints
/// differently — acceptable for a matching aid, never an identifier.
pub fn invite_code_audit_fingerprint(deployment_seed: &[u8; 32], code: &str) -> String {
    let key = blake3::derive_key(INVITE_CODE_AUDIT_CONTEXT, deployment_seed);
    let digest = blake3::keyed_hash(&key, code.as_bytes());
    format!("invite-fp:{}", hex::encode(&digest.as_bytes()[..8]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_audit_fingerprint_matches_itself_and_never_spells_the_code() {
        let seed = [0x11u8; 32];
        let a = invite_code_audit_fingerprint(&seed, "WELCOME");
        assert_eq!(a, invite_code_audit_fingerprint(&seed, "WELCOME"));
        assert_ne!(a, invite_code_audit_fingerprint(&seed, "WELCOMF"));
        // Keyed: another nest's seed fingerprints the same code differently,
        // so the digest is no guessing oracle without this nest's seed.
        assert_ne!(a, invite_code_audit_fingerprint(&[0x22u8; 32], "WELCOME"));
        assert!(a.starts_with("invite-fp:") && a.len() == "invite-fp:".len() + 16);
        assert!(!a.contains("WELCOME"));
    }
}
