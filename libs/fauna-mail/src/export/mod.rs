//! Mailbox-export serializers — the three MUA-portable output formats the
//! `mail-export` wizard offers, plus the deterministic archive container.
//!
//! Authority: `docs/goal/behavior/mail-export.md` § Format choices (the mbox /
//! Maildir++ / EML-zip contracts), § Container shape (zip-inside-zstd, and the
//! determinism contract every serializer here is written against),
//! § Compression wrapper, § Compile-time decisions (nothing in this module is a
//! knob) and § Export pipeline (conversion runs on the **client**, so this
//! module must stay WASM-clean — the web app runs the identical loop).
//!
//! # Where this sits
//!
//! The nest holds ciphertext only and never sees plaintext mail during an
//! export (§ Export pipeline — nest-side conversion is the *recorded rejected
//! alternative*). So the caller here is always the user's own client: it
//! unwraps a fetched chunk, hands the plaintext messages to an
//! [`ExportSerializer`], and the entries that come back are packed by
//! [`archive`] into the one zip / one zstd stream that becomes the blob.
//!
//! # The determinism contract
//!
//! § Goal's self-verifiability bar is *byte-identical output for the same
//! input*, which is stronger than it sounds: the live Maildir convention mints
//! filenames from pid + wall-clock + hostname, and a zip's per-entry timestamp
//! defaults to "now". Every such input is designed out here, not merely
//! avoided:
//!
//! - messages arrive in one total order — mailbox name ascending by raw bytes,
//!   then IMAP UID ascending — and [`ExportSerializer::push`] **rejects** an
//!   out-of-order message rather than silently producing a different archive
//!   (the chunk-relay's mailbox cursor already walks in this order; the check
//!   is what keeps it true). The order dropped INTERNALDATE from between the
//!   two on 2026-09-21: the down-leg's only cursor is `after_uid`, and an
//!   INTERNALDATE-first order is not streamable against it — an *imported*
//!   message carries its source's old date on a freshly-minted high UID, so a
//!   UID walk of a mailbox that has ever been imported into hands the
//!   serializer a descending date and the run dies. Restoring the old order
//!   would mean buffering a whole mailbox's bodies to sort them, which the
//!   10 GiB ceiling forbids; UID is already unique, stable and total within a
//!   mailbox, so the determinism bar is met either way (§ Container shape);
//! - every entry's mtime is the message's own INTERNALDATE, never the clock;
//! - the Maildir++ `<unique>` is a digest of the message bytes and `<host>` is
//!   a fixed literal (§ Format choices → `Maildir++`);
//! - no random source, no hostname, no process id, no locale is read anywhere
//!   in this module.
//!
//! # Why `push` streams instead of collecting
//!
//! An export is capped at 10 GiB (§ Quota composition), so nothing here may
//! buffer the whole run. Maildir++ and EML zip emit one entry per message
//! immediately. mbox concatenates a whole mailbox into one file, so it buffers
//! exactly one mailbox and flushes it the moment the ordered input crosses into
//! the next one — the peak is one mailbox, not one export.

pub mod archive;
#[cfg(feature = "mail-export-client")]
pub mod client;
mod eml;
mod maildir;
mod mbox;
mod paths;
pub mod seal;
pub mod stream;

pub use archive::{ZSTD_LEVEL, build_blob, build_zip};
pub use seal::{
    EXPORT_BLOB_PREAMBLE, EXPORT_SESSION_KEY_BYTES, ExportBlobOpener, ExportBlobSealer,
    ExportSealError, MAX_EXPORT_FRAME_BYTES, open_export_blob,
};
pub use stream::{EXPORT_CHUNK_BYTES, ExportArchiveStream};

pub use paths::encode_path_component;
use serde::{Deserialize, Serialize};

