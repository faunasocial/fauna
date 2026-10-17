//! DMARC forensic-report **receive**: tell a forensic report from an aggregate
//! one, check its RFC 5965 wrapper, and extract the best-effort index the nest
//! stores (`docs/goal/behavior/dmarc-reporting.md` § Forensic-report receive
//! (RUF)).
//!
//! Forensic and aggregate reports share the `dmarc-report@` address, so the
//! nest classifies each message it is handed: a top-level
//! `multipart/report; report-type=feedback-report` (RFC 5965 §2) is a forensic
//! report, anything else goes to the aggregate parse (`dmarc_report`). The
//! classification reads the top-level header block only.
//!
//! The parse is **strict on the wrapper and lenient on the inner shape**
//! (§ Format leniency): the message must parse as RFC 5322, be a
//! `multipart/report` of report type `feedback-report`, and carry a
//! `message/feedback-report` part whose `Feedback-Type` is `auth-failure`
//! (RFC 6591) with the RFC 5965 §3.1 required `User-Agent` and `Version`
//! fields — anything less is [`ReportError::Malformed`]. The reported message
//! (`message/rfc822`, `message/global` or `text/rfc822-headers`) is read for a
//! few headers on a best-effort basis; when it is missing or carries no
//! `Message-ID` the report is still stored, marked
//! [`DmarcForensicSummary::partial`].
//!
//! **PII rule (§ Storage):** the index this module builds never names a
//! recipient of the reported message (`Original-Rcpt-To`, `To`, `Cc`) and
//! carries no subject or body. Those stay only in the raw archive the nest
//! keeps for the retention window.
//!
//! Pure (no I/O): the nest calls it from `ingest_dmarc_report`.

use fauna_protocol::bridge_routing::DmarcForensicSummary;
use mail_parser::{MessageParser, MimeHeaders, PartType};

pub use crate::dmarc_report::ReportError;

/// Largest report message the forensic parse reads. A forensic report is one
/// message's headers (and at most its body) plus a few fields; the bound keeps
/// the stored archive small.
pub const MAX_FORENSIC_MESSAGE_BYTES: usize = 10 * 1024 * 1024;

/// Most feedback-report fields read (a real report carries a dozen).
const MAX_FEEDBACK_FIELDS: usize = 256;

/// Longest single index value kept; a longer one is cut at a char boundary.
const MAX_FIELD_CHARS: usize = 512;

fn malformed(why: impl Into<String>) -> ReportError {
    ReportError::Malformed(why.into())
}

/// The parsed fields of one forensic report.
#[derive(Debug, Clone, PartialEq)]
pub struct ForensicReport {
    /// The report mail's `From:` (`Name <addr>`, else the bare address); empty
    /// when it has none — the nest falls back to the envelope sender.
    pub submitter: String,
    /// The domain the report is about, lowercased, without a trailing dot:
    /// `Reported-Domain`, else the reported message's `From:` domain; empty
    /// when neither is present.
    pub reported_domain: String,
    /// The reported message's `Message-ID`, without angle brackets.
    pub reported_message_id: String,
    /// `Arrival-Date` as Unix seconds; 0 when absent or unparseable.
    pub reported_arrival_date: i64,
    /// The index stored as `parsed_summary_cbor`.
    pub summary: DmarcForensicSummary,
}

/// The end of the top-level header block (the offset of the empty line), or
/// the whole message when it has no body.
fn header_block(raw: &[u8]) -> &[u8] {
    let crlf = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
    let lf = raw.windows(2).position(|w| w == b"\n\n").map(|i| i + 2);
    let end = match (crlf, lf) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => raw.len(),
    };
    &raw[..end]
}

fn content_type_is(ct: Option<&mail_parser::ContentType<'_>>, main: &str, sub: &str) -> bool {
    ct.is_some_and(|ct| {
        ct.ctype().eq_ignore_ascii_case(main)
            && ct.subtype().is_some_and(|s| s.eq_ignore_ascii_case(sub))
    })
}

fn is_feedback_report_container(ct: Option<&mail_parser::ContentType<'_>>) -> bool {
    content_type_is(ct, "multipart", "report")
        && ct.and_then(|ct| ct.attributes()).is_some_and(|attrs| {
            attrs.iter().any(|a| {
                a.name.eq_ignore_ascii_case("report-type")
                    && a.value.trim().eq_ignore_ascii_case("feedback-report")
            })
        })
}

