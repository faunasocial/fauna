//! IMAP BODYSTRUCTURE derivation from raw RFC 5322 / MIME bytes.
//!
//! Walks `mail-parser`'s parsed tree and emits a flat, UniFFI-friendly
//! [`BodyStructure`] tree the Go bridge can map onto emersion's
//! `imap.BodyStructure` shape (RFC 9051 §7.5.2). Multipart parts have a
//! non-empty [`BodyStructure::parts`] vec; leaf parts have `parts.is_empty()`.
//!
//! `type_` and `subtype` are upper-cased (RFC 9051 conventional form);
//! parameter names are upper-cased; parameter values are preserved as-is.
//! `encoding` is normalised to one of `"7BIT"`, `"8BIT"`, `"BASE64"`,
//! `"QUOTED-PRINTABLE"` (defaults to `"7BIT"` when absent).
//!
//! Phase C.4 of the I5 mail-bridge MDA arm (tracked internally).

use crate::parser::ParseError;
use mail_parser::{
    Encoding, Header, HeaderName, HeaderValue, Message, MessageParser, MessagePart, MimeHeaders,
    PartType,
};
use serde::{Deserialize, Serialize};

/// One MIME part of a derived BODYSTRUCTURE tree.
///
/// Multipart parts have `type_ == "MULTIPART"`, `parts` non-empty, and
/// `encoding`/`size_octets`/`lines` unused. Leaf parts have `parts` empty
/// and the leaf-only fields populated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BodyStructure {
    /// Top-level MIME type, upper-cased (`"TEXT"`, `"MULTIPART"`,
    /// `"APPLICATION"`, `"MESSAGE"`, …).
    pub type_: String,
    /// MIME subtype, upper-cased (`"PLAIN"`, `"HTML"`, `"ALTERNATIVE"`,
    /// `"OCTET-STREAM"`, …). Empty string when absent.
    pub subtype: String,
    /// Content-Type parameters (e.g. `CHARSET=utf-8`, `BOUNDARY=...`,
    /// `NAME=...`). Names upper-cased, values verbatim.
    pub parameters: Vec<MimeParam>,
    /// Content-ID header value (with surrounding angle brackets if
    /// present in the source).
    pub id: Option<String>,
    /// Content-Description header value, RFC 2047 decoded by mail-parser.
    pub description: Option<String>,
    /// Content-Transfer-Encoding, upper-cased. Unused for multipart.
    pub encoding: Option<String>,
    /// Octet count of the part body. For text parts: the decoded body
    /// length. For binary parts: the decoded body length. For multipart:
    /// `0` (multipart bodies don't carry an aggregate octet count in the
    /// derived shape — the IMAP layer sums children if asked).
    pub size_octets: u32,
    /// Line count for `text/…` parts (LF count in the decoded body).
    /// `None` for non-text leaves and for multipart.
    pub lines: Option<u32>,
    /// Content-Disposition main value, upper-cased (`"ATTACHMENT"`,
    /// `"INLINE"`). `None` when no Content-Disposition header.
    pub disposition: Option<String>,
    /// Content-Disposition parameters (`FILENAME=...`, `SIZE=...`).
    /// Names upper-cased, values verbatim.
    pub disposition_parameters: Vec<MimeParam>,
    /// Children for multipart parts (in source order). Empty for leaves.
    pub parts: Vec<BodyStructure>,
}

/// One Content-Type or Content-Disposition parameter.
///
/// Keys are upper-cased per the RFC 9051 BODYSTRUCTURE convention; values
/// are preserved as the parser exposes them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MimeParam {
    pub name: String,
    pub value: String,
}

/// Derive an IMAP BODYSTRUCTURE tree from raw RFC 5322 / MIME bytes.
///
/// The bridge layer (Go) consumes this to emit `BODYSTRUCTURE` /
/// `BODY` FETCH responses without re-parsing on the Go side.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn derive_body_structure(raw: &[u8]) -> Result<BodyStructure, ParseError> {
    let msg = MessageParser::default()
        .parse(raw)
        .ok_or(ParseError::Malformed)?;
    Ok(build_part(&msg, &msg.parts[0]))
}

