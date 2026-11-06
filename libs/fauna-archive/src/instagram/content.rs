//! The owner's own content: `posts_N.json`, `reels.json` and
//! `archived_posts.json` → `ArchivePost`; `personal_information.json` →
//! `ArchiveProfile`.
//!
//! An Instagram post record is a `media` list plus, for a multi-photo post, a
//! top-level `title` (the caption) and `creation_timestamp`; a single-photo
//! post carries both on its one media item instead. The export records no
//! per-post audience, so a post is `Unknown` — which the goal doc maps to
//! owner-only, never to public — except an archived post, which the owner
//! had hidden from everyone: `OnlyMe`.

use serde_json::Value;

use crate::error::EntityError;
use crate::facebook::json::{
    arr, check_identity, entity_error, first_key, index_dated, media_ref, parse_member, secs_field,
    str_field,
};
use crate::facebook::layout::file_name;
use crate::facebook::posts::{MAX_RECORD_MEDIA_REFS, hash_media};
use crate::model::{
    ArchiveAudience, ArchiveMediaRef, ArchivePost, ArchiveProfile, ArchiveSummary, Category,
    Entity, EntityKind, ExternalActorRef, ExternalId, MediaInstants, Platform, Timestamp,
    media_instants,
};
use crate::reader::ArchiveReader;

const POST_KEYS: &[&str] = &["ig_posts", "ig_reels_media", "ig_archived_post_media"];

/// A member's records: a top-level array, else the first of `keys`, else the
/// longest array-valued field of a wrapping object (a `serde_json::Map` is
/// ordered by key, not file position, so "first" would be arbitrary).
pub(crate) fn records<'a>(value: &'a Value, keys: &[&str]) -> &'a [Value] {
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

/// The audience every post of `member` carries (see the module doc).
pub(crate) fn member_audience(member: &str) -> ArchiveAudience {
    if file_name(&member.to_ascii_lowercase()) == "archived_posts.json" {
        ArchiveAudience::OnlyMe
    } else {
        ArchiveAudience::Unknown
    }
}

/// The post's instant: the record's own stamp, else its earliest media's.
fn post_created_at(post: &Value) -> Option<Timestamp> {
    secs_field(post, "creation_timestamp").or_else(|| {
        arr(post, "media")
            .iter()
            .filter_map(|m| secs_field(m, "creation_timestamp"))
            .min()
    })
}

/// The caption: the record's `title`, else its first media item's.
fn post_text(post: &Value) -> Option<String> {
    str_field(post, "title").or_else(|| {
        arr(post, "media")
            .iter()
            .find_map(|m| str_field(m, "title"))
    })
}

pub fn index_posts(member: &str, value: &Value, summary: &mut ArchiveSummary) {
    let records = records(value, POST_KEYS);
    index_dated(
        records.iter(),
        &mut summary.counts.posts,
        &mut summary.date_range,
        post_created_at,
    );
    let audience = member_audience(member);
    for _ in records {
        summary.audiences.bump(audience);
    }
}

/// One post record; the `usize` is how many media refs were dropped past
/// [`MAX_RECORD_MEDIA_REFS`] (the caller reports them, never silently).
fn parse_post(
    reader: &mut ArchiveReader<'_>,
    member: &str,
    position: u64,
    post: &Value,
) -> Result<(ArchivePost, usize), EntityError> {
    let created_at = post_created_at(post).ok_or_else(|| {
        entity_error(
            Category::Posts,
            member,
            position,
            "post has no creation_timestamp",
        )
    })?;
    let text = post_text(post);
    let mut media: Vec<ArchiveMediaRef> = Vec::new();
    let mut dropped = 0usize;
    for item in arr(post, "media").iter().filter_map(media_ref) {
        if media.len() >= MAX_RECORD_MEDIA_REFS {
            dropped += 1;
            continue;
        }
        media.push(item);
    }
    hash_media(reader, &mut media);
    let external_id = ExternalId::derive(
        Platform::Instagram,
        EntityKind::Post,
        created_at,
        text.as_deref().unwrap_or(""),
        &media_instants(&media),
    );
    Ok((
        ArchivePost {
            external_id,
            created_at,
            audience: member_audience(member),
            text,
            media,
            links: Vec::new(),
            tagged: Vec::new(),
            album_name: None,
            url: None,
            place: None,
        },
        dropped,
    ))
}

