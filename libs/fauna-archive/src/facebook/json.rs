//! `serde_json::Value` helpers shared by the Facebook category parsers.
//! Every string leaves through [`clean`] (mojibake repair + whitespace),
//! every second-resolution timestamp becomes a microsecond `Timestamp`.

use crate::model::Timestamp;
use serde_json::Value;

use crate::error::EntityError;
use crate::model::{
    ArchiveAudience, ArchiveMediaRef, ArchivePlace, Category, DateRange, ExternalActorRef, Platform,
};
use crate::text::clean;

pub fn parse_member(bytes: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(bytes).map_err(|e| e.to_string())
}

/// A per-record failure for the skip log.
pub fn entity_error(
    category: Category,
    member: &str,
    position: u64,
    reason: impl Into<String>,
) -> EntityError {
    EntityError {
        category,
        member: member.to_string(),
        position,
        reason: reason.into(),
    }
}

/// The most bytes an identity may hold when the parser COPIES it into many
/// records: a thread's platform ID goes into every one of its messages, and
/// the owner's ID and display name into every comment, reaction and message
/// the owner wrote or is the target of. The archive chooses both how long an
/// identity is and how many records repeat it, so with no ceiling one long
/// name was multiplied by a record count — the product of two numbers the
/// archive picks (`archive-import.md` § Parser contract rule 9). Real ones
/// are tens of bytes; 1 KiB refuses nothing an export writes.
pub const MAX_IDENTITY_BYTES: usize = 1024;

/// `Err` naming the cause when `value` — an identity the parser copies into
/// many records, described by `what` — is longer than [`MAX_IDENTITY_BYTES`].
/// The caller refuses the record that owns it (§ Parser contract rule 1).
pub fn check_identity(what: &str, value: &str) -> Result<(), String> {
    let len = value.len();
    if len > MAX_IDENTITY_BYTES {
        return Err(format!(
            "{what} is {len} bytes, over the {MAX_IDENTITY_BYTES} an identity copied into \
             every record that names it may hold"
        ));
    }
    Ok(())
}

/// The array under `key`, or empty.
pub fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// The first of `keys` present on `v` (`comments_v2` before the legacy `comments`).
pub fn first_key<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| v.get(*k))
}

/// A cleaned, non-empty string field.
pub fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(clean)
        .filter(|s| !s.is_empty())
}

/// Seconds since the epoch → `Timestamp` (microseconds).
pub fn secs(v: &Value) -> Option<Timestamp> {
    v.as_u64().map(|s| Timestamp(s.saturating_mul(1_000_000)))
}

pub fn secs_field(v: &Value, key: &str) -> Option<Timestamp> {
    v.get(key).and_then(secs)
}

/// Milliseconds since the epoch (message `timestamp_ms`) → `Timestamp`.
pub fn millis_field(v: &Value, key: &str) -> Option<Timestamp> {
    v.get(key)
        .and_then(Value::as_u64)
        .map(|ms| Timestamp(ms.saturating_mul(1_000)))
}

/// Bump `count` once per item in `items` and widen `date_range` from each
/// item's `extract_ts` (a no-op when it returns `None`) — the count+range
/// bookkeeping every category's `index_*` hand-copied identically (each
/// still supplies its own record source and its own timestamp field/unit).
pub fn index_dated<'a>(
    items: impl Iterator<Item = &'a Value>,
    count: &mut u64,
    date_range: &mut Option<DateRange>,
    extract_ts: impl Fn(&Value) -> Option<Timestamp>,
) {
    for item in items {
        *count += 1;
        if let Some(t) = extract_ts(item) {
            DateRange::extend(date_range, t);
        }
    }
}

/// A number or numeric string as its decimal text — coordinates stay
/// strings because canonical DAG-CBOR forbids floats.
pub fn decimal(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        _ => None,
    }
}

/// Facebook's privacy labels onto the five audiences; anything unrecognised
/// is `Unknown` (which the goal doc maps to owner-only, never public).
pub fn audience_from_privacy(raw: &str) -> ArchiveAudience {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.as_str() {
        "public" | "everyone" => ArchiveAudience::Public,
        "friends" => ArchiveAudience::Friends,
        "only me" | "only_me" | "self" | "me" => ArchiveAudience::OnlyMe,
        s if s.contains("custom")
            || s.contains("friends except")
            || s.contains("specific")
            || s.contains("friends of friends") =>
        {
            ArchiveAudience::Custom
        }
        _ => ArchiveAudience::Unknown,
    }
}

