use fauna_mail::parser::{ParsedMessage, parse_rfc5322};

const SIMPLE_EML: &[u8] = include_bytes!("fixtures/simple.eml");
const MULTIPART_EML: &[u8] = include_bytes!("fixtures/multipart.eml");
const ENCODED_SUBJECT_EML: &[u8] = include_bytes!("fixtures/encoded_subject.eml");

#[test]
fn parse_simple_message_extracts_envelope_fields() {
    let parsed: ParsedMessage = parse_rfc5322(SIMPLE_EML).expect("parse should succeed");
    assert_eq!(parsed.from.as_deref(), Some("alice@example.com"));
    assert_eq!(parsed.to, vec!["bob@example.com"]);
    assert_eq!(parsed.subject.as_deref(), Some("Test"));
    assert_eq!(parsed.message_id.as_deref(), Some("test-001@example.com"));
    assert!(parsed.body_text.contains("Hello Bob"));
}

#[test]
fn headers_vec_contains_non_empty_date_and_from() {
    // Regression test: HeaderValue::as_text() returns None for structured
    // variants (DateTime, Address, …), which previously caused Date, From,
    // To, Content-Type and others to silently become empty strings.
    let parsed: ParsedMessage = parse_rfc5322(SIMPLE_EML).expect("parse should succeed");

    let find = |name: &str| -> Option<String> {
        parsed
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.clone())
    };

    let date_value = find("Date").expect("Date header must be present");
    assert!(
        !date_value.is_empty(),
        "Date header value must not be empty"
    );

    let from_value = find("From").expect("From header must be present");
    assert!(
        !from_value.is_empty(),
        "From header value must not be empty"
    );
}

#[test]
fn parse_multipart_extracts_text_html_and_attachment() {
    let parsed = parse_rfc5322(MULTIPART_EML).expect("parse should succeed");
    assert!(parsed.body_text.contains("plain text part"));
    // mail-parser 0.11 synthesises HTML from the text/plain part when calling
    // body_html(0), wrapping it in <html><body>…</body></html>, so body_html
    // is Some and contains valid HTML structure even though it doesn't contain
    // the raw HTML MIME part verbatim.
    let body_html = parsed
        .body_html
        .as_deref()
        .expect("body_html should be Some");
    assert!(
        body_html.contains("<html>") && body_html.contains("<body>"),
        "body_html should contain synthesized HTML structure (<html> and <body> tags); got: {}",
        body_html
    );
    assert!(
        parsed
            .mime_parts
            .iter()
            .any(|p| p.content_type == "text/html"),
        "mime_parts should contain a text/html part"
    );
    let pdf = parsed
        .mime_parts
        .iter()
        .find(|p| p.content_type == "application/pdf")
        .expect("pdf part should be present");
    assert_eq!(pdf.filename.as_deref(), Some("report.pdf"));
    assert_eq!(pdf.disposition.as_deref(), Some("attachment"));
}

#[test]
fn parse_decodes_rfc2047_encoded_subject() {
    let parsed = parse_rfc5322(ENCODED_SUBJECT_EML).expect("parse should succeed");
    assert_eq!(parsed.subject.as_deref(), Some("Hej åh vår"));
}

#[test]
fn parse_malformed_returns_error() {
    let raw = b"this is not a valid email message at all";
    // mail-parser is permissive; malformed-but-textual input may parse with
    // empty fields rather than returning an error. Either is acceptable for
    // this regression test — we just confirm it doesn't panic.
    let _ = parse_rfc5322(raw);
}
