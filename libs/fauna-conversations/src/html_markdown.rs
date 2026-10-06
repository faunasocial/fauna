//! Inbound HTML email → markdown, and the MIME part selection that feeds it.
//!
//! `docs/goal/behavior/html-mail.md`: email uses **markdown as the single
//! interchange format both ways**. Inbound, a received message's best body part
//! (the `text/html` alternative when present) is converted to markdown here and
//! stamped [`BodyFormat::Markdown`] by the SMTP backend, so every app renders
//! it through the markdown path it already has — no per-app HTML renderer.
//! Outbound markdown→HTML reuses [`fauna_core::markdown::markdown_to_html`] (an
//! already-sanitized subset) directly in [`crate::rfc5322`]; there is
//! deliberately no markdown→HTML function here (priority #2/#4 — reuse the
//! existing shared converter, don't add a second one).
//!
//! **Security perimeter** (the converters are a security-review surface):
//! - `script` / `style` / `head` / embedding tags (`iframe`, `object`, …) are
//!   dropped wholesale — htmd's default block handler would otherwise emit a
//!   `<script>`/`<style>` element's *raw text* as markdown.
//! - link / image destinations are scheme-gated: only `http(s)` (and `mailto:`
//!   for links) survive; `javascript:` / `data:` and other active schemes are
//!   neutralized (a link collapses to its text, an image is dropped).
//! - remote `http(s)` images survive as **un-fetched** markdown image refs; no
//!   client markdown renderer fetches them (the per-message "load remote
//!   content" opt-in is a later slice). Nothing here performs a network fetch.
//!
//! WASM-safe (`mail_parser` + `htmd`/`html5ever` are pure Rust): this runs
//! client-side in the SMTP backend on every app, web (WASM) included.

use crate::message::{AttachmentSnapshot, BodyFormat};
use htmd::{Element, HtmlToMarkdown};
// `content_type` / `attachment_name` on a `MessagePart` come from this trait.
use mail_parser::MimeHeaders;

/// Tags whose entire subtree is dropped during HTML→markdown conversion: active
/// content (`script`), presentation (`style`), document metadata (`head` and
/// friends), and embedding / active-object elements. Without this, htmd's
/// default block handler emits a `<script>`/`<style>` element's raw text as
/// markdown.
const DROP_TAGS: &[&str] = &[
    "script", "style", "head", "title", "meta", "link", "base", "noscript", "iframe", "frame",
    "frameset", "object", "embed", "applet", "svg", "template", "map", "area",
];

/// Convert an HTML email body to the markdown subset the apps render.
/// Infallible — a parse error (html5ever is effectively infallible on UTF-8)
/// yields an empty string rather than propagating.
pub fn html_to_markdown(html: &str) -> String {
    let converter = HtmlToMarkdown::builder()
        .skip_tags(DROP_TAGS.to_vec())
        .add_handler(vec!["a"], anchor_handler)
        .add_handler(vec!["img"], img_handler)
        .build();
    converter
        .convert(html)
        .map(|md| md.trim().to_string())
        .unwrap_or_default()
}

/// Select the best body of an inbound RFC 5322 message and return it as
/// `(body, format)`:
/// - a `text/html` part present → HTML→markdown + [`BodyFormat::Markdown`];
/// - otherwise the `text/plain` body verbatim + [`BodyFormat::PlainText`];
/// - an unparseable / body-less message → `("", PlainText)`.
///
/// `mail_parser` decodes transfer-encoding (base64 / quoted-printable) and
/// charset, and picks the html / plain alternative of a `multipart/alternative`
/// message — the "best alternative part" the goal doc calls for. Replaces the
/// old hard-coded `BodyFormat::PlainText` + raw-MIME-as-body behaviour.
pub fn inbound_mail_body(rfc5322: &[u8]) -> (String, BodyFormat) {
    let Some(msg) = mail_parser::MessageParser::default().parse(rfc5322) else {
        return (String::new(), BodyFormat::PlainText);
    };
    // Prefer a *genuine* `text/html` alternative. `mail_parser` promotes a lone
    // `text/plain` part into its `html_body` list too (converting text→html on
    // access), so `body_html(0).is_some()` does NOT imply real HTML — gate on
    // the selected part actually being `PartType::Html`, else treat as plaintext.
    let has_real_html = msg
        .html_part(0)
        .is_some_and(|p| matches!(p.body, mail_parser::PartType::Html(_)));
    if has_real_html && let Some(html) = msg.body_html(0) {
        return (html_to_markdown(&html), BodyFormat::Markdown);
    }
    match msg.body_text(0) {
        Some(text) => (text.trim_end().to_string(), BodyFormat::PlainText),
        None => (String::new(), BodyFormat::PlainText),
    }
}