/// The audience of a record: a top-level `privacy` string, else a
/// `data[].privacy` string, else `Unknown`.
pub fn audience_field(v: &Value) -> ArchiveAudience {
    if let Some(p) = v.get("privacy").and_then(Value::as_str) {
        return audience_from_privacy(p);
    }
    arr(v, "data")
        .iter()
        .find_map(|d| d.get("privacy").and_then(Value::as_str))
        .map(audience_from_privacy)
        .unwrap_or(ArchiveAudience::Unknown)
}

/// MIME from the path's extension; `None` for anything not in the media set.
pub fn mime_for_path(path: &str) -> Option<String> {
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "m4v" => "video/x-m4v",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wav" => "audio/wav",
        _ => return None,
    };
    Some(mime.to_string())
}

/// A `{ "uri", "creation_timestamp", "description", "media_metadata" }`
/// object → a media ref (size/hash filled by the caller once read).
pub fn media_ref(v: &Value) -> Option<ArchiveMediaRef> {
    let path = v.get("uri").and_then(Value::as_str)?.trim().to_string();
    if path.is_empty() || path.starts_with("http://") || path.starts_with("https://") {
        return None;
    }
    let exif_taken = v
        .get("media_metadata")
        .and_then(|m| m.get("photo_metadata").or_else(|| m.get("video_metadata")))
        .and_then(|m| m.get("exif_data"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|e| secs_field(e, "taken_timestamp"));
    Some(ArchiveMediaRef {
        mime: mime_for_path(&path),
        taken_at: secs_field(v, "creation_timestamp").or(exif_taken),
        caption: str_field(v, "description"),
        path,
        size: None,
        blake3_hex: None,
    })
}

/// A Facebook display name → actor ref (names only; no IDs in these files).
pub fn actor(name: &str) -> ExternalActorRef {
    ExternalActorRef::new(Platform::Facebook, None, &clean(name))
}

/// `"tags": ["Name"]` or `"tags": [{"name": "Name"}]` → actor refs.
pub fn tagged(v: &Value) -> Vec<ExternalActorRef> {
    arr(v, "tags")
        .iter()
        .filter_map(|t| t.as_str().or_else(|| t.get("name").and_then(Value::as_str)))
        .map(actor)
        .collect()
}

/// `{ "name", "coordinate": { "latitude", "longitude" }, "address", "url" }`.
pub fn place(v: &Value) -> Option<ArchivePlace> {
    let name = str_field(v, "name")?;
    let coordinate = v.get("coordinate");
    Some(ArchivePlace {
        name,
        address: str_field(v, "address"),
        latitude: coordinate.and_then(|c| c.get("latitude")).and_then(decimal),
        longitude: coordinate
            .and_then(|c| c.get("longitude"))
            .and_then(decimal),
        url: str_field(v, "url"),
    })
}

const VERBS: &[&str] = &[
    " commented on ",
    " replied to ",
    " reacted to ",
    " likes ",
    " liked ",
    " loves ",
    " loved ",
    " shared ",
];

/// Facebook names a comment's or reaction's target only in prose:
/// `"Test Owner commented on Friend One's photo."`. Returns the target's
/// owner (the archive owner for "his/her/their own …") and the kind word.
pub fn parse_title_target(
    title: &str,
    owner: &ExternalActorRef,
) -> (Option<ExternalActorRef>, Option<String>) {
    let title = clean(title);
    let Some((idx, verb)) = VERBS
        .iter()
        .filter_map(|v| title.find(v).map(|i| (i, *v)))
        .min_by_key(|(i, _)| *i)
    else {
        return (None, None);
    };
    let rest = title[idx + verb.len()..].trim_end_matches('.').trim();
    for own in ["his own ", "her own ", "their own ", "own "] {
        if let Some(kind) = rest.strip_prefix(own) {
            return (Some(owner.clone()), Some(kind.to_string()));
        }
    }
    for possessive in ["'s ", "\u{2019}s "] {
        if let Some(p) = rest.find(possessive) {
            let name = &rest[..p];
            let kind = &rest[p + possessive.len()..];
            let who = actor(name);
            let who = if who.name_key == owner.name_key {
                owner.clone()
            } else {
                who
            };
            return (Some(who), Some(kind.to_string()));
        }
    }
    if let Some(kind) = rest.strip_prefix("a ").or_else(|| rest.strip_prefix("an ")) {
        return (None, Some(kind.to_string()));
    }
    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> ExternalActorRef {
        ExternalActorRef::new(Platform::Facebook, Some("1".into()), "Test Owner")
    }

    #[test]
    fn title_target_names_the_other_persons_item() {
        let (who, kind) =
            parse_title_target("Test Owner commented on Friend One's photo.", &owner());
        assert_eq!(who.unwrap().display_name, "Friend One");
        assert_eq!(kind.as_deref(), Some("photo"));
        let (who, kind) =
            parse_title_target("Test Owner likes Friend Two\u{2019}s post.", &owner());
        assert_eq!(who.unwrap().name_key, "friend two");
        assert_eq!(kind.as_deref(), Some("post"));
    }

    #[test]
    fn title_target_owner_by_name_is_the_owner_ref() {
        let (who, _) = parse_title_target("Friend Two commented on Test Owner's post.", &owner());
        assert_eq!(who, Some(owner()));
    }

    #[test]
    fn title_target_own_item_is_the_owner() {
        let (who, kind) = parse_title_target("Test Owner commented on his own post.", &owner());
        assert_eq!(who, Some(owner()));
        assert_eq!(kind.as_deref(), Some("post"));
    }

    #[test]
    fn title_target_anonymous_and_unknown() {
        let (who, kind) = parse_title_target("Test Owner commented on a video.", &owner());
        assert!(who.is_none());
        assert_eq!(kind.as_deref(), Some("video"));
        assert_eq!(
            parse_title_target("Test Owner updated his status.", &owner()),
            (None, None)
        );
    }

    #[test]
    fn privacy_labels_map_onto_audiences() {
        assert_eq!(audience_from_privacy("Public"), ArchiveAudience::Public);
        assert_eq!(audience_from_privacy("friends"), ArchiveAudience::Friends);
        assert_eq!(audience_from_privacy("Only me"), ArchiveAudience::OnlyMe);
        assert_eq!(
            audience_from_privacy("Friends except..."),
            ArchiveAudience::Custom
        );
        assert_eq!(audience_from_privacy("Custom"), ArchiveAudience::Custom);
        assert_eq!(audience_from_privacy(""), ArchiveAudience::Unknown);
        assert_eq!(audience_from_privacy("whatever"), ArchiveAudience::Unknown);
    }

    #[test]
    fn timestamps_scale_to_microseconds() {
        let v: Value =
            serde_json::from_str(r#"{"timestamp": 1600000000, "timestamp_ms": 1650000002000}"#)
                .unwrap();
        assert_eq!(
            secs_field(&v, "timestamp"),
            Some(Timestamp(1_600_000_000_000_000))
        );
        assert_eq!(
            millis_field(&v, "timestamp_ms"),
            Some(Timestamp(1_650_000_002_000_000))
        );
        assert_eq!(secs_field(&v, "missing"), None);
    }

    #[test]
    fn coordinates_stay_decimal_strings() {
        let v: Value = serde_json::from_str(
            r#"{"name": "P", "coordinate": {"latitude": 59.91, "longitude": 10.75}}"#,
        )
        .unwrap();
        let p = place(&v).unwrap();
        assert_eq!(p.latitude.as_deref(), Some("59.91"));
        assert_eq!(p.longitude.as_deref(), Some("10.75"));
    }

    #[test]
    fn media_ref_maps_mime_and_skips_urls() {
        let v: Value = serde_json::from_str(
            r#"{"uri": "a/b/photo.JPG", "creation_timestamp": 5, "description": "cap"}"#,
        )
        .unwrap();
        let m = media_ref(&v).unwrap();
        assert_eq!(m.mime.as_deref(), Some("image/jpeg"));
        assert_eq!(m.taken_at, Some(Timestamp(5_000_000)));
        assert_eq!(m.caption.as_deref(), Some("cap"));
        let url: Value =
            serde_json::from_str(r#"{"uri": "https://example.invalid/x.jpg"}"#).unwrap();
        assert!(media_ref(&url).is_none());
    }
}
