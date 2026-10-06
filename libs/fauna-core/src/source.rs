//! Post **source tokens** — the one-word origin/protocol token the nest
//! indexes per post (`content_meta.source`, read back as the wire `source`
//! field `fauna_feed::classify_sources` turns into badges).
//!
//! One home for the vocabulary (priority #3): the nest's index derivation
//! (`extract_post_metadata`), its interact door, `fauna_feed::SourceKind` and
//! `fauna_archive::Platform` all read these constants instead of re-spelling
//! the strings.
//!
//! Two families:
//!
//! - [`NATIVE`] — a post authored on Fauna with no other origin. The nest's
//!   client create path indexed exactly this token for every post before
//!   `Post.origin` existed.
//! - [`ARCHIVE_PLATFORMS`] — a post the account **re-authored from an export
//!   archive** (`docs/goal/behavior/archive-import.md` § What each category
//!   becomes → *The post origin field*). It is a native signed post — its
//!   likes, reposts and replies are signed Fauna posts referencing it, never a
//!   bridge interact — whose *origin* is another platform; the token names
//!   that platform so the feed badge can.
//!
//! Bridge tokens (`bluesky`, `nostr`, `activitypub`, `email`) are NOT here: a
//! bridge writes its own token through `put_post_with_source`, and those posts
//! are not native (their interactions travel through the bridge).

/// The token of a post authored on Fauna with no other origin.
pub const NATIVE: &str = "fauna";

/// Facebook export archive.
pub const FACEBOOK: &str = "facebook";

/// Instagram export archive.
pub const INSTAGRAM: &str = "instagram";

/// Every archive-import platform token. Additive: a new platform lands here,
/// in `fauna_archive::Platform`, and in `fauna_feed::SourceKind` together.
pub const ARCHIVE_PLATFORMS: &[&str] = &[FACEBOOK, INSTAGRAM];

/// Longest token [`normalize`] accepts. A token is one lowercase word — the
/// wire `source` field is a comma-separated *list* of them, so a comma can
/// never be inside one, and a nest never indexes an unbounded string a client
/// chose.
pub const MAX_TOKEN_LEN: usize = 32;

/// Normalize a client-supplied token to the indexed form: trimmed, ASCII
/// lowercase, `1..=MAX_TOKEN_LEN` bytes, every byte in `[a-z0-9_-]`.
/// `None` for anything else — the caller falls back to [`NATIVE`].
pub fn normalize(token: &str) -> Option<String> {
    let t = token.trim().to_ascii_lowercase();
    if t.is_empty() || t.len() > MAX_TOKEN_LEN {
        return None;
    }
    if !t
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return None;
    }
    Some(t)
}

/// Whether a post carrying `token` is a **native Fauna post** — [`NATIVE`] or
/// an [`ARCHIVE_PLATFORMS`] token — as opposed to bridged content whose
/// canonical home is another network. Tolerates the un-normalized spelling
/// (`" Facebook "`); anything [`normalize`] refuses is not native.
pub fn is_native(token: &str) -> bool {
    match normalize(token) {
        Some(t) => t == NATIVE || ARCHIVE_PLATFORMS.contains(&t.as_str()),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vocabulary_is_pinned() {
        // Wire tokens: changing one is a compat break, not a rename.
        assert_eq!(NATIVE, "fauna");
        assert_eq!(FACEBOOK, "facebook");
        assert_eq!(INSTAGRAM, "instagram");
        assert_eq!(ARCHIVE_PLATFORMS, &[FACEBOOK, INSTAGRAM]);
        for t in ARCHIVE_PLATFORMS {
            assert_eq!(
                normalize(t).as_deref(),
                Some(*t),
                "{t} is already canonical"
            );
        }
    }

    #[test]
    fn normalize_trims_lowercases_and_refuses_non_tokens() {
        assert_eq!(normalize(" Facebook ").as_deref(), Some("facebook"));
        assert_eq!(normalize("my_platform-2").as_deref(), Some("my_platform-2"));
        assert_eq!(normalize(""), None);
        assert_eq!(normalize("   "), None);
        // A comma would let one token impersonate a source LIST.
        assert_eq!(normalize("fauna, bluesky"), None);
        assert_eq!(normalize("face book"), None);
        assert_eq!(normalize("ünïcode"), None);
        assert_eq!(
            normalize(&"x".repeat(MAX_TOKEN_LEN)).map(|t| t.len()),
            Some(MAX_TOKEN_LEN)
        );
        assert_eq!(normalize(&"x".repeat(MAX_TOKEN_LEN + 1)), None);
    }

    #[test]
    fn native_is_fauna_or_an_archive_platform() {
        assert!(is_native("fauna"));
        assert!(is_native(" FAUNA "));
        assert!(is_native("facebook"));
        assert!(is_native("Instagram"));
        assert!(!is_native("bluesky"));
        assert!(!is_native("nostr"));
        assert!(!is_native("activitypub"));
        assert!(!is_native("email"));
        assert!(!is_native(""));
        assert!(!is_native("fauna, bluesky"));
        assert!(!is_native("rss"));
    }
}