/// Whether a `dmarc-report@` message is a forensic report: its top-level
/// `Content-Type` is `multipart/report; report-type=feedback-report`. Reads the
/// header block only, so classifying a large aggregate report costs nothing.
pub fn is_forensic_report(raw_message: &[u8]) -> bool {
    MessageParser::default()
        .parse_headers(header_block(raw_message))
        .is_some_and(|m| is_feedback_report_container(m.content_type()))
}

/// Unfold RFC 5965 §3 `Name: value` fields (continuation lines start with
/// whitespace), lowercasing the names. The first occurrence of a name wins.
fn feedback_fields(body: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in body.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, v)) = out.last_mut() {
                v.push(' ');
                v.push_str(line.trim());
            }
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if out.len() >= MAX_FEEDBACK_FIELDS {
            break;
        }
        out.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    out
}

fn field<'a>(fields: &'a [(String, String)], name: &str) -> &'a str {
    fields
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
}

fn clip(s: &str) -> String {
    match s.char_indices().nth(MAX_FIELD_CHARS) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn normalise_domain(d: &str) -> String {
    d.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// An `Arrival-Date` (RFC 5322 date-time) as Unix seconds, 0 when unparseable.
fn arrival_epoch(value: &str) -> i64 {
    if value.is_empty() {
        return 0;
    }
    let header = format!("Date: {value}\r\n\r\n");
    MessageParser::default()
        .parse_headers(header.as_bytes())
        .and_then(|m| m.date().map(|d| d.to_timestamp()))
        .unwrap_or(0)
}

/// The first address of a parsed `From:`.
fn first_from(msg: &mail_parser::Message<'_>) -> Option<(Option<String>, String)> {
    let addr = msg.from()?.first()?;
    let address = addr.address()?.trim().to_string();
    let name = addr
        .name
        .as_deref()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    Some((name, address))
}

/// The headers of the reported message, best effort: `(header_from,
/// message_id)`, each empty when not found.
fn reported_headers(part: &mail_parser::MessagePart<'_>) -> Option<(String, String)> {
    let parsed;
    let inner: &mail_parser::Message<'_> = match &part.body {
        PartType::Message(m) => m,
        PartType::Text(t) => {
            parsed = MessageParser::default().parse_headers(header_block(t.as_bytes()))?;
            &parsed
        }
        PartType::Binary(b) | PartType::InlineBinary(b) => {
            parsed = MessageParser::default().parse_headers(header_block(b))?;
            &parsed
        }
        _ => return None,
    };
    let from = first_from(inner)
        .map(|(_, a)| a.to_ascii_lowercase())
        .unwrap_or_default();
    let message_id = inner.message_id().unwrap_or("").trim().to_string();
    Some((from, message_id))
}

fn is_reported_message_part(ct: Option<&mail_parser::ContentType<'_>>) -> bool {
    content_type_is(ct, "message", "rfc822")
        || content_type_is(ct, "message", "global")
        || content_type_is(ct, "text", "rfc822-headers")
        || content_type_is(ct, "message", "rfc822-headers")
        || content_type_is(ct, "message", "global-headers")
}

/// Gzip a report mail for the `payload_raw_gz` archive column.
pub fn gzip_message(raw_message: &[u8]) -> Vec<u8> {
    crate::dmarc_report::gzip_xml(raw_message)
}

/// Parse one forensic report message (see the module docs for what is strict
/// and what is best effort).
pub fn parse_forensic_report(raw_message: &[u8]) -> Result<ForensicReport, ReportError> {
    if raw_message.len() > MAX_FORENSIC_MESSAGE_BYTES {
        return Err(ReportError::Oversize);
    }
    let msg = MessageParser::default()
        .parse(raw_message)
        .ok_or_else(|| malformed("message does not parse as RFC 5322"))?;
    if !is_feedback_report_container(msg.content_type()) {
        return Err(malformed(
            "not a multipart/report of report-type feedback-report",
        ));
    }

    let mut fields = None;
    let mut reported = None;
    for part in msg.parts.iter().skip(1) {
        let ct = part.content_type();
        if fields.is_none() && content_type_is(ct, "message", "feedback-report") {
            let text = match &part.body {
                PartType::Text(t) => t.to_string(),
                PartType::Binary(b) | PartType::InlineBinary(b) => {
                    String::from_utf8_lossy(b).into_owned()
                }
                // A parser that read the part as a nested message: its raw
                // bytes are the fields.
                _ => String::from_utf8_lossy(
                    raw_message
                        .get(part.offset_body as usize..part.offset_end as usize)
                        .unwrap_or_default(),
                )
                .into_owned(),
            };
            fields = Some(feedback_fields(&text));
        } else if reported.is_none() && is_reported_message_part(ct) {
            reported = Some(reported_headers(part));
        }
    }
    let fields = fields.ok_or_else(|| malformed("no message/feedback-report part"))?;
    let feedback_type = field(&fields, "feedback-type");
    if !feedback_type.eq_ignore_ascii_case("auth-failure") {
        return Err(malformed(format!(
            "Feedback-Type is {feedback_type:?}, not auth-failure"
        )));
    }
    for required in ["user-agent", "version"] {
        if field(&fields, required).is_empty() {
            return Err(malformed(format!("feedback report lacks {required}")));
        }
    }

    let (header_from, message_id) = reported.flatten().unwrap_or_default();
    let partial = message_id.is_empty();
    let reported_domain = match field(&fields, "reported-domain") {
        "" => header_from
            .rsplit_once('@')
            .map(|(_, d)| normalise_domain(d))
            .unwrap_or_default(),
        d => normalise_domain(d),
    };
    let submitter = match first_from(&msg) {
        Some((Some(name), addr)) => format!("{name} <{addr}>"),
        Some((None, addr)) => addr,
        None => String::new(),
    };
    let arrival_date = field(&fields, "arrival-date");
    let summary = DmarcForensicSummary {
        auth_failure: clip(field(&fields, "auth-failure")),
        source_ip: clip(field(&fields, "source-ip")),
        original_mail_from: clip(
            field(&fields, "original-mail-from")
                .trim_start_matches('<')
                .trim_end_matches('>'),
        ),
        header_from: clip(&header_from),
        arrival_date: clip(arrival_date),
        delivery_result: clip(field(&fields, "delivery-result")),
        identity_alignment: clip(field(&fields, "identity-alignment")),
        dkim_domain: clip(field(&fields, "dkim-domain")),
        user_agent: clip(field(&fields, "user-agent")),
        partial,
        extra: Default::default(),
    };
    Ok(ForensicReport {
        submitter: clip(&submitter),
        reported_domain: clip(&reported_domain),
        reported_message_id: clip(&message_id),
        reported_arrival_date: arrival_epoch(arrival_date),
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An RFC 6591 §B.1-shaped report: the reported message's headers as
    /// `text/rfc822-headers`.
    fn report(feedback: &str, inner_type: &str, inner: &str) -> Vec<u8> {
        format!(
            "From: DMARC Reports <dmarc-noreply@reporter.test>\r\n\
             To: dmarc-report@example.org\r\n\
             Subject: FW: auth failure report\r\n\
             Message-ID: <report-1@reporter.test>\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/report; report-type=feedback-report;\r\n\
             \tboundary=\"rpt\"\r\n\
             \r\n\
             --rpt\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             \r\n\
             This is an authentication failure report.\r\n\
             --rpt\r\n\
             Content-Type: message/feedback-report\r\n\
             \r\n\
             {feedback}\
             --rpt\r\n\
             Content-Type: {inner_type}\r\n\
             \r\n\
             {inner}\
             --rpt--\r\n"
        )
        .into_bytes()
    }

    const FEEDBACK: &str = "Feedback-Type: auth-failure\r\n\
         User-Agent: Reporter/1.0\r\n\
         Version: 1\r\n\
         Original-Mail-From: <bounce@forger.test>\r\n\
         Original-Rcpt-To: <victim@peer.test>\r\n\
         Arrival-Date: Sat, 23 May 2026 12:00:00 +0000\r\n\
         Source-IP: 203.0.113.9\r\n\
         Reported-Domain: Example.ORG.\r\n\
         Auth-Failure: dmarc\r\n\
         Delivery-Result: reject\r\n\
         Identity-Alignment: none\r\n\
         DKIM-Domain: forger.test\r\n\
         \r\n";

    const INNER: &str = "From: Example Billing <billing@example.org>\r\n\
         To: victim@peer.test\r\n\
         Subject: Your invoice\r\n\
         Message-ID: <forged-42@forger.test>\r\n\
         Date: Sat, 23 May 2026 11:59:58 +0000\r\n\
         \r\n";

    #[test]
    fn classifies_by_the_top_level_content_type() {
        assert!(is_forensic_report(&report(
            FEEDBACK,
            "text/rfc822-headers",
            INNER
        )));
        let aggregate = b"From: r@reporter.test\r\n\
            Content-Type: multipart/mixed; boundary=\"x\"\r\n\r\n--x\r\n\r\nhi\r\n--x--\r\n";
        assert!(!is_forensic_report(aggregate));
        let delivery_report = b"Content-Type: multipart/report; report-type=delivery-status;\r\n\
            \tboundary=\"x\"\r\n\r\n--x--\r\n";
        assert!(!is_forensic_report(delivery_report));
        assert!(!is_forensic_report(b"not a message at all"));
    }

    #[test]
    fn extracts_the_index_without_recipient_or_subject() {
        let raw = report(FEEDBACK, "text/rfc822-headers", INNER);
        let r = parse_forensic_report(&raw).expect("parses");
        assert_eq!(r.submitter, "DMARC Reports <dmarc-noreply@reporter.test>");
        assert_eq!(r.reported_domain, "example.org");
        assert_eq!(r.reported_message_id, "forged-42@forger.test");
        assert_eq!(r.reported_arrival_date, 1_779_537_600);
        let s = &r.summary;
        assert_eq!(s.auth_failure, "dmarc");
        assert_eq!(s.source_ip, "203.0.113.9");
        assert_eq!(s.original_mail_from, "bounce@forger.test");
        assert_eq!(s.header_from, "billing@example.org");
        assert_eq!(s.delivery_result, "reject");
        assert_eq!(s.identity_alignment, "none");
        assert_eq!(s.dkim_domain, "forger.test");
        assert_eq!(s.user_agent, "Reporter/1.0");
        assert!(!s.partial);
        // The § Storage PII rule: no recipient and no subject in the index.
        let index = format!("{r:?}");
        assert!(!index.contains("victim"), "recipient leaked into {index}");
        assert!(!index.contains("invoice"), "subject leaked into {index}");
    }

    #[test]
    fn reads_a_full_rfc822_reported_message() {
        let inner = format!("{INNER}the forged body\r\n");
        let r = parse_forensic_report(&report(FEEDBACK, "message/rfc822", &inner)).expect("parses");
        assert_eq!(r.reported_message_id, "forged-42@forger.test");
        assert_eq!(r.summary.header_from, "billing@example.org");
        assert!(!r.summary.partial);
    }

    #[test]
    fn a_missing_reported_message_is_partial_not_malformed() {
        let raw = report(FEEDBACK, "text/plain", "nothing useful\r\n");
        let r = parse_forensic_report(&raw).expect("stored anyway");
        assert!(r.summary.partial);
        assert_eq!(r.reported_message_id, "");
        assert_eq!(
            r.reported_domain, "example.org",
            "Reported-Domain still names it"
        );
    }

    #[test]
    fn falls_back_to_the_reported_from_domain() {
        let feedback = FEEDBACK.replace("Reported-Domain: Example.ORG.\r\n", "");
        let r = parse_forensic_report(&report(&feedback, "text/rfc822-headers", INNER))
            .expect("parses");
        assert_eq!(r.reported_domain, "example.org");
    }

    #[test]
    fn the_wrapper_is_strict() {
        let not_auth = FEEDBACK.replace("auth-failure", "abuse");
        let no_version = FEEDBACK.replace("Version: 1\r\n", "");
        for (why, raw) in [
            (
                "abuse feedback",
                report(&not_auth, "text/rfc822-headers", INNER),
            ),
            (
                "no Version",
                report(&no_version, "text/rfc822-headers", INNER),
            ),
            (
                "plain multipart/mixed",
                String::from_utf8(report(FEEDBACK, "text/rfc822-headers", INNER))
                    .unwrap()
                    .replace(
                        "multipart/report; report-type=feedback-report;",
                        "multipart/mixed;",
                    )
                    .into_bytes(),
            ),
        ] {
            assert!(
                matches!(parse_forensic_report(&raw), Err(ReportError::Malformed(_))),
                "{why} must be malformed"
            );
        }
    }

    #[test]
    fn an_oversize_message_is_refused_before_parsing() {
        let raw = vec![b'a'; MAX_FORENSIC_MESSAGE_BYTES + 1];
        assert_eq!(parse_forensic_report(&raw), Err(ReportError::Oversize));
    }

    #[test]
    fn folded_fields_unfold_and_the_first_wins() {
        let f = feedback_fields("Auth-Failure: dkim\r\n Source-IP x\r\nAuth-Failure: spf\r\n");
        assert_eq!(field(&f, "auth-failure"), "dkim Source-IP x");
    }
}
