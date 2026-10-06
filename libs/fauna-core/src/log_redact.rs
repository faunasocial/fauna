//! Redact user-chosen file-sync names/paths before they reach a nest-side or
//! client-side log line or error string
//! (`docs/goal/behavior/file-sync.md` § Sealed names & paths — the log +
//! error-string scrub, slice S7). A log line is a confidentiality boundary
//! exactly like the wire and the DB: `fauna.admin.logs` hands the process-wide
//! log ring to any admin allowlisted for it, so an unredacted path/name in a
//! log line is as much a leak as an unsealed DB column would be.
//!
//! Each helper keeps enough of the field's own equality-only addressing hash
//! ([`crate::sync::path_hash`], [`crate::path_crypto::set_name_hash`]) to
//! correlate repeated log lines about the same file/set across a debugging
//! session, without ever carrying the plaintext.

use crate::path_crypto::set_name_hash;
use crate::sync::{is_reserved_folder_name, path_hash};

/// Bytes of the hash kept in a redacted log value — enough to distinguish
/// unrelated files/sets in one debugging session, short enough to keep log
/// lines readable.
///
/// The truncation buys **brevity and a narrow correlation window, NOT
/// dictionary resistance**: the underlying digests are unkeyed and publicly
/// computable (`encryption-at-rest.md` § Carve-outs declares that residual),
/// so 48 bits over a small plaintext space still identifies a guessable
/// name near-uniquely — the log form is NOT safe to ship off-box, exactly
/// like the full digest it prefixes.
const PREFIX_BYTES: usize = 6;

/// Redact a folder-relative path for a log line or error string. Always
/// hashed — unlike a folder name, a path has no reserved/routing-constant
/// class.
pub fn log_path(relative: &str) -> String {
    log_hash_prefix("path", &path_hash(relative))
}

/// Redact a folder name for a log line or error string. A reserved (`__`)
/// name is a routing constant, not user data
/// ([`is_reserved_folder_name`]), so it stays literal for readability;
/// every user-chosen name hashes.
pub fn log_folder_name(name: &str) -> String {
    if is_reserved_folder_name(name) {
        name.to_string()
    } else {
        log_hash_prefix("name", &set_name_hash(name))
    }
}

/// Format an already-computed hash (a stored `path_hash`/`name_hash` column,
/// or any other 32-byte digest) in the same short form [`log_path`] /
/// [`log_folder_name`] produce, for call sites that already hold the hash
/// and would otherwise have to re-hash the plaintext just to log it.
pub fn log_hash_prefix(kind: &str, hash: &[u8]) -> String {
    format!(
        "{kind}~{}",
        hex::encode(&hash[..hash.len().min(PREFIX_BYTES)])
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_path_never_contains_the_plaintext() {
        let redacted = log_path("Documents/2026 taxes/eviction_notice.pdf");
        assert!(!redacted.contains("eviction_notice"));
        assert!(!redacted.contains("taxes"));
        assert!(redacted.starts_with("path~"));
        // Deterministic — the same path always redacts to the same value, so
        // repeated log lines about one file correlate during debugging.
        assert_eq!(
            redacted,
            log_path("Documents/2026 taxes/eviction_notice.pdf")
        );
    }

    #[test]
    fn log_path_distinguishes_different_paths() {
        assert_ne!(log_path("a/b.txt"), log_path("a/c.txt"));
    }

    #[test]
    fn log_folder_name_hashes_a_user_chosen_name() {
        let redacted = log_folder_name("My Photos 2026");
        assert!(!redacted.contains("Photos"));
        assert!(redacted.starts_with("name~"));
    }

    #[test]
    fn log_folder_name_keeps_a_reserved_name_literal() {
        // Reserved names are routing constants (`fauna_core::sync::
        // is_reserved_folder_name`), identical on every deployment for
        // every account — hashing one protects nothing and costs every
        // reader a readable log.
        assert_eq!(log_folder_name("__mail"), "__mail");
        assert_eq!(log_folder_name("__conv/abcd"), "__conv/abcd");
    }

    #[test]
    fn log_hash_prefix_is_a_short_hex_prefix_of_the_given_hash() {
        let hash = path_hash("some/path.txt");
        let redacted = log_hash_prefix("path", &hash);
        assert_eq!(
            redacted,
            format!("path~{}", hex::encode(&hash[..PREFIX_BYTES]))
        );
    }
}
