//! The `bsky.app` web address of a Bluesky post, from its AT-URI — the ONE
//! conversion every "open this bridged post off-app" arm shares
//! (`behavior/notifications.md` § Deep-link destinations, the `External`
//! destination; user-ruled 2026-09-25: a bridged notification hands its
//! subject to the OS browser rather than staying informational).
//!
//! An `app.bsky.feed.post` record lives at
//! `at://<authority>/app.bsky.feed.post/<rkey>`, and bsky.app serves the same
//! post at `https://bsky.app/profile/<authority>/post/<rkey>` — the authority
//! is the author's DID or handle, and bsky.app resolves either. Anything else
//! is `None`: a like or follow record, a profile, an empty or malformed string
//! — there is no post page to send the user to, and guessing one would hand
//! the OS browser an address that 404s. The AT-URI is third-party data the
//! nest relays verbatim from the AppView, so every segment is validated
//! against the AT Protocol's own charsets before it is spliced into the URL:
//! a subject carrying `/`, `?` or `#` where a record key belongs cannot steer
//! the browser anywhere but a post page on the one fixed origin.
//!
//! WASM-safe, string-shape only (no resolution, no I/O).

/// The one web origin a bridged Bluesky post is opened at. A constant, not a
/// knob: no user or admin would choose a different AppView front-end for a
/// notification tap (`principles.md` § One configuration surface).
pub const BSKY_APP_ORIGIN: &str = "https://bsky.app";

const AT_SCHEME: &str = "at://";
const POST_COLLECTION: &str = "app.bsky.feed.post";

/// The AT Protocol caps a record key at 512 bytes; a longer one is malformed,
/// not merely unusual.
const RKEY_MAX_LEN: usize = 512;

/// `https://bsky.app/profile/<authority>/post/<rkey>` for the post record
/// `at_uri` names, or `None` when it names no post — a non-post collection,
/// a missing or trailing segment, a segment outside the AT Protocol's charset,
/// or a string that is not an AT-URI at all.
pub fn post_web_url(at_uri: &str) -> Option<String> {
    let rest = at_uri.strip_prefix(AT_SCHEME)?;
    let mut parts = rest.split('/');
    let authority = parts.next()?;
    let collection = parts.next()?;
    let rkey = parts.next()?;
    if parts.next().is_some() || collection != POST_COLLECTION {
        return None;
    }
    if !is_authority(authority) || !is_rkey(rkey) {
        return None;
    }
    Some(format!("{BSKY_APP_ORIGIN}/profile/{authority}/post/{rkey}"))
}

/// A DID (`did:<method>:<id>`, whose id charset is `[A-Za-z0-9._:%-]`) or a
/// handle (a hostname: `[A-Za-z0-9.-]`) — the union charset, non-empty.
fn is_authority(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'%' | b'-'))
}

/// A record key: `[A-Za-z0-9._:~-]{1,512}`, never `.` or `..`.
fn is_rkey(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= RKEY_MAX_LEN
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'~' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_post_record_maps_to_its_bsky_app_page() {
        assert_eq!(
            post_web_url("at://did:plc:z72i7hdynmk6r22z27h6tvur/app.bsky.feed.post/3kfhq2xkmqj2c")
                .as_deref(),
            Some("https://bsky.app/profile/did:plc:z72i7hdynmk6r22z27h6tvur/post/3kfhq2xkmqj2c")
        );
    }

    /// bsky.app resolves a handle authority exactly as it does a DID, and the
    /// AppView may hand out either form.
    #[test]
    fn a_handle_authority_maps_too() {
        assert_eq!(
            post_web_url("at://alice.bsky.social/app.bsky.feed.post/3kfhq2xkmqj2c").as_deref(),
            Some("https://bsky.app/profile/alice.bsky.social/post/3kfhq2xkmqj2c")
        );
    }

    /// A like or follow record, a profile — records bsky.app has no page for.
    /// The notification's OWN record URI is one of these for a like/follow,
    /// which is exactly why the router reads `subject_uri`, never `content_id`.
    #[test]
    fn a_non_post_record_has_no_page() {
        for uri in [
            "at://did:plc:xyz/app.bsky.feed.like/3kabc",
            "at://did:plc:xyz/app.bsky.graph.follow/3kabc",
            "at://did:plc:xyz/app.bsky.actor.profile/self",
            "at://did:plc:xyz/app.bsky.feed.repost/3kabc",
        ] {
            assert_eq!(post_web_url(uri), None, "{uri} must not map to a post page");
        }
    }

    #[test]
    fn a_malformed_or_partial_at_uri_is_none() {
        for uri in [
            "",
            "at://",
            "at://did:plc:xyz",
            "at://did:plc:xyz/app.bsky.feed.post",
            "at://did:plc:xyz/app.bsky.feed.post/",
            "at:///app.bsky.feed.post/3kabc",
            "https://bsky.app/profile/did:plc:xyz/post/3kabc",
            "did:plc:xyz/app.bsky.feed.post/3kabc",
        ] {
            assert_eq!(post_web_url(uri), None, "{uri:?} must be None");
        }
    }

    /// The subject is relayed third-party data. A segment carrying a path,
    /// query or fragment delimiter — or anything outside the AT Protocol's
    /// charsets — is refused rather than spliced into the URL.
    #[test]
    fn a_segment_outside_the_at_protocol_charset_is_refused() {
        for uri in [
            "at://did:plc:xyz/app.bsky.feed.post/3kabc/extra",
            "at://did:plc:xyz/app.bsky.feed.post/3kabc?x=1",
            "at://did:plc:xyz/app.bsky.feed.post/3kabc#frag",
            "at://did:plc:xyz/app.bsky.feed.post/3k abc",
            "at://did:plc:xyz/app.bsky.feed.post/.",
            "at://did:plc:xyz/app.bsky.feed.post/..",
            "at://evil.example/../x/app.bsky.feed.post/3kabc",
            "at://did:plc:xyz?x/app.bsky.feed.post/3kabc",
            "at://did:plc:xyz/app.bsky.feed.post/3kabc\u{ff0f}x",
        ] {
            assert_eq!(post_web_url(uri), None, "{uri:?} must be refused");
        }
        let too_long = format!("at://did:plc:xyz/app.bsky.feed.post/{}", "a".repeat(513));
        assert_eq!(post_web_url(&too_long), None);
        let at_cap = format!("at://did:plc:xyz/app.bsky.feed.post/{}", "a".repeat(512));
        assert!(post_web_url(&at_cap).is_some());
    }
}
