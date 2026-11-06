//! The Instagram JSON-export parser (`archive-import.md` § Parser contract,
//! slice 5). Meta writes this export with the Facebook export's JSON writer —
//! the same byte-wise mojibake, the same media objects, Messenger-shaped
//! direct-message threads — so the shared helpers and the thread stream are
//! the Facebook parser's (`crate::facebook`); what is Instagram's is where the
//! files live (`layout`) and the record shapes of its own content and its
//! social lists (`content`, `social`).
//!
//! What it yields: profile, posts (feed posts, reels and archived posts),
//! the owner's comments and likes, followers and following, threads and
//! messages. Instagram has no albums, events or groups; stories are left in
//! the archive, unparsed.

pub mod content;
pub mod layout;
pub mod social;

use serde_json::Value;

use crate::error::{ArchiveError, EntityError};
use crate::facebook::json::parse_member;
use crate::facebook::layout::media_bytes;
use crate::facebook::{MemberStream, threads};
use crate::model::{
    ArchiveSummary, AudienceCounts, Category, CategoryCounts, Entity, ExternalActorRef,
    PARSER_VERSION, Platform,
};
use crate::parser::{ArchiveParser, Detected, ExportFormat};
use crate::reader::{ArchiveReader, ZipDirectory};

use self::layout::{Layout, locate};

pub struct InstagramParser;

impl InstagramParser {
    /// The owner's actor ref from the profile member; an export without a
    /// readable profile gets a nameless owner rather than a refusal.
    fn owner(&self, reader: &mut ArchiveReader<'_>, layout: &Layout) -> ExternalActorRef {
        let nameless = || ExternalActorRef::new(Platform::Instagram, None, "");
        let Some(member) = &layout.profile else {
            return nameless();
        };
        reader
            .read_member(member)
            .ok()
            .and_then(|bytes| content::parse_profile(&bytes, member).ok())
            .map(|p| p.actor)
            .unwrap_or_else(nameless)
    }

    fn index_member(
        &self,
        category: &Category,
        member: &str,
        value: &Value,
        summary: &mut ArchiveSummary,
    ) {
        match category {
            Category::Profile => summary.counts.profile += 1,
            Category::Posts => content::index_posts(member, value, summary),
            Category::Comments => social::index_comments(value, summary),
            Category::Reactions => social::index_reactions(value, summary),
            Category::Friends => social::index_friends(value, summary),
            Category::Messages => threads::index_messages(value, summary),
            Category::Albums
            | Category::Events
            | Category::Groups
            | Category::Threads
            | Category::Other(_) => {}
        }
    }
}

impl ArchiveParser for InstagramParser {
    fn platform(&self) -> Platform {
        Platform::Instagram
    }

    fn detect(&self, dir: &ZipDirectory) -> Option<Detected> {
        let layout = locate(dir);
        let format = if layout.json {
            ExportFormat::Json
        } else if layout.html {
            ExportFormat::Html
        } else {
            return None;
        };
        Some(Detected {
            platform: Platform::Instagram,
            format,
        })
    }

    fn index(&self, reader: &mut ArchiveReader<'_>) -> Result<ArchiveSummary, ArchiveError> {
        let layout = locate(reader.directory());
        if !layout.json {
            if layout.html {
                return Err(ArchiveError::UnsupportedFormat {
                    platform: Platform::Instagram.label().to_string(),
                    format: ExportFormat::Html.label().to_string(),
                });
            }
            return Err(ArchiveError::Undetected);
        }
        let mut summary = ArchiveSummary {
            platform: Platform::Instagram,
            owner: self.owner(reader, &layout),
            date_range: None,
            counts: CategoryCounts::default(),
            media_bytes: media_bytes(reader.directory()),
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
                    continue; // a stream-time EntityError, not an index failure
                };
                let Ok(value) = parse_member(&bytes) else {
                    continue;
                };
                self.index_member(&category, &member, &value, &mut summary);
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
                    vec![content::parse_profile(&bytes, member).map(Entity::Profile)]
                },
            )),
            Category::Posts => Box::new(MemberStream::new(
                reader,
                category,
                members,
                content::parse_posts,
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
            Category::Friends => {
                let owner = self.owner(reader, &layout);
                Box::new(MemberStream::new(
                    reader,
                    category,
                    members,
                    social::parse_friends(owner),
                ))
            }
            Category::Threads | Category::Messages => {
                let owner = self.owner(reader, &layout);
                Box::new(threads::ThreadStream::new(
                    Platform::Instagram,
                    reader,
                    layout.threads.clone(),
                    category,
                    owner,
                ))
            }
            // No such category on Instagram, or one this build cannot name.
            Category::Albums | Category::Events | Category::Groups | Category::Other(_) => {
                Box::new(std::iter::empty())
            }
        }
    }
}