/// Extract the file attachments of an inbound RFC 5322 message as
/// `(AttachmentSnapshot, plaintext bytes)` pairs — the SMTP rail's inbound
/// producer (`docs/goal/ui/conversations.md` § Attachments). `mail_parser`
/// decodes each part's Content-Transfer-Encoding (base64 / quoted-printable), so
/// `contents()` is the raw file bytes; `blob_hash` is their lowercase-hex BLAKE3
/// (the content handle the snapshot carries and the client loader resolves). The
/// receive path caches the bytes under that hash so the rendered bubble can load
/// the real file. The body alternatives (text/plain, text/html) are *not*
/// returned — those are handled by [`inbound_mail_body`]; only `mail_parser`'s
/// attachment parts (Content-Disposition: attachment, or non-body parts with a
/// filename) appear here.
///
/// `c2pa` is always `false`: detecting a C2PA manifest needs the heavy
/// `fauna-media` pipeline, which is off on the FFI/WASM build this runs in, so
/// the SMTP rail can't surface the badge client-side yet (the same constraint
/// that gives feed images no web/mobile C2PA today).
pub fn extract_attachments(rfc5322: &[u8]) -> Vec<(AttachmentSnapshot, Vec<u8>)> {
    let Some(msg) = mail_parser::MessageParser::default().parse(rfc5322) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for part in msg.attachments() {
        let bytes = part.contents().to_vec();
        if bytes.is_empty() {
            continue;
        }
        let mime_type = part
            .content_type()
            .map(|ct| match ct.subtype() {
                Some(sub) => format!("{}/{}", ct.ctype(), sub),
                None => ct.ctype().to_string(),
            })
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let filename = part
            .attachment_name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "attachment".to_string());
        let blob_hash = blake3::hash(&bytes).to_hex().to_string();
        let is_image = mime_type.to_ascii_lowercase().starts_with("image/");
        out.push((
            AttachmentSnapshot {
                blob_hash,
                filename,
                mime_type,
                size_bytes: bytes.len() as u64,
                is_image,
                c2pa: false,
            },
            bytes,
        ));
    }
    out
}

// ── scheme-gated link / image handlers ──────────────────────────────

/// First attribute value matching `key` (case-sensitive local name, as html5ever
/// lower-cases tag/attr names during parsing).
fn attr(el: &Element, key: &str) -> Option<String> {
    el.attrs.iter().find_map(|a| {
        let name = &a.name.local;
        (name == key).then(|| a.value.to_string())
    })
}

/// `http(s)` and `mailto:` links survive as `[text](href)`; everything else
/// (`javascript:`, `data:`, relative, scheme-less) is neutralized to its link
/// text — the destination is dropped, the words stay.
fn anchor_handler(el: Element) -> Option<String> {
    let text = el.content.trim();
    match attr(&el, "href") {
        Some(href) if is_safe_link_scheme(&href) => Some(if text.is_empty() {
            href
        } else {
            format!("[{text}]({href})")
        }),
        _ => Some(el.content.to_string()),
    }
}

/// Only `http(s)` image refs survive (emitted as an **un-fetched** markdown
/// image ref); active / inline schemes (`javascript:`, `data:`) are dropped
/// entirely.
fn img_handler(el: Element) -> Option<String> {
    match attr(&el, "src") {
        Some(src) if is_safe_img_scheme(&src) => {
            let alt = attr(&el, "alt").unwrap_or_default().replace(['[', ']'], "");
            Some(format!("![{alt}]({src})"))
        }
        _ => None,
    }
}

fn scheme_is(url: &str, schemes: &[&str]) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    schemes.iter().any(|s| lower.starts_with(s))
}

fn is_safe_link_scheme(url: &str) -> bool {
    scheme_is(url, &["http://", "https://", "mailto:"])
}

