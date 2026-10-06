//! Error types. `ArchiveError` fails an operation on the whole archive
//! (unreadable source, not a zip, unsupported format); `EntityError` is a
//! per-record failure that lands in the skip log and never stops the
//! stream (`archive-import.md` § Parser contract rule 1).

use crate::model::Category;

/// A failure that stops one operation on the archive as a whole.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// The `ArchiveSource` could not be read.
    #[error("archive source: {0}")]
    Source(String),
    /// The bytes are not a readable zip archive.
    #[error("not a readable zip archive: {0}")]
    Zip(String),
    /// No parser recognised the archive's layout.
    #[error("no supported platform detected in the archive")]
    Undetected,
    /// The archive is a recognised platform but in a format this version
    /// does not parse (the HTML export) — refused at the Archive step with
    /// the message naming the format to request instead.
    #[error("{platform} export is in {format} format; request the JSON format")]
    UnsupportedFormat { platform: String, format: String },
    /// A zip member could not be read or was larger than the parser's cap.
    #[error("member {member}: {reason}")]
    Member { member: String, reason: String },
    /// A model value could not be encoded.
    #[error("encode: {0}")]
    Encode(String),
}

/// One bad record inside a category file. Carries enough to render a skip
/// log line: which category, which zip member, the record's position in
/// that member, and why.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EntityError {
    pub category: Category,
    pub member: String,
    /// The record's 0-based index within `member`; 0 for a member-level
    /// failure (unreadable or unparsable member).
    pub position: u64,
    pub reason: String,
}

impl std::fmt::Display for EntityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} record {} in {}: {}",
            self.category.token(),
            self.position,
            self.member,
            self.reason
        )
    }
}

impl std::error::Error for EntityError {}