/// One plaintext message to serialize, as the client holds it after unwrapping
/// a fetched chunk.
///
/// Field roles mirror `fauna_protocol::bridge_routing::ImportMessageItem` —
/// the import twin's per-message wire item — because § Goal requires every
/// export shape to be symmetric with the import it reverses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportMessage {
    /// Source mailbox name, as the user's mail area names it (`INBOX`, `Sent`,
    /// `Work/Reports`). mbox and Maildir++ put it into an archive path through
    /// the one injective encoding, handed out per run by
    /// `paths::ComponentNamer` so two mailboxes stay apart once extracted too
    /// (it folds the `/` hierarchy delimiter to `.`); the EML manifest records
    /// it raw.
    pub mailbox: String,
    /// IMAP flag atoms carried from the store, e.g. `\Seen`. Compared
    /// case-insensitively (system flags are case-insensitive); unknown flags
    /// are preserved in the EML manifest and dropped by the two formats whose
    /// encodings have no room for them.
    pub flags: Vec<String>,
    /// The raw RFC 5322 bytes — exactly what a fresh APPEND would carry.
    pub body: Vec<u8>,
    /// INTERNALDATE as epoch seconds. Every timestamp this module writes comes
    /// from here.
    pub internal_date_epoch: i64,
    /// Source IMAP UID + UIDVALIDITY (recorded in the EML manifest; the UID is
    /// also the within-mailbox tiebreak of the total order).
    pub uid: u32,
    pub uid_validity: u32,
}

/// The export file format — the wizard's step-1 choice.
///
/// Three arms, no more, no less in v1 (§ Architectural rules).
///
/// **The one definition** (collapsed 2026-09-21, with the client drive loop).
/// `fauna_client_mail_settings::export::ExportFormat` is a plain `pub use` of
/// this type, not a parallel declaration: one concept gets one definition
/// (priority #1/#2), and the layering puts it here — that crate depends on
/// this one, never the reverse.
///
/// The serde + UniFFI derives therefore live on **this** declaration rather
/// than on the re-export, because a `pub use` carries no derives of its own:
/// without them here the FFI face would compile with the type silently
/// missing, the same trap `fauna-mail`'s own `uniffi` feature comment records
/// for the mail-auth verdict types. A consumer that wants the FFI face enables
/// this crate's `uniffi` feature; `fauna-client-mail-settings` forwards it.
///
/// **Never reintroduce a client-side twin with a `From` between them** — a
/// conversion is exactly what would let the divergence grow back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ExportFormat {
    /// RFC 4155 — one text file per mailbox. Broadest MUA support.
    Mbox,
    /// The Courier Maildir++ extension — one file per message in a
    /// `cur/`/`new/`/`tmp/` tree per mailbox.
    MaildirPlus,
    /// One `.eml` per message in a flat directory plus a metadata manifest.
    EmlZip,
}

impl ExportFormat {
    /// The wire token `start_export_session` carries and every frame's AAD
    /// binds (§ Blob shape on disk) — `mbox` / `maildir` / `eml-zip`.
    ///
    /// ⚠ The nest keeps its own closed copy of these three strings
    /// (`bins/fauna-nest/src/bridge_export_handlers.rs::EXPORT_FORMATS`) and
    /// deliberately cannot share this one: reaching it would mean the nest
    /// enabling the `mail-export` feature, i.e. linking the serializers into
    /// the nest binary — the shape § Export pipeline records as the REJECTED
    /// alternative. `the_three_wire_format_tokens_are_the_nests_closed_list`
    /// pins the strings so a rename here cannot drift away from that list
    /// silently.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            ExportFormat::Mbox => "mbox",
            ExportFormat::MaildirPlus => "maildir",
            ExportFormat::EmlZip => "eml-zip",
        }
    }

    /// Parse a wire token back to the format. `None` for anything else — a
    /// session row carrying an unknown format is not openable, so guessing
    /// would only move the failure to the AEAD.
    #[must_use]
    pub fn from_wire_name(token: &str) -> Option<Self> {
        match token {
            "mbox" => Some(ExportFormat::Mbox),
            "maildir" => Some(ExportFormat::MaildirPlus),
            "eml-zip" => Some(ExportFormat::EmlZip),
            _ => None,
        }
    }

    /// The archive's single root directory, `<actor>-<format>/`
    /// (§ Format choices pins one root per format).
    fn root(self, actor_handle: &str) -> String {
        let handle = encode_path_component(actor_handle);
        match self {
            ExportFormat::Mbox => format!("{handle}-mbox"),
            ExportFormat::MaildirPlus => format!("{handle}-maildir"),
            ExportFormat::EmlZip => format!("{handle}-eml"),
        }
    }
}

