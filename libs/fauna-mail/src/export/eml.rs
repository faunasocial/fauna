//! The `EML zip` serializer — the lowest-common-denominator format.
//!
//! Authority: `docs/goal/behavior/mail-export.md` § Format choices → `EML zip`.
//! One pristine `.eml` per message in a flat directory, plus a
//! `manifest.json` that is the *only* place per-message metadata lives. A tool
//! that ignores the manifest still sees one `.eml` per message; a tool that
//! reads it (a future Fauna re-import) recovers mailbox, flag and UID fidelity.
//!
//! The `.eml` bytes are the raw RFC 5322 message, byte-for-byte — the same
//! sequence a fresh APPEND would carry. Unlike mbox, this format mutates
//! nothing: no separator line, no flag headers, no line-ending normalization.

use std::collections::HashSet;

use serde::Serialize;

use super::helpers::{first_header_value, message_digest16, rfc3339_utc};
use super::paths::{extraction_key, is_windows_device_name};
use super::{ExportEntry, ExportError, ExportMessage};

/// Longest filename stem taken from a `Message-ID`. Long enough that real
/// identifiers survive intact, short enough to stay well inside the path
/// limits of every extractor.
const MAX_STEM: usize = 96;

/// One `manifest.json` row. Field order is the serialized order, and it matches
/// § Format choices' list exactly.
#[derive(Debug, Serialize)]
struct ManifestRow {
    source_mailbox: String,
    imap_uid: u32,
    imap_uidvalidity: u32,
    imap_flags: Vec<String>,
    internal_date: String,
    message_id: String,
    filename: String,
}

#[derive(Default)]
pub(super) struct EmlState {
    rows: Vec<ManifestRow>,
    /// The [`extraction_key`] of every filename handed out — not the filename:
    /// a `Message-ID` is case-sensitive, and `<ABC@x>` beside `<abc@x>` would
    /// otherwise be two entries in the zip and one file once extracted, with
    /// two manifest rows pointing at it (§ Format choices → *Archive paths on
    /// the extracting filesystem*).
    used: HashSet<String>,
    earliest: Option<i64>,
}

impl EmlState {
    pub(super) fn push(
        &mut self,
        root: &str,
        message: &ExportMessage,
        body: &[u8],
    ) -> Vec<ExportEntry> {
        let message_id = first_header_value(body, b"Message-ID")
            .map(|v| String::from_utf8_lossy(&v).into_owned())
            .unwrap_or_default();
        let filename = self.mint_filename(&message_id, body);

        self.earliest = Some(match self.earliest {
            Some(e) => e.min(message.internal_date_epoch),
            None => message.internal_date_epoch,
        });
        self.rows.push(ManifestRow {
            source_mailbox: message.mailbox.clone(),
            imap_uid: message.uid,
            imap_uidvalidity: message.uid_validity,
            imap_flags: message.flags.clone(),
            internal_date: rfc3339_utc(message.internal_date_epoch),
            message_id,
            filename: filename.clone(),
        });

        vec![ExportEntry::file(
            format!("{root}/{filename}"),
            body.to_vec(),
            message.internal_date_epoch,
        )]
    }

    pub(super) fn finish(self, root: &str) -> Result<Vec<ExportEntry>, ExportError> {
        if self.rows.is_empty() {
            return Ok(Vec::new());
        }
        // Pretty-printed because a user opens this file: it is the one part of
        // the archive meant to be read rather than imported. `serde_json`
        // emits fields in declaration order, so this stays byte-deterministic.
        let json = serde_json::to_vec_pretty(&self.rows)
            .map_err(|e| ExportError::Manifest(e.to_string()))?;
        Ok(vec![ExportEntry::file(
            format!("{root}/manifest.json"),
            json,
            self.earliest.unwrap_or_default(),
        )])
    }

    /// `<stem>.eml`, the stem taken from the `Message-ID` when it yields a safe
    /// one and from the message digest otherwise, disambiguated on collision.
    fn mint_filename(&mut self, message_id: &str, body: &[u8]) -> String {
        let base = stem_from_message_id(message_id).unwrap_or_else(|| message_digest16(body));
        let mut candidate = format!("{base}.eml");
        let mut n = 2usize;
        while !self.used.insert(extraction_key(&candidate)) {
            candidate = format!("{base}-{n}.eml");
            n += 1;
        }
        candidate
    }
}

/// Fold a `Message-ID` into a filename stem, or decline.
///
/// Declines rather than mangles when the identifier carries nothing usable, so
/// the digest fallback keeps every such message distinguishable instead of
/// piling them onto one sanitized stem.
fn stem_from_message_id(message_id: &str) -> Option<String> {
    let inner = message_id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    let stem: String = inner
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' | '@' | '+' => c,
            _ => '_',
        })
        .take(MAX_STEM)
        .collect();
    let stem = stem.trim_matches(['.', '_']).to_string();
    // An identifier that survives as nothing but separators carries no
    // information; fall back to the digest instead. So does one Windows would
    // open as a device (`<CON>`, `<aux.17@host>` — reserved with any
    // extension): this fold was never decodable, so there is no name to keep.
    if stem.is_empty()
        || !stem.chars().any(|c| c.is_ascii_alphanumeric())
        || is_windows_device_name(&stem)
    {
        return None;
    }
    Some(stem)
}
