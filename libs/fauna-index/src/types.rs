//! Domain types for the content index.
//!
//! All types are platform-agnostic plain data; no Tantivy types leak through
//! the public API.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One of the eight content kinds tracked per the encryption-at-rest target
/// doc. The variant order is part of the wire format and must not be reordered.
///
/// `Ord` is derived, so it follows that same declaration order — which is what
/// gives a multi-kind builder a deterministic publish order for the several
/// segments one flush can produce. It is an ordering of convenience, not of
/// meaning: no kind ranks above another.
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `ladder` ground: a new variant raises the format
/// version the reader checks before it decodes, so there is no unknown arm). A
/// new variant is an edit to `tools/check-additive-evolution/enum_ledger.txt`,
/// made in the same change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Mail,
    Calendar,
    Conversation,
    Post,
    File,
    Contact,
    Draft,
    Media,
}

impl ContentKind {
    pub const ALL: &'static [ContentKind] = &[
        ContentKind::Mail,
        ContentKind::Calendar,
        ContentKind::Conversation,
        ContentKind::Post,
        ContentKind::File,
        ContentKind::Contact,
        ContentKind::Draft,
        ContentKind::Media,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ContentKind::Mail => "mail",
            ContentKind::Calendar => "calendar",
            ContentKind::Conversation => "conversation",
            ContentKind::Post => "post",
            ContentKind::File => "file",
            ContentKind::Contact => "contact",
            ContentKind::Draft => "draft",
            ContentKind::Media => "media",
        }
    }

    /// Which key class wraps this kind's segments — the S0-ratified per-kind
    /// key split (`content-index.md` § Encryption posture). Exhaustive match on
    /// purpose: a new kind must consciously pick its class.
    pub fn class(self) -> KindClass {
        match self {
            ContentKind::Mail | ContentKind::Calendar => KindClass::MailCal,
            ContentKind::Conversation
            | ContentKind::Post
            | ContentKind::File
            | ContentKind::Contact
            | ContentKind::Draft
            | ContentKind::Media => KindClass::Master,
        }
    }
}

/// The two key classes of the per-kind split: `MailCal` kinds (mail, calendar)
/// wrap under the MSEK-derived [`crate::IndexSegmentKey`] and live in
/// `manifest-mailcal.idx`; `Master` kinds wrap under the [`crate::IndexMasterKey`]
/// and live in `manifest.idx`. One manifest file per class; a kind never appears
/// in the wrong-class manifest ([`IndexError::WrongKindClass`]).
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `ladder` ground: a new variant raises the format
/// version the reader checks before it decodes, so there is no unknown arm). A
/// new variant is an edit to `tools/check-additive-evolution/enum_ledger.txt`,
/// made in the same change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum KindClass {
    MailCal,
    Master,
}

impl KindClass {
    /// Every [`ContentKind`] this class wraps — the inverse of
    /// [`ContentKind::class`], and the set a resume walks when it opens a
    /// class's manifest and has to reach all of that manifest's live segments.
    ///
    /// Derived from `ContentKind::class` rather than hand-listed, so it cannot
    /// drift from it: a new kind picks its class in the one exhaustive match
    /// above and appears here automatically.
    pub fn kinds(self) -> Vec<ContentKind> {
        ContentKind::ALL
            .iter()
            .copied()
            .filter(|k| k.class() == self)
            .collect()
    }
}

/// Opaque content identifier. Owned by the producer of the content; the index
/// stores it as bytes and returns it verbatim on query hits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentId(pub Vec<u8>);

impl From<&[u8]> for ContentId {
    fn from(b: &[u8]) -> Self {
        ContentId(b.to_vec())
    }
}

impl From<Vec<u8>> for ContentId {
    fn from(v: Vec<u8>) -> Self {
        ContentId(v)
    }
}

/// Which searchable surface of a doc a field represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    /// Short, high-signal field: subject, title, contact name, file name.
    Title,
    /// Free-form long text: mail body, post body, file contents, transcript.
    Body,
    /// Pre-tokenized tag tokens (intended for classifier output, e.g.
    /// `tag-dog tag-beach` from a moderate-scope image classifier per spec
    /// D7). Currently tokenized identically to body text — the
    /// `FieldKind::PreTokenizedTags` enum variant exists to mark caller
    /// intent and to give the schema room to grow a non-tokenizing
    /// pipeline later (Plan 7). Until then, choose tag formats that
    /// survive UAX#29 word segmentation (avoid `:` and other punctuation
    /// that splits the token).
    PreTokenizedTags,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct IndexedField {
    pub kind: FieldKind,
    /// For Title and Body: raw text. For PreTokenizedTags: a vec of
    /// already-canonical tokens encoded as a single space-separated string.
    pub text: String,
}