fn build_part(msg: &Message<'_>, part: &MessagePart<'_>) -> BodyStructure {
    let (mime_type, mime_subtype, parameters) = mime_type_fields(part);
    let id = first_header_text(part, &HeaderName::ContentId);
    let description = first_header_text(part, &HeaderName::ContentDescription);
    let encoding = encoding_string(part);
    let (disposition, disposition_parameters) = disposition_fields(part);

    match &part.body {
        PartType::Multipart(child_ids) => BodyStructure {
            type_: "MULTIPART".to_string(),
            subtype: mime_subtype,
            parameters,
            id,
            description,
            encoding: None,
            size_octets: 0,
            lines: None,
            disposition,
            disposition_parameters,
            parts: child_ids
                .iter()
                .filter_map(|id| msg.parts.get(*id as usize))
                .map(|child| build_part(msg, child))
                .collect(),
        },
        PartType::Text(text) | PartType::Html(text) => {
            let bytes = text.as_bytes();
            BodyStructure {
                type_: mime_type,
                subtype: mime_subtype,
                parameters,
                id,
                description,
                encoding,
                size_octets: bytes.len() as u32,
                lines: Some(count_lines(bytes)),
                disposition,
                disposition_parameters,
                parts: Vec::new(),
            }
        }
        PartType::Binary(bin) | PartType::InlineBinary(bin) => BodyStructure {
            type_: mime_type,
            subtype: mime_subtype,
            parameters,
            id,
            description,
            encoding,
            size_octets: bin.len() as u32,
            lines: None,
            disposition,
            disposition_parameters,
            parts: Vec::new(),
        },
        PartType::Message(_) => {
            // Nested message/rfc822: surface the part's raw byte slice
            // as the size; the IMAP layer can recurse with a separate
            // derive_body_structure call on the inner bytes when the
            // client asks for BODY[N].
            let body_len = part.offset_end.saturating_sub(part.offset_body);
            BodyStructure {
                type_: mime_type,
                subtype: mime_subtype,
                parameters,
                id,
                description,
                encoding,
                size_octets: body_len,
                lines: None,
                disposition,
                disposition_parameters,
                parts: Vec::new(),
            }
        }
    }
}

fn mime_type_fields(part: &MessagePart<'_>) -> (String, String, Vec<MimeParam>) {
    let ct = part.content_type();
    let main = ct
        .map(|c| c.ctype().to_ascii_uppercase())
        .unwrap_or_else(|| {
            // Default per RFC 2045: text/plain.
            "TEXT".to_string()
        });
    let sub = ct
        .and_then(|c| c.subtype().map(|s| s.to_ascii_uppercase()))
        .unwrap_or_else(|| {
            if main == "TEXT" {
                "PLAIN".to_string()
            } else {
                String::new()
            }
        });
    let params = ct
        .and_then(|c| c.attributes())
        .map(|attrs| {
            attrs
                .iter()
                .map(|a| MimeParam {
                    name: a.name.to_ascii_uppercase(),
                    value: a.value.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    (main, sub, params)
}

fn disposition_fields(part: &MessagePart<'_>) -> (Option<String>, Vec<MimeParam>) {
    let cd = match part.content_disposition() {
        Some(cd) => cd,
        None => return (None, Vec::new()),
    };
    let main = cd.ctype().to_ascii_uppercase();
    let params = cd
        .attributes()
        .map(|attrs| {
            attrs
                .iter()
                .map(|a| MimeParam {
                    name: a.name.to_ascii_uppercase(),
                    value: a.value.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    (Some(main), params)
}

fn encoding_string(part: &MessagePart<'_>) -> Option<String> {
    // Prefer the parsed `encoding` enum (mail-parser normalises
    // Content-Transfer-Encoding). Default to "7BIT" when the part has a
    // body but no explicit header.
    let header_value = first_header_text(part, &HeaderName::ContentTransferEncoding);
    Some(match part.encoding {
        Encoding::Base64 => "BASE64".to_string(),
        Encoding::QuotedPrintable => "QUOTED-PRINTABLE".to_string(),
        Encoding::None => header_value
            .map(|s| s.to_ascii_uppercase())
            .unwrap_or_else(|| "7BIT".to_string()),
    })
}

fn first_header_text(part: &MessagePart<'_>, name: &HeaderName<'_>) -> Option<String> {
    part.headers
        .iter()
        .find(|h: &&Header<'_>| &h.name == name)
        .and_then(|h| match &h.value {
            HeaderValue::Text(s) => Some(s.to_string()),
            HeaderValue::TextList(list) => list.first().map(|s| s.to_string()),
            _ => None,
        })
}

fn count_lines(body: &[u8]) -> u32 {
    body.iter().filter(|&&b| b == b'\n').count() as u32
}
