//! The small social categories: comments, reactions, friends, groups,
//! events. None of these reads media during `stream`; IDs derive from a
//! timestamp and one stable text alone (a reaction's is its kind token, not
//! its localized title — see `ReactionKind::token`).

use std::collections::BTreeMap;

use crate::model::Timestamp;
use serde_json::Value;

use crate::error::EntityError;
use crate::facebook::json::{
    actor, arr, entity_error, first_key, index_dated, media_ref, parse_member, parse_title_target,
    place, secs_field, str_field,
};
use crate::model::{
    ArchiveComment, ArchiveEvent, ArchiveFriendship, ArchiveGroup, ArchiveMediaRef,
    ArchiveReaction, ArchiveSummary, ArchiveTargetRef, Category, Entity, EntityKind,
    ExternalActorRef, ExternalId, FriendshipKind, MediaInstants, Platform, ReactionKind, Rsvp,
};
use crate::reader::ArchiveReader;

const COMMENT_KEYS: &[&str] = &["comments_v2", "comments"];
const REACTION_KEYS: &[&str] = &["reactions_v2", "reactions"];
const FRIEND_KEYS: &[&str] = &["friends_v2", "friends", "followers_v2", "following_v2"];
const GROUP_KEYS: &[&str] = &["groups_joined_v2", "groups_joined"];

/// A member's records: a top-level array, else the first of `keys`, else the
/// longest array-valued field of a wrapping object (a `serde_json::Map` is
/// ordered by key, not by file position, so "first" would be arbitrary).
fn records<'a>(value: &'a Value, keys: &[&str]) -> &'a [Value] {
    if let Some(a) = value.as_array() {
        return a;
    }
    if let Some(a) = first_key(value, keys).and_then(Value::as_array) {
        return a;
    }
    value
        .as_object()
        .and_then(|o| {
            o.values()
                .filter_map(Value::as_array)
                .max_by_key(|a| a.len())
        })
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn attachment_items(record: &Value) -> impl Iterator<Item = &Value> {
    arr(record, "attachments")
        .iter()
        .flat_map(|a| arr(a, "data"))
}

fn attachment_url(record: &Value) -> Option<String> {
    attachment_items(record)
        .find_map(|i| i.get("external_context").and_then(|c| str_field(c, "url")))
}

fn attachment_media(record: &Value) -> Vec<ArchiveMediaRef> {
    attachment_items(record)
        .filter_map(|i| i.get("media").and_then(media_ref))
        .collect()
}

fn decode(bytes: &[u8], category: Category, member: &str) -> Result<Value, EntityError> {
    parse_member(bytes).map_err(|e| entity_error(category, member, 0, e))
}

// ---------------------------------------------------------------- comments

pub fn index_comments(value: &Value, summary: &mut ArchiveSummary) {
    index_dated(
        records(value, COMMENT_KEYS).iter(),
        &mut summary.counts.comments,
        &mut summary.date_range,
        |r| secs_field(r, "timestamp"),
    );
}

fn parse_comment(
    owner: &ExternalActorRef,
    member: &str,
    position: u64,
    record: &Value,
) -> Result<ArchiveComment, EntityError> {
    let inner = arr(record, "data")
        .iter()
        .find_map(|d| d.get("comment"))
        .ok_or_else(|| {
            entity_error(
                Category::Comments,
                member,
                position,
                "record has no comment",
            )
        })?;
    let created_at = secs_field(inner, "timestamp")
        .or_else(|| secs_field(record, "timestamp"))
        .ok_or_else(|| {
            entity_error(
                Category::Comments,
                member,
                position,
                "comment has no timestamp",
            )
        })?;
    let text = str_field(inner, "comment");
    let author = str_field(inner, "author")
        .map(|a| actor(&a))
        .unwrap_or_else(|| owner.clone());
    let author = if author.name_key == owner.name_key {
        owner.clone()
    } else {
        author
    };
    let (target_owner, kind_hint) = record
        .get("title")
        .and_then(Value::as_str)
        .map(|t| parse_title_target(t, owner))
        .unwrap_or((None, None));
    let external_id = ExternalId::derive(
        Platform::Facebook,
        EntityKind::Comment,
        created_at,
        text.as_deref().unwrap_or(""),
        &MediaInstants::new(),
    );
    Ok(ArchiveComment {
        external_id,
        created_at,
        author,
        target: ArchiveTargetRef {
            id: None,
            url: attachment_url(record),
            owner: target_owner,
            kind_hint,
        },
        text,
        media: attachment_media(record),
    })
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
            .map(|(i, r)| parse_comment(&owner, member, i as u64, r).map(Entity::Comment))
            .collect()
    }
}

