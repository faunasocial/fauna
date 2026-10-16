//! DMARC aggregate-report **receive**: find the report in an inbound message,
//! bound and decompress it, and parse the RFC 7489 §A.1 XML into the fields the
//! nest stores plus the summary the admin reads
//! (`docs/goal/behavior/dmarc-reporting.md` § Aggregate-report receive (RUA)).
//!
//! The payload is attacker-supplied XML inside a compressed attachment, so every
//! stage is bounded before the next one reads it: the compressed attachment is
//! capped at [`MAX_COMPRESSED_BYTES`], decompression stops at
//! [`MAX_DECOMPRESSED_BYTES`], and the XML is read by the small strict reader in
//! this module rather than a general-purpose parser. That reader accepts only
//! what an aggregate report needs — elements, attributes, character data,
//! comments, processing instructions, CDATA and the five predefined entities
//! plus character references — and **refuses any `<!DOCTYPE` or other markup
//! declaration outright**, so entity expansion and external-entity fetches
//! cannot happen by construction. Element nesting is capped at [`MAX_DEPTH`].
//!
//! Pure (no I/O): the nest calls it from `ingest_dmarc_report`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::net::IpAddr;

use fauna_protocol::bridge_routing::{DmarcAggregateSummary, ReportCountEntry};

/// Largest compressed report attachment accepted (§ XML validation step 1).
pub const MAX_COMPRESSED_BYTES: usize = 50 * 1024 * 1024;
/// Largest decompressed XML accepted (§ XML validation step 1).
pub const MAX_DECOMPRESSED_BYTES: usize = 500 * 1024 * 1024;
/// Deepest element nesting the reader accepts. An aggregate report nests five
/// deep (`feedback/record/row/policy_evaluated/reason`); the margin covers
/// reporters' extension elements.
pub const MAX_DEPTH: usize = 32;
/// Most `<record>` rows one report may carry.
pub const MAX_RECORDS: usize = 1_000_000;
/// Length of each summary top-N list.
pub const SUMMARY_TOP_N: usize = 10;

/// Why a message did not yield a storable report. Every variant is still a
/// `250` on the SMTP wire (§ Architectural rules → accept-on-malformed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportError {
    /// Over [`MAX_COMPRESSED_BYTES`] or [`MAX_DECOMPRESSED_BYTES`].
    Oversize,
    /// No report attachment, an undecodable container, ill-formed XML, or a
    /// missing / invalid required field. The string is for the log line.
    Malformed(String),
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReportError::Oversize => f.write_str("report exceeds the size bounds"),
            ReportError::Malformed(why) => write!(f, "malformed report: {why}"),
        }
    }
}

impl std::error::Error for ReportError {}

pub(crate) fn malformed(why: impl Into<String>) -> ReportError {
    ReportError::Malformed(why.into())
}

/// The evaluated disposition of one `<record>` row (RFC 7489 §A.1
/// `DispositionType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    None,
    Quarantine,
    Reject,
}

/// One `<record>` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateRecord {
    pub source_ip: IpAddr,
    pub count: u64,
    pub disposition: Disposition,
    /// `policy_evaluated/dkim` was `pass` (aligned DKIM).
    pub dkim_pass: bool,
    /// `policy_evaluated/spf` was `pass` (aligned SPF).
    pub spf_pass: bool,
    /// `identifiers/header_from`, lowercased; empty when the row omits it.
    pub header_from: String,
}

/// The parsed fields of one aggregate report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateReport {
    pub org_name: String,
    pub email: String,
    pub report_id: String,
    pub begin_at: i64,
    pub end_at: i64,
    /// `policy_published/domain`, lowercased, without a trailing dot.
    pub policy_domain: String,
    pub records: Vec<AggregateRecord>,
}

impl AggregateReport {
    /// The storage key's submitter: `report_metadata.org_name` and `email`
    /// joined (§ Storage).
    pub fn submitter(&self) -> String {
        match (self.org_name.is_empty(), self.email.is_empty()) {
            (false, false) => format!("{} <{}>", self.org_name, self.email),
            (false, true) => self.org_name.clone(),
            _ => self.email.clone(),
        }
    }

