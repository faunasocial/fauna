//! TLSRPT inbound report **receive**: find the RFC 8460 report in an inbound
//! message, bound and decompress it, and parse its JSON into the fields the
//! nest stores plus the summary the admin reads
//! (`docs/goal/behavior/smtp-server.md` § TLSRPT inbound report listener).
//!
//! The DMARC aggregate leg's twin (`crate::dmarc_report`), sharing its error
//! type, bounded reader and top-N helper. The payload is attacker-supplied JSON
//! inside a (usually gzipped) attachment, so the compressed part is capped at
//! [`MAX_COMPRESSED_BYTES`] and decompression stops at
//! [`MAX_DECOMPRESSED_BYTES`] before `serde_json` reads a byte; its own
//! recursion limit bounds nesting. Only the fields a report needs are read —
//! everything else (the `policy-string` lines, `mx-host`, additional failure
//! information) is skipped unparsed.
//!
//! Pure (no I/O): the nest calls it from `ingest_tlsrpt_report`.

use std::collections::HashMap;
use std::io::Write;

use fauna_protocol::bridge_routing::TlsrptReportSummary;
use serde::Deserialize;

use crate::dmarc_report::{ReportError, malformed, read_bounded, top_n};

/// Largest compressed report part accepted. A TLSRPT report is one JSON
/// document per policy domain per day — kilobytes in practice.
pub const MAX_COMPRESSED_BYTES: usize = 16 * 1024 * 1024;
/// Largest decompressed JSON accepted.
pub const MAX_DECOMPRESSED_BYTES: usize = 64 * 1024 * 1024;

/// One `failure-details[]` entry (RFC 8460 §4.3), the fields the summary uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsrptFailure {
    pub result_type: String,
    /// Empty when the reporter left it out.
    pub sending_mta_ip: String,
    /// Empty when the reporter left it out.
    pub receiving_mx_hostname: String,
    pub failed_session_count: u64,
}

/// One `policies[]` entry (RFC 8460 §4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsrptPolicy {
    /// `sts`, `tlsa` or `no-policy-found`, as the reporter wrote it.
    pub policy_type: String,
    /// `policy-domain`, lowercased, without a trailing dot.
    pub policy_domain: String,
    pub successful_sessions: u64,
    pub failed_sessions: u64,
    pub failures: Vec<TlsrptFailure>,
}

/// The parsed fields of one TLSRPT report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsrptReport {
    pub organization_name: String,
    pub contact_info: String,
    pub report_id: String,
    /// `date-range.start-datetime` / `end-datetime`, as Unix seconds.
    pub begin_at: i64,
    pub end_at: i64,
    /// Never empty.
    pub policies: Vec<TlsrptPolicy>,
}

impl TlsrptReport {
    /// The storage key's submitter: `organization-name` and `contact-info`
    /// joined, the same shape as the DMARC leg's.
    pub fn submitter(&self) -> String {
        match (
            self.organization_name.is_empty(),
            self.contact_info.is_empty(),
        ) {
            (false, false) => format!("{} <{}>", self.organization_name, self.contact_info),
            (false, true) => self.organization_name.clone(),
            _ => self.contact_info.clone(),
        }
    }

    /// The policy domains the report covers, in report order, deduplicated.
    pub fn policy_domains(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for p in &self.policies {
            if !out.contains(&p.policy_domain.as_str()) {
                out.push(&p.policy_domain);
            }
        }
        out
    }

