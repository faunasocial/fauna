//! The `fauna://` **in-app routes**: a URI an OS surface outside the app opens
//! to bring the app forward on one of its own surfaces. Today's producer is the
//! Windows Explorer **Share** leaf, which hands off to the app instead of the
//! seedless sync agent minting a link (`docs/goal/behavior/share-links.md`
//! § Windows Explorer's Share leaf; the mechanism is
//! `docs/goal/architecture/apps/windows.md` § Shell Extension → *The Share
//! hand-off*).
//!
//! One grammar, spelled here and nowhere else, so the shell's builder and the
//! app's parser cannot drift:
//!
//! - `fauna://share-link?folder=<id>&path=<rel>` — open the file `rel` of the set
//!   whose durable id is `<id>` on its detail surface, with the share-link create
//!   surface open.
//! - `fauna://folder-share?folder=<id>` — open the Folders page with that set's
//!   member-share flow open.
//! - `fauna://consent/<request_uri>` — open the connected-apps page's consent
//!   card for the pushed authorization request `<request_uri>`: the same-device
//!   handoff start, where a device app that has just pushed its request hands
//!   the user to the Fauna app on the same device
//!   (`docs/goal/behavior/authorization-server.md` § Consent). The handle rides
//!   as the one path segment, not a query, and is exactly what PAR minted
//!   ([`PAR_REQUEST_URI_PREFIX`] over a non-empty token).
//!
//! **A route is navigation only.** Applying one opens a surface the user must
//! still act on; it never mints or grants anything, and what it may write is
//! only the pending request that surface shows (the consent route opens the
//! row the card renders, as the browser's `GET /oauth/authorize` does — a row
//! the user must still answer). That is what makes a `fauna://` URI from any
//! source — another program, a web page — safe to honour, and it is the
//! contract every new route must keep.
//!
//! The query values are percent-encoded (RFC 3986 unreserved characters pass
//! through; every other UTF-8 byte is `%XX`), so a path carrying `&`, `=`, `#`,
//! spaces or non-ASCII survives the round trip. The consent handle's `:`s are
//! written raw (legal in a path segment) and accepted escaped.

/// An in-app route (module doc).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum AppRoute {
    /// Open `path` (forward-slash, folder-relative) of set `folder_id` with the
    /// share-link create surface open.
    ShareLink { folder_id: i64, path: String },
    /// Open the Folders page with set `folder_id`'s member-share flow open.
    FolderShare { folder_id: i64 },
    /// Open the consent card for the pushed authorization request
    /// `request_uri`. Revealing the card is all it does — the approval stays
    /// the user's act on the card.
    Consent { request_uri: String },
}

/// The RFC 9126 URN prefix every `request_uri` the nest's PAR endpoint mints
/// starts with — spelled once, for the minter and the consent route alike.
pub const PAR_REQUEST_URI_PREFIX: &str = "urn:ietf:params:oauth:request_uri:";

const SCHEME: &str = "fauna://";
const SHARE_LINK: &str = "share-link";
const FOLDER_SHARE: &str = "folder-share";
const CONSENT: &str = "consent";

impl AppRoute {
    /// The route's URI.
    pub fn to_uri(&self) -> String {
        match self {
            Self::ShareLink { folder_id, path } => format!(
                "{SCHEME}{SHARE_LINK}?folder={folder_id}&path={}",
                percent_encode(path)
            ),
            Self::FolderShare { folder_id } => format!("{SCHEME}{FOLDER_SHARE}?folder={folder_id}"),
            Self::Consent { request_uri } => {
                format!("{SCHEME}{CONSENT}/{}", percent_encode_segment(request_uri))
            }
        }
    }

    /// Parse a route URI. `None` for anything that is not exactly one of the
    /// routes above — an unknown route, a missing or malformed parameter, an
    /// empty path, or a path that climbs (`..`) or is absolute. The scheme and
    /// route name are case-insensitive; surrounding whitespace is ignored.
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        let head = trimmed.get(..SCHEME.len())?;
        if !head.eq_ignore_ascii_case(SCHEME) {
            return None;
        }
        let rest = &trimmed[SCHEME.len()..];
        if let Some(route) = parse_consent(rest) {
            return route;
        }
        // A trailing slash before the query is what some shells append.
        let (name, query) = rest.split_once('?')?;
        let name = name.trim_end_matches('/');

        let mut folder = None;
        let mut path = None;
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=')?;
            match key {
                "folder" if folder.is_none() => folder = Some(value.parse::<i64>().ok()?),
                "path" if path.is_none() => path = Some(percent_decode(value)?),
                // A repeated or unknown parameter means this is not a URI this
                // build minted — refuse rather than guess which one was meant.
                _ => return None,
            }
        }
        let folder_id = folder?;

        if name.eq_ignore_ascii_case(SHARE_LINK) {
            let path = path?;
            if !is_relative_path(&path) {
                return None;
            }
            Some(Self::ShareLink { folder_id, path })
        } else if name.eq_ignore_ascii_case(FOLDER_SHARE) && path.is_none() {
            Some(Self::FolderShare { folder_id })
        } else {
            None
        }
    }
}