    /// The per-report index stored as `parsed_summary_cbor`.
    pub fn summary(&self) -> DmarcAggregateSummary {
        let mut s = DmarcAggregateSummary {
            record_count: self.records.len() as u64,
            ..Default::default()
        };
        let mut by_ip: HashMap<String, u64> = HashMap::new();
        let mut by_from: HashMap<String, u64> = HashMap::new();
        for r in &self.records {
            s.message_count = s.message_count.saturating_add(r.count);
            let bucket = match r.disposition {
                Disposition::None => &mut s.disposition_none,
                Disposition::Quarantine => &mut s.disposition_quarantine,
                Disposition::Reject => &mut s.disposition_reject,
            };
            *bucket = bucket.saturating_add(r.count);
            if r.dkim_pass {
                s.dkim_pass = s.dkim_pass.saturating_add(r.count);
            }
            if r.spf_pass {
                s.spf_pass = s.spf_pass.saturating_add(r.count);
            }
            if r.dkim_pass || r.spf_pass {
                s.dmarc_pass = s.dmarc_pass.saturating_add(r.count);
            }
            let ip = by_ip.entry(r.source_ip.to_string()).or_default();
            *ip = ip.saturating_add(r.count);
            if !r.header_from.is_empty() {
                let from = by_from.entry(r.header_from.clone()).or_default();
                *from = from.saturating_add(r.count);
            }
        }
        s.top_source_ips = top_n(by_ip);
        s.top_header_from = top_n(by_from);
        s
    }
}

/// Largest count first; ties by value so the order is deterministic.
pub(crate) fn top_n(counts: HashMap<String, u64>) -> Vec<ReportCountEntry> {
    let mut v: Vec<ReportCountEntry> = counts
        .into_iter()
        .map(|(value, count)| ReportCountEntry {
            value,
            count,
            extra: Default::default(),
        })
        .collect();
    v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
    v.truncate(SUMMARY_TOP_N);
    v
}

/// Find the aggregate report in an RFC 5322 message and return its XML,
/// decompressed and bounded. Each MIME part is considered whose content type
/// or file name marks it as a report container (`application/gzip`,
/// `application/zip`, their `x-` aliases, `application/octet-stream`, XML
/// types, or a `.gz` / `.zip` / `.xml` name); the container is recognised by
/// its magic bytes, not its label. The first part that yields XML wins.
pub fn extract_report_xml(raw_message: &[u8]) -> Result<Vec<u8>, ReportError> {
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
            Ok(xml) => return Ok(xml),
            Err(ReportError::Oversize) => return Err(ReportError::Oversize),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

fn is_report_candidate(ct: Option<&mail_parser::ContentType<'_>>, name: Option<&str>) -> bool {
    let name_hit = name.is_some_and(|n| {
        let n = n.to_ascii_lowercase();
        n.ends_with(".gz") || n.ends_with(".zip") || n.ends_with(".xml")
    });
    let type_hit = ct.is_some_and(|ct| {
        let main = ct.ctype().to_ascii_lowercase();
        let sub = ct.subtype().unwrap_or_default().to_ascii_lowercase();
        matches!(
            (main.as_str(), sub.as_str()),
            ("application", "gzip")
                | ("application", "x-gzip")
                | ("application", "zip")
                | ("application", "x-zip-compressed")
                | ("application", "octet-stream")
                | ("application", "xml")
                | ("text", "xml")
        )
    });
    name_hit || type_hit
}

/// Decompress one report container — gzip, zip (the first `.xml` entry, else
/// the first file) or bare XML — under the size bounds.
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
        let decoder = flate2::read::MultiGzDecoder::new(body);
        return read_bounded(decoder, max_decompressed);
    }
    if body.starts_with(b"PK\x03\x04") {
        let mut archive = zip::ZipArchive::new(Cursor::new(body))
            .map_err(|e| malformed(format!("zip container: {e}")))?;
        let mut pick = None;
        for i in 0..archive.len() {
            let entry = archive
                .by_index(i)
                .map_err(|e| malformed(format!("zip entry: {e}")))?;
            if !entry.is_file() {
                continue;
            }
            if entry.name().to_ascii_lowercase().ends_with(".xml") {
                pick = Some(i);
                break;
            }
            pick.get_or_insert(i);
        }
        let i = pick.ok_or_else(|| malformed("zip container holds no file"))?;
        let entry = archive
            .by_index(i)
            .map_err(|e| malformed(format!("zip entry: {e}")))?;
        return read_bounded(entry, max_decompressed);
    }
    let start = body
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(body.len());
    let rest = &body[start..];
    if rest.starts_with(b"<") || rest.starts_with(b"\xef\xbb\xbf<") {
        if body.len() > max_decompressed {
            return Err(ReportError::Oversize);
        }
        return Ok(body.to_vec());
    }
    Err(malformed("attachment is not gzip, zip or XML"))
}

