//! RFC 5322 / MIME parsing wrapper around `mail-parser`.

use mail_parser::{MessageParser, MimeHeaders, PartType};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
pub enum ParseError {
    #[error("message could not be parsed (malformed RFC 5322 / MIME)")]
    Malformed,
    /// The message parsed fine, but the requested operation is not yet
    /// supported. Used by [`crate::bodysection::fetch_body_section`] for
    /// RFC 9051 §6.4.5 section forms outside the implemented slice
    /// (numbered-part addressing `BODY[N…]`, top-level `MIME`) — distinct
    /// from `Malformed` so the caller can tell "your message is broken"
    /// apart from "this section form isn't wired yet".
    #[error("requested section form is not supported")]
    Unsupported,
    /// A `BINARY[…]` FETCH (RFC 3516 / RFC 9051 §6.4.5) addressed a part
    /// whose `Content-Transfer-Encoding` is one this server cannot decode
    /// (anything other than `7bit` / `8bit` / `binary` / `quoted-printable`
    /// / `base64`). Used by [`crate::bodysection::fetch_binary_section`];
    /// the MDA maps it to a tagged `NO [UNKNOWN-CTE]` (RFC 9051 §6.4.5 — the
    /// server "MUST fail the request" rather than return undecoded bytes).
    #[error("content-transfer-encoding cannot be decoded for BINARY fetch")]
    UnknownCte,
}

/// Result of parsing an RFC 5322 / MIME message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ParsedMessage {
    pub from: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: Option<String>,
    pub message_id: Option<String>,
    pub date_unix_seconds: Option<i64>,
    pub body_text: String,
    pub body_html: Option<String>,
    pub headers: Vec<ParsedHeader>,
    pub mime_parts: Vec<ParsedMimePart>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ParsedHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ParsedMimePart {
    pub content_type: String,
    pub disposition: Option<String>,
    pub filename: Option<String>,
    pub size_bytes: u64,
}

/// The first `From:` header address on an already-parsed message, or `None`
/// when the header is absent or carries no address. Split out of
/// [`parse_rfc5322`] so [`extract_from_address`] can share it without a
/// second parse.
fn from_address_of(msg: &mail_parser::Message<'_>) -> Option<String> {
    msg.from()
        .and_then(|addrs| addrs.first())
        .and_then(|a| a.address())
        .map(|s| s.to_string())
}