// --------------------------------------------------------------- reactions

pub fn index_reactions(value: &Value, summary: &mut ArchiveSummary) {
    index_dated(
        records(value, REACTION_KEYS).iter(),
        &mut summary.counts.reactions,
        &mut summary.date_range,
        |r| secs_field(r, "timestamp"),
    );
}

/// One reaction record. The ID is the plain `(created_at, kind token)` pair;
/// the tie rule that disambiguates two same-kind reactions in one second
/// lives in [`parse_reactions`], which alone sees the member's other records.
fn parse_reaction(
    owner: &ExternalActorRef,
    member: &str,
    position: u64,
    record: &Value,
) -> Result<ArchiveReaction, EntityError> {
    let inner = arr(record, "data")
        .iter()
        .find_map(|d| d.get("reaction"))
        .ok_or_else(|| {
            entity_error(
                Category::Reactions,
                member,
                position,
                "record has no reaction",
            )
        })?;
    let created_at = secs_field(record, "timestamp").ok_or_else(|| {
        entity_error(
            Category::Reactions,
            member,
            position,
            "reaction has no timestamp",
        )
    })?;
    let kind = inner
        .get("reaction")
        .and_then(Value::as_str)
        .map(ReactionKind::from_facebook)
        .unwrap_or(ReactionKind::Other { raw: String::new() });
    let author = str_field(inner, "actor")
        .map(|a| actor(&a))
        .unwrap_or_else(|| owner.clone());
    let author = if author.name_key == owner.name_key {
        owner.clone()
    } else {
        author
    };
    let title = str_field(record, "title").unwrap_or_default();
    let (target_owner, kind_hint) = parse_title_target(&title, owner);
    // (created_at, kind) is everything a re-export cannot change; a tie —
    // two same-kind reactions in one second — gets an ordinal in export
    // order so neither is dropped by external-ID dedup. The first of a tie
    // keeps the plain token, so the common case's ID never moves.
    let external_id = ExternalId::derive(
        Platform::Facebook,
        EntityKind::Reaction,
        created_at,
        &kind.token(),
        &MediaInstants::new(),
    );
    Ok(ArchiveReaction {
        external_id,
        created_at,
        author,
        target: ArchiveTargetRef {
            id: None,
            url: attachment_url(record),
            owner: target_owner,
            kind_hint,
        },
        kind,
    })
}

pub fn parse_reactions(
    owner: ExternalActorRef,
) -> impl FnMut(&mut ArchiveReader<'_>, &str, Vec<u8>) -> Vec<Result<Entity, EntityError>> {
    move |_reader: &mut ArchiveReader<'_>, member: &str, bytes: Vec<u8>| {
        let value = match decode(&bytes, Category::Reactions, member) {
            Ok(v) => v,
            Err(e) => return vec![Err(e)],
        };
        let mut seen: BTreeMap<(u64, String), u32> = BTreeMap::new();
        records(&value, REACTION_KEYS)
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let mut reaction = parse_reaction(&owner, member, i as u64, r)?;
                let key = (reaction.created_at.0, reaction.kind.token());
                let ordinal = seen.entry(key).and_modify(|n| *n += 1).or_insert(0);
                if *ordinal > 0 {
                    reaction.external_id = ExternalId::derive(
                        Platform::Facebook,
                        EntityKind::Reaction,
                        reaction.created_at,
                        &format!("{}#{}", reaction.kind.token(), ordinal),
                        &MediaInstants::new(),
                    );
                }
                Ok(Entity::Reaction(reaction))
            })
            .collect()
    }
}

// ----------------------------------------------------------------- friends

pub fn index_friends(value: &Value, summary: &mut ArchiveSummary) {
    summary.counts.friends += records(value, FRIEND_KEYS).len() as u64;
}

/// The relationship a friends-category member describes, by its file name.
fn friendship_kind(member: &str) -> FriendshipKind {
    let file = member
        .rsplit('/')
        .next()
        .unwrap_or(member)
        .to_ascii_lowercase();
    if file.starts_with("followers") {
        FriendshipKind::Follower
    } else if file.starts_with("who_you_follow") {
        FriendshipKind::Following
    } else {
        FriendshipKind::Friend
    }
}

