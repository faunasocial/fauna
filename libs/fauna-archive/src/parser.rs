//! The parser contract (`archive-import.md` § Parser contract) and the
//! registry of platform parsers. One model, N parsers: platform knowledge
//! stops at this trait's implementations.

use crate::error::{ArchiveError, EntityError};
use crate::model::{ArchiveSummary, Category, Entity, Platform};
use crate::reader::{ArchiveReader, ZipDirectory};

/// The export format a platform produced. Only JSON is parsed in version
/// one; HTML is detected so the Archive step can name the format to request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Json,
    Html,
}

impl ExportFormat {
    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Json => "JSON",
            ExportFormat::Html => "HTML",
        }
    }
}

/// What `detect` found from the central directory alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub platform: Platform,
    pub format: ExportFormat,
}

/// One platform's parser. Rules (the goal doc's, restated as contract):
/// never fail the whole import on one bad record; ignore unknown files;
/// count before writing; deterministic IDs; stream members by offset.
pub trait ArchiveParser {
    fn platform(&self) -> Platform;

    /// Recognises the platform from member paths only (no member is read).
    fn detect(&self, dir: &ZipDirectory) -> Option<Detected>;

    /// Builds the [`ArchiveSummary`]: reads the profile and the category
    /// JSON members to count records and bound the date range; media is
    /// sized from the directory, never read.
    fn index(&self, reader: &mut ArchiveReader<'_>) -> Result<ArchiveSummary, ArchiveError>;

    /// Yields every record of one category, one member at a time. A bad
    /// record is an `Err(EntityError)` item; the stream continues.
    fn stream<'r, 's>(
        &self,
        reader: &'r mut ArchiveReader<'s>,
        category: Category,
    ) -> Box<dyn Iterator<Item = Result<Entity, EntityError>> + 'r>;
}

/// Every parser this build carries, in detection order.
pub fn parsers() -> Vec<Box<dyn ArchiveParser>> {
    vec![Box::new(crate::facebook::FacebookParser)]
}

/// The first parser that recognises the directory, with what it detected.
pub fn detect(dir: &ZipDirectory) -> Option<(Box<dyn ArchiveParser>, Detected)> {
    parsers().into_iter().find_map(|parser| {
        let detected = parser.detect(dir)?;
        Some((parser, detected))
    })
}