    /// The per-report index stored as `parsed_summary_cbor`.
    pub fn summary(&self) -> TlsrptReportSummary {
        let mut s = TlsrptReportSummary {
            policy_count: self.policies.len() as u64,
            ..Default::default()
        };
        let mut by_result: HashMap<String, u64> = HashMap::new();
        let mut by_ip: HashMap<String, u64> = HashMap::new();
        let mut by_mx: HashMap<String, u64> = HashMap::new();
        for p in &self.policies {
            s.successful_sessions = s.successful_sessions.saturating_add(p.successful_sessions);
            s.failed_sessions = s.failed_sessions.saturating_add(p.failed_sessions);
            for f in &p.failures {
                let n = f.failed_session_count;
                for (map, key) in [
                    (&mut by_result, &f.result_type),
                    (&mut by_ip, &f.sending_mta_ip),
                    (&mut by_mx, &f.receiving_mx_hostname),
                ] {
                    if !key.is_empty() {
                        let slot = map.entry(key.clone()).or_default();
                        *slot = slot.saturating_add(n);
                    }
                }
            }
        }
        s.top_result_types = top_n(by_result);
        s.top_sending_ips = top_n(by_ip);
        s.top_receiving_mx = top_n(by_mx);
        s
    }
}

/// Find the TLSRPT report in an RFC 5322 message and return its JSON,
/// decompressed and bounded. Each MIME part is considered whose content type
/// or file name marks it as a report (`application/tlsrpt+gzip` /
/// `+json` per RFC 8460 §5.3, generic gzip / JSON / octet-stream types, or a
/// `.gz` / `.json` name); gzip is recognised by its magic bytes, not its label.
/// The first part that yields JSON wins.
pub fn extract_report_json(raw_message: &[u8]) -> Result<Vec<u8>, ReportError> {
    use mail_parser::{MessageParser, MimeHeaders, PartType};

    let msg = MessageParser::default()
        .parse(raw_message)
        .ok_or_else(|| malformed("message does not parse as RFC 5322"))?;
    let mut last_err = malformed("no report attachment");
    for part in &msg.parts {
        let body: &[u8] = match &part.body {
            PartType::Binary(b) | PartType::InlineBinary(b) => b,
            PartType::Text(t) => t.as_bytes(),
            _ => continue,
        };
        if !is_report_candidate(part.content_type(), part.attachment_name()) {
            continue;
        }
        match unpack(body) {
            Ok(json) => return Ok(json),
            Err(ReportError::Oversize) => return Err(ReportError::Oversize),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

fn is_report_candidate(ct: Option<&mail_parser::ContentType<'_>>, name: Option<&str>) -> bool {
    let name_hit = name.is_some_and(|n| {
        let n = n.to_ascii_lowercase();
        n.ends_with(".gz") || n.ends_with(".json")
    });
    let type_hit = ct.is_some_and(|ct| {
        let main = ct.ctype().to_ascii_lowercase();
        let sub = ct.subtype().unwrap_or_default().to_ascii_lowercase();
        main == "application"
            && matches!(
                sub.as_str(),
                "tlsrpt+gzip" | "tlsrpt+json" | "gzip" | "x-gzip" | "json" | "octet-stream"
            )
    });
    name_hit || type_hit
}

/// Decompress one report part — gzip or bare JSON — under the size bounds.
pub fn unpack(body: &[u8]) -> Result<Vec<u8>, ReportError> {
    unpack_bounded(body, MAX_COMPRESSED_BYTES, MAX_DECOMPRESSED_BYTES)
}

fn unpack_bounded(
    body: &[u8],
    max_compressed: usize,
    max_decompressed: usize,
) -> Result<Vec<u8>, ReportError> {
    if body.len() > max_compressed {
        return Err(ReportError::Oversize);
    }
    if body.starts_with(&[0x1f, 0x8b]) {
        return read_bounded(flate2::read::MultiGzDecoder::new(body), max_decompressed);
    }
    let start = body
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(body.len());
    let rest = &body[start..];
    if rest.starts_with(b"{") || rest.starts_with(b"\xef\xbb\xbf{") {
        return Ok(body.to_vec());
    }
    Err(malformed("attachment is not gzip or JSON"))
}

/// Gzip the report JSON for the `payload_json_gz` archive column.
pub fn gzip_json(json: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    // Writing into a Vec cannot fail.
    enc.write_all(json).expect("gzip into Vec");
    enc.finish().expect("gzip into Vec")
}

/// Gunzip an archived `payload_json_gz` (bounded like an inbound report).
pub fn gunzip_json(gz: &[u8]) -> Result<Vec<u8>, ReportError> {
    read_bounded(
        flate2::read::MultiGzDecoder::new(gz),
        MAX_DECOMPRESSED_BYTES,
    )
}

// The RFC 8460 §4.4 schema, the fields read. serde skips every other member.

#[derive(Deserialize)]
struct RawReport {
    #[serde(rename = "organization-name", default)]
    organization_name: String,
    #[serde(rename = "date-range")]
    date_range: Option<RawDateRange>,
    #[serde(rename = "contact-info", default)]
    contact_info: String,
    #[serde(rename = "report-id", default)]
    report_id: String,
    #[serde(default)]
    policies: Vec<RawPolicyEntry>,
}

#[derive(Deserialize)]
struct RawDateRange {
    #[serde(rename = "start-datetime")]
    start: Option<String>,
    #[serde(rename = "end-datetime")]
    end: Option<String>,
}

#[derive(Deserialize)]
struct RawPolicyEntry {
    policy: RawPolicy,
    summary: RawSummary,
    #[serde(rename = "failure-details", default)]
    failure_details: Vec<RawFailure>,
}

#[derive(Deserialize)]
struct RawPolicy {
    #[serde(rename = "policy-type", default)]
    policy_type: String,
    #[serde(rename = "policy-domain", default)]
    policy_domain: String,
}

#[derive(Deserialize)]
struct RawSummary {
    #[serde(rename = "total-successful-session-count")]
    successful: u64,
    #[serde(rename = "total-failure-session-count")]
    failed: u64,
}

#[derive(Deserialize)]
struct RawFailure {
    #[serde(rename = "result-type", default)]
    result_type: String,
    #[serde(rename = "sending-mta-ip", default)]
    sending_mta_ip: String,
    #[serde(rename = "receiving-mx-hostname", default)]
    receiving_mx_hostname: String,
    #[serde(rename = "failed-session-count", default)]
    failed_session_count: u64,
}

/// Parse a TLSRPT report's JSON and check the required fields (RFC 8460 §4.4):
/// a `report-id`, an `organization-name` or `contact-info`, an RFC 3339
/// `date-range`, and at least one policy, each naming its `policy-domain` and
/// carrying both session totals.
pub fn parse_report_json(json: &[u8]) -> Result<TlsrptReport, ReportError> {
    let raw: RawReport =
        serde_json::from_slice(json).map_err(|e| malformed(format!("report JSON: {e}")))?;
    if raw.report_id.is_empty() {
        return Err(malformed("report-id missing"));
    }
    if raw.organization_name.is_empty() && raw.contact_info.is_empty() {
        return Err(malformed(
            "report names no organization-name or contact-info",
        ));
    }
    let range = raw
        .date_range
        .ok_or_else(|| malformed("date-range missing"))?;
    let begin_at = parse_datetime(range.start.as_deref(), "date-range.start-datetime")?;
    let end_at = parse_datetime(range.end.as_deref(), "date-range.end-datetime")?;
    if raw.policies.is_empty() {
        return Err(malformed("report carries no policy"));
    }
    let policies = raw
        .policies
        .into_iter()
        .map(|p| {
            let policy_domain = p
                .policy
                .policy_domain
                .trim_end_matches('.')
                .to_ascii_lowercase();
            if policy_domain.is_empty() {
                return Err(malformed("policy-domain missing"));
            }
            Ok(TlsrptPolicy {
                policy_type: p.policy.policy_type,
                policy_domain,
                successful_sessions: p.summary.successful,
                failed_sessions: p.summary.failed,
                failures: p
                    .failure_details
                    .into_iter()
                    .map(|f| TlsrptFailure {
                        result_type: f.result_type,
                        sending_mta_ip: f.sending_mta_ip,
                        receiving_mx_hostname: f.receiving_mx_hostname,
                        failed_session_count: f.failed_session_count,
                    })
                    .collect(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TlsrptReport {
        organization_name: raw.organization_name,
        contact_info: raw.contact_info,
        report_id: raw.report_id,
        begin_at,
        end_at,
        policies,
    })
}

fn parse_datetime(v: Option<&str>, field: &str) -> Result<i64, ReportError> {
    let v = v.ok_or_else(|| malformed(format!("{field} missing")))?;
    chrono::DateTime::parse_from_rfc3339(v)
        .map(|t| t.timestamp())
        .map_err(|_| malformed(format!("{field} is not an RFC 3339 date-time")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8460 §4.5's example, trimmed, with a second policy for a domain the
    /// nest does not serve.
    const REPORT: &str = r#"{
  "organization-name": "Company-X",
  "date-range": {
    "start-datetime": "2016-04-01T00:00:00Z",
    "end-datetime": "2016-04-01T23:59:59Z"
  },
  "contact-info": "sts-reporting@company-x.example",
  "report-id": "5065427c-23d3-47ca-b6e0-946ea0e8c4be",
  "policies": [{
    "policy": {
      "policy-type": "sts",
      "policy-string": ["version: STSv1", "mode: testing", "mx: *.mail.Company-Y.example", "max_age: 86400"],
      "policy-domain": "Company-Y.example.",
      "mx-host": ["*.mail.company-y.example"]
    },
    "summary": {
      "total-successful-session-count": 5326,
      "total-failure-session-count": 303
    },
    "failure-details": [{
      "result-type": "certificate-expired",
      "sending-mta-ip": "2001:db8:abcd:0012::1",
      "receiving-mx-hostname": "mx1.mail.company-y.example",
      "failed-session-count": 100
    }, {
      "result-type": "starttls-not-supported",
      "sending-mta-ip": "2001:db8:abcd:0013::1",
      "receiving-mx-hostname": "mx2.mail.company-y.example",
      "receiving-ip": "203.0.113.56",
      "failed-session-count": 200,
      "additional-information": "https://reports.company-x.example/report_info?id=5065427c-23d3#StarttlsNotSupported"
    }, {
      "result-type": "validation-failure",
      "sending-mta-ip": "198.51.100.62",
      "receiving-ip": "203.0.113.58",
      "receiving-mx-hostname": "mx-backup.mail.company-y.example",
      "failed-session-count": 3,
      "failure-reason-code": "X509_V_ERR_PROXY_PATH_LENGTH_EXCEEDED"
    }]
  }, {
    "policy": {"policy-type": "no-policy-found", "policy-domain": "other.example"},
    "summary": {"total-successful-session-count": 4, "total-failure-session-count": 0}
  }]
}"#;

    #[test]
    fn parses_the_required_fields_and_summarises() {
        let r = parse_report_json(REPORT.as_bytes()).expect("parses");
        assert_eq!(r.report_id, "5065427c-23d3-47ca-b6e0-946ea0e8c4be");
        assert_eq!(r.submitter(), "Company-X <sts-reporting@company-x.example>");
        assert_eq!((r.begin_at, r.end_at), (1_459_468_800, 1_459_555_199));
        assert_eq!(
            r.policy_domains(),
            vec!["company-y.example", "other.example"]
        );
        assert_eq!(r.policies[0].policy_type, "sts");
        let s = r.summary();
        assert_eq!(s.policy_count, 2);
        assert_eq!((s.successful_sessions, s.failed_sessions), (5330, 303));
        assert_eq!(s.top_result_types[0].value, "starttls-not-supported");
        assert_eq!(s.top_result_types[0].count, 200);
        assert_eq!(s.top_result_types.len(), 3);
        assert_eq!(s.top_sending_ips[2].value, "198.51.100.62");
        assert_eq!(s.top_receiving_mx[1].value, "mx1.mail.company-y.example");
    }

    #[test]
    fn a_missing_required_field_is_malformed() {
        for (from, to) in [
            (
                r#""report-id": "5065427c-23d3-47ca-b6e0-946ea0e8c4be""#,
                r#""report-id": """#,
            ),
            (r#""2016-04-01T00:00:00Z""#, r#""yesterday""#),
            (
                r#""policy-domain": "Company-Y.example.""#,
                r#""policy-domain": """#,
            ),
            (
                r#""total-failure-session-count": 303"#,
                r#""total-failure-session-count": "many""#,
            ),
            (r#""total-successful-session-count": 5326,"#, ""),
        ] {
            let json = REPORT.replacen(from, to, 1);
            assert!(
                matches!(
                    parse_report_json(json.as_bytes()),
                    Err(ReportError::Malformed(_))
                ),
                "replacing {from}"
            );
        }
        let no_org = REPORT
            .replacen(r#""organization-name": "Company-X","#, "", 1)
            .replacen(
                r#""contact-info": "sts-reporting@company-x.example","#,
                "",
                1,
            );
        assert!(matches!(
            parse_report_json(no_org.as_bytes()),
            Err(ReportError::Malformed(_))
        ));
        let no_policies = r#"{"organization-name":"x","report-id":"1","date-range":{"start-datetime":"2016-04-01T00:00:00Z","end-datetime":"2016-04-01T23:59:59Z"},"policies":[]}"#;
        assert!(matches!(
            parse_report_json(no_policies.as_bytes()),
            Err(ReportError::Malformed(_))
        ));
        assert!(matches!(
            parse_report_json(b"<feedback/>"),
            Err(ReportError::Malformed(_))
        ));
    }

    #[test]
    fn unpacks_gzip_and_bare_json() {
        let gz = gzip_json(REPORT.as_bytes());
        assert_eq!(unpack(&gz).unwrap(), REPORT.as_bytes());
        assert_eq!(gunzip_json(&gz).unwrap(), REPORT.as_bytes());
        assert_eq!(unpack(REPORT.as_bytes()).unwrap(), REPORT.as_bytes());
        assert!(matches!(
            unpack(b"\x00\x01garbage"),
            Err(ReportError::Malformed(_))
        ));
    }

    #[test]
    fn a_decompression_bomb_is_oversize() {
        // The production bounds scaled down 1000x.
        let (max_c, max_d) = (MAX_COMPRESSED_BYTES / 1000, MAX_DECOMPRESSED_BYTES / 1000);
        let bomb = gzip_json(&vec![b' '; max_d + 1]);
        assert!(bomb.len() < max_c);
        assert_eq!(
            unpack_bounded(&bomb, max_c, max_d),
            Err(ReportError::Oversize)
        );
        assert_eq!(
            unpack_bounded(&vec![b'{'; max_c + 1], max_c, max_d),
            Err(ReportError::Oversize)
        );
    }

    // `base64` is an `outbound` dependency, on in the default feature set.
    #[cfg(feature = "outbound")]
    #[test]
    fn extracts_the_report_from_a_mime_message() {
        use base64::Engine;
        let gz = gzip_json(REPORT.as_bytes());
        let b64 = base64::engine::general_purpose::STANDARD.encode(&gz);
        let msg = format!(
            "From: tlsrpt-noreply@company-x.example\r\n\
             To: tlsrpt@company-y.example\r\n\
             Subject: Report Domain: company-y.example Submitter: company-x.example Report-ID: <5065427c>\r\n\
             TLS-Report-Domain: company-y.example\r\n\
             TLS-Report-Submitter: company-x.example\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/report; report-type=\"tlsrpt\"; boundary=\"b\"\r\n\r\n\
             --b\r\nContent-Type: text/plain\r\n\r\nThis is an aggregate TLS report.\r\n\
             --b\r\nContent-Type: application/tlsrpt+gzip\r\n\
             Content-Transfer-Encoding: base64\r\n\
             Content-Disposition: attachment; filename=\"company-x.example!company-y.example!1459468800!1459555199.json.gz\"\r\n\r\n{b64}\r\n--b--\r\n"
        );
        assert_eq!(
            extract_report_json(msg.as_bytes()).unwrap(),
            REPORT.as_bytes()
        );

        let plain = "From: someone@sender.test\r\nTo: tlsrpt@company-y.example\r\nSubject: hi\r\n\r\nhello\r\n";
        assert!(matches!(
            extract_report_json(plain.as_bytes()),
            Err(ReportError::Malformed(_))
        ));
    }
}
