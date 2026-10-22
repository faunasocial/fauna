//! Wire types for `fauna.profile.author.set` — the oracle's profile door
//! (TP11, `docs/goal/architecture/key-material-hierarchy.md` § Audience:
//! deployment infrastructure → *The oracle*, the `profile.author` row).
//!
//! Principal-only: a third-party principal holding a live
//! `identity.op:profile.author` grant hands the account's authoring-key
//! custodian the display fields it wants changed; the custodian reads the
//! account's stored profile, applies them, carries every other field
//! unchanged, signs the result with the account's delegated authoring sub-key
//! `K` under `Capability::UpdateProfile`
//! (`docs/goal/behavior/atproto-pds-full.md` § D10) and ingests it through
//! the one profile write door `fauna.profile.set` runs.
//!
//! **Every field is optional, and absent means unchanged** — a principal
//! editing the bio does not have to echo the name. A present `display_name`
//! or `bio` sets it, the empty string clearing it; present `links` replace
//! the list whole, an empty list clearing it. `avatar` and `banner` name a
//! media item the account already holds, which no principal can put yet, so
//! the custodian refuses a request naming either
//! (`fauna.profile.author.media_unavailable`) until the media plane's arm
//! lets it. A request carrying ANY member this build does not name is
//! refused rather than dropped: a newer field the principal meant to set
//! would otherwise be reported as written when it was not.

use std::collections::BTreeMap;

use fauna_core::data::ProfileLink;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use crate::Value;

/// The set kind.
pub const KIND_SET: &str = "fauna.profile.author.set";

/// Request for [`KIND_SET`]: the display fields to change.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProfileAuthorSetRequest {
    /// The display name; `""` clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The bio; `""` clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    /// The links, replacing the stored list whole; `[]` clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<ProfileLink>>,
    /// The profile picture's media item (a content hash, hex) — refused
    /// today (module doc).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// The banner's media item (a content hash, hex) — refused today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<String>,
    /// Members this build does not name. The custodian REFUSES a request
    /// where this is non-empty (module doc).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for [`KIND_SET`]: the profile now stored, as `fauna.profile.get`
/// would serve it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileAuthorSetReply {
    /// The stored signed `Profile` wire, verbatim — decoded client-side
    /// exactly as `ProfileGetReply::body` is.
    pub body: ByteBuf,
    /// Forward-compat catch-all.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict, encode_canonical};

    #[test]
    fn a_set_round_trips_absent_fields_stay_absent_and_an_unknown_member_lands_in_extra() {
        let req = ProfileAuthorSetRequest {
            bio: Some("hello".into()),
            links: Some(vec![ProfileLink {
                label: "site".into(),
                uri: "https://example.org".into(),
            }]),
            ..Default::default()
        };
        let back: ProfileAuthorSetRequest =
            decode_strict(&encode_canonical(&req).unwrap()).unwrap();
        assert_eq!(back, req);
        assert!(back.display_name.is_none(), "absent stays absent");

        // A member this build does not name is carried in `extra`, where the
        // custodian sees it and refuses — never dropped silently.
        let mut newer = req.clone();
        newer
            .extra
            .insert("pronouns".into(), Value::String("x".into()));
        let back: ProfileAuthorSetRequest =
            decode_strict(&encode_canonical(&newer).unwrap()).unwrap();
        assert!(back.extra.contains_key("pronouns"));
    }
}