/// Read a decompressing stream to its end, refusing past `max` bytes (one
/// byte over is enough to know).
pub(crate) fn read_bounded(r: impl Read, max: usize) -> Result<Vec<u8>, ReportError> {
    let mut out = Vec::new();
    r.take(max as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| malformed(format!("decompression: {e}")))?;
    if out.len() > max {
        return Err(ReportError::Oversize);
    }
    Ok(out)
}

/// Gzip the report XML for the `payload_xml_gz` archive column.
pub fn gzip_xml(xml: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    // Writing into a Vec cannot fail.
    enc.write_all(xml).expect("gzip into Vec");
    enc.finish().expect("gzip into Vec")
}

/// Gunzip an archived `payload_xml_gz` (bounded like an inbound report).
pub fn gunzip_xml(gz: &[u8]) -> Result<Vec<u8>, ReportError> {
    read_bounded(
        flate2::read::MultiGzDecoder::new(gz),
        MAX_DECOMPRESSED_BYTES,
    )
}

/// Parse an aggregate report's XML and check the required fields
/// (§ XML validation steps 2–3).
pub fn parse_aggregate_xml(xml: &[u8]) -> Result<AggregateReport, ReportError> {
    let text = std::str::from_utf8(xml).map_err(|_| malformed("report XML is not UTF-8"))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = XmlReader::new(text);

    let mut path: Vec<&str> = Vec::new();
    let mut buf = String::new();
    let mut meta = MetaFields::default();
    let mut records: Vec<AggregateRecord> = Vec::new();
    let mut rec: Option<RecordFields> = None;

    while let Some(ev) = reader.next_event()? {
        match ev {
            XmlEvent::Start(name) => {
                if path.is_empty() && name != "feedback" {
                    return Err(malformed(format!(
                        "root element is <{name}>, not <feedback>"
                    )));
                }
                path.push(name);
                buf.clear();
                if path.as_slice() == ["feedback", "record"] {
                    if records.len() >= MAX_RECORDS {
                        return Err(malformed("too many <record> rows"));
                    }
                    rec = Some(RecordFields::default());
                }
            }
            XmlEvent::Text(t) => buf.push_str(&t),
            XmlEvent::End => {
                let value = buf.trim().to_string();
                buf.clear();
                match path.as_slice() {
                    ["feedback", "report_metadata", "org_name"] => meta.org_name = value,
                    ["feedback", "report_metadata", "email"] => meta.email = value,
                    ["feedback", "report_metadata", "report_id"] => meta.report_id = value,
                    ["feedback", "report_metadata", "date_range", "begin"] => {
                        meta.begin = Some(value)
                    }
                    ["feedback", "report_metadata", "date_range", "end"] => meta.end = Some(value),
                    ["feedback", "policy_published", "domain"] => meta.domain = value,
                    ["feedback", "record"] => {
                        let fields = rec.take().unwrap_or_default();
                        records.push(fields.validate(records.len())?);
                    }
                    ["feedback", "record", rest @ ..] => {
                        if let Some(r) = rec.as_mut() {
                            r.set(rest, value);
                        }
                    }
                    _ => {}
                }
                path.pop();
            }
        }
    }
    meta.validate(records)
}

#[derive(Default)]
struct MetaFields {
    org_name: String,
    email: String,
    report_id: String,
    begin: Option<String>,
    end: Option<String>,
    domain: String,
}