/// Scope choices from the wizard's step 2 that reach the serializer.
///
/// Mailbox selection and the date range are applied *before* this module — they
/// decide which messages the chunk-relay fetches at all — so the only scope
/// field that changes serialized bytes is the header strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOptions {
    /// The user's handle, which names the archive's root directory.
    pub actor_handle: String,
    /// Step 2's header-strip toggle: drop `TRANSIT_STRIP_HEADERS` so the
    /// export carries no transit-path metadata (§ UX shape step 2 owns the
    /// list). Default off — full forensic fidelity.
    pub strip_headers: bool,
}

impl ExportOptions {
    /// The ratified default: strip off.
    pub fn new(actor_handle: impl Into<String>) -> Self {
        Self {
            actor_handle: actor_handle.into(),
            strip_headers: false,
        }
    }
}

/// One file (or empty directory) in the export archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportEntry {
    /// Archive-relative path, always beginning with the format's root
    /// directory and always `/`-separated.
    pub path: String,
    /// The entry's bytes; empty for a directory.
    pub bytes: Vec<u8>,
    /// Stored modification time, always a message INTERNALDATE (§ Container
    /// shape — never the wall clock).
    pub mtime_epoch: i64,
    /// A `new/` or `tmp/` placeholder rather than a file.
    pub is_dir: bool,
}

impl ExportEntry {
    pub(crate) fn file(path: String, bytes: Vec<u8>, mtime_epoch: i64) -> Self {
        Self {
            path,
            bytes,
            mtime_epoch,
            is_dir: false,
        }
    }

    pub(crate) fn dir(path: String, mtime_epoch: i64) -> Self {
        Self {
            path,
            bytes: Vec::new(),
            mtime_epoch,
            is_dir: true,
        }
    }
}

/// What can go wrong converting a message or packing the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportError {
    /// A message arrived out of the total order the determinism contract
    /// requires. Carries both keys so a caller can see which cursor slipped.
    OutOfOrder { previous: String, offending: String },
    /// A message with no body — the store never holds one, so this is a
    /// caller bug rather than a per-message skip.
    EmptyBody { mailbox: String, uid: u32 },
    /// The header-strip toggle removed every byte of a message — one made of
    /// nothing but transit headers. Refused rather than written as an empty
    /// entry into an archive the export would then report complete.
    EmptyAfterStrip { mailbox: String, uid: u32 },
    /// The zip writer refused an entry.
    Archive(String),
    /// The zstd encoder refused the stream.
    Compress(String),
    /// The EML manifest failed to serialize.
    Manifest(String),
    /// No archive path could be found for this mailbox that stays clear of
    /// every other once extracted (§ Format choices → *Archive paths on the
    /// extracting filesystem*). Unreachable short of a digest collision between
    /// two over-long names; refused rather than letting one mailbox be
    /// extracted over another.
    PathCollision { name: String },
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportError::OutOfOrder {
                previous,
                offending,
            } => write!(
                f,
                "export messages must arrive in (mailbox, uid) order: \
                 {offending} followed {previous}"
            ),
            ExportError::EmptyBody { mailbox, uid } => {
                write!(f, "message {mailbox}:{uid} has an empty body")
            }
            ExportError::EmptyAfterStrip { mailbox, uid } => write!(
                f,
                "message {mailbox}:{uid} is empty once its transit headers are stripped"
            ),
            ExportError::Archive(e) => write!(f, "export archive error: {e}"),
            ExportError::Compress(e) => write!(f, "export compression error: {e}"),
            ExportError::Manifest(e) => write!(f, "export manifest error: {e}"),
            ExportError::PathCollision { name } => write!(
                f,
                "no archive path keeps mailbox {name:?} apart from the others once extracted"
            ),
        }
    }
}

