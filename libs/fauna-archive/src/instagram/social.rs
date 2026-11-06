//! Comments, likes and the followers / following lists. Instagram names
//! people by username — its stable public handle — so an actor ref here
//! carries the username as both its `id` and its display name, unlike the
//! name-only refs of Facebook's prose titles. Every comment and like in the
//! export is the OWNER's (other people's comments on the owner's posts are
//! not exported), so the author is always the owner and the target's owner is
//! the person whose item it was.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::EntityError;
use crate::facebook::json::{arr, entity_error, index_dated, parse_member, secs_field, str_field};
use crate::facebook::layout::file_name;
use crate::instagram::content::{map_value, records};
use crate::model::{
    ArchiveComment, ArchiveFriendship, ArchiveReaction, ArchiveSummary, ArchiveTargetRef, Category,
    Entity, EntityKind, ExternalActorRef, ExternalId, FriendshipKind, MediaInstants, Platform,
    ReactionKind, Timestamp,
};
use crate::reader::ArchiveReader;
use crate::text::clean;

const COMMENT_KEYS: &[&str] = &["comments_media_comments", "comments_reels_comments"];
const LIKE_KEYS: &[&str] = &["likes_media_likes", "likes_comment_likes"];
const FRIEND_KEYS: &[&str] = &["relationships_followers", "relationships_following"];

/// An Instagram username → actor ref; the owner's own ref when it is theirs.
pub(crate) fn user(owner: &ExternalActorRef, username: &str) -> ExternalActorRef {
    let username = clean(username);
    if owner.id.as_deref() == Some(username.as_str()) {
        return owner.clone();
    }
    ExternalActorRef::new(Platform::Instagram, Some(username.clone()), &username)
}

fn decode(bytes: &[u8], category: Category, member: &str) -> Result<Value, EntityError> {
    parse_member(bytes).map_err(|e| entity_error(category, member, 0, e))
}

/// The first `string_list_data` entry — where a like or a follow keeps its
/// link, value and instant.
fn list_entry(record: &Value) -> Option<&Value> {
    arr(record, "string_list_data").first()
}

/// What kind of item a comments / likes member is about, by its file name.
fn kind_hint(member: &str) -> &'static str {
    let file = file_name(&member.to_ascii_lowercase()).to_string();
    if file.starts_with("reels_") {
        "reel"
    } else if file == "liked_comments.json" {
        "comment"
    } else {
        "post"
    }
}

// ---------------------------------------------------------------- comments

fn comment_time(record: &Value) -> Option<Timestamp> {
    let map = record.get("string_map_data")?.as_object()?;
    map.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("Time"))
        .and_then(|(_, v)| secs_field(v, "timestamp"))
}

pub fn index_comments(value: &Value, summary: &mut ArchiveSummary) {
    index_dated(
        records(value, COMMENT_KEYS).iter(),
        &mut summary.counts.comments,
        &mut summary.date_range,
        comment_time,
    );
}

pub fn parse_comments(
    owner: ExternalActorRef,
) -> impl FnMut(&mut ArchiveReader<'_>, &str, Vec<u8>) -> Vec<Result<Entity, EntityError>> {
    move |_reader: &mut ArchiveReader<'_>, member: &str, bytes: Vec<u8>| {
        let value = match decode(&bytes, Category::Comments, member) {
            Ok(v) => v,
            Err(e) => return vec![Err(e)],
        };
        records(&value, COMMENT_KEYS)
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let created_at = comment_time(r).ok_or_else(|| {
                    entity_error(Category::Comments, member, i as u64, "comment has no time")
                })?;
                let text = map_value(r, "Comment");
                let external_id = ExternalId::derive(
                    Platform::Instagram,
                    EntityKind::Comment,
                    created_at,
                    text.as_deref().unwrap_or(""),
                    &MediaInstants::new(),
                );
                Ok(Entity::Comment(ArchiveComment {
                    external_id,
                    created_at,
                    author: owner.clone(),
                    target: ArchiveTargetRef {
                        id: None,
                        url: None,
                        owner: map_value(r, "Media Owner").map(|u| user(&owner, &u)),
                        kind_hint: Some(kind_hint(member).to_string()),
                    },
                    text,
                    media: Vec::new(),
                }))
            })
            .collect()
    }
}

