//! `your_posts*.json` → `ArchivePost`; `posts/album/*.json` → `ArchiveAlbum`.
//! Streaming a post hashes its media members (the refs carry the hashes for
//! the upload side; the ID folds in the media creation instants —
//! `external_id.rs`), so posts are the one category whose stream reads media;
//! `index` never does.

use crate::model::Timestamp;
use serde_json::Value;

use crate::error::EntityError;
use crate::facebook::json::{
    arr, audience_field, entity_error, first_key, index_dated, media_ref, parse_member, place,
    secs_field, str_field, tagged,
};
use crate::model::{
    ArchiveAlbum, ArchiveMediaRef, ArchivePost, ArchiveSummary, Category, DateRange, Entity,
    EntityKind, ExternalId, MediaInstants, Platform, media_instants,
};
use crate::reader::ArchiveReader;
use crate::text::repair_facebook_mojibake;

/// The post records of one member: a top-level array in every vintage seen,
/// tolerating a wrapping object.
fn post_records(value: &Value) -> &[Value] {
    value
        .as_array()
        .map(Vec::as_slice)
        .or_else(|| {
            first_key(value, &["posts", "status_updates"])
                .and_then(Value::as_array)
                .map(Vec::as_slice)
        })
        .unwrap_or(&[])
}

/// Index: counts every record (bad ones included, so "N of M" adds up with
/// the skip log) and widens the date range.
pub fn index_posts(value: &Value, summary: &mut ArchiveSummary) {
    let records = post_records(value);
    index_dated(
        records.iter(),
        &mut summary.counts.posts,
        &mut summary.date_range,
        |post| secs_field(post, "timestamp"),
    );
    // The per-audience breakdown (`ArchiveSummary.audiences`) — a second walk
    // over the same records, since `index_dated`'s extractor is a plain `Fn`
    // and cannot bump a sibling field.
    for post in records {
        summary.audiences.bump(audience_field(post));
    }
}

/// The album's date: its earliest photo, else its last-modified stamp.
fn album_created_at(album: &Value) -> Option<Timestamp> {
    arr(album, "photos")
        .iter()
        .chain(arr(album, "videos"))
        .filter_map(|p| secs_field(p, "creation_timestamp"))
        .min()
        .or_else(|| secs_field(album, "last_modified_timestamp"))
}

pub fn index_album(value: &Value, summary: &mut ArchiveSummary) {
    summary.counts.albums += 1;
    summary.audiences.bump(audience_field(value));
    if let Some(t) = album_created_at(value) {
        DateRange::extend(&mut summary.date_range, t);
    }
}

/// Hashes every media ref that is actually in the archive, filling `size`
/// and `blake3_hex` **in place** — the refs are the whole output, which is why
/// this returns nothing. A `uri` is escaped byte-wise like every other string
/// in the export, so a non-ASCII member name arrives mojibake'd: the raw name
/// is tried first, then the repaired one, and the name that resolved is kept.
/// A referenced-but-missing member is left unhashed (the export sometimes
/// references media it did not include).
pub fn hash_media(reader: &mut ArchiveReader<'_>, media: &mut [ArchiveMediaRef]) {
    for m in media.iter_mut() {
        let mut hashed = reader.blake3_member(&m.path).ok();
        if hashed.is_none() {
            let repaired = repair_facebook_mojibake(&m.path);
            if repaired != m.path
                && let Ok(h) = reader.blake3_member(&repaired)
            {
                m.path = repaired.into_owned();
                hashed = Some(h);
            }
        }
        if let Some((hex, size)) = hashed {
            m.size = Some(size);
            m.blake3_hex = Some(hex);
        }
    }
}

/// The most media refs one record may contribute. The list is attacker-chosen
/// and decides how many zip members `read_media` holds at once in the machine
/// crate, so it is bounded here; far above any real post, and a truncation is
/// always reported (never silent).
pub const MAX_RECORD_MEDIA_REFS: usize = 512;