impl std::error::Error for ExportError {}

/// Per-format accumulated state.
enum FormatState {
    Mbox(mbox::MboxState),
    Maildir(maildir::MaildirState),
    Eml(eml::EmlState),
}

/// Converts an ordered message stream into archive entries.
///
/// See the module docs for the ordering contract and the buffering bound.
pub struct ExportSerializer {
    options: ExportOptions,
    root: String,
    last_key: Option<(String, u32)>,
    state: FormatState,
}

impl ExportSerializer {
    /// Open a serializer for one export session.
    pub fn new(format: ExportFormat, options: ExportOptions) -> Self {
        let root = format.root(&options.actor_handle);
        let state = match format {
            ExportFormat::Mbox => FormatState::Mbox(mbox::MboxState::default()),
            ExportFormat::MaildirPlus => FormatState::Maildir(maildir::MaildirState::default()),
            ExportFormat::EmlZip => FormatState::Eml(eml::EmlState::default()),
        };
        Self {
            options,
            root,
            last_key: None,
            state,
        }
    }

    /// Feed the next message, returning whatever entries it completed.
    ///
    /// Maildir++ and EML zip return the message's own entry; mbox returns the
    /// *previous* mailbox's file whenever this message opens a new one, and
    /// otherwise nothing.
    pub fn push(&mut self, message: &ExportMessage) -> Result<Vec<ExportEntry>, ExportError> {
        if message.body.is_empty() {
            return Err(ExportError::EmptyBody {
                mailbox: message.mailbox.clone(),
                uid: message.uid,
            });
        }
        self.check_order(message)?;

        let body = if self.options.strip_headers {
            strip_transit_headers(&message.body)
        } else {
            message.body.clone()
        };
        if body.is_empty() {
            return Err(ExportError::EmptyAfterStrip {
                mailbox: message.mailbox.clone(),
                uid: message.uid,
            });
        }

        match &mut self.state {
            FormatState::Mbox(s) => s.push(&self.root, message, &body),
            FormatState::Maildir(s) => s.push(&self.root, message, &body),
            FormatState::Eml(s) => Ok(s.push(&self.root, message, &body)),
        }
    }

    /// Close the stream, returning the entries that only exist once every
    /// message is known: mbox's final mailbox file, Maildir++'s `subscriptions`
    /// index, EML zip's `manifest.json`.
    pub fn finish(self) -> Result<Vec<ExportEntry>, ExportError> {
        match self.state {
            FormatState::Mbox(s) => Ok(s.finish(&self.root)),
            FormatState::Maildir(s) => Ok(s.finish(&self.root)),
            FormatState::Eml(s) => s.finish(&self.root),
        }
    }

    /// Enforce the total order the determinism contract rests on:
    /// `(mailbox name raw bytes, IMAP UID)`, both ascending.
    ///
    /// INTERNALDATE is deliberately **not** part of the key — see the module
    /// docs. It cannot be, because the only cursor the down-leg offers is
    /// `after_uid`, and imported mail puts an old date on a high UID.
    fn check_order(&mut self, message: &ExportMessage) -> Result<(), ExportError> {
        let key = (message.mailbox.clone(), message.uid);
        if let Some(previous) = &self.last_key {
            // Mailbox names compare by raw bytes, matching § Container shape.
            let ordered = (previous.0.as_bytes(), previous.1) <= (key.0.as_bytes(), key.1);
            if !ordered {
                return Err(ExportError::OutOfOrder {
                    previous: describe_key(previous),
                    offending: describe_key(&key),
                });
            }
        }
        self.last_key = Some(key);
        Ok(())
    }
}

