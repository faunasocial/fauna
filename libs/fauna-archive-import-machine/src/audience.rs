//! The archive's per-record audience onto the three classes `ui/feed.md`
//! owns (`archive-import.md` § What each category becomes → *Audience
//! mapping*). Nothing new is invented here: every imported record lands on
//! public, the free followers tier, or the reserved owner-only tier.

use fauna_archive::ArchiveAudience;

use crate::state::StoredAudienceMode;

/// The three classes `ui/feed.md` owns, as the import maps onto them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostAudience {
    Public,
    Followers,
    OwnerOnly,
}

/// `archive-import.md` § Audience mapping: Public → public; Friends →
/// followers; Custom/OnlyMe/Unknown → owner-only; the OnlyMe mode overrides
/// everything to owner-only.
///
/// `Unknown` is deliberately on the owner-only side: the honest default when
/// an export carries no audience is the most private class, never public.
///
/// A mode this build cannot name (a newer build's, carried in
/// [`StoredAudienceMode::Other`]) maps as `OnlyMe` — the most restrictive
/// mode, never the original audience (`transport.md` § *Rule 3 in full*).
pub fn map_audience(a: ArchiveAudience, mode: &StoredAudienceMode) -> PostAudience {
    match mode {
        StoredAudienceMode::Original => {}
        StoredAudienceMode::OnlyMe | StoredAudienceMode::Other(_) => {
            return PostAudience::OwnerOnly;
        }
    }
    match a {
        ArchiveAudience::Public => PostAudience::Public,
        ArchiveAudience::Friends => PostAudience::Followers,
        ArchiveAudience::Custom | ArchiveAudience::OnlyMe | ArchiveAudience::Unknown => {
            PostAudience::OwnerOnly
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_archive::ArchiveAudience as A;

    #[test]
    fn the_mapping_is_the_goal_docs_table() {
        for (a, want) in [
            (A::Public, PostAudience::Public),
            (A::Friends, PostAudience::Followers),
            (A::Custom, PostAudience::OwnerOnly),
            (A::OnlyMe, PostAudience::OwnerOnly),
            (A::Unknown, PostAudience::OwnerOnly),
        ] {
            assert_eq!(
                map_audience(a, &StoredAudienceMode::Original),
                want,
                "{a:?}"
            );
            assert_eq!(
                map_audience(a, &StoredAudienceMode::OnlyMe),
                PostAudience::OwnerOnly,
                "{a:?} under only-me"
            );
        }
    }

    /// A mode a newer build stored decodes inside its scope and maps as
    /// only-me for every archive audience — never the original audience.
    #[test]
    fn an_unknown_stored_mode_maps_as_only_me() {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "snake_case")]
        enum NewerStoredAudienceMode {
            #[allow(dead_code)]
            Original,
            FriendsOfFriends,
        }
        let bytes =
            fauna_cbor::encode_canonical(&vec![NewerStoredAudienceMode::FriendsOfFriends]).unwrap();
        let modes: Vec<StoredAudienceMode> = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(
            modes,
            vec![StoredAudienceMode::Other("friends_of_friends".into())]
        );
        for a in [A::Public, A::Friends, A::Custom, A::OnlyMe, A::Unknown] {
            assert_eq!(map_audience(a, &modes[0]), PostAudience::OwnerOnly, "{a:?}");
        }
        assert_eq!(fauna_cbor::encode_canonical(&modes).unwrap(), bytes);
    }
}
