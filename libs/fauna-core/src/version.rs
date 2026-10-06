//! Application release-version comparison + the canonical release source.
//!
//! Canonical, shared "is a newer release available?" semver check, used by the
//! client update-checkers (linux today; available to every app that polls a
//! release feed). Replaces the previously hand-rolled tuple parsers that had
//! drifted apart (lenient `u32` vs strict `u64`, opposite arg orders) and all
//! mishandled pre-release tags.
//!
//! Uses the [`semver`] crate so pre-release ordering and build metadata follow
//! the spec (`1.0.0-beta < 1.0.0 < 1.0.1`). Both inputs must be valid
//! `MAJOR.MINOR.PATCH` semver with **no** leading `v` (callers strip it). Any
//! parse failure is treated as "not newer" (`false`) — never prompt an update on
//! an unparseable version.
//!
//! Also home to [`RELEASE_REPO`], the single source of truth for *where* every
//! binary looks for releases, so the repo slug can never drift between checkers.

/// The canonical GitHub repository (`owner/name`) for Fauna releases.
///
/// Single source of truth for every update-checker — the `fauna-update`
/// self-update loop in `fauna-nest` and the linux desktop's
/// lightweight startup poll all read this, so a typo can never silently point
/// one binary at a nonexistent repo (the bug this constant retired: the linux
/// checker had hard-coded `fauna-social/fauna` while the rest used the correct
/// slug). Every release is published to `faunasocial/fauna`.
pub const RELEASE_REPO: &str = "faunasocial/fauna";

/// The GitHub REST endpoint that names the newest published release.
///
/// Every app's user-triggered "is a newer version out?" check reads this one
/// URL (the notify-only promise, `installers/README.md` § Knowing a newer
/// version is out); `base` is the API origin, `https://api.github.com` in
/// production, and the seam an e2e harness points at a stub feed.
pub fn latest_release_api_url(base: &str) -> String {
    format!(
        "{}/repos/{RELEASE_REPO}/releases/latest",
        base.trim_end_matches('/')
    )
}

/// The production API origin [`latest_release_api_url`] is called with.
pub const GITHUB_API_ORIGIN: &str = "https://api.github.com";

/// Where a user gets the release `tag` — the "here" in *"a newer version is
/// out, get it here"*: the release page every channel's artifacts hang off.
pub fn release_page_url(tag: &str) -> String {
    format!("https://github.com/{RELEASE_REPO}/releases/tag/{tag}")
}

/// Read the newest release out of the JSON body [`latest_release_api_url`]
/// answers with, and say whether it is newer than `current`.
///
/// Returns the release's tag (with its leading `v`, as GitHub spells it) when
/// it is strictly newer; `None` when the body is not the endpoint's shape, the
/// tag is not semver, or the release is not newer — every failure degrades to
/// "no update", never to a spurious prompt (the [`is_newer`] rule). Shared so
/// that no app parses the feed on its own: what counts as newer *and* what the
/// feed looks like are decided once.
pub fn newer_release_from_latest_json(current: &str, body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let tag = json.get("tag_name")?.as_str()?;
    let remote = tag.trim_start_matches('v');
    is_newer(current, remote).then(|| tag.to_owned())
}