pub fn parse_friends(
    _reader: &mut ArchiveReader<'_>,
    member: &str,
    bytes: Vec<u8>,
) -> Vec<Result<Entity, EntityError>> {
    let value = match decode(&bytes, Category::Friends, member) {
        Ok(v) => v,
        Err(e) => return vec![Err(e)],
    };
    let kind = friendship_kind(member);
    records(&value, FRIEND_KEYS)
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let name = str_field(r, "name").ok_or_else(|| {
                entity_error(
                    Category::Friends,
                    member,
                    i as u64,
                    "friend record has no name",
                )
            })?;
            Ok(Entity::Friendship(ArchiveFriendship {
                actor: actor(&name),
                since: secs_field(r, "timestamp"),
                kind,
            }))
        })
        .collect()
}

// ------------------------------------------------------------------ groups

pub fn index_groups(value: &Value, summary: &mut ArchiveSummary) {
    summary.counts.groups += records(value, GROUP_KEYS).len() as u64;
}

pub fn parse_groups(
    _reader: &mut ArchiveReader<'_>,
    member: &str,
    bytes: Vec<u8>,
) -> Vec<Result<Entity, EntityError>> {
    let value = match decode(&bytes, Category::Groups, member) {
        Ok(v) => v,
        Err(e) => return vec![Err(e)],
    };
    records(&value, GROUP_KEYS)
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let name = str_field(r, "name").ok_or_else(|| {
                entity_error(
                    Category::Groups,
                    member,
                    i as u64,
                    "group record has no name",
                )
            })?;
            let joined_at = secs_field(r, "timestamp");
            let external_id = ExternalId::derive(
                Platform::Facebook,
                EntityKind::Group,
                joined_at.unwrap_or(Timestamp(0)),
                &name,
                &MediaInstants::new(),
            );
            Ok(Entity::Group(ArchiveGroup {
                external_id,
                name,
                joined_at,
                url: str_field(r, "url"),
            }))
        })
        .collect()
}

// ------------------------------------------------------------------ events

/// Every event list in the member with the RSVP it implies, in file order.
fn event_lists(value: &Value) -> Vec<(Rsvp, &[Value])> {
    let mut lists = Vec::new();
    if let Some(r) = first_key(value, &["event_responses_v2", "event_responses"]) {
        for (key, rsvp) in [
            ("events_joined", Rsvp::Joined),
            ("events_interested", Rsvp::Interested),
            ("events_declined", Rsvp::Declined),
            ("events_invited", Rsvp::Invited),
        ] {
            let list = arr(r, key);
            if !list.is_empty() {
                lists.push((rsvp, list));
            }
        }
    }
    if let Some(a) = first_key(value, &["your_events_v2", "your_events"]).and_then(Value::as_array)
    {
        lists.push((Rsvp::Hosted, a.as_slice()));
    }
    if let Some(a) =
        first_key(value, &["event_invitations_v2", "event_invitations"]).and_then(Value::as_array)
    {
        lists.push((Rsvp::Invited, a.as_slice()));
    }
    lists
}

pub fn index_events(value: &Value, summary: &mut ArchiveSummary) {
    index_dated(
        event_lists(value)
            .into_iter()
            .flat_map(|(_, list)| list.iter()),
        &mut summary.counts.events,
        &mut summary.date_range,
        |ev| secs_field(ev, "start_timestamp"),
    );
}

pub fn parse_events(
    _reader: &mut ArchiveReader<'_>,
    member: &str,
    bytes: Vec<u8>,
) -> Vec<Result<Entity, EntityError>> {
    let value = match decode(&bytes, Category::Events, member) {
        Ok(v) => v,
        Err(e) => return vec![Err(e)],
    };
    let mut out = Vec::new();
    let mut position = 0u64;
    for (rsvp, list) in event_lists(&value) {
        for ev in list {
            let item = (|| {
                let title = str_field(ev, "name").ok_or_else(|| {
                    entity_error(Category::Events, member, position, "event has no name")
                })?;
                let start = secs_field(ev, "start_timestamp").ok_or_else(|| {
                    entity_error(
                        Category::Events,
                        member,
                        position,
                        "event has no start_timestamp",
                    )
                })?;
                let end = secs_field(ev, "end_timestamp").filter(|t| t.0 > 0);
                let external_id = ExternalId::derive(
                    Platform::Facebook,
                    EntityKind::Event,
                    start,
                    &title,
                    &MediaInstants::new(),
                );
                Ok(Entity::Event(ArchiveEvent {
                    external_id,
                    title,
                    description: str_field(ev, "description"),
                    start,
                    end,
                    place: ev.get("place").and_then(place),
                    rsvp,
                    attendees: Vec::new(),
                    url: str_field(ev, "url"),
                }))
            })();
            out.push(item);
            position += 1;
        }
    }
    out
}