/// One document being added to the index.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct IndexedDoc {
    pub kind: ContentKind,
    pub content_id: ContentId,
    /// Unix nanos. Used for time-range filtering and as a tie-breaker on rank.
    pub timestamp_ns: i64,
    /// Optional sender / author. Only populated for kinds that have one.
    pub sender_actor_id: Option<Vec<u8>>,
    /// Optional *secondary* identity — a second producer-owned spelling of the
    /// same document, stored (never searched) so a caller that knows a doc only
    /// by this spelling can decide coverage without touching the content id.
    ///
    /// Exactly one consumer today (`content-index.md` § Where the index is
    /// built → the 2026-08-10 carrier ruling): mail docs carry the raw 32-byte
    /// nest message id here, which is the only identity the MDA's IMAP `SEARCH`
    /// path holds, while `content_id` stays the ratified RFC `Message-ID`.
    /// It is a lookup carrier, not a substitute key — `add_doc`'s upsert and
    /// the stage-time guards key on `content_id` alone.
    #[serde(default)]
    pub secondary_id: Option<Vec<u8>>,
    pub fields: Vec<IndexedField>,
}

/// One live document's identities — the content id every guard and upsert keys
/// on, plus the optional secondary spelling ([`IndexedDoc::secondary_id`]).
///
/// Returned by `Index::doc_identities`, the coverage walk for callers that key
/// candidates by the secondary spelling (the MDA's `SEARCH` path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocIdentity {
    pub content_id: ContentId,
    pub secondary_id: Option<Vec<u8>>,
}

/// One result from `Index::query`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct QueryHit {
    pub kind: ContentKind,
    pub content_id: ContentId,
    pub timestamp_ns: i64,
    pub sender_actor_id: Option<Vec<u8>>,
    /// BM25 score from Tantivy; higher is more relevant.
    pub score: f32,
}

/// Half-open `[start_ns, end_ns)` filter on `IndexedDoc::timestamp_ns`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TimeRange {
    pub start_ns: i64,
    pub end_ns: i64,
}

#[derive(Debug, Error)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
#[cfg_attr(feature = "uniffi", uniffi(flat_error))]
pub enum IndexError {
    #[error("tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("query parse error: {0}")]
    QueryParse(String),
    #[error("invalid query: {0}")]
    InvalidQuery(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("schema mismatch: {0}")]
    SchemaMismatch(String),
    #[error("crypto error: {0}")]
    Crypto(String),
    /// The blob was written by a **newer** build that raised the reader floor past this
    /// one: it is intact, and this binary must not touch it.
    ///
    /// Deliberately distinct from [`IndexError::Crypto`] and
    /// [`IndexError::SchemaMismatch`]. Collapsing "written by a newer build — update the
    /// app, the data is fine" into a generic "corrupt" is what licenses a caller to
    /// *heal* the blob by overwriting it, and that conflation is exactly what produced
    /// the account-index cliff (`version-compatibility.md` § 5 item 9).
    /// A caller matching this variant can say the true thing and leave the bytes alone.
    #[error(
        "incompatible index format: written by a newer build (format_version={file_v}, \
         min_reader_version={file_min}) that requires a reader at format_version >= {file_min}, \
         but this build is at format_version {bin_v} — update the app; the index is intact and \
         untouched"
    )]
    Incompatible { file_v: u8, file_min: u8, bin_v: u8 },
    /// A kind was offered to the wrong-class manifest (a mail/calendar kind to
    /// `manifest.idx`, or a master-class kind to `manifest-mailcal.idx`). The
    /// split is refused at the API, never trusted to convention
    /// (`content-index.md` § Encryption posture).
    #[error("kind {kind} belongs to the {kind_class} manifest, not the {manifest_class} one")]
    WrongKindClass {
        kind: String,
        kind_class: String,
        manifest_class: String,
    },
}

#[cfg(feature = "uniffi")]
uniffi::custom_type!(ContentId, Vec<u8>, {
    remote,
    lower: |id| id.0,
    try_lift: |bytes| Ok(ContentId(bytes)),
});