impl MetaFields {
    fn validate(self, records: Vec<AggregateRecord>) -> Result<AggregateReport, ReportError> {
        if self.report_id.is_empty() {
            return Err(malformed("report_metadata.report_id missing"));
        }
        if self.org_name.is_empty() && self.email.is_empty() {
            return Err(malformed("report_metadata names no org_name or email"));
        }
        let begin_at = parse_epoch(self.begin.as_deref(), "date_range.begin")?;
        let end_at = parse_epoch(self.end.as_deref(), "date_range.end")?;
        let policy_domain = self.domain.trim_end_matches('.').to_ascii_lowercase();
        if policy_domain.is_empty() {
            return Err(malformed("policy_published.domain missing"));
        }
        if records.is_empty() {
            return Err(malformed("report carries no <record>"));
        }
        Ok(AggregateReport {
            org_name: self.org_name,
            email: self.email,
            report_id: self.report_id,
            begin_at,
            end_at,
            policy_domain,
            records,
        })
    }
}

fn parse_epoch(v: Option<&str>, field: &str) -> Result<i64, ReportError> {
    let v = v.ok_or_else(|| malformed(format!("report_metadata.{field} missing")))?;
    v.parse::<u32>()
        .map(i64::from)
        .map_err(|_| malformed(format!("report_metadata.{field} is not a timestamp")))
}

#[derive(Default)]
struct RecordFields {
    source_ip: Option<String>,
    count: Option<String>,
    disposition: Option<String>,
    dkim: String,
    spf: String,
    header_from: String,
}

impl RecordFields {
    fn set(&mut self, rest: &[&str], value: String) {
        match rest {
            ["row", "source_ip"] => self.source_ip = Some(value),
            ["row", "count"] => self.count = Some(value),
            ["row", "policy_evaluated", "disposition"] => self.disposition = Some(value),
            ["row", "policy_evaluated", "dkim"] => self.dkim = value,
            ["row", "policy_evaluated", "spf"] => self.spf = value,
            ["identifiers", "header_from"] => self.header_from = value,
            _ => {}
        }
    }

    fn validate(self, index: usize) -> Result<AggregateRecord, ReportError> {
        let row =
            |field: &str| malformed(format!("record {index}: row.{field} missing or invalid"));
        let source_ip = self
            .source_ip
            .and_then(|s| s.parse::<IpAddr>().ok())
            .ok_or_else(|| row("source_ip"))?;
        let count = self
            .count
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| row("count"))?;
        let disposition = match self.disposition.as_deref() {
            Some("none") => Disposition::None,
            Some("quarantine") => Disposition::Quarantine,
            Some("reject") => Disposition::Reject,
            _ => return Err(row("policy_evaluated.disposition")),
        };
        Ok(AggregateRecord {
            source_ip,
            count,
            disposition,
            dkim_pass: self.dkim == "pass",
            spf_pass: self.spf == "pass",
            header_from: self.header_from.trim_end_matches('.').to_ascii_lowercase(),
        })
    }
}

// ── The strict XML reader ────────────────────────────────────────

enum XmlEvent<'a> {
    /// An element opened; the local name (any `prefix:` stripped).
    Start(&'a str),
    /// Character data inside an element, entities decoded.
    Text(Cow<'a, str>),
    /// The innermost open element closed.
    End,
}

struct XmlReader<'a> {
    s: &'a str,
    pos: usize,
    /// Qualified names of the open elements, for end-tag matching.
    stack: Vec<&'a str>,
    root_done: bool,
    /// A self-closing tag owes an `End` after its `Start`.
    pending_end: bool,
}

