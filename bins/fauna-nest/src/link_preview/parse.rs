//! OpenGraph / `<meta>` extraction for the D4 link-preview resolver
//! (`render-model.md` § D4). Pure: HTML bytes (already fetched, size-capped, and
//! SSRF-guarded by [`super::fetch`]) → [`ParsedMeta`]. No I/O.
//!
//! Built on `scraper` (the same `html5ever` tree-builder the inbound-HTML
//! `htmd` path uses), so malformed third-party markup parses to the same robust
//! DOM a browser would build rather than tripping a hand-rolled scan.

use scraper::{Html, Selector};
use url::Url;

/// Per-field caps so one hostile page cannot bloat the wire reply / URL cache.
/// The card truncates again for display (per-app) — these only bound storage.
const MAX_TITLE_LEN: usize = 300;
const MAX_DESCRIPTION_LEN: usize = 1000;

/// Metadata extracted from a fetched HTML page for a link-preview card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedMeta {
    /// `og:title` → `<title>` fallback. Empty when neither is present.
    pub title: String,
    /// `og:description` → `<meta name="description">` fallback. Empty when neither.
    pub description: String,
    /// `og:image`, resolved to an absolute URL against the page's final
    /// (post-redirect) base. `None` when absent or unresolvable.
    pub image_url: Option<String>,
}

/// Parse OpenGraph + standard `<meta>`/`<title>` from `html`. `base_url` is the
/// page's final (post-redirect) URL, used to resolve a relative `og:image`.
pub fn parse_meta(html: &str, base_url: &Url) -> ParsedMeta {
    // `Html` is `!Send` (tendril); keep it confined to this sync fn so callers
    // never hold it across an `.await`.
    let doc = Html::parse_document(html);

    let title = cap(
        meta_content(&doc, "og:title")
            .or_else(|| title_tag(&doc))
            .unwrap_or_default(),
        MAX_TITLE_LEN,
    );
    let description = cap(
        meta_content(&doc, "og:description")
            .or_else(|| meta_content(&doc, "description"))
            .unwrap_or_default(),
        MAX_DESCRIPTION_LEN,
    );
    let image_url = meta_content(&doc, "og:image")
        .and_then(|raw| base_url.join(raw.trim()).ok())
        // Only http(s) image refs — the fetcher rejects other schemes anyway,
        // and this keeps a `javascript:`/`data:` `og:image` from ever reaching it.
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .map(|u| u.to_string());

    ParsedMeta {
        title,
        description,
        image_url,
    }
}

/// First non-empty `content` of `<meta property="key">` or `<meta name="key">`.
/// Checks `property` first (the OpenGraph spec form) then `name` (the Twitter-card
/// / legacy form some sites use even for `og:` keys).
fn meta_content(doc: &Html, key: &str) -> Option<String> {
    for attr in ["property", "name"] {
        // `key` is a known literal (`og:title`, `description`, …); the `{key:?}`
        // Debug-quoting yields a valid quoted CSS attribute value (the colon in
        // `og:title` is fine inside quotes).
        let Ok(sel) = Selector::parse(&format!("meta[{attr}={key:?}]")) else {
            continue;
        };
        if let Some(content) = doc
            .select(&sel)
            .find_map(|el| el.value().attr("content"))
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(content.to_string());
        }
    }
    None
}