/// The consent route's path form: `Some(parsed)` when `rest` names the consent
/// route (so the query routes never see it), `None` when it names another.
fn parse_consent(rest: &str) -> Option<Option<AppRoute>> {
    let (name, segment) = rest.split_once('/')?;
    if !name.eq_ignore_ascii_case(CONSENT) {
        return None;
    }
    // A trailing slash is what some shells append; a second segment, a query or
    // a fragment is not a URI this build minted.
    let segment = segment.strip_suffix('/').unwrap_or(segment);
    if segment.contains(['/', '?', '#']) {
        return Some(None);
    }
    Some(
        percent_decode(segment)
            .filter(|handle| is_par_handle(handle))
            .map(|request_uri| AppRoute::Consent { request_uri }),
    )
}

/// The shape of a handle PAR mints: the URN prefix over a non-empty token of
/// URL-safe characters (no `/`, so it stays one segment). Public so the nest
/// door that consumes a route's handle refuses exactly what this grammar would
/// not parse.
pub fn is_par_handle(handle: &str) -> bool {
    handle
        .strip_prefix(PAR_REQUEST_URI_PREFIX)
        .is_some_and(|token| {
            !token.is_empty()
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
}

/// A non-empty, forward-slash, folder-relative path with no `..`, `.` or empty
/// segment and no leading slash — the only shape a set member's path takes.
fn is_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// One path segment: unreserved characters and `:` pass through, `/` and
/// everything else is `%XX`.
fn percent_encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b':') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_link_round_trips_an_awkward_path() {
        let route = AppRoute::ShareLink {
            folder_id: 42,
            path: "photos/Sommer & Sol/bål #1 = 100%.jpg".into(),
        };
        let uri = route.to_uri();
        assert!(
            uri.starts_with("fauna://share-link?folder=42&path=photos/"),
            "{uri}"
        );
        assert!(!uri.contains(' ') && !uri.contains('#'), "{uri}");
        assert_eq!(AppRoute::parse(&uri), Some(route));
    }

    #[test]
    fn folder_share_round_trips() {
        let route = AppRoute::FolderShare { folder_id: 7 };
        assert_eq!(route.to_uri(), "fauna://folder-share?folder=7");
        assert_eq!(AppRoute::parse(&route.to_uri()), Some(route));
    }

    #[test]
    fn parse_is_lenient_on_case_whitespace_and_a_trailing_slash() {
        assert_eq!(
            AppRoute::parse("  FAUNA://Folder-Share/?folder=7\n"),
            Some(AppRoute::FolderShare { folder_id: 7 })
        );
    }

    const HANDLE: &str =
        "urn:ietf:params:oauth:request_uri:Zx9-_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789ab";

    #[test]
    fn consent_round_trips_a_par_handle_in_path_form() {
        let route = AppRoute::Consent {
            request_uri: HANDLE.into(),
        };
        assert_eq!(route.to_uri(), format!("fauna://consent/{HANDLE}"));
        assert_eq!(AppRoute::parse(&route.to_uri()), Some(route));
    }

    #[test]
    fn consent_parse_is_lenient_on_case_escapes_and_a_trailing_slash() {
        let want = Some(AppRoute::Consent {
            request_uri: HANDLE.into(),
        });
        assert_eq!(
            AppRoute::parse(&format!(" FAUNA://Consent/{HANDLE}/\n")),
            want
        );
        let escaped = HANDLE.replace(':', "%3A");
        assert_eq!(AppRoute::parse(&format!("fauna://consent/{escaped}")), want);
    }

    #[test]
    fn consent_refuses_anything_but_one_par_handle() {
        for bad in [
            "fauna://consent".to_string(),
            "fauna://consent/".to_string(),
            "fauna://consent/urn:ietf:params:oauth:request_uri:".to_string(), // empty handle
            "fauna://consent/https://evil.example/".to_string(),              // not a PAR handle
            format!("fauna://consent/{HANDLE}?folder=1"),                     // a query
            format!("fauna://consent/{HANDLE}/extra"),                        // two segments
            format!("fauna://consent/{HANDLE}%2Fx"),                          // escaped slash
            format!("fauna://consent/{HANDLE}#frag"),                         // a fragment
            format!("fauna://consent/{HANDLE}%ZZ"),                           // bad escape
            format!("fauna://folder-share/{HANDLE}"), // path on another route
        ] {
            assert_eq!(AppRoute::parse(&bad), None, "{bad:?} must not parse");
        }
    }

    #[test]
    fn parse_refuses_what_this_build_never_mints() {
        for bad in [
            "",
            "https://share-link?folder=1&path=a",
            "fauna://share-link?folder=1",                 // no path
            "fauna://share-link?path=a",                   // no folder
            "fauna://share-link?folder=x&path=a",          // non-numeric id
            "fauna://share-link?folder=1&path=",           // empty path
            "fauna://share-link?folder=1&path=%2Fetc",     // absolute
            "fauna://share-link?folder=1&path=a%2F..%2Fb", // climbs
            "fauna://share-link?folder=1&path=a&path=b",   // repeated
            "fauna://share-link?folder=1&path=a&mint=1",   // unknown param
            "fauna://share-link?folder=1&path=%ZZ",        // bad escape
            "fauna://folder-share?folder=1&path=a",        // path on a folder
            "fauna://consent?folder=1",                    // another route
            "fauna://recovery?secret=00",                  // a secret URI
        ] {
            assert_eq!(AppRoute::parse(bad), None, "{bad:?} must not parse");
        }
    }
}