impl<'a> XmlReader<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            s,
            pos: 0,
            stack: Vec::new(),
            root_done: false,
            pending_end: false,
        }
    }

    fn rest(&self) -> &'a str {
        &self.s[self.pos..]
    }

    /// Skip past `end`, which must occur; returns the text before it.
    fn skip_past(&mut self, end: &str, what: &str) -> Result<&'a str, ReportError> {
        let rest = self.rest();
        let i = rest
            .find(end)
            .ok_or_else(|| malformed(format!("unterminated {what}")))?;
        self.pos += i + end.len();
        Ok(&rest[..i])
    }

    fn next_event(&mut self) -> Result<Option<XmlEvent<'a>>, ReportError> {
        if self.pending_end {
            self.pending_end = false;
            self.close_one();
            return Ok(Some(XmlEvent::End));
        }
        loop {
            let rest = self.rest();
            if rest.is_empty() {
                if !self.stack.is_empty() {
                    return Err(malformed("document ends inside an element"));
                }
                if !self.root_done {
                    return Err(malformed("document has no root element"));
                }
                return Ok(None);
            }
            if rest.starts_with("<?") {
                self.pos += 2;
                self.skip_past("?>", "processing instruction")?;
                continue;
            }
            if rest.starts_with("<!--") {
                self.pos += 4;
                self.skip_past("-->", "comment")?;
                continue;
            }
            if rest.starts_with("<![CDATA[") {
                if self.stack.is_empty() {
                    return Err(malformed("CDATA outside the root element"));
                }
                self.pos += 9;
                let data = self.skip_past("]]>", "CDATA section")?;
                return Ok(Some(XmlEvent::Text(Cow::Borrowed(data))));
            }
            if rest.starts_with("<!") {
                // DOCTYPE, ENTITY, ELEMENT, … — never needed by a report, and
                // the door to entity-expansion and external-entity attacks.
                return Err(malformed("markup declarations (<!DOCTYPE …>) are refused"));
            }
            if rest.starts_with("</") {
                self.pos += 2;
                let name = self.skip_past(">", "end tag")?.trim_end();
                match self.stack.last() {
                    Some(open) if *open == name => {}
                    _ => return Err(malformed(format!("mismatched end tag </{name}>"))),
                }
                self.close_one();
                return Ok(Some(XmlEvent::End));
            }
            if rest.starts_with('<') {
                return self.start_tag().map(Some);
            }
            // Character data up to the next markup.
            let len = rest.find('<').unwrap_or(rest.len());
            let raw = &rest[..len];
            self.pos += len;
            if self.stack.is_empty() {
                if raw.trim().is_empty() {
                    continue;
                }
                return Err(malformed("text outside the root element"));
            }
            return decode_entities(raw).map(|t| Some(XmlEvent::Text(t)));
        }
    }

    fn close_one(&mut self) {
        self.stack.pop();
        if self.stack.is_empty() {
            self.root_done = true;
        }
    }

    fn start_tag(&mut self) -> Result<XmlEvent<'a>, ReportError> {
        if self.stack.is_empty() && self.root_done {
            return Err(malformed("more than one root element"));
        }
        if self.stack.len() >= MAX_DEPTH {
            return Err(malformed("elements nest too deep"));
        }
        self.pos += 1;
        let rest = self.rest();
        let name_len = rest
            .find(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
            .ok_or_else(|| malformed("unterminated start tag"))?;
        let qname = &rest[..name_len];
        if !is_name(qname) {
            return Err(malformed(format!("invalid element name {qname:?}")));
        }
        self.pos += name_len;
        // Attributes: validated for shape, values ignored (a report's data is
        // all element content; `xmlns` declarations need no resolution because
        // names are matched by local part).
        loop {
            let rest = self.rest();
            let trimmed = rest.trim_start();
            self.pos += rest.len() - trimmed.len();
            if trimmed.starts_with("/>") {
                self.pos += 2;
                self.stack.push(qname);
                self.pending_end = true;
                return Ok(XmlEvent::Start(local_name(qname)));
            }
            if trimmed.starts_with('>') {
                self.pos += 1;
                self.stack.push(qname);
                return Ok(XmlEvent::Start(local_name(qname)));
            }
            let eq = trimmed
                .find('=')
                .ok_or_else(|| malformed("attribute without a value"))?;
            if !is_name(trimmed[..eq].trim_end()) {
                return Err(malformed("invalid attribute name"));
            }
            self.pos += eq + 1;
            let rest = self.rest();
            let trimmed = rest.trim_start();
            self.pos += rest.len() - trimmed.len();
            let quote = match trimmed.chars().next() {
                Some('"') => "\"",
                Some('\'') => "'",
                _ => return Err(malformed("unquoted attribute value")),
            };
            self.pos += 1;
            let value = self.skip_past(quote, "attribute value")?;
            if value.contains('<') {
                return Err(malformed("'<' in an attribute value"));
            }
            decode_entities(value)?;
        }
    }
}

fn local_name(qname: &str) -> &str {
    qname.rsplit(':').next().unwrap_or(qname)
}

fn is_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == ':')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '-' | '.'))
}

