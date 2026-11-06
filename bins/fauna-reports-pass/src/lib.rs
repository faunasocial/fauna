//! The admin's headless reports pass — the pure half.
//!
//! `docs/goal/architecture/org-nests.md` § fauna.build → *Disposable* bounds the
//! client the store-review exemption admits: it lists the open reports and
//! resolves them, renders no media and no HTML, and handles every byte the box
//! returns as untrusted data. This module is that last clause for a terminal:
//! every string a report carries is printed only through [`untrusted`], which
//! escapes what a terminal would act on (C0/C1 controls — an ESC opens a control
//! sequence — and the Unicode format characters that reorder or hide text) and
//! caps its length, so a report cannot repaint the admin's screen or pose as a
//! different report.

use fauna_client_admin::moderation::{
    AbuseReportOutcome, AbuseReportQueueEntry, AbuseReportSubject,
};

/// The longest a single field is printed, in characters, before it is cut.
pub const FIELD_MAX_CHARS: usize = 300;

/// Whether `c` must never reach the terminal raw: a control character, or a
/// format character that reorders or hides text (bidi embeddings, overrides and
/// isolates, zero-width marks, the BOM).
fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}')
}

/// `s` as it is safe to print: every unsafe character escaped as `\u{…}`, and
/// at most `max` characters of the original kept (a cut is marked `…`).
pub fn untrusted(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i == max {
            out.push('…');
            break;
        }
        if is_unsafe(c) {
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

fn field(s: &str) -> String {
    untrusted(s, FIELD_MAX_CHARS)
}

/// The subject as one short, safe phrase.
pub fn subject_phrase(subject: &AbuseReportSubject) -> String {
    match subject {
        AbuseReportSubject::Post { cid } => format!("post {}", field(cid)),
        AbuseReportSubject::Message {
            channel,
            record_cid,
        } => {
            format!("message {} in {}", field(record_cid), field(channel))
        }
        AbuseReportSubject::Actor { actor_id } => format!("account {}", field(actor_id)),
        AbuseReportSubject::Unknown(_) => "a subject this build cannot read".to_string(),
    }
}

/// One open report as one tab-separated line: id, reason, subject, filed-at
/// (microseconds), then the optional fields as `key=value`. Every
/// box-sourced string passes through [`untrusted`], so the line holds no tab
/// or newline of the box's own.
pub fn render_entry(entry: &AbuseReportQueueEntry) -> String {
    let mut cols = vec![
        field(&entry.report_id),
        entry.reason.token().to_string(),
        subject_phrase(&entry.subject),
        entry.created_at.to_string(),
    ];
    let optional = [
        ("author", &entry.subject_actor),
        ("reporter", &entry.reporter_handle),
        ("from", &entry.origin_nest),
        ("note", &entry.note),
        ("excerpt", &entry.excerpt),
    ];
    for (key, value) in optional {
        if let Some(v) = value {
            cols.push(format!("{key}={}", field(v)));
        }
    }
    cols.join("\t")
}

/// The outcome a `resolve` names: `acted` or `dismissed`, nothing else.
pub fn parse_outcome(word: &str) -> Option<AbuseReportOutcome> {
    match word {
        "acted" => Some(AbuseReportOutcome::Acted),
        "dismissed" => Some(AbuseReportOutcome::Dismissed),
        _ => None,
    }
}

/// A report id as the admin typed it: lowercase hex, 1–128 digits. Checked
/// before anything connects, so a typo never reaches the box.
pub fn valid_report_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The admin's Ed25519 seed from the first line of `input`: 64 hex digits.
pub fn parse_seed(input: &str) -> Result<[u8; 32], &'static str> {
    let line = input.lines().next().unwrap_or("").trim();
    let bytes = hex::decode(line).map_err(|_| "the seed on stdin is not hex")?;
    bytes
        .try_into()
        .map_err(|_| "the seed on stdin is not 32 bytes (64 hex digits)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_admin::moderation::AbuseReportReason;

    fn entry() -> AbuseReportQueueEntry {
        AbuseReportQueueEntry {
            report_id: "ab12".into(),
            subject: AbuseReportSubject::Actor {
                actor_id: "cd34".into(),
            },
            subject_actor: None,
            reason: AbuseReportReason::Spam,
            note: None,
            excerpt: None,
            reporter_handle: None,
            origin_nest: None,
            created_at: 7,
            extra: Default::default(),
        }
    }

    #[test]
    fn an_escape_sequence_is_printed_inert() {
        let s = untrusted("a\x1b[2Jb\x07", 100);
        assert_eq!(s, "a\\u{1b}[2Jb\\u{7}");
        assert!(!s.chars().any(char::is_control));
    }

    #[test]
    fn tabs_newlines_and_c1_controls_cannot_split_or_steer_a_line() {
        let s = untrusted("x\ty\nz\r\u{9b}", 100);
        assert_eq!(s, "x\\u{9}y\\u{a}z\\u{d}\\u{9b}");
    }

    #[test]
    fn bidi_overrides_and_zero_width_marks_are_escaped() {
        let s = untrusted("ok\u{202E}gnp.exe\u{200B}\u{2066}\u{FEFF}", 100);
        assert_eq!(s, "ok\\u{202e}gnp.exe\\u{200b}\\u{2066}\\u{feff}");
    }

    #[test]
    fn ordinary_text_passes_unchanged() {
        assert_eq!(
            untrusted("Spam ünïcode 日本 🙂", 100),
            "Spam ünïcode 日本 🙂"
        );
    }

    #[test]
    fn a_long_field_is_cut_and_marked() {
        assert_eq!(untrusted("abcdef", 3), "abc…");
        assert_eq!(untrusted("abc", 3), "abc");
    }

    #[test]
    fn a_line_holds_only_its_own_tabs() {
        let mut e = entry();
        e.note = Some("evil\tcolumn\nnext line".into());
        e.reporter_handle = Some("r@x".into());
        let line = render_entry(&e);
        assert_eq!(
            line,
            "ab12\tspam\taccount cd34\t7\treporter=r@x\tnote=evil\\u{9}column\\u{a}next line"
        );
        assert!(!line.contains('\n'));
        assert_eq!(line.matches('\t').count(), 5);
    }

    #[test]
    fn only_the_two_outcomes_parse() {
        assert_eq!(parse_outcome("acted"), Some(AbuseReportOutcome::Acted));
        assert_eq!(
            parse_outcome("dismissed"),
            Some(AbuseReportOutcome::Dismissed)
        );
        assert_eq!(parse_outcome("unknown"), None);
        assert_eq!(parse_outcome("Acted"), None);
    }

    #[test]
    fn report_ids_are_hex_only() {
        assert!(valid_report_id("00ffAB"));
        assert!(!valid_report_id(""));
        assert!(!valid_report_id("ab;rm"));
        assert!(!valid_report_id(&"a".repeat(129)));
    }

    #[test]
    fn the_seed_is_one_hex_line_of_32_bytes() {
        let hexed = "11".repeat(32);
        assert_eq!(parse_seed(&format!("{hexed}\n")).unwrap(), [0x11; 32]);
        assert!(parse_seed("zz").is_err());
        assert!(parse_seed(&"11".repeat(31)).is_err());
    }
}