/// Returns `true` if `candidate` is a strictly newer release than `current`.
///
/// Both arguments are semver strings without a leading `v`. A parse failure on
/// either side returns `false` (the safe default: no spurious update prompt).
pub fn is_newer(current: &str, candidate: &str) -> bool {
    match (
        semver::Version::parse(current),
        semver::Version::parse(candidate),
    ) {
        // `cmp_precedence` follows semver *precedence* (build metadata ignored),
        // unlike `Ord`, which orders by build metadata to stay consistent with
        // `Eq`. Precedence is what "is a newer release?" actually means.
        (Ok(current), Ok(candidate)) => {
            candidate.cmp_precedence(&current) == std::cmp::Ordering::Greater
        }
        // Either side unparseable -> "not newer" (no spurious update prompt).
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{RELEASE_REPO, is_newer};

    #[test]
    fn release_repo_is_canonical_owner_slash_name() {
        // `owner/name`, exactly one slash, no leading `v`/scheme/host.
        assert_eq!(RELEASE_REPO, "faunasocial/fauna");
        let (owner, name) = RELEASE_REPO
            .split_once('/')
            .expect("RELEASE_REPO must be `owner/name`");
        assert!(!owner.is_empty() && !name.is_empty());
        assert!(!name.contains('/'), "exactly one slash");
        // Guard the exact bug this constant retired: a hyphenated `fauna-social`
        // owner pointed the linux checker at a repo that does not exist.
        assert_eq!(owner, "faunasocial");
        assert!(!owner.contains('-'));
    }

    #[test]
    fn newer_patch_minor_major() {
        assert!(is_newer("1.2.3", "1.2.4"));
        assert!(is_newer("1.2.3", "1.3.0"));
        assert!(is_newer("1.2.3", "2.0.0"));
    }

    #[test]
    fn equal_is_not_newer() {
        assert!(!is_newer("1.2.3", "1.2.3"));
        assert!(!is_newer("0.1.0", "0.1.0"));
    }

    #[test]
    fn older_is_not_newer() {
        assert!(!is_newer("1.2.4", "1.2.3"));
        assert!(!is_newer("2.0.0", "1.9.9"));
        assert!(!is_newer("1.3.0", "1.2.9"));
    }

    #[test]
    fn double_digit_components_compare_numerically() {
        // Tuple/lexical parsers that don't widen would get this wrong.
        assert!(is_newer("1.9.0", "1.10.0"));
        assert!(!is_newer("1.10.0", "1.9.0"));
    }

    #[test]
    fn prerelease_orders_below_release() {
        // Per semver: a pre-release is older than its release.
        assert!(is_newer("1.0.0-beta", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0.0-beta"));
        // ...and pre-releases order among themselves.
        assert!(is_newer("1.0.0-alpha", "1.0.0-beta"));
        assert!(is_newer("1.0.0-rc1", "1.0.0"));
    }

    #[test]
    fn build_metadata_is_ignored() {
        assert!(!is_newer("1.2.3", "1.2.3+build.9"));
        assert!(!is_newer("1.2.3+build.1", "1.2.3+build.2"));
    }

    #[test]
    fn missing_component_fails_to_not_newer() {
        // "1.3" is not valid semver -> treated as not-newer (no spurious prompt),
        // even though it would look "newer" than "1.2.0" to a lenient parser.
        assert!(!is_newer("1.2.0", "1.3"));
        assert!(!is_newer("1.3", "1.2.0"));
    }

    #[test]
    fn garbage_input_is_not_newer() {
        assert!(!is_newer("1.2.3", "not-a-version"));
        assert!(!is_newer("", "1.2.3"));
        assert!(!is_newer("1.2.3", ""));
    }

    #[test]
    fn leading_v_is_not_stripped_here() {
        // Callers strip the leading 'v'; "v1.2.4" is not valid semver, so a
        // forgotten strip degrades safely to "no update" rather than misparsing.
        assert!(!is_newer("1.2.3", "v1.2.4"));
    }

    #[test]
    fn latest_release_url_is_the_repo_endpoint_on_the_given_origin() {
        assert_eq!(
            super::latest_release_api_url(super::GITHUB_API_ORIGIN),
            "https://api.github.com/repos/faunasocial/fauna/releases/latest"
        );
        // A stub feed's origin, with or without a trailing slash.
        assert_eq!(
            super::latest_release_api_url("http://127.0.0.1:8123/"),
            "http://127.0.0.1:8123/repos/faunasocial/fauna/releases/latest"
        );
    }

    #[test]
    fn release_page_url_names_the_tag() {
        assert_eq!(
            super::release_page_url("v0.2.0"),
            "https://github.com/faunasocial/fauna/releases/tag/v0.2.0"
        );
    }

    #[test]
    fn newer_release_is_read_off_the_latest_body() {
        let newer = super::newer_release_from_latest_json;
        assert_eq!(
            newer("0.1.2", r#"{"tag_name":"v0.2.0","html_url":"x"}"#),
            Some("v0.2.0".to_owned())
        );
        // Same or older → no update; the tag keeps GitHub's `v`.
        assert_eq!(newer("0.2.0", r#"{"tag_name":"v0.2.0"}"#), None);
        assert_eq!(newer("0.3.0", r#"{"tag_name":"v0.2.0"}"#), None);
        // Not the endpoint's shape, or not semver → no update, never a panic.
        assert_eq!(newer("0.1.2", "not json"), None);
        assert_eq!(newer("0.1.2", r#"{"message":"Not Found"}"#), None);
        assert_eq!(newer("0.1.2", r#"{"tag_name":"nightly"}"#), None);
    }
}
