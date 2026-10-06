//! IMAP BODYSTRUCTURE derivation, shared-Rust side.
//!
//! Drives `fauna_mail::bodystructure::derive_body_structure` against
//! single-part text/plain, multipart/alternative, and multipart/mixed
//! with attachment fixtures. Phase C.4 of the I5 mail-bridge MDA arm.

use fauna_mail::bodystructure::derive_body_structure;

const TEXT_PLAIN: &[u8] = b"From: a@example.com\r\n\
To: b@example.com\r\n\
Subject: Hi\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hello\r\n\
world\r\n\
final\r\n";

const MULTIPART_ALTERNATIVE: &[u8] = b"From: a@example.com\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=\"BNDR\"\r\n\
\r\n\
--BNDR\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
plain body\r\n\
--BNDR\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>html body</p>\r\n\
--BNDR--\r\n";

const MULTIPART_MIXED_WITH_ATTACHMENT: &[u8] = b"From: a@example.com\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"BNDR\"\r\n\
\r\n\
--BNDR\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
the body\r\n\
--BNDR\r\n\
Content-Type: application/octet-stream; name=\"data.bin\"\r\n\
Content-Disposition: attachment; filename=\"data.bin\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
QUFB\r\n\
--BNDR--\r\n";

#[test]
fn derives_text_plain_top_level() {
    let bs = derive_body_structure(TEXT_PLAIN).expect("parse should succeed");
    assert_eq!(bs.type_, "TEXT");
    assert_eq!(bs.subtype, "PLAIN");
    assert!(
        bs.parameters
            .iter()
            .any(|p| p.name == "CHARSET" && p.value.eq_ignore_ascii_case("utf-8")),
        "expected CHARSET=utf-8 parameter, got {:?}",
        bs.parameters
    );
    assert!(bs.parts.is_empty(), "leaf parts must have empty `parts`");
}

#[test]
fn derives_multipart_alternative() {
    let bs = derive_body_structure(MULTIPART_ALTERNATIVE).expect("parse should succeed");
    assert_eq!(bs.type_, "MULTIPART");
    assert_eq!(bs.subtype, "ALTERNATIVE");
    assert_eq!(bs.parts.len(), 2, "expected exactly 2 children");
    assert_eq!(bs.parts[0].type_, "TEXT");
    assert_eq!(bs.parts[0].subtype, "PLAIN");
    assert_eq!(bs.parts[1].type_, "TEXT");
    assert_eq!(bs.parts[1].subtype, "HTML");
}

#[test]
fn derives_multipart_mixed_with_attachment() {
    let bs = derive_body_structure(MULTIPART_MIXED_WITH_ATTACHMENT).expect("parse should succeed");
    assert_eq!(bs.type_, "MULTIPART");
    assert_eq!(bs.subtype, "MIXED");
    assert_eq!(bs.parts.len(), 2);

    let attach = &bs.parts[1];
    assert_eq!(attach.type_, "APPLICATION");
    assert_eq!(attach.subtype, "OCTET-STREAM");
    assert_eq!(
        attach.disposition.as_deref(),
        Some("ATTACHMENT"),
        "disposition should be uppercase ATTACHMENT"
    );
    assert!(
        attach
            .disposition_parameters
            .iter()
            .any(|p| p.name == "FILENAME" && p.value == "data.bin"),
        "expected disposition FILENAME=data.bin, got {:?}",
        attach.disposition_parameters
    );
}

#[test]
fn derives_lines_for_text_parts() {
    // Body is "Hello\r\nworld\r\nfinal\r\n" — three LFs.
    let bs = derive_body_structure(TEXT_PLAIN).expect("parse should succeed");
    assert_eq!(
        bs.lines,
        Some(3),
        "expected 3 lines (one per LF in body), got {:?}",
        bs.lines
    );
}

// --- Content-Type / encoding defaults (RFC 2045) ---

const NO_CONTENT_TYPE: &[u8] = b"From: a@example.com\r\n\
Subject: bare\r\n\
\r\n\
just text\r\n";

#[test]
fn defaults_to_text_plain_and_7bit_when_headers_absent() {
    // No Content-Type and no Content-Transfer-Encoding: RFC 2045 defaults
    // are text/plain and 7BIT.
    let bs = derive_body_structure(NO_CONTENT_TYPE).expect("parse should succeed");
    assert_eq!(bs.type_, "TEXT");
    assert_eq!(bs.subtype, "PLAIN");
    assert_eq!(
        bs.encoding.as_deref(),
        Some("7BIT"),
        "absent Content-Transfer-Encoding must default to 7BIT, got {:?}",
        bs.encoding
    );
}

const TEXT_QUOTED_PRINTABLE: &[u8] = b"From: a@example.com\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
Hello=20world\r\n";