/// Convert a whole ordered message slice in one call — the shape every test and
/// every golden uses, and the shape a caller with the run already in memory
/// wants. The streaming pipeline drives [`ExportSerializer`] directly.
pub fn serialize_all(
    format: ExportFormat,
    options: ExportOptions,
    messages: &[ExportMessage],
) -> Result<Vec<ExportEntry>, ExportError> {
    let mut serializer = ExportSerializer::new(format, options);
    let mut entries = Vec::new();
    for message in messages {
        entries.extend(serializer.push(message)?);
    }
    entries.extend(serializer.finish()?);
    Ok(entries)
}

fn describe_key(key: &(String, u32)) -> String {
    format!("{}:uid {}", key.0, key.1)
}

/// The header-strip toggle's strip set: the exact field names (matched
/// case-insensitively, whole-name) removed when `strip_headers` is on.
///
/// **The authority is `docs/goal/behavior/mail-export.md` § UX shape step 2's
/// "Strip set" line, not this constant** — a header is added there first, and
/// `the_strip_set_constant_is_the_owner_docs_list` fails until both agree. The
/// same paragraph records what is deliberately *kept* (`DKIM-Signature`,
/// `Return-Path`) and why; `Return-Path` in particular feeds the mbox `From `
/// separator, which is derived *after* this strip runs.
///
/// Deliberately separate from
/// [`crate::outbound::received_strip::strip_received_headers`]: that one is the
/// pre-DKIM relay strip, drops `Received` alone, and pins `Received-SPF`
/// surviving. Same walk, different purpose — never merge the predicates.
pub(crate) const TRANSIT_STRIP_HEADERS: &[&str] = &[
    "Received",
    "X-Received",
    "Received-SPF",
    "Authentication-Results",
    "ARC-Seal",
    "ARC-Message-Signature",
    "ARC-Authentication-Results",
    "X-Originating-IP",
    "X-Originating-Client-IP",
    "X-Sender-IP",
    "X-Forwarded-For",
    "X-Real-IP",
    "X-Client-IP",
    "X-Forefront-Antispam-Report",
    "X-Forefront-Antispam-Report-Untrusted",
    "X-ClientProxiedBy",
    "X-MS-Exchange-Organization-OriginalClientIPAddress",
    "X-MS-Exchange-Organization-OriginalServerIPAddress",
    "X-MS-Exchange-Organization-ConnectingIP",
    "X-MS-Exchange-Organization-AuthSource",
    "X-MS-Exchange-CrossTenant-AuthSource",
    "X-MS-Exchange-Transport-CrossTenantHeadersStamped",
    "X-SES-Outgoing",
    "X-Mailgun-Sending-Ip",
    "X-Ovh-Remote",
    "X-Barracuda-Connect",
    "X-Barracuda-Apparent-Source-IP",
    "X-Scanned-By",
    "X-Greylist",
    "X-Virus-Scanned",
    "X-Spam-Checker-Version",
    "X-AntiAbuse",
    "X-Authenticated-Sender",
    "X-Get-Message-Sender-Via",
];

/// Drop the transit headers (step 2's header-strip toggle), reusing the one
/// continuation-aware header walk the relay strip and the `List-*` stamp share
/// — priority #2: one walk, no drift.
fn strip_transit_headers(raw: &[u8]) -> Vec<u8> {
    crate::header_walk::strip_headers_where(raw, |name| {
        TRANSIT_STRIP_HEADERS
            .iter()
            .any(|strip| name.eq_ignore_ascii_case(strip.as_bytes()))
    })
}