/// One posts member → its records, bad ones as errors in place.
pub fn parse_posts(
    reader: &mut ArchiveReader<'_>,
    member: &str,
    bytes: Vec<u8>,
) -> Vec<Result<Entity, EntityError>> {
    let value = match parse_member(&bytes) {
        Ok(v) => v,
        Err(e) => return vec![Err(entity_error(Category::Posts, member, 0, e))],
    };
    records(&value, POST_KEYS)
        .iter()
        .enumerate()
        .flat_map(
            |(i, post)| match parse_post(reader, member, i as u64, post) {
                Ok((post, dropped)) if dropped > 0 => {
                    let note = entity_error(
                        Category::Posts,
                        member,
                        i as u64,
                        format!(
                            "post named more than {MAX_RECORD_MEDIA_REFS} media files; \
                             imported the first {MAX_RECORD_MEDIA_REFS} and skipped {dropped}"
                        ),
                    );
                    vec![Ok(Entity::Post(post)), Err(note)]
                }
                Ok((post, _)) => vec![Ok(Entity::Post(post))],
                Err(e) => vec![Err(e)],
            },
        )
        .collect()
}

// ----------------------------------------------------------------- profile

/// The `value` of a `string_map_data` entry, its label matched
/// case-insensitively (vintages differ in capitalisation).
pub(crate) fn map_value(record: &Value, label: &str) -> Option<String> {
    record
        .get("string_map_data")
        .and_then(Value::as_object)?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(label))
        .and_then(|(_, v)| str_field(v, "value"))
}

fn profile_error(member: &str, reason: impl Into<String>) -> EntityError {
    entity_error(Category::Profile, member, 0, reason)
}

pub fn parse_profile(bytes: &[u8], member: &str) -> Result<ArchiveProfile, EntityError> {
    let value = parse_member(bytes).map_err(|e| profile_error(member, e))?;
    let record = arr(&value, "profile_user")
        .first()
        .or_else(|| value.get("string_map_data").map(|_| &value))
        .ok_or_else(|| profile_error(member, "no profile_user record"))?;
    let username = map_value(record, "Username");
    let name = map_value(record, "Name")
        .or_else(|| username.clone())
        .ok_or_else(|| profile_error(member, "profile has no name or username"))?;
    let actor = ExternalActorRef::new(Platform::Instagram, username.clone(), &name);
    // Copied into every record the owner wrote or is the target of — the
    // Facebook profile's ceiling, for the same reason.
    if let Some(id) = &actor.id {
        check_identity("the owner's ID", id).map_err(|e| profile_error(member, e))?;
    }
    check_identity("the owner's name", &actor.display_name)
        .map_err(|e| profile_error(member, e))?;
    let external_id = match &username {
        Some(id) => ExternalId::native(Platform::Instagram, EntityKind::Profile, id),
        None => ExternalId::derive(
            Platform::Instagram,
            EntityKind::Profile,
            Timestamp(0),
            &actor.display_name,
            &MediaInstants::new(),
        ),
    };
    let picture = record
        .get("media_map_data")
        .and_then(Value::as_object)
        .and_then(|m| {
            m.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("Profile Photo"))
        })
        .and_then(|(_, v)| media_ref(v));
    Ok(ArchiveProfile {
        external_id,
        actor,
        bio: map_value(record, "Bio"),
        links: map_value(record, "Website").into_iter().collect(),
        picture,
        registered_at: None,
    })
}
