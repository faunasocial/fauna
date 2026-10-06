//! Kebab-case credential-id derivation, shared by every client-minted
//! credential family.
//!
//! Lifted here from `fauna-client-mail-settings::credential` (2026-07-22) when
//! the ATProto app-credential machine needed the same derivation: both mint a
//! user-labelled credential whose stable id is a kebab-cased, collision-suffixed
//! form of that label, and a second copy would have been per-family divergence
//! in the one place where two credential families must agree (priority #4).
//! `fauna-client-mail-settings` re-exports it, so its call sites are unchanged.
//!
//! This crate is the shared home because it is the thin, wasm-clean base
//! dependency **both** consumers already carry — the same reason
//! `atproto_credential` lives here.

/// Derive a kebab-case `credential_id` from a user-supplied display label,
/// appending a numeric suffix on collision with `existing`: lowercase ASCII
/// alphanumeric segments separated by `-`, with non-alphanumeric runs collapsed
/// to a single `-` and leading/trailing separators trimmed. An empty or
/// entirely-non-alphanumeric label falls back to `"credential"`.
///
/// `existing` must carry **every** id the new one has to be distinct from. For
/// mail that is the client's own `fauna.state.mail` credential list; for ATProto app
/// credentials it is the union of the nest-listed rows and the local config
/// (the nest is the authority on which ids exist, since a sibling device may
/// have minted one this device's config has not synced yet).
///
/// Note for the mail family: `mail-credentials.md` treats the literal id
/// `"default"` as the "no suffix" case when rendering a MUA username via RFC
/// 5233 sub-addressing, but this function still returns the literal string
/// `"default"` — suffix omission is a render-time concern, not an
/// id-generation one.
pub fn derive_credential_id(display_name: &str, existing: &[String]) -> String {
    // Normalize: lowercase, collapse non-alnum runs to a single `-`, trim
    // leading/trailing `-`.
    let mut buf = String::with_capacity(display_name.len());
    let mut prev_was_sep = true;
    for c in display_name.chars() {
        if c.is_ascii_alphanumeric() {
            buf.push(c.to_ascii_lowercase());
            prev_was_sep = false;
        } else if !prev_was_sep {
            buf.push('-');
            prev_was_sep = true;
        }
    }
    while buf.ends_with('-') {
        buf.pop();
    }
    let base = if buf.is_empty() {
        "credential".to_string()
    } else {
        buf
    };
    if !existing.iter().any(|e| e == &base) {
        return base;
    }
    for n in 2.. {
        let candidate = format!("{base}-{n}");
        if !existing.iter().any(|e| e == &candidate) {
            return candidate;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_normalizes_spaces_and_capitals() {
        assert_eq!(derive_credential_id("iPhone Mail", &[]), "iphone-mail");
        assert_eq!(derive_credential_id("Default", &[]), "default");
    }

    #[test]
    fn punctuation_collapses_to_single_dash() {
        assert_eq!(derive_credential_id("Mac's Mail!!", &[]), "mac-s-mail");
        assert_eq!(derive_credential_id("---bad---", &[]), "bad");
    }

    #[test]
    fn empty_normalizes_to_fallback() {
        assert_eq!(derive_credential_id("", &[]), "credential");
        assert_eq!(derive_credential_id("!!!", &[]), "credential");
    }

    #[test]
    fn collision_appends_numeric_suffix() {
        let existing = vec!["iphone-mail".to_string()];
        assert_eq!(
            derive_credential_id("iPhone Mail", &existing),
            "iphone-mail-2"
        );
        let existing = vec!["iphone-mail".to_string(), "iphone-mail-2".into()];
        assert_eq!(
            derive_credential_id("iPhone Mail", &existing),
            "iphone-mail-3"
        );
    }

    #[test]
    fn non_ascii_label_falls_back_rather_than_emitting_non_ascii_ids() {
        // Only ASCII alphanumerics survive, so a wholly non-ASCII label lands on
        // the fallback instead of producing an id the wire vocabulary (kebab
        // ASCII) does not admit.
        assert_eq!(derive_credential_id("привет", &[]), "credential");
        assert_eq!(derive_credential_id("Ivory 📱", &[]), "ivory");
    }
}