/// Decode the five predefined entities and character references; any other
/// `&name;` is refused (no DTD can declare one).
fn decode_entities(raw: &str) -> Result<Cow<'_, str>, ReportError> {
    if !raw.contains('&') {
        return Ok(Cow::Borrowed(raw));
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        let end = rest
            .find(';')
            .filter(|&e| e <= 10)
            .ok_or_else(|| malformed("unterminated entity reference"))?;
        let entity = &rest[..end];
        let ch = match entity {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = if let Some(hex) = entity.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16).ok()
                } else if let Some(dec) = entity.strip_prefix('#') {
                    dec.parse::<u32>().ok()
                } else {
                    None
                };
                code.and_then(char::from_u32)
                    .ok_or_else(|| malformed(format!("undeclared entity &{entity};")))?
            }
        };
        out.push(ch);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(Cow::Owned(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = r#"<?xml version="1.0" encoding="UTF-8" ?>
<feedback xmlns="urn:ietf:params:xml:ns:dmarc-2.0">
  <!-- a comment -->
  <report_metadata>
    <org_name>reporter.test</org_name>
    <email>dmarc-noreply@reporter.test</email>
    <report_id>17321789264532&amp;1</report_id>
    <date_range><begin>1700000000</begin><end>1700086399</end></date_range>
  </report_metadata>
  <policy_published>
    <domain>Example.ORG.</domain><adkim>s</adkim><aspf>s</aspf><p>reject</p><sp>reject</sp><pct>100</pct>
  </policy_published>
  <record>
    <row>
      <source_ip>192.0.2.10</source_ip>
      <count>7</count>
      <policy_evaluated><disposition>none</disposition><dkim>pass</dkim><spf>pass</spf></policy_evaluated>
    </row>
    <identifiers><header_from>example.org</header_from></identifiers>
    <auth_results><dkim><domain>example.org</domain><result>pass</result></dkim></auth_results>
  </record>
  <record>
    <row>
      <source_ip>2001:db8::1</source_ip>
      <count>3</count>
      <policy_evaluated><disposition>reject</disposition><dkim>fail</dkim><spf>fail</spf></policy_evaluated>
    </row>
    <identifiers><header_from><![CDATA[example.org]]></header_from></identifiers>
  </record>
</feedback>
"#;

    #[test]
    fn parses_the_required_fields_and_summarises() {
        let r = parse_aggregate_xml(REPORT.as_bytes()).expect("parses");
        assert_eq!(r.report_id, "17321789264532&1");
        assert_eq!(r.submitter(), "reporter.test <dmarc-noreply@reporter.test>");
        assert_eq!((r.begin_at, r.end_at), (1_700_000_000, 1_700_086_399));
        assert_eq!(r.policy_domain, "example.org");
        assert_eq!(r.records.len(), 2);
        let s = r.summary();
        assert_eq!(s.record_count, 2);
        assert_eq!(s.message_count, 10);
        assert_eq!((s.disposition_none, s.disposition_reject), (7, 3));
        assert_eq!((s.dkim_pass, s.spf_pass, s.dmarc_pass), (7, 7, 7));
        assert_eq!(s.top_source_ips[0].value, "192.0.2.10");
        assert_eq!(s.top_source_ips[1].value, "2001:db8::1");
        assert_eq!(
            s.top_header_from,
            vec![ReportCountEntry {
                value: "example.org".into(),
                count: 10,
                extra: Default::default(),
            }]
        );
    }

    #[test]
    fn refuses_a_doctype_before_any_entity_is_seen() {
        let xml = r#"<?xml version="1.0"?><!DOCTYPE lolz [<!ENTITY lol "lol">]><feedback>&lol;</feedback>"#;
        assert!(matches!(
            parse_aggregate_xml(xml.as_bytes()),
            Err(ReportError::Malformed(m)) if m.contains("DOCTYPE")
        ));
    }

    #[test]
    fn refuses_undeclared_entities_and_ill_formed_documents() {
        for bad in [
            "<feedback>&lol;</feedback>",
            "<feedback><report_metadata></feedback>",
            "<feedback></feedback><feedback></feedback>",
            "<feedback>",
            "text<feedback></feedback>",
            "<feedback a=b></feedback>",
            "<other></other>",
        ] {
            assert!(
                matches!(
                    parse_aggregate_xml(bad.as_bytes()),
                    Err(ReportError::Malformed(_))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn refuses_nesting_past_the_depth_cap() {
        let mut xml = String::from("<feedback>");
        for _ in 0..MAX_DEPTH {
            xml.push_str("<x>");
        }
        assert!(matches!(
            parse_aggregate_xml(xml.as_bytes()),
            Err(ReportError::Malformed(m)) if m.contains("deep")
        ));
    }

    #[test]
    fn a_missing_required_field_is_malformed() {
        for (from, to) in [
            ("<report_id>17321789264532&amp;1</report_id>", ""),
            ("<begin>1700000000</begin>", ""),
            ("<domain>Example.ORG.</domain>", ""),
            ("<source_ip>192.0.2.10</source_ip>", ""),
            ("<count>7</count>", "<count>seven</count>"),
            (
                "<disposition>none</disposition>",
                "<disposition>maybe</disposition>",
            ),
        ] {
            let xml = REPORT.replacen(from, to, 1);
            assert!(
                matches!(
                    parse_aggregate_xml(xml.as_bytes()),
                    Err(ReportError::Malformed(_))
                ),
                "removing {from}"
            );
        }
    }

    #[test]
    fn unpacks_gzip_zip_and_bare_xml() {
        let gz = gzip_xml(REPORT.as_bytes());
        assert_eq!(unpack(&gz).unwrap(), REPORT.as_bytes());
        assert_eq!(gunzip_xml(&gz).unwrap(), REPORT.as_bytes());
        assert_eq!(unpack(REPORT.as_bytes()).unwrap(), REPORT.as_bytes());

        let mut zipped = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut zipped));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            w.start_file("reporter.test!example.org!1700000000!1700086399.xml", opts)
                .unwrap();
            w.write_all(REPORT.as_bytes()).unwrap();
            w.finish().unwrap();
        }
        assert_eq!(unpack(&zipped).unwrap(), REPORT.as_bytes());
        assert!(matches!(
            unpack(b"\x00\x01garbage"),
            Err(ReportError::Malformed(_))
        ));
    }

    #[test]
    fn a_decompression_bomb_is_oversize() {
        // The production bounds scaled down 1000x: a 512 KiB run of zeros
        // gzips to under a kilobyte and must be refused past the 500 KiB cap,
        // and an over-cap compressed body is refused before it is read.
        let (max_c, max_d) = (MAX_COMPRESSED_BYTES / 1000, MAX_DECOMPRESSED_BYTES / 1000);
        let bomb = gzip_xml(&vec![0u8; max_d + 1]);
        assert!(bomb.len() < max_c);
        assert_eq!(
            unpack_bounded(&bomb, max_c, max_d),
            Err(ReportError::Oversize)
        );
        let fits = gzip_xml(&vec![0u8; max_d]);
        assert!(unpack_bounded(&fits, max_c, max_d).is_ok());
        assert_eq!(
            unpack_bounded(&vec![0x1f; max_c + 1], max_c, max_d),
            Err(ReportError::Oversize)
        );
    }

    // `base64` is an `outbound` dependency, on in the default feature set.
    #[cfg(feature = "outbound")]
    #[test]
    fn extracts_the_report_from_a_mime_message() {
        use base64::Engine;
        let gz = gzip_xml(REPORT.as_bytes());
        let b64 = base64::engine::general_purpose::STANDARD.encode(&gz);
        let msg = format!(
            "From: dmarc-noreply@reporter.test\r\n\
             To: dmarc-report@example.org\r\n\
             Subject: Report domain: example.org\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
             --b\r\nContent-Type: text/plain\r\n\r\nThis is an aggregate report.\r\n\
             --b\r\nContent-Type: application/gzip; name=\"r.xml.gz\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             Content-Disposition: attachment; filename=\"r.xml.gz\"\r\n\r\n{b64}\r\n--b--\r\n"
        );
        assert_eq!(
            extract_report_xml(msg.as_bytes()).unwrap(),
            REPORT.as_bytes()
        );

        let plain = "From: someone@sender.test\r\nTo: dmarc-report@example.org\r\nSubject: hi\r\n\r\nhello\r\n";
        assert!(matches!(
            extract_report_xml(plain.as_bytes()),
            Err(ReportError::Malformed(_))
        ));
    }
}