fn is_safe_img_scheme(url: &str) -> bool {
    scheme_is(url, &["http://", "https://"])
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── security perimeter: active content dropped ──────────────────

    #[test]
    fn script_and_style_text_never_leak() {
        let html = r#"<html><head><style>.x{color:red}</style></head>
            <body><script>alert('xss');var leak=1</script>
            <p>Hello <b>world</b></p>
            <style>p{display:none}</style></body></html>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("Hello"), "body text kept: {md:?}");
        assert!(md.contains("**world**"), "bold preserved: {md:?}");
        assert!(!md.contains("alert"), "script text dropped: {md:?}");
        assert!(!md.contains("color:red"), "style text dropped: {md:?}");
        assert!(!md.contains("display:none"), "inline style dropped: {md:?}");
    }

    #[test]
    fn embedding_tags_dropped() {
        let html = r#"<p>before</p>
            <iframe src="https://evil.test"></iframe>
            <object data="x.swf"></object>
            <p>after</p>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("before") && md.contains("after"));
        assert!(!md.contains("evil.test"), "iframe src dropped: {md:?}");
        assert!(!md.contains("x.swf"), "object data dropped: {md:?}");
    }

    // ── security perimeter: scheme gating ───────────────────────────

    #[test]
    fn javascript_link_neutralized_to_text() {
        let md = html_to_markdown(r#"<a href="javascript:alert(1)">click me</a>"#);
        assert_eq!(md, "click me", "javascript: link collapses to text: {md:?}");
    }

    #[test]
    fn http_and_mailto_links_survive() {
        assert_eq!(
            html_to_markdown(r#"<a href="https://example.com/a">docs</a>"#),
            "[docs](https://example.com/a)"
        );
        assert_eq!(
            html_to_markdown(r#"<a href="mailto:a@b.test">mail</a>"#),
            "[mail](mailto:a@b.test)"
        );
    }

    #[test]
    fn remote_image_survives_as_unfetched_ref() {
        let md = html_to_markdown(r#"<img src="https://tracker.test/p.gif" alt="pixel">"#);
        assert_eq!(md, "![pixel](https://tracker.test/p.gif)");
    }

    #[test]
    fn data_and_javascript_image_dropped() {
        assert_eq!(
            html_to_markdown(r#"<p>x</p><img src="data:image/png;base64,AAAA" alt="inline">"#),
            "x"
        );
        assert_eq!(
            html_to_markdown(r#"<p>y</p><img src="javascript:alert(1)">"#),
            "y"
        );
    }

    // ── formatting carried into markdown ────────────────────────────

    #[test]
    fn headings_lists_emphasis_convert() {
        let html =
            "<h1>Title</h1><p>a <em>b</em> <strong>c</strong></p><ul><li>one</li><li>two</li></ul>";
        let md = html_to_markdown(html);
        assert!(md.contains("# Title"), "heading: {md:?}");
        // htmd emits the UNDERSCORE form for <em>/<i> (and `**` for <strong>/<b>) —
        // pinned here because it is load-bearing: the shared markdown renderer
        // (`fauna_core::markdown`) MUST understand `_…_` or every received HTML
        // email's italics render as literal underscores (see the round-trip test).
        assert!(
            md.contains("_b_"),
            "italic is underscore-delimited (htmd): {md:?}"
        );
        assert!(md.contains("**c**"), "bold: {md:?}");
        assert!(
            md.contains("one") && md.contains("two"),
            "list items: {md:?}"
        );
    }

    #[test]
    fn inbound_html_italic_round_trips_to_em() {
        // The full inbound italic path: a received <em> → htmd `_b_` → the shared
        // renderer → <em>. Regression guard for the bug where htmd's underscore
        // emphasis rendered as literal `_b_` on every app (the parser only knew
        // `*…*`). Bold (`**`) already worked; both must.
        let md = html_to_markdown("<p>a <em>b</em> and <strong>c</strong></p>");
        let html = fauna_core::markdown::markdown_to_html(&md);
        assert!(
            html.contains("<em>b</em>"),
            "italic must render: md={md:?} html={html:?}"
        );
        assert!(
            html.contains("<strong>c</strong>"),
            "bold must render: md={md:?} html={html:?}"
        );
    }

    #[test]
    fn empty_html_is_empty_plain_text_passes_through() {
        assert_eq!(html_to_markdown(""), "");
        // Tag-less text is inert content — html5ever treats it as a text node.
        assert_eq!(html_to_markdown("just words"), "just words");
    }

    // ── inbound MIME part selection ─────────────────────────────────

    #[test]
    fn multipart_alternative_prefers_html_to_markdown() {
        let raw = b"From: a@x.test\r\nTo: b@y.test\r\nSubject: Hi\r\n\
            MIME-Version: 1.0\r\n\
            Content-Type: multipart/alternative; boundary=\"BB\"\r\n\r\n\
            --BB\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nplain version\r\n\
            --BB\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
            <p>html <strong>version</strong></p>\r\n--BB--\r\n";
        let (body, fmt) = inbound_mail_body(raw);
        assert_eq!(fmt, BodyFormat::Markdown);
        assert!(body.contains("**version**"), "html→md: {body:?}");
        assert!(!body.contains("<p>"), "no raw tags: {body:?}");
    }

    #[test]
    fn plain_text_only_stays_plaintext() {
        let raw = b"From: a@x.test\r\nTo: b@y.test\r\nSubject: Hi\r\n\
            Content-Type: text/plain; charset=utf-8\r\n\r\njust plain text\r\n";
        let (body, fmt) = inbound_mail_body(raw);
        assert_eq!(fmt, BodyFormat::PlainText);
        assert_eq!(body, "just plain text");
    }

    #[test]
    fn html_only_message_converts_to_markdown() {
        let raw = b"From: a@x.test\r\nSubject: Hi\r\n\
            Content-Type: text/html; charset=utf-8\r\n\r\n\
            <h2>News</h2><p>read <a href=\"https://e.test\">here</a></p>\r\n";
        let (body, fmt) = inbound_mail_body(raw);
        assert_eq!(fmt, BodyFormat::Markdown);
        assert!(body.contains("## News"), "heading: {body:?}");
        assert!(body.contains("[here](https://e.test)"), "link: {body:?}");
    }

    #[test]
    fn quoted_printable_html_is_decoded() {
        // mail_parser decodes the CTE before we see the HTML.
        let raw = b"From: a@x.test\r\nSubject: Hi\r\n\
            Content-Type: text/html; charset=utf-8\r\n\
            Content-Transfer-Encoding: quoted-printable\r\n\r\n\
            <p>caf=C3=A9 <b>bold</b></p>\r\n";
        let (body, fmt) = inbound_mail_body(raw);
        assert_eq!(fmt, BodyFormat::Markdown);
        assert!(body.contains("café"), "QP+charset decoded: {body:?}");
        assert!(body.contains("**bold**"));
    }

    #[test]
    fn unparseable_message_is_empty_plaintext() {
        let (body, fmt) = inbound_mail_body(b"");
        assert_eq!(fmt, BodyFormat::PlainText);
        assert_eq!(body, "");
    }

    // ── inbound attachment extraction ───────────────────────────────

    #[test]
    fn extracts_base64_attachment_excluding_body() {
        // multipart/mixed: a text body part + a base64 image attachment. Only the
        // attachment part is returned; mail_parser decodes the base64 transfer
        // encoding so `contents()` is the raw file bytes.
        let raw = b"From: a@x.test\r\nTo: b@y.test\r\nSubject: files\r\n\
            MIME-Version: 1.0\r\n\
            Content-Type: multipart/mixed; boundary=\"MIX\"\r\n\r\n\
            --MIX\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nbody text\r\n\
            --MIX\r\nContent-Type: image/png\r\n\
            Content-Transfer-Encoding: base64\r\n\
            Content-Disposition: attachment; filename=\"p.png\"\r\n\r\n\
            aGVsbG8=\r\n\
            --MIX--\r\n";
        let atts = extract_attachments(raw);
        assert_eq!(atts.len(), 1, "only the attachment part, not the body");
        let (snap, bytes) = &atts[0];
        assert_eq!(snap.filename, "p.png");
        assert_eq!(snap.mime_type, "image/png");
        assert!(snap.is_image);
        assert!(!snap.c2pa);
        assert_eq!(bytes, b"hello", "base64 `aGVsbG8=` decodes to `hello`");
        assert_eq!(snap.size_bytes, 5);
    }

    #[test]
    fn non_image_attachment_is_not_flagged_image() {
        let raw = b"From: a@x.test\r\nSubject: doc\r\n\
            MIME-Version: 1.0\r\n\
            Content-Type: multipart/mixed; boundary=\"MIX\"\r\n\r\n\
            --MIX\r\nContent-Type: text/plain\r\n\r\nhi\r\n\
            --MIX\r\nContent-Type: application/pdf\r\n\
            Content-Disposition: attachment; filename=\"r.pdf\"\r\n\r\n\
            %PDF-1.4 bytes\r\n\
            --MIX--\r\n";
        let atts = extract_attachments(raw);
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].0.mime_type, "application/pdf");
        assert!(!atts[0].0.is_image);
    }

    #[test]
    fn body_only_message_has_no_attachments() {
        assert!(
            extract_attachments(b"From: a@x.test\r\nSubject: hi\r\n\r\njust text\r\n").is_empty()
        );
        assert!(extract_attachments(b"").is_empty());
    }
}
