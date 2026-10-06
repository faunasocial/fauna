//! The `mbox` serializer — RFC 4155 plus the de facto mboxrd extension.
//!
//! Authority: `docs/goal/behavior/mail-export.md` § Format choices → `mbox`.
//! One text file per source mailbox, messages concatenated behind a
//! `From <envelope-sender> <ctime>` separator line, IMAP flag state carried in
//! the `Status:` / `X-Status:` / `X-Mozilla-Status:` headers every mainstream
//! MUA already reads.
//!
//! # Two deliberate mutations
//!
//! mbox is the one format of the three that cannot be byte-preserving, and
//! both mutations are the format's own definition rather than a choice here:
//!
//! 1. **`>From ` escaping.** A body line beginning `From ` would otherwise read
//!    as the next message's separator. This uses the *mboxrd* rule — escape
//!    `>*From `, i.e. `From `, `>From `, `>>From `, … each gain one `>` — not
//!    the older mboxo rule, because mboxrd is losslessly reversible on import
//!    and mboxo is not.
//! 2. **CRLF → LF.** mbox is defined as a platform text file and every reader
//!    (Thunderbird, Apple Mail, mutt, `formail`) writes LF. The two formats
//!    that *are* byte-preserving containers — Maildir++ and EML zip — keep the
//!    stored CRLF untouched, so a user who needs the exact wire bytes has two
//!    formats that give them.

use super::helpers::{asctime_utc, first_header_value, has_flag};
use super::paths::ComponentNamer;
use super::{ExportEntry, ExportError, ExportMessage};

/// Flag headers this serializer owns. Any copy already on the message is
/// dropped first, so a re-export never accumulates a second generation of them.
const OWNED_FLAG_HEADERS: [&[u8]; 4] = [
    b"Status",
    b"X-Status",
    b"X-Mozilla-Status",
    b"X-Mozilla-Status2",
];

/// Thunderbird's `X-Mozilla-Status` bits (its de facto cross-MUA convention).
const MOZ_READ: u16 = 0x0001;
const MOZ_REPLIED: u16 = 0x0002;
const MOZ_MARKED: u16 = 0x0004;
const MOZ_EXPUNGED: u16 = 0x0008;

/// Fallback envelope sender when a message carries neither `Return-Path:` nor a
/// parseable `From:` — the same placeholder every mbox tool uses.
const UNKNOWN_SENDER: &str = "MAILER-DAEMON";

/// One mailbox's accumulating `.mbox` file.
pub(super) struct MboxState {
    /// The mailbox being written.
    ///
    /// Keyed on the **raw mailbox name**: the namer hands every mailbox a path
    /// of its own (§ Format choices → *Mailbox names in archive paths*, →
    /// *Archive paths on the extracting filesystem*), so "a new mailbox opens"
    /// and "a new file path begins" are the same test — and a mailbox is named
    /// exactly once, which is what the namer's ladder requires.
    open: Option<OpenMailbox>,
    /// Every `.mbox` file shares one suffix, so keeping the *names* apart keeps
    /// the files apart.
    namer: ComponentNamer,
}

struct OpenMailbox {
    mailbox: String,
    component: String,
    buffer: Vec<u8>,
    /// The mailbox's earliest INTERNALDATE, which dates the file.
    earliest: i64,
}

impl Default for MboxState {
    fn default() -> Self {
        Self {
            open: None,
            namer: ComponentNamer::reserving(&[]),
        }
    }
}

impl MboxState {
    pub(super) fn push(
        &mut self,
        root: &str,
        message: &ExportMessage,
        body: &[u8],
    ) -> Result<Vec<ExportEntry>, ExportError> {
        let mut flushed = Vec::new();

        // The input is ordered by mailbox, so a mailbox is complete the moment
        // the next one opens — that is what bounds the buffer to one mailbox.
        let opens_new_mailbox = self
            .open
            .as_ref()
            .is_none_or(|open| open.mailbox != message.mailbox);
        if opens_new_mailbox {
            if let Some(entry) = self.flush(root) {
                flushed.push(entry);
            }
            self.open = Some(OpenMailbox {
                mailbox: message.mailbox.clone(),
                component: self.namer.name(&message.mailbox)?,
                buffer: Vec::new(),
                earliest: message.internal_date_epoch,
            });
        }

        let rendered = render_message(message, body);
        if let Some(OpenMailbox {
            buffer, earliest, ..
        }) = self.open.as_mut()
        {
            buffer.extend_from_slice(&rendered);
            // A running MINIMUM, not the first message's date. Under the
            // pre-2026-09-21 total order the two were the same thing — the
            // input was INTERNALDATE-ascending within a mailbox, so the first
            // message WAS the earliest. The order is `(mailbox, uid)` now
            // (mod.rs's module docs say why), so an imported message can carry
            // an older date on a later UID, and taking the first would date the
            // mbox file by whatever happened to arrive first. Maildir++ and EML
            // zip already tracked the true minimum for their index entries.
            *earliest = (*earliest).min(message.internal_date_epoch);
        }
        Ok(flushed)
    }