#[test]
fn normalises_quoted_printable_encoding() {
    let bs = derive_body_structure(TEXT_QUOTED_PRINTABLE).expect("parse should succeed");
    assert_eq!(
        bs.encoding.as_deref(),
        Some("QUOTED-PRINTABLE"),
        "got {:?}",
        bs.encoding
    );
}

const TEXT_8BIT: &[u8] = b"From: a@example.com\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: 8bit\r\n\
\r\n\
8-bit body\r\n";

#[test]
fn normalises_8bit_encoding_via_header() {
    // mail-parser folds both 7bit and 8bit to Encoding::None, so 8BIT is
    // recovered from the raw header and upper-cased.
    let bs = derive_body_structure(TEXT_8BIT).expect("parse should succeed");
    assert_eq!(
        bs.encoding.as_deref(),
        Some("8BIT"),
        "got {:?}",
        bs.encoding
    );
}

#[test]
fn base64_attachment_encoding_is_normalised() {
    // The existing mixed/attachment fixture carries Content-Transfer-Encoding:
    // base64 but never asserted the normalised encoding string.
    let bs = derive_body_structure(MULTIPART_MIXED_WITH_ATTACHMENT).expect("parse should succeed");
    assert_eq!(
        bs.parts[1].encoding.as_deref(),
        Some("BASE64"),
        "got {:?}",
        bs.parts[1].encoding
    );
}

// --- Content-ID / Content-Description on an inline part ---

const MULTIPART_RELATED_INLINE_IMAGE: &[u8] = b"From: a@example.com\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=\"REL\"\r\n\
\r\n\
--REL\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<img src=\"cid:img1@example.com\">\r\n\
--REL\r\n\
Content-Type: image/png\r\n\
Content-ID: <img1@example.com>\r\n\
Content-Description: a small image\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
QUFB\r\n\
--REL--\r\n";

#[test]
fn surfaces_content_id_and_description_on_inline_part() {
    let bs = derive_body_structure(MULTIPART_RELATED_INLINE_IMAGE).expect("parse should succeed");
    assert_eq!(bs.type_, "MULTIPART");
    assert_eq!(bs.subtype, "RELATED");
    let image = &bs.parts[1];
    assert_eq!(image.type_, "IMAGE");
    assert_eq!(image.subtype, "PNG");
    let id = image.id.as_deref().expect("Content-ID must be surfaced");
    assert!(
        id.contains("img1@example.com"),
        "Content-ID should carry the source value, got {id:?}"
    );
    assert_eq!(
        image.description.as_deref(),
        Some("a small image"),
        "got {:?}",
        image.description
    );
}

// --- nested multipart recursion ---

const NESTED_MULTIPART: &[u8] = b"From: a@example.com\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"OUT\"\r\n\
\r\n\
--OUT\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
intro\r\n\
--OUT\r\n\
Content-Type: multipart/alternative; boundary=\"IN\"\r\n\
\r\n\
--IN\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
alt plain\r\n\
--IN\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>alt html</p>\r\n\
--IN--\r\n\
--OUT--\r\n";

#[test]
fn recurses_into_nested_multipart() {
    let bs = derive_body_structure(NESTED_MULTIPART).expect("parse should succeed");
    assert_eq!(bs.type_, "MULTIPART");
    assert_eq!(bs.subtype, "MIXED");
    assert_eq!(bs.parts.len(), 2, "outer should have 2 children");
    assert_eq!(bs.parts[0].type_, "TEXT");

    let inner = &bs.parts[1];
    assert_eq!(inner.type_, "MULTIPART");
    assert_eq!(inner.subtype, "ALTERNATIVE");
    assert_eq!(
        inner.parts.len(),
        2,
        "nested alternative should have 2 children, got {}",
        inner.parts.len()
    );
    assert_eq!(inner.parts[0].subtype, "PLAIN");
    assert_eq!(inner.parts[1].subtype, "HTML");
}

// --- message/rfc822 leaf ---

const MESSAGE_RFC822: &[u8] = b"From: a@example.com\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"B\"\r\n\
\r\n\
--B\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
outer body\r\n\
--B\r\n\
Content-Type: message/rfc822\r\n\
\r\n\
From: inner@example.com\r\n\
Subject: inner message\r\n\
\r\n\
inner body\r\n\
--B--\r\n";

#[test]
fn derives_message_rfc822_part() {
    let bs = derive_body_structure(MESSAGE_RFC822).expect("parse should succeed");
    assert_eq!(bs.parts.len(), 2);
    let inner = &bs.parts[1];
    assert_eq!(inner.type_, "MESSAGE", "got {:?}", inner.type_);
    assert_eq!(inner.subtype, "RFC822", "got {:?}", inner.subtype);
    assert!(
        inner.parts.is_empty(),
        "message/rfc822 is surfaced as a leaf; the IMAP layer recurses on BODY[N]"
    );
    assert!(
        inner.size_octets > 0,
        "message/rfc822 leaf should carry a non-zero body size, got {}",
        inner.size_octets
    );
}