/// Parse just enough of a raw RFC 5322 message to read its `From:` address —
/// for a caller that needs to tag/rate-limit a message before (or instead of)
/// running it through the full [`parse_rfc5322`]. `None` covers both "the
/// message doesn't parse at all" and "it parses but has no From address";
/// callers that need to tell those apart should use [`parse_rfc5322`] instead.
pub fn extract_from_address(raw: &[u8]) -> Option<String> {
    from_address_of(&MessageParser::default().parse(raw)?)
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn parse_rfc5322(raw: &[u8]) -> Result<ParsedMessage, ParseError> {
    let msg = MessageParser::default()
        .parse(raw)
        .ok_or(ParseError::Malformed)?;

    let from = from_address_of(&msg);

    let collect_addrs = |list: Option<&mail_parser::Address<'_>>| -> Vec<String> {
        list.map(|addrs| {
            addrs
                .iter()
                .filter_map(|a| a.address().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
    };

    let to = collect_addrs(msg.to());
    let cc = collect_addrs(msg.cc());
    let bcc = collect_addrs(msg.bcc());
    let subject = msg.subject().map(|s| s.to_string());
    let message_id = msg.message_id().map(|s| s.to_string());
    let date_unix_seconds = msg.date().map(|d| d.to_timestamp());

    let body_text = msg.body_text(0).unwrap_or_default().to_string();
    let body_html = msg.body_html(0).map(|s| s.to_string());

    // Slice the raw on-the-wire bytes for each header value.
    // `offset_start` points to the first byte after the ':' separator;
    // `offset_end` points one past the trailing CRLF.  Using raw bytes
    // avoids the silent data loss from `HeaderValue::as_text()`, which
    // returns `None` for structured variants (DateTime, Address,
    // ContentType, Received, …).
    let raw = msg.raw_message.as_ref();
    let headers: Vec<ParsedHeader> = msg
        .headers()
        .iter()
        .map(|h| {
            let start = h.offset_start() as usize;
            let end = h.offset_end() as usize;
            let raw_value = raw
                .get(start..end)
                .map(|b| String::from_utf8_lossy(b).trim().to_string())
                .unwrap_or_default();
            ParsedHeader {
                name: h.name().to_string(),
                value: raw_value,
            }
        })
        .collect();

    let mime_parts: Vec<ParsedMimePart> = msg
        .parts
        .iter()
        .map(|p| ParsedMimePart {
            content_type: p
                .content_type()
                .map(|ct| {
                    let mut s = ct.ctype().to_string();
                    if let Some(sub) = ct.subtype() {
                        s.push('/');
                        s.push_str(sub);
                    }
                    s
                })
                .unwrap_or_else(|| "application/octet-stream".to_string()),
            disposition: p.content_disposition().map(|d| d.ctype().to_string()),
            filename: p.attachment_name().map(|s| s.to_string()),
            size_bytes: p.len() as u64,
        })
        .collect();

    Ok(ParsedMessage {
        from,
        to,
        cc,
        bcc,
        subject,
        message_id,
        date_unix_seconds,
        body_text,
        body_html,
        headers,
        mime_parts,
    })
}

/// A `text/calendar` MIME part extracted from an inbound message — an iTIP/iMIP
/// payload (RFC 6047): the decoded iCalendar text plus the scheduling `method`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextCalendarPart {
    /// The iTIP scheduling method, upper-cased (`REQUEST` / `REPLY` / `CANCEL` /
    /// `REFRESH` / …), preferring the `text/calendar; method=…` Content-Type
    /// parameter (RFC 6047 §2.4) and falling back to the body `METHOD:` property
    /// (RFC 5545). `None` when the part carried neither.
    pub method: Option<String>,
    /// The decoded iCalendar (`.ics`) text of the part.
    pub ics: String,
}

/// Find the first `text/calendar` MIME part in a raw RFC 5322 message and return
/// its decoded iCalendar text + scheduling method (RFC 6047 iMIP). Returns `None`
/// when the message has no `text/calendar` part — the common case for ordinary
/// mail, so a caller can cheaply gate on `Some(..)`.
///
/// Used by the inbound-mail receive path to detect an iTIP `REPLY`/`REQUEST`/
/// `CANCEL` and route it to the calendar/scheduling layer (caldav-server.md
/// § Scheduling & invitations — inbound invites/replies arrive at the mail MTA,
/// are sealed-to-recipient, and the organizer's Fauna app merges a `REPLY`).
/// Handles a single-part `text/calendar` message and a `multipart/*` carrying a
/// `text/calendar` alternative/mixed part alike; decodes the part's
/// Content-Transfer-Encoding via `mail-parser`.
pub fn extract_text_calendar_part(raw: &[u8]) -> Option<TextCalendarPart> {
    let msg = MessageParser::default().parse(raw)?;
    for part in &msg.parts {
        let Some(ct) = part.content_type() else {
            continue;
        };
        let is_calendar = ct.ctype().eq_ignore_ascii_case("text")
            && ct
                .subtype()
                .is_some_and(|s| s.eq_ignore_ascii_case("calendar"));
        if !is_calendar {
            continue;
        }
        // The `text/calendar` body — decoded by `mail-parser` (the CTE is already
        // applied). A text/* part lands in `PartType::Text`; tolerate a part an
        // encoder mislabelled binary by lossily decoding its bytes.
        let ics = match &part.body {
            PartType::Text(t) => t.to_string(),
            PartType::Binary(b) | PartType::InlineBinary(b) => {
                String::from_utf8_lossy(b).into_owned()
            }
            _ => continue,
        };
        // Prefer the `method=` Content-Type parameter (RFC 6047); fall back to the
        // body `METHOD:` line (RFC 5545).
        let method = ct
            .attributes()
            .and_then(|attrs| {
                attrs
                    .iter()
                    .find(|a| a.name.eq_ignore_ascii_case("method"))
                    .map(|a| a.value.trim().to_ascii_uppercase())
            })
            .filter(|m| !m.is_empty())
            .or_else(|| method_from_ics(&ics));
        return Some(TextCalendarPart { method, ics });
    }
    None
}

/// Read the `METHOD:` property value (upper-cased) from an iCalendar body, or
/// `None` when absent. RFC 5545 §3.7.2 — present on iTIP messages (RFC 5546).
fn method_from_ics(ics: &str) -> Option<String> {
    for line in ics.lines() {
        let line = line.trim();
        if let Some(rest) = line
            .strip_prefix("METHOD:")
            .or_else(|| line.strip_prefix("method:"))
        {
            let m = rest.trim().to_ascii_uppercase();
            if !m.is_empty() {
                return Some(m);
            }
        }
    }
    None
}

#[cfg(test)]
mod from_address_tests {
    use super::*;

    /// `extract_from_address` and `parse_rfc5322`'s own `from` field must agree
    /// — they share `from_address_of` precisely so a nest-side pre-filter and
    /// the full parse can never read a different sender off the same bytes.
    #[test]
    fn extract_from_address_agrees_with_parse_rfc5322() {
        let raw =
            b"From: Bob <bob@fauna.test>\r\nTo: alice@fauna.test\r\nSubject: hi\r\n\r\nhello\r\n";
        assert_eq!(extract_from_address(raw).as_deref(), Some("bob@fauna.test"));
        assert_eq!(
            parse_rfc5322(raw).unwrap().from.as_deref(),
            Some("bob@fauna.test")
        );
    }

    #[test]
    fn extract_from_address_is_none_without_a_from_header() {
        let raw = b"To: alice@fauna.test\r\nSubject: hi\r\n\r\nhello\r\n";
        assert_eq!(extract_from_address(raw), None);
    }
}

#[cfg(test)]
mod calendar_part_tests {
    use super::*;

    const REPLY_ICS: &str = "BEGIN:VCALENDAR\r\n\
METHOD:REPLY\r\n\
BEGIN:VEVENT\r\n\
UID:kickoff-1@fauna.test\r\n\
ATTENDEE;PARTSTAT=ACCEPTED:mailto:bob@fauna.test\r\n\
ORGANIZER:mailto:alice@fauna.test\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    fn multipart_with_calendar(method_param: &str, body: &str) -> Vec<u8> {
        format!(
            "From: bob@fauna.test\r\n\
To: alice@fauna.test\r\n\
Subject: Re: Kickoff\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"b0undary\"\r\n\
\r\n\
--b0undary\r\n\
Content-Type: text/plain; charset=UTF-8\r\n\
\r\n\
Bob has accepted.\r\n\
--b0undary\r\n\
Content-Type: text/calendar; charset=UTF-8{method_param}\r\n\
\r\n\
{body}\r\n\
--b0undary--\r\n"
        )
        .into_bytes()
    }

    #[test]
    fn extracts_reply_with_method_from_content_type_param() {
        let raw = multipart_with_calendar("; method=REPLY", REPLY_ICS);
        let part = extract_text_calendar_part(&raw).expect("a text/calendar part");
        assert_eq!(part.method.as_deref(), Some("REPLY"));
        assert!(part.ics.contains("UID:kickoff-1@fauna.test"));
        assert!(part.ics.contains("PARTSTAT=ACCEPTED"));
    }

    #[test]
    fn falls_back_to_body_method_line_without_content_type_param() {
        // No `method=` on the Content-Type, but `METHOD:REPLY` in the body.
        let raw = multipart_with_calendar("", REPLY_ICS);
        let part = extract_text_calendar_part(&raw).expect("a text/calendar part");
        assert_eq!(part.method.as_deref(), Some("REPLY"));
    }

    #[test]
    fn reads_request_method() {
        let request_ics = REPLY_ICS.replace("METHOD:REPLY", "METHOD:REQUEST");
        let raw = multipart_with_calendar("; method=REQUEST", &request_ics);
        let part = extract_text_calendar_part(&raw).expect("a text/calendar part");
        assert_eq!(part.method.as_deref(), Some("REQUEST"));
    }

    #[test]
    fn single_part_text_calendar_message() {
        // A message whose entire body is text/calendar (no multipart wrapper).
        let raw = format!(
            "From: bob@fauna.test\r\n\
To: alice@fauna.test\r\n\
Subject: Re: Kickoff\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/calendar; charset=UTF-8; method=REPLY\r\n\
\r\n\
{REPLY_ICS}"
        )
        .into_bytes();
        let part = extract_text_calendar_part(&raw).expect("a text/calendar part");
        assert_eq!(part.method.as_deref(), Some("REPLY"));
        assert!(part.ics.contains("UID:kickoff-1@fauna.test"));
    }

    // `base64` is provided by the `outbound` feature (not `parser`); gate this
    // case so a `--features parser`-only build still compiles. mail-parser does
    // the actual CTE decode — this asserts the decoded (not raw base64) body
    // reaches the caller.
    #[cfg(feature = "outbound")]
    #[test]
    fn base64_encoded_calendar_part_is_decoded() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(REPLY_ICS);
        let raw = format!(
            "From: bob@fauna.test\r\n\
To: alice@fauna.test\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"b0undary\"\r\n\
\r\n\
--b0undary\r\n\
Content-Type: text/calendar; charset=UTF-8; method=REPLY\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
{b64}\r\n\
--b0undary--\r\n"
        )
        .into_bytes();
        let part = extract_text_calendar_part(&raw).expect("a text/calendar part");
        assert_eq!(part.method.as_deref(), Some("REPLY"));
        assert!(part.ics.contains("UID:kickoff-1@fauna.test"));
    }

    #[test]
    fn ordinary_mail_without_calendar_part_returns_none() {
        let raw = b"From: bob@fauna.test\r\n\
To: alice@fauna.test\r\n\
Subject: Lunch?\r\n\
Content-Type: text/plain; charset=UTF-8\r\n\
\r\n\
Want to grab lunch?\r\n";
        assert!(extract_text_calendar_part(raw).is_none());
    }
}