/// Helpers shared by the three serializers. Kept in one place so a rule the
/// determinism contract names is stated once.
mod helpers {
    /// IMAP system flags are case-insensitive (RFC 3501 §2.3.2).
    pub(super) fn has_flag(flags: &[String], want: &str) -> bool {
        flags.iter().any(|f| f.eq_ignore_ascii_case(want))
    }

    /// The first value of `name` in the header section, unfolded to one line.
    ///
    /// Deliberately lexical and infallible: both callers want a *display*
    /// string (an mbox separator line, a manifest row), never a routing or
    /// trust decision, so this must not disagree with a full parser in any way
    /// that matters, and must not fail on a malformed message.
    pub(super) fn first_header_value(raw: &[u8], name: &[u8]) -> Option<Vec<u8>> {
        let body_offset = crate::header_walk::find_body_offset(raw).unwrap_or(raw.len());
        let mut cursor = 0usize;
        while cursor < body_offset {
            let (end, field) = crate::header_walk::parse_header(&raw[cursor..body_offset]);
            if end == 0 {
                break;
            }
            if field.eq_ignore_ascii_case(name) {
                let header = &raw[cursor..cursor + end];
                let colon = header.iter().position(|&b| b == b':')?;
                let mut value = Vec::with_capacity(header.len() - colon);
                // Unfold: each continuation's line break becomes one space.
                let mut chunk = &header[colon + 1..];
                while let Some(pos) = chunk.iter().position(|&b| b == b'\n') {
                    value.extend_from_slice(trim_ascii(&chunk[..pos]));
                    value.push(b' ');
                    chunk = &chunk[pos + 1..];
                }
                value.extend_from_slice(trim_ascii(chunk));
                return Some(trim_ascii(&value).to_vec());
            }
            cursor += end;
        }
        None
    }

    pub(super) fn trim_ascii(bytes: &[u8]) -> &[u8] {
        let start = bytes
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .unwrap_or(bytes.len());
        let end = bytes
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(start, |p| p + 1);
        &bytes[start..end]
    }

    /// 16 lowercase hex characters of BLAKE3 over the message bytes — the
    /// Maildir++ `<unique>` component and the EML filename fallback
    /// (§ Format choices → `Maildir++`, pinned 2026-09-20).
    pub(super) fn message_digest16(body: &[u8]) -> String {
        blake3::hash(body).to_hex()[..16].to_string()
    }

    /// `YYYY-MM-DDTHH:MM:SSZ` — the EML manifest's `internal_date`.
    pub(super) fn rfc3339_utc(epoch: i64) -> String {
        let (days, h, m, s) = fauna_core::caltime::epoch_secs_to_days_and_time(epoch);
        let (y, mo, d) = fauna_core::caltime::civil_from_days(days);
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
    }

    /// The C-locale `ctime` form RFC 4155 puts on an mbox `From ` line:
    /// `Wed Jul 17 09:44:25 1996`, day space-padded, always UTC.
    ///
    /// The weekday and month names come from `fauna_core::imf_date`, which owns
    /// the two wire tables for the whole workspace (a merge gate,
    /// `check_date_wire_grammar.py`, refuses a re-derivation anywhere else).
    /// Delegating also removes a trap: `weekday_abbrev` takes epoch-days and
    /// does its own Sunday-based arithmetic, so no caller can repeat the
    /// mistake of indexing a Sunday-first table with
    /// `caltime::day_of_week`, which is **0 = Monday**.
    pub(super) fn asctime_utc(epoch: i64) -> String {
        let (days, h, mi, s) = fauna_core::caltime::epoch_secs_to_days_and_time(epoch);
        let (y, mo, d) = fauna_core::caltime::civil_from_days(days);
        let weekday = fauna_core::imf_date::weekday_abbrev(days);
        let month = fauna_core::imf_date::month_abbrev(mo).unwrap_or("Jan");
        format!("{weekday} {month} {d:2} {h:02}:{mi:02}:{s:02} {y:04}")
    }
}

#[cfg(test)]
mod tests;
