//! The frozen derived-ID rule (`archive-import.md` § The archive model):
//! `h:<blake3-hex>` over the canonical DAG-CBOR of (platform, kind,
//! created_at, normalized text, sorted media creation instants). Golden-tested
//! — the same archive yields the same IDs forever.
//!
//! **Media enter by creation instant, never by bytes (user ruling 2026-09-08).**
//! Facebook's export request lets the requester pick a media quality (high /
//! medium / low), so the same photo's bytes legitimately differ between two
//! exports of one account; an ID over media bytes would re-import every photo
//! post as a duplicate on the next export. A media item's creation instant is
//! what the platform recorded when the photo was uploaded, survives every
//! re-export, and still tells two text-less posts in the same second apart.
//! Ruled before any archive was ever imported — the last moment the frozen
//! rule could move; the goldens below were re-pinned in that same commit.

use serde::Serialize;

use crate::model::{EntityKind, ExternalId, MediaInstants, Platform, Timestamp};
use crate::text::normalize_text;

/// The prefix marking a derived (hash) ID as opposed to a platform-native one.
pub const DERIVED_PREFIX: &str = "h:";

/// The hashed shape. FROZEN: field names, order and value forms are part of
/// the ID rule; `platform`/`kind` are the snake_case tokens, `created_at` is
/// microseconds, `text` is `normalize_text`'s output, `media` is the sorted
/// set of media creation instants (microseconds) as a list.
#[derive(Serialize)]
struct IdInput<'a> {
    platform: &'a str,
    kind: &'a str,
    created_at: u64,
    text: String,
    media: Vec<u64>,
}

impl ExternalId {
    /// An ID the platform itself assigned (a permalink, a thread directory,
    /// a numeric user ID) — used verbatim.
    pub fn native(platform: Platform, kind: EntityKind, id: &str) -> ExternalId {
        ExternalId {
            platform,
            kind,
            id: id.to_string(),
        }
    }

    /// The frozen derivation for records the export does not identify.
    pub fn derive(
        platform: Platform,
        kind: EntityKind,
        created_at: Timestamp,
        text: &str,
        media: &MediaInstants,
    ) -> ExternalId {
        let input = IdInput {
            platform: platform.token(),
            kind: kind.token(),
            created_at: created_at.0,
            text: normalize_text(text),
            media: media.iter().copied().collect(),
        };
        // The IPLD dag-cbor codec directly — canonical by construction (map
        // keys length-first then bytewise, shortest-form integers), and the
        // very codec Fauna's own encoder wraps, so the bytes hashed here are
        // the ones the golden tests froze.
        let bytes = serde_ipld_dagcbor::to_vec(&input)
            .expect("IdInput has only strings and integers; canonical encoding cannot fail");
        let digest = blake3::hash(&bytes);
        ExternalId {
            platform,
            kind,
            id: format!("{DERIVED_PREFIX}{}", hex::encode(digest.as_bytes())),
        }
    }

    /// `true` for an `h:`-prefixed derived ID.
    pub fn is_derived(&self) -> bool {
        self.id.starts_with(DERIVED_PREFIX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instants(items: &[u64]) -> MediaInstants {
        items.iter().copied().collect()
    }

    #[test]
    fn native_ids_carry_the_platform_value_verbatim() {
        let id = ExternalId::native(Platform::Facebook, EntityKind::Thread, "friendone_abc123");
        assert_eq!(id.id, "friendone_abc123");
        assert!(!id.is_derived());
    }

    #[test]
    fn derived_ids_are_prefixed_and_hex() {
        let id = ExternalId::derive(
            Platform::Facebook,
            EntityKind::Post,
            Timestamp(1_600_000_000_000_000),
            "hello",
            &MediaInstants::new(),
        );
        assert!(id.is_derived());
        assert!(id.id.starts_with(DERIVED_PREFIX));
        assert_eq!(id.id.len(), DERIVED_PREFIX.len() + 64);
        assert!(
            id.id[DERIVED_PREFIX.len()..]
                .chars()
                .all(|c| c.is_ascii_hexdigit())
        );
    }

    #[test]
    fn derivation_ignores_whitespace_noise_and_media_order() {
        let a = ExternalId::derive(
            Platform::Facebook,
            EntityKind::Post,
            Timestamp(1),
            "  hello   world ",
            &instants(&[20, 10]),
        );
        let b = ExternalId::derive(
            Platform::Facebook,
            EntityKind::Post,
            Timestamp(1),
            "hello world",
            &instants(&[10, 20]),
        );
        assert_eq!(a, b);
    }

    #[test]
    fn every_input_field_changes_the_id() {
        let base = ExternalId::derive(
            Platform::Facebook,
            EntityKind::Post,
            Timestamp(1),
            "t",
            &instants(&[10]),
        );
        assert_ne!(
            base,
            ExternalId::derive(
                Platform::Instagram,
                EntityKind::Post,
                Timestamp(1),
                "t",
                &instants(&[10])
            )
        );
        assert_ne!(
            base,
            ExternalId::derive(
                Platform::Facebook,
                EntityKind::Comment,
                Timestamp(1),
                "t",
                &instants(&[10])
            )
        );
        assert_ne!(
            base,
            ExternalId::derive(
                Platform::Facebook,
                EntityKind::Post,
                Timestamp(2),
                "t",
                &instants(&[10])
            )
        );
        assert_ne!(
            base,
            ExternalId::derive(
                Platform::Facebook,
                EntityKind::Post,
                Timestamp(1),
                "u",
                &instants(&[10])
            )
        );
        assert_ne!(
            base,
            ExternalId::derive(
                Platform::Facebook,
                EntityKind::Post,
                Timestamp(1),
                "t",
                &instants(&[11])
            )
        );
    }

    /// An empty media list encodes as the same CBOR whatever its element
    /// type, so every text-only ID pinned before the 2026-09-08 rule change
    /// is unchanged by it — the property that lets the comment / reaction /
    /// event / group / album goldens in `tests/facebook_golden.rs` stand.
    #[test]
    fn a_text_only_id_is_unchanged_by_the_media_rule() {
        let id = ExternalId::derive(
            Platform::Facebook,
            EntityKind::Post,
            Timestamp(1_600_000_000_000_000),
            "Dobrý den, přátelé",
            &MediaInstants::new(),
        );
        assert_eq!(
            id.id,
            "h:7994c384e40458fbb61f4d8791446e63a5a4797785a7ac74a0c119637a87572b"
        );
    }

    /// GOLDEN — the frozen rule for a post WITH media, re-pinned 2026-09-08
    /// when media instants replaced media bytes (module doc). Never update
    /// this value to make a test pass: a change here means every previously
    /// imported archive would re-import as duplicates.
    #[test]
    fn golden_post_id_is_frozen() {
        let id = ExternalId::derive(
            Platform::Facebook,
            EntityKind::Post,
            Timestamp(1_600_000_000_000_000),
            "Dobrý den, přátelé",
            &instants(&[1_609_999_000_000_000]),
        );
        assert_eq!(
            id.id,
            "h:fa428081dec7f444795a0d591b74efbf651ce10a228babb79a9d9f0b9ef417a4"
        );
    }
}