    /// Flush the last open mailbox — the only entry mbox cannot emit during
    /// `push`, since a mailbox is complete only once the stream ends.
    pub(super) fn finish(mut self, root: &str) -> Vec<ExportEntry> {
        self.flush(root).into_iter().collect()
    }

    fn flush(&mut self, root: &str) -> Option<ExportEntry> {
        let open = self.open.take()?;
        Some(ExportEntry::file(
            format!("{root}/{}.mbox", open.component),
            open.buffer,
            open.earliest,
        ))
    }
}

/// Render one message as its mbox stanza: separator line, flag headers, escaped
/// body, trailing blank line.
fn render_message(message: &ExportMessage, body: &[u8]) -> Vec<u8> {
    let without_owned = crate::header_walk::strip_headers_where(body, |name| {
        OWNED_FLAG_HEADERS
            .iter()
            .any(|owned| name.eq_ignore_ascii_case(owned))
    });

    let sender = envelope_sender(&without_owned);
    let mut out = Vec::with_capacity(without_owned.len() + 128);
    out.extend_from_slice(b"From ");
    out.extend_from_slice(sender.as_bytes());
    out.push(b' ');
    out.extend_from_slice(asctime_utc(message.internal_date_epoch).as_bytes());
    out.push(b'\n');

    let mut stanza = flag_headers(&message.flags).into_bytes();
    stanza.extend_from_slice(&without_owned);
    out.extend_from_slice(&escape_from_lines(&to_lf(&stanza)));

    // A blank line terminates the message for the next `From ` scan. Only add
    // one if the body did not already end in a newline.
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    out.push(b'\n');
    out
}

/// The three flag headers, in a fixed order, CRLF-framed so they splice onto a
/// CRLF header section before the LF normalization runs.
fn flag_headers(flags: &[String]) -> String {
    let seen = has_flag(flags, "\\Seen");
    let answered = has_flag(flags, "\\Answered");
    let flagged = has_flag(flags, "\\Flagged");
    let deleted = has_flag(flags, "\\Deleted");
    let draft = has_flag(flags, "\\Draft");

    // `Status:` carries R(ead) and O(ld) — an exported message is always old.
    let mut status = String::new();
    if seen {
        status.push('R');
    }
    status.push('O');

    // `X-Status:` carries the rest, in the conventional ADFT order.
    let mut x_status = String::new();
    if answered {
        x_status.push('A');
    }
    if deleted {
        x_status.push('D');
    }
    if flagged {
        x_status.push('F');
    }
    if draft {
        x_status.push('T');
    }

    let mut moz = 0u16;
    if seen {
        moz |= MOZ_READ;
    }
    if answered {
        moz |= MOZ_REPLIED;
    }
    if flagged {
        moz |= MOZ_MARKED;
    }
    if deleted {
        moz |= MOZ_EXPUNGED;
    }

    format!("Status: {status}\r\nX-Status: {x_status}\r\nX-Mozilla-Status: {moz:04x}\r\n")
}

/// Envelope sender for the separator line: `Return-Path:` if the message kept
/// one, else the `From:` addr-spec, else the conventional placeholder.
fn envelope_sender(raw: &[u8]) -> String {
    let from_return_path = first_header_value(raw, b"Return-Path").and_then(|v| addr_spec(&v));
    if let Some(addr) = from_return_path.filter(|a| !a.is_empty()) {
        return addr;
    }
    first_header_value(raw, b"From")
        .and_then(|v| addr_spec(&v))
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| UNKNOWN_SENDER.to_string())
}

/// Pull the bare addr-spec out of a header value: the angle-addr if there is
/// one, otherwise the whole trimmed value.
fn addr_spec(value: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(value);
    let bare = match (text.rfind('<'), text.rfind('>')) {
        (Some(open), Some(close)) if close > open => &text[open + 1..close],
        _ => text.as_ref(),
    };
    let bare = bare.trim();
    // A separator line is whitespace-delimited, so a sender containing a space
    // would corrupt the next parse; such a value is not an address anyway.
    if bare.is_empty() || bare.contains(char::is_whitespace) {
        return None;
    }
    Some(bare.to_string())
}

/// CRLF → LF, leaving a bare LF or a bare CR alone.
fn to_lf(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\r' && raw.get(i + 1) == Some(&b'\n') {
            out.push(b'\n');
            i += 2;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    out
}

/// mboxrd: every line matching `>*From ` gains one leading `>`.
fn escape_from_lines(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 64);
    for line in split_keeping_newline(raw) {
        let quotes = line.iter().take_while(|&&b| b == b'>').count();
        if line[quotes..].starts_with(b"From ") {
            out.push(b'>');
        }
        out.extend_from_slice(line);
    }
    out
}

/// Split on LF, keeping each terminator with its line.
fn split_keeping_newline(raw: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, &b) in raw.iter().enumerate() {
        if b == b'\n' {
            lines.push(&raw[start..=i]);
            start = i + 1;
        }
    }
    if start < raw.len() {
        lines.push(&raw[start..]);
    }
    lines
}
