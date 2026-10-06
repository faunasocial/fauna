//! The Facebook JSON-export parser (`archive-import.md` § Parser contract).
//! `layout` locates members, `json` decodes them, one module per category
//! turns records into model entities; this file is the `ArchiveParser` glue
//! and the per-member streaming iterator the category modules share.

pub mod json;
pub mod layout;
pub mod posts;
pub mod profile;
pub mod social;
pub mod threads;

use std::collections::VecDeque;

use serde_json::Value;

use crate::error::{ArchiveError, EntityError};
use crate::model::{
    ArchiveSummary, AudienceCounts, Category, CategoryCounts, Entity, ExternalActorRef,
    PARSER_VERSION, Platform,
};
use crate::parser::{ArchiveParser, Detected, ExportFormat};
use crate::reader::ArchiveReader;

use self::layout::{Layout, locate};

pub struct FacebookParser;

/// Yields the records of one member at a time: pops the next member, reads
/// it, hands the bytes to `parse`, drains what it produced, then moves on.
/// A member that cannot be read becomes one `EntityError` at position 0.
pub(crate) struct MemberStream<'r, 's, F> {
    reader: &'r mut ArchiveReader<'s>,
    category: Category,
    members: VecDeque<String>,
    pending: VecDeque<Result<Entity, EntityError>>,
    parse: F,
}

impl<'r, 's, F> MemberStream<'r, 's, F>
where
    F: FnMut(&mut ArchiveReader<'s>, &str, Vec<u8>) -> Vec<Result<Entity, EntityError>>,
{
    pub(crate) fn new(
        reader: &'r mut ArchiveReader<'s>,
        category: Category,
        members: Vec<String>,
        parse: F,
    ) -> Self {
        MemberStream {
            reader,
            category,
            members: members.into(),
            pending: VecDeque::new(),
            parse,
        }
    }
}

impl<'s, F> Iterator for MemberStream<'_, 's, F>
where
    F: FnMut(&mut ArchiveReader<'s>, &str, Vec<u8>) -> Vec<Result<Entity, EntityError>>,
{
    type Item = Result<Entity, EntityError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Some(item);
            }
            let member = self.members.pop_front()?;
            match self.reader.read_member(&member) {
                Ok(bytes) => {
                    let items = (self.parse)(self.reader, &member, bytes);
                    self.pending.extend(items);
                }
                Err(e) => self.pending.push_back(Err(EntityError {
                    category: self.category.clone(),
                    member,
                    position: 0,
                    reason: e.to_string(),
                })),
            }
        }
    }
}

impl FacebookParser {
    /// The owner's actor ref from the profile member; an export without a
    /// profile gets a nameless owner rather than a refusal.
    fn owner(&self, reader: &mut ArchiveReader<'_>, layout: &Layout) -> ExternalActorRef {
        let Some(member) = &layout.profile else {
            return ExternalActorRef::new(Platform::Facebook, None, "");
        };
        reader
            .read_member(member)
            .ok()
            .and_then(|bytes| profile::parse_profile(&bytes, member).ok())
            .map(|p| p.actor)
            .unwrap_or_else(|| ExternalActorRef::new(Platform::Facebook, None, ""))
    }

    /// Counts one category member into the summary (records + date range).
    /// Threads are counted from the layout in `index`; their files are read
    /// here as the `Messages` category.
    fn index_member(&self, category: &Category, value: &Value, summary: &mut ArchiveSummary) {
        match category {
            Category::Profile => summary.counts.profile += 1,
            Category::Posts => posts::index_posts(value, summary),
            Category::Albums => posts::index_album(value, summary),
            Category::Comments => social::index_comments(value, summary),
            Category::Reactions => social::index_reactions(value, summary),
            Category::Friends => social::index_friends(value, summary),
            Category::Groups => social::index_groups(value, summary),
            Category::Events => social::index_events(value, summary),
            Category::Messages => threads::index_messages(value, summary),
            Category::Threads | Category::Other(_) => {}
        }
    }
}

impl ArchiveParser for FacebookParser {
    fn platform(&self) -> Platform {
        Platform::Facebook
    }

    fn detect(&self, dir: &crate::reader::ZipDirectory) -> Option<Detected> {
        let layout = locate(dir);
        let format = if layout.json {
            ExportFormat::Json
        } else if layout.html {
            ExportFormat::Html
        } else {
            return None;
        };
        Some(Detected {
            platform: Platform::Facebook,
            format,
        })
    }

    fn index(&self, reader: &mut ArchiveReader<'_>) -> Result<ArchiveSummary, ArchiveError> {
        let layout = locate(reader.directory());
        if !layout.json {
            if layout.html {
                return Err(ArchiveError::UnsupportedFormat {
                    platform: Platform::Facebook.label().to_string(),
                    format: ExportFormat::Html.label().to_string(),
                });
            }
            return Err(ArchiveError::Undetected);
        }
        let mut summary = ArchiveSummary {
            platform: Platform::Facebook,
            owner: self.owner(reader, &layout),
            date_range: None,
            counts: CategoryCounts::default(),
            media_bytes: layout.media_bytes(reader.directory()),
            parser_version: PARSER_VERSION,
            audiences: AudienceCounts::default(),
        };
        summary.counts.threads = layout.threads.len() as u64;
        for category in Category::ALL {
            if category == Category::Threads {
                continue; // counted from the layout above; its files are the Messages category's
            }
            for member in layout.members_for(&category) {
                let Ok(bytes) = reader.read_member(&member) else {
                    continue; // an unreadable member is a stream-time EntityError, not an index failure
                };
                let Ok(value) = json::parse_member(&bytes) else {
                    continue;
                };
                self.index_member(&category, &value, &mut summary);
            }
        }
        Ok(summary)
    }

    fn stream<'r, 's>(
        &self,
        reader: &'r mut ArchiveReader<'s>,
        category: Category,
    ) -> Box<dyn Iterator<Item = Result<Entity, EntityError>> + 'r> {
        let layout = locate(reader.directory());
        let members = layout.members_for(&category);
        match category {
            Category::Profile => Box::new(MemberStream::new(
                reader,
                category,
                members,
                |_reader, member, bytes| {
                    vec![profile::parse_profile(&bytes, member).map(Entity::Profile)]
                },
            )),
            Category::Posts => Box::new(MemberStream::new(
                reader,
                category,
                members,
                posts::parse_posts,
            )),
            Category::Albums => Box::new(MemberStream::new(
                reader,
                category,
                members,
                posts::parse_album,
            )),
            Category::Comments => {
                let owner = self.owner(reader, &layout);
                Box::new(MemberStream::new(
                    reader,
                    category,
                    members,
                    social::parse_comments(owner),
                ))
            }
            Category::Reactions => {
                let owner = self.owner(reader, &layout);
                Box::new(MemberStream::new(
                    reader,
                    category,
                    members,
                    social::parse_reactions(owner),
                ))
            }
            Category::Friends => Box::new(MemberStream::new(
                reader,
                category,
                members,
                social::parse_friends,
            )),
            Category::Groups => Box::new(MemberStream::new(
                reader,
                category,
                members,
                social::parse_groups,
            )),
            Category::Events => Box::new(MemberStream::new(
                reader,
                category,
                members,
                social::parse_events,
            )),
            Category::Threads | Category::Messages => {
                let owner = self.owner(reader, &layout);
                Box::new(threads::ThreadStream::new(
                    reader,
                    layout.threads.clone(),
                    category,
                    owner,
                ))
            }
            // A category this build cannot name has nothing to stream.
            Category::Other(_) => Box::new(std::iter::empty()),
        }
    }
}