/// Parses one post record. The `usize` is how many media refs were dropped for
/// exceeding [`MAX_RECORD_MEDIA_REFS`] — non-zero means the caller must emit a
/// skip-log line beside the post it still returns.
fn parse_post(
    reader: &mut ArchiveReader<'_>,
    member: &str,
    position: u64,
    post: &Value,
) -> Result<(ArchivePost, usize), EntityError> {
    let created_at = secs_field(post, "timestamp")
        .ok_or_else(|| entity_error(Category::Posts, member, position, "post has no timestamp"))?;
    let text = arr(post, "data").iter().find_map(|d| str_field(d, "post"));
    let mut media = Vec::new();
    let mut links = Vec::new();
    let mut place_ref = None;
    let mut album_name = None;
    // How many media refs beyond `MAX_RECORD_MEDIA_REFS` this record named. The
    // list is attacker-chosen — one crafted record can name millions — and it
    // decides how many members the machine's `read_media` later holds in memory
    // AT ONCE, so the count is bounded here, at the only place that knows it.
    let mut media_dropped = 0usize;
    for attachment in arr(post, "attachments") {
        for item in arr(attachment, "data") {
            if let Some(m) = item.get("media")
                && let Some(r) = media_ref(m)
            {
                if album_name.is_none() {
                    album_name = str_field(m, "title");
                }
                // Truncate, never silently: the tail is reported as a skip-log
                // line by the caller (§ Parser contract rule 1 — a per-entity
                // error lands in the skip log *with the reason*), so a user
                // whose genuinely enormous post was trimmed can see it. The
                // ceiling is far above any real post; Facebook's own composer
                // caps an album post well below it.
                if media.len() >= MAX_RECORD_MEDIA_REFS {
                    media_dropped += 1;
                    continue;
                }
                media.push(r);
            }
            if let Some(url) = item
                .get("external_context")
                .and_then(|c| str_field(c, "url"))
            {
                links.push(url);
            }
            if place_ref.is_none() {
                place_ref = item.get("place").and_then(place);
            }
        }
    }
    hash_media(reader, &mut media);
    let external_id = ExternalId::derive(
        Platform::Facebook,
        EntityKind::Post,
        created_at,
        text.as_deref().unwrap_or(""),
        &media_instants(&media),
    );
    Ok((
        ArchivePost {
            external_id,
            created_at,
            audience: audience_field(post),
            text,
            media,
            links,
            tagged: tagged(post),
            album_name,
            url: None,
            place: place_ref,
        },
        media_dropped,
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
    post_records(&value)
        .iter()
        .enumerate()
        .flat_map(
            |(i, post)| match parse_post(reader, member, i as u64, post) {
                // The post still imports — rule 1 refuses to fail a whole record over
                // this — but the dropped tail rides beside it as its own skip-log
                // line, so the loss is visible rather than silent. `collect_category`
                // partitions the two independently, so one record legitimately
                // contributes both an entity and an error.
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

/// One album member → one album (or one error).
pub fn parse_album(
    reader: &mut ArchiveReader<'_>,
    member: &str,
    bytes: Vec<u8>,
) -> Vec<Result<Entity, EntityError>> {
    let value = match parse_member(&bytes) {
        Ok(v) => v,
        Err(e) => return vec![Err(entity_error(Category::Albums, member, 0, e))],
    };
    let album = (|| {
        let name = str_field(&value, "name")
            .ok_or_else(|| entity_error(Category::Albums, member, 0, "album has no name"))?;
        let created_at = album_created_at(&value)
            .ok_or_else(|| entity_error(Category::Albums, member, 0, "album has no dated media"))?;
        let mut media: Vec<ArchiveMediaRef> = arr(&value, "photos")
            .iter()
            .chain(arr(&value, "videos"))
            .filter_map(media_ref)
            .collect();
        hash_media(reader, &mut media);
        // The cover's own `uri` is still raw while `hash_media` may have
        // rewritten a media path to its repaired form, so match on either.
        let cover = value.get("cover_photo").and_then(media_ref).map(|mut c| {
            let repaired = repair_facebook_mojibake(&c.path).into_owned();
            if let Some(hashed) = media
                .iter()
                .find(|m| m.path == c.path || m.path == repaired)
            {
                c.path = hashed.path.clone();
                c.size = hashed.size;
                c.blake3_hex = hashed.blake3_hex.clone();
            }
            c
        });
        // (created_at, name) only: an album's media set is the one thing that
        // legitimately changes between two exports of the same account, so it
        // never enters the album's identity.
        let external_id = ExternalId::derive(
            Platform::Facebook,
            EntityKind::Album,
            created_at,
            &name,
            &MediaInstants::new(),
        );
        Ok(ArchiveAlbum {
            external_id,
            name,
            description: str_field(&value, "description"),
            created_at,
            audience: audience_field(&value),
            media,
            cover,
        })
    })();
    vec![album.map(Entity::Album)]
}