/// Trimmed text of the first `<title>` element, if non-empty.
fn title_tag(doc: &Html) -> Option<String> {
    let sel = Selector::parse("title").ok()?;
    doc.select(&sel)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Truncate to at most `max` *characters* (never splitting a UTF-8 boundary).
fn cap(mut s: String, max: usize) -> String {
    if s.chars().count() > max {
        s = s.chars().take(max).collect();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://example.com/articles/the-page").unwrap()
    }

    #[test]
    fn full_opengraph() {
        let html = r#"<html><head>
            <meta property="og:title" content="The Title">
            <meta property="og:description" content="A description.">
            <meta property="og:image" content="https://cdn.example.com/x.png">
            <title>fallback title</title>
        </head><body>hi</body></html>"#;
        let m = parse_meta(html, &base());
        assert_eq!(m.title, "The Title");
        assert_eq!(m.description, "A description.");
        assert_eq!(
            m.image_url.as_deref(),
            Some("https://cdn.example.com/x.png")
        );
    }

    #[test]
    fn falls_back_to_title_and_meta_description() {
        let html = r#"<html><head>
            <title>  Plain Title  </title>
            <meta name="description" content="Meta desc">
        </head></html>"#;
        let m = parse_meta(html, &base());
        assert_eq!(m.title, "Plain Title");
        assert_eq!(m.description, "Meta desc");
        assert_eq!(m.image_url, None);
    }

    #[test]
    fn og_title_wins_over_title_tag() {
        let html = r#"<head><meta property="og:title" content="OG"><title>Tag</title></head>"#;
        assert_eq!(parse_meta(html, &base()).title, "OG");
    }

    #[test]
    fn relative_og_image_resolves_against_base() {
        let html = r#"<head><meta property="og:image" content="/static/card.jpg"></head>"#;
        let m = parse_meta(html, &base());
        assert_eq!(
            m.image_url.as_deref(),
            Some("https://example.com/static/card.jpg")
        );
    }

    #[test]
    fn protocol_relative_og_image_resolves() {
        let html = r#"<head><meta property="og:image" content="//cdn.example.com/y.png"></head>"#;
        let m = parse_meta(html, &base());
        assert_eq!(
            m.image_url.as_deref(),
            Some("https://cdn.example.com/y.png")
        );
    }

    #[test]
    fn non_http_og_image_rejected() {
        let html =
            r#"<head><meta property="og:image" content="data:image/png;base64,AAAA"></head>"#;
        assert_eq!(parse_meta(html, &base()).image_url, None);
    }

    #[test]
    fn name_attr_variant_for_og_keys() {
        // Some sites emit `name="og:title"` instead of the spec `property=`.
        let html = r#"<head><meta name="og:title" content="Named OG"></head>"#;
        assert_eq!(parse_meta(html, &base()).title, "Named OG");
    }

    #[test]
    fn entities_are_decoded() {
        let html = r#"<head><meta property="og:title" content="Tom &amp; Jerry &lt;3"></head>"#;
        assert_eq!(parse_meta(html, &base()).title, "Tom & Jerry <3");
    }

    #[test]
    fn empty_content_is_skipped_for_fallback() {
        let html = r#"<head>
            <meta property="og:title" content="   ">
            <title>Real Title</title>
        </head>"#;
        assert_eq!(parse_meta(html, &base()).title, "Real Title");
    }

    #[test]
    fn no_metadata_yields_empty() {
        let m = parse_meta("<html><body><p>nothing</p></body></html>", &base());
        assert_eq!(m, ParsedMeta::default());
    }

    #[test]
    fn long_fields_are_capped() {
        let long = "x".repeat(5000);
        let html = format!(
            r#"<head><meta property="og:title" content="{long}"><meta property="og:description" content="{long}"></head>"#
        );
        let m = parse_meta(&html, &base());
        assert_eq!(m.title.chars().count(), MAX_TITLE_LEN);
        assert_eq!(m.description.chars().count(), MAX_DESCRIPTION_LEN);
    }

    #[test]
    fn cap_respects_char_boundaries() {
        // Multibyte chars must not be split mid-codepoint.
        let s = "é".repeat(400);
        let html = format!(r#"<head><meta property="og:title" content="{s}"></head>"#);
        let m = parse_meta(&html, &base());
        assert_eq!(m.title.chars().count(), MAX_TITLE_LEN);
        // Round-trips as valid UTF-8 (no panic, no replacement chars).
        assert!(m.title.chars().all(|c| c == 'é'));
    }
}
