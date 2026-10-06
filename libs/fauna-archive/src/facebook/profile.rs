//! `profile_information.json` → `ArchiveProfile` — and the owner's actor
//! ref every other category parser needs (title parsing keys on it).

use crate::model::Timestamp;
use serde_json::Value;

use crate::error::EntityError;
use crate::facebook::json::{arr, check_identity, first_key, parse_member, secs_field, str_field};
use crate::model::{
    ArchiveProfile, Category, EntityKind, ExternalActorRef, ExternalId, MediaInstants, Platform,
};

fn error(member: &str, reason: impl Into<String>) -> EntityError {
    EntityError {
        category: Category::Profile,
        member: member.to_string(),
        position: 0,
        reason: reason.into(),
    }
}

/// The platform's own identifier for the owner: `username`, else the last
/// path segment of `profile_uri` (a numeric ID or a username). The ID-only
/// permalink form `.../profile.php?id=<numeric>` yields the numeric ID.
fn owner_id(profile: &Value) -> Option<String> {
    if let Some(u) = str_field(profile, "username") {
        return Some(u);
    }
    let uri = str_field(profile, "profile_uri")?;
    let tail = uri
        .trim_end_matches('/')
        .rsplit('/')
        .next()?
        .trim()
        .to_string();
    let tail = tail
        .strip_prefix("profile.php?id=")
        .map(str::to_string)
        .unwrap_or(tail);
    if tail.is_empty() || tail.starts_with("www.") || tail.contains(':') {
        return None;
    }
    Some(tail)
}

pub fn parse_profile(bytes: &[u8], member: &str) -> Result<ArchiveProfile, EntityError> {
    let value = parse_member(bytes).map_err(|e| error(member, e))?;
    let profile = first_key(&value, &["profile_v2", "profile"])
        .ok_or_else(|| error(member, "no profile_v2 object"))?;
    let name = profile
        .get("name")
        .and_then(|n| str_field(n, "full_name"))
        .or_else(|| {
            let n = profile.get("name")?;
            let first = str_field(n, "first_name")?;
            let last = str_field(n, "last_name").unwrap_or_default();
            Some(format!("{first} {last}").trim().to_string())
        })
        .ok_or_else(|| error(member, "profile has no name"))?;
    let id = owner_id(profile);
    let registered_at = secs_field(profile, "registration_timestamp");
    let actor = ExternalActorRef::new(Platform::Facebook, id.clone(), &name);
    // The owner ref is copied into every record the owner wrote or is the
    // target of; a profile that would make it outsized is refused, and the
    // export is read with the nameless owner a missing profile gives.
    if let Some(id) = &actor.id {
        check_identity("the owner's ID", id).map_err(|e| error(member, e))?;
    }
    check_identity("the owner's name", &actor.display_name).map_err(|e| error(member, e))?;
    let external_id = match &id {
        Some(id) => ExternalId::native(Platform::Facebook, EntityKind::Profile, id),
        None => ExternalId::derive(
            Platform::Facebook,
            EntityKind::Profile,
            registered_at.unwrap_or(Timestamp(0)),
            &actor.display_name,
            &MediaInstants::new(),
        ),
    };
    Ok(ArchiveProfile {
        external_id,
        actor,
        bio: profile.get("intro_bio").and_then(|b| str_field(b, "text")),
        links: arr(profile, "websites")
            .iter()
            .filter_map(|w| str_field(w, "address"))
            .collect(),
        picture: None,
        registered_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_php_permalink_yields_the_numeric_id() {
        let bytes = br#"{"profile_v2":{"name":{"full_name":"Test Owner"},
            "profile_uri":"https://www.facebook.com/profile.php?id=100099999999999"}}"#;
        let profile = parse_profile(bytes, "profile_information.json").expect("parses");
        assert_eq!(profile.actor.id, Some("100099999999999".to_string()));
        assert_eq!(profile.external_id.id, "100099999999999");
    }
}