// ------------------------------------------------------------------- likes

pub fn index_reactions(value: &Value, summary: &mut ArchiveSummary) {
    index_dated(
        records(value, LIKE_KEYS).iter(),
        &mut summary.counts.reactions,
        &mut summary.date_range,
        |r| list_entry(r).and_then(|e| secs_field(e, "timestamp")),
    );
}

/// Instagram has one reaction, the like; its ID follows the Facebook rule —
/// `(created_at, kind token)`, with an export-order ordinal on a same-second
/// tie so neither like is lost to external-ID dedup.
pub fn parse_reactions(
    owner: ExternalActorRef,
) -> impl FnMut(&mut ArchiveReader<'_>, &str, Vec<u8>) -> Vec<Result<Entity, EntityError>> {
    move |_reader: &mut ArchiveReader<'_>, member: &str, bytes: Vec<u8>| {
        let value = match decode(&bytes, Category::Reactions, member) {
            Ok(v) => v,
            Err(e) => return vec![Err(e)],
        };
        let mut seen: BTreeMap<u64, u32> = BTreeMap::new();
        records(&value, LIKE_KEYS)
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let entry = list_entry(r);
                let created_at =
                    entry
                        .and_then(|e| secs_field(e, "timestamp"))
                        .ok_or_else(|| {
                            entity_error(
                                Category::Reactions,
                                member,
                                i as u64,
                                "like has no timestamp",
                            )
                        })?;
                let kind = ReactionKind::Like;
                let ordinal = seen
                    .entry(created_at.0)
                    .and_modify(|n| *n += 1)
                    .or_insert(0);
                let token = if *ordinal > 0 {
                    format!("{}#{}", kind.token(), ordinal)
                } else {
                    kind.token()
                };
                let external_id = ExternalId::derive(
                    Platform::Instagram,
                    EntityKind::Reaction,
                    created_at,
                    &token,
                    &MediaInstants::new(),
                );
                Ok(Entity::Reaction(ArchiveReaction {
                    external_id,
                    created_at,
                    author: owner.clone(),
                    target: ArchiveTargetRef {
                        id: None,
                        url: entry.and_then(|e| str_field(e, "href")),
                        owner: str_field(r, "title").map(|u| user(&owner, &u)),
                        kind_hint: Some(kind_hint(member).to_string()),
                    },
                    kind,
                }))
            })
            .collect()
    }
}

// ----------------------------------------------------------------- friends

pub fn index_friends(value: &Value, summary: &mut ArchiveSummary) {
    summary.counts.friends += records(value, FRIEND_KEYS).len() as u64;
}

pub fn parse_friends(
    owner: ExternalActorRef,
) -> impl FnMut(&mut ArchiveReader<'_>, &str, Vec<u8>) -> Vec<Result<Entity, EntityError>> {
    move |_reader: &mut ArchiveReader<'_>, member: &str, bytes: Vec<u8>| {
        let value = match decode(&bytes, Category::Friends, member) {
            Ok(v) => v,
            Err(e) => return vec![Err(e)],
        };
        let kind = if file_name(&member.to_ascii_lowercase()) == "following.json" {
            FriendshipKind::Following
        } else {
            FriendshipKind::Follower
        };
        records(&value, FRIEND_KEYS)
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let entry = list_entry(r);
                // The username is the entry's `value`; a newer vintage of
                // `following.json` moved it to the record's `title`.
                let username = entry
                    .and_then(|e| str_field(e, "value"))
                    .or_else(|| str_field(r, "title"))
                    .ok_or_else(|| {
                        entity_error(
                            Category::Friends,
                            member,
                            i as u64,
                            "follow record has no username",
                        )
                    })?;
                Ok(Entity::Friendship(ArchiveFriendship {
                    actor: user(&owner, &username),
                    since: entry.and_then(|e| secs_field(e, "timestamp")),
                    kind,
                }))
            })
            .collect()
    }
}
