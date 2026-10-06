//! Core data types for the Fauna protocol.
//!
//! Posts, profiles, tombstones, and their supporting types.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::carried::CarriedValue;
use crate::identity::ActorId;
use crate::localized::LocalizedText;
use crate::secret::{SecretArray32, SecretByteBuf, SecretString};
use crate::subscription::types::GatedInfo;

// Prevent silent runtime panic: WASM builds must enable the `js` feature.
#[cfg(all(target_arch = "wasm32", not(feature = "js")))]
compile_error!("fauna-core on wasm32 requires the `js` feature for Timestamp::now()");

/// Raw-codec CID used for content addressing of opaque byte streams
/// (blobs, chunks, video segments, file manifests, web content, …) —
/// payloads whose canonical form IS the byte sequence itself, not a
/// dag-cbor encoding.
///
/// Layer 3 Task 3.8 of the CBOR-DAG-everywhere plan collapsed
/// `ContentHash` into [`fauna_cbor::Cid`] with the raw codec
/// (0x55). The alias preserves the readable "content hash" name at
/// call sites (`blob_hash`, `file_hash`, `chunk_hash`, …) while the
/// underlying machinery is the codec-parametric Cid from Task 3.7.
///
/// Construct with [`Cid::of_raw`] (when the bytes are available) or
/// [`Cid::from_digest_raw`] (when a 32-byte BLAKE3 digest is held
/// separately, e.g. read from a SQLite column). Extract the 32-byte
/// digest with [`Cid::digest`] when crossing a 32-byte boundary
/// (SQLite BLOB column, FFI bindings, hex display).
///
/// Wire-format note: a raw-codec `Cid` is 36 bytes on the wire
/// (v1 + raw + blake3-256 + 32 digest); the previous `ContentHash`
/// shape was 32 bytes. Kinds that embed this in a serialized wire
/// shape gained 4 bytes per field; pre-production no on-disk
/// migration is needed.
pub type ContentHash = fauna_cbor::Cid;

/// Unique identifier for a post: the canonical-dag-cbor CID of the
/// signed `Post` value.
///
/// `PostId` is now an alias for [`fauna_cbor::Cid`] (36 bytes:
/// v1 + dag-cbor codec + blake3-256 multihash + 32-byte digest). The
/// alias is retained for kind-named clarity at API boundaries; new
/// code may use `Cid` directly. The previous `PostId([u8; 32])`
/// shape is gone — wire bytes are now 36, the user-facing form is
/// multibase-b base32 lowercase (`Cid::to_base32`).
pub type PostId = fauna_cbor::Cid;

/// Microseconds since Unix epoch (UTC). `Default` is `Timestamp(0)` — the
/// epoch, read everywhere as "absent / maximally stale", never as a real
/// moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub fn now() -> Self {
        #[cfg(all(target_arch = "wasm32", feature = "js"))]
        {
            let millis = js_sys::Date::now();
            Self((millis * 1000.0) as u64)
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "js")))]
        {
            use std::time::{SystemTime, UNIX_EPOCH};
            let micros = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before Unix epoch")
                .as_micros() as u64;
            Self(micros)
        }
    }

    /// Microseconds value as i64 (for SQLite columns that use signed integers).
    pub fn as_i64(self) -> i64 {
        self.0 as i64
    }

    /// The `Default` — the epoch, read everywhere as "absent / maximally
    /// stale". Shaped for `#[serde(skip_serializing_if = …)]`, so an additive
    /// stamp field that was never written stays off the wire.
    pub fn is_epoch(&self) -> bool {
        self.0 == 0
    }

    /// Current time as milliseconds since epoch.
    /// Useful for auth tokens where the wire format uses milliseconds.
    pub fn now_millis() -> u64 {
        #[cfg(all(target_arch = "wasm32", feature = "js"))]
        {
            js_sys::Date::now() as u64
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "js")))]
        {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before Unix epoch")
                .as_millis() as u64
        }
    }

    /// Current time as seconds since epoch.
    /// Useful for DKIM/domain signing where the wire format uses seconds.
    pub fn now_secs() -> i64 {
        #[cfg(all(target_arch = "wasm32", feature = "js"))]
        {
            (js_sys::Date::now() / 1000.0) as i64
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "js")))]
        {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before Unix epoch")
                .as_secs() as i64
        }
    }

    /// Same as [`Timestamp::now`], but falls back to the Unix epoch (`0`)
    /// instead of panicking if the system clock reads before it — on every
    /// target, wasm included: the wasm branch's cast is to `u64`, which
    /// self-saturates a negative reading to `0` rather than needing an
    /// explicit clamp (Rust's `as` float→int cast semantics; unlike
    /// [`Timestamp::now_secs_or_zero`], whose `i64` result does not).
    /// **Direction matters — this fallback is safe in one comparison shape
    /// and dangerous in the other.** Folding to epoch-0 is conservative for
    /// a *past*-timestamp comparison (an age check, `now - created > ttl`):
    /// epoch-0 reads as "not old enough yet". It is dangerous for a
    /// *future* absolute deadline (`deadline > now`): epoch-0 reads as "not
    /// expired", forever, silently treating an unreadable clock as
    /// permanent validity. **Not a freshness/expiry cache's fallback** — a
    /// cache that cannot panic on a clock failure wants a fallible read
    /// whose failure it can direct itself (`Option`, treated as
    /// unconditionally stale), not this fold. Prefer [`Timestamp::now`] for
    /// everything else.
    pub fn now_or_zero() -> Self {
        #[cfg(all(target_arch = "wasm32", feature = "js"))]
        {
            Self::now()
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "js")))]
        {
            use std::time::{SystemTime, UNIX_EPOCH};
            let micros = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_micros() as u64)
                .unwrap_or(0);
            Self(micros)
        }
    }

    /// Same as [`Timestamp::now_millis`], but falls back to `0` instead of
    /// panicking if the system clock reads before the Unix epoch — on every
    /// target, wasm included: the wasm branch's cast is to `u64`, which
    /// self-saturates rather than needing an explicit clamp (see
    /// [`Timestamp::now_or_zero`]'s doc). See [`Timestamp::now_or_zero`] for
    /// when this is the right choice.
    pub fn now_millis_or_zero() -> u64 {
        #[cfg(all(target_arch = "wasm32", feature = "js"))]
        {
            Self::now_millis()
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "js")))]
        {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)
        }
    }

    /// The floor [`Timestamp::now_secs_or_zero`]'s wasm branch applies to a
    /// raw `js_sys::Date::now()` millisecond reading — pure and `cfg`-free
    /// (gated only to compile where it is actually reachable: the real wasm
    /// call site, or a native test) so the floor can be pinned by a native
    /// test even though production only ever calls it under `wasm32` + `js`.
    ///
    /// **This is the one `_or_zero` sibling whose floor was accidental until
    /// now.** [`Timestamp::now_or_zero`] and [`Timestamp::now_millis_or_zero`]
    /// both return `u64`, so their `as u64` cast already saturates a negative
    /// reading to `0` — no clamp needed. `now_secs_or_zero` returns `i64`,
    /// which lets a pre-epoch clock's negative value survive the cast
    /// unchanged, breaking the contract this function's own name and doc
    /// promise.
    #[cfg(any(all(target_arch = "wasm32", feature = "js"), test))]
    fn secs_or_zero_from_js_millis(millis: f64) -> i64 {
        ((millis / 1000.0) as i64).max(0)
    }

    /// Same as [`Timestamp::now_secs`], but falls back to `0` instead of
    /// panicking if the system clock reads before the Unix epoch — on every
    /// target, wasm included ([`Timestamp::secs_or_zero_from_js_millis`]).
    /// See [`Timestamp::now_or_zero`] for when this is the right choice.
    pub fn now_secs_or_zero() -> i64 {
        #[cfg(all(target_arch = "wasm32", feature = "js"))]
        {
            Self::secs_or_zero_from_js_millis(js_sys::Date::now())
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "js")))]
        {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        }
    }

    /// Convert an arbitrary `SystemTime` (e.g. a file's mtime — not the
    /// current clock reading) to seconds since the Unix epoch, falling back
    /// to `0` if it predates the epoch. See [`Timestamp::now_secs_or_zero`]
    /// for the "read the clock now" flavor of the same fallback.
    pub fn secs_or_zero(t: std::time::SystemTime) -> i64 {
        t.duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    /// Same as [`Timestamp::secs_or_zero`], but milliseconds.
    pub fn millis_or_zero(t: std::time::SystemTime) -> i64 {
        t.duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod timestamp_conversion_tests {
    use super::Timestamp;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn secs_or_zero_converts_a_supplied_instant() {
        let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(Timestamp::secs_or_zero(t), 1_700_000_000);
    }

    #[test]
    fn secs_or_zero_falls_back_to_zero_before_the_epoch() {
        let t = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(Timestamp::secs_or_zero(t), 0);
    }

    #[test]
    fn millis_or_zero_converts_a_supplied_instant() {
        let t = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        assert_eq!(Timestamp::millis_or_zero(t), 1_700_000_000_123);
    }

    /// Pin for
    /// `now_secs_or_zero`'s wasm branch: a pre-epoch `js_sys::Date::now()`
    /// millisecond reading must floor at `0`, not survive as a negative
    /// `i64`. Native-testable because the conversion is factored into a
    /// pure, `cfg`-free-under-test helper — the wasm call site itself
    /// cannot be exercised without a browser clock nothing here can force
    /// negative.
    #[test]
    fn secs_or_zero_from_js_millis_floors_a_pre_epoch_reading_at_zero() {
        assert_eq!(Timestamp::secs_or_zero_from_js_millis(-5_000.0), 0);
    }

    #[test]
    fn secs_or_zero_from_js_millis_converts_a_post_epoch_reading() {
        assert_eq!(
            Timestamp::secs_or_zero_from_js_millis(1_700_000_000_123.0),
            1_700_000_000,
        );
    }

    #[test]
    fn millis_or_zero_falls_back_to_zero_before_the_epoch() {
        let t = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(Timestamp::millis_or_zero(t), 0);
    }

    #[test]
    fn secs_or_zero_matches_now_secs_or_zero_for_the_current_instant() {
        // Not a flakiness risk: both read the same underlying clock, and
        // the two calls happen microseconds apart at worst.
        let now_secs = Timestamp::now_secs_or_zero();
        let via_conversion = Timestamp::secs_or_zero(SystemTime::now());
        assert!((via_conversion - now_secs).abs() <= 1);
    }
}

/// Where a post **originated** when that is not Fauna — set on every post an
/// archive import re-authors (`docs/goal/behavior/archive-import.md` § What
/// each category becomes → *The post origin field*). Plain strings; an
/// older peer tolerates the unknown key on decode and stores and forwards the
/// content-addressed bytes verbatim. A field of the signed `Post` envelope —
/// plaintext on every post, gated or not, like `gated` and `content_warning`
/// (a gated post seals only its `PostBody`) — which is what lets the nest
/// index `source` from it ([`Post::source_token`]) for gated imports too.
///
/// **The envelope identifies the platform and nothing that identifies the
/// post on the other network** (ruling 2, 2026-09-06): there is deliberately
/// no `external_id` here — dedup, the phase-two origin index and the
/// collapsed-comment join all key on the sealed archive folder's
/// `state/import.cbor` map, and an `h:<blake3>` id on a gated post would be a
/// content-confirmation oracle over the sealed body. `url` stays optional
/// and phase one authors it `None` on every post.
///
/// Absent on every post authored in Fauna itself: the field is
/// `skip_serializing_if`, so such a post encodes byte-identically to one from
/// before the field existed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostOrigin {
    /// The platform token — one of [`crate::source::ARCHIVE_PLATFORMS`] in
    /// practice, normalized by [`crate::source::normalize`] when indexed.
    pub platform: String,
    /// The original permalink. Unset in phase one (a permalink names the
    /// user's handle on the other network on every card); a later per-import
    /// "link to originals" choice may set it on public posts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// A signed post — the fundamental content unit.
///
/// Signed via `fauna_cbor::SignedEnvelope` (sign-over-CID) per
/// `docs/goal/architecture/serialization.md`. The signature does NOT live on
/// the struct — sign() returns `(canonical_bytes, SignedEnvelope)` and the
/// envelope ships alongside the bytes in the embed-as-bytes wire shape
/// (`docs/goal/architecture/serialization.md` § Embed-as-bytes for signed payloads).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Post {
    pub author: ActorId,
    pub created_at: Timestamp,
    pub body: PostBody,
    pub references: Vec<Reference>,
    pub expires_at: Option<Timestamp>,
    /// Gated content info. When Some, `body` is the preview and the full
    /// content is encrypted at `encrypted_ref`. When None, this is a normal post.
    #[serde(default)]
    pub gated: Option<GatedInfo>,
    /// Content warning / spoiler text (maps to ActivityPub `summary`).
    #[serde(default)]
    pub content_warning: Option<String>,
    /// Where the post originated when that is not Fauna — see [`PostOrigin`].
    /// Additive like `gated` / `content_warning`, and additionally
    /// `skip_serializing_if`: an origin-less post's bytes are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<PostOrigin>,
}

impl Post {
    /// The `source` token the nest indexes for this post — the normalized
    /// [`PostOrigin::platform`] when it is one of [`crate::source::ARCHIVE_PLATFORMS`],
    /// else [`crate::source::NATIVE`]. Shared by the nest's index derivation
    /// (`extract_post_metadata`) so no second walk can drift from it. The
    /// vocabulary is closed (`archive-import.md` § What each category becomes
    /// → *The post origin field*: only a known archive platform is ever
    /// indexed): a client cannot make a nest index its own
    /// signed post under a bridge token (`email`/`bluesky`/`nostr`/
    /// `activitypub`) and route its interactions into a bridge arm or spoof
    /// the badge — a platform this build does not know falls back to `fauna`
    /// exactly like an older nest does.
    pub fn source_token(&self) -> String {
        self.origin
            .as_ref()
            .and_then(|o| crate::source::normalize(&o.platform))
            .filter(|t| crate::source::is_native(t))
            .unwrap_or_else(|| crate::source::NATIVE.to_string())
    }

    /// Decode a `Post` from its **resolved wire/storage bytes** — the payload a
    /// nest stores in its `content` table and serves verbatim on
    /// `fauna.posts.get` (`PostGetReply.body`). The bytes are either the
    /// **embed-as-bytes** wire shape (signed posts) or a **bare** canonical
    /// `Post` (bridge-translated unsigned posts); this tries the former
    /// (envelope + inner signed bytes), then falls back to the latter. Returns
    /// `None` if the bytes are neither. The single decode path every reader —
    /// nest-side (storage, moderation train, ActivityPub, video routes) and
    /// client-side (the tier-1 spam-model train-text fetch) — shares, so the
    /// two ends can never drift on the accepted shapes.
    pub fn decode_resolved_bytes(data: &[u8]) -> Option<Post> {
        crate::encoding::canonical_decode::<crate::encoding::EmbedAsBytes>(data)
            .ok()
            .and_then(|wire| crate::encoding::decode_signed_bytes::<Post>(&wire.bytes).ok())
            .or_else(|| crate::encoding::canonical_decode::<Post>(data).ok())
    }

    /// The post's plain-text content — the searchable/trainable text of its
    /// body variant (FTS indexing, the Bayesian spam train). Media-only posts
    /// yield their alt text; video posts yield the empty string. Shared by the
    /// nest indexer/train handler and the client-side tier-1 model write so a
    /// client-path train is byte-identical with a nest-path train on the same
    /// post (`docs/goal/behavior/mail-spam.md` § Encrypted-mode interaction).
    /// Also the source `libs/fauna-feed::quote`'s embedded-quote projection
    /// truncates — that module hand-rolled an identical match before
    /// 2026-08-23 (priorities #1/#2).
    pub fn body_text(&self) -> String {
        self.body.text()
    }

    /// The post's tag names, read back out of its body facets — the inverse of
    /// the `tags_to_facets` encoding every app's post-builder uses
    /// (`fauna_client_core::post::build_post`). Tags are not a `Post` field:
    /// they ride as [`FacetFeature::Tag`] facets on whichever body variant
    /// carries facets, so a reader that wants them (the trainable-text
    /// composition for a topic factor; any client rendering a decoded post's
    /// hashtags) would otherwise re-walk the facets by hand and drift on which
    /// variants it remembered to cover. Order is the facet order; a body variant
    /// with no facets yields an empty vec.
    pub fn tags(&self) -> Vec<String> {
        let facets = match &self.body {
            PostBody::Text { facets, .. } => facets,
            PostBody::TextWithMedia { facets, .. } => facets,
            PostBody::Structured { facets, .. } => facets,
            PostBody::Media { .. } | PostBody::Video { .. } => return Vec::new(),
        };
        facets
            .iter()
            .filter_map(|f| match &f.feature {
                FacetFeature::Tag { name } => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// The post's tags **as the feed index stores them** — [`tags`](Self::tags)
    /// lowercased. That is the spelling the nest writes into `content_links`,
    /// hence the spelling `FeedPostItem.tags` carries, hence what a client's
    /// `PostSummary.tags` holds for a post served by the feed query. A client
    /// that projects a post it fetched *by id* instead (`fauna.posts.get` +
    /// decode — the deep-link path, `libs/fauna-feed`'s `map_fetched_post`)
    /// must use this rather than [`tags`](Self::tags), or the same post's
    /// hashtags read differently depending on which door opened it.
    pub fn indexed_tags(&self) -> Vec<String> {
        self.tags().iter().map(|t| t.to_lowercase()).collect()
    }

    /// Whether the post carries media — the `content_meta.has_media` predicate,
    /// shared so the nest indexer and a client-side single-post projection can
    /// never disagree on which body variants count. A `Structured` body counts
    /// only when it actually carries items.
    pub fn has_media(&self) -> bool {
        self.body.has_media()
    }

    /// Whether the post replies to another — the `content_meta.is_reply`
    /// predicate, the [`has_media`](Self::has_media) twin.
    pub fn is_reply(&self) -> bool {
        self.references
            .iter()
            .any(|r| matches!(r, Reference::Reply { .. }))
    }

    /// The 32-byte content id of the first quoted post, stripped from the full
    /// 36-byte CID the reference carries — the id `content_links
    /// link_type='quote'` is keyed on, and (hex-encoded) what
    /// `PostSummary.quoted_post_id` holds. A post quotes at most one post in the
    /// ratified wire shape. `None` for a non-quoting post, and for a malformed
    /// reference whose CID is too short to carry a digest.
    pub fn quoted_post_id(&self) -> Option<[u8; 32]> {
        self.references.iter().find_map(|r| match r {
            Reference::Quote { post_id } => post_id
                .as_bytes()
                .get(4..36)
                .and_then(|d| d.try_into().ok()),
            _ => None,
        })
    }

    /// The 32-byte content id of the first *reposted* post — the
    /// [`Reference::Repost`] twin of [`quoted_post_id`](Self::quoted_post_id),
    /// keying the `content_links link_type='repost'` projection and
    /// (hex-encoded) what `PostSummary.reposted_post_id` holds
    /// (`ui/feed.md` § Interaction bar → Repost, ratified 2026-08-10). Shared
    /// so the nest indexer and the client decode paths cannot drift on how the
    /// CID is stripped. `None` for a non-reposting post, and for a malformed
    /// reference whose CID is too short to carry a digest.
    pub fn reposted_post_id(&self) -> Option<[u8; 32]> {
        self.references.iter().find_map(|r| match r {
            Reference::Repost { post_id } => post_id
                .as_bytes()
                .get(4..36)
                .and_then(|d| d.try_into().ok()),
            _ => None,
        })
    }

    /// Every blob-store content address a nest can see from this post's
    /// envelope: the plaintext body's [`PostBody::blob_refs`] (a public post's
    /// media; a gated post's preview media) plus, for a gated post, the sealed
    /// body blob `encrypted_ref` and the plaintext `attachment_refs`. This is
    /// exactly what the nest's blob GC pins for a live post — never the
    /// hashes inside a sealed body, which the nest cannot enumerate and which
    /// `attachment_refs` exists to carry (`encryption-at-rest.md` § Plaintext
    /// floor → Posts row, ratified 2026-09-08).
    pub fn blob_refs(&self) -> Vec<ContentHash> {
        let mut out = self.body.blob_refs();
        if let Some(gated) = &self.gated {
            out.push(gated.encrypted_ref);
            out.extend(gated.attachment_refs.iter().copied());
        }
        out
    }
}

/// The remote origin a proxied media path wraps — the inverse of
/// [`shared_media_proxy_url`], and of Bluesky's `bluesky/media` rewrite, which
/// carries its origin in the same `url` query parameter. For a writer that must
/// hand the media to a remote server (an outbound ActivityPub attachment), where a
/// nest-relative path means nothing. `None` for anything that is not a
/// nest-relative path carrying an `https` `url` parameter.
pub fn proxied_media_origin(path: &str) -> Option<String> {
    let path = path.trim();
    if !path.starts_with('/') || path.starts_with("//") {
        return None;
    }
    let (_, query) = path.split_once('?')?;
    let encoded = query.split('&').find_map(|kv| kv.strip_prefix("url="))?;
    let origin = urlencoding::decode(encoded).ok()?.into_owned();
    origin.starts_with("https://").then_some(origin)
}

#[cfg(test)]
mod shared_media_proxy_url_tests {
    use super::{proxied_media_origin, shared_media_proxy_url};

    #[test]
    fn proxied_media_origin_inverts_both_proxy_forms() {
        let origin = "https://r.example/a b.png?x=1&y=2";
        let path = shared_media_proxy_url(origin).unwrap();
        assert_eq!(proxied_media_origin(&path).as_deref(), Some(origin));
        assert_eq!(
            proxied_media_origin("/api/v1/bluesky/media?url=https%3A%2F%2Fcdn.bsky.app%2Fa.jpg")
                .as_deref(),
            Some("https://cdn.bsky.app/a.jpg")
        );
        assert!(proxied_media_origin("https://r.example/a.png").is_none());
        assert!(proxied_media_origin("/api/v1/media/proxy").is_none());
        assert!(proxied_media_origin("/api/v1/media/proxy?url=http%3A%2F%2Fx").is_none());
    }

    #[test]
    fn shared_media_proxy_url_wraps_https_only() {
        assert_eq!(
            shared_media_proxy_url("https://r.example/a.png").as_deref(),
            Some("/api/v1/media/proxy?url=https%3A%2F%2Fr.example%2Fa.png")
        );
        assert!(shared_media_proxy_url("http://r.example/a.png").is_none());
        assert!(shared_media_proxy_url("  ").is_none());
    }
}

#[cfg(test)]
mod post_index_facts_tests {
    use super::{
        ActorId, ContentHash, Facet, FacetFeature, MediaItem, Post, PostBody, Reference, Timestamp,
    };

    fn post_with(body: PostBody, references: Vec<Reference>) -> Post {
        Post {
            author: ActorId([7u8; 32]),
            created_at: Timestamp(0),
            body,
            references,
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        }
    }

    fn tag_facet(name: &str) -> Facet {
        Facet {
            byte_start: 0,
            byte_end: 0,
            feature: FacetFeature::Tag { name: name.into() },
        }
    }

    fn media_item() -> MediaItem {
        MediaItem {
            blob_hash: ContentHash::of_raw(b"blob"),
            media_type: "image/png".into(),
            size_bytes: 1,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        }
    }

    /// `MediaItem.alt` is skipped when `None`: an item with no alt encodes
    /// without the key, and one with an alt round-trips.
    #[test]
    fn media_item_alt_is_omitted_when_none() {
        use crate::encoding::{canonical_decode, canonical_encode};

        #[derive(serde::Serialize)]
        struct AltlessMediaItem {
            blob_hash: ContentHash,
            media_type: String,
            size_bytes: u64,
            dimensions: Option<crate::data::Dimensions>,
            thumbnail: Option<ContentHash>,
            remote_url: Option<String>,
        }
        let altless = AltlessMediaItem {
            blob_hash: ContentHash::of_raw(b"blob"),
            media_type: "image/png".into(),
            size_bytes: 1,
            dimensions: None,
            thumbnail: None,
            remote_url: None,
        };
        assert_eq!(
            canonical_encode(&media_item()).unwrap(),
            canonical_encode(&altless).unwrap(),
            "an item with no alt encodes without the key"
        );

        let described = MediaItem {
            alt: Some("a cat".into()),
            ..media_item()
        };
        let back: MediaItem =
            canonical_decode(canonical_encode(&described).unwrap().as_ref()).unwrap();
        assert_eq!(back.alt.as_deref(), Some("a cat"));
    }

    /// The nest's blob GC pins exactly this set for a live post, so every
    /// hash-typed field a body or its gate can carry must be listed — a
    /// variant this forgets is a photo the sweep deletes.
    #[test]
    fn blob_refs_names_every_blob_a_body_and_its_gate_carry() {
        use super::VideoSegment;
        use crate::subscription::types::{GatedInfo, KeyAccess};

        let thumb = ContentHash::of_raw(b"thumb");
        let mut item = media_item();
        item.thumbnail = Some(thumb);
        let public = post_with(
            PostBody::TextWithMedia {
                content: "photo".into(),
                facets: vec![],
                items: vec![item.clone(), media_item()],
            },
            vec![],
        );
        assert_eq!(
            public.blob_refs(),
            vec![item.blob_hash, thumb, item.blob_hash],
            "a media item's blob and thumbnail, in body order, duplicates kept"
        );

        let manifest = ContentHash::of_raw(b"manifest");
        let poster = ContentHash::of_raw(b"poster");
        let seg = ContentHash::of_raw(b"seg-0");
        let video = post_with(
            PostBody::Video {
                manifest,
                segments: vec![VideoSegment {
                    hash: seg,
                    resolution: 720,
                    codec: "avc1".into(),
                    bitrate: 1000,
                    byte_size: 1,
                }],
                thumbnail: poster,
                duration_ms: 1,
                aspect_ratio: (16, 9),
                anchors: vec![],
            },
            vec![],
        );
        assert_eq!(video.blob_refs(), vec![manifest, poster, seg]);

        let sealed_body = ContentHash::of_raw(b"sealed body");
        let sealed_photo = ContentHash::of_raw(b"sealed photo");
        let mut gated = post_with(
            PostBody::Text {
                content: "teaser".into(),
                facets: vec![],
            },
            vec![],
        );
        gated.gated = Some(GatedInfo {
            encrypted_ref: sealed_body,
            key_access: KeyAccess::Broadcast {
                key_blob_ref: ContentHash::of_raw(b"key blob"),
            },
            tier: "gold".into(),
            tier_rank: 1,
            seal_id: crate::data::ContentHash::from_digest_raw([0x5e; 32]),
            attachment_refs: vec![sealed_photo],
        });
        assert_eq!(
            gated.blob_refs(),
            vec![sealed_body, sealed_photo],
            "a gated post pins its sealed body and its plaintext attachment list; the \
             KeyBlob is a DB row, not a blob, and is not listed"
        );
        assert!(
            PostBody::Text {
                content: "text".into(),
                facets: vec![]
            }
            .blob_refs()
            .is_empty()
        );
    }

    /// The feed index lowercases; a client projecting the same post from its
    /// decoded bytes must read the same spelling back.
    #[test]
    fn indexed_tags_lowercase_what_tags_returns_verbatim() {
        let p = post_with(
            PostBody::Text {
                content: "hi".into(),
                facets: vec![tag_facet("Rust"), tag_facet("FAUNA")],
            },
            vec![],
        );
        assert_eq!(p.tags(), vec!["Rust".to_string(), "FAUNA".to_string()]);
        assert_eq!(
            p.indexed_tags(),
            vec!["rust".to_string(), "fauna".to_string()]
        );
    }

    #[test]
    fn has_media_covers_every_body_variant_that_carries_items() {
        assert!(
            !post_with(
                PostBody::Text {
                    content: "hi".into(),
                    facets: vec![]
                },
                vec![]
            )
            .has_media()
        );
        assert!(
            post_with(
                PostBody::Media {
                    items: vec![media_item()],
                    alt_text: None
                },
                vec![]
            )
            .has_media()
        );
        assert!(
            post_with(
                PostBody::TextWithMedia {
                    content: "hi".into(),
                    facets: vec![],
                    items: vec![media_item()],
                },
                vec![]
            )
            .has_media()
        );
        assert!(
            post_with(
                PostBody::Video {
                    manifest: ContentHash::of_raw(b"m"),
                    segments: vec![],
                    thumbnail: ContentHash::of_raw(b"t"),
                    duration_ms: 1,
                    aspect_ratio: (16, 9),
                    anchors: vec![],
                },
                vec![]
            )
            .has_media()
        );
        // A structured body counts only when it actually carries items.
        let structured = |items: Vec<MediaItem>| {
            post_with(
                PostBody::Structured {
                    schema: "poll".into(),
                    fields: vec![],
                    content: None,
                    facets: vec![],
                    items,
                },
                vec![],
            )
        };
        assert!(!structured(vec![]).has_media());
        assert!(structured(vec![media_item()]).has_media());
    }

    #[test]
    fn is_reply_reads_the_reply_reference_only() {
        let text = || PostBody::Text {
            content: "hi".into(),
            facets: vec![],
        };
        let cid = ContentHash::of_raw(b"target");
        assert!(!post_with(text(), vec![]).is_reply());
        assert!(!post_with(text(), vec![Reference::Quote { post_id: cid }]).is_reply());
        assert!(post_with(text(), vec![Reference::Reply { post_id: cid }]).is_reply());
    }

    /// The 36-byte CID is stripped to the 32-byte digest `content_links` keys on
    /// — the spelling `PostSummary.quoted_post_id` hex-encodes.
    #[test]
    fn quoted_post_id_strips_the_cid_prefix_to_the_digest() {
        let hash = ContentHash::of_raw(b"quoted");
        let text = || PostBody::Text {
            content: "hi".into(),
            facets: vec![],
        };
        assert_eq!(post_with(text(), vec![]).quoted_post_id(), None);
        let quoting = post_with(
            text(),
            vec![
                Reference::Reply { post_id: hash },
                Reference::Quote { post_id: hash },
            ],
        );
        assert_eq!(quoting.quoted_post_id(), Some(hash.digest()));
    }
}

/// The content of a post.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PostBody {
    Text {
        content: String,
        facets: Vec<Facet>,
    },
    Media {
        items: Vec<MediaItem>,
        alt_text: Option<String>,
    },
    TextWithMedia {
        content: String,
        facets: Vec<Facet>,
        items: Vec<MediaItem>,
    },
    Structured {
        schema: String,
        fields: Vec<StructuredField>,
        content: Option<String>,
        facets: Vec<Facet>,
        items: Vec<MediaItem>,
    },
    Video {
        manifest: ContentHash,
        segments: Vec<VideoSegment>,
        thumbnail: ContentHash,
        duration_ms: u64,
        aspect_ratio: (u16, u16),
        anchors: Vec<VerificationAnchor>,
    },
}

impl PostBody {
    /// The body's plain-text content — the searchable/trainable text of this
    /// variant. Media-only bodies yield their alt text; video bodies yield the
    /// empty string.
    ///
    /// On the *body* rather than on [`Post`] because a reader can hold a body
    /// with no envelope around it: a tier-gated post's full `PostBody` arrives
    /// as a sealed blob and is opened on the reader's client
    /// (`docs/goal/ui/feed.md` § Encryption at rest — "unsealing at render time
    /// on the reader's client"), never as part of a signed `Post`. [`Post::body_text`]
    /// delegates here so the envelope path and the unseal path can never come
    /// to disagree about which variants carry text.
    pub fn text(&self) -> String {
        match self {
            PostBody::Text { content, .. } => content.clone(),
            PostBody::TextWithMedia { content, .. } => content.clone(),
            PostBody::Structured { content, .. } => content.clone().unwrap_or_default(),
            PostBody::Media { alt_text, .. } => alt_text.clone().unwrap_or_default(),
            PostBody::Video { .. } => String::new(),
        }
    }

    /// Whether this body carries media — the `content_meta.has_media` predicate.
    /// A `Structured` body counts only when it actually carries items.
    ///
    /// On the body for [`text`](Self::text)'s reason: the unsealed body of a
    /// gated post is the authority for that post's media, and it reaches the
    /// feed's read model without an envelope. [`Post::has_media`] delegates here.
    pub fn has_media(&self) -> bool {
        match self {
            PostBody::Media { .. } | PostBody::TextWithMedia { .. } | PostBody::Video { .. } => {
                true
            }
            PostBody::Structured { items, .. } => !items.is_empty(),
            PostBody::Text { .. } => false,
        }
    }

    /// This body's media attachments, in body order — empty for a variant that
    /// carries none. The one home for "which variants hold a `Vec<MediaItem>`",
    /// so a reader walking a body's attachments (the feed's render fold, the
    /// gated-media unseal registration) never re-spells that match.
    pub fn media_items(&self) -> &[MediaItem] {
        match self {
            PostBody::Media { items, .. }
            | PostBody::TextWithMedia { items, .. }
            | PostBody::Structured { items, .. } => items.as_slice(),
            PostBody::Text { .. } | PostBody::Video { .. } => &[],
        }
    }

    /// Every blob-store content address this body names — each media item's
    /// `blob_hash` and `thumbnail`, and a video's manifest, thumbnail and
    /// segment hashes — in body order, duplicates included. The one home for
    /// "which blobs does a body reference": the nest's blob GC pins them so a
    /// live post's media is never swept (`backup-restore.md` § 9 step 2), and
    /// the gated-post builder copies the *sealed* body's list onto
    /// [`crate::subscription::types::GatedInfo::attachment_refs`] so the nest
    /// can pin what it cannot read. A `remote_url`-only bridged item still
    /// carries a `blob_hash` and is listed — pinning a hash the nest does not
    /// hold is harmless, missing one that it does is data loss.
    pub fn blob_refs(&self) -> Vec<ContentHash> {
        match self {
            PostBody::Video {
                manifest,
                segments,
                thumbnail,
                ..
            } => {
                let mut out = Vec::with_capacity(segments.len() + 2);
                out.push(*manifest);
                out.push(*thumbnail);
                out.extend(segments.iter().map(|s| s.hash));
                out
            }
            PostBody::Text { .. }
            | PostBody::Media { .. }
            | PostBody::TextWithMedia { .. }
            | PostBody::Structured { .. } => self
                .media_items()
                .iter()
                .flat_map(|m| std::iter::once(m.blob_hash).chain(m.thumbnail))
                .collect(),
        }
    }
}

/// A key-value field in a structured post.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredField {
    pub key: String,
    pub value: String,
}

/// A range annotation over text content for rich-text features.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Facet {
    pub byte_start: u32,
    pub byte_end: u32,
    pub feature: FacetFeature,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FacetFeature {
    Mention {
        actor_id: ActorId,
    },
    Link {
        uri: String,
    },
    Tag {
        name: String,
    },
    /// A feature a newer build writes and this one does not name, carried
    /// whole (`transport.md` § Schema and forward-compat discipline → *Rule 3
    /// in full*). Its range renders as plain text; no build writes one except
    /// by passing a decoded post's facets through.
    #[serde(untagged)]
    Unknown(CarriedValue),
}

/// Hint about the type of post body, included in firehose events
/// so consumers can filter without fetching full content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyHint {
    Text,
    Media,
    TextWithMedia,
    Video,
    /// A hint a newer writer named that this build does not know — the open
    /// arm of `transport.md` § Rule 3 in full (*Open, carrying*). It holds the
    /// exact string read and re-emits it, because a feed's rules are stored by
    /// the nest and echoed whole on update; it matches nothing.
    #[serde(untagged)]
    Other(String),
}

/// A reference to content-addressed media stored as a blob.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaItem {
    pub blob_hash: ContentHash,
    pub media_type: String,
    pub size_bytes: u64,
    pub dimensions: Option<Dimensions>,
    pub thumbnail: Option<ContentHash>,
    /// A bridged item's media, served by the reader's OWN nest: always a
    /// nest-relative, already-proxied path (`/api/v1/media/proxy?url=…` for
    /// ActivityPub and nostr, `/api/v1/bluesky/media?url=…` for Bluesky), never
    /// the remote origin (bridges.md § Unified feed ingestion ruling 4). Set only
    /// on an item with no blob (a zero `blob_hash`); the feed fold turns it into
    /// `RenderBlock::ProxiedImage` (render-model.md § D6c). An absolute
    /// `https://` value (a non-conforming writer) is rewritten through
    /// [`shared_media_proxy_url`] by the fold.
    #[serde(default)]
    pub remote_url: Option<String>,
    /// This item's own text description — what a screen reader speaks for it and
    /// what the feed fold puts on the item's media block (render-model.md § D6c).
    /// Written by the two bridge ingest paths from the remote attachment's
    /// description (ActivityPub `name`, Bluesky per-image `alt`; an empty one
    /// rests as `None`); a native upload leaves it `None`. Independent of the
    /// post-level `alt_text` of [`PostBody::Media`], which is the *body text* of a
    /// media-only post and is never copied onto an item. Additive: omitted from
    /// the encoding when `None`, so an item without one encodes exactly as it
    /// did before the field existed (`version-compatibility.md` § Dimension 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
}

/// An item naming no blob (the zero `blob_hash` a bridged item carries) and
/// nothing else — the base for struct-update construction
/// (`..Default::default()`), so a literal names only the fields it means and
/// the next additive field does not touch it.
impl Default for MediaItem {
    fn default() -> Self {
        MediaItem {
            blob_hash: ContentHash::from_digest_raw([0u8; 32]),
            media_type: String::new(),
            size_bytes: 0,
            dimensions: None,
            thumbnail: None,
            remote_url: None,
            alt: None,
        }
    }
}

/// A remote attachment's description as [`MediaItem::alt`] rests it: the text as
/// the remote wrote it, or `None` when it is absent, empty or only whitespace —
/// never `Some("")`. The one home of that rule, so the bridge ingest paths
/// (ActivityPub `name`, Bluesky per-image `alt`) cannot come to disagree.
pub fn media_alt(description: Option<&str>) -> Option<String> {
    description
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

/// The shared media proxy's nest-relative path for a remote `https` resource — the
/// lane ActivityPub and nostr media and avatars ride (the nest's
/// `media_proxy_routes`); Bluesky's rides its own `bluesky/media` rewrite, applied
/// in its translator. The one home of this form (bridges.md § Unified feed
/// ingestion ruling 4): the nest's author upsert, ActivityPub ingest and the feed
/// fold's defensive arm all call it. The proxy fetches only `https`, so a non-https
/// origin yields `None` — no item, no avatar — rather than a path the route would
/// refuse.
pub fn shared_media_proxy_url(remote: &str) -> Option<String> {
    let remote = remote.trim();
    if !remote.starts_with("https://") {
        return None;
    }
    Some(format!(
        "/api/v1/media/proxy?url={}",
        urlencoding::encode(remote)
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

/// A segment of an HLS video stream, content-addressed by BLAKE3 hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoSegment {
    pub hash: ContentHash,
    /// Height in pixels (360, 720, 1080, 2160).
    pub resolution: u16,
    pub codec: String,
    /// Bitrate in kbps.
    pub bitrate: u32,
    pub byte_size: u64,
}

/// A perceptual hash anchor at a specific video timestamp, used for
/// proof-of-consumption verification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationAnchor {
    pub timestamp_ms: u64,
    #[serde(with = "serde_bytes")]
    pub phash: [u8; 8],
}

/// References to other posts, forming threads, reposts, reactions, and votes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Reference {
    Reply {
        post_id: PostId,
    },
    Repost {
        post_id: PostId,
    },
    Quote {
        post_id: PostId,
    },
    React {
        post_id: PostId,
        emoji: String,
    },
    Upvote {
        post_id: PostId,
    },
    Downvote {
        post_id: PostId,
    },
    /// A reference a newer build writes and this one does not name, carried
    /// whole (`transport.md` § Schema and forward-compat discipline → *Rule 3
    /// in full*). Every reader ignores it: it threads, counts and notifies
    /// nothing.
    #[serde(untagged)]
    Unknown(CarriedValue),
}

/// An Actor's profile.
///
/// Signed via `fauna_cbor::SignedEnvelope` (sign-over-CID) per
/// `docs/goal/architecture/serialization.md`. The signature does NOT live on
/// the struct — sign() returns `(canonical_bytes, SignedEnvelope)` and the
/// envelope ships alongside the bytes in the embed-as-bytes wire shape
/// (`docs/goal/architecture/serialization.md` § Embed-as-bytes for signed payloads).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub actor_id: ActorId,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar: Option<ContentHash>,
    pub banner: Option<ContentHash>,
    pub links: Vec<ProfileLink>,
    /// Spelled `nests`; the `nodes` read alias for a pre-rename profile was
    /// retired 2026-09-24 (the compat-remnant sweep, universal scope) — a
    /// `nodes`-keyed profile is refused as a missing `nests` field.
    pub nests: Vec<NestEntry>,
    #[serde(default)]
    pub admin_nests: Vec<AdminNestEntry>,
    pub load_hint: Option<AccountLoadHint>,
    pub inbox_mode: InboxMode,
    /// The head of this actor's RecoveryKey registration chain, mirrored by
    /// the owner's client when it (re-)registers a RecoveryKey so peers can
    /// verify an [`crate::recovery::IdentitySuccession`] from the profile they
    /// already cache, without fetching the chain
    /// (`identity-succession.md:36`).
    ///
    /// **Both halves ride in one value on purpose** — the coupling
    /// [`crate::recovery::ChainHead`] enforces in memory, extended
    /// to the wire by the review: a parallel `recovery_seq` field
    /// would let an editor whose decoder predates the seq half preserve the
    /// pubkey while silently stripping the seq on a read-modify-write edit,
    /// re-creating exactly the "pubkey but no seq" state that runs the chain
    /// rewrite/truncation guard switched off. Under a single key, an editor
    /// that predates the field drops the binding whole: the cache degrades to
    /// absent, never to poisoned. (The pubkey-only `recovery_pubkey` field
    /// this replaces was never emitted by any shipped producer, so its removal
    /// has no wire footprint; bytes carrying that retired key decode with this
    /// field `None`.)
    ///
    /// `None` means "no binding as of this profile version" — but the chain is
    /// authoritative, this field is a cache: a consumer that finds `None` and
    /// needs certainty must fetch `fauna.recovery.registration.chain`.
    /// `skip_serializing_if` keeps a binding-less profile byte-identical to
    /// the pre-field encoding (additive per `version-compatibility.md`; absent
    /// — never `null` — is the only encoding of "no binding").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_head: Option<crate::recovery::ChainHead>,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileLink {
    pub label: String,
    pub uri: String,
}

/// A nest that this actor uses, advertised in their profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NestEntry {
    /// The nest's Ed25519 public key (32 bytes).
    #[serde(with = "serde_bytes")]
    pub nest_id: Vec<u8>,
    /// HTTPS URL of the nest.
    pub url: String,
    /// What this nest does for the actor.
    pub roles: Vec<NestRole>,
}

/// Role a nest plays for an actor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NestRole {
    /// Serves public posts, blobs, handles, firehose.
    Social,
    /// Accepts MLS ciphertext for delivery to this actor.
    Mls,
    /// A role a newer build writes and this one does not name — the exact
    /// string read, re-emitted on the owner's re-sign so an older device's
    /// profile edit never strips it (`transport.md` § Schema and forward-compat
    /// discipline → *Rule 3 in full*). Ignored for routing.
    #[serde(untagged)]
    Other(String),
}

/// A nest that this actor administers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminNestEntry {
    /// The nest's Ed25519 public key (32 bytes).
    #[serde(with = "serde_bytes")]
    pub nest_id: Vec<u8>,
    /// HTTPS URL of the nest.
    pub url: String,
    /// Human-readable name (e.g. domain or custom label).
    pub name: String,
}

/// A request by the author to hide a post.
///
/// Signed via `fauna_cbor::SignedEnvelope` (sign-over-CID) per
/// `docs/goal/architecture/serialization.md`. The signature does NOT live on
/// the struct — sign() returns `(canonical_bytes, SignedEnvelope)` and the
/// envelope ships alongside the bytes in the embed-as-bytes wire shape
/// (`docs/goal/architecture/serialization.md` § Embed-as-bytes for signed payloads).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tombstone {
    pub author: ActorId,
    pub post_id: PostId,
    pub created_at: Timestamp,
}

/// Hints for how a subscriber should load an actor's content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AccountLoadHint {
    /// Load all posts from this actor.
    Full,
    /// Load only the N most recent posts.
    Recent(u32),
    /// Load posts from the given timestamp onward.
    Since(Timestamp),
    /// A hint a newer build writes and this one does not name, carried whole
    /// so the owner's re-sign keeps it (`transport.md` § Schema and
    /// forward-compat discipline → *Rule 3 in full*). Loads as [`Self::Full`].
    #[serde(untagged)]
    Unknown(CarriedValue),
}

/// How the actor accepts incoming contact requests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InboxMode {
    /// Accept messages from anyone.
    Open,
    /// Unknown senders must knock first (default).
    #[default]
    AllowKnock,
    /// Only confirmed/accepted contacts can message; no knocking allowed.
    ContactsOnly,
    /// No incoming messages at all.
    Closed,
    /// A mode a newer build writes into the signed profile and this one does
    /// not name — the exact string read, re-emitted on the owner's re-sign so
    /// an older device's profile edit never strips it (`transport.md`
    /// § Schema and forward-compat discipline → *Rule 3 in full*). It acts as
    /// [`Self::Closed`] ([`Self::effective`]), has no wire/DB token
    /// ([`Self::to_wire`] is `None`) and no selector row.
    #[serde(untagged)]
    Other(String),
}

impl InboxMode {
    /// Parse the lowercase wire/DB mode string (`inbox_modes.mode`, validated by
    /// the nest's `set_inbox_mode` against exactly these four tokens) into the
    /// typed enum. Any other value is `None` — callers reject rather than guess,
    /// mirroring the knock path's own unknown-mode arm
    /// (`routes.rs::deliver_inbox_payload_core`).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "allow_knock" => Some(Self::AllowKnock),
            "contacts_only" => Some(Self::ContactsOnly),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }

    /// The wire/DB mode token — the inverse of [`from_wire`](Self::from_wire).
    /// The single source the selector's wire half is built from, so the four
    /// literal tokens live in exactly one place. As of 2026-08-22 that is one
    /// consumer, not two: `fauna_protocol::contacts::INBOX_MODES` pairs these
    /// tokens with their labels once for both Rust-native shells, where tui
    /// and linux each used to keep a hand-written table (priority #2).
    ///
    /// `None` for [`Self::Other`]: a mode this build does not name has no
    /// token the nest's `set_inbox_mode` accepts, so a caller sends nothing
    /// rather than a guessed mode.
    pub const fn to_wire(&self) -> Option<&'static str> {
        match self {
            Self::Open => Some("open"),
            Self::AllowKnock => Some("allow_knock"),
            Self::ContactsOnly => Some("contacts_only"),
            Self::Closed => Some("closed"),
            Self::Other(_) => None,
        }
    }

    /// The known mode this one behaves as: itself, or [`Self::Closed`] — the
    /// most restrictive — for a mode this build does not name.
    pub fn effective(&self) -> Self {
        match self {
            Self::Other(_) => Self::Closed,
            known => known.clone(),
        }
    }
}

/// Status of a contact relationship between two actors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactStatus {
    /// Knock received, awaiting recipient action.
    Pending,
    /// Knock accepted. Temporary — expires after TTL if no message exchange.
    Accepted,
    /// Message exchanged in either direction. Permanent.
    Confirmed,
    /// Explicitly blocked. Rejects all future knocks and messages.
    Blocked,
    /// A stored status token this build does not name — a newer nest's
    /// relationship, read back by an older reader (`transport.md` § Schema
    /// and forward-compat discipline → *Rule 3 in full*: the duty binds a
    /// hand-written projection). The edge exists, so it is never read as a
    /// stranger: the nest's gates suppress it as they do [`Self::Blocked`],
    /// and the client folder gate — where suppressing would leave the share,
    /// a deletion — knocks.
    Unrecognized,
}

impl ContactStatus {
    /// Parse the lowercase wire/DB status string the nest stores in the
    /// `contacts.status` column (and returns over `fauna.contacts.status` /
    /// `fauna.contacts.list`) back into the typed enum. `upsert_contact` /
    /// `accept_contact` / `block_contact` / `promote_to_confirmed` all persist
    /// exactly these four tokens (`bins/fauna-nest/src/db/contacts.rs`). An
    /// empty value is `None` (a true stranger — no contact record), which
    /// [`contact_arrival_disposition`] then treats as a knock; any other token
    /// is [`Self::Unrecognized`] — a relationship a newer nest stored, which no
    /// gate may read as more permissive than a stranger's. The single wire↔enum
    /// mapping so every gate parses a stored status identically (priority #2).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "" => None,
            "pending" => Some(Self::Pending),
            "accepted" => Some(Self::Accepted),
            "confirmed" => Some(Self::Confirmed),
            "blocked" => Some(Self::Blocked),
            _ => Some(Self::Unrecognized),
        }
    }
}

/// How a relationship-gated inbound arrival (a knock, a cross-user shared file
/// set, …) should be handled, given the recipient's contact-status toward the
/// *sender*. This is the shared decision the nest DM-knock gate already applies
/// inline (`bins/fauna-nest/src/routes.rs::deliver_inbox_payload_core`,
/// `InboxMode::AllowKnock`): a known contact is frictionless, a stranger must
/// knock, a blocked sender is suppressed. Lifting the mapping here lets every
/// gate — the folder recipient gate (`docs/goal/ui/folders.md` § Sharing),
/// and a future unification of the DM-knock path (priority #4) — decide
/// identically instead of re-deriving the auto/knock/suppress split per surface.
///
/// This is the **client-side** disposition (the recipient's own client decides
/// auto-join vs. knock for a share it already received). Its nest-side sibling —
/// the routing floor that must hold *before* an inbox row is ever written, and
/// against a non-conforming client — is [`supervised_reach_verdict`] below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrivalDisposition {
    /// A Confirmed or Accepted contact → accept automatically, no user action
    /// (the folder share auto-joins + auto-appears read-only in the list).
    Auto,
    /// An unknown sender (no contact record) or a still-`Pending` knock → stage
    /// for the recipient to accept/decline. The MLS Welcome stays unprocessed
    /// until accept, so the recipient never joins a stranger's group unbidden.
    Knock,
    /// A Blocked sender → drop silently; the arrival never surfaces to the user.
    Suppress,
}

/// Map a recipient's contact-status toward a sender to the [`ArrivalDisposition`]
/// for a relationship-gated arrival. `None` (no contact record — a true
/// stranger) is treated exactly like a `Pending` knock: it must knock. This is
/// the single source of the "auto for contacts, knock for strangers, suppress
/// the blocked" rule ratified in `docs/goal/ui/folders.md` § Sharing.
pub fn contact_arrival_disposition(status: Option<ContactStatus>) -> ArrivalDisposition {
    match status {
        Some(ContactStatus::Confirmed | ContactStatus::Accepted) => ArrivalDisposition::Auto,
        Some(ContactStatus::Blocked) => ArrivalDisposition::Suppress,
        // An unrecognized edge knocks rather than suppresses: suppressing here
        // leaves the share, and an unknown value never deletes.
        Some(ContactStatus::Pending | ContactStatus::Unrecognized) | None => {
            ArrivalDisposition::Knock
        }
    }
}

/// Which ingress class an arrival entered the nest through. `Federation` means
/// the arriving party reached us through a peer nest, which the open-federation
/// trust model authenticates as a *nest*, never as an *actor*
/// (`docs/goal/architecture/federation.md` § Trust model).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrivalOrigin {
    Local,
    Federation,
}

/// The two reach knobs a guardian sets on a supervised account that bind at the
/// nest's routing floor (`docs/goal/behavior/family-safety.md` § Guardian policy
/// pillar 1). Absent (`None` at the call sites below) means the recipient is not
/// supervised, and the caller's own routing — inbox mode — decides alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisedReach {
    /// New contact edges, in both directions, need the guardian's approval.
    pub contact_approval: bool,
    /// Actors on other nests may initiate contact with the ward.
    pub federation_contact: bool,
}

/// The routing-floor verdict for one sender-attributed arrival, decided *before*
/// and independently of the recipient's inbox mode. It is only ever more
/// restrictive than the mode: a `Proceed` hands the arrival back to the caller's
/// own routing, it never forces a delivery the mode would refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReachVerdict {
    /// The floor imposes nothing — the caller applies its own routing.
    Proceed,
    /// A new party the recipient (or, when supervised, their guardian) must
    /// review before it reaches them.
    Knock,
    /// Never surfaces to the recipient.
    Suppress,
}

/// The nest's **routing floor** for a sender-attributed arrival: the decision
/// that must hold on every path that writes a recipient's inbox, whatever the
/// sending client does (`family-safety.md` § Guardian policy pillar 1 — *"holds
/// even against a non-conforming client … it cannot be bypassed by using a
/// different client"*).
///
/// It reads only facts the **recipient's own nest** owns and the **sender cannot
/// manufacture**: the stored contact edge and the stored guardian policy. In
/// particular it never consults anything the sender declares about itself — the
/// post's `schema` string, the Welcome's `kind`, or a peer nest's assertion about
/// who its user is. Every such self-declaration authorizes exactly nothing.
///
/// Established contacts (`Accepted` / `Confirmed`) keep flowing under every knob:
/// both reach knobs act on *new parties only*.
pub fn supervised_reach_verdict(
    status: Option<ContactStatus>,
    supervised: Option<SupervisedReach>,
    origin: ArrivalOrigin,
) -> ReachVerdict {
    // A blocked edge suppresses on every path, supervised or not — the same
    // rule `contact_arrival_disposition` already applies to folder shares.
    if matches!(
        status,
        Some(ContactStatus::Blocked | ContactStatus::Unrecognized)
    ) {
        return ReachVerdict::Suppress;
    }
    if matches!(
        status,
        Some(ContactStatus::Accepted | ContactStatus::Confirmed)
    ) {
        return ReachVerdict::Proceed;
    }
    let Some(reach) = supervised else {
        // Not supervised: the floor imposes nothing beyond the blocked edge.
        return ReachVerdict::Proceed;
    };
    // `federation_contact = off`: actors on other nests cannot *initiate*.
    // Checked before `contact_approval` so a suppressed cross-nest stranger
    // never even reaches the guardian's queue.
    if origin == ArrivalOrigin::Federation && !reach.federation_contact {
        return ReachVerdict::Suppress;
    }
    if reach.contact_approval {
        return ReachVerdict::Knock;
    }
    ReachVerdict::Proceed
}

/// The floor verdict when the arriving party **cannot be authenticated at all** —
/// the cross-nest MLS Welcome plane, whose wire carries no signed sender (the
/// `welcome_bytes` are opaque MLS ciphertext, and a peer nest's word about its
/// own user is attribution, not authorization).
///
/// Fail-closed by construction: a party the nest cannot name can never be an
/// approved contact, so under `contact_approval` it is suppressed rather than
/// knocked — there is no identity to put in the guardian's queue for them to
/// approve. An unsupervised recipient is unaffected.
pub fn unauthenticated_reach_verdict(
    supervised: Option<SupervisedReach>,
    origin: ArrivalOrigin,
) -> ReachVerdict {
    let Some(reach) = supervised else {
        return ReachVerdict::Proceed;
    };
    if origin == ArrivalOrigin::Federation && !reach.federation_contact {
        return ReachVerdict::Suppress;
    }
    if reach.contact_approval {
        return ReachVerdict::Suppress;
    }
    ReachVerdict::Proceed
}

/// What the recipient's **inbox mode** says about a conversation-shaped
/// initiation (a DM or group Welcome) from `status` — the unsupervised half of
/// the DM reach policy, and the "caller's own routing" [`ReachVerdict`]'s
/// contract hands back after a floor `Proceed`
/// (`docs/goal/behavior/direct-messages.md` § Reach policy — inbox mode on the
/// DM plane).
///
/// One shared mapping so every conversation-shaped ingress — the same-nest
/// Welcome plane today, the family bridge-DM gate and any future DM-shaped
/// arrival tomorrow — reads a mode identically instead of re-deriving it
/// (priority #2; the knock path's own routing in
/// `routes.rs::deliver_inbox_payload_core` is deliberately NOT this function:
/// there a mode decides *knock-store semantics* — `open` auto-accepts,
/// `allow_knock` stores the knock — with side effects a pure verdict cannot
/// carry; the two agree on who ultimately gets through).
///
/// Established contacts (`Accepted` / `Confirmed`) flow under **every** mode —
/// the same rule the floor and `contact_arrival_disposition` apply. For a
/// stranger (or a still-`Pending` knock — "must accept before DMs flow"):
/// `Open` proceeds, `AllowKnock` answers `Knock` (the contact request is the
/// sender's path — the ratified shape; holding the Welcome itself in a second
/// pending store is forbidden, `family-safety.md` § Don't do these), and
/// `ContactsOnly` / `Closed` suppress (those modes offer no knock path either).
pub fn dm_initiation_mode_verdict(status: Option<ContactStatus>, mode: InboxMode) -> ReachVerdict {
    if matches!(
        status,
        Some(ContactStatus::Blocked | ContactStatus::Unrecognized)
    ) {
        return ReachVerdict::Suppress;
    }
    if matches!(
        status,
        Some(ContactStatus::Accepted | ContactStatus::Confirmed)
    ) {
        return ReachVerdict::Proceed;
    }
    match mode {
        InboxMode::Open => ReachVerdict::Proceed,
        InboxMode::AllowKnock => ReachVerdict::Knock,
        InboxMode::ContactsOnly | InboxMode::Closed | InboxMode::Other(_) => ReachVerdict::Suppress,
    }
}

/// The `unknown_sender_mail` knob (`family-safety.md` § The mail gate) — what a
/// ward's nest does with cold inbound mail from an address the ward has never
/// corresponded with. Parsed from the closed wire enum stored in
/// `guardian_policies.unknown_sender_mail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownSenderMail {
    /// The unsupervised-equivalent default: cold mail is delivered normally.
    Allow,
    /// Deliver, but *place* it in the ward's held mailbox pending the guardian's
    /// review. Never a refusal — the message is accepted and sealed to the ward.
    Hold,
    /// Refuse the recipient outright: `550` at SMTP `RCPT TO`, or a typed
    /// refusal of the sender on the in-domain twin.
    Reject,
}

impl UnknownSenderMail {
    /// Parse the stored/wire value. Unrecognized values **fail closed to
    /// [`Self::Hold`]** — the strictest verdict that still loses no mail. A newer
    /// nest could write a value this binary does not know (evolution is
    /// additive-everywhere, `version-compatibility.md`); holding lets the
    /// guardian release it, whereas `Allow` would silently void the policy and
    /// `Reject` would bounce mail the sender cannot re-send.
    pub fn from_wire(value: &str) -> Self {
        match value {
            "allow" => Self::Allow,
            "reject" => Self::Reject,
            _ => Self::Hold,
        }
    }

    /// The canonical wire/DB string — the inverse of [`Self::from_wire`] and what
    /// the policy editor's select writes back to `guardian_policies.
    /// unknown_sender_mail`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Hold => "hold",
            Self::Reject => "reject",
        }
    }

    /// The ratified picker order (`family-safety.md` § Guardian policy — *"allow /
    /// hold / reject"*), default first. Every app's select renders this order.
    pub const ORDER: [Self; 3] = [Self::Allow, Self::Hold, Self::Reject];

    /// The option an unrepresentable selection degrades to — the same verdict
    /// [`Self::from_wire`] gives an unparseable value. Named rather than left as
    /// "index 1 of [`Self::ORDER`]" so a picker's out-of-range fallback states the
    /// safety rule instead of re-deriving it from the catalog's shape.
    pub const FAIL_CLOSED: Self = Self::Hold;
}

/// The `feed_sources` knob (`family-safety.md` § Guardian policy pillar 1) —
/// whether a ward may connect **new** external feed sources (Bluesky / Nostr /
/// ActivityPub bridge accounts) or add follows on them. The mail gate's sibling
/// knob, and the second of the two closed string enums the guardian's policy
/// editor renders.
///
/// Deliberately a **separate type** from [`UnknownSenderMail`] rather than a
/// shared "policy value" enum: the two knobs have different value sets and
/// different fail-closed defaults (`hold` vs. `block`), and collapsing them is
/// exactly the latent trap this type retires — windows' single `ValueLabel` map
/// rendered a `feed_sources` value of `"reject"` as "Reject" (a label outside
/// this knob's catalog) and an *unrecognized* one as "Hold for review", because
/// its `_ =>` arm baked in the other knob's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedSources {
    /// The unsupervised-equivalent default: the ward connects sources freely.
    Allow,
    /// The ward cannot connect new external sources or add follows on them
    /// (typed refusal). Already-connected sources keep flowing until the
    /// guardian removes them.
    Block,
}

impl FeedSources {
    /// Parse the stored/wire value. Unrecognized values **fail closed to
    /// [`Self::Block`]** — this knob's strict option, *not* [`UnknownSenderMail`]'s
    /// `Hold`. Same reasoning as its sibling: within a major version a client may
    /// be older than its nest (`version-compatibility.md`), so a newer nest may
    /// store a value this binary cannot name; rendering it as `Allow` would show
    /// the guardian a policy weaker than the one actually enforced, and — because
    /// save writes the editor's state back — the next save would silently
    /// downgrade the ward's protection for real.
    pub fn from_wire(value: &str) -> Self {
        match value {
            "allow" => Self::Allow,
            _ => Self::Block,
        }
    }

    /// The canonical wire/DB string — the inverse of [`Self::from_wire`].
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Block => "block",
        }
    }

    /// The ratified picker order (`family-safety.md` § Guardian policy — *"allow /
    /// block"*), default first.
    pub const ORDER: [Self; 2] = [Self::Allow, Self::Block];

    /// The option an unrepresentable selection degrades to — **`Block`**, this
    /// knob's strict option, not [`UnknownSenderMail::FAIL_CLOSED`]'s `Hold`.
    pub const FAIL_CLOSED: Self = Self::Block;
}

/// The `operation` of a feed-source approval ask (`family-safety.md` § Feed-source
/// approvals) — which bridge operation the ward is asking the guardian to approve,
/// and the closed set the wire admits.
///
/// **Not a knob, and deliberately shaped unlike [`FeedSources`] /
/// [`UnknownSenderMail`].** Those parse a *stored policy value*, where an
/// unrecognized string must still resolve to some verdict — so they fail closed to
/// their strict option. This parses a *caller's request parameter*, where no
/// "strictest operation" exists to degrade to: an unnameable operation names no
/// object to approve, so [`Self::from_wire`] returns `None` and the caller refuses
/// outright. Degrading it to some default operation would mint a grant for an
/// object the guardian never saw.
///
/// Shared rather than nest-local per § Where logic lives (*write validation is
/// shared Rust, so clients pre-validate identically to the nest's refusal*): a
/// grant matches `(bridge_id, operation, target)` **exactly**, so a client that
/// invented a fourth operation string would mint asks no gate could ever redeem —
/// a dead-end the ward would experience as a guardian who approves and changes
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedSourceOperation {
    /// Connect the bridge account itself (`fauna.bridges.link`). Approving a link
    /// approves *connecting that bridge* — the OAuth mode is mechanism, not scope
    /// — so a `Link` ask carries an empty target ([`Self::takes_target`]).
    Link,
    /// Add a follow on an already-connected bridge (`fauna.bridges.add_follow`);
    /// the target is the follow id.
    Follow,
    /// Subscribe to an external feed (`fauna.bridges.feeds.create`); the target is
    /// the feed URI.
    Feed,
}

impl FeedSourceOperation {
    /// Parse the wire value. `None` for anything outside the closed set — the
    /// caller refuses it (see the type docs on why this is deliberately *not* a
    /// fail-closed parse).
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "link" => Some(Self::Link),
            "follow" => Some(Self::Follow),
            "feed" => Some(Self::Feed),
            _ => None,
        }
    }

    /// The canonical wire/DB string — the inverse of [`Self::from_wire`].
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Follow => "follow",
            Self::Feed => "feed",
        }
    }

    /// Whether this operation names a *specific object* (and so carries a
    /// non-empty target), or approves the bridge connection as a whole.
    ///
    /// Load-bearing in both directions, which is why it is a named rule rather
    /// than an `is_link()` check at each call site: a `Link` ask with a stray
    /// target would mint a grant keyed on a target the redeeming gate never
    /// passes (unredeemable — the ward sees an approval that changes nothing),
    /// and a `Follow`/`Feed` ask with an *empty* target would mint a grant that
    /// names no object, which the guardian cannot meaningfully approve.
    pub fn takes_target(self) -> bool {
        match self {
            Self::Link => false,
            Self::Follow | Self::Feed => true,
        }
    }
}

#[cfg(test)]
mod feed_source_operation_tests {
    use super::FeedSourceOperation;

    /// The closed set is exactly the ratified three (`family-safety.md`
    /// § Feed-source approvals — *"`operation` is the closed set `link | follow |
    /// feed`"*), and `as_str` is a true inverse of `from_wire`.
    #[test]
    fn the_closed_set_round_trips() {
        for op in [
            FeedSourceOperation::Link,
            FeedSourceOperation::Follow,
            FeedSourceOperation::Feed,
        ] {
            assert_eq!(FeedSourceOperation::from_wire(op.as_str()), Some(op));
        }
        assert_eq!(
            FeedSourceOperation::from_wire("link"),
            Some(FeedSourceOperation::Link)
        );
        assert_eq!(
            FeedSourceOperation::from_wire("follow"),
            Some(FeedSourceOperation::Follow)
        );
        assert_eq!(
            FeedSourceOperation::from_wire("feed"),
            Some(FeedSourceOperation::Feed)
        );
    }

    /// An operation outside the set is **refused, never degraded**. This is the
    /// pin that keeps this type from drifting into the knobs' fail-closed shape
    /// (`FeedSources::from_wire` returns `Block` for junk): here there is no
    /// strictest operation, and silently resolving junk to any of the three would
    /// mint a grant for an object the guardian never approved.
    ///
    /// Note `"Link"`/`"LINK"` are junk too — the wire spelling is exact, matching
    /// the case-sensitivity the sibling knobs' fail-closed arms pin.
    #[test]
    fn an_unnameable_operation_is_refused_not_degraded() {
        for junk in [
            "",
            "hologram",
            "Link",
            "LINK",
            "link ",
            " link",
            "links",
            "unlink",
            "remove_follow",
            "allow",
            "block",
            "0",
        ] {
            assert_eq!(
                FeedSourceOperation::from_wire(junk),
                None,
                "operation {junk:?} must be refused, never resolved to an operation"
            );
        }
    }

    /// `link` approves connecting the bridge as a whole and so names no object;
    /// the other two name one (`family-safety.md` § Feed-source approvals — *"the
    /// follow id for `follow`, the feed URI for `feed`, and empty for `link`"*).
    #[test]
    fn only_link_carries_an_empty_target() {
        assert!(!FeedSourceOperation::Link.takes_target());
        assert!(FeedSourceOperation::Follow.takes_target());
        assert!(FeedSourceOperation::Feed.takes_target());
    }
}

/// The `unknown_peer_dm` knob (`family-safety.md` § The bridge-DM gate) — what a
/// ward's nest does with an inbound bridge DM from an external peer the ward has
/// never corresponded with. Parsed from the closed wire enum stored in
/// `guardian_policies.unknown_peer_dm`.
///
/// The fifth closed knob enum, and — per the two-knobs lesson [`FeedSources`]
/// records — a **separate type** rather than a reuse of [`UnknownSenderMail`]
/// despite the shared `hold` spelling: this knob's value set has no `reject`,
/// so a merged type would render an unreachable third option in this knob's
/// picker and admit a value its own gate cannot honor.
///
/// **There is deliberately no `Reject` arm.** Bridge DMs arrive by *pull* (a
/// relay subscription, a poller), so there is no per-sender refusal stage to
/// bounce from — unlike mail's `RCPT TO`. `Hold` is therefore the strictest
/// verdict that still loses no message, the same rationale as the mail gate's
/// null-path downgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownPeerDm {
    /// The unsupervised-equivalent default: DMs from cold external peers are
    /// stored and delivered normally.
    Allow,
    /// A conversation with a peer carrying no verdict row is *computed* as held
    /// pending the guardian's review. Never a refusal and never a placement:
    /// the DM is stored sealed exactly as today (§ The bridge-DM gate —
    /// *"placement is computed, never stored"*), and the read surfaces mark it.
    Hold,
}

impl UnknownPeerDm {
    /// Parse the stored/wire value. Unrecognized values **fail closed to
    /// [`Self::Hold`]** — this knob's strict option. Same reasoning as its two
    /// siblings: within a major version a client may be older than its nest
    /// (`version-compatibility.md`), so a newer nest may store a value this
    /// binary cannot name, and resolving it to `Allow` would silently void the
    /// policy the guardian actually set.
    pub fn from_wire(value: &str) -> Self {
        match value {
            "allow" => Self::Allow,
            _ => Self::Hold,
        }
    }

    /// The canonical wire/DB string — the inverse of [`Self::from_wire`].
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Hold => "hold",
        }
    }

    /// The ratified picker order (`family-safety.md` § The bridge-DM gate —
    /// *"closed enum `allow | hold`, default `allow`"*), default first.
    pub const ORDER: [Self; 2] = [Self::Allow, Self::Hold];

    /// The option an unrepresentable selection degrades to — **`Hold`**, this
    /// knob's strict option.
    pub const FAIL_CLOSED: Self = Self::Hold;
}

/// A guardian-decided verdict on one external DM peer, stored per
/// `(ward, bridge, peer)` in `guardian_dm_peers` (`family-safety.md`
/// § The bridge-DM gate).
///
/// Distinct from [`UnknownPeerDm`], which is the *policy* over peers carrying no
/// verdict at all: this is the per-peer decision itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmPeerVerdict {
    /// A known peer — DMs deliver. Written by the guardian's approve, and
    /// **seeded by the ward's own outbound sends** (the mail-allowlist premise:
    /// the child initiating chose the correspondent, so replies always flow).
    Allow,
    /// The guardian denied this peer: new inbound DMs are refused *before*
    /// storage. Already-stored rows remain the ward's to read — refusing new
    /// arrivals is the RCPT-reject analogue, never destruction of stored data.
    Block,
}

impl DmPeerVerdict {
    /// Parse a stored verdict. `None` for anything outside the closed set —
    /// deliberately **not** degraded here, because neither arm is a safe guess
    /// for an unnameable verdict: see [`supervised_dm_verdict`], which resolves
    /// it to [`DmVerdict::Held`] (the one outcome that loses no message and
    /// voids no guardian decision).
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "allow" => Some(Self::Allow),
            "block" => Some(Self::Block),
            _ => None,
        }
    }

    /// The canonical wire/DB string — the inverse of [`Self::from_wire`].
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Block => "block",
        }
    }
}

/// What the bridge-DM gate does with one inbound DM from one external peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmVerdict {
    /// Store and deliver normally.
    Deliver,
    /// Store exactly as `Deliver` does — **the hold is computed, never stored**
    /// (§ Don't do these — *"don't store bridge-DM hold state"*) — and mark the
    /// conversation held on the read surfaces so the ward's conforming client
    /// renders it as awaiting the guardian. Reading is never gated (§ The trust
    /// shape invariant 4).
    Held,
    /// Refuse this DM *before* storage. Only a guardian's explicit `block`
    /// reaches this arm; already-stored rows from the peer stay readable.
    Blocked,
}

/// The bridge-DM gate's verdict for one inbound DM (`family-safety.md`
/// § The bridge-DM gate).
///
/// The third sibling of [`supervised_reach_verdict`] and [`supervised_mail_verdict`]:
/// same fail-closed shape, its own ingress class. The reach floor keys on a stored
/// `ContactStatus` between two *actors*; the mail gate keys on an *address* in the
/// ward's allowlist; this keys on an *external bridge peer id* in the ward's
/// `guardian_dm_peers` set. They share the shape, never the code — an external peer
/// is not an actor on this nest, which is precisely why `contact_approval` cannot
/// see it.
///
/// Like both siblings it reads only facts the recipient's own nest owns (the stored
/// policy, the stored verdict rows) and nothing the *sender* declares. It cannot be
/// stronger than the bridge account it rides — the nest custodies the bridge keys
/// and is already inside that TCB (§ Honest bounds (b), the mail gate's
/// bridge-boundary rationale unchanged).
///
/// `policy` is `None` when the ward is not supervised, which short-circuits to
/// [`DmVerdict::Deliver`]: a verdict row that outlives its link is **inert**, never
/// a gate on a now-full account (the Slice F precedent — the device marker's
/// predicate is *"marked AND currently supervised"*, never the flag alone, so no
/// crash can strand an unsupervised account behind stale oversight state).
///
/// `peer_verdict` is the stored `guardian_dm_peers.verdict` string for this
/// `(ward, bridge, peer)`, or `None` when **no row exists** — a cold peer. The raw
/// string (rather than a pre-parsed value) is deliberate: it keeps the whole
/// fail-closed rule in this one shared implementation per § Where logic lives,
/// instead of letting each caller pick a degrade for a verdict it cannot name.
pub fn supervised_dm_verdict(
    policy: Option<UnknownPeerDm>,
    peer_verdict: Option<&str>,
) -> DmVerdict {
    let Some(policy) = policy else {
        return DmVerdict::Deliver;
    };
    match peer_verdict {
        // A cold peer — no verdict row. This is the *only* case the knob
        // governs, which is what makes hold-ness computed: relaxing the knob
        // releases every held conversation by construction, with no drainage
        // rule and nothing strandable (§ The bridge-DM gate).
        None => match policy {
            UnknownPeerDm::Allow => DmVerdict::Deliver,
            UnknownPeerDm::Hold => DmVerdict::Held,
        },
        Some(v) => match DmPeerVerdict::from_wire(v) {
            // A known peer: the ward wrote it by DMing them, or the guardian
            // approved. Either way replies flow whatever the knob says.
            Some(DmPeerVerdict::Allow) => DmVerdict::Deliver,
            // An explicit guardian decision, and so deliberately **not** gated
            // on the knob: relaxing `unknown_peer_dm` means "stop reviewing
            // cold peers", never "unblock the peers I decided against".
            Some(DmPeerVerdict::Block) => DmVerdict::Blocked,
            // A verdict only a newer nest could write. `Held` is the
            // fail-closed resolution: `Deliver` would void a guardian decision
            // this binary cannot read, and `Blocked` would refuse arrivals on a
            // guess — where `Held` loses no message and lets the guardian
            // decide, the same reasoning that denies this knob a `reject` arm.
            None => DmVerdict::Held,
        },
    }
}

#[cfg(test)]
mod bridge_dm_gate_tests {
    use super::{DmPeerVerdict, DmVerdict, UnknownPeerDm, supervised_dm_verdict};

    /// The knob's fail-closed parse (`family-safety.md` § The bridge-DM gate —
    /// *"an unrecognized stored value fails closed to `hold`"*). Only the exact
    /// byte string `"allow"` may yield the permissive value: a newer nest can
    /// store a value this binary cannot name, and resolving it to `Allow` would
    /// silently void the guardian's actual policy.
    #[test]
    fn only_the_exact_allow_string_is_permissive() {
        assert_eq!(UnknownPeerDm::from_wire("allow"), UnknownPeerDm::Allow);
        for v in [
            "hold", "Allow", "ALLOW", " allow", "allow ", "allow\n",
            "аllow", // Cyrillic а — a confusable, not our "allow"
            "", "block",  // the feed_sources knob's value, not this knob's
            "reject", // the mail knob's third arm, which this knob has not got
            "deliver",
        ] {
            assert_eq!(
                UnknownPeerDm::from_wire(v),
                UnknownPeerDm::Hold,
                "{v:?} must fail closed to Hold"
            );
        }
    }

    /// `FAIL_CLOSED` and `from_wire`'s degrade must never drift apart — the
    /// picker's out-of-range fallback and the parse are the same safety rule.
    #[test]
    fn fail_closed_agrees_with_the_parse_degrade() {
        assert_eq!(UnknownPeerDm::FAIL_CLOSED, UnknownPeerDm::from_wire(""));
        assert_eq!(UnknownPeerDm::FAIL_CLOSED, UnknownPeerDm::Hold);
    }

    #[test]
    fn the_knob_round_trips_and_orders_default_first() {
        for v in UnknownPeerDm::ORDER {
            assert_eq!(UnknownPeerDm::from_wire(v.as_str()), v);
        }
        assert_eq!(
            UnknownPeerDm::ORDER,
            [UnknownPeerDm::Allow, UnknownPeerDm::Hold]
        );
    }

    /// The peer verdict is a closed set with **no** degrade — see
    /// `supervised_dm_verdict`, which owns the resolution of an unnameable one.
    #[test]
    fn a_peer_verdict_outside_the_closed_set_does_not_parse() {
        assert_eq!(
            DmPeerVerdict::from_wire("allow"),
            Some(DmPeerVerdict::Allow)
        );
        assert_eq!(
            DmPeerVerdict::from_wire("block"),
            Some(DmPeerVerdict::Block)
        );
        for v in ["hold", "Allow", "", "mute", "reject"] {
            assert_eq!(DmPeerVerdict::from_wire(v), None, "{v:?} must not parse");
        }
        for v in [DmPeerVerdict::Allow, DmPeerVerdict::Block] {
            assert_eq!(DmPeerVerdict::from_wire(v.as_str()), Some(v));
        }
    }

    /// An unsupervised account has no gate — and a verdict row that outlived its
    /// link is **inert**, never a gate on a now-full account (the Slice F
    /// device-marker precedent: *"marked AND currently supervised"*).
    #[test]
    fn an_unsupervised_ward_is_never_gated_even_with_a_stale_block_row() {
        assert_eq!(supervised_dm_verdict(None, None), DmVerdict::Deliver);
        assert_eq!(
            supervised_dm_verdict(None, Some("block")),
            DmVerdict::Deliver
        );
        assert_eq!(
            supervised_dm_verdict(None, Some("allow")),
            DmVerdict::Deliver
        );
    }

    /// The knob governs **cold peers only** — the property that makes hold-ness
    /// computed rather than stored (§ The bridge-DM gate).
    #[test]
    fn the_knob_governs_only_a_peer_with_no_verdict_row() {
        assert_eq!(
            supervised_dm_verdict(Some(UnknownPeerDm::Allow), None),
            DmVerdict::Deliver
        );
        assert_eq!(
            supervised_dm_verdict(Some(UnknownPeerDm::Hold), None),
            DmVerdict::Held
        );
    }

    /// A known peer delivers whatever the knob says: the ward seeded the row by
    /// DMing them, or the guardian approved. Replies always flow.
    #[test]
    fn an_allow_row_delivers_under_either_knob() {
        for knob in UnknownPeerDm::ORDER {
            assert_eq!(
                supervised_dm_verdict(Some(knob), Some("allow")),
                DmVerdict::Deliver,
                "an allowed peer must deliver under {knob:?}"
            );
        }
    }

    /// A block is an explicit guardian decision, so it binds **independently of
    /// the knob**: relaxing `unknown_peer_dm` means "stop reviewing cold peers",
    /// never "unblock the peers I decided against".
    #[test]
    fn a_block_row_binds_under_either_knob() {
        for knob in UnknownPeerDm::ORDER {
            assert_eq!(
                supervised_dm_verdict(Some(knob), Some("block")),
                DmVerdict::Blocked,
                "a blocked peer must stay blocked under {knob:?}"
            );
        }
    }

    /// A verdict only a *newer* nest could write resolves to `Held` under either
    /// knob — the one outcome that loses no message (`Blocked` would refuse an
    /// arrival on a guess) and voids no guardian decision (`Deliver` would).
    #[test]
    fn an_unnameable_verdict_row_holds_rather_than_delivering_or_blocking() {
        for knob in UnknownPeerDm::ORDER {
            for v in ["mute", "quarantine", "", "ALLOW"] {
                assert_eq!(
                    supervised_dm_verdict(Some(knob), Some(v)),
                    DmVerdict::Held,
                    "{v:?} under {knob:?} must fail closed to Held"
                );
            }
        }
    }

    /// Knob-relax releases every held conversation **by construction** — the
    /// property § Reach approvals leans on when it gives `dm_hold` no drainage
    /// rule. Nothing strandable exists because nothing was stored.
    #[test]
    fn relaxing_the_knob_releases_every_held_conversation() {
        let cold_peer = None;
        assert_eq!(
            supervised_dm_verdict(Some(UnknownPeerDm::Hold), cold_peer),
            DmVerdict::Held
        );
        // The guardian flips the knob to allow — and the same cold peer, with no
        // row written and no state migrated, now delivers.
        assert_eq!(
            supervised_dm_verdict(Some(UnknownPeerDm::Allow), cold_peer),
            DmVerdict::Deliver
        );
    }
}

/// Where an inbound mail delivery came from — the discriminator that keeps
/// **system-generated mail out of the guardian mail gate** (`family-safety.md`
/// § The mail gate → *"System-generated mail is never gated"*). A held
/// delivery-failure notice would strand the ward, so bounces (DSN), forwarder
/// NDRs and security notifications declare themselves [`Self::System`] at the
/// call site rather than relying on the empty null-reverse-path address.
///
/// The mail sibling of [`ArrivalOrigin`], which discriminates the *social* inbox
/// by transport rather than by authorship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailIngress<'a> {
    /// Mail from a named envelope sender — external MX inbound, or the in-domain
    /// Fauna-user-to-Fauna-user twin. The address is what the gate evaluates.
    Sender(&'a str),
    /// The SMTP null reverse-path `MAIL FROM:<>` — no sender address to key on,
    /// and anyone on the internet can claim it. `dsn_original_msgid` is the
    /// **original Message-ID the report is about**, which the MTA extracts from
    /// a genuine RFC 3464 report's `message/rfc822` / `text/rfc822-headers`
    /// part; the caller matches it against the ward's *sent*-Message-ID set and
    /// passes the outcome as `known_sender` — a bounce of mail the ward really
    /// sent is never gated, while a DSN *costume* is. `None` = not a DSN, no
    /// extractable id, both of which are never known.
    ///
    /// The correlation deliberately keys on the Message-ID and **not** on the
    /// address the report claims to bounce. An address only establishes *"the
    /// ward once mailed that address"* — which anyone may claim, and the first
    /// address a supervised child mails is very often their guardian's public
    /// one. A Message-ID the nest's own client minted is unguessable. (
    /// `family-safety.md` § The mail gate carries the full reasoning and the
    /// residual bound.)
    NullReversePath { dsn_original_msgid: Option<&'a str> },
    /// Mail this nest generated for the recipient. Never gated.
    System,
}

impl<'a> MailIngress<'a> {
    /// Classify an envelope: a named sender, or the null reverse-path with its
    /// optional DSN correlation. Using this constructor keeps `Sender("")`
    /// unrepresentable at every envelope-driven call site.
    pub fn from_envelope(sender_address: &'a str, dsn_original_msgid: Option<&'a str>) -> Self {
        if sender_address.is_empty() {
            Self::NullReversePath { dsn_original_msgid }
        } else {
            Self::Sender(sender_address)
        }
    }
}

/// What the mail gate does with one inbound message for one recipient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailVerdict {
    /// Ordinary placement — the caller's own routing (filter rules, spam
    /// disposition) decides the mailbox.
    Deliver,
    /// Place in the ward's held mailbox and record the envelope sidecar.
    Hold,
    /// Refuse this recipient. Per-recipient by construction: a co-recipient
    /// adult on the same message still receives their copy.
    Reject,
}

/// The mail gate's verdict for one inbound message and one recipient
/// (`family-safety.md` § The mail gate).
///
/// The sibling of [`supervised_reach_verdict`]: same fail-closed shape, but a
/// **separate ingress class**. The reach floor keys on a stored `ContactStatus`
/// between two *actors*; the mail gate keys on whether an *email address* is in
/// the ward's `guardian_mail_allowlist`. They share the shape, never the code.
///
/// Like its sibling it reads only facts the recipient's own nest owns — the
/// stored policy and the stored allowlist — and nothing the *sender* declares
/// about itself. The nest recomputes this at ingest rather than trusting a
/// placement flag carried by the MTA, so the pillar holds against **external
/// senders and non-conforming clients**. It does not — and cannot — hold
/// against a compromised MTA bridge: the bridge is inside the mail TCB (it
/// already supplies the recipient `actor_id`, the sealed body, and the
/// envelope facts this verdict keys on), so the gate can never be stronger
/// than the bridge feeding it, exactly as the reach floor can never be
/// stronger than the client that authenticates.
///
/// `policy` is `None` when the recipient is not supervised. `known_sender` is
/// the caller-resolved correlation fact, and it is a *different* fact per
/// ingress class: for [`MailIngress::Sender`], whether the envelope address is
/// in the ward's `guardian_mail_allowlist`; for [`MailIngress::NullReversePath`],
/// whether the report's original Message-ID is one the ward actually **sent**.
/// Both answer "is this correspondence the ward themselves initiated", but only
/// the second is unforgeable by a party the ward never mailed — see that
/// variant's docs.
pub fn supervised_mail_verdict(
    policy: Option<UnknownSenderMail>,
    known_sender: bool,
    ingress: MailIngress<'_>,
) -> MailVerdict {
    // Nest-generated notices (DSN the nest itself writes, forwarder NDRs,
    // security notices) reach the same ingest core as external mail. Holding
    // one would strand the ward with no way to learn a message failed to send.
    if ingress == MailIngress::System {
        return MailVerdict::Deliver;
    }
    let Some(policy) = policy else {
        return MailVerdict::Deliver;
    };
    // The gate is on *cold* mail only. A correspondent the ward has itself
    // mailed (the outbound auto-seed) is known, so replies always flow — and a
    // null-path message reporting on a Message-ID the ward itself sent is a
    // genuine bounce of the ward's own mail.
    if known_sender {
        return MailVerdict::Deliver;
    }
    match ingress {
        MailIngress::Sender(_) => match policy {
            UnknownSenderMail::Allow => MailVerdict::Deliver,
            UnknownSenderMail::Hold => MailVerdict::Hold,
            UnknownSenderMail::Reject => MailVerdict::Reject,
        },
        // Uncorrelated null-path mail can never be Rejected: RCPT precedes
        // DATA, so DSN-ness is undecidable at the one stage a per-recipient
        // refusal can fire, and a post-DATA refusal could never be bounced to
        // a null path (RFC 5321 — never send a DSN to `<>`). `reject`
        // therefore downgrades to the hold, the strictest verdict that loses
        // no mail — the same rationale as the unrecognized-knob fail-close.
        MailIngress::NullReversePath { .. } => match policy {
            UnknownSenderMail::Allow => MailVerdict::Deliver,
            UnknownSenderMail::Hold | UnknownSenderMail::Reject => MailVerdict::Hold,
        },
        MailIngress::System => unreachable!("handled above"),
    }
}

/// Rate information for posts — used in load hints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostRate {
    pub posts_per_day: f64,
}

/// A delivery receipt confirming a message was received.
///
/// Signed via `fauna_cbor::SignedEnvelope` (sign-over-CID); the envelope
/// ships alongside the canonical bytes in the embed-as-bytes wire shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryReceipt {
    pub post_id: PostId,
    pub recipient: ActorId,
    pub received_at: Timestamp,
}

/// A contact request sent to another actor's inbox topic.
///
/// Signed via `fauna_cbor::SignedEnvelope` (sign-over-CID); the envelope
/// ships alongside the canonical bytes in the embed-as-bytes wire shape.
/// The inbox-POST path is the sole production constructor and verifies
/// signatures via `verify_envelope` (the SMTP bridge's unsigned-payload
/// construction path was removed with that bridge, 2026-05-26).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactRequest {
    pub sender: ActorId,
    pub post_id: PostId,
    #[serde(with = "serde_bytes")]
    pub sender_node: Vec<u8>,
    pub summary: String,
    pub created_at: Timestamp,
}

/// A single inbox entry returned by the BARE inbox route.
///
/// Named `InboxPayload` to stay distinct from the historical inbox-entry types deleted at the I6 cutover.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxPayload {
    pub id: i64,
    pub delivered: bool,
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
}

/// Authorization for a secondary device to act on behalf of an actor.
///
/// Signed via `fauna_cbor::SignedEnvelope` (sign-over-CID); the envelope
/// ships alongside the canonical bytes in the embed-as-bytes wire shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceAuthorization {
    pub actor_id: ActorId,
    #[serde(with = "serde_bytes")]
    pub device_key: [u8; 32],
    pub capabilities: Vec<Capability>,
    pub created_at: Timestamp,
    pub expires_at: Option<Timestamp>,
}

/// Capabilities a device can be authorized to perform.
///
/// ⚠ **Serialized as a plain string, decoded OPEN-SET: an unknown value is
/// preserved verbatim in [`Self::Other`]** (the [`MemberUnattestedReason`]
/// precedent, never a lossy `#[serde(other)]` catch-all) — every verifier of a
/// `DeviceAuthorization` decodes the capability list without failing on a
/// variant a newer build added, so later variants stay additive on every wire
/// that carries a cert (`mls-group-key-material.md` § M2 → *Writer-signed
/// change records* (1)). An unknown variant is simply not the required one.
/// The wire string of each named variant is its Rust name, byte-identical to
/// the derived unit-variant encoding it replaces. Verification always runs
/// over the cert's carried bytes, never a re-encoding, so the round-trip is a
/// convenience for re-registering hosts (`principal_bundle`), not a
/// signature-bearing path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    Post,
    Follow,
    React,
    UpdateProfile,
    ManageSubscribers,
    All,
    /// Exchange a device-key signature for an ordinary session bearer via the
    /// pre-identity `fauna.auth.device_handshake` kind — the per-user sync
    /// agent's app-dead renewal grant (`docs/goal/architecture/apps/
    /// sync-agent.md` § Credential model). Conveys exactly that exchange and
    /// nothing else: a `RenewBearer`-only authorization carries no
    /// post/follow/content capability. Additive variant (2026-07-19): decoded
    /// only by peers that implement the renewal kinds.
    RenewBearer,
    /// Sign file-sync **change records** on the actor's behalf — the record
    /// plane's capability (`devices.md` § Device-signed authoring; the
    /// writer-signed change records ruling). Minted beside `RenewBearer` by the
    /// store-principal enrollment ceremony; implied by [`Self::All`].
    SyncWrite,
    /// A capability this build does not name — a newer build's variant,
    /// preserved verbatim so a decode never fails and a re-encode never
    /// destroys it. Never equal to any named variant, and never a requirement
    /// a verifier asks for.
    Other(String),
}

impl Capability {
    /// The wire string for this capability.
    pub fn as_wire(&self) -> &str {
        match self {
            Self::Post => "Post",
            Self::Follow => "Follow",
            Self::React => "React",
            Self::UpdateProfile => "UpdateProfile",
            Self::ManageSubscribers => "ManageSubscribers",
            Self::All => "All",
            Self::RenewBearer => "RenewBearer",
            Self::SyncWrite => "SyncWrite",
            Self::Other(s) => s,
        }
    }

    /// Whether a grant carrying `self` conveys `required` — equality, or
    /// [`Self::All`]. An [`Self::Other`] conveys nothing this build names and
    /// is never satisfiable as a requirement (fail closed: a caller cannot
    /// accidentally require an unnamed capability a forged cert spells).
    pub fn grants(&self, required: &Capability) -> bool {
        !matches!(required, Self::Other(_)) && (self == required || *self == Self::All)
    }
}

impl From<&str> for Capability {
    fn from(s: &str) -> Self {
        match s {
            "Post" => Self::Post,
            "Follow" => Self::Follow,
            "React" => Self::React,
            "UpdateProfile" => Self::UpdateProfile,
            "ManageSubscribers" => Self::ManageSubscribers,
            "All" => Self::All,
            "RenewBearer" => Self::RenewBearer,
            "SyncWrite" => Self::SyncWrite,
            other => Self::Other(other.to_string()),
        }
    }
}

impl Serialize for Capability {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for Capability {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from(String::deserialize(deserializer)?.as_str()))
    }
}

#[cfg(test)]
mod capability_open_set_tests {
    use super::*;

    /// The derived unit-variant shape the hand-written impl replaced — kept
    /// here only to pin byte-identity with every cert minted before it.
    #[derive(Serialize)]
    enum DerivedShape {
        Post,
        RenewBearer,
        All,
    }

    #[test]
    fn named_variants_encode_byte_identically_to_the_derived_form() {
        for (ours, derived) in [
            (Capability::Post, DerivedShape::Post),
            (Capability::RenewBearer, DerivedShape::RenewBearer),
            (Capability::All, DerivedShape::All),
        ] {
            assert_eq!(
                fauna_cbor::encode_canonical(&ours).unwrap(),
                fauna_cbor::encode_canonical(&derived).unwrap(),
            );
        }
    }

    #[test]
    fn an_unknown_variant_decodes_as_other_and_round_trips_verbatim() {
        let wire = fauna_cbor::encode_canonical(&vec!["RenewBearer", "FutureThing"]).unwrap();
        let caps: Vec<Capability> = fauna_cbor::decode_strict(&wire).expect("open-set decode");
        assert_eq!(
            caps,
            vec![
                Capability::RenewBearer,
                Capability::Other("FutureThing".into())
            ]
        );
        assert_eq!(fauna_cbor::encode_canonical(&caps).unwrap(), wire);
    }

    #[test]
    fn sync_write_is_granted_by_itself_and_all_never_by_renew_bearer_or_other() {
        assert!(Capability::SyncWrite.grants(&Capability::SyncWrite));
        assert!(Capability::All.grants(&Capability::SyncWrite));
        assert!(!Capability::RenewBearer.grants(&Capability::SyncWrite));
        assert!(!Capability::Other("SyncWrite2".into()).grants(&Capability::SyncWrite));
        // An unnamed capability is never satisfiable as a requirement.
        let other = Capability::Other("X".into());
        assert!(!other.grants(&other));
        assert!(!Capability::All.grants(&other));
    }
}

// The dormant `KeyRotation` struct (old_key + new_key, dual-signed by both) was
// retired here in favour of the identity-succession plane in
// `crate::recovery` — `docs/goal/behavior/identity-succession.md` § Don't do
// these. It never had a consumer, never reached a wire, and was never
// persisted, so its removal is compat-vacuous. It must not come back: a thief
// holds the old key, so old+new dual-signing authorizes a takeover rather than
// proving a recovery. `IdentitySuccession` replaces it, authorized by the
// offline-only RecoveryKey the thief provably cannot hold.

/// The one implementation of the latest-wins total order over a
/// `(at_ms, writer)` stamp: later millisecond wins, ties broken by the writer's
/// 32 bytes. Returns a key that orders correctly under plain byte comparison.
///
/// **Why this is a free function in `fauna-core` rather than a method.** More
/// than one type carries the same stamp — the plane-side
/// `fauna_protocol::merge_policy::LwwStamp`, and this crate's own stamped
/// registers (`contact_overlay`) — and no two of them may rank the same pair
/// differently, or two replicas adopt different winners for one entry.
/// `fauna-protocol` depends on `fauna-core` and not the reverse, so the order
/// cannot live on `LwwStamp` where this crate could reach it; it lives here,
/// below both, and every stamp type delegates.
///
/// The `i128` offset maps `i64` onto `u64` so a pre-epoch (negative) stamp still
/// sorts below a positive one; big-endian bytes make numeric and byte order
/// agree. Neither extreme wraps.
pub fn lww_rank(at_ms: i64, writer: [u8; 32]) -> ([u8; 8], [u8; 32]) {
    let shifted = (at_ms as i128 - i64::MIN as i128) as u64;
    (shifted.to_be_bytes(), writer)
}

#[cfg(test)]
mod lww_rank_tests {
    use super::lww_rank;

    const LOW: [u8; 32] = [0u8; 32];
    const HIGH: [u8; 32] = [0xffu8; 32];

    /// Later millisecond wins whatever the writers are — the millisecond is the
    /// leading half of the key, so it outranks the tiebreak.
    #[test]
    fn a_later_millisecond_outranks_an_earlier_one() {
        for (lo, hi) in [
            (i64::MIN, i64::MIN + 1),
            (-5, 5),
            (-1, 0),
            (0, 1),
            (i64::MAX - 1, i64::MAX),
        ] {
            assert!(
                lww_rank(lo, HIGH) < lww_rank(hi, LOW),
                "at_ms {lo} must rank below {hi} even carrying the higher writer"
            );
        }
    }

    /// A pre-epoch stamp sorts below a positive one. The i64→u64 offset is what
    /// makes that true for a big-endian byte comparison — without it a negative
    /// millisecond's two's-complement bytes would sort *above* every positive
    /// one, and a clock-skewed device could never be outranked.
    #[test]
    fn a_pre_epoch_stamp_sorts_below_a_positive_one() {
        assert!(lww_rank(-1, LOW) < lww_rank(0, LOW));
        assert!(lww_rank(i64::MIN, HIGH) < lww_rank(0, LOW));
    }

    /// Equal milliseconds tie-break on the writer bytes, which is what makes the
    /// order total: two replicas ranking the same pair always agree.
    #[test]
    fn equal_milliseconds_tiebreak_on_the_writer() {
        assert!(lww_rank(7, LOW) < lww_rank(7, HIGH));
        assert_eq!(lww_rank(7, LOW), lww_rank(7, LOW));
    }

    /// The extremes do not wrap: `i64::MIN` is the floor of the whole order and
    /// `i64::MAX` its ceiling, so no stamp can be crafted to outrank every other
    /// one by overflowing the offset.
    #[test]
    fn the_extremes_are_the_floor_and_the_ceiling() {
        let floor = lww_rank(i64::MIN, LOW);
        let ceiling = lww_rank(i64::MAX, HIGH);
        for at_ms in [i64::MIN, -1, 0, 1, i64::MAX] {
            for writer in [LOW, HIGH] {
                let r = lww_rank(at_ms, writer);
                assert!(floor <= r, "{at_ms} ranked below the floor");
                assert!(r <= ceiling, "{at_ms} ranked above the ceiling");
            }
        }
    }
}

/// One nest's blessing verdict — one `fauna.state.blessed-nests` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlessedNest {
    /// The nest's Ed25519 identity (32 bytes — the `fauna.nest.info` id the
    /// Nests page keys its rows on).
    #[serde(with = "serde_bytes")]
    pub nest_id: Vec<u8>,
    /// `true` = the user blessed this nest; `false` = they un-blessed it.
    pub blessed: bool,
    /// When the verdict was given, seconds since the Unix epoch — the merge's
    /// last-writer-wins key.
    pub at: u64,
}

impl BlessedNest {
    /// The per-`nest_id` half of [`merge_blessed_nests`] — the join the
    /// `fauna.state.blessed-nests` plane arm runs on one nest's row: `other`
    /// wins iff its `at` is newer, or equal with `other` un-blessing where
    /// `self` blesses (a tie goes to the more restrictive verdict); else
    /// `self`. Both sides are assumed to name the same nest.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        if other.at > self.at || (other.at == self.at && self.blessed && !other.blessed) {
            other.clone()
        } else {
            self.clone()
        }
    }

    /// The write stamp of a user's verdict for `nest_id` at `now`, over this
    /// device's `prior` entry for the nest: `None` when `prior` already
    /// carries `blessed` (re-asserting the current verdict writes nothing);
    /// otherwise the new entry, stamped never at or behind the prior verdict
    /// (`max(now, prior + 1)`), so a toggle always supersedes the state it
    /// was made against, even on a clock that stepped backwards.
    #[must_use]
    pub fn verdict(
        prior: Option<&BlessedNest>,
        nest_id: &[u8],
        blessed: bool,
        now: u64,
    ) -> Option<BlessedNest> {
        match prior {
            Some(p) if p.blessed == blessed => None,
            _ => Some(BlessedNest {
                nest_id: nest_id.to_vec(),
                blessed,
                at: match prior {
                    Some(p) => now.max(p.at.saturating_add(1)),
                    None => now,
                },
            }),
        }
    }
}

/// Merge two sides' blessing lists (`fauna.state.blessed-nests`): per `nest_id` the newer `at`
/// wins, and an equal `at` goes to the **un-blessed** side — blessing is what
/// lets a box's read grants renew without the user, so a tie resolves to the
/// more restrictive verdict. Sorted by `nest_id` so the result is a function of
/// its contents, not of who merged.
pub fn merge_blessed_nests(ours: &[BlessedNest], theirs: &[BlessedNest]) -> Vec<BlessedNest> {
    let mut merged: Vec<BlessedNest> = ours.to_vec();
    for t in theirs {
        match merged.iter_mut().find(|o| o.nest_id == t.nest_id) {
            Some(o) => *o = o.join(t),
            None => merged.push(t.clone()),
        }
    }
    merged.sort_by(|a, b| a.nest_id.cmp(&b.nest_id));
    merged
}

/// One un-adjudicated mark on a carried-across capability grant — a
/// `fauna.state.succession-ledger` row family. `predecessor` is the raising event's
/// retired identity (the ledger the grant was carried across from), recorded so
/// the surface can say *which* succession raised the row and so a later
/// succession legitimately re-raises a kept one — the same
/// (person, raising event) rule as the unattested-member item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantUnattestedMark {
    /// The grant this mark annotates — matches `GrantEvent::grant_id` (16
    /// bytes). Before the re-mint this is the predecessor-minted id; the
    /// re-mint driver re-keys it to the successor-minted replacement.
    #[serde(with = "serde_bytes")]
    pub grant_id: Vec<u8>,
    /// The retired identity whose ledger the grant was carried across from —
    /// the **raising event**, and what makes a second succession legitimately
    /// re-raise a grant a first *Keep* closed.
    pub predecessor: ActorId,
    /// The owner's verdict, or [`UnattestedVerdict::Open`] until they give one.
    /// Decided marks stay at rest; see [`UnattestedVerdict`] for the two
    /// failures deleting them causes.
    #[serde(default)]
    pub verdict: UnattestedVerdict,
}

impl GrantUnattestedMark {
    /// Whether any mark on `grant_id` is still open — the per-row question
    /// every trust surface asks, answered once here so all 7 apps cannot each
    /// invent their own reading of a multi-succession grant.
    pub fn any_open(marks: &[Self], grant_id: &[u8]) -> bool {
        marks
            .iter()
            .any(|m| m.grant_id == grant_id && m.verdict.is_open())
    }
}

/// One un-adjudicated mark on a carried-across **backup destination** — a
/// `fauna.state.backup` row family. The third plane onto the one
/// adjudication encoding (2026-08-11), and keyed like its two siblings:
/// `(the row, the raising event)`, with the owner's verdict at rest.
///
/// Keyed on `destination_id` rather than on the `BackupDestination` row,
/// because one destination the user sees expands to one row per reserved
/// folder kind backed up there — the same grouping `keep`/`edit`/`remove`
/// already operate on, so one mark answers for the whole destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestinationUnattestedMark {
    /// The destination this mark annotates — matches
    /// [`BackupDestination::destination_id`] on every row of that destination.
    pub destination_id: String,
    /// The retired identity whose account state the row was carried across from —
    /// the **raising event**, and what makes a second succession legitimately
    /// re-raise a destination a first *Keep* closed.
    pub predecessor: ActorId,
    /// The owner's verdict, or [`UnattestedVerdict::Open`] until they give one.
    /// Decided marks stay at rest; see [`UnattestedVerdict`] for the two
    /// failures deleting them causes.
    #[serde(default)]
    pub verdict: UnattestedVerdict,
}

impl DestinationUnattestedMark {
    /// Whether any mark on `destination_id` is still open — the per-destination
    /// question, answered once here so all 7 apps cannot each invent their own
    /// reading of a twice-succeeded row. The twin of
    /// [`GrantUnattestedMark::any_open`].
    pub fn any_open(marks: &[Self], destination_id: &str) -> bool {
        marks
            .iter()
            .any(|m| m.destination_id == destination_id && m.verdict.is_open())
    }

    /// **The question every Backups surface asks**: is this row raised and
    /// still unadjudicated? The verdict plane is the only authority: an open
    /// mark on the row's destination raises it.
    ///
    /// Until 2026-09-24 this fell back to a legacy
    /// `BackupDestination::unattested_from_predecessor` stamp on the row — a
    /// downgrade mirror a build older than the mark plane rendered from. The
    /// compat-remnant sweep (`version-compatibility.md` § Dimension 2, program 4)
    /// retired the stamp and the fallback: no pre-sweep build exists to render
    /// from it.
    pub fn row_is_raised(marks: &[Self], dest: &BackupDestination) -> bool {
        Self::any_open(marks, &dest.destination_id)
    }
}

/// One un-adjudicated mark on an **email filter rule** a succession carried
/// across — a `fauna.state.succession-ledger` row family. The **fourth** plane
/// onto the one adjudication encoding, keyed like
/// its three siblings: `(the row, the raising event)`, verdict at rest.
///
/// ⚠ **Two things differ from [`DestinationUnattestedMark`]; do not copy that
/// plane blindly** (`succession-aftermath.md` § Adjudicating what the aftermath
/// carries across, the 2026-08-14 paragraphs).
///
/// **`Removed` here is RECORDED, not enforced.** There the removal *is* a
/// deletion inside a latest-wins record, so the merge has to prune or a stale
/// peer resurrects the row rendering clean. A filter row cannot merge at all —
/// `email_filters` is nest-side SQL under a single authority, one copy every
/// device reads over `fauna.email.filters.list` — so removal happens nest-side
/// through the deletion the filter list already offers, and this verdict is only
/// its record. That is also why the mark rests on the account plane rather than in a column:
/// the nest never acts on the judgment, so a column would buy it nothing while
/// costing the schema + wire + 7-app change the `Reject` ruling already priced
/// and rejected as disproportionate.
///
/// **There is NO row stamp on this plane**: like the destination plane since
/// its stamp retired, the verdict plane is the only authority and no row-stamp
/// fallback may be invented. A client that cannot read the mark renders none at all, which is a
/// *narrower* reading rather than a wrong one, and the fail-visible asymmetry is preserved by the raise being
/// idempotent: a newer client re-reads the same rows and renders them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterUnattestedMark {
    /// The filter row this mark annotates — `EmailFilter::id`, which is a
    /// durable key across the ceremony because the succession re-points
    /// `email_filters.owner` and leaves the primary key alone
    /// (`bins/fauna-nest/src/db/successions.rs`). If that ever became a
    /// delete+reinsert, this keying collapses.
    pub filter_id: i64,
    /// The retired identity whose era the row was carried across from — the
    /// **raising event**, and what makes a second succession legitimately
    /// re-raise a filter a first *Keep* closed.
    pub predecessor: ActorId,
    /// The owner's verdict, or [`UnattestedVerdict::Open`] until they give one.
    /// Decided marks stay at rest; see [`UnattestedVerdict`] for the two
    /// failures deleting them causes.
    #[serde(default)]
    pub verdict: UnattestedVerdict,
}

impl FilterUnattestedMark {
    /// Whether any mark on `filter_id` is still open — the per-row question,
    /// answered once here so all 7 apps cannot each invent their own reading of
    /// a twice-succeeded row. The twin of [`GrantUnattestedMark::any_open`] and
    /// [`DestinationUnattestedMark::any_open`].
    ///
    /// ⚠ This **is** the whole per-row question on this plane — like the
    /// destination plane since its stamp retired, the verdict plane is the only
    /// authority, by ratified design (see the type docs).
    pub fn any_open(marks: &[Self], filter_id: i64) -> bool {
        marks
            .iter()
            .any(|m| m.filter_id == filter_id && m.verdict.is_open())
    }
}

/// One open review item on a person a succession's group sweep could not vouch
/// for — a `fauna.state.succession-ledger` row family.
///
/// The item is the unit of *state*; the **person** is the unit of *decision*.
/// One human raised by two events is two items and one row on every surface,
/// because "do I trust this person" is a single judgment — Keep/Remove closes
/// every open item for them, and a later event brings them back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberUnattestedItem {
    /// The person under review — a group member's actor id, which is often
    /// *not* a contact.
    pub person: ActorId,
    /// The retired identity whose succession raised this item — the same
    /// "raising event" role [`GrantUnattestedMark::predecessor`] plays, and
    /// what makes a second succession re-raise a person a first *Keep* closed.
    pub predecessor: ActorId,
    /// Why this person is under review. One value is produced today; the field
    /// exists from the start because a second producer is already in sight (see
    /// [`MemberUnattestedReason`]).
    pub reason: MemberUnattestedReason,
    /// The owner's verdict, or [`UnattestedVerdict::Open`] until they give
    /// one. Decided items stay at rest so a re-run of the raising sweep cannot
    /// re-ask a question already answered.
    #[serde(default)]
    pub verdict: UnattestedVerdict,
}

/// What the owner decided about one thing the succession aftermath carried
/// across — **one type for every adjudication plane**, because it is one ruling
/// (`succession-aftermath.md` § Re-key scope → *Adjudicating what the aftermath
/// carries across*) and a second encoding of it would be drift, not nuance.
/// Today: [`MemberUnattestedItem`] (a person a group sweep could not vouch for)
/// and [`GrantUnattestedMark`] (a capability grant re-minted out of a
/// predecessor's ledger).
///
/// **Kept at rest, never deleted.** The reflex encoding — presence means open,
/// *Keep* removes the row — makes an answered item indistinguishable from one
/// never raised, which breaks in two independent ways. A **re-run** of whatever
/// raised it (a resumed sweep, the *"finish moving your groups"* retry)
/// re-raises everyone already worked through; and **two of the owner's devices**
/// working the same backlog erase each other, because the only way a deletion
/// can win a merge is a whole-field latest-wins that discards the peer's
/// adjudications wholesale. Carrying the verdict is what makes the merge both
/// lossless and resurrection-free.
///
/// Two adjacent buttons, not a dropdown: *"I recognise this"* is a real judgment
/// rather than a fall-through default, and a dropdown hides it behind a gesture,
/// fights tui, and reads worse to a screen reader. Neither verdict gets a
/// confirm — a group removal is re-invitable, a revoked grant is re-mintable,
/// and a wrong *Keep* stays undoable from the ordinary list forever after.
///
/// Serialized as a plain string with unknown values preserved verbatim, for the
/// reason [`MemberUnattestedReason`] documents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UnattestedVerdict {
    /// Not yet adjudicated — the state every raised item starts in, and the
    /// only one the review surfaces render.
    #[default]
    Open,
    /// The owner recognises it: the person stays in the group, the grant keeps
    /// its capability.
    Kept,
    /// The owner does not: the person was removed from the groups that raised
    /// them, the grant was revoked.
    Removed,
    /// A verdict this build does not name (a newer build's value). Preserved
    /// verbatim on re-seal, and rendered **as still open** — re-asking a
    /// question is harmless, whereas silently hiding a flagged person is the
    /// failure this whole surface exists to prevent.
    Other(String),
}

impl UnattestedVerdict {
    /// The wire string for this verdict.
    pub fn as_wire(&self) -> &str {
        match self {
            Self::Open => "open",
            Self::Kept => "kept",
            Self::Removed => "removed",
            Self::Other(raw) => raw.as_str(),
        }
    }

    /// Whether a review surface still asks about this item. Only [`Self::Open`]
    /// does; an unrecognized verdict is treated as open (fail-visible) — a
    /// re-asked question is harmless, a silently hidden one is the failure every
    /// adjudication surface exists to prevent.
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open | Self::Other(_))
    }

    /// Whether this verdict is the owner's own decision, rather than a question
    /// still waiting for one.
    ///
    /// ⚠ **Not the merge rule.** It used to be — "a decided verdict beats
    /// `Open`" was implemented as this predicate — but the rule outgrew a
    /// boolean once the two *both-decided* arms had to converge too, and it now
    /// lives in one place as a precedence order
    /// (`succession_ledger::verdict_precedence`; the ruling is
    /// `succession-aftermath.md` § Adjudicating what the aftermath carries
    /// across). Two encodings of one rule is the drift that section names, so
    /// read this as the plain predicate it says it is and never re-derive the
    /// merge from it.
    pub fn is_decided(&self) -> bool {
        !matches!(self, Self::Open)
    }
}

impl From<&str> for UnattestedVerdict {
    fn from(raw: &str) -> Self {
        match raw {
            "open" => Self::Open,
            "kept" => Self::Kept,
            "removed" => Self::Removed,
            other => Self::Other(other.to_string()),
        }
    }
}

impl Serialize for UnattestedVerdict {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for UnattestedVerdict {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from(String::deserialize(deserializer)?.as_str()))
    }
}

/// Why a person carries an open review item.
///
/// ⚠ **Serialized as a plain string, with unknown values preserved verbatim in
/// [`Self::Other`]** — deliberately *not* the `#[serde(other)] Unknown` shape
/// [`crate::obligation::ContentFloor`] uses. That shape is safe there because
/// its write path never serializes the catch-all back; here **every
/// re-write of the record writes every field back**, so a lossy catch-all would let an older
/// build silently rewrite a newer build's reason. Round-tripping the raw string
/// is what keeps this field additive-everywhere per
/// `version-compatibility.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberUnattestedReason {
    /// The person was in one of the owner's groups during the compromise window
    /// a succession closed. The sweep re-points what it can and reports the
    /// rest: it added exactly one leaf, so it cannot distinguish an honest
    /// member from a leaf the thief seated, and a leaf carries no join date.
    CompromiseWindow,
    /// A reason this build does not name — either a newer build's value, or the
    /// foreseen second producer: the witness's *unverifiable* arm, where a
    /// peer's identity change could not be confirmed against their registration
    /// chain. Preserved verbatim so a re-seal never destroys it.
    Other(String),
}

impl MemberUnattestedReason {
    /// The wire string for this reason.
    pub fn as_wire(&self) -> &str {
        match self {
            Self::CompromiseWindow => "compromise_window",
            Self::Other(raw) => raw.as_str(),
        }
    }
}

impl From<&str> for MemberUnattestedReason {
    fn from(raw: &str) -> Self {
        match raw {
            "compromise_window" => Self::CompromiseWindow,
            other => Self::Other(other.to_string()),
        }
    }
}

impl Serialize for MemberUnattestedReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for MemberUnattestedReason {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from(String::deserialize(deserializer)?.as_str()))
    }
}

/// One person's open review, as every unattested-member surface renders it —
/// see `SuccessionLedger::open_member_reviews`.
///
/// A *projection*, never stored: the at-rest unit is the
/// [`MemberUnattestedItem`], and collapsing items to people here is what keeps
/// all 7 apps from each inventing their own grouping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberReview {
    /// The person the user is being asked about.
    pub person: ActorId,
    /// Every distinct reason raised against them, in first-raised order.
    pub reasons: Vec<MemberUnattestedReason>,
}

/// How many [`RefusedSchedulingChange`] rows one author may hold at once.
///
/// A refusal row is **peer-caused** — someone else's message is what mints it —
/// so without a per-author ceiling the cheapest flood in the design wins: a
/// creating `REQUEST` carrying a spoofed `ORGANIZER` is refused for a UID the
/// recipient has never seen, and its UID is the sender's to choose, so one
/// author can mint unbounded distinct keys and push every other author's row
/// out of the global ceiling below. Three keeps one author legible (what they
/// tried, on up to three events) while leaving the surface room for everyone
/// else. Repeat attempts on one event do not consume rows at all — they collapse
/// onto the row's `occurrences` (see [`RefusedSchedulingChange`]).
pub const MAX_REFUSED_CHANGES_PER_AUTHOR: usize = 3;

/// How many [`RefusedSchedulingChange`] rows the fleet keeps in all.
///
/// The list is a **notice surface, not an audit log**: it exists so the user
/// learns someone tried to change their calendar, and 20 rows is far past what
/// a human reads on a page they opened to see their week.
///
/// ⚠ **This ceiling evicts the OLDEST rows, deliberately unlike
/// [`MAX_PEER_ANCHOR_ENTRIES`], which refuses rather than evicts.** The two
/// vectors fail in opposite directions. An anchor is a *comparand* a later
/// check reads, so dropping an old one silently downgrades that check — there,
/// eviction is the attacker's primitive. A refusal row is a *report to the
/// user*, and its failure is a surface that has gone blind: keep-oldest would
/// let one flood fill the list permanently and hide every later attempt,
/// including a different attacker's. Keeping the newest bounds what a flood can
/// hide to rows the user has already had the chance to read, and
/// [`MAX_REFUSED_CHANGES_PER_AUTHOR`] stops a single author from reaching even
/// that far.
pub const MAX_REFUSED_SCHEDULING_CHANGES: usize = 20;

/// The most bytes a [`RefusedSchedulingChange::summary`] keeps — a title, not a
/// document.
///
/// The count ceilings above bound rows, never bytes, and the summary is the
/// field a stranger writes: on a refused creating `REQUEST` it is the message's
/// own `SUMMARY`, copied verbatim off an iMIP that may ride an envelope near the
/// 2 MiB frame. Unbounded, one such message pins the account's
/// `fauna.state.refused-scheduling-changes` row against the plane's per-entry
/// byte cap, and from then on every write that
/// grows it is refused — a client-unrecoverable state a stranger reached
/// (`nest/common.md` § Client-state recoverability). Cut on a char boundary, so
/// a title in any script survives as a readable prefix.
pub const MAX_REFUSED_CHANGE_SUMMARY_BYTES: usize = 256;

/// The most bytes each of a [`RefusedSchedulingChange`]'s token fields keeps —
/// `uid_hash` (64-hex by construction), `method` (an iTIP verb) and `reason` (a
/// wire token). Every honest value is well under it; the bound exists for the
/// one that is not.
pub const MAX_REFUSED_CHANGE_TOKEN_BYTES: usize = 64;

/// The most bytes a [`RefusedSchedulingChange::author_home_nest_url`] keeps.
/// The recipient's own nest stamps it (never the sender), so this is hygiene
/// rather than the security bound the summary's is — but a row states its
/// whole worst case, not most of it.
pub const MAX_REFUSED_CHANGE_URL_BYTES: usize = 512;

/// The most bytes a [`RefusedSchedulingChange::sender_address`] may carry —
/// RFC 5321 §4.5.3.1.3's path ceiling with the angle brackets off, the same
/// ceiling the delivery stamp it is copied from holds
/// (`fauna_mail::sender_auth::MAX_AUTHENTICATED_SENDER_BYTES`). A longer or
/// non-printable value is **cleared**, never cut: a prefix of an address names
/// nobody the door authenticated.
pub const MAX_REFUSED_CHANGE_ADDRESS_BYTES: usize = 254;

/// The stated byte budget of the whole refused-change list (the one
/// `fauna.state.refused-scheduling-changes` row): what
/// [`MAX_REFUSED_SCHEDULING_CHANGES`] rows, each at every field's ceiling, may
/// add to the encoded row — 32 KiB, well inside the plane's per-entry byte cap
/// (`caldav-server.md` § *Where the record rests* →
/// **Retention**).
///
/// Not enforced by measuring: it follows from the per-field ceilings, and the
/// `const` assertion below keeps the arithmetic honest when one of them moves.
/// The row's `extra` catch-all is outside it by design — only the user's own
/// builds write it (the recorder mints every row with it empty), so no peer
/// reaches it.
pub const REFUSED_SCHEDULING_CHANGES_BYTE_BUDGET: usize = 32 * 1024;

/// A generous per-row allowance for everything a row encodes besides its
/// strings' contents — the field names, the two timestamps and two counters,
/// and the CBOR framing.
const REFUSED_CHANGE_ROW_FRAMING_BYTES: usize = 256;

const _: () = assert!(
    MAX_REFUSED_SCHEDULING_CHANGES
        * (MAX_REFUSED_CHANGE_SUMMARY_BYTES
            + 3 * MAX_REFUSED_CHANGE_TOKEN_BYTES
            + 64 // `author`: a 64-hex actor id or nothing
            + MAX_REFUSED_CHANGE_URL_BYTES
            + MAX_REFUSED_CHANGE_ADDRESS_BYTES
            + REFUSED_CHANGE_ROW_FRAMING_BYTES)
        <= REFUSED_SCHEDULING_CHANGES_BYTE_BUDGET
);

/// One **refused inbound scheduling change** — someone whose message may not
/// make the change it asked for tried to cancel, rewrite, or answer for an
/// event on this user's calendar, and the client refused it
/// (`docs/goal/behavior/caldav-server.md` § Who may mutate an existing event
/// over the inbound rail → *Surfacing*).
///
/// **Informational, never actionable.** The ruling forbids an "apply anyway"
/// affordance — a user cannot adjudicate an authority question the client could
/// not — so the only gesture a row carries is *dismiss*.
///
/// **Why it rests in account state (the plane's
/// `fauna.state.refused-scheduling-changes` row) rather than on the device that
/// refused it.**
/// A sealed scheduling delivery is a one-off MLS group built from *one* of the
/// recipient's key packages, so exactly one of the user's devices ever drains a
/// given message and refuses it. Per-device storage would therefore show the
/// notice on whichever device happened to hold that key package and nowhere
/// else, while the change the message tried to make — had it been allowed —
/// would have shown up on every device, since the calendar is account state.
/// The principal this surface protects is the user, not a device, so the record
/// follows the peer chain heads' (`fauna.state.peer-anchors`) reasoning: *a head seen on the
/// phone should anchor the laptop*.
///
/// **Keyed `(uid_hash, author, method, reason)`** — [`Self::key`]. Repeat
/// attempts on one event collapse onto the same row and bump
/// [`Self::occurrences`], so "mallory tried 47 times" is one row with a count
/// rather than 47 rows; it is also what makes a re-drain of the same record a
/// no-op, which the inbound drain's idempotence needs (its cursor is
/// per-session, so a relaunch can walk a channel again).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedSchedulingChange {
    /// 64-hex `uid_hash` of the event the message addressed — the same id the
    /// calendar store keys events by, so a surface can link the row to the
    /// event when one exists. (A refused *creating* `REQUEST` names a UID no
    /// event was ever stored under.)
    pub uid_hash: String,
    /// The refused author: 64-hex actor id **as the home nest attested it**,
    /// or `None` when the nest attested no author at all
    /// ([`RefusalReason::NoAttestedAuthor`](https://docs.rs/fauna-client-caldav)).
    ///
    /// ⚠ **Never read off the message.** A co-attendee's forged `CANCEL`
    /// carries the real organizer's `ORGANIZER` line verbatim — that is the
    /// whole reason the rule exists — so a row built from the `.ics` would
    /// name the victim as the culprit. The handle is resolved for display from
    /// this id, never stored: a handle cached at refusal time goes stale
    /// exactly when it matters, the same reason
    /// `ConversationsManager::handle_for_person` re-derives instead of caching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// The channel's home-nest base URL as the recipient's own nest stamped it;
    /// empty = the recipient's own nest. Half of the principal the rule
    /// compares, so a row names the *pair*, never a bare actor id.
    #[serde(default)]
    pub author_home_nest_url: String,
    /// The email address the **delivery door authenticated** as the sender of
    /// a refused mailed `REPLY` — the value of the sealed copy's
    /// `X-Fauna-Authenticated-Sender` stamp (`caldav-server.md` § Who may
    /// mutate an existing event over the inbound rail → *The mail rail*).
    /// Empty for the sealed scheduling rail (which names an actor in
    /// [`Self::author`] instead) and for a mailed copy that carried no stamp
    /// (`sender_unauthenticated` — nothing names anyone).
    ///
    /// ⚠ **Never read off the message's `From:`** — for the reason
    /// [`Self::author`] is never read off the `.ics`: a line the sender writes
    /// would let a spoofer name the victim. It stands in for `author` in
    /// [`Self::key`] and the per-author ceiling when `author` is `None`, so two
    /// different spoofers make two rows and one spoofer's flood collapses.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sender_address: String,
    /// The iTIP `METHOD` that was refused — `REQUEST`, `CANCEL` or `REPLY`,
    /// upper-cased. A plain string because it is the wire's own vocabulary and
    /// an unrecognized value must render rather than fail to decode.
    pub method: String,
    /// Why it was refused — the `RefusalReason::as_wire` vocabulary of
    /// `fauna-client-caldav`, which owns the decision this records.
    ///
    /// A plain string for the reason [`MemberUnattestedReason`] documents at
    /// length: every re-write of the record writes every field back, so a build
    /// that does not name a newer reason must round-trip it verbatim rather
    /// than lose it. A renderer that cannot name it says only that the change
    /// was refused.
    pub reason: String,
    /// The event's title as the **stored** event carries it — what the user
    /// knows the event as, not what the refused message called it. Empty when
    /// no event was stored (a refused creating `REQUEST`), where the message's
    /// own `SUMMARY` is used instead: that text is the sender's, and naming it
    /// is the point of the row.
    #[serde(default)]
    pub summary: String,
    /// When this row was first raised (epoch seconds).
    pub first_refused_at: i64,
    /// When the most recent attempt on this key was refused (epoch seconds) —
    /// what the global and per-author ceilings rank by, and what
    /// [`Self::is_open`] measures a dismissal against.
    pub last_refused_at: i64,
    /// How many attempts on this key the fleet has counted.
    ///
    /// **A lower bound, never a total.** It merges as `max` rather than a sum
    /// because a sum is not idempotent under re-merge — two devices that
    /// exchange configs twice would double it — and a count that inflates on
    /// its own would be worse than one that undercounts.
    #[serde(default)]
    pub occurrences: u32,
    /// The [`Self::occurrences`] value the owner dismissed this row at; `0` =
    /// never dismissed.
    ///
    /// Recording *what was dismissed* rather than a bare flag is what lets a
    /// **later attempt re-open the row**: a dismissal answers the attempts the
    /// user saw, and a new one is new information. It merges as `max` — the
    /// most restrictive verdict wins, exactly as the four adjudication planes
    /// rule ([`UnattestedVerdict`]) — so a device that never saw the dismissal
    /// cannot resurrect the row, while an attempt past the dismissed count
    /// re-opens it on every device.
    #[serde(default)]
    pub dismissed_through: u32,
    /// Forward-compat catch-all: an older build's wholesale record
    /// rewrite must not drop a field a newer build adds to a row
    /// ([`DeploymentSeedEntry::extra`]'s reason, unchanged).
    #[serde(flatten)]
    pub extra: BTreeMap<String, fauna_cbor::Value>,
}

impl RefusedSchedulingChange {
    /// The row's identity — `(uid_hash, author, method, reason)` rendered as one
    /// string, so an app can hand a dismissal back through a single value
    /// (`refused-change-dismiss`) without re-deriving the tuple.
    ///
    /// The author slot is [`Self::refused_party`]: the attested actor, else the
    /// door-authenticated [`Self::sender_address`]. The two cannot collide — a
    /// 64-hex id carries no `@`, an address always does.
    ///
    /// `\u{1f}` (unit separator) joins the parts: it cannot occur in a hex id,
    /// a printable-ASCII address, an iTIP method, or a wire reason token, so no
    /// pair of distinct tuples can collide on one key.
    #[must_use]
    pub fn key(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.uid_hash,
            self.refused_party(),
            self.method,
            self.reason
        )
    }

    /// Who this row refused, as one string: the attested [`Self::author`]
    /// when there is one, else the door-authenticated [`Self::sender_address`]
    /// (a mailed `REPLY`), else `""` (nobody could be named). What the row key
    /// and the per-author ceiling both bucket on, stated once.
    #[must_use]
    pub fn refused_party(&self) -> &str {
        match self.author.as_deref() {
            Some(author) => author,
            None => &self.sender_address,
        }
    }

    /// Whether a surface still shows this row: an attempt has been refused that
    /// the owner has not dismissed.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.occurrences > self.dismissed_through
    }

    /// Hold every string on this row to its byte ceiling, whoever wrote it —
    /// the per-field half of [`REFUSED_SCHEDULING_CHANGES_BYTE_BUDGET`].
    ///
    /// An `author` that is not a lowercase 64-hex actor id becomes `None` (the
    /// no-attested-author rendering) rather than a cut string: a prefix of a
    /// hostile nest's attestation names nobody, and a malformed one names
    /// nobody either.
    pub fn bound_to_budget(&mut self) {
        fn cut(s: &mut String, max: usize) {
            let keep = crate::encoding::truncate_to_char_boundary(s, max).len();
            s.truncate(keep);
        }
        cut(&mut self.summary, MAX_REFUSED_CHANGE_SUMMARY_BYTES);
        cut(&mut self.uid_hash, MAX_REFUSED_CHANGE_TOKEN_BYTES);
        cut(&mut self.method, MAX_REFUSED_CHANGE_TOKEN_BYTES);
        cut(&mut self.reason, MAX_REFUSED_CHANGE_TOKEN_BYTES);
        cut(&mut self.author_home_nest_url, MAX_REFUSED_CHANGE_URL_BYTES);
        // Cleared, never cut: a prefix of an address names nobody, and a
        // non-printable byte (the key's separator included) is no stamp value.
        if self.sender_address.len() > MAX_REFUSED_CHANGE_ADDRESS_BYTES
            || !self
                .sender_address
                .bytes()
                .all(|b| (0x21..=0x7e).contains(&b))
        {
            self.sender_address.clear();
        }
        if self
            .author
            .as_deref()
            .is_some_and(|a| !crate::hex32::is_lowercase_hex64(a))
        {
            self.author = None;
        }
    }

    /// Fold `other` — a row with the same [`Self::key`] — into this one: the
    /// cross-device merge rule, stated once so the merge and the de-duplication
    /// [`cap_refused_scheduling_changes`] does cannot drift apart.
    ///
    /// **Lexicographic on the last attempt: a row that saw a LATER attempt
    /// wins whole.** Only at an equal `last_refused_at` do the two copies
    /// join field by field — `max` of `occurrences` and `dismissed_through`,
    /// `min` of `first_refused_at`, the byte-smaller title — which is where a
    /// dismissal meets its undismissed copy, and the most restrictive verdict
    /// wins. Never a sum: a sum of `occurrences` would inflate every time two
    /// devices exchanged configs. `extra` is a key union at a tie, ours
    /// winning a collision.
    ///
    /// Why the later attempt takes everything (built 2026-09-30, the plane
    /// arm's byte-level join laws demanded it): the ceilings rank rows by
    /// `last_refused_at`, and a row one merge cuts can come back through a
    /// replica whose copy saw a newer attempt. A field-wise join would then
    /// read differently by merge ORDER — the cut copy's counts are gone in one
    /// order and joined in the other — so the capped merge would not be
    /// associative and three devices need not converge. With the later
    /// attempt dominating, whatever the cut copy carried is exactly what the
    /// winning copy would have overridden anyway. What it costs is the count's
    /// history across a concurrent attempt (a lower bound, as it always was),
    /// and what it keeps is the dismissal's semantics: a newer attempt
    /// re-opens the row either way, and a dismissal is never undone by a copy
    /// that saw no attempt after it.
    pub fn absorb(&mut self, other: &Self) {
        match other.last_refused_at.cmp(&self.last_refused_at) {
            std::cmp::Ordering::Greater => {
                *self = other.clone();
                return;
            }
            std::cmp::Ordering::Less => return,
            std::cmp::Ordering::Equal => {}
        }
        if other.summary < self.summary {
            self.summary = other.summary.clone();
        }
        self.occurrences = self.occurrences.max(other.occurrences);
        self.dismissed_through = self.dismissed_through.max(other.dismissed_through);
        self.last_refused_at = self.last_refused_at.max(other.last_refused_at);
        self.first_refused_at = self.first_refused_at.min(other.first_refused_at);
        for (k, v) in &other.extra {
            self.extra.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
}

/// Hold `rows` under both refusal ceilings, newest-last-attempt first within
/// each — the **one** statement of the policy, called by the recorder and by
/// the cross-device merge alike (two encodings of one ceiling is how a merge
/// silently grows past what its writer bounds — see
/// [`MAX_PEER_ANCHOR_ENTRIES`]'s merge-path note).
///
/// Order of the two: per-author first, so one author's flood is cut back before
/// it can crowd other authors out of the global ceiling.
///
/// **The byte ceilings come first of all** ([`RefusedSchedulingChange::bound_to_budget`]
/// on every row), so the merge path and a heal of rows planted before the bound
/// existed hold the same per-field budget the recorder does. Bounding can make
/// two rows' keys equal — two non-hex authors both fall to `None` — so equal
/// keys are then folded by the merge rule ([`RefusedSchedulingChange::absorb`])
/// rather than left to count twice against the ceilings.
pub fn cap_refused_scheduling_changes(rows: &mut Vec<RefusedSchedulingChange>) {
    let mut folded: Vec<RefusedSchedulingChange> = Vec::with_capacity(rows.len());
    for mut row in rows.drain(..) {
        row.bound_to_budget();
        let key = row.key();
        match folded.iter_mut().find(|held| held.key() == key) {
            Some(held) => held.absorb(&row),
            None => folded.push(row),
        }
    }
    *rows = folded;
    // Newest attempt first; the key breaks ties so two devices holding the same
    // rows always cut the same ones.
    rows.sort_by(|a, b| {
        b.last_refused_at
            .cmp(&a.last_refused_at)
            .then_with(|| a.key().cmp(&b.key()))
    });
    let mut per_author: BTreeMap<&str, usize> = BTreeMap::new();
    let mut keep = Vec::with_capacity(rows.len().min(MAX_REFUSED_SCHEDULING_CHANGES));
    for (i, row) in rows.iter().enumerate() {
        let held = per_author.entry(row.refused_party()).or_default();
        if *held < MAX_REFUSED_CHANGES_PER_AUTHOR && keep.len() < MAX_REFUSED_SCHEDULING_CHANGES {
            *held += 1;
            keep.push(i);
        }
    }
    let kept: std::collections::BTreeSet<usize> = keep.into_iter().collect();
    let mut i = 0;
    rows.retain(|_| {
        let k = kept.contains(&i);
        i += 1;
        k
    });
    // Stored in key order, so an unchanged set re-seals to identical bytes.
    rows.sort_by_cached_key(RefusedSchedulingChange::key);
}

/// **The refused-change list as one value** — the content of the
/// `fauna.state.refused-scheduling-changes` row, a type of its own so every
/// fold shares one merge. [`Self::merge`] is the shipped rule. Encodes as a
/// CBOR array of rows, the plane row's own shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RefusedSchedulingChanges {
    /// The rows, in key order once merged or capped.
    pub rows: Vec<RefusedSchedulingChange>,
}

impl RefusedSchedulingChanges {
    /// The per-key union through [`RefusedSchedulingChange::absorb`], then
    /// held under the ceilings by [`cap_refused_scheduling_changes`] — the rule
    /// the plane arm runs for the kind, `self` in the local replica's
    /// seat.
    ///
    /// The ceilings are re-asserted on the MERGE path for the reason the anchor
    /// vectors state: what one replica may add does not bound the union of D
    /// devices' disjoint rows. The byte ceilings ride along, so a row a
    /// pre-bound replica planted is trimmed here too.
    #[must_use]
    pub fn merge(&self, other: &Self) -> Self {
        let mut merged = self.rows.clone();
        for theirs in &other.rows {
            let key = theirs.key();
            match merged.iter_mut().find(|ours| ours.key() == key) {
                Some(ours) => ours.absorb(theirs),
                None => merged.push(theirs.clone()),
            }
        }
        cap_refused_scheduling_changes(&mut merged);
        Self { rows: merged }
    }
}

/// Whether `person` carries an open review in a roster an app is **holding** —
/// the per-row question a group member list and a contacts list each ask once
/// per rendered row.
///
/// The twin of `SuccessionLedger::member_is_unattested`, for the surfaces that
/// cannot hold a whole ledger: a member list paints far more often than the
/// ledger changes, so every app caches `SuccessionLedger::open_member_reviews`
/// and answers from the cache. It exists **so that no app writes the join
/// itself** — an inline `roster.iter().any(|r| r.person == p)` in seven
/// renderers is seven places for the flag to silently stop appearing, and the
/// failure mode of this whole surface is a flagged person who renders
/// unflagged.
pub fn is_under_review(roster: &[MemberReview], person: &ActorId) -> bool {
    roster.iter().any(|review| &review.person == person)
}

/// [`is_under_review`] for a caller holding the person as hex, not a decoded
/// [`ActorId`] — the shape every per-row contact/member list actually has on
/// hand (a wire row's peer id is a hex string). tui's and linux's contacts
/// lists each carried this identical decode-then-join before the lift; an
/// undecodable hex answers `false` rather than panicking — a real contact is
/// always keyed by a real actor id, but a defensive caller should never crash
/// on one that somehow isn't.
pub fn is_under_review_hex(roster: &[MemberReview], person_hex: &str) -> bool {
    ActorId::from_hex(person_hex).is_ok_and(|person| is_under_review(roster, &person))
}

/// The text parts of one review row, for every surface that renders one.
///
/// Deliberately *parts* rather than one finished sentence. The row reads
/// `{who} — {reason}` (`settings.recovery_kit.review_row`) and both halves are
/// translatable, but `who` is frequently a user-supplied handle:
/// [`crate::localized::LocalizedText::resolve_args`] would translate a handle
/// that happened to equal an i18n key — a hazard its own documentation calls
/// out — so composition is left to the caller, which resolves each part with
/// plain [`crate::localized::LocalizedText::resolve`] and joins the reasons.
/// That also lets the join survive a person carrying several reasons, which a
/// single template argument cannot express.
///
/// It exists for the reason [`is_under_review`] does, one level up: the rules
/// encoded here are subtle, their failure mode is a row that silently says
/// *less* than it should, and seven renderers re-deriving them is seven
/// chances to get one wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MemberReviewRowText {
    /// Who the row is about — the handle verbatim when one resolves, and
    /// otherwise the "no longer in any of your groups" key. A person the app
    /// can no longer name is still a person the owner has to decide about, so
    /// the row is named rather than dropped.
    pub who: LocalizedText,
    /// One entry per distinct reason, in first-raised order. Never empty for a
    /// review that reached a surface, and **an unrecognised reason is carried,
    /// not filtered**: hiding the row would be exactly the silent
    /// disappearance the fail-visible rule exists to prevent.
    pub reasons: Vec<LocalizedText>,
}

/// Build the display parts of `review`'s row, given whatever handle the caller
/// could resolve for the person (`None` when they are no longer nameable).
///
/// ⚠ **Behaviour-preserving lift of tui's original renderer** — the one
/// wrinkle worth knowing is that every [`MemberUnattestedReason::Other`],
/// whatever string it carries, renders as the *witness-arm* wording ("could
/// not confirm their identity"). That is right for the one producer foreseen
/// when the copy was written and merely approximate for a genuinely unknown
/// reason a newer build wrote. Rendering something is the ratified behaviour;
/// giving a future reason its own copy is a design change, not a refactor, so
/// it is left alone here rather than silently altered.
pub fn review_row_text(review: &MemberReview, handle: Option<&str>) -> MemberReviewRowText {
    MemberReviewRowText {
        who: match handle {
            Some(name) => LocalizedText::key(name),
            None => LocalizedText::key("settings.recovery_kit.review_unknown_person"),
        },
        reasons: review
            .reasons
            .iter()
            .map(|reason| {
                LocalizedText::key(match reason {
                    MemberUnattestedReason::CompromiseWindow => {
                        "settings.recovery_kit.review_reason_compromise"
                    }
                    MemberUnattestedReason::Other(_) => "settings.recovery_kit.review_reason_other",
                })
            })
            .collect(),
    }
}

/// Render [`MemberReviewRowText::reasons`] into one line, in raise order — a
/// join and deliberately nothing more. *Which* reasons appear — including
/// that a reason this build cannot name is still rendered rather than
/// dropped — is decided once by [`review_row_text`], so every renderer
/// downstream inherits that instead of re-deriving it.
pub fn reason_text<F, S>(reasons: &[LocalizedText], lookup: F) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    reasons
        .iter()
        .map(|r| r.resolve(&lookup))
        .collect::<Vec<_>>()
        .join(", ")
}

/// **The count ceiling on each per-peer anchor vector** —
/// the two vectors of `fauna.state.peer-anchors`, `chain_heads` and
/// `anchor_domains` (`identity-succession.md` § The succession statement → *the peer-profile
/// harvest*, rule 5's count axis).
///
/// The length axis alone does not bound the store. Both vectors are filled by
/// a background sweep with **no human in the loop**, merge by per-actor
/// **union**, and have no prune, no eviction and no app affordance to remove an
/// entry — while the account store they rest in is byte-bounded
/// (`MAX_STATE_ENTRY_BYTES` per entry). Bounding the host at
/// [`crate::web::MAX_HOSTNAME_BYTES`] shrank each entry; it did not bound the
/// count, and on a large shared nest four thousand same-nest peers sharing a
/// thread is a population, not an attack budget. An unbounded store on every
/// device, permanently, with no in-app repair is the client-causable
/// unrecoverable state `nest/common.md` § Client-state recoverability forbids (its
/// per-object-remedy carve-out does not apply: condition 2 wants an ordinary
/// in-app flow reaching equivalent function, and there is none).
///
/// **Why the number.** Worst case per entry is 285 B of domain vector
/// (32 B actor + a 253 B host) and 72 B of head vector (32 B actor + 32 B
/// pubkey + 8 B seq), so 256 entries per vector bound the pair at
/// ~89 KiB across both vectors. 256 same-nest group peers is generous for the honest population:
/// the sweep attempts every Fauna row (since 2026-09-15 — a name is no longer
/// evidence the owner typed it, and the head it seeds is the offline anchor
/// no handle replaces), but `fauna.profile.get` answers only for identities
/// homed on the member's own nest, so reach, not a skip, bounds who reaches
/// here: the same-nest peers the member shares any thread with.
///
/// **Refusal, never eviction — deliberate.** These entries are first-write-
/// wins, which makes the OLDEST ones the most likely to be load-bearing, so a
/// "keep the newest N" policy would invert the value order *and* hand an
/// attacker an eviction primitive: publish entries until the honest anchors
/// are pushed out. A full vector therefore refuses new seeds and keeps what it
/// holds — and ONLY new seeds: a full vector still demotes a head it
/// already holds ([`PeerAnchorRefusal::StoreFull`]), because the outrun mark
/// takes no slot and a store that could be filled to silence it would hand
/// a retired RecoveryKey its offline shortcut back. The residual that
/// leaves, stated plainly: a peer who fills the vector first denies *future*
/// anchoring, and statements about un-anchored
/// peers degrade to the bare add — the same graceful degrade rule 3 already
/// concedes for a peer with no `nests` entry or one homed on another nest.
/// That is a bounded loss of function, not a brick, and it is the trade this
/// ceiling deliberately takes.
///
/// **The merge path holds the same value order, by a key no peer chooses.**
/// The writers bound what ONE replica may add; two devices holding disjoint
/// full vectors still union past the ceiling, so `PeerAnchors::merge`
/// re-asserts it by keeping the `MAX_PEER_ANCHOR_ENTRIES` entries with the
/// smallest `(first_seen, actor)` — the fleet's OLDEST anchors, the writers'
/// own first-write-wins order carried onto the merge. The ordering key is
/// [`PeerChainHead::first_seen`] / [`PeerAnchorDomain::first_seen`], stamped
/// by the owner's device at seed time and merged as the per-actor minimum; the
/// actor id is only the equal-stamp tiebreak. It used to be the actor id
/// alone, and an actor id is a value the *peer* mints — grinding one that
/// sorts first is seconds of work, so a same-nest attacker with a ground
/// batch on one replica displaced the honest anchors held on the other at the
/// next merge: the eviction primitive this paragraph refuses, reached through
/// the merge instead of the writer. What a peer can
/// still do is be *early* — the same residual the writer already concedes.
///
/// **The stamp never heals a pre-existing entry.** Nothing re-stamps an
/// entry once written — only a first write sets `first_seen`, and an
/// advance keeps it — so every entry that has ever decoded at the epoch
/// sits in that cohort permanently, ordered among its own members by actor
/// id alone; once a fleet's per-actor union holds this many such entries, no
/// newly-stamped anchor can ever win a merge slot again
/// (`identity-succession.md` § The succession statement → *the peer-profile
/// harvest*, rule 5, the count axis).
pub const MAX_PEER_ANCHOR_ENTRIES: usize = 256;

/// One remembered chain head for another identity — see
/// [`PeerAnchors::chain_heads`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerChainHead {
    /// The identity this head is about.
    pub actor: ActorId,
    /// The RecoveryKey registered at the head this fleet has seen.
    #[serde(with = "serde_bytes")]
    pub recovery_pubkey: Vec<u8>,
    /// The `seq` that head sat at. Monotonic: a merge keeps the higher one.
    pub seq: u64,
    /// When this fleet first anchored `actor` — the merge ceiling's ordering
    /// key (see [`MAX_PEER_ANCHOR_ENTRIES`]). Stamped by the owner's device at
    /// the first write and never re-stamped by an advance; merged as the
    /// per-actor **minimum**, so the earliest sighting on any device wins.
    /// Additive: an entry written before the field existed decodes as the
    /// epoch, which sorts **oldest** — the upgrade itself must never evict the
    /// honest back-catalogue, and the epoch is what a peer cannot mint.
    #[serde(default, skip_serializing_if = "Timestamp::is_epoch")]
    pub first_seen: Timestamp,
    /// The peer's own directly-signed profile has claimed a head **past** this
    /// one, so this head no longer settles a statement offline — it still
    /// guards the walk (`identity-succession.md` § The succession statement →
    /// *What a held head may settle offline*). Set only by the harvest door
    /// ([`PeerAnchors::mark_chain_head_outrun`]), cleared only by a verified
    /// walk advancing the head ([`PeerAnchors::remember_chain_head`]); merged
    /// as an OR between replicas holding the **same** head. Additive: absent
    /// decodes as `false`, and an unmarked head encodes exactly as it did
    /// before the field existed. The `fauna.state.peer-anchors` rows decode
    /// strictly (`peer_anchor_rows::decode_exact`), so a decoder lacking the
    /// field refuses a marked head rather than stripping the mark — the blob
    /// era's "a pre-field device drops the mark until the next harvest" is
    /// closed (`identity-succession.md` § The succession statement → *What a
    /// held head may settle offline*, residual (5)).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub outrun: bool,
}

/// One harvested home-domain anchor for a peer identity —
/// [`PeerAnchors::anchor_domains`]' element. First-write-wins per actor;
/// see the field doc for the merge rule and the trust grade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerAnchorDomain {
    /// The identity this anchor is about.
    pub actor: ActorId,
    /// The host of the peer's self-asserted home-nest URL (`Profile.nests[0]`),
    /// scheme/port/path stripped — the string the witness's
    /// `walk_from_domain` resolves and dials.
    pub domain: String,
    /// When this fleet first anchored `actor` — same contract as
    /// [`PeerChainHead::first_seen`]: the merge ceiling's ordering key, stamped
    /// at the first write, merged as the per-actor minimum, epoch when absent.
    #[serde(default, skip_serializing_if = "Timestamp::is_epoch")]
    pub first_seen: Timestamp,
}

/// **The peer-anchor cache as one value** — the chain heads and the anchor
/// domains together, the two vectors `config-dissolution.md` places in ONE
/// kind (`fauna.state.peer-anchors`) because they share one ceiling
/// ([`MAX_PEER_ANCHOR_ENTRIES`]) and one order ([`peer_anchor_order`]) — P2's
/// invariant closure. [`Self::merge`] is the shipped rule, which the plane
/// rows (`crate::peer_anchor_rows`, which fold through it) share.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerAnchors {
    /// The per-actor remembered chain heads.
    pub chain_heads: Vec<PeerChainHead>,
    /// The per-actor harvested home-domain anchors.
    pub anchor_domains: Vec<PeerAnchorDomain>,
}

impl PeerAnchors {
    /// The two per-actor unions, each held under [`MAX_PEER_ANCHOR_ENTRIES`]
    /// by [`peer_anchor_order`] — the rule the plane fold runs for the
    /// two vectors, `self` in the local replica's seat.
    #[must_use]
    pub fn merge(&self, other: &Self) -> Self {
        // Chain heads: per-actor union, higher `seq` wins. These are monotonic
        // facts about what this fleet has seen, and the guard they feed refuses
        // a chain that rewinds a seen head; a latest-wins rule would let a
        // device whose replica is behind *lower* another device's anchor,
        // handing an attacker exactly the reset the guard exists to deny. Same
        // rule the nest's `foreign_recovery_heads` UPSERT implements
        // (`MAX(seq)`), and the same monotonicity
        // `SuccessionAnchors::remember_head` promises in memory.
        let chain_heads = {
            let mut merged: Vec<PeerChainHead> = self.chain_heads.clone();
            for theirs in &other.chain_heads {
                match merged.iter_mut().find(|ours| ours.actor == theirs.actor) {
                    Some(ours) => ours.join_from(theirs),
                    None => merged.push(theirs.clone()),
                }
            }
            merged.sort_by_key(peer_anchor_order);
            // The count ceiling, re-asserted on the MERGE path. The seed
            // writers bound what one replica may add, and that alone does not
            // bound the union: two devices holding 256 *disjoint* peers each
            // merge to 512, and a fleet of D devices to D×256, walking the
            // sealed blob back toward the 1 MiB cap this ceiling exists to keep
            // it under.
            //
            // What survives is the fleet's OLDEST anchors — smallest
            // `(first_seen, actor)`, see `peer_anchor_order` — which is the
            // same first-write-wins value order the writers hold, carried onto
            // the merge. It is **convergent**, and convergence is the property
            // the merge cannot trade away: the per-actor merge is a join (max
            // seq, min stamp) under which an entry's key can only fall, and
            // "keep the k smallest" over such a union is associative — an entry
            // outside the k smallest of X stays outside the k smallest of X∪Y,
            // since every entry below it in X is still below it there — so no
            // merge ORDER lets a dropped entry survive and a three-device fleet
            // cannot oscillate.
            //
            // The key used to be the actor id alone, and that was the writers'
            // refused eviction primitive reached from the other side: an actor
            // id is minted by the PEER, and grinding one that sorts first is
            // seconds of work, so a same-nest attacker with a ground batch
            // harvested on one replica displaced the honest anchors held on the
            // other at the next merge — silently, with no `StoreFull` and no
            // event. The stamp is written by the owner's own device, so what a
            // peer can still do is be *early*, which is exactly the residual
            // the writer already concedes.
            merged.truncate(MAX_PEER_ANCHOR_ENTRIES);
            merged
        };

        // Harvested anchor domains: per-actor union, lexically-smaller wins a
        // same-actor conflict — arbitrary but convergent (see the field doc on
        // `PeerAnchors::anchor_domains`). Never a preference: a conflict is
        // already inside the harvest's conceded TOFU window. `first_seen`
        // merges as the per-actor minimum, as for the chain heads.
        let anchor_domains = {
            let mut merged: Vec<PeerAnchorDomain> = self.anchor_domains.clone();
            for theirs in &other.anchor_domains {
                match merged.iter_mut().find(|ours| ours.actor == theirs.actor) {
                    Some(ours) => ours.join_from(theirs),
                    None => merged.push(theirs.clone()),
                }
            }
            merged.sort_by_key(peer_anchor_order);
            // Same ceiling, same convergent oldest-survive rule as the chain
            // heads above.
            merged.truncate(MAX_PEER_ANCHOR_ENTRIES);
            merged
        };

        Self {
            chain_heads,
            anchor_domains,
        }
    }
}

impl PeerAnchors {
    /// The chain head this fleet has seen for `actor`, if any — the durable half
    /// of the witness's `known_head` anchor.
    pub fn known_chain_head(&self, actor: &ActorId) -> Option<crate::recovery::ChainHead> {
        self.chain_heads
            .iter()
            .find(|entry| &entry.actor == actor)
            .and_then(|entry| {
                let pubkey: [u8; 32] = entry.recovery_pubkey.as_slice().try_into().ok()?;
                Some(crate::recovery::ChainHead::new(pubkey, entry.seq))
            })
    }

    /// Remember `head` for `actor`, **monotonically**: a lower `seq` never
    /// displaces a higher one.
    ///
    /// That single rule is the whole value of the cache — a remembered head is
    /// what lets a member refuse a chain that *rewrites or truncates* what it
    /// already saw, and a rule that let a head go backwards would hand an
    /// attacker the reset they need. Returns whether anything changed, so a
    /// caller can skip a merge through the plane door that would write nothing.
    pub fn remember_chain_head(
        &mut self,
        actor: ActorId,
        head: crate::recovery::ChainHead,
    ) -> bool {
        match self
            .chain_heads
            .iter_mut()
            .find(|entry| entry.actor == actor)
        {
            Some(entry) if entry.seq >= head.seq => false,
            // An advance keeps `first_seen`: the stamp records when this fleet
            // first anchored the actor, and a verified walk moving the head
            // forward is not a new sighting — re-stamping would make the
            // longest-held anchors look newest to the merge ceiling.
            // It also clears `outrun`: the mark says "the profile claims a head
            // past THIS one", and the walk just replaced this one with a head
            // it verified. If the profile still claims past the new head, the
            // next harvest marks it again.
            Some(entry) => {
                entry.recovery_pubkey = head.recovery_pubkey.to_vec();
                entry.seq = head.seq;
                entry.outrun = false;
                true
            }
            None => {
                self.chain_heads.push(PeerChainHead {
                    actor,
                    recovery_pubkey: head.recovery_pubkey.to_vec(),
                    seq: head.seq,
                    first_seen: Timestamp::now(),
                    outrun: false,
                });
                true
            }
        }
    }

    /// Whether the head held for `actor` is marked [`PeerChainHead::outrun`] —
    /// held, still the walk's rewrite/truncation guard, but no longer allowed
    /// to settle a statement offline.
    pub fn chain_head_is_outrun(&self, actor: &ActorId) -> bool {
        self.chain_heads
            .iter()
            .any(|entry| &entry.actor == actor && entry.outrun)
    }

    /// Mark the head held for `actor` **outrun** when `claimed` — the head the
    /// peer's own directly-signed profile mirrors — is past it: a higher `seq`,
    /// or a different key at the same `seq`. The harvest's second write
    /// strength, and deliberately the opposite direction from an advance.
    ///
    /// A RecoveryKey the owner rotated away keeps signing valid statements for
    /// ever, and a held head naming it would go on settling them offline
    /// (`SignedIdentitySuccession::verify` checks the key, the two signatures
    /// and an advancing `seq` — nothing about currency). The profile cannot be
    /// trusted to *advance* the head (rule 3: its signer is the key a seed
    /// thief holds), but it can be trusted to *withdraw the offline shortcut*:
    /// the worst a doctored claim buys is a statement that has to be walked,
    /// which is the honest path anyway. A claim at or behind the held head is
    /// the mirror lagging a walk and marks nothing. Returns whether anything
    /// was written; never touches the head itself or `first_seen`.
    pub fn mark_chain_head_outrun(
        &mut self,
        actor: &ActorId,
        claimed: crate::recovery::ChainHead,
    ) -> bool {
        let Some(entry) = self
            .chain_heads
            .iter_mut()
            .find(|entry| &entry.actor == actor)
        else {
            return false;
        };
        let past = claimed.seq > entry.seq
            || (claimed.seq == entry.seq
                && claimed.recovery_pubkey.as_slice() != entry.recovery_pubkey.as_slice());
        if !past || entry.outrun {
            return false;
        }
        entry.outrun = true;
        true
    }

    /// Seed `head` for `actor` **only if no head is held yet** — the harvest's
    /// write strength, deliberately weaker than [`Self::remember_chain_head`].
    ///
    /// A harvested head comes from a profile signed by the identity key, which
    /// a seed thief holds; letting it *advance* a held head would let a
    /// window-doctored profile stamp an arbitrarily high `seq` and turn the
    /// rewrite/truncation guard against the genuine chain. Filling an empty
    /// slot is the TOFU grade the ratification concedes
    /// (`identity-succession.md` § The succession statement → *the
    /// peer-profile harvest*, rule 3). Returns whether anything was written.
    ///
    /// Bounded at [`MAX_PEER_ANCHOR_ENTRIES`], re-asserted here rather than
    /// only at the door: this is `pub`, so the door's rule would otherwise be
    /// a rule only the door's own callers obey — the same reason the host
    /// length bound is re-asserted at [`Self::seed_anchor_domain`].
    pub fn seed_chain_head(&mut self, actor: ActorId, head: crate::recovery::ChainHead) -> bool {
        if self.chain_heads.len() >= MAX_PEER_ANCHOR_ENTRIES
            || self.chain_heads.iter().any(|entry| entry.actor == actor)
        {
            return false;
        }
        self.chain_heads.push(PeerChainHead {
            actor,
            recovery_pubkey: head.recovery_pubkey.to_vec(),
            seq: head.seq,
            first_seen: Timestamp::now(),
            outrun: false,
        });
        true
    }

    /// The harvested home domain this fleet holds for `actor`, if any — the
    /// witness's tier-2 fallback when no resolved handle exists.
    pub fn known_anchor_domain(&self, actor: &ActorId) -> Option<String> {
        self.anchor_domains
            .iter()
            .find(|entry| &entry.actor == actor)
            .map(|entry| entry.domain.clone())
    }

    /// Seed a harvested home `domain` for `actor`, **first-write-wins**: an
    /// already-held domain is never displaced (same rationale as
    /// [`Self::seed_chain_head`] — the artifact's signer is the key a thief
    /// holds, so re-pointing must not be reachable from a later harvest).
    /// Returns whether anything was written.
    pub fn seed_anchor_domain(&mut self, actor: ActorId, domain: String) -> bool {
        // The length AND count bounds are re-asserted here, not only at
        // `seed_from_peer_profile_bytes`: this is `pub`, so the door's rules
        // would otherwise be rules only the door's own callers obey.
        if domain.is_empty()
            || domain.len() > crate::web::MAX_HOSTNAME_BYTES
            || self.anchor_domains.len() >= MAX_PEER_ANCHOR_ENTRIES
            || self.anchor_domains.iter().any(|entry| entry.actor == actor)
        {
            return false;
        }
        self.anchor_domains.push(PeerAnchorDomain {
            actor,
            domain,
            first_seen: Timestamp::now(),
        });
        true
    }

    /// **The one door for the peer-profile harvest** (`identity-succession.md`
    /// § The succession statement → *the peer-profile harvest*): decode +
    /// envelope-verify `bytes` as `peer`'s profile and seed the two anchor
    /// stores. The four ratified rules live here so no call site can hold a
    /// weaker subset:
    ///
    /// 1. signed by the identity key ITSELF — the unsigned legacy bare decode
    ///    (`origin: None`) is refused, and so is a delegated authoring
    ///    signature ([`PeerAnchorRefusal::Delegated`]), which the account's own
    ///    home nest can make;
    /// 2. the profile must name `peer` — envelope verification is
    ///    self-consistent, not self-targeted, so a host serving a *different*
    ///    identity's genuine profile is refused;
    /// 3. seed, never advance — [`Self::seed_chain_head`] /
    ///    [`Self::seed_anchor_domain`] — and a head claimed past a held one
    ///    only DEMOTES it ([`Self::mark_chain_head_outrun`]);
    /// 4. read-paths-only is the *caller's* half (nothing here can enforce
    ///    when it is called) — never call this from a verification path;
    /// 5. the store is bounded on **both** axes — each harvested host at RFC
    ///    1035's 253 bytes ([`crate::web::MAX_HOSTNAME_BYTES`]) and each
    ///    vector at [`MAX_PEER_ANCHOR_ENTRIES`] entries; see
    ///    [`PeerAnchorRefusal::HostTooLong`] for why an unbounded store is a
    ///    recoverability bug rather than a cosmetic one, and
    ///    [`MAX_PEER_ANCHOR_ENTRIES`] for why the count half refuses rather
    ///    than evicts. The count half bounds NEW SEEDS only: rule 3's
    ///    demotion takes no slot, so a full store never refuses it
    ///    ([`PeerAnchorRefusal::StoreFull`]).
    pub fn seed_from_peer_profile_bytes(
        &mut self,
        peer: &ActorId,
        bytes: &[u8],
    ) -> Result<PeerAnchorSeed, PeerAnchorRefusal> {
        let (profile, origin) =
            crate::encoding::decode_profile(bytes).map_err(|_| PeerAnchorRefusal::Undecodable)?;
        // Rule 1, both arms. An unsigned body is any host's fabrication and
        // never decodes (`decode_profile` is signed-only); a delegated one is
        // signed by a key the peer's OWN HOME NEST holds (`atproto-pds-full.md`
        // D10), which is the same party serving this profile — so it is not
        // the identity key's word, and anchors nothing.
        if let crate::encoding::AuthoringOrigin::Delegated { .. } = origin {
            return Err(PeerAnchorRefusal::Delegated);
        }
        if &profile.actor_id != peer {
            return Err(PeerAnchorRefusal::WrongActor);
        }
        // Rule 5's length axis is checked BEFORE anything is written: every
        // `PeerAnchorRefusal` arm promises nothing was seeded, and the chain
        // head below is a write. Refusing the whole profile (rather than just
        // dropping the host) is deliberate — an over-length host is not a
        // shape this parser failed to read, it is the peer asserting one, and
        // the peer signed the artifact carrying it.
        let host = profile.nests.first().and_then(|entry| url_host(&entry.url));
        if host
            .as_ref()
            .is_some_and(|host| host.len() > crate::web::MAX_HOSTNAME_BYTES)
        {
            return Err(PeerAnchorRefusal::HostTooLong);
        }
        // What this profile would need a NEW slot for — read before the
        // writers run, for rule 5's count axis below.
        let wants_new_slot = (profile.recovery_head.is_some()
            && !self.chain_heads.iter().any(|entry| &entry.actor == peer))
            || (host.is_some() && !self.anchor_domains.iter().any(|entry| &entry.actor == peer));
        // Seed an empty slot; against a held head the claim can only DEMOTE
        // (`mark_chain_head_outrun`), never advance. A profile carrying no head
        // says nothing either way — an editor without the field drops the binding whole.
        let (seeded_head, marked_outrun) = match profile.recovery_head {
            Some(head) if self.seed_chain_head(*peer, head) => (true, false),
            Some(head) => (false, self.mark_chain_head_outrun(peer, head)),
            None => (false, false),
        };
        let seeded_domain = match host {
            Some(domain) => self.seed_anchor_domain(*peer, domain),
            None => false,
        };
        let seed = PeerAnchorSeed {
            seeded_head,
            seeded_domain,
            marked_outrun,
        };
        // Rule 5's count axis, raised AFTER the writers on purpose. Both seed
        // writers hold the bound themselves (they are `pub`), so a full vector
        // has already refused its half; what must never be refused for
        // capacity is the DEMOTION, which flips a flag on an entry already
        // held and takes no slot. The check used to sit ahead of the head
        // match, and a full store — a big roster fills one, and so can one
        // hostile room's policy names — then hid every later kit rotation from
        // this device: the sweep settled the peer as refused, the harvest wait
        // released, and the retired kit's statement settled offline at tier 1.
        //
        // So `StoreFull` means exactly: NEITHER vector has room, the profile
        // offered something that needed one, and nothing was written — which
        // keeps every refusal arm's promise that nothing was seeded, and lets
        // the caller tell "the store is full" from the ordinary
        // already-anchored `NothingNew`.
        if !seed.changed()
            && wants_new_slot
            && self.chain_heads.len() >= MAX_PEER_ANCHOR_ENTRIES
            && self.anchor_domains.len() >= MAX_PEER_ANCHOR_ENTRIES
        {
            return Err(PeerAnchorRefusal::StoreFull);
        }
        Ok(seed)
    }
}

impl PeerChainHead {
    /// Join `theirs` — the same actor's head from another replica — into
    /// `self`: the per-actor half of [`PeerAnchors::merge`].
    ///
    /// `first_seen` merges as the per-actor **minimum** independently of which
    /// head wins: the stamp records the fleet's earliest sighting of the
    /// actor, and the earliest sighting is a fact no later replica can move.
    /// `min` is the associative choice in the direction that matters — `max`
    /// would let one device's late first sighting make a long-held anchor look
    /// new to the ceiling.
    pub fn join_from(&mut self, theirs: &Self) {
        let ours = self;
        let first_seen = ours.first_seen.min(theirs.first_seen);
        // `outrun` is a fact about ONE head, so it ORs only between replicas
        // holding the same head and otherwise rides with whichever head wins
        // below — a walk's advance (which clears it) must not be re-marked by a
        // replica still on the head the walk replaced. Lexicographic on (head
        // order, mark), so still a join.
        let same_head = theirs.seq == ours.seq && theirs.recovery_pubkey == ours.recovery_pubkey;
        if same_head {
            ours.outrun |= theirs.outrun;
        }
        if theirs.seq > ours.seq {
            *ours = theirs.clone();
        } else if theirs.seq == ours.seq && theirs.recovery_pubkey < ours.recovery_pubkey {
            // Equal `seq`, different key registered at it: the two replicas saw
            // different bytes at one height, which is an equivocation the merge
            // must not resolve by preferring itself. The byte-smaller key wins —
            // arbitrary, identical on both replicas, and it neither rewinds nor
            // advances the anchor, so the rewrite guard's monotonicity is
            // untouched.
            *ours = theirs.clone();
        }
        ours.first_seen = first_seen;
    }
}

impl PeerAnchorDomain {
    /// Join `theirs` — the same actor's domain from another replica — into
    /// `self`: the lexically smaller domain, the earlier `first_seen`. The
    /// per-actor half of [`PeerAnchors::merge`].
    pub fn join_from(&mut self, theirs: &Self) {
        if theirs.domain < self.domain {
            self.domain = theirs.domain.clone();
        }
        self.first_seen = self.first_seen.min(theirs.first_seen);
    }
}

/// The one ordering both peer-anchor vectors are sorted and truncated by on the
/// merge path: **oldest first**, by the owner-stamped `first_seen`, with the
/// actor id only as the equal-stamp tiebreak. An entry from before the stamp
/// existed carries the epoch and therefore sorts first — the upgrade must never
/// evict the honest back-catalogue. The actor id is a value the PEER mints and
/// can grind to sort anywhere it likes; the stamp is written by the owner's own
/// device, which is why it is the primary key and the id only the tiebreak
/// (`MAX_PEER_ANCHOR_ENTRIES`' doc has the argument).
pub fn peer_anchor_order<E: PeerAnchorEntry>(entry: &E) -> (Timestamp, [u8; 32]) {
    (entry.first_seen(), entry.actor().0)
}

/// The two fields [`peer_anchor_order`] reads, shared by both anchor vectors'
/// element types.
pub trait PeerAnchorEntry {
    /// When this fleet first anchored the entry's actor.
    fn first_seen(&self) -> Timestamp;
    /// The identity the entry is about.
    fn actor(&self) -> &ActorId;
}

impl PeerAnchorEntry for PeerChainHead {
    fn first_seen(&self) -> Timestamp {
        self.first_seen
    }
    fn actor(&self) -> &ActorId {
        &self.actor
    }
}

impl PeerAnchorEntry for PeerAnchorDomain {
    fn first_seen(&self) -> Timestamp {
        self.first_seen
    }
    fn actor(&self) -> &ActorId {
        &self.actor
    }
}

/// What [`PeerAnchors::seed_from_peer_profile_bytes`] wrote — `false`/`false`
/// is the ordinary already-anchored (or nothing-to-harvest) case, not an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerAnchorSeed {
    /// A chain head filled a previously-empty `peer_chain_heads` slot.
    pub seeded_head: bool,
    /// A home domain filled a previously-empty `peer_anchor_domains` slot.
    pub seeded_domain: bool,
    /// The profile claimed a head past the one already held, and the held
    /// entry was newly marked [`PeerChainHead::outrun`]. Never set together
    /// with `seeded_head` — a slot this harvest just filled holds exactly what
    /// the profile claims.
    pub marked_outrun: bool,
}

impl PeerAnchorSeed {
    /// Whether anything was written (a caller skips the plane write
    /// when nothing was).
    pub fn changed(&self) -> bool {
        self.seeded_head || self.seeded_domain || self.marked_outrun
    }
}

/// Why [`PeerAnchors::seed_from_peer_profile_bytes`] refused the bytes. Every
/// arm is a hard refusal — nothing is seeded — and none is retryable with the
/// same bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerAnchorRefusal {
    /// Not a decodable, envelope-verified profile payload at all — an unsigned
    /// body included, which any host can fabricate (harvest rule 1).
    Undecodable,
    /// Envelope-verified, but under a **delegated authoring sub-key** rather
    /// than the identity key itself ([`crate::encoding::AuthoringOrigin::Delegated`])
    /// — harvest rule 1's second arm.
    ///
    /// The delegated key this door meets in practice is D10's: minted and held
    /// by the account's own HOME NEST (`atproto-pds-full.md` D10 — "K custody:
    /// home-nest-held, per-account"), authorized for `Profile` envelopes by
    /// `Capability::UpdateProfile`. That nest is the same party serving this
    /// profile to a same-nest harvester, so a `recovery_head` or `nests` entry
    /// signed that way is the nest's choice, not the peer's. Seeding it would
    /// hand the nest a tier-1 succession anchor for an identity whose key it
    /// does not hold — the one thing the harvest's grade rests on
    /// (`identity-succession.md` § The succession statement → *the peer-profile
    /// harvest*, rule 1).
    ///
    /// Both halves are refused, not just the head: the domain half is
    /// first-write-wins with no eviction, so a planted domain blocks the
    /// genuine one for good, and the field's own precedent is that the cache
    /// degrades to absent rather than to poisoned.
    ///
    /// Terminal for the session like every arm. An owner app re-publishing the
    /// profile under the identity key makes it harvestable again, which a later
    /// session's sweep picks up.
    Delegated,
    /// A validly signed profile naming a *different* identity — the
    /// self-consistent-is-not-self-targeted refusal (harvest rule 2).
    WrongActor,
    /// The profile's first nest URL carries a host longer than RFC 1035's
    /// 253-byte ceiling ([`crate::web::MAX_HOSTNAME_BYTES`]) — harvest rule 5.
    ///
    /// The harvested host is peer-chosen and reaches this store through a
    /// background sweep with no human in the loop, while
    /// the anchor domains rest in `fauna.state.peer-anchors`, which has no
    /// prune, no eviction and a per-actor UNION merge. So an unbounded host is
    /// not merely ugly: a few peers publishing large ones grow the victim's
    /// store past its byte bounds on every device, permanently, with no app
    /// affordance to undo it — the client-causable unrecoverable state
    /// `nest/common.md` § Client-state recoverability forbids. Bounding it
    /// HERE, at the one door, is the fix.
    ///
    /// Length only. `url_host` deliberately admits a bracketed IPv6 literal,
    /// which [`crate::web::normalize_custom_domain`]'s stricter shape rules
    /// (no IP literals, at least two labels) would refuse — those belong to
    /// an admin typing a custom domain, not to a harvested anchor.
    HostTooLong,
    /// **Both** anchor vectors already hold [`MAX_PEER_ANCHOR_ENTRIES`]
    /// entries, and this profile offered a seed that needed a slot, so it has
    /// nowhere to go — harvest rule 5's count axis.
    ///
    /// **Never raised over a demotion.** A profile claiming past a head this
    /// store already holds marks that head outrun
    /// ([`PeerAnchors::mark_chain_head_outrun`]) however full the vectors are:
    /// the mark takes no slot, and refusing it would let a full store hide a
    /// kit rotation from this device for good. Nor is it raised for a peer
    /// already anchored in both vectors, which has nothing left to seed — that
    /// is the ordinary `Ok` with nothing changed.
    ///
    /// Raised only when neither vector has room. A profile that can still fill
    /// one of them is NOT refused: the two are seeded independently
    /// ([`PeerAnchorSeed`] reports each half), a head is a tier-1 anchor and a
    /// domain the tier-2 fallback, and refusing a head that fits because the
    /// *domain* vector is full would discard the more useful half for the
    /// fuller one's sake. Terminal like every other arm — a full store is a
    /// standing condition, not a hiccup, so a caller must not retry it (the
    /// harvest sweep's retry ladder classifies refusals as settled).
    StoreFull,
}

/// Host of an HTTPS-ish URL string, lowercased; scheme, userinfo, port and
/// path stripped. `None` for anything without a non-empty host. Deliberately
/// tiny rather than a URL-crate dependency — the input is a self-asserted
/// `NestEntry.url`, and a shape this parser cannot read simply seeds nothing
/// (fail-closed). A bracketed IPv6 host is returned with its brackets; a
/// nonstandard port is stripped with the rest (declared: a nest served only on
/// a nonstandard port is not anchorable through this store).
///
/// Composes [`crate::web::generic_authority`] (scheme strip + the four-byte
/// WHATWG authority terminator set `/ \ ? #`) and [`crate::web::strip_userinfo`]
/// — one of `generic_authority`'s callers (`security.md` § Transport trust
/// keeps the current list) — rather than keeping a private copy of the
/// terminator set that could drift narrower again, as it already had once
/// (missing only `\`) before catching up to the shared set
/// . Only the final host cut — the
/// bracketed IPv6 literal or the pre-colon label — stays local:
/// [`crate::web::split_host_port`] would also keep a malformed port suffix
/// whole (`host:junk`) instead of trimming to `host`, which this store's
/// pin-store key does not want.
pub fn url_host(url: &str) -> Option<String> {
    let authority = crate::web::generic_authority(url);
    let rest = crate::web::strip_userinfo(authority);
    let host = if rest.starts_with('[') {
        rest.split_inclusive(']').next().unwrap_or("")
    } else {
        rest.split(':').next().unwrap_or("")
    };
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() { None } else { Some(host) }
}

#[cfg(test)]
mod url_host_tests {
    use super::url_host;

    #[test]
    fn url_host_ends_at_all_four_authority_terminators() {
        assert_eq!(
            url_host("https://nest.example.com/path"),
            Some("nest.example.com".to_string())
        );
        assert_eq!(
            url_host("https://nest.example.com?x=1"),
            Some("nest.example.com".to_string())
        );
        assert_eq!(
            url_host("https://nest.example.com#frag"),
            Some("nest.example.com".to_string())
        );
        // `\` was the one terminator this parser missed before it joined
        // `fauna_core::web`'s shared set .
        assert_eq!(
            url_host(r"https://nest.example.com\x"),
            Some("nest.example.com".to_string())
        );
        // Shared cases so a narrower `authority_len` reds this alongside the
        // other five callers .
        for &(url, expected_host) in crate::web::AUTHORITY_TERMINATOR_CASES {
            assert_eq!(url_host(url), Some(expected_host.to_string()), "{url}");
        }
    }

    // Userinfo dropped via the shared `strip_userinfo`, now that `url_host`
    // delegates rather than keeping its own copy of the terminator set
    // .
    #[test]
    fn url_host_drops_userinfo() {
        assert_eq!(
            url_host("https://user@nest.example.com/x"),
            Some("nest.example.com".to_string())
        );
    }
}

// ── Tier-1 user-moderation sub-record ──
//
// The `fauna.state.moderation` entry's typed contents. Lives here so the
// at-rest dag-cbor encoding boundary stays in one place;
// normalization + merge live in `libs/fauna-client-config`. Authority for the
// kind's shape is `docs/goal/architecture/config-dissolution.md`; authority for behavior
// is `docs/goal/architecture/content-moderation-and-ranking.md`
// § Resolved design decisions Q3 (+ `docs/goal/behavior/moderation.md`
// § Muted keywords).

/// Per-actor tier-1 moderation preferences. `ModerationConfig::default()`
/// is the no-prefs state (empty muted list) — every field is
/// `#[serde(default)]` so a stored record written before this field
/// existed decodes unchanged (additive-everywhere, `version-compatibility.md`).
/// User-global file-sync preferences (`fauna.state.sync-prefs`). Additive:
/// `#[serde(default)]` so a stored record written before this field existed
/// decodes unchanged (additive-everywhere, `version-compatibility.md`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPrefsConfig {
    /// Default conflict policy stamped onto **newly created** folders (the
    /// `sync-default-conflict-policy-select` control). Stored as the
    /// `ConflictPolicy` wire string (`"auto"` | `"latest_wins_always"`) — a
    /// plain `String`, not the enum, so a future policy value never fails the
    /// strict decoder on an older client (readers degrade unknown
    /// → auto via `ConflictPolicy::from_wire`). `None` = no preference (new
    /// sets take the nest column default, `auto`). Existing sets are NOT
    /// retroactively changed — each set's row stays authoritative.
    #[serde(default)]
    pub default_conflict_policy: Option<String>,
}

/// One **followed public folder** — the entire client-side state of a
/// publicly-synced follow (`docs/goal/behavior/folders.md` § Publicly-synced
/// follow).
///
/// The home nest holds **no follower state at all**: no roster, no
/// registration, no per-follower row. So this record is not a cache of
/// something authoritative elsewhere — it *is* the follow, and unfollow is
/// simply its removal (there is nothing to revoke anywhere).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowedFolder {
    /// The folder's home nest base URL — where `fauna.folders.public.fetch`
    /// relays to. Empty means the folder is homed on the follower's own nest
    /// (the same-nest follow), which the fetch reads locally.
    #[serde(default)]
    pub home_nest_url: String,
    /// The home nest's deployment `nest_actor_id` (hex), stamped from the first
    /// successful fetch — the byte-plane SPKI-pin trust root the follower dials
    /// the open by-hash bulk plane under (`security.md` § Transport trust).
    /// `None` if a reply somehow carried none; the reader then declines to pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_nest_actor_id: Option<String>,
    /// Hex owner actor id, resolved from the handle the user picked.
    pub owner_actor_id: String,
    /// The handle the user followed the owner BY, when they typed one — the
    /// source of the followed row's owner label (`docs/goal/ui/folders.md`
    /// § Following a public folder: *name + owner handle + badge + status*).
    /// `None` when the owner was given as a bare actor id: no kind maps an
    /// actor id back to a handle, and the public plane deliberately names no
    /// owner in its reply.
    ///
    /// ⚠ **A remembered input, never trusted as current.** A handle can be
    /// changed and later taken by someone else, so the reader re-verifies it
    /// against `owner_actor_id` beside each availability probe
    /// (`fauna_client_folders::public_follow::resolve_availability`) and shows
    /// the actor id's short form whenever the two no longer agree — a stale
    /// name on the row would say the folder is someone else's.
    ///
    /// Additive (`default`, `skip_serializing_if`): a record written before the
    /// field decodes as `None`, and an older client re-sealing the config drops
    /// the key, which degrades the label to the short id and loses nothing the
    /// user cannot restore by following again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_handle: Option<String>,
    /// The home nest's stable `folders.id`, pinned by the first successful
    /// fetch. Every later read addresses by this, so the follow survives
    /// anything that changes the folder's name.
    pub folder_id: i64,
    /// The folder's plaintext name as of the last successful fetch — display
    /// only. Public names are world-readable by the ratified exception, so this
    /// is not secret; it is refreshed from each reply rather than trusted as an
    /// address.
    #[serde(default)]
    pub display_name: String,
}

impl FollowedFolder {
    /// This follow's logical key on the plane (`fauna.state.follows`, one row
    /// per followed folder — `config-dissolution.md` § Phases and gates →
    /// *Bounded rows*): the lowercase-hex blake3 digest of `home_nest_url`,
    /// `/`, the decimal `folder_id` — the `(home_nest_url, folder_id)`
    /// identity [`FollowsConfig::sort_canonically`] deduplicates on, at a
    /// fixed length however long the URL runs. Frozen with the kind: a row
    /// already written is addressed by it.
    #[must_use]
    pub fn plane_key(&self) -> String {
        Self::plane_key_of(&self.home_nest_url, self.folder_id)
    }

    /// [`Self::plane_key`] for an identity without its record — the unfollow's
    /// address.
    #[must_use]
    pub fn plane_key_of(home_nest_url: &str, folder_id: i64) -> String {
        format!(
            "{}/{folder_id}",
            blake3::hash(home_nest_url.as_bytes()).to_hex()
        )
    }
}

/// The user's followed public folders (`fauna.state.follows`). Additive:
/// `#[serde(default)]` so a stored record written before this field existed
/// decodes unchanged (additive-everywhere, `version-compatibility.md`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowsConfig {
    /// Followed folders, kept sorted by `(home_nest_url, folder_id)` so two
    /// devices that followed the same set in different orders encode
    /// identically — the canonical-representation rule the account-state merges
    /// are built on (canonical in CONTENT and in REPRESENTATION).
    #[serde(default)]
    pub followed: Vec<FollowedFolder>,
}

impl FollowsConfig {
    /// The canonical order every writer must leave this list in.
    pub fn sort_canonically(&mut self) {
        self.followed
            .sort_by(|a, b| (&a.home_nest_url, a.folder_id).cmp(&(&b.home_nest_url, b.folder_id)));
        self.followed
            .dedup_by(|a, b| a.home_nest_url == b.home_nest_url && a.folder_id == b.folder_id);
    }
}

/// One entry of the user's muted-keywords list: the term and how hard it
/// demotes (`content-moderation-and-ranking.md` § Composition, the 2026-07-10
/// ruling — "each muted-keywords entry is `(keyword, weight)`").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MutedKeyword {
    /// The term, case preserved for display; matched case-insensitively as a
    /// substring (`crate::keyword::body_excludes_matches`).
    pub keyword: String,
    /// The factor value a match contributes, per-mille, in
    /// `[MUTED_KEYWORDS_PENALTY, 0]` (`crate::scoring::clamp_muted_keyword_weight`
    /// reads any other value as its nearest bound). The default,
    /// `MUTED_KEYWORDS_PENALTY` (−1000), sinks the item and collapses it; a
    /// softer weight only demotes it in a ranked feed.
    pub weight: i64,
}

impl MutedKeyword {
    /// A keyword muted at the default weight, the full penalty.
    pub fn new(keyword: impl Into<String>) -> Self {
        Self {
            keyword: keyword.into(),
            weight: crate::scoring::MUTED_KEYWORD_DEFAULT_WEIGHT,
        }
    }

    /// The level the muted-words page renders this entry at
    /// (`crate::scoring::MutedKeywordLevel::of` over the stored weight).
    pub fn level(&self) -> crate::scoring::MutedKeywordLevel {
        crate::scoring::MutedKeywordLevel::of(self.weight)
    }
}

impl From<&str> for MutedKeyword {
    fn from(keyword: &str) -> Self {
        Self::new(keyword)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModerationConfig {
    /// The user-global **muted-keywords** list (frame Q3): one
    /// [`MutedKeyword`] — term and weight — per entry. Case is preserved for
    /// display; matching is case-insensitive substring
    /// (`fauna_core::keyword::body_excludes_matches`, the one eval nest and
    /// client share), and the weight is the scorer's value
    /// (`fauna_core::scoring::muted_keywords_penalty_entry`). Normalized on
    /// write by `fauna_client_config::set_muted_keywords` (trim, drop blanks,
    /// case-insensitive dedupe keeping the first-seen entry, weight clamped).
    /// Applied client-side, post-decrypt — the nest never sees this list or
    /// the plaintext it filters. Empty until the user adds a word on the
    /// `muted-words` Settings sub-page.
    #[serde(default)]
    pub muted_keywords: Vec<MutedKeyword>,
    /// What the user **reported**, hidden for them (`moderation.md` §
    /// Corollary — block also hides): each entry is a report subject's id — a
    /// post's cid or a message's record cid, both lowercase hex, or an account's
    /// actor id, which hides everything that account authored. The shared
    /// render verdict reads it as a `Block` with the "you reported this"
    /// placeholder (`obligation::render_verdict_for_item`). Normalized and
    /// bounded on write by `fauna_client_config::preference_records::
    /// hide_reported_content`; sealed like the rest of this record, so the nest
    /// never learns what a user hid.
    #[serde(default)]
    pub hidden_content: Vec<String>,
}

// ── Personalization sub-record (trained topic factors) ──
//
// Authority: `docs/goal/behavior/topic-factors.md` § Wire & registry. The
// *registry* (names + flags) lives here, on `fauna.state.personalization`; the trained
// **model blob** deliberately does NOT — it lives in the nest's
// `personalization_models` table, because a plane entry carries a per-entry
// byte cap and a whole-entry merge, neither of which
// suits a growing 256 KiB-capped model blob (same doc, § At rest).

/// The user's **sealed trained-factor registry** — the display-name side of her
/// private topic factors (`docs/goal/behavior/topic-factors.md` § Wire &
/// registry). BackupKey-sealed on the account plane (`fauna.state.personalization`),
/// readable by the user's Fauna app fleet only.
///
/// **The name never reaches the nest.** The nest sees only the opaque
/// `topic:<hex>` composition key ([`TrainedFactorMeta::factor_key`]), never what
/// the factor *means* — the tier-1 seal behind "the nest never learns what Cats
/// means" (same doc, § Goal).
///
/// `default()` (empty) is the no-trained-factors state, which is also the
/// works-out-of-the-box state: an actor who never trains a topic carries an
/// empty registry and her feed falls back to its other factors.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonalizationConfig {
    /// One entry per trained topic factor the user has created, capped at
    /// `TRAINED_FACTORS_MAX = 32` per actor (enforced nest-side at `model.put`;
    /// `topic-factors.md` § At rest → Caps).
    #[serde(default)]
    pub trained_factors: Vec<TrainedFactorMeta>,
}

/// One trained topic factor's metadata. The statistical model itself is a sealed
/// blob in the nest's `personalization_models` table, keyed by this entry's
/// derived [`TrainedFactorMeta::factor_key`].
///
/// **Deleting an entry does not invalidate compositions** that still reference
/// its key: the sealed-factor seam contributes a zero term for a model the client
/// can no longer load, so an orphan key is inert rather than broken
/// (`topic-factors.md` § Delete semantics).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainedFactorMeta {
    /// The factor's 16-byte random id, minted at creation. Rides the wire as a
    /// CBOR **byte string** (the `GrantEvent::grant_id` convention), not a
    /// 16-element integer array — smaller, and it matches every other
    /// fixed-width id in this graph. Exactly 16 bytes; a wrong-length id makes
    /// [`TrainedFactorMeta::factor_key`] return `None` rather than mint a
    /// malformed composition key.
    #[serde(with = "serde_bytes")]
    pub id: Vec<u8>,
    /// The user's chosen display name ("Cats"). Rendered in the
    /// `personalization-trained-factor-item` row and the `feed-factor-select`
    /// picker; **never sent to the nest**.
    pub name: String,
    /// v2 opt-in: train this factor from implicit engagement (likes, later
    /// replies/saves) as a weak positive example. **Default false**, per-factor
    /// (`topic-factors.md` § Training signals → v2). v1 ignores it: no
    /// engagement-ingestion path exists yet. It is carried now so the at-rest
    /// shape stays additive when one does.
    #[serde(default)]
    pub learn_from_engagement: bool,
    /// Epoch **seconds** at creation (the `personalization_models.updated_at`
    /// sibling convention). Integer — the dag-cbor wire forbids floats.
    pub created_at: u64,
}

impl TrainedFactorMeta {
    /// This factor's canonical composition key, `topic:<32-lowercase-hex>`.
    ///
    /// Derived, never stored: a call site cannot hand-write a key that disagrees
    /// with the id, and [`crate::scoring::topic_factor_id`] is its exact inverse.
    /// `None` when [`TrainedFactorMeta::id`] is not exactly 16 bytes (a corrupt
    /// or foreign-written record) — the caller skips such an entry rather than
    /// composing an unparseable key.
    pub fn factor_key(&self) -> Option<String> {
        let id: [u8; 16] = self.id.as_slice().try_into().ok()?;
        Some(crate::scoring::topic_factor(&id))
    }
}

// ── Mail-credential sub-record ──
//
// The `fauna.state.mail` entry's typed contents. Lives here so the
// at-rest dag-cbor encoding boundary stays in one place;
// per-credential lifecycle / rotation logic lives in
// `libs/fauna-client-mail-settings/`. Authority for the at-rest
// shape is `docs/goal/architecture/config-dissolution.md`;
// authority for behavior is `docs/goal/behavior/mail-credentials.md`.

/// Per-actor mail-credential persistence. None of these fields are
/// populated until the user runs the first "Enable mail" flow on
/// `mail-settings`; `MailConfig::default()` is the not-yet-enabled
/// state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailConfig {
    /// The actor's MLS Storage Encryption Key (32 bytes), used by
    /// the MDA bridge at MUA-AUTH to unwrap the wrapped-MSEK blob
    /// and reach the `MlsSnapshotBlob` plaintext per
    /// `docs/goal/behavior/imap-server.md` § Authentication. None
    /// until the first "Enable mail" flow runs; rotated only by the
    /// rotate-mail-keys flow.
    ///
    /// Held as [`SecretArray32`] (zeroize-on-drop + redacted `Debug`;
    /// wire-identical to a bare `[u8; 32]`, so the at-rest shape is
    /// unchanged) — the *Carrier shape* rule of
    /// `key-material-hierarchy.md` § Plaintext key lifetime on bridges. This is
    /// the longest-lived MSEK carrier in the system: it **is** the at-rest
    /// record, and the in-memory mail record holds it for the whole session.
    pub msek: Option<SecretArray32>,
    /// Prior MSEKs retained after a hard-revoke (rotate-mail-keys),
    /// most-recent first, capped at 2. The read-side snapshot derives
    /// a grace-decrypt recipient-mail keypair from each so in-flight
    /// mail sealed to a now-rotated recipient pubkey still opens (per
    /// `docs/goal/architecture/key-material-hierarchy.md` § Path
    /// B-sibling-2 "current + last 2 rotations"). Empty until the
    /// first rotation. `#[serde(default)]` so pre-existing
    /// records (written before this field existed) parse unchanged.
    ///
    /// Each generation is held as [`SecretArray32`] for the same reason
    /// [`Self::msek`] is: a retired MSEK still opens mail (that is the entire
    /// point of retaining it), so it is live key material, not a spent value.
    #[serde(default)]
    pub prior_mseks: Vec<SecretArray32>,
    /// Per-credential metadata + secret bytes needed for unattended
    /// rotation per `docs/goal/behavior/mail-credentials.md`
    /// § Rotation and recovery.
    pub credentials: Vec<MailCredential>,
    /// Resume sentinel for the rotate-mail-keys flow. Set at the
    /// start of a rotation (before any nest writes); cleared when
    /// every surviving credential has been re-wrapped under the
    /// new MSEK. If present at client startup, the mail-settings
    /// page surfaces a banner offering to resume.
    pub pending_rotation: Option<PendingRotation>,
    /// Whether **email** (SMTP submission + IMAP) is enabled for this actor,
    /// as distinct from merely holding mailbox key material — the shared `msek`
    /// also backs a **CalDAV-only** mailbox (`docs/goal/behavior/caldav-server.md`
    /// § Independent enablement: "no separate CalDAV credential" — one MSEK +
    /// one `default` credential serve IMAP + SMTP + CalDAV). `None` means no
    /// enable/disable flow has written it — email off. Set explicitly by those
    /// flows: `Some(true)` by "Enable mail", `Some(false)` by "Enable CalDAV"
    /// (read-only) and "Disable mail". The cross-device merge takes it
    /// **present-wins** ([`Self::merge`]), so a `None` never blanks a
    /// written flag.
    #[serde(default)]
    pub mail_enabled: Option<bool>,
    /// Whether **CalDAV** (calendar) is enabled for this actor. Independent of
    /// [`Self::mail_enabled`] — both ride the one shared `msek` + `default`
    /// credential. `#[serde(default)]` ⇒ `false` for a fresh config (nothing has
    /// enabled CalDAV yet). Set `true` by "Enable
    /// CalDAV"; gates whether "Disable mail" preserves the shared `msek` (it
    /// must, when CalDAV still needs it).
    #[serde(default)]
    pub caldav_enabled: bool,
    /// Whether **CardDAV** (contacts / address book) is enabled for this actor.
    /// The contacts sibling of [`Self::caldav_enabled`] — same shared `msek` +
    /// `default` credential, same independence from [`Self::mail_enabled`]
    /// (`docs/goal/behavior/carddav-server.md` § Independent enablement).
    /// `#[serde(default)]` ⇒ `false` for a fresh config. Set `true` by "Enable
    /// CardDAV"; like `caldav_enabled` it gates whether "Disable mail" preserves
    /// the shared `msek` (the address-book store still seals under it).
    #[serde(default)]
    pub carddav_enabled: bool,
    /// Retirement instants for the [`Self::prior_mseks`] generations, **keyed
    /// by the prior MSEK value itself — never positional**: the mail
    /// merge unions, dedups, and reorders `prior_mseks`, so a parallel vec
    /// cannot stay aligned. Recorded by the rotate-mail-keys flow from the
    /// content-sealing-epochs rotation-heal amendment (2026-07-19) on, in the
    /// same write that retains the generation. Entries whose `msek` leaves
    /// `prior_mseks` are pruned alongside, so no writer leaves a retained
    /// prior without its instant. (The pre-amendment "legacy retention" — a
    /// prior with no entry, read as provably pre-flip — was retired 2026-09-24
    /// by the compat-remnant sweep, `version-compatibility.md` § Dimension 2,
    /// program 4; the bounded mint now treats an unrecorded prior as an
    /// inconsistent config and mints nothing for it.)
    #[serde(default)]
    pub prior_msek_retirements: Vec<PriorMsekRetirement>,
    /// **The retired identities whose mail material has been burned by the
    /// succession leg** — one entry per predecessor, appended by exactly one
    /// writer (`fauna_client_mail_settings::succession::burn_mail_after_succession`).
    ///
    /// This is the leg's idempotency device, and it is the reason the burn is
    /// a *record* rather than a derivation. The mail plane carries no signature
    /// or era stamp: an MSEK is raw key bytes and a credential secret is a raw
    /// password, so nothing in `MailConfig` can tell a predecessor-era row from
    /// one the successor added afterwards — unlike the grant ledger, where the
    /// signer identifies the era with no state at rest
    /// (`succession-aftermath.md` § Re-key scope, the MSEK row). Owed is
    /// therefore `prior_actor_ids ⊄ {predecessor of each burn}`.
    ///
    /// ⚠ **Recorded even when there is no mail plane to burn.** A successor with
    /// mail disabled has nothing to revoke, but the record must still land: an
    /// unrecorded burn would leave the leg owed forever, and the credentials the
    /// successor mints at a *later* "Enable mail" — fresh secrets the
    /// predecessor never saw — would be burned as if they were stolen.
    ///
    /// Merged as a **union keyed by predecessor** (a burn never un-happens),
    /// which is also why it does not ride the `mail` block's whole-record
    /// latest-wins: a stale-base peer reverting this field would re-arm the leg
    /// against the successor's own fresh credentials. Empty — and wire-invisible
    /// — for every identity that never succeeded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub succession_burns: Vec<MailSuccessionBurn>,
}

/// One burn of the mail plane by the post-succession aftermath: the retired
/// identity whose compromise forced it, and when it ran
/// (`succession-aftermath.md` § Re-key scope's MSEK row;
/// `mail-credentials.md` § Rotation and recovery → *Succession*).
///
/// Used in two places, deliberately the same type: [`MailConfig::succession_burns`]
/// records that the leg ran for a predecessor, and [`MailCredential::burned`]
/// records that a specific row lost its access to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailSuccessionBurn {
    /// The retired identity whose seed holder could read this record — hence knows
    /// this MSEK and every credential secret it wrapped.
    pub predecessor: ActorId,
    /// Unix-seconds instant the burn was recorded.
    pub at_unix: u64,
}

/// One entry in [`MailConfig::prior_msek_retirements`]: the instant a
/// rotate-mail-keys hard-revoke retired `msek`, in unix seconds. Feeds the
/// bounded-mail mint's generation seal intervals
/// (`fauna_mls::wrapped_blob::bounded_mail_mint`; owner doc
/// `encryption-at-rest.md` § Capability tiering, amendment 2026-07-19).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorMsekRetirement {
    /// The retired generation's MSEK — the association key.
    ///
    /// Being an association key does not make it any less key material: these
    /// are the very bytes of a [`MailConfig::prior_mseks`] generation, so the
    /// same custody type applies (*Carrier shape*; the survey that flipped
    /// `prior_mseks` found this second copy of the same secret in the record
    /// keyed by it).
    pub msek: SecretArray32,
    /// Unix-seconds instant the rotation committed.
    pub retired_at_unix: u64,
}

impl MailConfig {
    /// The recorded retirement instant for a prior MSEK, if any — `None` only
    /// for an inconsistent config (see [`Self::prior_msek_retirements`]).
    #[must_use]
    pub fn prior_msek_retired_at(&self, msek: &SecretArray32) -> Option<u64> {
        self.prior_msek_retirements
            .iter()
            .find(|r| &r.msek == msek)
            .map(|r| r.retired_at_unix)
    }
}

/// **Named mutation M-241-custody** — revert any of these four fields to a bare
/// `[u8; 32]` and the build must fail *here*, at the accessor whose return type
/// names the custody type.
///
/// The same pin shape as [`crate::crypto::_INDEX_MASTER_KEY_IS_NOT_COPY`], moved
/// from a derivation's return type to a record's field type, and for the same
/// reason: a bare `[u8; 32]` is `Copy`, so it is silently duplicated on every
/// assignment and no duplicate is ever zeroized — and `Copy` + `Drop` being
/// mutually exclusive is precisely what makes the non-`Copy` shape the thing
/// that carries zeroize-on-drop. A runtime test cannot observe an absent `Drop`,
/// so compile-time is the only honest pin.
///
/// All four are pinned together **because they are one family holding one
/// secret**, and this family has already split once: the 2026-08-12 *Carrier
/// shape* survey named `msek` and `prior_mseks` and did not name the other two,
/// which hold the same MSEK bytes 100 and 200 lines away in this file. Priority
/// #4 — fixing one member of a family splits it.
mod _msek_family_custody_pins {
    use super::{MailConfig, PendingRotation, PriorMsekRetirement};
    use crate::secret::SecretArray32;

    const _CURRENT: fn(&MailConfig) -> &Option<SecretArray32> = |c| &c.msek;
    const _PRIOR: fn(&MailConfig) -> &Vec<SecretArray32> = |c| &c.prior_mseks;
    const _RETIREMENT: fn(&PriorMsekRetirement) -> &SecretArray32 = |r| &r.msek;
    const _PENDING: fn(&PendingRotation) -> &SecretArray32 = |p| &p.new_msek;
    // The plane's copies of the same family (`crate::mail_rows`).
    const _ROW_CURRENT: fn(&crate::mail_rows::MailStateRow) -> &Option<SecretArray32> = |r| &r.msek;
    const _ROW_PRIOR: fn(&crate::mail_rows::MailStateRow) -> &Vec<SecretArray32> =
        |r| &r.prior_mseks;
    const _ROW_SENTINEL: fn(&crate::mail_rows::MailRotationSentinel) -> &SecretArray32 =
        |s| &s.new_msek;
}

impl MailConfig {
    /// Whether the succession burn has already run for `predecessor` — the
    /// leg's owed-check, and the guard that keeps a second pass from burning
    /// credentials the successor minted after the first one
    /// ([`Self::succession_burns`]).
    #[must_use]
    pub fn burned_for(&self, predecessor: &ActorId) -> bool {
        self.succession_burns
            .iter()
            .any(|b| &b.predecessor == predecessor)
    }

    /// The credentials that still have access — the set every rotation re-wraps
    /// and the mail page renders as usable. A burned row is deliberately still
    /// in `credentials` (it is the user's list of MUAs to re-add), so every
    /// consumer that means "live credential" must go through here rather than
    /// counting the vec.
    pub fn live_credentials(&self) -> impl Iterator<Item = &MailCredential> {
        self.credentials.iter().filter(|c| c.burned.is_none())
    }

    /// Whether **email** is enabled for this actor — the value the mail-settings
    /// page renders as "Mail enabled". A CalDAV-only actor holds an `msek` (the
    /// shared calendar seal key) but no email, so this is deliberately distinct
    /// from `msek.is_some()`: only the explicit [`Self::mail_enabled`] flag
    /// counts, and an unwritten one is off.
    pub fn is_mail_enabled(&self) -> bool {
        self.mail_enabled == Some(true)
    }
}

impl MailConfig {
    /// **The shipped cross-device merge of the `mail` record** — the one rule
    /// the account-state plane's `fauna.state.mail` arm runs
    /// (`config-dissolution.md`, P1).
    ///
    /// `self_at` / `other_at` are the instants the whole-record latest-wins
    /// decisions compare — each side's record stamp. The per-field rules (the MSEK present-wins, the
    /// capped grace window with its retirements, the succession-burn min-union,
    /// the recreatable five as ONE latest-wins record with `mail_enabled`
    /// present-wins beside the MSEK) are documented inline below.
    #[must_use]
    pub fn merge(&self, self_at: Timestamp, other: &Self, other_at: Timestamp) -> Self {
        // Merge `mail` **per field**, not whole-record. The MSEK key material is
        // irrecoverable (lose `msek` ⇒ the user's stored mail is permanently
        // unreadable — `key-material-hierarchy.md` § Path B; no-data-loss
        // invariant), so it is NOT whole-record-latest-wins like `backup`/`dns`:
        //
        // * `msek` merges **present-wins** (a
        //   held `Some` is never clobbered by a peer's `None`, the common
        //   multi-device case where a device that hasn't yet synced the MSEK bumps
        //   `updated_at` on an unrelated edit), with a differing-`Some` pair (a
        //   deliberate rotation) broken by newer-`updated_at`.
        // * `prior_mseks` (the cap-2 grace-decrypt window — `MailConfig::prior_mseks`,
        //   "current + last 2 rotations") is **unioned** (newer side first, dedup,
        //   the winning `msek` excluded, capped at 2) so a peer's retained grace
        //   key is never dropped.
        // * The rest (`credentials`, `pending_rotation`, `mail_enabled`,
        //   `caldav_enabled`) is recreatable/idempotent, so it stays whole-record
        //   latest-wins from the side with the newer of `self_at` / `other_at` —
        //   except that `mail_enabled` is **present-wins** beside the `msek`: a
        //   device that never wrote the flag must not blank one written beside
        //   the MSEK that survived, or the merge yields `msek: Some` with no flag
        //   and email reads disabled (`MailConfig::is_mail_enabled`).
        //
        // ⚠ KNOWN RESIDUAL — the `(Some, Some)`
        // differing-`msek` arm picks newer-`updated_at`. A rotate-mail-keys on one
        // device racing an unrelated edit (newer timestamp, pre-rotation `msek`) on
        // another can therefore pick the older `msek` as `current`. The displaced
        // one is NOT unioned into the grace window — the window unions the two
        // sides' `prior_mseks` only, and a just-rotated key is in neither — so the
        // record alone would lose it; safeguard (2) below is what keeps it
        // (`mail-credentials.md` § Cross-device finalize race says the same).
        //
        // This residual is **deliberately NOT closed by a per-nest map** the way the
        // seed's BR-1-RESIDUAL is (decision 2026-06-30, the BR-1 map slice). The seed
        // map works because a deployment seed IS a nest's identity, so two *differing*
        // seeds always mean *different nests* → distinct `nest_actor_id` keys → a
        // per-nest map separates them. A differing `msek` is the opposite: it is
        // overwhelmingly a *rotation of the same mailbox* (same nest → same key → a
        // per-nest map would NOT separate the two generations), and **no consumer
        // reads a per-nest msek** — the whole mail stack (bridge, mail-settings
        // machine, all 7 apps) is single-`msek`. A per-nest msek map would be
        // at-rest wire shape nothing produces or reads per-nest, for a residual
        // whose *consequence* (mail loss) is already closed two ways. Defer it until a
        // genuine per-nest-mailbox model exists (then key it like the seed map). The
        // two existing safeguards: (1) the merge never drops a *held* `msek` to an
        // absent one (present-wins); (2) rotation *finalize* no longer trusts this
        // arm's outcome blind — it verifies the swap committed and re-drives if a peer
        // reverted it, keeping the new MSEK recoverable from the account plane until durable,
        // so a benign cross-device race can't silently strand mail sealed to the
        // already-published new recipient pubkey (
        // `libs/fauna-client-mail-settings/src/rotation.rs` step (f),
        // `mail-credentials.md` § Cross-device finalize race).
        //
        // The MSEK, grace-window and burn halves are the three helpers below
        // ([`merge_msek`], [`merge_grace_window`], [`merge_succession_burns`]) —
        // ONE statement each, which the `fauna.state.mail` state row's arm
        // ([`crate::mail_rows::MailStateRow::merge`]) calls too, over its own
        // stamp (`config-dissolution.md` § Phases and gates → *Bounded rows* →
        // *The mail plane*).
        let mail_msek = merge_msek(&self.msek, self_at, &other.msek, other_at);
        let (mail_prior_mseks, mail_prior_retirements) = merge_grace_window(
            mail_msek.as_ref(),
            (&self.prior_mseks, &self.prior_msek_retirements[..]),
            (&other.prior_mseks, &other.prior_msek_retirements[..]),
        );
        let mail_succession_burns =
            merge_succession_burns(&self.succession_burns, &other.succession_burns);
        // The recreatable half moves as ONE record (a credential list and the
        // rotation it belongs to must not come from different devices), so it takes
        // a single whole-record decision over the five fields together. (The
        // state row decides over FOUR — the credentials ride their own rows
        // there.)
        let (recreatable_mail, other_mail) = if crate::latest_wins::theirs_wins(
            &(
                &self.credentials,
                &self.pending_rotation,
                self.mail_enabled,
                self.caldav_enabled,
                self.carddav_enabled,
            ),
            self_at,
            &(
                &other.credentials,
                &other.pending_rotation,
                other.mail_enabled,
                other.caldav_enabled,
                other.carddav_enabled,
            ),
            other_at,
        ) {
            (other, self)
        } else {
            (self, other)
        };
        MailConfig {
            msek: mail_msek,
            prior_mseks: mail_prior_mseks,
            prior_msek_retirements: mail_prior_retirements,
            credentials: recreatable_mail.credentials.clone(),
            pending_rotation: recreatable_mail.pending_rotation.clone(),
            // Present-wins, the record's own value first: a `None` is a device
            // that never wrote the flag (every enable/disable writes `Some`).
            mail_enabled: recreatable_mail.mail_enabled.or(other_mail.mail_enabled),
            caldav_enabled: recreatable_mail.caldav_enabled,
            carddav_enabled: recreatable_mail.carddav_enabled,
            succession_burns: mail_succession_burns,
        }
    }
}

/// **The MSEK half of the mail merge** — present-wins, a differing pair (a
/// deliberate rotation) broken by the newer instant, a tie by the smaller key.
/// Shared by [`MailConfig::merge`] (the caller-supplied record instants) and
/// [`crate::mail_rows::MailStateRow::merge`] (the state row's own stamp).
pub(crate) fn merge_msek(
    ours: &Option<SecretArray32>,
    our_at: Timestamp,
    theirs: &Option<SecretArray32>,
    their_at: Timestamp,
) -> Option<SecretArray32> {
    // Matched through references and cloned into the winner: the MSEK carriers
    // are `SecretArray32` (non-`Copy`) custody types, so there is no silent
    // bitwise duplicate here to leave un-zeroized. The comparisons below are
    // unchanged in meaning — `SecretArray32`'s `Ord` delegates to the inner
    // array, so `theirs_wins_ord`'s "lexicographically smaller key" tiebreak
    // picks exactly the byte order it always did.
    let (self_at, other_at) = (our_at, their_at);
    match (ours, theirs) {
        (Some(o), Some(t)) if o == t => Some(o.clone()),
        // A deliberate rotation: newer `updated_at` wins, and on a tie the
        // lexicographically smaller key — never "ours", which is the preference
        // two replicas cannot both hold. The displaced key is NOT carried into
        // the grace window ([`merge_grace_window`] unions the priors only); the
        // rotation finalize's verify-and-re-drive keeps it (the residual on
        // [`MailConfig::merge`]).
        (Some(o), Some(t)) => Some(
            if crate::latest_wins::theirs_wins_ord(o, self_at, t, other_at) {
                t.clone()
            } else {
                o.clone()
            },
        ),
        (Some(o), None) => Some(o.clone()),
        (None, Some(t)) => Some(t.clone()),
        (None, None) => None,
    }
}

/// **The grace-window half of the mail merge** — the cap-2 `prior_mseks`
/// union with its retirement instants, the winning current `msek` excluded,
/// ordered by content. Each side is `(prior_mseks, prior_msek_retirements)`.
/// Shared by [`MailConfig::merge`] and
/// [`crate::mail_rows::MailStateRow::merge`].
pub(crate) fn merge_grace_window(
    msek: Option<&SecretArray32>,
    ours: (&[SecretArray32], &[PriorMsekRetirement]),
    theirs: (&[SecretArray32], &[PriorMsekRetirement]),
) -> (Vec<SecretArray32>, Vec<PriorMsekRetirement>) {
    // The retirement instants come FIRST because the grace window's order is
    // derived from them. Per prior-MSEK key the **later** instant wins: the
    // instant gates grace decryption, so extending the window is the
    // no-data-loss direction, and `max` is symmetric where a side-preference
    // is not.
    // Keyed by the custody type itself (`SecretArray32: Ord`), not by a bare
    // `[u8; 32]` copied out of it — the map outlives every individual compare,
    // so a bare key would be a duplicate of the secret held for the whole merge.
    let mut retirement_by_msek: BTreeMap<SecretArray32, u64> = BTreeMap::new();
    for r in ours.1.iter().chain(theirs.1.iter()) {
        let slot = retirement_by_msek
            .entry(r.msek.clone())
            .or_insert(r.retired_at_unix);
        *slot = (*slot).max(r.retired_at_unix);
    }
    // Union the grace-decrypt window, deduped, the winning current `msek`
    // excluded (it belongs in `msek`, not `prior`), then ordered **by content**
    // — most recently retired first, an unrecorded retention (an inconsistent
    // input) last, ties by key bytes — and capped at the cap-2 window
    // (`MailConfig::prior_mseks`).
    //
    // The order used to be "the newer side's list first", and with a cap that
    // is not merely a reordering: on an equal `updated_at` the two replicas
    // disagree about which side is newer, so the truncation keeps *different
    // keys* on each. A total order derived from the values themselves makes the
    // capped set a join — `top2(top2(a ∪ b) ∪ b) == top2(a ∪ b)`, since the
    // global top 2 are still present and still on top.
    let mut prior_mseks: Vec<SecretArray32> = Vec::new();
    for k in ours.0.iter().chain(theirs.0.iter()) {
        if msek != Some(k) && !prior_mseks.contains(k) {
            prior_mseks.push(k.clone());
        }
    }
    // `sort_by` rather than `sort_by_key`: the key includes the MSEK itself (the
    // tiebreak), and a `sort_by_key` closure cannot return a borrow of its
    // argument — so with a non-`Copy` custody type it would have to clone every
    // secret once per comparison. The ordering is byte-for-byte the one the
    // bare-array version produced.
    prior_mseks.sort_by(|a, b| {
        let ra = std::cmp::Reverse(retirement_by_msek.get(a).copied().unwrap_or(0));
        let rb = std::cmp::Reverse(retirement_by_msek.get(b).copied().unwrap_or(0));
        (ra, a).cmp(&(rb, b))
    });
    prior_mseks.truncate(2);
    // Retirement instants for the keys that made the window, keyed by the
    // prior-MSEK value (never positional) and in the window's own order;
    // entries whose msek fell out of the window are pruned with it
    // (content-sealing-epochs amendment 2026-07-19).
    let retirements: Vec<PriorMsekRetirement> = prior_mseks
        .iter()
        .filter_map(|k| {
            retirement_by_msek.get(k).map(|at| PriorMsekRetirement {
                msek: k.clone(),
                retired_at_unix: *at,
            })
        })
        .collect();
    (prior_mseks, retirements)
}

/// **The succession-burn half of the mail merge** — a union by predecessor,
/// the earliest instant winning. Shared by [`MailConfig::merge`] and
/// [`crate::mail_rows::MailStateRow::merge`].
pub(crate) fn merge_succession_burns(
    ours: &[MailSuccessionBurn],
    theirs: &[MailSuccessionBurn],
) -> Vec<MailSuccessionBurn> {
    // The succession burns union by predecessor, DELIBERATELY outside the
    // whole-record latest-wins decision: a burn never un-happens, and this
    // record is the mail leg's only idempotency device
    // (`MailConfig::succession_burns`). Let a stale-base peer's
    // newer-`updated_at` edit revert it and the next sign-in re-arms the leg —
    // against the successor's own post-succession credentials, which it would
    // then burn as if the predecessor had read them. Per predecessor the
    // **earliest** instant wins: the burn happened once, so the earliest record
    // is the closest to that instant, and `min` is symmetric where a
    // side-preference is not.
    let mut burn_at_by_predecessor: BTreeMap<[u8; 32], u64> = BTreeMap::new();
    for b in ours.iter().chain(theirs.iter()) {
        let slot = burn_at_by_predecessor
            .entry(b.predecessor.0)
            .or_insert(b.at_unix);
        *slot = (*slot).min(b.at_unix);
    }
    // Ordered by predecessor (the BTreeMap's own order over the id bytes), so
    // the merged representation is a function of the content — the same join
    // property the grace window needs.
    burn_at_by_predecessor
        .into_iter()
        .map(|(predecessor, at_unix)| MailSuccessionBurn {
            predecessor: ActorId(predecessor),
            at_unix,
        })
        .collect()
}

/// One row in `MailConfig::credentials` — and, on the account-state plane,
/// one `fauna.state.mail` row of its own at `credential/<credential_id>`
/// (`crate::mail_rows`; `config-dissolution.md` § Phases and gates →
/// *Bounded rows* → *The mail plane*). The `secret` is the raw PLAIN password
/// or OAUTHBEARER token bytes; sealing/unsealing against the wrapped-MSEK blob
/// happens in `libs/fauna-client-mail-settings/`.
///
/// The type stays tolerant (no `deny_unknown_fields`); the plane row decodes
/// it strict by round-trip instead (`crate::mail_rows`). The three stamp/marker fields are
/// `#[serde(default)]` and always emitted, so a decoded row re-encodes to its
/// own bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailCredential {
    /// Kebab-case identifier derived from `display_name`; carried
    /// in the MUA username via RFC 5233 sub-addressing
    /// (`<handle>+<credential_id>@<domain>`).
    pub credential_id: String,
    /// User-supplied human-readable name, e.g. `"iPhone Mail"`.
    pub display_name: String,
    pub kind: MailCredentialKind,
    /// Raw credential bytes (PLAIN password OR OAUTHBEARER token).
    /// Sealed at rest under BackupKey on the account plane
    /// (`fauna.state.mail`); never leaves the user's Fauna app fleet. [`SecretByteBuf`]
    /// (not a bare `Vec<u8>`) so the decoded plaintext is zeroized on
    /// drop instead of lingering in the in-memory record for its
    /// lifetime, and so it encodes as a CBOR **byte string** — the
    /// *Bounded rows* rider (re-cut from `SecretBytes`, an integer array,
    /// 2026-09-30 under the baseline reset, no fallback). Empty on a marked
    /// row ([`Self::burned`], [`Self::revoked_at_unix`]).
    pub secret: SecretByteBuf,
    /// Unix seconds.
    pub created_at: u64,
    /// **The row's own stamp** — what [`Self::merge`] orders the remainder
    /// by (`config-dissolution.md` § *The mail plane*; P3). Advanced by every
    /// write of the row; the epoch on a row no writer stamped.
    #[serde(default)]
    pub updated_at: Timestamp,
    /// **The generation marker** — the fingerprint of the MSEK generation
    /// this credential's nest-side blobs are wrapped under
    /// (`mail-credentials.md` § Rotation and recovery → *The generation
    /// marker*). `None` reads as unknown, which is owed a re-wrap on a live
    /// row; a marked row naming a generation is owed a delete.
    #[serde(default)]
    pub wrapped_under: Option<MsekFingerprint>,
    /// **The soft-revoke marker** — the unix-seconds instant the user revoked
    /// the credential. Monotone: present-wins, the earliest instant, never
    /// undone by a later stamp ([`Self::merge`]). The plane's fold hides a revoked row, and its id stays spent.
    #[serde(default)]
    pub revoked_at_unix: Option<u64>,
    /// **Set when the row lost access but was deliberately kept** — today only
    /// by the succession burn, which revokes every pre-succession credential at
    /// once (`mail-credentials.md` § Rotation and recovery → *Succession*:
    /// "every pre-succession credential row goes to *Compromised — access
    /// revoked*"). `None` on a live credential.
    ///
    /// Why the row survives at all: the burn is total and not user-selectable,
    /// so a successor whose list simply emptied would have to *remember* which
    /// MUAs they had configured before they could re-add them. The row is the
    /// list of what to re-add — with [`Self::secret`] emptied, because those
    /// bytes are exactly what the predecessor's seed holder read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burned: Option<MailSuccessionBurn>,
}

impl MailCredential {
    /// Whether a monotone marker is set — burned or revoked. A marked row
    /// carries no secret and is never re-wrapped.
    #[must_use]
    pub fn is_marked(&self) -> bool {
        self.burned.is_some() || self.revoked_at_unix.is_some()
    }

    /// **The `fauna.state.mail` credential row's join** (`config-dissolution.md`
    /// § Phases and gates → *Bounded rows* → *The mail plane*). Both sides
    /// name one `credential_id` (the plane arm refuses a pair that does not).
    ///
    /// - The two markers — [`Self::burned`] and [`Self::revoked_at_unix`] —
    ///   are **present-wins with the earliest instant**: monotone, so no later
    ///   stamp undoes a burn (a racing re-wrap must never put a thief-known
    ///   secret back into the survivor set) or a revoke.
    /// - The remainder — `display_name`, `kind`, `wrapped_under`,
    ///   `created_at`, `updated_at` and the secret — is **latest-wins on the
    ///   row's own `updated_at`**, a tie settling on the remainder itself (the
    ///   group-share clause), the secret compared last.
    /// - A marked join carries an **empty secret**. Comparing the secret last
    ///   is what keeps the emptying a join: emptying never reorders two
    ///   remainders that differ anywhere else.
    #[must_use]
    pub fn merge(&self, other: &Self) -> Self {
        let burned = match (&self.burned, &other.burned) {
            (Some(a), Some(b)) => Some(
                if (b.at_unix, b.predecessor.0) < (a.at_unix, a.predecessor.0) {
                    b.clone()
                } else {
                    a.clone()
                },
            ),
            (a, b) => a.clone().or_else(|| b.clone()),
        };
        let revoked_at_unix = match (self.revoked_at_unix, other.revoked_at_unix) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let remainder = |c: &Self| {
            (
                c.updated_at,
                c.display_name.clone(),
                c.kind.clone(),
                c.wrapped_under,
                c.created_at,
            )
        };
        let theirs = match remainder(other).cmp(&remainder(self)) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => other.secret.as_slice() > self.secret.as_slice(),
        };
        let win = if theirs { other } else { self };
        let marked = burned.is_some() || revoked_at_unix.is_some();
        Self {
            credential_id: self.credential_id.clone(),
            display_name: win.display_name.clone(),
            kind: win.kind.clone(),
            secret: if marked {
                SecretByteBuf::default()
            } else {
                win.secret.clone()
            },
            created_at: win.created_at,
            updated_at: win.updated_at,
            wrapped_under: win.wrapped_under,
            revoked_at_unix,
            burned,
        }
    }
}

/// **The MSEK fingerprint** — a public identity of one MSEK generation,
/// `blake3::derive_key("fauna.mail.msek-fingerprint.v1", msek)`: 32 bytes, a
/// CBOR byte string, revealing nothing about the MSEK (256 bits of entropy).
/// [`MailCredential::wrapped_under`] names the generation a row's nest-side
/// blobs are wrapped under (`mail-credentials.md` § Rotation and recovery →
/// *The generation marker* owns the invariant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MsekFingerprint(#[serde(with = "serde_bytes")] pub [u8; 32]);

impl MsekFingerprint {
    /// The derivation context — frozen: a new context is a new fingerprint of
    /// every generation, and every row would read owed at once.
    pub const CONTEXT: &'static str = "fauna.mail.msek-fingerprint.v1";

    /// The fingerprint of `msek`.
    #[must_use]
    pub fn of(msek: &SecretArray32) -> Self {
        Self(blake3::derive_key(Self::CONTEXT, msek.as_ref()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MailCredentialKind {
    Plain,
    OAuthBearer,
    /// A kind a newer build writes and this one does not name — the exact
    /// string read, re-emitted when the sealed record is merged and
    /// re-sealed (`transport.md` § Schema and forward-compat discipline →
    /// *Rule 3 in full*). This build cannot wrap under such a credential: a
    /// path that would use it refuses, and no settings row offers it.
    #[serde(untagged)]
    Other(String),
}

/// Resume sentinel for the rotate-mail-keys flow — the folded
/// `MailConfig::pending_rotation`, set between the first nest-side blob update
/// and the finalize so a crash mid-rotation can resume idempotently. It carries
/// the incoming MSEK **alone**: which credentials are still owed a re-wrap is
/// derived from the rows' generation markers (`MailRows::owed_rewrap`), never
/// stored (`mail-credentials.md` § Rotation and recovery → *The generation
/// marker*; the id list this type once carried was deleted with the
/// `fauna.state.mail` consumer cut, 2026-09-30).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRotation {
    /// The incoming MSEK, persisted across a crash-resumable rotation.
    ///
    /// The freshest MSEK there is — the one every surviving credential is being
    /// re-wrapped under — so it is held in custody ([`SecretArray32`]) like the
    /// generations on either side of it (*Carrier shape*).
    pub new_msek: SecretArray32,
}

// ── Backup-destinations sub-record ──
//
// The `fauna.state.backup` entries' typed contents. Lives here so the
// at-rest dag-cbor encoding boundary stays in one place;
// per-destination behavior and the coordinator that consumes this list
// live in `libs/fauna-sync-engine/src/segment_backup.rs`. Authority for the
// at-rest shape (sealed under `BackupKey` on the account plane) is
// `docs/goal/architecture/config-dissolution.md`; the
// cross-location upload protocol that consumes these destinations
// is tracked internally.

/// Per-actor cross-location backup destinations. `BackupConfig::default()`
/// is the no-destinations-configured state; the coordinator simply does
/// nothing when the list is empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupConfig {
    pub destinations: Vec<BackupDestination>,
}

/// One row in `BackupConfig::destinations`. What the destination nest holds
/// for this owner is a custody copy it provisioned and marked itself
/// (`folders.custody_copy` — `docs/goal/behavior/reserved-folders.md`
/// § Destination capability); the row carries no capability declaration of
/// its own (the former informational mode hint was deleted with the folders
/// mode contraction — `docs/goal/behavior/backup-destinations.md` § State &
/// data shape → *Capability*).
/// `Default` exists for a fixture-shape reason: this row has already grown once
/// (`display_name`) and will grow again, and hand-listing every field in each
/// fixture makes two parallel branches collide on the grown axis. Construct
/// test/fixture rows as `BackupDestination { .., ..Default::default() }` so a
/// field added on one branch merges cleanly with the other. The default is
/// inert, not a valid destination — an empty URL and a zero pubkey resolve to
/// nothing — with one deliberate exception: `kind` defaults to `"nest"`, so the
/// fixtures this convention exists for keep meaning what they meant before the
/// kind discriminator landed. That is why `Default` is hand-written below
/// rather than derived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupDestination {
    /// Stable client-assigned identifier; used as the key in the
    /// sync_db state tables.
    pub destination_id: String,
    /// Origin URL of the destination nest.
    pub destination_nest_url: String,
    /// Destination nest's actor pubkey (32-byte Ed25519). Recorded so
    /// the client can detect URL changes vs. nest-identity changes.
    #[serde(with = "serde_bytes")]
    pub destination_actor_pubkey: [u8; 32],
    /// Reserved folder name on the destination. Always `"__mail"` for
    /// Plan 5; the per-kind rollouts add per-kind values when their
    /// at-rest designs land.
    ///
    /// Spelled `folder_name` since the folders re-model's phase 1b
    /// (2026-08-13) renamed it from `file_set_name`. The read alias that kept a
    /// blob sealed under the old spelling decodable was retired 2026-09-24
    /// (the 2026-09-24 compat-remnant sweep at its universal scope (`docs/goal/architecture/version-compatibility.md` § Dimension 2, the fourth ratified exception): no pre-sweep blob rests anywhere), so the field is simply required and a pre-rename blob is
    /// refused whole — pinned by
    /// `pre_rename_config_with_a_backup_destination_is_refused`.
    pub folder_name: String,
    /// Unix seconds.
    pub added_at: u64,
    /// User-facing label for the destination (ratified addition,
    /// 2026-06-14 — `docs/goal/behavior/backup-destinations.md` § State & data shape).
    /// `None` ⇒ the client derives the label from the destination
    /// domain. Uniform with `MailCredential.display_name`.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Which *kind* of custodian holds these bytes — the discriminator
    /// ratified 2026-08-02 (`docs/goal/behavior/backup-destinations.md` § State & data
    /// shape → Kind discriminator). `"nest"` (a peer nest, the v1
    /// kind), `"client-device"` (one of the owner's own devices,
    /// § Third destination kind), `"s3"` reserved for the deferred S3
    /// pass. Defaults to `"nest"` so pre-existing records —
    /// written before this field existed — decode as what they are.
    ///
    /// Read it through [`BackupDestination::kind_view`], not by
    /// string-matching: the per-kind fields below are only meaningful
    /// on their own kind, and the view is what makes misreading them
    /// unrepresentable.
    #[serde(default = "default_destination_kind")]
    pub kind: String,
    /// `client-device` rows only: the custodian device's stable sync
    /// `device_id` — the same id its file-sync engines present. The
    /// source nest keys the custodian's status projection on it
    /// (`fauna.backup.custodian.checkin`), so a client-device row
    /// without one is not drivable and projects as
    /// [`DestinationKind::Inert`]. `None` on every other kind.
    #[serde(default)]
    pub custodian_device_id: Option<String>,
    /// `client-device` rows only: the user-set capacity cap in bytes —
    /// the kind's *only* knob (`docs/goal/behavior/backup-destinations.md` § Third
    /// destination kind → Intermittency semantics). `None` on every
    /// other kind.
    #[serde(default)]
    pub capacity_cap_bytes: Option<u64>,
}

/// The v1 destination kind, and the serde default for every carrier of a
/// destination kind — the at-rest [`BackupDestination::kind`] here, and the wire
/// fields in `fauna_protocol::backup`.
///
/// A missing `kind` (the `serde` default; current writers always serialize it), at
/// rest and on the wire alike, means a peer-nest destination. Shared rather
/// than restated per crate so "absent means nest" has exactly one definition.
pub fn default_destination_kind() -> String {
    DESTINATION_KIND_NEST.to_string()
}

/// A peer nest the owner administers — the v1 destination kind.
pub const DESTINATION_KIND_NEST: &str = "nest";

/// One of the owner's own devices, holding a sealed replica it pulls from the
/// source nest (`docs/goal/behavior/backup-destinations.md` § Third destination kind).
pub const DESTINATION_KIND_CLIENT_DEVICE: &str = "client-device";

/// The custodian is pulling normally, within its capacity cap.
///
/// Lives beside the destination-kind discriminators, and *below* the wire crate
/// that carries it (`fauna_protocol::backup` re-exports both, so every existing
/// path still resolves), for one reason: the label layer
/// (`crate::format::backup_usage_label`) must compare against the canonical
/// value, and `fauna-core` cannot name `fauna-protocol`. Duplicating the string
/// there would put the one rule that matters — *cap-reached is read, never
/// inferred from `held >= cap`* — one silent typo away from always being false.
pub const CAP_STATE_OK: &str = "ok";

/// The custodian has hit its user-set capacity cap: retained generations have
/// already been reclaimed and it has **stopped pulling** rather than evict live
/// paths (`docs/goal/architecture/message-segment-store.md` § Client-device
/// custodian → *Custody = the local store*).
pub const CAP_STATE_REACHED: &str = "cap-reached";

/// The custodian's last **self-audit** of its own local store passed: the paths
/// it sampled were present, openable and content-address-verified.
///
/// Beside the cap states for the same reason they sit here rather than in the
/// wire crate — the render layer must compare against the canonical value and
/// `fauna-core` cannot name `fauna-protocol`.
///
/// The state answers `docs/goal/behavior/backup-destinations.md` § Custodian contract question 4
/// (*Audit answerability*) for the client-device kind. It is **absent**, never
/// [`AUDIT_STATE_FAILED`], on a custodian that has not audited yet: enrollment
/// must not raise an alarm, exactly as `evaluate_overdue` refuses to call a
/// never-audited destination overdue.
pub const AUDIT_STATE_OK: &str = "ok";

/// The custodian's last self-audit **failed**: a path its own index calls live
/// could not be produced from local bytes.
///
/// This is the state the kind's whole audit answer exists for. A custodian's
/// store can rot — files deleted under it, a truncated blob, a corrupt chunk —
/// while every other signal stays healthy: it keeps pulling, keeps checking in,
/// and `cap_state` stays [`CAP_STATE_OK`]. The 30-day intermittency alarm cannot
/// catch it, because that alarm watches for a *silent* device and this one is
/// talking. Without this state a rotting custodian renders healthy indefinitely.
pub const AUDIT_STATE_FAILED: &str = "failed";

/// The reserved-set prefix an **ordinary folder's destination mirror** rests
/// under — `__folder/<source-nest-id-hex>/<folder-id>`
/// (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage).
///
/// It lives here, in `fauna-core`, because the *routing* it drives is a
/// cross-crate policy question rather than a formatting detail: the bytes in
/// this plane are the folder's own content-layer ciphertext, mirrored as-is
/// with **no re-seal**, so an auditor holding the nest-backup root key cannot
/// open them and must not try. A client never invents a set name: it holds the
/// one `attach_folder` answered, and re-derives it ([`folder_backup_set_name`])
/// only to re-key a held name under the box's current identity on a verified
/// rotation chain (`segment-backup-protocol.md` § Cross-location backup
/// protocol → *The writer seat*).
///
/// ⚠ The same prefix also opens a custodian store's **local path** for a
/// mirrored file (`{folder_set}/{hex(path_hash)}`), which is why
/// [`is_folder_mirror_set`] takes any name-or-path string: both audit arms ask
/// the same question of the two different strings they happen to hold.
pub const FOLDER_MIRROR_SET_PREFIX: &str = "__folder/";

/// Whether a custody set name — or a custodian-store path inside one — belongs
/// to the ordinary-folder mirror plane, and therefore takes the
/// **hash-verified-presence** audit rather than the full open.
///
/// The distinction is the goal doc's, ratified at
/// `backup-destinations.md` § Ordinary-folder coverage → *Retention + audit*:
/// hash-verified presence is the floor for this plane, and openability "is not
/// required where the auditor legitimately lacks keys". A reserved segment set
/// (`__mail`, `__conv/<channel>`, …) is sealed under the nest-backup root the
/// auditor does hold, so it keeps the stronger test.
///
/// Getting this backwards is not a weaker audit but a **permanently red** one:
/// every mirrored file fails an open the keys can never satisfy, and an alarm
/// that always fires is worse than no alarm — it buries the real custody rot
/// this mechanism exists to surface.
pub fn is_folder_mirror_set(set_name_or_path: &str) -> bool {
    set_name_or_path.starts_with(FOLDER_MIRROR_SET_PREFIX)
}

/// Read a **folder-axis** mirror set name (`__folder/<source-nest-hex>/<id>`)
/// back into the `(source_nest_id, folder_id)` it was derived from, or `None`
/// if the nest's `folder_backup_set_name` could not have produced it.
///
/// ## Why this is a separate function from `parse_reserved_backup_set_name`
///
/// That one (`fauna_sync_engine::segment_backup`) deliberately refuses this axis, and the refusal is load-bearing
/// **where it applies**: a folder set name embeds a *source nest id*, and the
/// **federated** custody relay must never let a writer declare one — it
/// re-derives the name from its handshake's verified `origin_nest_id`, so a
/// foreign nest can never aim custody at another source nest's mirror
/// namespace. Nothing here weakens that: the federated arm does not call this.
///
/// The **owner-authed** door is a different trust problem, and
/// `message-segment-store.md` § Client-device custodian → *Restore* ratifies it
/// (2026-08-22): re-seed pushes covered-folder mirrors "under their held
/// `__folder/<source-nest-hex>/<folder-id>` names; writer-declared names are
/// safe on this door because the authenticated writer is the owner aiming only
/// at their own actor-scoped custody". A declared source-nest id there crosses
/// no principal boundary — the set is minted under the caller's own actor id and
/// paid for by their own quota, so the worst an owner can do by declaring a
/// wrong id is mis-file their *own* corpus for their *own* later materialize.
/// The federation door's concern — attribution of a *foreign* writer's bytes —
/// has no analogue.
///
/// Parse-then-re-derive, like its sibling, so the two can never disagree.
pub fn parse_folder_backup_set_name(name: &str) -> Option<([u8; 32], i64)> {
    let rest = name.strip_prefix(FOLDER_MIRROR_SET_PREFIX)?;
    let (nest_hex, folder_hex) = rest.split_once('/')?;
    let mut source_nest_id = [0u8; 32];
    hex::decode_to_slice(nest_hex, &mut source_nest_id).ok()?;
    // A leading `+`, whitespace, or a zero-padded `007` all parse under a laxer
    // reader and re-derive to a different string; the re-derive check below is
    // what rejects them, so this stays a plain parse.
    let folder_id: i64 = folder_hex.parse().ok()?;
    // Re-derive is necessary but NOT sufficient here, and the difference is the
    // whole reason this check is spelled out: `folder_id` is a SQLite rowid, so
    // a real one is always >= 1, but the derivation is total and happily renders
    // `-1` — which then round-trips perfectly. Admitting it would let an
    // owner-authed caller mint reserved-shaped sets naming folders that can
    // never exist. The rowid floor is the door's admission rule, so it lives
    // here rather than in the derivation, which stays a plain formatter.
    if folder_id < 1 {
        return None;
    }
    (folder_backup_set_name(&source_nest_id, folder_id) == name)
        .then_some((source_nest_id, folder_id))
}

/// The reserved destination-side set name an **ordinary folder's** mirrored
/// corpus rests in — `__folder/<source-nest-id-hex>/<folder-id>`.
///
/// The one derivation, shared by the nest (its `db::sync_storage` re-exports
/// this rather than keeping a second copy), the sync engine and the client
/// crates — here in `fauna-core` because the sync engine depends on both
/// client crates, so neither could reach it there. [`parse_folder_backup_set_name`]
/// is its inverse.
pub fn folder_backup_set_name(source_nest_id: &[u8; 32], folder_id: i64) -> String {
    format!(
        "{FOLDER_MIRROR_SET_PREFIX}{}/{folder_id}",
        hex::encode(source_nest_id)
    )
}

/// One row per **enrolled destination** — the destination's own enrollment
/// row, never one of its per-folder coverage rows.
///
/// `attach_backup_destination_folder` clones the enrolled row as a template
/// for each covered folder, so a destination with N attached folders has N+1
/// rows in a `fauna.state.backup` destination list sharing one `destination_id` and
/// differing only in `folder_name` (`docs/goal/behavior/backup-destinations.md`
/// § Ordinary-folder coverage → *Coverage state + wire shape* ratifies that row
/// model). Every reader that renders, audits, or counts **destinations** —
/// rather than coverage rows — must fold on `destination_id` first or it sees
/// the same destination once per attached folder: the page renders it N+1
/// times, an audit pass queries it N+1 times, and a merge emits N+1 records
/// for one destination.
///
/// The enrollment row is identified by shape, not position — the one row
/// whose `folder_name` is *not* a folder-mirror set — so callers that still
/// need every row for a `destination_id` (the audit's own
/// [`is_folder_mirror_set`]-routed inclusion check) keep reading the raw
/// slice; this fold is for callers that want one row per destination.
pub fn distinct_destinations(destinations: &[BackupDestination]) -> Vec<BackupDestination> {
    let mut seen = std::collections::BTreeSet::new();
    destinations
        .iter()
        .filter(|d| !is_folder_mirror_set(&d.folder_name))
        .filter(|d| seen.insert(d.destination_id.clone()))
        .cloned()
        .collect()
}

impl Default for BackupDestination {
    /// Hand-written rather than derived so `kind` is `"nest"` and not `""`.
    ///
    /// The struct doc above explains why this matters: `..Default::default()`
    /// is the ratified fixture convention, every such fixture predates the
    /// discriminator and means "a peer nest", and a derived `Default` would
    /// quietly reclassify all of them as [`DestinationKind::Inert`].
    fn default() -> Self {
        Self {
            destination_id: String::new(),
            destination_nest_url: String::new(),
            destination_actor_pubkey: [0u8; 32],
            folder_name: String::new(),
            added_at: 0,
            display_name: None,
            kind: default_destination_kind(),
            custodian_device_id: None,
            capacity_cap_bytes: None,
        }
    }
}

/// A typed view over a [`BackupDestination`]'s flat, additive at-rest shape.
///
/// The at-rest row stays flat so evolution is additive-everywhere
/// (`docs/goal/architecture/version-compatibility.md`); this view is the *code*
/// shape, and its job is to make one specific bug unrepresentable: reading a
/// nest-only field on a row that is not a nest. On a `client-device` or `s3`
/// row, `destination_nest_url` and `destination_actor_pubkey` rest at empty and
/// zero sentinels, so a build that string-matched the kind and then reached for
/// the URL anyway would try to back up to `""`.
///
/// Because a client may be **older** than the peer that wrote a row, an
/// unrecognised kind is a normal condition rather than an error — it lands in
/// [`DestinationKind::Inert`], which carries no fields to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestinationKind<'a> {
    /// A peer nest — the v1 kind. Bytes are pushed by the source nest's
    /// coordinator (`docs/goal/behavior/backup-destinations.md` § Custodian contract, question 2).
    Nest {
        /// Origin URL of the destination nest.
        nest_url: &'a str,
        /// The destination nest's 32-byte Ed25519 identity.
        actor_pubkey: &'a [u8; 32],
    },
    /// One of the owner's own devices. Bytes are **pulled** by the device when
    /// it is awake — it has no stable address, so it can never be pushed to.
    ClientDevice {
        /// The custodian device's stable sync `device_id`.
        device_id: &'a str,
        /// The user-set capacity cap in bytes, if recorded.
        capacity_cap_bytes: Option<u64>,
    },
    /// Not actionable by this build — either a kind this version does not
    /// implement (a newer client wrote it), or a row whose required per-kind
    /// field is absent. Callers render such a row opaquely and never drive it.
    Inert {
        /// The kind string as written, for display and diagnostics.
        kind: &'a str,
    },
}

impl BackupDestination {
    /// Project this row onto its typed [`DestinationKind`].
    ///
    /// This is the only sanctioned way to read the per-kind fields; see the
    /// enum's docs for what it prevents.
    pub fn kind_view(&self) -> DestinationKind<'_> {
        match self.kind.as_str() {
            DESTINATION_KIND_NEST => DestinationKind::Nest {
                nest_url: &self.destination_nest_url,
                actor_pubkey: &self.destination_actor_pubkey,
            },
            DESTINATION_KIND_CLIENT_DEVICE => match self.custodian_device_id.as_deref() {
                Some(device_id) => DestinationKind::ClientDevice {
                    device_id,
                    capacity_cap_bytes: self.capacity_cap_bytes,
                },
                // Not drivable: the nest keys the status projection on this id.
                None => DestinationKind::Inert { kind: &self.kind },
            },
            _ => DestinationKind::Inert { kind: &self.kind },
        }
    }

    /// This row as the shared custodian-matching input
    /// ([`custodian_assignment_for`]).
    pub fn custodian_row(&self) -> CustodianRowRef<'_> {
        CustodianRowRef {
            destination_id: &self.destination_id,
            kind: &self.kind,
            custodian_device_id: self.custodian_device_id.as_deref(),
            capacity_cap_bytes: self.capacity_cap_bytes,
        }
    }
}

/// The four fields [`custodian_assignment_for`] needs from a destination row,
/// borrowed from whichever carrier the caller holds.
///
/// Two carriers exist for one concept: the at-rest
/// [`BackupDestination`] (which only a seed-holding client can open) and the
/// nest registry's wire row `fauna_protocol::backup::DestinationItem` (which the
/// bearer-only sync agent reads back over its authed connection —
/// `docs/goal/architecture/apps/sync-agent.md` § Control plane split, *policy
/// through the nest, never over local IPC*). Both project into this so the
/// matching rule below has one implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustodianRowRef<'a> {
    pub destination_id: &'a str,
    pub kind: &'a str,
    pub custodian_device_id: Option<&'a str>,
    pub capacity_cap_bytes: Option<u64>,
}

/// What a device needs in order to host its own custodian pull
/// (`fauna_sync_engine::custodian_pull::CustodianPull`): which destination row
/// it is check-in-ing against, and the cap it must honour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodianAssignment {
    /// The `BackupDestination::destination_id` of this device's own row.
    pub destination_id: String,
    /// The user-set cap in bytes; `None` = uncapped.
    pub capacity_cap_bytes: Option<u64>,
    /// **Which device this assignment is for** — the id the match below was made
    /// on, carried out rather than left for each host to remember separately.
    ///
    /// It rides the assignment because the check-in has to name it: the nest
    /// authenticates the owner, not the device, so a check-in that does not say
    /// who is speaking cannot be checked against the row it claims
    /// (`fauna_protocol::backup::CustodianCheckinRequest::device_id`). Keeping it
    /// here means the id that *won* the match is the id that gets reported —
    /// a host cannot pair a destination from one device with a name from
    /// another.
    pub device_id: String,
}

/// Find the custodian assignment `this_device_id` holds among `rows`, if any.
///
/// The rule, stated once for both hosts: a row assigns work to this device iff
/// its kind is `client-device` **and** its `custodian_device_id` equals this
/// device's stable sync id.
///
/// Two refusals are deliberate, and both are failures a per-host reimplementation
/// would plausibly get wrong:
///
/// * **A blank id on either side matches nothing.** A device whose capability
///   has not carried a sync id yet, or a row that somehow reached the registry
///   without one, would otherwise match each other — and the pairing is
///   invisible: the device would start sealing the owner's whole corpus onto
///   disk under a `destination_id` no status row describes.
/// * **More than one match is `None`, not the first.** Enrollment writes one row
///   per device, so two rows naming this device is a state no client produces;
///   picking one would silently honour one cap and ignore the other, and the
///   cap is the only thing standing between a custodian and a full disk.
pub fn custodian_assignment_for<'a>(
    rows: impl IntoIterator<Item = CustodianRowRef<'a>>,
    this_device_id: &str,
) -> Option<CustodianAssignment> {
    let me = this_device_id.trim();
    if me.is_empty() {
        return None;
    }
    let mut found: Option<CustodianAssignment> = None;
    for row in rows {
        if row.kind != DESTINATION_KIND_CLIENT_DEVICE {
            continue;
        }
        let Some(row_device) = row.custodian_device_id.map(str::trim) else {
            continue;
        };
        if row_device.is_empty() || row_device != me {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(CustodianAssignment {
            destination_id: row.destination_id.to_string(),
            capacity_cap_bytes: row.capacity_cap_bytes,
            device_id: me.to_string(),
        });
    }
    found
}

/// Is this row a client device this build can actually drive? — the row-level
/// half of [`every_destination_is_a_client_device`], over the carrier-agnostic
/// [`CustodianRowRef`] rather than the at-rest row.
///
/// It exists because the at-rest [`BackupDestination`] deliberately does not
/// cross the FFI boundary, while the *question* must be answered identically on
/// every carrier: the native read projection
/// (`fauna_ffi::FfiBackupDestinationView`), the nest registry's wire row
/// (`fauna_protocol::backup::DestinationItem`), and the at-rest row itself. A
/// per-carrier re-derivation from the raw `kind` string is exactly what
/// [`every_destination_is_a_client_device`]'s Inert arm exists to prevent.
///
/// The rule is `kind_view()`'s `ClientDevice`-vs-`Inert` decision stated over
/// the two fields a carrier is guaranteed to have: a `client-device` row with no
/// `custodian_device_id` is **not** a client device, because nothing can drive
/// it (the nest keys the status projection on that id). The two expressions are
/// pinned to agree by `backup_destination_kind.rs`'s
/// `row_predicate_agrees_with_kind_view`.
pub fn row_is_a_client_device(row: CustodianRowRef<'_>) -> bool {
    row.kind == DESTINATION_KIND_CLIENT_DEVICE && row.custodian_device_id.is_some()
}

/// [`every_destination_is_a_client_device`] over any carrier's rows — see that
/// function for the two arms and why each is the conservative direction, and
/// [`row_is_a_client_device`] for why the row shape is borrowed rather than the
/// at-rest struct.
///
/// An empty iterator is **not** sole-client, same as the empty slice.
pub fn every_row_is_a_client_device<'a>(
    rows: impl IntoIterator<Item = CustodianRowRef<'a>>,
) -> bool {
    let mut saw_any = false;
    for row in rows {
        saw_any = true;
        if !row_is_a_client_device(row) {
            return false;
        }
    }
    saw_any
}

/// Does **any** configured destination row name this device as its custodian? —
/// the claim half of [`custodian_store_is_orphaned`]
/// (`docs/goal/ui/backups.md` § Manage backup destinations → *Reclaim this
/// device's copy*: the orphaned-store row renders "whenever this device holds a
/// sealed store with **no** matching destination row").
///
/// # Why this is NOT `custodian_assignment_for(...).is_none()`
///
/// The two questions look identical and answer differently in exactly the case
/// that matters. [`custodian_assignment_for`] answers *"can this device host?"*
/// and returns `None` for **two** rows naming this device, because a host that
/// guessed which cap to honour is worse than one that refuses. This function
/// answers *"is this device's store still somebody's custody?"* — and two rows
/// naming it is emphatically **yes**.
///
/// Reading the refusal as an absence is how the reclaim gesture would offer to
/// delete a store whose destination rows are right there in the user's config,
/// still listed on their own Backups page, one config repair away from being
/// driven again. The store is the owner's only offline copy
/// (`behavior/backup-destinations.md` § Third destination kind → 3c-ii keeps it
/// across a removal), so the conservative direction is the whole rule.
///
/// A blank `this_device_id` claims nothing — but it does not make a store
/// orphaned either; see [`custodian_store_is_orphaned`], which refuses on it.
pub fn a_destination_row_claims_this_device<'a>(
    rows: impl IntoIterator<Item = CustodianRowRef<'a>>,
    this_device_id: &str,
) -> bool {
    let me = this_device_id.trim();
    if me.is_empty() {
        return false;
    }
    rows.into_iter().any(|row| {
        row.kind == DESTINATION_KIND_CLIENT_DEVICE
            && row.custodian_device_id.map(str::trim) == Some(me)
    })
}

/// Should this device offer to reclaim its sealed custodian store? — the
/// `backup-orphaned-store-row` render rule, shared so that no app re-derives
/// the conjunction behind a **destructive** gesture
/// (`docs/goal/ui/backups.md` § Manage backup destinations → *Reclaim this
/// device's copy*).
///
/// Two facts decide it, and each app supplies one of them from a different
/// place: `store_holds_bytes` is a **local disk** fact (desktop reads it off the
/// sync agent that hosts the store, mobile off its own disk — **not** off its
/// in-app host, which does not exist in this state: `build_custodian_host`
/// answers `None` without a registry row, and no row is exactly what makes a
/// store orphaned), while `rows`
/// is the **destination list** the page already loaded. The rule joining them is
/// neither app's to invent.
///
/// Both refusals are the conservative direction, because the gesture destroys
/// this device's standalone-restore property
/// (`behavior/backup-destinations.md` § Third destination kind → *Standalone
/// restore*):
///
/// * **A device with no id of its own is never orphaned.** An empty
///   `this_device_id` means this build cannot tell whether a row names it — and
///   "cannot tell" must not paint a delete button over the owner's only offline
///   copy. (It is also unreachable in practice: `enroll_client_custodian`
///   refuses a device with no sync id, so a store on an id-less device is a
///   state nothing produces.)
/// * **An empty store offers nothing.** Reclaim frees disk space; with no bytes
///   held there is nothing to free, and a row that renders anyway is a
///   permanent invitation to a no-op on every device that never enrolled.
pub fn custodian_store_is_orphaned<'a>(
    rows: impl IntoIterator<Item = CustodianRowRef<'a>>,
    this_device_id: &str,
    store_holds_bytes: bool,
) -> bool {
    if !store_holds_bytes || this_device_id.trim().is_empty() {
        return false;
    }
    !a_destination_row_claims_this_device(rows, this_device_id)
}

/// Does every configured destination hold its copy on one of the owner's own
/// devices? — the `backup-sole-client-destination-warning` predicate
/// (`docs/goal/behavior/backup-destinations.md` § Third destination kind → *Durability +
/// labeling*: "when **every** configured destination is a client device the page
/// shows a standing warning").
///
/// Two arms decide it, and both are the conservative direction:
///
/// * **An empty list is NOT sole-client.** Zero destinations is the empty state,
///   which every page renders its own way; asserting "all your destinations are
///   your own devices" about no destinations would paint a durability warning on
///   a fresh account that has no backup at all.
/// * **An [`DestinationKind::Inert`] row counts as *not* a client device** — an
///   unrecognised kind a newer client wrote may well BE the off-site copy this
///   warning would otherwise tell the user they do not have, and crying wolf at
///   someone who is covered is how a standing warning gets tuned out. (A
///   `client-device` row missing its device id is Inert too, and it is likewise
///   no evidence of a copy: nothing can drive it.)
///
/// Shared because it is a *policy* answer, not a rendering one: it decides
/// whether the user is told their durability story is weaker than they think,
/// and seven apps re-deriving that from `kind` strings is seven chances to get
/// the Inert arm backwards. tui wrote it privately when it led the surface
/// (2026-08-03) and recorded it as a shared-Rust candidate for the second app to
/// render the element; linux was that app.
pub fn every_destination_is_a_client_device(destinations: &[BackupDestination]) -> bool {
    every_row_is_a_client_device(destinations.iter().map(BackupDestination::custodian_row))
}

// ── Task-delegation assignment sub-record ──
//
// The `fauna.state.delegation` entry's typed contents. Lives here next to
// `BackupConfig` so the at-rest dag-cbor encoding boundary stays
// in one place; the participant **class** model + the pure policy-order
// eligibility function (which operate on *live* participant state, not this
// at-rest record) live in `crate::delegation`. Authority for the shape:
// `docs/goal/behavior/participants.md` § Task delegation → Data shape
// (shape-level ratified 2026-07-06; field-level still refutable).

/// Per-user task-delegation assignments (participants unification, Q-C).
/// `DelegationConfig::default()` (empty) is the works-out-of-the-box state:
/// zero configuration yields the pure automatic policy order
/// (`crate::delegation::current_candidates`). A non-empty `assignments` list
/// carries the user's explicit per-task-kind pins.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationConfig {
    /// One entry per task kind the user has expressed a preference for. A task
    /// kind absent from this list runs on the pure automatic policy order.
    #[serde(default)]
    pub assignments: Vec<TaskAssignment>,
}

/// One task-kind assignment. Absent `pinned_to` ⇒ automatic (policy order);
/// `Some(participant)` ⇒ the user pinned this kind to that participant (the
/// escape hatch, Q-B). Merged whole-record with the rest of `delegation`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAssignment {
    /// Stable kind string, e.g. `"backup-upload"` | `"content-rescore"` |
    /// `"index"` (participants.md § Concepts). Free-form + extensible: new
    /// kinds are additive, and an assignment naming a kind this binary doesn't
    /// know is preserved on re-seal (it round-trips as data, never dropped).
    pub task_kind: String,
    /// `None` ⇒ automatic (the policy order picks the runner). `Some(_)` ⇒
    /// pinned to exactly this participant; it runs iff currently eligible,
    /// else the kind waits for it (`crate::delegation::current_candidates`).
    #[serde(default)]
    pub pinned_to: Option<ParticipantRef>,
}

/// Identifies a single participant — one of the user's client devices, or one
/// of the user's nests — as the key into the shared participant-row model
/// (participants.md § The participant model). The arms mirror the identity
/// each surface already uses at rest: a device by its hex-encoded 32-byte id
/// (matching `DeviceSummary.device_id`), a nest by its raw 32-byte Ed25519
/// actor pubkey (matching `BackupDestination.destination_actor_pubkey` /
/// `DeploymentSeedEntry.nest_actor_id`). Field-level refutable per the goal
/// doc; this pairing avoids a type conversion at every comparison against
/// those two surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParticipantRef {
    /// A client device, keyed by its hex-encoded 32-byte device id.
    Device { device_id: String },
    /// A nest, keyed by its 32-byte Ed25519 actor pubkey.
    Nest {
        #[serde(with = "serde_bytes")]
        actor_pubkey: [u8; 32],
    },
    /// A participant kind a newer build writes and this one does not name,
    /// carried whole so the delegation record's re-seal keeps it
    /// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in
    /// full*). It matches no participant this build knows, so a kind pinned to
    /// one has nobody eligible and waits.
    #[serde(untagged)]
    Unknown(CarriedValue),
}

impl DelegationConfig {
    /// Set (or clear) the pin for a task kind — the escape-hatch write behind
    /// the Task-delegation assignment picker (participants.md § Concepts →
    /// Assignment, slice 4). `Some(participant)` upserts a pin for the kind;
    /// `None` clears it back to automatic by **removing** the row (absent ⇒
    /// automatic, so the config stays minimal — empty is the pure
    /// works-out-of-the-box policy order). The whole `delegation` record is
    /// merged latest-wins on sync, so a cleared pin round-trips as a removed row.
    pub fn set_pin(&mut self, task_kind: &str, pinned_to: Option<ParticipantRef>) {
        match pinned_to {
            Some(to) => {
                if let Some(existing) = self
                    .assignments
                    .iter_mut()
                    .find(|a| a.task_kind == task_kind)
                {
                    existing.pinned_to = Some(to);
                } else {
                    self.assignments.push(TaskAssignment {
                        task_kind: task_kind.to_string(),
                        pinned_to: Some(to),
                    });
                }
            }
            None => self.assignments.retain(|a| a.task_kind != task_kind),
        }
    }
}

// ── DNS-provider-credential sub-record ──
//
// The `fauna.state.dns` entry's typed contents. Lives here so the BARE
// encoding boundary stays in one place;
// the credential lifecycle / publish-reconcile / effective-mode
// projection live in `libs/fauna-client-dns`. Authority for the
// at-rest shape is `docs/goal/architecture/config-dissolution.md`;
// authority for behavior + the client-held / nest-never-sees-it
// invariant is `docs/goal/behavior/dns-management.md`
// § Where the credential lives + § The two modes.

/// Per-actor DNS-provider credentials + the per-domain "Fauna controls
/// DNS" opt-in. `DnsConfig::default()` is the no-credentials,
/// no-managed-domains state (the manual-DNS path). Sealed under
/// BackupKey on the account plane (`fauna.state.dns`); the nest stores only
/// bytes it cannot read in either storage mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsConfig {
    /// One row per (provider, set of zones) the admin holds — a
    /// deployment may hold e.g. a Hetzner credential covering
    /// `example.com` and a Porkbun credential covering `example.org`
    /// (`dns-management.md` § App surface, multi-credential shape).
    pub credentials: Vec<DnsProviderCredential>,
    /// Domains the admin has opted into "Fauna controls DNS" for. This
    /// is only the opt-in half; **effective** managed mode is the
    /// client-side projection `opted_in ∧ a held credential's `zones`
    /// cover the domain`, computed by `DnsManagementMachine`
    /// (`dns-management.md` § The two modes). A `BTreeSet` for
    /// deterministic canonical-dag-cbor encoding.
    pub managed_domains: BTreeSet<String>,
    /// Serialized ACME `AccountCredentials` for the client-driven DNS-01
    /// issuance flow (`tls-certificates.md` § C mitigation 3, D6): a
    /// nest-opaque JSON blob persisted here BackupKey-sealed like the rest of
    /// the DNS record, reused across the admin's devices and renewals so every
    /// synced device renews against the **same** ACME account (no new-account
    /// churn against the CA's rate limits). Written by `DnsManagementMachine`'s
    /// `IssueCert` orchestration — the sole writer; `None` until the first
    /// issuance. `#[serde(default)]` keeps configs written before this field
    /// existed decodable (→ `None`). A CBOR byte string (`serde_bytes`), the
    /// `fauna.state.dns` kind's byte-string rule (`config-dissolution.md`
    /// § Phases and gates → *Bounded rows*).
    #[serde(default, with = "serde_bytes")]
    pub acme_account: Option<Vec<u8>>,
    /// One-time `_acme-challenge` CNAME delegations for **manual-mode** domains
    /// (`tls-certificates.md` § B tier 3, S6b): after the admin sets a single
    /// CNAME at their no-API registrar, the order publishes the renewal
    /// `_acme-challenge` TXT into a zone the admin *does* control (a held
    /// credential covers it), so recurring renewals automate from that one
    /// manual step. Written by `DnsManagementMachine`'s `DelegateRenewal` /
    /// `RemoveDelegation` actions — the sole writer; empty until the admin sets
    /// up a delegation. `#[serde(default)]` keeps pre-existing configs decodable.
    #[serde(default)]
    pub delegations: Vec<CnameDelegation>,
    /// Domains the admin has turned automatic certificate renewal **off** for.
    /// Auto-renew defaults **on** for every managed/delegated domain — a synced
    /// admin device silently re-issues when the served cert nears expiry
    /// (`tls-certificates.md` § C.3, "any synced admin device renews"), so the
    /// common case needs no action. This stores only the opt-OUTs: an empty set
    /// ⇒ every managed/delegated domain auto-renews. Written by
    /// `DnsManagementMachine`'s `SetAutoRenew` action — the sole writer. A
    /// `BTreeSet` for deterministic canonical-dag-cbor encoding + clean
    /// cross-branch merge; `#[serde(default)]` keeps pre-existing configs
    /// decodable (→ empty ⇒ default-on, the intended migration).
    #[serde(default)]
    pub auto_renew_off: BTreeSet<String>,
    /// Fauna-published DKIM TXT names per domain — the client-side memory the
    /// withdraw-aware DKIM converge pass diffs against
    /// (`fauna-client-dns::reconcile_dkim_txt`; `dns-management.md`
    /// § Fauna-managed → *Withdraw-aware convergence*). The pass visits
    /// `desired ∪ remembered` names, so a selector the nest has revoked — its
    /// `<selector>._domainkey.<domain>` name no longer in the record matrix —
    /// still gets its stale published TXT withdrawn, while **only** names Fauna
    /// itself once desired are ever visited (never a zone-wide `_domainkey`
    /// sweep, which would clobber a third-party mailer's DKIM record at the
    /// same domain). Keyed by domain; values are fully-qualified
    /// `<selector>._domainkey.<domain>` names. Written by
    /// `DnsManagementMachine`'s managed publish — the sole writer.
    /// `BTreeMap`/`BTreeSet` for deterministic canonical-dag-cbor encoding;
    /// `#[serde(default)]` keeps pre-existing configs decodable (→ empty ⇒
    /// nothing remembered, the pass starts from the matrix alone).
    #[serde(default)]
    pub dkim_published_names: BTreeMap<String, BTreeSet<String>>,
    /// Fauna-published `_atproto.<handle>.<primary>` TXT names per domain — the
    /// same client-side memory as [`Self::dkim_published_names`], for the
    /// ATProto handle-verification slot (`fauna-client-dns::reconcile_atproto_txt`;
    /// `dns-management.md` § Fauna-managed → *Withdraw-aware convergence*,
    /// `atproto-pds-bridge.md` § Handle). The memory is what makes a **rename**
    /// converge: the handle is derived at read time, so a renamed identity's old
    /// `_atproto` name leaves the record matrix entirely and a matrix-only visit
    /// set could never reach the stale TXT it left at the provider. Keyed by
    /// domain; values are fully-qualified `_atproto.<handle>.<domain>` names.
    /// Written by `DnsManagementMachine`'s managed publish — the sole writer.
    /// `#[serde(default)]` keeps pre-existing configs decodable (→ empty ⇒
    /// nothing remembered, the pass starts from the matrix alone).
    #[serde(default)]
    pub atproto_published_names: BTreeMap<String, BTreeSet<String>>,
    /// Fauna-published `_fauna.<primary-domain>` identity-root TXT names per
    /// domain — the same client-side memory as [`Self::dkim_published_names`],
    /// for the public-domain identity root (`fauna-client-dns::reconcile_fauna_self_txt`;
    /// `dns-management.md` § Records covered + § Fauna-managed → *Withdraw-aware
    /// convergence*). The withdraw case is a **deployment-seed rotation**, which
    /// is unlike the other two slots: the name never moves, only the `self=`
    /// VALUE changes (`box-recovery.md` § Deployment-seed rotation). The memory
    /// therefore does *not* rescue a name that left the matrix — it records that
    /// this client owns the slot, so a later publish still converges it after
    /// the domain stops being primary (or a failed withdraw must be retried).
    /// Keyed by domain; values are fully-qualified `_fauna.<domain>` names.
    /// Written by `DnsManagementMachine`'s managed publish — the sole writer.
    /// `#[serde(default)]` keeps pre-existing configs decodable (→ empty ⇒
    /// nothing remembered, the pass starts from the matrix alone).
    #[serde(default)]
    pub fauna_self_published_names: BTreeMap<String, BTreeSet<String>>,
    /// A **manual-mode** DNS-01 issuance the admin has begun but not yet
    /// completed — the breadcrumb that lets a *fresh* `DnsManagementMachine`
    /// re-surface the paste card instead of silently dropping the order
    /// (`tls-certificates.md` § Surviving an interrupted manual issuance). The live `Dns01OrderInProgress` is a process-local handle and
    /// is deliberately **not** stored here; what is stored is enough to (a) show
    /// the admin the exact TXT they were asked to publish and (b) resume by
    /// opening a fresh order for the same domain and checking the challenge is
    /// unchanged. Written by `DnsManagementMachine`'s
    /// `BeginManualIssueCert` / `CompleteManualIssueCert` /
    /// `CancelManualIssueCert` — the sole writer; `None` whenever no manual
    /// issuance is in flight. `#[serde(default)]` keeps pre-existing configs
    /// decodable (→ `None` ⇒ nothing in flight, the pre-breadcrumb behaviour).
    #[serde(default)]
    pub pending_manual_issue: Option<PendingManualIssue>,
}

/// The persisted breadcrumb for an in-flight **manual-mode** DNS-01 issuance
/// ([`DnsConfig::pending_manual_issue`]). Manual issuance is a two-phase flow
/// with an out-of-band step in the middle — the admin pastes a TXT at their
/// registrar and waits for it to propagate — and the propagation gate can hold
/// the completion for up to 45 minutes (`tls-certificates.md` § B, the
/// propagation gate). Anything that rebuilds the machine in that window (a page
/// navigation, an app restart, picking up on another device) used to destroy the
/// order with no error at all; this record is what survives instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingManualIssue {
    /// The domain the cert is being issued for.
    pub domain: String,
    /// The private nest the issued cert is sealed and delivered to — the
    /// `target_nest_id` the begin step was given, so a resume needs no second
    /// round trip to re-derive it. A CBOR byte string (`serde_bytes`).
    #[serde(with = "serde_bytes")]
    pub target_nest_id: Vec<u8>,
    /// The exact `_acme-challenge` TXT record(s) the admin was asked to publish.
    /// A resume compares these against a fresh order's challenges: unchanged ⇒
    /// the published TXT still validates and completion proceeds; changed ⇒ the
    /// admin is shown the new value rather than left waiting on a token no CA
    /// will ever ask for.
    pub challenges: Vec<PendingChallengeRecord>,
    /// When the issuance was begun — not used to expire the breadcrumb (a
    /// resume's fresh-order comparison handles staleness on its own), but it is
    /// the one fact about an interrupted order that cannot be reconstructed
    /// afterwards.
    pub started_at: Timestamp,
}

/// One `_acme-challenge` TXT record inside a [`PendingManualIssue`] — the
/// at-rest twin of the `DnsRecordRow` the paste card renders, minus the live
/// red/green verdict (which is re-derived by the page's verify pass, never
/// persisted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingChallengeRecord {
    /// Owner name — `_acme-challenge.<domain>` (or the delegated target name).
    pub name: String,
    /// Record type; always `TXT` for an ACME DNS-01 challenge, stored rather
    /// than assumed so the row projects back with no re-derivation.
    pub record_type: String,
    /// The order's raw key-authorization digest — the exact value to paste.
    pub value: String,
    pub ttl_seconds: u32,
}

/// A one-time `_acme-challenge` CNAME delegation for a manual-mode domain
/// (`tls-certificates.md` § B tier 3, S6b). The admin sets, **once**, a CNAME
/// `_acme-challenge.<domain>` → `target_name` at their no-API registrar;
/// thereafter the client publishes each renewal's `_acme-challenge` TXT at
/// `target_name` inside `target_zone` (covered by a held credential) and the CA
/// follows the CNAME, so renewals no longer need a manual paste.
///
/// `target_name` is **authoritative** — it is the exact name the admin's CNAME
/// points at (`_acme-challenge.<domain>.<target_zone>` by the re-homing
/// convention, but persisted explicitly so a future convention change never
/// orphans a delegation whose CNAME is already set).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CnameDelegation {
    /// The manual-mode domain whose `_acme-challenge` renewals are delegated.
    pub domain: String,
    /// The owner name the renewal `_acme-challenge` TXT is published at (and the
    /// CNAME target the admin set): `_acme-challenge.<domain>.<target_zone>`.
    pub target_name: String,
    /// The controlled zone (covered by a held DNS credential) `target_name`
    /// lives in — where the renewal TXT is published via that credential's seam.
    pub target_zone: String,
}

/// One DNS-provider credential the admin's client holds. The `fields`
/// bag maps a provider field-id (keyed by `providers.yaml` ids — e.g.
/// Hetzner `api-token`, Porkbun `api-key` + `secret-api-key`) to its
/// secret value. The values are sealed at rest under BackupKey on the
/// account plane (`fauna.state.dns`); in memory each value is a [`SecretString`] (zeroized
/// on drop, redacted `Debug`), mirroring `MailCredential.secret`'s
/// [`crate::secret::SecretBytes`] — the text twin, since DNS-provider tokens are text.
/// `SecretString` serializes byte-identically to a plain `String`, so the
/// at-rest dag-cbor record is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsProviderCredential {
    /// `fauna_provisioning::ProviderId::as_str()` (e.g. `"hetzner"`).
    pub provider_id: String,
    /// Provider field-id (public, plain `String`) → secret value
    /// ([`SecretString`]), keyed by `providers.yaml` field ids. Un-wrapped
    /// to the plain `String` `fauna_provisioning::Credentials::entries`
    /// expects only at the provider-API boundary (`fauna-client-dns`'s
    /// native seam).
    pub fields: Vec<(String, SecretString)>,
    /// Cached `verify()` zones so the effective-mode coverage check
    /// (does any held credential cover this domain?) needs no network
    /// round trip on every snapshot projection.
    pub zones: Vec<DnsZoneRef>,
    /// User-facing label, e.g. `"Hetzner (example.com)"`.
    pub label: String,
    /// Unix seconds.
    pub created_at: u64,
}

/// A cached DNS zone (`{id, name}`) from a provider `verify()`. Mirrors
/// `fauna_provisioning::DnsZone`, duplicated here because `fauna-core`
/// sits **below** `fauna-provisioning` in the dependency graph (the
/// provider crate depends on core, not vice-versa, so core cannot name
/// the provider type); `fauna-client-dns` maps between the two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsZoneRef {
    pub id: String,
    pub name: String,
}

// ── ATProto identity sub-record ──

/// Client-held ATProto identity custody (the `fauna.state.atproto-identity` rows). `default()` is the no-identity state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AtprotoIdentityConfig {
    /// User-custodied PLC rotation **keyring**, in deterministic
    /// `(created_at, pubkey)` order. The order is a *stability* order for
    /// CAS-merge convergence — it does **not** encode per-DID seniority.
    /// Seniority is a property of each *identity*: the key placed at
    /// `rotationKeys[0]` of a DID's published PLC operations (the did:plc
    /// spec orders that array by descending authority, so that key can always
    /// out-rotate every bridge-held key within the 72 h recovery window), and
    /// each key remembers the DIDs it was published senior for
    /// ([`AtprotoRotationKey::published_for_dids`]). A mint uses a key never
    /// published for any DID — fresh-key-per-mint, so a retirement's
    /// replacement identity shares no key with the retired one's public log.
    /// Consumers derive "which key matters" from the published log (custody
    /// verification: every standing op's senior key ∈ this ring; tombstone:
    /// the ring key the standing head lists), never from list position.
    #[serde(default)]
    pub rotation_keys: Vec<AtprotoRotationKey>,
    /// DIDs whose terminal PLC retirement the USER explicitly consented to —
    /// the client-authored half of the tombstone opt-in, written only by the
    /// client-side sole writer at the moment the user ticks the opt-in
    /// (before the nest RPC that records the durable intent, mirroring the
    /// mint's store-before-publish ordering). The converge pass publishes a
    /// tombstone only when the nest's recorded intent AND this record agree:
    /// nest testimony alone must never be able to make a client perform a
    /// terminal, irreversible act with the user's own key. Additive (`serde(default)`), sorted + deduped,
    /// union-merged like the ring: consent recorded on one device must
    /// survive a CAS merge so any device can finish the retirement.
    #[serde(default)]
    pub tombstone_consents: Vec<String>,
    /// The USER's explicit, per-op consents to contest a box-authored PLC
    /// operation with the held senior rotation key — the 72 h recovery fork
    /// (`atproto-pds-bridge.md` § State & data shape, the recovery-fork
    /// contest, decisions 6 and 7).
    ///
    /// Scoped to the named op by CID, never a standing "contest anything"
    /// flag: an always-on contester is a *detector* promoted to an *actor*
    /// with power to nullify the user's own out-of-band operations, so a new
    /// hostile op after a completed contest requires a new human decision.
    /// Additive (`serde(default)`), union-merged like the ring so a gesture
    /// made on one device lets any device finish the contest inside the 72 h
    /// window. Written only by
    /// [`fauna_client_atproto::rotation_key::record_contest_intent`]; the
    /// nest neither sees nor stores any of this (decision 9 — the contest adds
    /// nothing to the nest or the wire).
    #[serde(default)]
    pub contest_intents: Vec<AtprotoContestIntent>,
    /// Every did:plc the nest has **ever** named as this account's hosted
    /// identity — frozen client-side so it can never be retracted. The second
    /// source of feeder #1's audit floor.
    ///
    /// ## Why this exists beside [`AtprotoRotationKey::published_for_dids`]
    ///
    /// That field is directory-derived and therefore *stronger* evidence — but
    /// it is written **only on a passing custody verdict**, so it is empty for
    /// a DID whose genesis was compromised: exactly the case feeder #1 exists
    /// to detect. A floor built on it alone left the genesis-time attack fully
    /// nest-gated: mint under a hostile key, then answer `identity: null` and
    /// no client would ever look.
    ///
    /// The security property here is **different and weaker, and that is
    /// deliberate**: this value *is* nest testimony, so it proves nothing about
    /// custody. What it provides is *irrevocability* — the nest cannot unsay
    /// what it already said. That is sufficient for a floor, because a floor
    /// only ever **adds** audit targets and can never mute one (the ring's own
    /// bounding clause). Do not read an entry here as evidence the DID is the
    /// user's; read it as "this box once claimed it, so it does not get to
    /// withdraw the claim to dodge the audit".
    ///
    /// Additive (`serde(default)`), sorted + deduped, union-merged like
    /// [`Self::tombstone_consents`]. Absent ⇒ no floor from this
    /// source, the same as an empty list — never a poisoned one. The binding
    /// lives here and not as a parallel field inside `AtprotoRotationKey` so a
    /// partial write cannot leave half a record (the half-state lesson). Deliberately **not** consulted by
    /// [`fauna_client_atproto::rotation_key::mint_rotation_key`]'s reuse
    /// predicate, which still reads an empty `published_for_dids` as
    /// "reusable".
    #[serde(default)]
    pub nest_named_dids: Vec<String>,
}

/// One scoped consent to contest a specific standing PLC operation.
///
/// Inert by construction once spent: the converge pass re-derives the log's
/// first standing violation every time and signs nothing unless it is still
/// this exact CID, so an intent whose named op has left the standing chain
/// (contested, or superseded by a different attack) can never authorize a
/// second signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtprotoContestIntent {
    /// The did:plc whose log is contested.
    pub did: String,
    /// CID of the standing operation the user consented to displace — the
    /// first op violating custody at the moment of the gesture. The scope of
    /// the consent, and what the converge pass re-checks the fresh log
    /// against.
    pub contested_op_cid: String,
    /// Unix seconds at the gesture. Diagnostic only: the 72 h deadline is
    /// computed from the *contested op's* own published `createdAt`, and the
    /// directory's acceptance is the truth either way.
    pub requested_at: u64,
}

impl AtprotoContestIntent {
    /// The per-intent half of [`AtprotoIdentityConfig::merge`], for two
    /// intents naming the same `(did, contested_op_cid)` scope: the earlier
    /// gesture's `requested_at` wins. The field is diagnostic only, but "keep
    /// whichever side ran the merge" is a local preference, which a
    /// convergent merge cannot hold (the rotation key's `created_at` fold).
    /// The `fauna.state.atproto-identity` plane arm runs this on one
    /// `intent/` row.
    #[must_use]
    pub fn merge(&self, other: &Self) -> Self {
        Self {
            requested_at: self.requested_at.min(other.requested_at),
            ..self.clone()
        }
    }
}

impl AtprotoIdentityConfig {
    /// Two-device union merge (the `subscriptions`/`folders` arm, NOT
    /// latest-wins): a rotation key is irrecoverable key material — dropping
    /// one forfeits the recovery seniority the custody split exists for — so
    /// every distinct key survives. Deterministic + commutative: dedup by
    /// pubkey — same-pubkey entries **union their `published_for_dids`**
    /// (each device converges bindings from the public log independently, and
    /// a dropped binding would un-burn a published key, reopening the re-mint
    /// linkability) — then order by `(created_at, pubkey)` so both devices
    /// converge on the SAME list. The order is stability, not seniority: see
    /// the field doc on [`Self::rotation_keys`].
    pub fn merge(&self, other: &Self) -> Self {
        let mut rotation_keys = self.rotation_keys.clone();
        for k in &other.rotation_keys {
            if let Some(existing) = rotation_keys
                .iter_mut()
                .find(|e| e.pubkey_did_key == k.pubkey_did_key)
            {
                *existing = existing.merge(k);
            } else {
                rotation_keys.push(k.clone());
            }
        }
        rotation_keys.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.pubkey_did_key.cmp(&b.pubkey_did_key))
        });
        let mut tombstone_consents = self.tombstone_consents.clone();
        for did in &other.tombstone_consents {
            if !tombstone_consents.contains(did) {
                tombstone_consents.push(did.clone());
            }
        }
        tombstone_consents.sort();
        // Union by `(did, contested_op_cid)`: the pair IS the scope of the
        // consent, so two devices contesting different ops of the same DID
        // keep both intents, and the same gesture converged twice collapses
        // to one. Sorted on the same pair for CAS-merge determinism.
        let mut contest_intents = self.contest_intents.clone();
        for intent in &other.contest_intents {
            if let Some(existing) = contest_intents
                .iter_mut()
                .find(|i| i.did == intent.did && i.contested_op_cid == intent.contested_op_cid)
            {
                *existing = existing.merge(intent);
            } else {
                contest_intents.push(intent.clone());
            }
        }
        contest_intents.sort_by(|a, b| {
            a.did
                .cmp(&b.did)
                .then_with(|| a.contested_op_cid.cmp(&b.contested_op_cid))
        });
        // Union like `tombstone_consents`: a DID the nest named on ONE device
        // must reach every device's audit floor, and a merge that dropped it
        // would hand the box back the retraction this field exists to deny.
        let mut nest_named_dids = self.nest_named_dids.clone();
        for did in &other.nest_named_dids {
            if !nest_named_dids.contains(did) {
                nest_named_dids.push(did.clone());
            }
        }
        nest_named_dids.sort();
        Self {
            rotation_keys,
            tombstone_consents,
            contest_intents,
            nest_named_dids,
        }
    }
}

/// One user-custodied PLC rotation key (NIST P-256 — spec-valid for rotation
/// keys, and the curve the client fleet already ships for ACME account keys).
/// The scalar never leaves the client; the nest and bridge see only
/// `pubkey_did_key`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtprotoRotationKey {
    /// Raw P-256 scalar, zeroize-on-drop + redacted `Debug`
    /// ([`SecretArray32`] serializes as a 32-byte CBOR byte string, exactly
    /// like a `serde_bytes` `[u8; 32]` field, so the at-rest dag-cbor record
    /// carries no wrapper).
    pub secret_scalar: SecretArray32,
    /// The public half as a `did:key:zDn…` string — what the enable flow
    /// sends nest-side into the identity row / PLC genesis op.
    pub pubkey_did_key: String,
    /// Unix seconds.
    pub created_at: u64,
    /// The DIDs whose **published** PLC operation log lists this key at
    /// `rotationKeys[0]` — provenance recorded from the directory's own log
    /// (never from nest testimony), sorted + deduped for deterministic
    /// merging. A key with an entry here is *burned for minting*: sending it
    /// into a second mint would publicly link the two DIDs' logs, the
    /// linkability the fresh-key-per-mint rule exists to prevent. Additive
    /// (`serde(default)`): pre-existing records read as unpublished and are
    /// re-bound by the log-derived converge writers in
    /// `fauna-client-atproto`/the settings machine — which is also what heals
    /// the field if an older client rewrites the record without it.
    #[serde(default)]
    pub published_for_dids: Vec<String>,
}

impl AtprotoRotationKey {
    /// The per-key half of [`AtprotoIdentityConfig::merge`], for two entries
    /// with the same `pubkey_did_key`: the published bindings union (each
    /// device converges them from the public log independently, and a dropped
    /// binding would un-burn a published key, reopening the re-mint
    /// linkability), sorted. Same `pubkey_did_key` ⇒ the same key ⇒ the same
    /// scalar and birth instant by construction, so the `created_at` fold is
    /// inert on every real pair — but "keep whichever side ran the merge" is a
    /// local preference, and a local preference is the one thing a convergent
    /// merge cannot hold. The earlier instant
    /// wins, matching the composite's sort key so the fold cannot reorder the
    /// list it feeds. The `fauna.state.atproto-identity` plane arm runs this
    /// on one `key/` row, after refusing a pair whose scalars differ.
    #[must_use]
    pub fn merge(&self, other: &Self) -> Self {
        let mut published_for_dids = self.published_for_dids.clone();
        for did in &other.published_for_dids {
            if !published_for_dids.contains(did) {
                published_for_dids.push(did.clone());
            }
        }
        published_for_dids.sort();
        Self {
            secret_scalar: self.secret_scalar.clone(),
            pubkey_did_key: self.pubkey_did_key.clone(),
            created_at: self.created_at.min(other.created_at),
            published_for_dids,
        }
    }
}

/// Client-held ATProto **login-plane** custody: the app-credential secrets the
/// user minted for external ATProto apps (`atproto-pds-full.md` § Detailed
/// design, F1). Deliberately a separate kind (`fauna.state.atproto`) from
/// [`AtprotoIdentityConfig`] (`fauna.state.atproto-identity`; identity custody — rotation keys, sole writer
/// `fauna-client-atproto::rotation_key`): different lifecycle, different
/// writer (the atproto settings machine), and different merge arm (latest-wins
/// like `mail.credentials` — a credential is re-mintable, a rotation key is
/// irrecoverable). BackupKey-sealed on the account plane; the nest
/// holds only the Argon2id PHC *verifier* and can never recover these bytes
/// (`mail-credentials.md` custody model, applied verbatim).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtprotoConfig {
    /// The minted app credentials, newest last. Source of the settings
    /// page's per-row re-reveal (the mail-credentials interaction template).
    #[serde(default)]
    pub app_credentials: Vec<AtprotoAppCredential>,
}

/// One row in [`AtprotoConfig::app_credentials`] — the client-custodied half
/// of a minted ATProto app credential ([`MailCredential`] is the shape twin;
/// field names follow the `fauna.bridges.atproto.*` wire vocabulary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtprotoAppCredential {
    /// Kebab-case identifier derived from `label` — the same id the nest's
    /// `atproto_app_credentials` row carries.
    pub credential_id: String,
    /// User-supplied human-readable name, e.g. `"Ivory"`.
    pub label: String,
    /// The generated `xxxx-xxxx-xxxx-xxxx` secret bytes (ASCII).
    /// [`SecretByteBuf`] so the decoded plaintext zeroizes on drop and encodes
    /// as a CBOR byte string — the `fauna.state.atproto` kind's byte-string
    /// rule (`config-dissolution.md` § Phases and gates → *Bounded rows*),
    /// re-cut from the integer array with no fallback under the 2026-09-24
    /// baseline reset (`serialization.md` § *Fixed-size byte arrays*).
    pub secret: SecretByteBuf,
    /// Whether the credential carries the DM-privileged scope
    /// (`atproto-pds-full.md` § Detailed design — the ecosystem's
    /// `com.atproto.appPassPrivileged` split).
    pub dm_allowed: bool,
    /// Unix seconds.
    pub created_at: u64,
}

// ── Subscription period-key sub-record ──
//
// The `fauna.state.subscriptions` entries' typed contents. Lives here so the
// at-rest dag-cbor encoding boundary stays in one place;
// the generate/retrieve/rotate lifecycle lives in
// `libs/fauna-client-subscriptions::custody` (mirroring how `MailConfig`'s MSEK
// lifecycle lives in `libs/fauna-client-mail-settings`, not here). Authority
// for the at-rest custody shape: `docs/goal/architecture/key-material-
// hierarchy.md` § Audience: an opaque set of subscriber pubkeys → *Encrypted-
// mode at-rest custody*; behavior: `docs/goal/behavior/monetization.md`
// § Pillar 1.

/// Per-actor subscription-tier period-key custody (encrypted mode). The author's
/// client generates a period key at tier-create and wraps it to each subscriber
/// in a broadcast `KeyBlob` via `mint_key_blob`; the nest never holds it.
/// `default()` is the no-tiers (or plaintext-mode) state — nothing here until
/// the author creates a tier in encrypted mode.
///
/// On the account plane this composite is the READ fold over the account's
/// `fauna.state.subscriptions` rows — one per period key, one per staged
/// removal ([`crate::subscription_rows`] owns the grammar,
/// [`SubscriptionsConfig::rows`] / [`SubscriptionsConfig::fold_row`] the two
/// directions).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionsConfig {
    /// One entry per tier the author has created in encrypted mode. Keyed by
    /// `TierPeriodKeys::tier_name` (the same tier name the `fauna.subscriptions.*`
    /// kinds carry).
    #[serde(default)]
    pub tiers: Vec<TierPeriodKeys>,
    /// In-flight subscriber-removal rotations awaiting a confirmed nest upload.
    /// A removal rotates to a **fresh** period key, which is *irrecoverable* if
    /// lost after the nest stores the re-wrapped `KeyBlob` but before the local
    /// rotation commits — so the new period is staged here (BackupKey-sealed,
    /// synced) *before* the network upload and only moved into the tier's
    /// `current` once the nest confirms. A crash between upload and commit
    /// resumes from this sentinel; merge **unions** both devices' entries so a
    /// staged key is never dropped (no-data-loss). Empty in steady state.
    /// Mirrors `MailConfig::pending_rotation`. See
    /// `fauna-client-subscriptions::orchestration`.
    #[serde(default)]
    pub pending_removals: Vec<PendingRemoval>,
}

/// The period-key history for one subscription tier. `current` is the active
/// period the mint wraps to the live roster; `prior` retains every rotated-out
/// period (most-recent first, **uncapped** — unlike `MailConfig::prior_mseks`'s
/// cap-2 — because minting an archival blob for a new subscriber wanting the
/// back-catalogue needs the full period history).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierPeriodKeys {
    /// The tier name (matches the `fauna.subscriptions.*` wire tier names).
    pub tier_name: String,
    /// The active period — the key `mint_key_blob` wraps to the current roster.
    pub current: TierPeriod,
    /// Rotated-out periods, most-recent first. Empty until the first rotation.
    #[serde(default)]
    pub prior: Vec<TierPeriod>,
}

/// One subscription period's key material.
///
/// Decodes with `deny_unknown_fields`, like [`PendingRemoval`]: both ride
/// inside the `fauna.state.subscriptions` rows (`crate::subscription_rows`),
/// a CrdtPerField kind, where a tolerant reader would strip a newer build's
/// field from the bytes it re-encodes (`config-dissolution.md` P4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierPeriod {
    /// Period version: 1 at tier-create, +1 each rotation. Correlates a stored
    /// key with the nest's `current_key_blobs.version` / an archival fetch.
    pub version: u64,
    /// The 32-byte broadcast period key — the `wrapped_key` input to
    /// `mint_key_blob`. Sealed at rest under BackupKey on the account plane
    /// (same audience + seal as `MailConfig::msek`); never
    /// leaves the user's Fauna app fleet. Held as [`SecretArray32`]
    /// (zeroize-on-drop + redacted `Debug`; wire-identical to a bare
    /// `[u8; 32]`, so the at-rest shape is unchanged) — like
    /// `ContentKeyGeneration::key` and `DeploymentSeedEntry::seed`.
    pub key: SecretArray32,
    /// Microseconds since the Unix epoch — the `rotated_at` stamped into the
    /// minted `KeyBlob`. Strictly increases across rotations so the nest's
    /// `rotated_at`-monotonicity check (`fauna.subscriptions.stale_rotation`)
    /// is satisfiable.
    pub rotated_at: u64,
    /// **The identity that minted this key** — the period's own era stamp, and
    /// the only thing in the system that answers *"is this key one a retired
    /// identity's seed holder read?"*.
    ///
    /// Written from the account's current actor id (`fauna.state.succession-ledger`) by whichever custody transition
    /// minted the key (`fauna_client_subscriptions::custody`), never rewritten
    /// afterwards: a republish is not a mint.
    ///
    /// ⚠ **It exists because `KeyBlob.author` cannot serve.** That field records
    /// who last *published* a blob, and an ordinary (auto-)approve republishes
    /// the tier's **current** key under the caller's authorship — so after a
    /// succession one approve stamped a blob with the successor's id while it
    /// still wrapped the predecessor's key, and the post-succession rotation
    /// read that as "already re-keyed" and never ran.
    /// This stamp moves only when the KEY moves, which is the question being
    /// asked. It is data *about the key*, like `version` and `rotated_at`
    /// beside it — not a record that a pass ran, so the aftermath leg keeps its
    /// no-progress-state-at-rest property.
    ///
    /// `None` reads as "not minted by me" for every consumer — the safe direction, costing one
    /// needless rotation rather than missing an owed one. `Option` rather than
    /// a zero-id default so "we do not know" stays distinguishable from a real
    /// identity, and `skip_serializing_if` so an unset value adds no
    /// key to the at-rest bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minted_by: Option<ActorId>,
}

/// Test-only builder: a single-period [`TierPeriodKeys`] with no `prior`
/// history, `key` filled with one repeated byte. Shared home for what
/// `fauna_client_config::store`'s unit tests and `bins/fauna-nest`'s
/// cross-device config-merge conformance test each hand-copied byte-for-byte
/// (round 92 lift). Deliberately NOT named the shorter `tier`: this module is
/// glob-imported (`use fauna_core::data::*;`) by several `bins/fauna-nest`
/// test modules that also bind a local `tier: &str`/`String` — a bare `tier`
/// free fn collided with one of those bindings the instant `test-helpers`
/// pulled it into scope (round 92, caught by the verification build before
/// landing).
#[cfg(any(test, feature = "test-helpers"))]
pub fn fixture_tier_period_keys(name: &str, version: u64, key: u8) -> TierPeriodKeys {
    TierPeriodKeys {
        tier_name: name.into(),
        current: TierPeriod {
            version,
            key: [key; 32].into(),
            rotated_at: version * 1000,
            minted_by: None,
        },
        prior: vec![],
    }
}

/// A subscriber-removal rotation that has been *staged* (its fresh period key
/// generated + persisted) but not yet committed into the tier's `current` —
/// the crash-recovery sentinel for [`SubscriptionsConfig::pending_removals`].
/// The `new_period` carries the irrecoverable fresh key; on a confirmed nest
/// upload the orchestration commits it via `custody::commit_period` and drops
/// the sentinel. Keyed by `(tier_name, subscriber_id)` for resume + merge dedup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingRemoval {
    /// The tier the subscriber is being removed from.
    pub tier_name: String,
    /// The subscriber being removed (the `KeyBlob` re-wraps to the roster
    /// *minus* this id).
    pub subscriber_id: ActorId,
    /// The freshly-generated post-removal period — `current.version + 1`, a
    /// fresh 32-byte key, `rotated_at` strictly above the prior. The only piece
    /// here that is irrecoverable if lost, which is why this sentinel exists.
    pub new_period: TierPeriod,
}

impl TierPeriod {
    /// Deterministic ordering key for picking the surviving `current` on a
    /// concurrent-rotation conflict: newer version first, then later
    /// `rotated_at`, then larger key bytes as a final total-order tiebreaker.
    fn order(&self) -> (u64, u64, [u8; 32]) {
        // The transient key copy exists only for the comparison (`Ord` needs an
        // owned tuple) — same exposure class as feeding the key to a crypto op.
        (self.version, self.rotated_at, self.key.to_array())
    }

    /// Whether `actor` is the identity that minted this key.
    ///
    /// **`false` when the minter is unknown**, which is the safe direction: an
    /// unknown minter costs one needless rotation, a wrongly-assumed one
    /// leaves a compromised key live. See [`Self::minted_by`] for why the
    /// question cannot be answered from `KeyBlob.author`.
    pub fn was_minted_by(&self, actor: &crate::identity::ActorId) -> bool {
        self.minted_by.as_ref() == Some(actor)
    }
}

impl SubscriptionsConfig {
    /// Merge two devices' period-key custody **without losing any key**.
    ///
    /// Period keys are *irrecoverable* (no-user-data-loss invariant), so unlike
    /// the whole-record latest-wins merge used for `mail`/`backup`/`dns` this
    /// unions **per tier**: a tier on only one side is kept; a tier on both
    /// keeps the deterministically-higher `current` (by `version`, then
    /// `rotated_at`, then key bytes) and retains *every* distinct period from
    /// both sides — the losing `current` plus both `prior` lists — in `prior`,
    /// most-recent first, deduplicated by identical `(version, key,
    /// rotated_at)`. Deterministic and commutative, so two devices converge on
    /// the same result regardless of merge order. Called by the
    /// `fauna.state.subscriptions` merge.
    ///
    /// `pending_removals` (the crash-recovery sentinels) are **unioned** —
    /// every distinct staged removal from both sides survives, because each
    /// carries an irrecoverable fresh period key (no-data-loss). Exact
    /// duplicates collapse; two stagings of the same `(tier, subscriber)` with
    /// different fresh keys are both kept (resume completes one and treats the
    /// rest as already-applied, folding their keys into `prior`).
    pub fn merge(&self, other: &Self) -> Self {
        use std::collections::BTreeMap;
        let mut by_name: BTreeMap<&str, TierPeriodKeys> = BTreeMap::new();
        for t in self.tiers.iter().chain(other.tiers.iter()) {
            match by_name.get_mut(t.tier_name.as_str()) {
                None => {
                    by_name.insert(t.tier_name.as_str(), t.clone());
                }
                Some(acc) => *acc = acc.merge(t),
            }
        }
        let mut pending_removals = self.pending_removals.clone();
        for p in &other.pending_removals {
            if !pending_removals.contains(p) {
                pending_removals.push(p.clone());
            }
        }
        // Canonical order, not arrival order — the same law every
        // account-plane merge takes. Two replicas holding the same set of sentinels must encode
        // it identically, or each reads the other's value as new state and
        // re-publishes forever. The key covers the record's whole canonical
        // encoding — exactly what the dedup above compares — so it is total by
        // construction and stays total as the record grows fields.
        pending_removals.sort_by_cached_key(crate::encoding::canonical_tiebreak_key);
        Self {
            tiers: by_name.into_values().collect(),
            pending_removals,
        }
    }
}

impl TierPeriodKeys {
    /// Merge two `TierPeriodKeys` for the same tier without dropping any period
    /// key: the deterministically-higher `current` (by `version`, then
    /// `rotated_at`, then key bytes) wins, and every distinct period from both
    /// sides (the losing `current` plus both `prior` lists) is retained in
    /// `prior`, most-recent first. See [`SubscriptionsConfig::merge`]; the
    /// idempotent single-period commit `custody::commit_period` also builds on
    /// this.
    pub fn merge(&self, other: &Self) -> Self {
        let (current, loser) = if self.current.order() >= other.current.order() {
            (self.current.clone(), other.current.clone())
        } else {
            (other.current.clone(), self.current.clone())
        };
        let mut prior: Vec<TierPeriod> = Vec::new();
        for p in self
            .prior
            .iter()
            .chain(other.prior.iter())
            .chain(std::iter::once(&loser))
        {
            if *p != current && !prior.contains(p) {
                prior.push(p.clone());
            }
        }
        // Most-recent first (version desc, then rotated_at desc) — same
        // convention `custody::rotate` maintains.
        prior.sort_by_key(|x| std::cmp::Reverse(x.order()));
        TierPeriodKeys {
            tier_name: self.tier_name.clone(),
            current,
            prior,
        }
    }
}

// ── Shared folder content-key custody sub-record ──
//
// The `fauna.state.folder-keys` entries' typed contents — the at-rest custody for the
// **M2** content-key mechanism (the direct analog of `SubscriptionsConfig` above,
// differing only in the distribution channel: an MLS group, not per-subscriber
// `KeyBlob` wraps). The pure generation-history primitive
// ([`crate::folder_keys::FolderContentKeys`]) + transitions live in
// `folder_keys.rs`; this wraps it per-set for the plane seal + CRDT merge.
// Authority: `docs/goal/architecture/mls-group-key-material.md` § M2 content-key
// mechanism → *Custody (per holder)*.

/// Per-actor shared-folder content-key custody (encrypted + plaintext alike —
/// the content key is the chunk-crypto root regardless of nest storage mode).
/// The **owner** (the binder, sole writer in Slices 2–3) holds the authoritative
/// [`FolderContentKeys`] per shared set; a **member** holds a received copy.
/// `default()` is the no-shared-sets state. BackupKey-sealed on the account plane (`fauna.state.folder-keys`), fleet-only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldersConfig {
    /// One entry per set this holder owns (from creation) or holds a received
    /// copy of, identified by its set nonce ([`FolderKeyCustody`]). Content-keyed
    /// consumers still resolve an entry by its 32-byte `channel_id` (the derived
    /// `ChannelId::from_group_id(mls_group_id)`, matching the nest content-key
    /// envelope's storage key, or the serve pseudo-channel).
    #[serde(default)]
    pub sets: Vec<FolderKeyCustody>,
    /// In-flight member-removal rotations awaiting a confirmed envelope publish.
    /// A removal rotates to a **fresh** content key, *irrecoverable* if lost after
    /// the nest stores the re-sealed envelope but before the local rotation
    /// commits — so the new generation is staged here (BackupKey-sealed, synced)
    /// *before* the network publish and only moved into the set's `current` once
    /// the nest confirms. A crash between publish and commit resumes from this
    /// sentinel; merge **unions** both devices' entries so a staged key is never
    /// dropped (no-data-loss). Mirrors [`SubscriptionsConfig::pending_removals`].
    /// Empty in steady state.
    #[serde(default)]
    pub pending_removals: Vec<FolderPendingRemoval>,
    // (Retired 2026-09-25: a `pending_reseals` sentinel list once sat here — the
    // M2 pre-bind re-seal's "this set still owes a pass" marker. The pass is now
    // the sync agent's ungated, idempotent `SyncEngine::reseal_pending_under_current`
    // on every engine start, terminating on the local row's `content_key_version`
    // stamp, so no reader was left; the field carried no key material and nothing
    // a user could not recreate. A blob written by an older client still decodes —
    // this struct ignores unknown fields — and the entry is simply dropped on the
    // next rewrite. `mls-group-key-material.md` § M2 *Pre-bind re-seal migration*.)
    /// The **foreign** (cross-nest) shared sets this holder is a member of —
    /// one [`ForeignFolder`] per set whose home is another nest, written at
    /// accept time (the member's own nest holds no row for these; Phase 2
    /// client read-side, `ui/folders.md` § Sharing). Merge **unions** by
    /// `channel_id` (see [`Self::merge`]). Wire-additive: absent on configs
    /// written before Phase 2.
    #[serde(default)]
    pub foreign_sets: Vec<ForeignFolder>,
}

/// One set's custody entry — the owner's from the set's creation, a member's
/// from its first envelope ingest (`mls-group-key-material.md` § M2 →
/// *Writer-signed change records* → *Custody shape of the set nonce*, (a)).
///
/// An owner-only set is an entry with a nonce, a name and nothing else; the
/// serve-enable or bind that mints the genesis generation fills `channel_id`
/// and `keys` in place. The entry's identity is [`Self::set_nonce`]
/// ([`FoldersConfig::merge`] unions by it); an entry without one (a first `merge_received_keys`
/// ingest or a `record_new_set` write, repaired by the owner's custody reconcile) keys on `channel_id`.
///
/// `Default` is derived so fixtures can be written struct-update style.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderKeyCustody {
    /// The set's 32-byte custody identity: the derived `ChannelId`
    /// (`ChannelId::from_group_id`) for a **shared** set, or the serve
    /// pseudo-channel ([`crate::folder_keys::serve_custody_channel_id`]) for a
    /// **WebDAV-served set with no MLS group** — same shape, domain-separated
    /// derivations (`webdav-server.md` § Key model custody note). Sharing a
    /// served set moves the entry pseudo→real at bind, in place. `None` until
    /// the set is served or bound.
    #[serde(default)]
    #[serde(with = "serde_bytes")]
    pub channel_id: Option<[u8; 32]>,
    /// The content-key generation history for this set; `None` until the
    /// serve-enable or bind that mints the genesis generation.
    #[serde(default)]
    pub keys: Option<crate::folder_keys::FolderContentKeys>,
    /// The client-minted 32-byte set binding every writer-signed change record
    /// of the set covers (`fauna_protocol::sync_writer_sig`). Minted at create
    /// by the one create helper; a member's copy is replaced by the owner's
    /// envelope. `None` on an entry first written by `merge_received_keys` or `record_new_set`.
    #[serde(default)]
    #[serde(with = "serde_bytes")]
    pub set_nonce: Option<[u8; 32]>,
    /// The owner's own address for the set, written once at create (no rename
    /// kind exists). A member's received copy carries none.
    #[serde(default)]
    pub name: Option<String>,
    /// Micros, the creating device's clock at mint (the unit `rotated_at`
    /// uses) — the same-name pick's first key. Write-once.
    #[serde(default)]
    pub created_at: u64,
    /// The delete tombstone (micros): a retired entry keeps its nonce and its
    /// keys, never selects as the set's live nonce, and stands as a delete
    /// intent the owner's reconcile re-drives. Entries are never removed.
    #[serde(default)]
    pub retired_at: Option<u64>,
    /// The lift of a retirement (micros) — written with exactly the stamp a
    /// delete retired the entry at, when the nest *answered* that delete with a
    /// refusal (ruling (e)'s one sanctioned un-retire, `mls-group-key-material.md`
    /// § M2 → *Custody shape of the set nonce*, ruling (l)(v)). The entry is live
    /// again while `lifted_at >= retired_at`; a later delete's later stamp
    /// retires it anew. Monotone, so the lift lands on a merge-only store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifted_at: Option<u64>,
    /// The identity whose device minted [`Self::set_nonce`]
    /// (`writer-signed-change-records.md` ruling (11)(a)): the creating
    /// identity at create, the current identity at a re-mint, whatever the
    /// owner's envelope names on a member's received copy. `None` where none
    /// was recorded — it reads as the earliest identity in the owner's chain,
    /// and the owner's reconcile re-mints such an entry (an owned set's live
    /// nonce is one the current identity minted). Joins `None ∨ Some → Some`,
    /// two `Some`s to the smaller — write-once in effect, since one device
    /// mints one entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minted_by: Option<crate::identity::ActorId>,
    /// The nonce this entry was re-minted over (ruling (11)(a)) — the
    /// **lineage** edge that tells a re-mint's retirement from a delete's: a
    /// reader's retired list is the connected component of these edges around
    /// the live entry, never a timestamp filter. `None` on a created entry.
    /// Joins as [`Self::minted_by`] does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "serde_bytes")]
    pub replaces: Option<[u8; 32]>,
    /// The winner's nonce, stamped when custody (c)'s same-name pick retired
    /// this entry as a duplicate (ruling (11)(b)) — what places a create
    /// race's loser in the lineage of this incarnation, where a timestamp
    /// across two devices' clocks could not. Joins as [`Self::minted_by`]
    /// does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "serde_bytes")]
    pub retired_by_pick: Option<[u8; 32]>,
    /// The owner's latest WebDAV serve-on gesture (micros, the flipping
    /// device's clock) — one half of the serve window
    /// (`writer-signed-change-records.md` ruling (7)(b)(ii) rule (1)). Written
    /// by the serve-on gesture and by nothing else, never from the nest's
    /// flag; a member's copy carries what the owner's envelope last said.
    /// Joins `None ∨ Some → Some`, two `Some`s → the later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_at: Option<u64>,
    /// The owner's latest serve-off (micros) — the other half of the serve
    /// window: the gesture, or the launch pass's unserve arm. Joins as
    /// [`Self::served_at`] does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unserved_at: Option<u64>,
}

impl FolderKeyCustody {
    /// Whether the owner serves this set over WebDAV now — THE served
    /// predicate every client judgement reads (ruling (7)(b)(ii) rule (1)):
    /// the entry is live and its serve-on stamp stands strictly above its
    /// serve-off. A tie reads NOT served (the safe side), and a tombstone of a
    /// set deleted while served never reads served.
    #[must_use]
    pub fn is_served(&self) -> bool {
        self.is_live() && crate::folder_keys::serve_window_open(self.served_at, self.unserved_at)
    }

    /// Stamp the owner's serve-on at `now_micros` — strictly above the
    /// entry's serve-off, so a clock that stepped back between two gestures
    /// still orders them; never below a serve-on it already carries.
    pub fn serve_on(&mut self, now_micros: u64) {
        let floor = self.unserved_at.map_or(0, |off| off.saturating_add(1));
        self.served_at = Some(now_micros.max(floor).max(self.served_at.unwrap_or(0)));
    }

    /// Stamp the owner's serve-off at `now_micros` — strictly above the
    /// entry's serve-on, the mirror of [`Self::serve_on`].
    pub fn serve_off(&mut self, now_micros: u64) {
        let floor = self.served_at.map_or(0, |on| on.saturating_add(1));
        self.unserved_at = Some(now_micros.max(floor).max(self.unserved_at.unwrap_or(0)));
    }

    /// Whether this entry is live (not retired by a delete or a leave).
    #[must_use]
    pub fn is_live(&self) -> bool {
        match self.retired_at {
            None => true,
            Some(retired) => self.lifted_at.is_some_and(|lifted| lifted >= retired),
        }
    }

    /// Retire this entry (a delete or a leave) at `now_micros` — stamped
    /// strictly above any lift it carries, so a clock that stepped back since
    /// a refused delete's lift still leaves the new tombstone standing
    /// (ruling (l)(v)).
    pub fn retire(&mut self, now_micros: u64) {
        let floor = self.lifted_at.map_or(0, |lifted| lifted.saturating_add(1));
        self.retired_at = Some(now_micros.max(floor));
    }

    /// Join two copies of ONE entry (same nonce, or same `channel_id` for a
    /// nonce-less entry) — the per-field semilattice rule (b) orders:
    /// `keys` by the generation CRDT; `channel_id` `None ∨ Some → Some`, and two
    /// differing `Some`s take the one that is not the entry's own serve
    /// pseudo-channel (bound dominates served), else the smaller; `retired_at`
    /// and `lifted_at` each `None ∨ Some → Some`, two `Some`s → the later (a
    /// lift covers only the stamp it names, so a later delete's tombstone
    /// outlives an earlier lift — ruling (l)(v)); `name`, `created_at` and
    /// `set_nonce` are write-once, so a disagreement (never produced) resolves
    /// to the smaller; `minted_by`, `replaces` and `retired_by_pick` (ruling
    /// (11)) `None ∨ Some → Some`, two `Some`s → the smaller; `served_at` and
    /// `unserved_at` (ruling (7)(b)(ii)) each to the later, as the retirement
    /// pair. Deterministic, commutative, idempotent.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        fn min_opt<T: Ord + Clone>(a: &Option<T>, b: &Option<T>) -> Option<T> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b).clone()),
                (a, b) => a.clone().or_else(|| b.clone()),
            }
        }
        let name = min_opt(&self.name, &other.name);
        let channel_id = match (self.channel_id, other.channel_id) {
            (Some(a), Some(b)) if a != b => {
                let pseudo = name
                    .as_deref()
                    .map(crate::folder_keys::serve_custody_channel_id);
                match pseudo {
                    Some(p) if a == p => Some(b),
                    Some(p) if b == p => Some(a),
                    _ => Some(a.min(b)),
                }
            }
            (a, b) => a.or(b),
        };
        let keys = match (&self.keys, &other.keys) {
            (Some(a), Some(b)) => Some(a.merge(b)),
            (a, b) => a.clone().or_else(|| b.clone()),
        };
        let created_at = match (self.created_at, other.created_at) {
            (0, b) => b,
            (a, 0) => a,
            (a, b) => a.min(b),
        };
        Self {
            channel_id,
            keys,
            set_nonce: min_opt(&self.set_nonce, &other.set_nonce),
            name,
            created_at,
            retired_at: self.retired_at.max(other.retired_at),
            lifted_at: self.lifted_at.max(other.lifted_at),
            minted_by: min_opt(&self.minted_by.map(|a| a.0), &other.minted_by.map(|a| a.0))
                .map(crate::identity::ActorId),
            replaces: min_opt(&self.replaces, &other.replaces),
            retired_by_pick: min_opt(&self.retired_by_pick, &other.retired_by_pick),
            served_at: self.served_at.max(other.served_at),
            unserved_at: self.unserved_at.max(other.unserved_at),
        }
    }
}

/// A member-removal rotation that has been *staged* (its fresh content key
/// generated + persisted) but not yet committed into the set's `current` — the
/// crash-recovery sentinel for [`FoldersConfig::pending_removals`]. The
/// `new_generation` carries the irrecoverable fresh key; on a confirmed envelope
/// publish the orchestration commits it and drops the sentinel. Keyed by
/// `(channel_id, removed_member)` for resume + merge dedup. The analog of
/// [`PendingRemoval`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderPendingRemoval {
    /// The shared set the member is being removed from (its derived `ChannelId`).
    #[serde(with = "serde_bytes")]
    pub channel_id: [u8; 32],
    /// The set's owner-local name — the address the nest content-key/evict kinds
    /// take (`fauna.folders.{content_key.put,members.evict}` look the set up by
    /// `(name, owner)`). Custody is keyed by the cross-device `channel_id`, but a
    /// crash-resumed removal still has to re-drive the *nest* operations, which
    /// address by name, so the sentinel carries it. `#[serde(default)]` is the
    /// additive-field discipline (every writer sets it).
    #[serde(default)]
    pub name: String,
    /// The member being removed (the MLS Remove + the re-key exclude them).
    pub removed_member: ActorId,
    /// The freshly-generated post-removal generation — `current.version + 1`, a
    /// fresh 32-byte key, `rotated_at` strictly above the prior. The only piece
    /// here that is irrecoverable if lost, which is why this sentinel exists.
    pub new_generation: crate::folder_keys::ContentKeyGeneration,
    /// The owner's MLS Remove **commit bytes** for this staged removal — the
    /// remaining members' epoch-advance liveness (5d(d)). Persisted into the
    /// sentinel BEFORE the channel send, so a crash-resumed drive re-distributes
    /// the *same* bytes (MLS cannot re-produce a commit for an already-merged
    /// transition; a duplicate channel append is harmless — members quiet-skip a
    /// past-epoch commit). `None` until the commit is produced, and on sentinels
    /// staged before this field existed (`#[serde(default)]` — additive at-rest).
    #[serde(default, skip_serializing_if = "Option::is_none", with = "serde_bytes")]
    pub commit: Option<Vec<u8>>,
    /// Whether an **engaged** gated attempt (one that can distribute a Remove
    /// commit inside the device-owned-epoch rebase loop, which records no
    /// `commit` bytes here) has ever been started for this sentinel. Stamped
    /// `true` — durably, BEFORE the gate runs — by the drive's gated leg;
    /// staged as `false` at creation. The fork-safety discriminator when
    /// the gate is wired but not engaged: a byte-less sentinel with
    /// `false` is **provably undistributed** (only an engaged gated
    /// attempt can distribute without recording bytes, and none ever started),
    /// so the ungated rebuild is safe — without it, a byte-less sentinel on a
    /// nest whose `fauna.mls` plane never engages would defer forever and the
    /// removal could never complete.
    ///
    /// Merge is the join `false < true`: an attempt stamped on any device
    /// survives. (The `None` "staged before this field existed" sentinel went
    /// with the compat-remnant sweep — every writer stamps it explicitly.)
    #[serde(default)]
    pub gated_attempted: bool,
}

/// One **foreign** (cross-nest) shared folder this holder is a member of —
/// the member's own durable record of a set whose home is ANOTHER nest
/// (`docs/goal/ui/folders.md` § Sharing; Phase 2 client read-side). The
/// member's own nest holds **no** `folders` row for such a set (the roster
/// row lives in the home nest's `channel_foreign_members`), so everything the
/// client needs to list and read it must live here, written at accept time
/// from the staged Welcome envelope:
///
/// - identity: the derived [`Self::channel_id`] (matching
///   [`FolderKeyCustody::channel_id`] — custody joins on it) + the raw
///   [`Self::mls_group_id`] (what the engine and the Media key-resolver key on);
/// - routing: [`Self::home_nest_url`] — the base every read relays to
///   (`changes.list` / `content_key.get` with the additive `nest_url` +
///   `channel_id`, and the direct byte GETs);
/// - display: [`Self::set_name`], resolved by the HOME nest from its claimed
///   row and carried on the Welcome relay wire. Display-only text from the
///   sharer's nest — never a lookup key, never an authorization input.
/// - **advisory access**: [`Self::access`] — the grant the home nest says this
///   member holds, so the client knows whether to *offer* a folder binding.
///   Same trust posture as [`Self::set_name`], and stated once in the field's
///   own doc: **never an authorization input.**
///
/// A same-nest share never records one of these (its row resolves via
/// `fauna.folders.list` `include_shared_with_me`). A leave tombstones the
/// record alongside custody's retire (`forget_set`'s foreign twin): it is
/// never removed ([`Self::left_at`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignFolder {
    /// The derived 32-byte `ChannelId` (`ChannelId::from_group_id`) — the
    /// stable identity custody + the federated read kinds are keyed by.
    #[serde(with = "serde_bytes")]
    pub channel_id: [u8; 32],
    /// The raw MLS group id (as the engine and `FolderSummary.mls_group_id`
    /// carry it) — recovered from the engine at join time; what the Media
    /// key-resolver hands back so a foreign set's reads open like any shared
    /// set's.
    #[serde(with = "serde_bytes")]
    pub mls_group_id: Vec<u8>,
    /// The set's home-nest base URL (the Welcome envelope's `nest_url`) — where
    /// every read for this set relays / fetches. A re-accepted share overwrites
    /// it (mirrors the home nest's own last-writer-wins re-bind, S1).
    pub home_nest_url: String,
    /// The home nest's **deployment identity** (`nest_actor_id`,
    /// hex-encoded 32-byte Ed25519 pubkey — the value the channel binding signs
    /// and `fauna.nest.info` advertises as `nest_id`), as the home nest stamped
    /// it beside [`Self::home_nest_url`] on the Welcome relay and refreshed on
    /// every `caller_access` federated read reply. The member holds no account
    /// on the home nest, so its direct byte-plane HTTPS dial has no other trust
    /// root: the agent graduates an SPKI pin against `IdentityRoot::PreResolved`
    /// of this actor id via the pre-identity `fauna.auth.nest_handshake`
    /// (`../architecture/security.md` § Transport trust, the federation-granted
    /// Axis-2 row). `None` = the home nest's best-effort lookup missed, or it relayed no
    /// actor id (a non-folder relay or non-conforming peer) → the byte plane
    /// falls back to today's `RequireWebPki` (never weaker, WebPKI homes
    /// unaffected). Like [`Self::home_nest_url`] it is own-nest-mediated relay
    /// metadata, not inviter-authenticated E2E material (the strictly-separable
    /// future hardening moves both inside the MLS Welcome payload).
    #[serde(default)]
    pub home_nest_actor_id: Option<String>,
    /// The set's name as the home nest resolved it from its claimed row at
    /// share time. `None` when the share was unstamped or the home nest's
    /// claimed-row resolve missed (a name-less record — clients render their
    /// unknown-set fallback label).
    #[serde(default)]
    pub set_name: Option<String>,
    /// The member's access grant on this set (`"reader"` / `"writer"`), as the
    /// **home** nest resolved it from its own `folder_member_access` row —
    /// seeded on the Welcome relay and refreshed by the `caller_access` stamp
    /// every federated read reply carries (`../architecture/federation.md`
    /// § Cross-nest → *Recipient-side access discovery*).
    ///
    /// **Advisory-for-UI ONLY — never an authorization input** (the cross-nest
    /// mirror of the same-nest D5 share-wire invariant). It decides whether this
    /// client *offers* a folder binding; the sole enforcement is the home nest's
    /// own `require_foreign_writer` gate on the write kinds, and a bind is
    /// verified authoritatively by an eager `write_token.get` at the gesture. So
    /// a stale or even lying value costs at most a bind that fails loudly — never
    /// access. Never derive quota, billing, or identity from it.
    ///
    /// `None` = unknown ⇒ **treated as reader** (fail-safe): a record whose
    /// share carried no access field (e.g. from a non-conforming peer)
    /// deserializes to `None` and stays unbindable.
    #[serde(default)]
    pub access: Option<String>,
    /// The set's owner-stamped content-key floor (the current generation the
    /// home nest keeps beside the set's envelope — `mls-group-key-material.md`
    /// § M2 → *Multi-writer*), as the **home** nest stamped it on the federated
    /// content-key read reply (`ContentKeyGetReply::content_key_floor`). A
    /// cross-nest member has no `folders` row on its own nest, so this record
    /// is where its engine reads the floor: the pre-seal hold arms from it and
    /// a floor move is a basis change at the host's custody edge
    /// (`../behavior/on-demand-files.md` § Shared sets on a capability host →
    /// *One mechanism*, question 2). Last-writer-wins from every stamp; `None`
    /// = the home nest holds no floor, or the record was
    /// written at share-accept (the Welcome carries none) → nothing armed, and
    /// the home nest's `stale_content_key` refusal alone enforces the floor,
    /// exactly as before the read carried it. Advisory to the HOLD only: the
    /// home nest's refusal stays the enforcement.
    #[serde(default)]
    pub content_key_floor: Option<u64>,
    /// The folder's content residency as the **home** nest last stamped it
    /// (`../behavior/file-sync.md` § Relay serving → *A member on another
    /// nest*, step (1)): seeded on the Welcome relay and refreshed by the
    /// `residency` stamp every federated folder read reply carries beside
    /// `caller_access`. The member's engine arms from it what a same-nest seat
    /// arms from its row — the upload skip, the holder-keeps gate, the upload
    /// door's refusal.
    ///
    /// **Three readings, never two:** `None` = no home nest has stated it yet
    /// — read as *unknown*, never as *full*, and each consumer takes its own
    /// safe side.
    /// Carried with its own stamp, so a flip lands on a merge-only store
    /// whichever advisory field moved last ([`ForeignResidency`]).
    #[serde(default)]
    pub residency: Option<ForeignResidency>,
    /// The set's owner's **handle**, as the home nest stamped it and this
    /// member's own nest verified it (`../architecture/federation.md`
    /// § Cross-nest shared folders + channel append → *The cross-nest owner
    /// label*): written at accept from the Welcome's
    /// `shared_by_handle`/`shared_by_domain` pair and refreshed from every
    /// federated content-key read reply's `owner_handle`/`owner_domain`.
    /// Paired with [`Self::owner_domain`] — the two are written together, and
    /// joined only at display (`fauna_core::format::qualified_handle` →
    /// `docs (alice@example.com)`). `None` = no verified pair yet → the
    /// display falls back to the home nest's host. The fifth latest-wins
    /// advisory pair under [`Self::updated_at`]: an older device's stamp-less
    /// newer write clears it and the next refresh restores it — never a wrong
    /// name. **Display-only — never a lookup key or an authorization input.**
    #[serde(default)]
    pub owner_handle: Option<String>,
    /// The handle domain [`Self::owner_handle`] is joined with at display.
    #[serde(default)]
    pub owner_domain: Option<String>,
    /// Micros, the latest accept of this share on any device — max-joined.
    /// With [`Self::left_at`] it is the record's liveness
    /// ([`Self::is_live`]): a re-accept of the same channel after a leave
    /// stamps it past the leave and revives the record
    /// (`mls-group-key-material.md` § M2 → *Custody shape of the set nonce*,
    /// ruling (l)(iii)).
    #[serde(default)]
    pub accepted_at: u64,
    /// Micros, the latest leave of this share on any device — max-joined, the
    /// record's tombstone: a leave never removes the record, because a record
    /// removed on one device returns from any other (ruling (l)(iii)).
    #[serde(default)]
    pub left_at: Option<u64>,
    /// Micros, the stamp of the five latest-wins advisory fields (`access`,
    /// `home_nest_url`, `home_nest_actor_id`, `mls_group_id`, and the
    /// [`Self::owner_handle`]/[`Self::owner_domain`] pair): every accept and
    /// every refresh that changes
    /// one of them stamps it strictly past the held value, and
    /// [`FoldersConfig::merge`] takes the five from the higher stamp whole —
    /// so a promotion or a re-bind lands on a merge-only store (ruling
    /// (l)(iv)).
    #[serde(default)]
    pub updated_at: u64,
}

/// A foreign set's residency as its home nest stated it, with the stamp the
/// fold orders it by ([`ForeignFolder::residency`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignResidency {
    /// `true` metadata-only, `false` full.
    pub metadata_only: bool,
    /// Micros, when a device last wrote a changed reading — every write that
    /// changes it stamps strictly past the held value.
    pub stamped_at: u64,
}

impl ForeignResidency {
    /// Parse the wire stamp — `"metadata_only"` or `"full"`; anything else
    /// (absent, empty, a value this build does not know) states nothing, so
    /// an unparseable stamp neither stops bytes resting nor frees a body.
    #[must_use]
    pub fn parse_stamp(stamp: Option<&str>) -> Option<bool> {
        match stamp {
            Some("metadata_only") => Some(true),
            Some("full") => Some(false),
            _ => None,
        }
    }

    /// The fold of two devices' readings: the later stamp wins; on a tie
    /// *metadata-only* wins, the side on which the holder-keeps gate keeps a
    /// body — so a disagreement never lets that gate read *full*. A stated
    /// reading beats an unstated one. Deterministic, commutative, idempotent.
    #[must_use]
    pub fn join(a: Option<Self>, b: Option<Self>) -> Option<Self> {
        match (a, b) {
            (Some(a), Some(b)) => Some(match a.stamped_at.cmp(&b.stamped_at) {
                std::cmp::Ordering::Greater => a,
                std::cmp::Ordering::Less => b,
                std::cmp::Ordering::Equal => Self {
                    metadata_only: a.metadata_only || b.metadata_only,
                    stamped_at: a.stamped_at,
                },
            }),
            (a, b) => a.or(b),
        }
    }
}

impl ForeignFolder {
    /// The residency reading this record states: `Some(true)` metadata-only,
    /// `Some(false)` full, `None` unknown (no home nest has stamped it).
    #[must_use]
    pub fn metadata_only_residency(&self) -> Option<bool> {
        self.residency.map(|r| r.metadata_only)
    }

    /// Whether this membership stands: never left, or accepted again after
    /// the latest leave (ruling (l)(iii)). Every reader of foreign records
    /// reads the live ones.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.left_at.is_none_or(|left| left < self.accepted_at)
    }
}

impl FoldersConfig {
    /// Merge two devices' shared-folder custody **without losing any content
    /// key** — the `fauna.state.folder-keys` CRDT merge. Like
    /// [`SubscriptionsConfig::merge`] (and unlike whole-record latest-wins for
    /// `mail`/`backup`/`dns`) this unions **per set**, by the entry's nonce: a
    /// set on only one side is kept; a set on both joins field-wise
    /// ([`FolderKeyCustody::join`]) — the deterministically-higher `current`,
    /// every distinct generation from both sides ([`FolderContentKeys::merge`]),
    /// bound over served, the earlier tombstone. `pending_removals` are **unioned** — every
    /// distinct staged removal survives (each carries an irrecoverable fresh key).
    /// Deterministic + commutative → two devices converge regardless of order.
    pub fn merge(&self, other: &Self) -> Self {
        use std::collections::BTreeMap;
        // `sets` union by the entry's identity — its nonce, else its
        // `channel_id` (a nonce-less entry), else its whole canonical encoding
        // (an entry with neither, never produced) — and join field-wise
        // ([`FolderKeyCustody::join`]). The map key orders the output
        // canonically.
        let mut by_identity: BTreeMap<(u8, [u8; 32]), FolderKeyCustody> = BTreeMap::new();
        for s in self.sets.iter().chain(other.sets.iter()) {
            let identity = match (s.set_nonce, s.channel_id) {
                (Some(nonce), _) => (0, nonce),
                (None, Some(channel)) => (1, channel),
                (None, None) => (2, crate::encoding::canonical_tiebreak_key(s)),
            };
            match by_identity.get_mut(&identity) {
                None => {
                    by_identity.insert(identity, s.clone());
                }
                Some(acc) => *acc = acc.join(s),
            }
        }
        // `pending_removals` union by the staging identity `(channel_id, name,
        // removed_member, new_generation)` — every distinct staged removal
        // survives (each carries an irrecoverable fresh key). The `commit` bytes
        // fold as a semilattice (`None` ∨ `Some` → `Some`; two differing `Some`s
        // → the lexicographically smaller), so a device that enriched its
        // sentinel with the produced Remove commit never duplicates the
        // pre-enrichment twin a peer replica still holds — deterministic,
        // commutative, idempotent.
        let mut pending_removals: Vec<FolderPendingRemoval> = Vec::new();
        for p in self
            .pending_removals
            .iter()
            .chain(other.pending_removals.iter())
        {
            match pending_removals.iter_mut().find(|q| {
                q.channel_id == p.channel_id
                    && q.name == p.name
                    && q.removed_member == p.removed_member
                    && q.new_generation == p.new_generation
            }) {
                None => pending_removals.push(p.clone()),
                Some(q) => {
                    q.commit = match (q.commit.take(), p.commit.clone()) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (a, b) => a.or(b),
                    };
                    // `gated_attempted` joins on `false < true`: an
                    // engaged-attempt stamp from any device survives.
                    q.gated_attempted |= p.gated_attempted;
                }
            }
        }
        // `foreign_sets` union **per set** by `channel_id` (mirroring the `sets`
        // fold): a record on only one side is kept; a record on both folds
        // per-field. `set_name` joins `None` ∨ `Some` → `Some` (two differing
        // `Some`s → the lexicographically smaller, matching the
        // `pending_removals.commit` fold). The five advisory fields the accept
        // and the read-reply refresh OVERWRITE — `access`, `home_nest_url`,
        // `home_nest_actor_id`, `mls_group_id`, and the owner label pair
        // (`owner_handle` + `owner_domain`, one unit) — are
        // latest-wins, whole, on the record's `updated_at` stamp: a promotion or
        // a re-bind must land even on a store that joins every write with the
        // stored row (`mls-group-key-material.md` § M2 → *Custody shape of the
        // set nonce*, ruling (l)(iv)). Only a stamp tie falls back to the
        // per-field semilattice below. `accepted_at` and `left_at` (the
        // record's liveness, ruling (l)(iii)) join to the maximum.
        // Deterministic + commutative + idempotent.
        //
        // On a tie, `access` takes the SAME lexicographic-min join as
        // `set_name`, which for the CURRENT two-value vocabulary (`"reader"` < `"writer"`) is also
        // the fail-safe direction: two devices disagreeing about a grant converge
        // on the *lesser* privilege. That can only under-offer a binding for one
        // poll — the `caller_access` refresh (every federated read reply) re-writes
        // the row, and the field is advisory anyway (enforcement is the home nest's
        // gate). Under-offering heals; over-offering would hand the user a bind
        // gesture that dies at the eager mint. NOTE: the fail-safe direction is a
        // property of the current vocabulary, not of lexicographic-min itself — a
        // future grant tier sorting BELOW `"reader"` (e.g. `"admin"`) would flip
        // the join to *over*-offer, so anyone adding a grant value must revisit
        // this fold (and `access`'s advisory-only invariant on the read path).
        //
        // On a tie, `home_nest_actor_id` folds like `home_nest_url` (the trust
        // root it pairs with): lexicographic-min on a genuine two-`Some`
        // conflict, `None` ∨ `Some` → `Some`. A re-bind re-writes both.
        //
        // On a tie, the owner label folds as ONE unit — `(owner_handle,
        // owner_domain)` — so a handle never pairs with another device's
        // domain: an unstamped side yields to a stamped one, two stamped sides
        // take the lexicographically smaller pair.
        //
        // `content_key_floor` folds to the numeric-MAX: the floor is monotone on
        // the home nest (`content_key.put` stores `MAX(stored, new)`), so two
        // devices' stamps differ only by staleness and the higher one is the one
        // the pre-seal hold must honour — a seal under the lower would be under a
        // generation the owner rotated past. `None` ∨ `Some` → `Some`.
        //
        // `residency` folds on its OWN stamp ([`ForeignResidency::join`]): the
        // later reading wins, a tie lands on metadata-only. Folding it with the
        // four advisory fields would let a device that refreshed only `access`
        // carry a stale *full* over a flip another device had just written.
        let mut foreign_by_channel: BTreeMap<[u8; 32], ForeignFolder> = BTreeMap::new();
        for f in self.foreign_sets.iter().chain(other.foreign_sets.iter()) {
            match foreign_by_channel.get_mut(&f.channel_id) {
                None => {
                    foreign_by_channel.insert(f.channel_id, f.clone());
                }
                Some(acc) => {
                    match f.updated_at.cmp(&acc.updated_at) {
                        std::cmp::Ordering::Greater => {
                            acc.mls_group_id = f.mls_group_id.clone();
                            acc.home_nest_url = f.home_nest_url.clone();
                            acc.home_nest_actor_id = f.home_nest_actor_id.clone();
                            acc.access = f.access.clone();
                            acc.owner_handle = f.owner_handle.clone();
                            acc.owner_domain = f.owner_domain.clone();
                            acc.updated_at = f.updated_at;
                        }
                        std::cmp::Ordering::Less => {}
                        std::cmp::Ordering::Equal => {
                            if f.mls_group_id < acc.mls_group_id {
                                acc.mls_group_id = f.mls_group_id.clone();
                            }
                            if f.home_nest_url < acc.home_nest_url {
                                acc.home_nest_url = f.home_nest_url.clone();
                            }
                            acc.home_nest_actor_id =
                                match (acc.home_nest_actor_id.take(), f.home_nest_actor_id.clone())
                                {
                                    (Some(a), Some(b)) => Some(a.min(b)),
                                    (a, b) => a.or(b),
                                };
                            acc.access = match (acc.access.take(), f.access.clone()) {
                                (Some(a), Some(b)) => Some(a.min(b)),
                                (a, b) => a.or(b),
                            };
                            let ours = (acc.owner_handle.take(), acc.owner_domain.take());
                            let theirs = (f.owner_handle.clone(), f.owner_domain.clone());
                            (acc.owner_handle, acc.owner_domain) = match (&ours, &theirs) {
                                ((None, None), _) => theirs,
                                (_, (None, None)) => ours,
                                _ => ours.min(theirs),
                            };
                        }
                    }
                    acc.set_name = match (acc.set_name.take(), f.set_name.clone()) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (a, b) => a.or(b),
                    };
                    acc.accepted_at = acc.accepted_at.max(f.accepted_at);
                    acc.left_at = acc.left_at.max(f.left_at);
                    acc.content_key_floor =
                        match (acc.content_key_floor.take(), f.content_key_floor) {
                            (Some(a), Some(b)) => Some(a.max(b)),
                            (a, b) => a.or(b),
                        };
                    acc.residency = ForeignResidency::join(acc.residency, f.residency);
                }
            }
        }
        // Both sentinel lists take a canonical order, not arrival order — the
        // same law as every account-plane merge:
        // two replicas holding the same set must encode it identically, or each reads the
        // other's value as new state. Each key covers its record's whole
        // canonical encoding — the same thing its dedup above compares — so it
        // is total by construction and stays total as either record grows
        // fields.
        pending_removals.sort_by_cached_key(crate::encoding::canonical_tiebreak_key);
        Self {
            sets: by_identity.into_values().collect(),
            pending_removals,
            foreign_sets: foreign_by_channel.into_values().collect(),
        }
    }
}

// ── Multi-nest deployment-seed custody sub-record (BR-1) ──
//
// One [`DeploymentSeedEntry`] per nest an admin identity administers, keyed by
// the nest's `nest_actor_id`. The custody *behavior* (capture, propagation,
// supersession) lives in `libs/fauna-client-config`; this is just the at-rest
// shape + the read accessors. Authority for scope:
// `docs/goal/architecture/nest/box-recovery.md` § Trust & audience.

/// One nest's off-box deployment-seed custody entry (BR-1, decision
/// 2026-06-29; `fauna.state.deployment-seeds`). Keyed
/// by the nest's `nest_actor_id` (= `ActorKeypair::from_secret(seed).actor_id()`,
/// the channel-binding identity clients TOFU-pin), so an admin identity that
/// administers several nests custodies *each* box's recovery seed without one
/// clobbering another. The `seed` is the nest's irreplaceable 32-byte Ed25519
/// deployment seed (the preimage of `nest_actor_id`).
///
/// `Default` is derived so fixtures can be written struct-update style
/// (`DeploymentSeedEntry { nest_actor_id, seed, ..Default::default() }`) — this
/// type grows along one axis (custody metadata: `domain`, `superseded_by`, …),
/// and hand-listing every field is what makes two branches that each add a field
/// collide on the grown axis when they merge.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeploymentSeedEntry {
    /// The nest's `nest_actor_id` = `ActorKeypair::from_secret(seed).actor_id()`.
    /// The per-nest custody key; the merge unions entries by this.
    #[serde(with = "serde_bytes")]
    pub nest_actor_id: [u8; 32],
    /// The nest's 32-byte Ed25519 deployment seed (preimage of `nest_actor_id`).
    /// Sealed at rest under BackupKey on the account plane; never leaves the
    /// client fleet.
    /// Held as [`SecretArray32`] (zeroize-on-drop + redacted `Debug`; wire-identical
    /// to `[u8; 32]`) — hardening of the irreplaceable identity.
    pub seed: SecretArray32,
    /// The box's own **handle domain** at capture (server_name / DNS zone) — the
    /// human label for the `recover-box-item` row and the cloud re-provision
    /// input (step-4 leg 3: cloud-init `domain`, the A/AAAA re-point zone). The
    /// admin's *handle* domain need not equal a non-primary box's domain in the
    /// multi-nest case, so it can't be derived from the actor — it is captured
    /// from the box itself (`fauna.nest.info`'s `domain`) at claim. `None` for a
    /// **domainless** box (nest.info `domain == "unknown"`/empty — the real
    /// home-relay case), which is why this is `Option`, not `String`.
    ///
    /// `#[serde(default, skip_serializing_if)]` for bidirectional compat: a
    /// client without the field omits it (decodes to `None`) and — at or after this
    /// struct's own [`Self::extra`] catch-all — preserves an unknown `domain`
    /// key on re-seal (VERSION-SKEW class; `box-recovery.md` § Version
    /// compatibility). ⚠ Until the entry-level catch-all landed (2026-08-12,
    /// with `superseded_by`) this comment named a record-level catch-all, which
    /// flattens only **top-level** keys and never covered a key
    /// *inside* an entry — so a pre-`domain` client did drop it. Fixed by
    /// [`Self::extra`], not by weakening the claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// The successor `nest_actor_id` this box rotated **to**, once the
    /// deployment-seed rotation ceremony has committed (`box-recovery.md`
    /// § Custody after rotation). `None` for a live entry.
    ///
    /// **Marked, never deleted.** The seed bytes stay custodied — a wrongly
    /// marked entry loses nothing, and unmarking is a client act against the
    /// rotation chain — but every *recovery* read refuses a marked entry
    /// ([`DeploymentSeedEntry::seed_for`], and through it every projection):
    /// re-provisioning a box with a superseded seed would rebuild an identity
    /// every converged client now refuses, so the recovery UI must be
    /// structurally unable to offer it.
    ///
    /// Written by the rotating client (`fauna_client_config::rotate_deployment_seed_on_plane`)
    /// and by any client that verifies the chain (the custody leg's mark
    /// reconcile). Merged **present-wins** — an unmarked
    /// peer copy never un-marks a marked one ([`Self::fold_from`]), the same
    /// asymmetry as seed-present-wins and for the same reason: the direction
    /// that loses information is the one to refuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "serde_bytes")]
    pub superseded_by: Option<[u8; 32]>,
    /// **Forward-compat unknown-field catch-all for this entry** — the
    /// entry-level twin of a record-level `extra` catch-all, and the reason
    /// `box-recovery.md` § Version skew can say the `superseded_by` marker
    /// "rides the `extra` catch-all" *truthfully*. A record-level flatten
    /// extracts named top-level keys only, so before this existed an unknown key
    /// **inside** a `deployment_seeds` entry was dropped on re-seal by any app
    /// without the catch-all. Empty in the steady state, and an empty flatten
    /// map emits no keys — so adding it changed no byte on any existing wire.
    #[serde(flatten)]
    pub extra: BTreeMap<String, fauna_cbor::Value>,
}

impl DeploymentSeedEntry {
    /// Derive a nest's `nest_actor_id` from its 32-byte deployment seed — the one
    /// `seed → nest_actor_id` derivation the custody map keys on (mirrors the
    /// BR-2 verify site in `fauna_client_config::run_deployment_seed_custody_leg`).
    pub fn nest_actor_id_for_seed(seed: [u8; 32]) -> [u8; 32] {
        crate::identity::ActorKeypair::from_secret(seed)
            .actor_id()
            .0
    }
    /// Union two custody maps by `nest_actor_id` (BR-1) — **the shipped
    /// rule**: whole-map here and, through its per-row half
    /// [`Self::fold_from`], the `fauna.state.deployment-seeds` plane arm
    /// (`crate::deployment_seed_rows`), so the two cannot drift.
    ///
    /// **The no-data-loss close for the BR-1-RESIDUAL.** Entries union per key.
    /// Differing seeds derive to different keys, so two devices concurrently
    /// claiming *different* boxes each keep their seed. A key present on both
    /// sides carries the *same* seed by construction (the seed IS the preimage
    /// of `nest_actor_id` — a collision would be a forged/corrupt entry), so
    /// the first-seen seed is kept and the rest of the row joins through
    /// [`Self::fold_from`] (label join, present-wins supersession, `extra`
    /// union). The result is in `nest_actor_id` order.
    #[must_use]
    pub fn merge_seed_map(ours: &[Self], theirs: &[Self]) -> Vec<Self> {
        let mut by_id: BTreeMap<[u8; 32], Self> = BTreeMap::new();
        for e in ours.iter().chain(theirs).cloned() {
            match by_id.get_mut(&e.nest_actor_id) {
                None => {
                    by_id.insert(e.nest_actor_id, e);
                }
                // Same box: join the two rows.
                Some(acc) => {
                    acc.fold_from(e);
                }
            }
        }
        by_id.into_values().collect()
    }

    /// The custodied deployment seed for a specific `nest_actor_id` in a folded
    /// custody map, or `None` — the **resolution-point read** every recovery
    /// reader resolves one box through, over whichever source folded the map
    /// (the plane's readers).
    ///
    /// **A superseded entry reads as absent** ([`Self::superseded_by`];
    /// `box-recovery.md` § Custody after rotation). This is the single filter site
    /// on purpose: the two projections the goal doc names are not the only recovery
    /// readers — the re-provision drive
    /// (`fauna-onboarding-machine/src/recovery_config.rs`), the wasm and native
    /// self-hosted-command getters all resolve a seed through *this* function, and
    /// the drive is the consumer that would actually rebuild the box on a refused
    /// identity. Filtering the readers one by one is completeness-by-enumeration,
    /// the failure mode the ceremony's own KEK-satellite step was already bitten by
    /// (130th pass); filtering at the resolution point is structural.
    ///
    /// The map itself deliberately does **not** filter — the seed bytes are
    /// retained, and the cross-device merge must still see a superseded entry to
    /// keep retaining them.
    pub fn seed_for(map: &[Self], nest_actor_id: &[u8; 32]) -> Option<[u8; 32]> {
        if map
            .iter()
            .any(|e| &e.nest_actor_id == nest_actor_id && e.superseded_by.is_some())
        {
            return None;
        }
        if let Some(e) = map.iter().find(|e| {
            // Match the requested id AND
            // re-derive it from the stored seed, so a corrupt at-rest entry (a key
            // that is not the preimage of its own `seed`) is treated as absent
            // rather than yielding a WRONG recovery seed — which would
            // re-instantiate a different identity every TOFU-pinned client rejects.
            // (The BR-2-gated capture path is the sole writer and never stores
            // a mismatched entry, so this is pure robustness against a corrupt
            // blob, not a reachable bug.)
            &e.nest_actor_id == nest_actor_id
                && crate::data::DeploymentSeedEntry::nest_actor_id_for_seed(e.seed.to_array())
                    == *nest_actor_id
        }) {
            // Copy the raw seed out at the read boundary (the restore path hex-encodes
            // it; un-zeroizable there by design — secret module docs).
            return Some(e.seed.to_array());
        }
        None
    }

    /// The custodied box's **handle domain** for a specific `nest_actor_id` in a
    /// folded custody map, or `None` (unknown box, or a domainless box). The
    /// recovery UI reads this for the box-list label and the cloud re-provision
    /// drive (step-4 leg 3 — the DNS zone / cloud-init `domain`). Re-derives the id
    /// from the stored seed (parity with
    /// [`Self::seed_for`]), so a corrupt at-rest entry is treated as absent rather
    /// than yielding a domain for the wrong box.
    pub fn domain_for(map: &[Self], nest_actor_id: &[u8; 32]) -> Option<String> {
        map.iter()
            .find(|e| {
                &e.nest_actor_id == nest_actor_id
                    && crate::data::DeploymentSeedEntry::nest_actor_id_for_seed(e.seed.to_array())
                        == *nest_actor_id
            })
            .and_then(|e| e.domain.clone())
    }

    /// Fold another custody row **for the same box** into this one, answering
    /// whether anything changed.
    ///
    /// **This is the one statement of how two custody rows for a box combine**,
    /// shared by every path that combines them: the whole-map union
    /// ([`Self::merge_seed_map`], the per-key union) and the
    /// `fauna.state.deployment-seeds` plane arm. Deliberately one function
    /// rather than two agreeing implementations — the
    /// defect was a propagation path that asked *"is this seed missing?"*
    /// where the merge asks *"what do these two rows join to"*, and two
    /// hand-written joins of one lattice is how that divergence recurs.
    ///
    /// The join is a semilattice per field, so the result never depends on which
    /// side folded or in what order:
    ///
    /// * `seed` — untouched. Same `nest_actor_id` ⇒ same seed by construction
    ///   (the id *is* its preimage), so there is nothing to choose.
    /// * `domain` — `None` ∨ `Some` → `Some`; two differing `Some`s → the
    ///   lexicographically smaller (the join `FoldersConfig::merge` uses for
    ///   `set_name`). A label captured on either side survives.
    /// * `superseded_by` — **present-wins**, the same asymmetry as
    ///   seed-present-wins and for the same reason: only one direction loses
    ///   information. A device that has not yet seen the rotation holds an
    ///   unmarked copy of the very entry another device just marked, so
    ///   `(Some, None)` is the *ordinary* mid-convergence state — letting `None`
    ///   win would un-mark the predecessor on every fold with a lagging peer and
    ///   put a refused identity back in the recovery list. Two differing `Some`s
    ///   mean two rotations observed off the same predecessor (fork evidence, or
    ///   one side seeing a later hop); take the smaller so the fold stays
    ///   commutative — the marker's job is only "not live", and which successor
    ///   named it changes no recovery decision.
    /// * `extra` — union per key, **ours** first, so a field one device
    ///   round-trips blindly survives (a record-level catch-all's rule at entry
    ///   scope).
    pub fn fold_from(&mut self, other: DeploymentSeedEntry) -> bool {
        let mut changed = false;

        let domain = match (self.domain.clone(), other.domain) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if domain != self.domain {
            self.domain = domain;
            changed = true;
        }

        let superseded_by = match (self.superseded_by, other.superseded_by) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if superseded_by != self.superseded_by {
            self.superseded_by = superseded_by;
            changed = true;
        }

        for (k, v) in other.extra {
            if let std::collections::btree_map::Entry::Vacant(slot) = self.extra.entry(k) {
                slot.insert(v);
                changed = true;
            }
        }

        changed
    }
}

#[cfg(test)]
mod arrival_disposition_tests {
    use super::{ArrivalDisposition, ContactStatus, contact_arrival_disposition};

    #[test]
    fn from_wire_parses_the_stored_status_tokens() {
        // The four lowercase tokens the nest persists (`bins/fauna-nest/src/db/
        // contacts.rs`) round-trip to the typed enum; anything else is `None` (a
        // stranger), which `contact_arrival_disposition` then treats as a knock.
        assert_eq!(
            ContactStatus::from_wire("pending"),
            Some(ContactStatus::Pending)
        );
        assert_eq!(
            ContactStatus::from_wire("accepted"),
            Some(ContactStatus::Accepted)
        );
        assert_eq!(
            ContactStatus::from_wire("confirmed"),
            Some(ContactStatus::Confirmed)
        );
        assert_eq!(
            ContactStatus::from_wire("blocked"),
            Some(ContactStatus::Blocked)
        );
        assert_eq!(ContactStatus::from_wire(""), None);
        assert_eq!(
            ContactStatus::from_wire("Confirmed"),
            Some(ContactStatus::Unrecognized),
            "case-sensitive"
        );
        assert_eq!(
            ContactStatus::from_wire("muted"),
            Some(ContactStatus::Unrecognized),
            "a newer nest's status is an edge this build cannot name, never a stranger"
        );
        // The client folder gate knocks on an unrecognized or absent status:
        // suppressing there would leave the share.
        assert_eq!(
            contact_arrival_disposition(ContactStatus::from_wire("muted")),
            ArrivalDisposition::Knock
        );
        assert_eq!(contact_arrival_disposition(None), ArrivalDisposition::Knock);
    }

    #[test]
    fn confirmed_and_accepted_contacts_auto() {
        // The frictionless case: a known contact's share auto-joins.
        assert_eq!(
            contact_arrival_disposition(Some(ContactStatus::Confirmed)),
            ArrivalDisposition::Auto
        );
        assert_eq!(
            contact_arrival_disposition(Some(ContactStatus::Accepted)),
            ArrivalDisposition::Auto
        );
    }

    #[test]
    fn stranger_and_pending_knock() {
        // A true stranger (no contact record) and a still-pending knock both
        // stage — never auto-join (folders.md § Sharing: "a stranger cannot
        // force a set into your list").
        assert_eq!(contact_arrival_disposition(None), ArrivalDisposition::Knock);
        assert_eq!(
            contact_arrival_disposition(Some(ContactStatus::Pending)),
            ArrivalDisposition::Knock
        );
    }

    #[test]
    fn blocked_is_suppressed() {
        // A blocked sender's knock is dropped silently — it never surfaces.
        assert_eq!(
            contact_arrival_disposition(Some(ContactStatus::Blocked)),
            ArrivalDisposition::Suppress
        );
    }
}

#[cfg(test)]
mod account_record_tests {
    use super::*;
    use crate::encoding::{canonical_decode, canonical_encode};

    /// A filled anchor vector REFUSES the next seed and keeps what it holds —
    /// the count half of harvest rule 5
    /// (`identity-succession.md` § The succession statement → *the
    /// peer-profile harvest*).
    ///
    /// The no-eviction half is the load-bearing assertion, not a nicety.
    /// Entries are first-write-wins, so the OLDEST are the most likely to be
    /// load-bearing; a "keep the newest N" policy would invert that value order
    /// AND hand an attacker an eviction primitive — publish entries until the
    /// honest anchors are pushed out. This test fails if anyone ever swaps the
    /// refusal for an eviction.
    #[test]
    fn a_full_anchor_vector_refuses_the_next_seed_and_evicts_nothing() {
        let mut config = PeerAnchors::default();

        for i in 0..MAX_PEER_ANCHOR_ENTRIES {
            let mut raw = [0u8; 32];
            raw[..8].copy_from_slice(&(i as u64).to_be_bytes());
            assert!(
                config.seed_anchor_domain(ActorId(raw), format!("peer{i}.example")),
                "the vector must accept every entry up to the ceiling (at {i})"
            );
        }
        assert_eq!(config.anchor_domains.len(), MAX_PEER_ANCHOR_ENTRIES);

        let overflow = ActorId([0xAA; 32]);
        assert!(
            !config.seed_anchor_domain(overflow, "one-too-many.example".into()),
            "a vector at MAX_PEER_ANCHOR_ENTRIES refuses the next seed"
        );
        assert_eq!(
            config.anchor_domains.len(),
            MAX_PEER_ANCHOR_ENTRIES,
            "the refusal must not grow the vector"
        );
        assert!(
            config.known_anchor_domain(&overflow).is_none(),
            "a refused seed is not stored"
        );

        // No eviction: the FIRST entry — the oldest, and the one a
        // newest-N policy would drop — is still exactly where it was.
        let mut first = [0u8; 32];
        first[..8].copy_from_slice(&0u64.to_be_bytes());
        assert_eq!(
            config.known_anchor_domain(&ActorId(first)).as_deref(),
            Some("peer0.example"),
            "refusal, never eviction — the oldest entry survives the overflow"
        );

        // The head vector holds the same bound, independently.
        let mut heads = PeerAnchors::default();
        for i in 0..MAX_PEER_ANCHOR_ENTRIES {
            let mut raw = [0u8; 32];
            raw[..8].copy_from_slice(&(i as u64).to_be_bytes());
            assert!(heads.seed_chain_head(
                ActorId(raw),
                crate::recovery::ChainHead {
                    recovery_pubkey: [3u8; 32],
                    seq: 1,
                },
            ));
        }
        assert!(
            !heads.seed_chain_head(
                overflow,
                crate::recovery::ChainHead {
                    recovery_pubkey: [4u8; 32],
                    seq: 1,
                },
            ),
            "peer_chain_heads carries the ceiling too — both writers are `pub`, \
             so a door-only rule would bind only the door's own callers"
        );
        assert_eq!(heads.chain_heads.len(), MAX_PEER_ANCHOR_ENTRIES);
    }

    /// The door refuses with `StoreFull` only when NEITHER vector has room —
    /// with room in one of them a profile still seeds that half.
    ///
    /// Refusing a chain head that fits because the *domain* vector is full
    /// would discard the more useful half (a head is the tier-1 anchor) for the
    /// fuller one's sake, so the door's bound is deliberately weaker than the
    /// writers'.
    #[test]
    fn the_door_refuses_only_when_both_anchor_vectors_are_full() {
        let peer_kp = crate::identity::ActorKeypair::from_secret([9u8; 32]);
        let peer = peer_kp.actor_id();
        let profile = Profile {
            actor_id: peer,
            display_name: Some("Peer".into()),
            bio: None,
            avatar: None,
            banner: None,
            links: Vec::new(),
            nests: vec![NestEntry {
                nest_id: vec![7u8; 32],
                url: "https://peer.example".into(),
                roles: Vec::new(),
            }],
            admin_nests: Vec::new(),
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: Some(crate::recovery::ChainHead {
                recovery_pubkey: [5u8; 32],
                seq: 4,
            }),
            updated_at: Timestamp::now(),
        };
        let bytes =
            crate::encoding::sign_and_pack(&peer_kp, &profile).expect("the fixture profile signs");

        // Domains full, heads empty: the head half still seeds.
        let mut config = PeerAnchors::default();
        for i in 0..MAX_PEER_ANCHOR_ENTRIES {
            let mut raw = [0u8; 32];
            raw[..8].copy_from_slice(&(i as u64).to_be_bytes());
            config.seed_anchor_domain(ActorId(raw), format!("peer{i}.example"));
        }
        let seed = config
            .seed_from_peer_profile_bytes(&peer, &bytes)
            .expect("one vector with room is not a refusal");
        assert!(
            seed.seeded_head && !seed.seeded_domain,
            "the half with room seeds; the full half does not: {seed:?}"
        );

        // Now both are full: the door refuses, and nothing is written.
        let mut both = PeerAnchors::default();
        for i in 0..MAX_PEER_ANCHOR_ENTRIES {
            let mut raw = [0u8; 32];
            raw[..8].copy_from_slice(&(i as u64).to_be_bytes());
            both.seed_anchor_domain(ActorId(raw), format!("peer{i}.example"));
            both.seed_chain_head(
                ActorId(raw),
                crate::recovery::ChainHead {
                    recovery_pubkey: [3u8; 32],
                    seq: 1,
                },
            );
        }
        assert_eq!(
            both.seed_from_peer_profile_bytes(&peer, &bytes),
            Err(PeerAnchorRefusal::StoreFull)
        );
        assert!(
            both.known_chain_head(&peer).is_none() && both.known_anchor_domain(&peer).is_none(),
            "every refusal arm promises nothing was seeded"
        );
    }

    /// A full store still DEMOTES a head it already holds. The outrun mark
    /// flips a flag on a held entry and takes no slot, so the count ceiling —
    /// which exists to refuse NEW seeds — has no business refusing it.
    ///
    /// The refusal used to sit ahead of the head match, so a member whose store
    /// had filled (organically, or because one hostile room's policy names
    /// filled it) could never again learn that an anchored contact rotated
    /// their RecoveryKey: the harvest settled the peer as refused, the harvest
    /// wait released, and the retired kit's statement settled offline at
    /// tier 1 — the exact "found later" case the wait was built to close.
    #[test]
    fn a_full_store_still_marks_a_held_head_outrun() {
        let peer_kp = crate::identity::ActorKeypair::from_secret([9u8; 32]);
        let peer = peer_kp.actor_id();
        let profile = Profile {
            actor_id: peer,
            display_name: Some("Peer".into()),
            bio: None,
            avatar: None,
            banner: None,
            links: Vec::new(),
            nests: vec![NestEntry {
                nest_id: vec![7u8; 32],
                url: "https://peer.example".into(),
                roles: Vec::new(),
            }],
            admin_nests: Vec::new(),
            load_hint: None,
            inbox_mode: InboxMode::Open,
            // The peer rotated: their own profile now claims a head past the
            // one this fleet anchored.
            recovery_head: Some(crate::recovery::ChainHead {
                recovery_pubkey: [6u8; 32],
                seq: 5,
            }),
            updated_at: Timestamp::now(),
        };
        let bytes =
            crate::encoding::sign_and_pack(&peer_kp, &profile).expect("the fixture profile signs");

        // The peer holds one slot in each vector (anchored before the
        // rotation); strangers fill every other one.
        let mut config = PeerAnchors::default();
        assert!(config.seed_chain_head(
            peer,
            crate::recovery::ChainHead {
                recovery_pubkey: [5u8; 32],
                seq: 4,
            },
        ));
        assert!(config.seed_anchor_domain(peer, "peer.example".into()));
        for i in 1..MAX_PEER_ANCHOR_ENTRIES {
            let mut raw = [0u8; 32];
            raw[..8].copy_from_slice(&(i as u64).to_be_bytes());
            config.seed_anchor_domain(ActorId(raw), format!("peer{i}.example"));
            config.seed_chain_head(
                ActorId(raw),
                crate::recovery::ChainHead {
                    recovery_pubkey: [3u8; 32],
                    seq: 1,
                },
            );
        }
        assert_eq!(config.chain_heads.len(), MAX_PEER_ANCHOR_ENTRIES);
        assert_eq!(config.anchor_domains.len(), MAX_PEER_ANCHOR_ENTRIES);

        let seed = config
            .seed_from_peer_profile_bytes(&peer, &bytes)
            .expect("a demotion needs no slot, so a full store must not refuse it");
        assert!(
            seed.marked_outrun && !seed.seeded_head && !seed.seeded_domain,
            "the held head is demoted and nothing new is seeded: {seed:?}"
        );
        assert!(config.chain_head_is_outrun(&peer));
        assert_eq!(
            config.known_chain_head(&peer).map(|head| head.seq),
            Some(4),
            "a demotion never advances or displaces the held head"
        );

        // A second read of the same profile writes nothing, and with nothing
        // to write and nothing it could seed it is the ordinary already-
        // anchored case — not a refusal.
        let again = config
            .seed_from_peer_profile_bytes(&peer, &bytes)
            .expect("an already-anchored peer is not refused by a full store");
        assert!(!again.changed());
    }

    /// A profile signed by the account's **delegated authoring sub-key** seeds
    /// neither half. That key is held by the peer's own home nest (D10), which
    /// is the party serving the profile, so honoring it would let that nest
    /// choose a tier-1 succession anchor for an identity whose key it does not
    /// hold.
    #[test]
    fn a_delegated_profile_seeds_nothing() {
        let peer_kp = crate::identity::ActorKeypair::from_secret([9u8; 32]);
        let peer = peer_kp.actor_id();
        // The nest-held authoring sub-key, and the identity-signed cert that
        // authorizes it for `Profile` envelopes — exactly D10's shape.
        let sub = crate::identity::ActorKeypair::from_secret([4u8; 32]);
        let profile = Profile {
            actor_id: peer,
            display_name: Some("Peer".into()),
            bio: None,
            avatar: None,
            banner: None,
            links: Vec::new(),
            nests: vec![NestEntry {
                nest_id: vec![7u8; 32],
                url: "https://planted.example".into(),
                roles: Vec::new(),
            }],
            admin_nests: Vec::new(),
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: Some(crate::recovery::ChainHead {
                recovery_pubkey: [5u8; 32],
                seq: 9,
            }),
            updated_at: Timestamp::now(),
        };
        let cert = DeviceAuthorization {
            actor_id: peer,
            device_key: sub.actor_id().0,
            capabilities: vec![Capability::UpdateProfile],
            created_at: Timestamp(0),
            expires_at: None,
        };
        let (cert_bytes, cert_env) =
            crate::encoding::sign_envelope(&peer_kp, &cert).expect("the cert signs");
        let (bytes, env) =
            crate::encoding::sign_envelope(&sub, &profile).expect("the sub-key signs the profile");
        let wire = crate::encoding::EmbedAsBytes::from_signed(bytes, env).with_signer_auth(
            crate::encoding::EmbedAsBytes::from_signed(cert_bytes, cert_env),
        );
        let delegated =
            crate::encoding::canonical_encode(&wire).expect("the delegated wire encodes");
        assert!(
            matches!(
                crate::encoding::decode_profile(&delegated),
                Ok((_, crate::encoding::AuthoringOrigin::Delegated { .. }))
            ),
            "the fixture really is the delegated shape the harvest must refuse"
        );

        let mut config = PeerAnchors::default();
        assert_eq!(
            config.seed_from_peer_profile_bytes(&peer, &delegated),
            Err(PeerAnchorRefusal::Delegated)
        );
        assert!(
            config.known_chain_head(&peer).is_none() && config.known_anchor_domain(&peer).is_none(),
            "every refusal arm promises nothing was seeded — the head AND the domain"
        );

        // The control: the same fields, signed by the identity key itself,
        // seed both halves. Only the signer differs.
        let direct =
            crate::encoding::sign_and_pack(&peer_kp, &profile).expect("the identity key signs");
        assert_eq!(
            config.seed_from_peer_profile_bytes(&peer, &direct),
            Ok(PeerAnchorSeed {
                seeded_head: true,
                seeded_domain: true,
                marked_outrun: false,
            })
        );
    }

    /// The harvest's second write strength: a directly-signed profile that
    /// claims a head PAST the held one demotes it — and only demotes. The head
    /// itself must not move (rule 3: the profile's signer is the key a seed
    /// thief holds), a claim at or behind the held head is the mirror lagging
    /// and marks nothing, and only a verified walk's advance clears the mark.
    #[test]
    fn a_profile_claiming_past_the_held_head_demotes_it_and_never_advances_it() {
        use crate::recovery::ChainHead;
        let peer_kp = crate::identity::ActorKeypair::from_secret([7u8; 32]);
        let peer = peer_kp.actor_id();
        let profile_with = |head: ChainHead| {
            let profile = Profile {
                actor_id: peer,
                display_name: Some("Peer".into()),
                bio: None,
                avatar: None,
                banner: None,
                links: Vec::new(),
                nests: Vec::new(),
                admin_nests: Vec::new(),
                load_hint: None,
                inbox_mode: InboxMode::Open,
                recovery_head: Some(head),
                updated_at: Timestamp::now(),
            };
            crate::encoding::sign_and_pack(&peer_kp, &profile).expect("the identity key signs")
        };
        let held = ChainHead::new([1u8; 32], 3);
        let mut config = PeerAnchors::default();
        assert!(config.remember_chain_head(peer, held));

        // Behind, and equal: the mirror lagging a walk. Nothing is written.
        for lagging in [ChainHead::new([0u8; 32], 2), held] {
            let seed = config
                .seed_from_peer_profile_bytes(&peer, &profile_with(lagging))
                .expect("a direct profile is admitted");
            assert!(
                !seed.changed(),
                "a claim at or behind the held head marks nothing"
            );
            assert!(!config.chain_head_is_outrun(&peer));
        }

        // Past — the rotated-kit shape (a new key at seq+1) and the same-height
        // equivocation both demote, exactly once.
        let rotated = ChainHead::new([2u8; 32], 4);
        let seed = config
            .seed_from_peer_profile_bytes(&peer, &profile_with(rotated))
            .expect("a direct profile is admitted");
        assert_eq!(
            seed,
            PeerAnchorSeed {
                seeded_head: false,
                seeded_domain: false,
                marked_outrun: true,
            }
        );
        assert!(config.chain_head_is_outrun(&peer));
        assert_eq!(
            config.known_chain_head(&peer),
            Some(held),
            "demoting is not advancing — the held head is still the walk's guard"
        );
        let again = config
            .seed_from_peer_profile_bytes(&peer, &profile_with(rotated))
            .expect("a direct profile is admitted");
        assert!(
            !again.changed(),
            "an already-marked head costs no second write"
        );

        let mut same_height = PeerAnchors::default();
        assert!(same_height.remember_chain_head(peer, held));
        assert!(same_height.mark_chain_head_outrun(&peer, ChainHead::new([2u8; 32], 3)));

        // Only the walk clears it, by replacing the head the mark was about.
        assert!(config.remember_chain_head(peer, rotated));
        assert!(!config.chain_head_is_outrun(&peer));
        assert_eq!(config.known_chain_head(&peer), Some(rotated));
    }

    /// The mark is additive: an unmarked entry encodes byte-identically to the
    /// pre-field shape, and pre-field bytes decode unmarked.
    #[test]
    fn an_unmarked_chain_head_encodes_as_it_did_before_the_mark_existed() {
        let mut config = PeerAnchors::default();
        let peer = ActorId([2u8; 32]);
        assert!(config.remember_chain_head(peer, crate::recovery::ChainHead::new([9u8; 32], 1)));
        let unmarked = crate::encoding::canonical_encode(&config.chain_heads[0]).unwrap();
        assert!(
            !unmarked.windows(6).any(|w| w == b"outrun"),
            "an unmarked entry must not carry the key at all"
        );
        let decoded: PeerChainHead = crate::encoding::canonical_decode(&unmarked).unwrap();
        assert!(!decoded.outrun);

        assert!(
            config.mark_chain_head_outrun(&peer, crate::recovery::ChainHead::new([8u8; 32], 2))
        );
        let marked = crate::encoding::canonical_encode(&config.chain_heads[0]).unwrap();
        let decoded: PeerChainHead = crate::encoding::canonical_decode(&marked).unwrap();
        assert!(decoded.outrun, "the mark survives the round trip");
    }

    /// Every writer stamps `first_seen` on the FIRST write and nothing
    /// re-stamps it: the stamp is the merge ceiling's ordering key
    /// ([`MAX_PEER_ANCHOR_ENTRIES`]), so a writer that left it at the epoch
    /// would make every new anchor look epoch-old, and an advance that
    /// re-stamped would make the longest-held anchors look newest.
    #[test]
    fn the_anchor_writers_stamp_first_seen_once_and_an_advance_keeps_it() {
        let mut config = PeerAnchors::default();
        let peer = ActorId([2u8; 32]);
        let seeded = ActorId([3u8; 32]);

        assert!(config.remember_chain_head(peer, crate::recovery::ChainHead::new([9u8; 32], 1)));
        let stamped = config.chain_heads[0].first_seen;
        assert!(
            !stamped.is_epoch(),
            "the first write stamps a real moment, never the epoch"
        );
        assert!(config.remember_chain_head(peer, crate::recovery::ChainHead::new([9u8; 32], 5)));
        assert_eq!(config.chain_heads[0].seq, 5);
        assert_eq!(
            config.chain_heads[0].first_seen, stamped,
            "a verified-walk advance is not a new sighting — first_seen keeps \
             the original stamp"
        );

        assert!(config.seed_chain_head(seeded, crate::recovery::ChainHead::new([8u8; 32], 1)));
        assert!(config.seed_anchor_domain(seeded, "seeded.example".to_string()));
        let head = config
            .chain_heads
            .iter()
            .find(|e| e.actor == seeded)
            .unwrap();
        let domain = config
            .anchor_domains
            .iter()
            .find(|e| e.actor == seeded)
            .unwrap();
        assert!(!head.first_seen.is_epoch() && !domain.first_seen.is_epoch());
        assert!(
            head.first_seen >= stamped && domain.first_seen >= head.first_seen,
            "stamps are taken from the clock at write time, in write order"
        );
    }

    #[test]
    fn trained_factor_meta_derives_its_canonical_factor_key() {
        // The registry stores the raw 16-byte id; the composition key is
        // *derived* (`scoring::topic_factor`), so a malformed key can never be
        // minted by hand at a call site. A wrong-length id yields `None`.
        let meta = TrainedFactorMeta {
            id: vec![0x01; 16],
            name: "Cats".into(),
            learn_from_engagement: false,
            created_at: 0,
        };
        assert_eq!(
            meta.factor_key().as_deref(),
            Some("topic:01010101010101010101010101010101")
        );
        assert!(crate::scoring::is_topic_factor(&meta.factor_key().unwrap()));

        let malformed = TrainedFactorMeta {
            id: vec![0x01; 15],
            ..meta
        };
        assert_eq!(malformed.factor_key(), None);
    }

    /// A corrupt map entry — one whose `nest_actor_id` is
    /// not the preimage of its own `seed` — must NOT be returned by
    /// `DeploymentSeedEntry::seed_for` (returning it would yield a wrong recovery seed that
    /// re-instantiates a different identity). The read accessor re-derives, so the
    /// entry is treated as absent; a well-formed entry for the same id resolves.
    #[test]
    fn deployment_seed_for_rejects_corrupt_entry() {
        let good_seed = [0x5au8; 32];
        let good_id = DeploymentSeedEntry::nest_actor_id_for_seed(good_seed);
        // Corrupt: key == good_id, but the stored seed derives to a different id.
        let corrupt = vec![DeploymentSeedEntry {
            nest_actor_id: good_id,
            seed: [0x77u8; 32].into(),
            domain: Some("attacker.example".into()),
            ..Default::default()
        }];
        assert_eq!(
            DeploymentSeedEntry::seed_for(&corrupt, &good_id),
            None,
            "a self-inconsistent map entry must be treated as absent"
        );
        assert_eq!(
            DeploymentSeedEntry::domain_for(&corrupt, &good_id),
            None,
            "domain_for re-derives too — a corrupt entry yields no domain"
        );
        // A well-formed entry for the same id still resolves.
        let good = vec![DeploymentSeedEntry {
            nest_actor_id: good_id,
            seed: good_seed.into(),
            domain: Some("good.example".into()),
            ..Default::default()
        }];
        assert_eq!(
            DeploymentSeedEntry::seed_for(&good, &good_id),
            Some(good_seed)
        );
        assert_eq!(
            DeploymentSeedEntry::domain_for(&good, &good_id),
            Some("good.example".into())
        );
    }

    /// A superseded custody entry reads as **absent** to every recovery reader
    /// (`box-recovery.md` § Custody after rotation) — re-provisioning a box with
    /// a superseded seed rebuilds an identity every converged client refuses, so
    /// the resolution point, not each caller, is where it must be refused.
    #[test]
    fn deployment_seed_for_refuses_a_superseded_entry() {
        let old_seed = [0xA1u8; 32];
        let new_seed = [0xB2u8; 32];
        let old_id = DeploymentSeedEntry::nest_actor_id_for_seed(old_seed);
        let new_id = DeploymentSeedEntry::nest_actor_id_for_seed(new_seed);
        let cfg = vec![
            DeploymentSeedEntry {
                nest_actor_id: old_id,
                seed: old_seed.into(),
                superseded_by: Some(new_id),
                ..Default::default()
            },
            DeploymentSeedEntry {
                nest_actor_id: new_id,
                seed: new_seed.into(),
                ..Default::default()
            },
        ];
        assert_eq!(DeploymentSeedEntry::seed_for(&cfg, &old_id), None);
        // The successor — the identity the box actually serves — still resolves.
        assert_eq!(DeploymentSeedEntry::seed_for(&cfg, &new_id), Some(new_seed));
        // Marked, never deleted: the bytes stay custodied, so the cross-device
        // merge keeps retaining them and an erroneous mark loses nothing.
        assert_eq!(cfg.len(), 2);
        assert_eq!(
            cfg.iter()
                .find(|e| e.nest_actor_id == old_id)
                .map(|e| e.seed.to_array()),
            Some(old_seed)
        );
    }

    /// The at-rest bytes of [`mail_msek_family_fixture`], captured 2026-08-12
    /// from the pre-`SecretArray32` (bare `[u8; 32]`) encoding and re-cut once,
    /// 2026-09-29, when every fixed-width byte field became a CBOR byte string
    /// (`docs/goal/architecture/serialization.md` § "Fixed-size byte arrays",
    /// landed under the 2026-09-24 baseline reset): each
    /// 32-element integer array became the 32-byte byte string, nothing else
    /// moved. Re-cut a second time, 2026-09-30, under the same reset, when the
    /// `fauna.state.mail` consumer cut deleted `PendingRotation`'s id list (the
    /// sentinel's map lost its `credentials_remaining` entry, nothing else
    /// moved; a blob still carrying the key decodes, the field having never
    /// refused unknown keys).
    ///
    /// **This constant is the compatibility contract, and it is deliberately a
    /// literal rather than a re-encode of whatever the code currently does.** A
    /// round-trip test (encode → decode → compare) passes just as happily after
    /// a wire change, because both halves move together; only a frozen literal
    /// captured *before* the change can prove the shape did not move. Every
    /// account-plane record written by a shipped client is sealed at rest under
    /// `BackupKey`, so a changed encoding is unreadable user data, not a
    /// migration (product invariant *No user-data loss*).
    const MAIL_MSEK_FAMILY_GOLDEN_HEX: &str = "b363646e73a86b63726564656e7469616c73806b64656c65676174696f6e73806c61636d655f6163636f756e74f66e6175746f5f72656e65775f6f6666806f6d616e616765645f646f6d61696e738074646b696d5f7075626c69736865645f6e616d6573a07470656e64696e675f6d616e75616c5f6973737565f677617470726f746f5f7075626c69736865645f6e616d6573a0646d61696ca8646d73656b582042424242424242424242424242424242424242424242424242424242424242426b63726564656e7469616c73806b7072696f725f6d73656b738258201111111111111111111111111111111111111111111111111111111111111111582022222222222222222222222222222222222222222222222222222222222222226c6d61696c5f656e61626c6564f66e63616c6461765f656e61626c6564f46f636172646461765f656e61626c6564f47070656e64696e675f726f746174696f6ea1686e65775f6d73656b58203333333333333333333333333333333333333333333333333333333333333333767072696f725f6d73656b5f7265746972656d656e747382a2646d73656b582011111111111111111111111111111111111111111111111111111111111111116f726574697265645f61745f756e69781a6553f164a2646d73656b582022222222222222222222222222222222222222222222222222222222222222226f726574697265645f61745f756e69781a6553f132666261636b7570a16c64657374696e6174696f6e738067617470726f746fa16f6170705f63726564656e7469616c738067626c7565736b79a46d726f746174696f6e5f6b657973806f636f6e746573745f696e74656e7473806f6e6573745f6e616d65645f646964738072746f6d6273746f6e655f636f6e73656e747380686163746f725f6964582003030303030303030303030303030303030303030303030303030303030303036873656374696f6e73806966696c655f73657473a46473657473806c666f726569676e5f73657473806f70656e64696e675f72657365616c73807070656e64696e675f72656d6f76616c73806a64656c65676174696f6ea16b61737369676e6d656e7473806a6d6f6465726174696f6ea16e6d757465645f6b6579776f726473806a73796e635f7072656673a17764656661756c745f636f6e666c6963745f706f6c696379f66a757064617465645f6174006c6772616e745f6576656e7473806d737562736372697074696f6e73a2657469657273807070656e64696e675f72656d6f76616c73806e736368656d615f76657273696f6e056f6465706c6f796d656e745f73656564f66f706572736f6e616c697a6174696f6ea16f747261696e65645f666163746f727380706465706c6f796d656e745f736565647380726d696e5f7265616465725f76657273696f6e01";

    /// The `mail` sub-blob of [`MAIL_MSEK_FAMILY_GOLDEN_HEX`] — every MSEK
    /// carrier, in its historical encoding, lifted out verbatim (it is a literal
    /// substring of the whole-config golden, which
    /// [`mail_msek_family_golden_mail_blob_is_the_historical_substring`] pins).
    ///
    /// This is the constant the byte-identity assertions use, because it is
    /// **stable under additive evolution elsewhere in the account state** — a new
    /// `#[serde(default)]` field on a sibling section (`dns`, `backup`, …) grows
    /// the whole-config encoding but cannot touch these bytes. The whole-config
    /// golden stays frozen as the *decode* fixture; this one carries the *encode*
    /// contract.
    const MAIL_MSEK_FAMILY_MAIL_GOLDEN_HEX: &str = "a8646d73656b582042424242424242424242424242424242424242424242424242424242424242426b63726564656e7469616c73806b7072696f725f6d73656b738258201111111111111111111111111111111111111111111111111111111111111111582022222222222222222222222222222222222222222222222222222222222222226c6d61696c5f656e61626c6564f66e63616c6461765f656e61626c6564f46f636172646461765f656e61626c6564f47070656e64696e675f726f746174696f6ea1686e65775f6d73656b58203333333333333333333333333333333333333333333333333333333333333333767072696f725f6d73656b5f7265746972656d656e747382a2646d73656b582011111111111111111111111111111111111111111111111111111111111111116f726574697265645f61745f756e69781a6553f164a2646d73656b582022222222222222222222222222222222222222222222222222222222222222226f726574697265645f61745f756e69781a6553f132";

    /// The two goldens cannot drift apart: the MSEK sub-blob must remain a
    /// verbatim substring of the frozen whole-config bytes. Without this, a
    /// session could "fix" a red M-241-wire by re-cutting only the sub-blob and
    /// quietly lose the historical anchor.
    #[test]
    fn mail_msek_family_golden_mail_blob_is_the_historical_substring() {
        assert!(
            MAIL_MSEK_FAMILY_GOLDEN_HEX.contains(MAIL_MSEK_FAMILY_MAIL_GOLDEN_HEX),
            "the MSEK sub-blob is no longer the one inside the historical \
             whole-config golden — one of the two was re-cut in isolation"
        );
    }

    /// One person raised by two different successions is ONE row on every
    /// surface, carrying both reasons — the ratified render shape, because "do I
    /// trust this person" is a single judgment and two rows read as being asked
    /// it twice.
    #[test]
    fn open_member_reviews_collapse_items_to_one_row_per_person() {
        let mut cfg = crate::succession_ledger::SuccessionLedger::empty(ActorId([1u8; 32]));
        let person = ActorId([2u8; 32]);
        let bystander = ActorId([3u8; 32]);
        let first_event = ActorId([4u8; 32]);
        let second_event = ActorId([5u8; 32]);

        cfg.raise_member_reviews(
            [person, bystander],
            first_event,
            MemberUnattestedReason::CompromiseWindow,
        );
        cfg.raise_member_reviews(
            [person],
            second_event,
            MemberUnattestedReason::Other("unverifiable_statement".into()),
        );

        let reviews = cfg.open_member_reviews();
        assert_eq!(reviews.len(), 2, "one row per person, not per item");
        let subject = reviews
            .iter()
            .find(|r| r.person == person)
            .expect("the twice-raised person");
        assert_eq!(
            subject.reasons,
            vec![
                MemberUnattestedReason::CompromiseWindow,
                MemberUnattestedReason::Other("unverifiable_statement".into()),
            ],
            "their row lists every distinct reason, in first-raised order"
        );
    }

    /// The held-roster predicate answers the same question as the whole-config
    /// one, for the surfaces that paint from a cache — and it must agree with it
    /// on both sides, including after an adjudication drops the person from the
    /// projection.
    #[test]
    fn the_held_roster_predicate_agrees_with_the_config_one() {
        let mut cfg = crate::succession_ledger::SuccessionLedger::empty(ActorId([1u8; 32]));
        let person = ActorId([2u8; 32]);
        let stranger = ActorId([3u8; 32]);
        cfg.raise_member_reviews(
            [person],
            ActorId([4u8; 32]),
            MemberUnattestedReason::CompromiseWindow,
        );

        let roster = cfg.open_member_reviews();
        assert!(is_under_review(&roster, &person));
        assert_eq!(
            is_under_review(&roster, &person),
            cfg.member_is_unattested(&person)
        );
        assert!(!is_under_review(&roster, &stranger));
        assert_eq!(
            is_under_review(&roster, &stranger),
            cfg.member_is_unattested(&stranger)
        );

        cfg.decide_member_reviews_for(&person, UnattestedVerdict::Kept);
        let roster = cfg.open_member_reviews();
        assert!(
            !is_under_review(&roster, &person),
            "a re-read roster must drop an adjudicated person, or the mark never clears"
        );
        assert_eq!(
            is_under_review(&roster, &person),
            cfg.member_is_unattested(&person)
        );
    }

    #[test]
    fn is_under_review_hex_agrees_with_the_decoded_predicate_and_never_panics() {
        let flagged = ActorId([1u8; 32]);
        let clean = ActorId([2u8; 32]);
        let roster = vec![MemberReview {
            person: flagged,
            reasons: vec![MemberUnattestedReason::CompromiseWindow],
        }];
        assert!(is_under_review_hex(&roster, &flagged.to_hex()));
        assert!(!is_under_review_hex(&roster, &clean.to_hex()));
        // An undecodable hex string (never a real contact's id) must not panic.
        assert!(!is_under_review_hex(&roster, "not-hex"));
    }

    /// A person the app can no longer name still gets a named row — the rule
    /// that keeps an un-closable item from becoming an invisible one.
    #[test]
    fn a_row_names_an_unnameable_person_rather_than_rendering_blank() {
        let review = MemberReview {
            person: ActorId([2u8; 32]),
            reasons: vec![MemberUnattestedReason::CompromiseWindow],
        };

        let named = review_row_text(&review, Some("alice.example"));
        assert_eq!(
            named.who,
            LocalizedText::key("alice.example"),
            "a resolved handle is carried verbatim, never through a key"
        );

        let unnamed = review_row_text(&review, None);
        assert_eq!(
            unnamed.who,
            LocalizedText::key("settings.recovery_kit.review_unknown_person"),
            "an unnameable person falls back to the ratified wording; a blank \
             where the name goes is a row nobody can act on"
        );
    }

    /// Every reason reaches the row, in first-raised order — **including one
    /// this build cannot name**. Dropping it would hide a person the owner
    /// still has to decide about, which is the whole failure this surface
    /// exists to prevent.
    #[test]
    fn every_reason_renders_including_one_from_a_newer_build() {
        let review = MemberReview {
            person: ActorId([2u8; 32]),
            reasons: vec![
                MemberUnattestedReason::CompromiseWindow,
                MemberUnattestedReason::Other("from-a-newer-build".into()),
            ],
        };

        let parts = review_row_text(&review, Some("bob.example"));
        assert_eq!(
            parts.reasons,
            vec![
                LocalizedText::key("settings.recovery_kit.review_reason_compromise"),
                LocalizedText::key("settings.recovery_kit.review_reason_other"),
            ],
            "an unrecognised reason is carried, not filtered, and order is \
             first-raised"
        );
    }

    /// `reason_text` joins in raise order without dropping anything —
    /// including a reason this build cannot name.
    #[test]
    fn reason_text_joins_in_raise_order_without_dropping_anything() {
        let review = MemberReview {
            person: ActorId([4u8; 32]),
            reasons: vec![
                MemberUnattestedReason::CompromiseWindow,
                MemberUnattestedReason::Other("second".into()),
            ],
        };
        let parts = review_row_text(&review, None);

        let lookup = |key: &str| -> Option<&'static str> {
            match key {
                "settings.recovery_kit.review_reason_compromise" => Some("compromise window"),
                "settings.recovery_kit.review_reason_other" => Some("could not confirm"),
                _ => None,
            }
        };
        let joined = reason_text(&parts.reasons, lookup);
        assert_eq!(joined, "compromise window, could not confirm");
    }

    /// The parts compose into the shipped row template — the assertion that
    /// keys the lift to real i18n rather than to three strings that merely
    /// look plausible.
    #[test]
    fn the_parts_compose_into_the_shipped_row_template() {
        let review = MemberReview {
            person: ActorId([2u8; 32]),
            reasons: vec![MemberUnattestedReason::CompromiseWindow],
        };
        let parts = review_row_text(&review, None);

        let lookup = |key: &str| -> Option<&'static str> {
            match key {
                "settings.recovery_kit.review_row" => Some("{who} — {reason}"),
                "settings.recovery_kit.review_unknown_person" => {
                    Some("Someone no longer in any of your groups")
                }
                "settings.recovery_kit.review_reason_compromise" => {
                    Some("was in your groups before you recovered your account")
                }
                _ => None,
            }
        };

        let who = parts.who.resolve(lookup);
        let reason = reason_text(&parts.reasons, lookup);
        let row = LocalizedText::key_args(
            "settings.recovery_kit.review_row",
            [("who", who), ("reason", reason)],
        )
        .resolve(lookup);

        assert_eq!(
            row,
            "Someone no longer in any of your groups — was in your groups \
             before you recovered your account"
        );
    }

    /// A stand-in 64-hex actor id for a readable test name — the only author
    /// shape a row keeps ([`RefusedSchedulingChange::bound_to_budget`]).
    fn actor(name: &str) -> String {
        let mut id = hex::encode(name);
        id.truncate(64);
        format!("{id:0<64}")
    }

    /// A refused-change row for `author` on `uid`, refused at `at`.
    fn refusal(uid: &str, author: &str, at: i64) -> RefusedSchedulingChange {
        RefusedSchedulingChange {
            uid_hash: uid.to_string(),
            author: Some(actor(author)),
            author_home_nest_url: String::new(),
            sender_address: String::new(),
            method: "CANCEL".to_string(),
            reason: "not_the_organizer".to_string(),
            summary: "Kickoff".to_string(),
            first_refused_at: at,
            last_refused_at: at,
            occurrences: 0,
            dismissed_through: 0,
            extra: BTreeMap::new(),
        }
    }

    /// A refused mailed `REPLY` as the mail rail records it: no actor, the
    /// door-authenticated address in `sender_address`.
    fn mail_refusal(uid: &str, sender: &str, at: i64) -> RefusedSchedulingChange {
        RefusedSchedulingChange {
            author: None,
            sender_address: sender.to_string(),
            method: "REPLY".to_string(),
            reason: "not_the_attendee".to_string(),
            ..refusal(uid, "unused", at)
        }
    }

    /// A mail-rail row has no actor, so its door-authenticated address stands
    /// in for one: two different spoofers answering for the same event are two
    /// rows (neither hides the other), one spoofer's repeats collapse onto one
    /// counted row, and one spoofer's flood across many events is held to the
    /// per-author ceiling rather than crowding everyone else out.
    #[test]
    fn a_mail_rail_row_is_keyed_and_capped_by_its_authenticated_sender() {
        let mut cfg = RefusedSchedulingChanges::default();
        assert!(cfg.record(mail_refusal("e1", "mallory@x.test", 1)));
        assert!(cfg.record(mail_refusal("e1", "trudy@y.test", 2)));
        assert_eq!(cfg.rows.len(), 2, "two spoofers, two rows");
        assert!(cfg.record(mail_refusal("e1", "mallory@x.test", 3)));
        assert_eq!(cfg.rows.len(), 2, "a repeat is a count");
        let mallory = cfg
            .rows
            .iter()
            .find(|r| r.sender_address == "mallory@x.test")
            .expect("mallory's row");
        assert_eq!(mallory.occurrences, 2);
        assert_eq!(mallory.refused_party(), "mallory@x.test");
        assert_ne!(
            mallory.key(),
            mail_refusal("e1", "trudy@y.test", 0).key(),
            "the address is part of the key"
        );

        // mallory floods many events: held to the per-author ceiling.
        for i in 0..10 {
            cfg.record(mail_refusal(
                &format!("flood-{i}"),
                "mallory@x.test",
                100 + i,
            ));
        }
        let malloryish = cfg
            .rows
            .iter()
            .filter(|r| r.sender_address == "mallory@x.test")
            .count();
        assert_eq!(malloryish, MAX_REFUSED_CHANGES_PER_AUTHOR);
        assert!(
            cfg.rows.iter().any(|r| r.sender_address == "trudy@y.test"),
            "one sender's flood does not evict another's row"
        );
    }

    /// An attested actor wins the author slot: a sealed-rail row's key is
    /// unchanged by an address riding along, and an unstamped mail row (no
    /// actor, no address) names nobody.
    #[test]
    fn the_refused_party_is_the_actor_else_the_address_else_nobody() {
        let mut rail = refusal("e", "mallory", 1);
        let before = rail.key();
        rail.sender_address = "someone@x.test".into();
        assert_eq!(rail.key(), before);
        assert_eq!(mail_refusal("e", "", 1).refused_party(), "");
        let mut odd = mail_refusal("e", "a\u{1f}b@x.test", 1);
        odd.bound_to_budget();
        assert_eq!(odd.sender_address, "", "a non-printable address is cleared");
    }

    /// Repeat attempts on one event are ONE row with a count — the property
    /// that keeps a flood from consuming the whole surface, and that makes a
    /// re-drain of the same record land harmlessly (the inbound scheduling
    /// cursor is per-session, so a relaunch can walk a channel again).
    #[test]
    fn repeat_refusals_on_one_event_collapse_onto_one_counted_row() {
        let mut cfg = RefusedSchedulingChanges::default();

        assert!(cfg.record(refusal("uid-a", "mallory", 100)));
        assert!(cfg.record(refusal("uid-a", "mallory", 200)));
        assert!(cfg.record(refusal("uid-a", "mallory", 300)));

        assert_eq!(cfg.rows.len(), 1, "one key, one row");
        let row = &cfg.rows[0];
        assert_eq!(row.occurrences, 3);
        assert_eq!(row.first_refused_at, 100, "the first sighting is a fact");
        assert_eq!(row.last_refused_at, 300);

        // A different event from the same author is a different row; so is the
        // same event from someone else.
        cfg.record(refusal("uid-b", "mallory", 400));
        cfg.record(refusal("uid-a", "trudy", 500));
        assert_eq!(cfg.rows.len(), 3);
    }

    /// A dismissal closes the attempts the owner SAW, and a later attempt
    /// re-opens the row — the reason the row stores *what was dismissed*
    /// rather than a bare flag.
    #[test]
    fn a_dismissal_closes_what_was_seen_and_a_later_attempt_re_opens_it() {
        let mut cfg = RefusedSchedulingChanges::default();
        cfg.record(refusal("uid-a", "mallory", 100));
        let key = cfg.rows[0].key();

        assert_eq!(cfg.open().len(), 1);
        assert!(cfg.dismiss(&key));
        assert!(cfg.open().is_empty(), "a dismissed row leaves the surface");
        assert!(
            !cfg.dismiss(&key),
            "dismissing a closed row changes nothing"
        );
        assert_eq!(
            cfg.rows.len(),
            1,
            "the row STAYS at rest — deleting it would let a re-drain re-raise \
             what the owner closed"
        );

        cfg.record(refusal("uid-a", "mallory", 900));
        assert_eq!(
            cfg.open().len(),
            1,
            "a NEW attempt past the dismissed count is new information"
        );
    }

    /// One author cannot crowd the surface out. The cheapest flood in the
    /// design is a creating `REQUEST` with a spoofed ORGANIZER — its UID is the
    /// sender's to choose, so distinct keys are free to mint — and this is what
    /// bounds it.
    #[test]
    fn one_author_cannot_flood_another_authors_refusal_off_the_surface() {
        let mut cfg = RefusedSchedulingChanges::default();
        cfg.record(refusal("uid-victim", "trudy", 1));

        for i in 0..50 {
            cfg.record(refusal(&format!("flood-{i}"), "mallory", 10 + i));
        }

        let mallory = cfg
            .rows
            .iter()
            .filter(|r| r.author.as_deref() == Some(actor("mallory").as_str()))
            .count();
        assert_eq!(mallory, MAX_REFUSED_CHANGES_PER_AUTHOR);
        assert!(
            cfg.rows
                .iter()
                .any(|r| r.author.as_deref() == Some(actor("trudy").as_str())),
            "the earlier row from ANOTHER author survives the flood"
        );
        assert!(cfg.rows.len() <= MAX_REFUSED_SCHEDULING_CHANGES);
    }

    /// The global ceiling keeps the NEWEST rows — the opposite of the anchor
    /// vector's keep-the-oldest rule, and deliberately so: a notice surface
    /// that filled up permanently would go blind to every later attempt.
    #[test]
    fn the_global_ceiling_keeps_the_newest_attempts() {
        let mut cfg = RefusedSchedulingChanges::default();
        for i in 0..40i64 {
            // A fresh author each time, so the per-author ceiling never fires.
            cfg.record(refusal(&format!("uid-{i}"), &format!("author-{i}"), i));
        }
        assert_eq!(cfg.rows.len(), MAX_REFUSED_SCHEDULING_CHANGES);
        let oldest_kept = cfg
            .rows
            .iter()
            .map(|r| r.last_refused_at)
            .min()
            .expect("rows");
        assert_eq!(oldest_kept, 40 - MAX_REFUSED_SCHEDULING_CHANGES as i64);
    }

    /// A stranger's refusal cannot carry more than the row's byte budget. The
    /// `summary` of a refused creating `REQUEST` is the message's own `SUMMARY`
    /// and the `author` is whatever the sender's home nest attests, so both
    /// arrive as long as the sender likes; unbounded, one row pins the whole
    /// refused-change row against the plane's per-entry byte cap.
    #[test]
    fn a_refused_row_keeps_no_more_than_its_byte_budget_whatever_the_sender_supplies() {
        let mut cfg = RefusedSchedulingChanges::default();
        let mut hostile = refusal("uid-a", "mallory", 100);
        // Two-byte chars, so a cut at the ceiling lands mid-character.
        hostile.summary = "é".repeat(700_000);
        hostile.author = Some("x".repeat(200_000));
        hostile.method = "M".repeat(100_000);
        hostile.reason = "r".repeat(100_000);
        hostile.uid_hash = "u".repeat(100_000);
        hostile.author_home_nest_url = "h".repeat(100_000);
        hostile.sender_address = format!("{}@example.test", "a".repeat(100_000));
        assert!(cfg.record(hostile));

        let row = &cfg.rows[0];
        assert!(row.summary.len() <= MAX_REFUSED_CHANGE_SUMMARY_BYTES);
        assert!(
            row.summary.len() > MAX_REFUSED_CHANGE_SUMMARY_BYTES - 2
                && row.summary.starts_with('é'),
            "the title survives as a readable prefix, cut on a char boundary"
        );
        assert_eq!(row.author, None, "a non-hex attestation names nobody");
        for token in [&row.uid_hash, &row.method, &row.reason] {
            assert!(token.len() <= MAX_REFUSED_CHANGE_TOKEN_BYTES);
        }
        assert!(row.author_home_nest_url.len() <= MAX_REFUSED_CHANGE_URL_BYTES);
        assert_eq!(
            row.sender_address, "",
            "an over-long address is cleared, never cut: a prefix names nobody"
        );

        // A repeat attempt adopts the incoming title — through the same bound.
        let mut repeat = refusal("uid-b", "trudy", 200);
        cfg.record(repeat.clone());
        repeat.summary = "é".repeat(700_000);
        repeat.last_refused_at = 300;
        cfg.record(repeat);
        assert!(
            cfg.rows
                .iter()
                .all(|r| r.summary.len() <= MAX_REFUSED_CHANGE_SUMMARY_BYTES),
            "a repeat's title is bounded like a new row's"
        );

        // The whole list, filled to both ceilings with rows at every field's
        // limit, stays inside the stated plane budget.
        let mut full = RefusedSchedulingChanges::default();
        let empty = crate::encoding::canonical_encode(&full)
            .expect("encode")
            .len();
        for i in 0..MAX_REFUSED_SCHEDULING_CHANGES as i64 {
            let mut r = refusal("uid", &format!("author-{i}"), i);
            r.uid_hash = format!("{i}{}", "u".repeat(100_000));
            r.summary = "s".repeat(100_000);
            r.method = "M".repeat(100_000);
            r.reason = "r".repeat(100_000);
            r.author_home_nest_url = "h".repeat(100_000);
            // At its ceiling exactly, so it survives the bound and counts.
            let at = format!("@{i}.test");
            r.sender_address = format!(
                "{}{at}",
                "a".repeat(MAX_REFUSED_CHANGE_ADDRESS_BYTES - at.len())
            );
            full.record(r);
        }
        assert_eq!(full.rows.len(), MAX_REFUSED_SCHEDULING_CHANGES);
        let grown = crate::encoding::canonical_encode(&full)
            .expect("encode")
            .len()
            - empty;
        assert!(
            grown <= REFUSED_SCHEDULING_CHANGES_BYTE_BUDGET,
            "{grown} B of refused rows exceeds the {REFUSED_SCHEDULING_CHANGES_BYTE_BUDGET} B budget"
        );
    }

    /// A row that is ALREADY over budget at rest — planted before the bound
    /// existed — is trimmed the next time the list is capped, so the plane
    /// heals without any gesture from the owner. Two planted rows whose
    /// authors both fall to `None` collapse onto one key rather than riding
    /// on as duplicates.
    #[test]
    fn a_planted_over_budget_row_is_trimmed_when_the_list_is_next_capped() {
        let mut planted = refusal("uid-a", "mallory", 100);
        planted.summary = "s".repeat(1_300_000);
        planted.author = Some("not-hex".repeat(10_000));
        planted.occurrences = 2;
        let mut twin = planted.clone();
        twin.author = Some("also-not-hex".into());
        twin.occurrences = 5;
        twin.last_refused_at = 200;
        let mut rows = vec![planted, twin];

        cap_refused_scheduling_changes(&mut rows);

        assert_eq!(rows.len(), 1, "one key after the authors are dropped");
        let row = &rows[0];
        assert_eq!(row.author, None);
        assert!(row.summary.len() <= MAX_REFUSED_CHANGE_SUMMARY_BYTES);
        assert_eq!(
            row.occurrences, 5,
            "the fold is the merge's: max, never sum"
        );
        assert_eq!(row.last_refused_at, 200);
        assert_eq!(row.first_refused_at, 100);
    }

    /// A verdict closes the person's whole backlog, and a **re-run of the same
    /// sweep does not re-ask** — while a *later* succession legitimately does.
    ///
    /// That pair is the `(person, raising event)` keying's entire purpose, and
    /// the re-run half is why an adjudicated item stays at rest instead of being
    /// deleted: a presence-means-open store cannot tell "answered" from "never
    /// asked", so the retry silently undoes the user's work.
    #[test]
    fn a_decision_survives_a_re_run_but_a_later_succession_re_raises() {
        let mut cfg = crate::succession_ledger::SuccessionLedger::empty(ActorId([1u8; 32]));
        let person = ActorId([2u8; 32]);
        let first_event = ActorId([4u8; 32]);
        let second_event = ActorId([5u8; 32]);

        cfg.raise_member_reviews(
            [person],
            first_event,
            MemberUnattestedReason::CompromiseWindow,
        );
        assert!(cfg.member_is_unattested(&person));
        assert!(cfg.decide_member_reviews_for(&person, UnattestedVerdict::Kept));
        assert!(!cfg.member_is_unattested(&person), "the Keep closed it");

        // The same sweep runs again — a resume, or the "finish moving your
        // groups" retry.
        cfg.raise_member_reviews(
            [person],
            first_event,
            MemberUnattestedReason::CompromiseWindow,
        );
        assert!(
            !cfg.member_is_unattested(&person),
            "a re-run of the SAME raising event must not re-ask a question the \
             owner already answered"
        );

        // A second compromise is a different event, and a Keep about the first
        // window cannot vouch for them across it.
        cfg.raise_member_reviews(
            [person],
            second_event,
            MemberUnattestedReason::CompromiseWindow,
        );
        assert!(
            cfg.member_is_unattested(&person),
            "a LATER succession legitimately re-raises the same person"
        );
    }

    // ── the filter plane's raise + reader (the fourth adjudication encoding) ──

    /// The plane's `(filter_id, raising event)` keying, in the pair that is its
    /// entire purpose: a re-run of the **same** succession must not re-ask a
    /// question the owner already answered, while a **later** succession
    /// legitimately re-raises the same rule.
    ///
    /// The re-run half is why an adjudicated mark stays at rest rather than
    /// being deleted — a presence-means-open store cannot tell "answered" from
    /// "never asked", so the retry silently undoes the owner's work.
    #[test]
    fn a_filter_decision_survives_a_re_run_but_a_later_succession_re_raises() {
        let mut cfg = crate::succession_ledger::SuccessionLedger::empty(ActorId([1u8; 32]));
        let first_event = ActorId([4u8; 32]);
        let second_event = ActorId([5u8; 32]);

        assert_eq!(cfg.raise_filter_marks([7, 8], first_event), 2);
        assert!(cfg.filter_is_unattested(7));
        assert!(cfg.decide_filter_marks_for(7, UnattestedVerdict::Kept));
        assert!(!cfg.filter_is_unattested(7), "the Keep closed it");
        assert!(cfg.filter_is_unattested(8), "its neighbour is untouched");

        assert_eq!(
            cfg.raise_filter_marks([7, 8], first_event),
            0,
            "a re-run of the SAME raising event adds nothing, so the caller can \
             skip a re-seal that would store identical bytes"
        );
        assert!(
            !cfg.filter_is_unattested(7),
            "a re-run of the SAME raising event must not re-ask a question the \
             owner already answered"
        );

        assert_eq!(cfg.raise_filter_marks([7], second_event), 1);
        assert!(
            cfg.filter_is_unattested(7),
            "a LATER succession legitimately re-raises the same rule"
        );
    }

    /// A *Remove* records its verdict on the mark and leaves the mark at rest,
    /// exactly like a *Keep*. **This plane never enforces the removal** — the
    /// rule itself lives in nest-side SQL and is deleted through the filter
    /// list's own deletion, so a caller that recorded `Removed` and skipped that
    /// deletion would leave an armed rule running under a clean-looking list.
    #[test]
    fn a_removed_filter_mark_is_recorded_and_kept_at_rest() {
        let mut cfg = crate::succession_ledger::SuccessionLedger::empty(ActorId([1u8; 32]));
        cfg.raise_filter_marks([7], ActorId([4u8; 32]));

        assert!(cfg.decide_filter_marks_for(7, UnattestedVerdict::Removed));
        assert_eq!(
            cfg.unattested_filter_marks.len(),
            1,
            "the mark survives its own Remove — deleting it would make an \
             answered question indistinguishable from one never raised"
        );
        assert_eq!(
            cfg.unattested_filter_marks[0].verdict,
            UnattestedVerdict::Removed
        );
        assert!(!cfg.filter_is_unattested(7));
        assert!(
            !cfg.decide_filter_marks_for(7, UnattestedVerdict::Kept),
            "nothing was open, so the second gesture is a no-op rather than an \
             overwrite of the owner's earlier decision"
        );
    }

    /// A verdict this build cannot name renders **as still open** — the
    /// fail-visible asymmetry every adjudication plane shares, and the only
    /// mixed-fleet protection this one has (there is no row stamp on this plane).
    #[test]
    fn an_unnameable_filter_verdict_renders_as_still_open() {
        let mut cfg = crate::succession_ledger::SuccessionLedger::empty(ActorId([1u8; 32]));
        cfg.unattested_filter_marks = vec![FilterUnattestedMark {
            filter_id: 7,
            predecessor: ActorId([4u8; 32]),
            verdict: UnattestedVerdict::Other("quarantined".into()),
        }];

        assert!(
            cfg.filter_is_unattested(7),
            "re-asking is harmless; silently hiding a flagged rule is the failure \
             the surface exists to prevent"
        );
        assert_eq!(cfg.open_filter_reviews(), vec![7]);
    }

    /// The review projection is a **function of its contents**: sorted, deduped
    /// across raising events, and carrying only the rows still open. A surface
    /// that painted it raw would show one rule twice after a second succession.
    #[test]
    fn the_open_filter_review_projection_is_sorted_and_deduped() {
        let mut cfg = crate::succession_ledger::SuccessionLedger::empty(ActorId([1u8; 32]));
        cfg.raise_filter_marks([9, 7], ActorId([4u8; 32]));
        cfg.raise_filter_marks([7], ActorId([5u8; 32]));

        assert_eq!(
            cfg.open_filter_reviews(),
            vec![7, 9],
            "one rule raised by two events is one row to review, in id order"
        );

        cfg.decide_filter_marks_for(7, UnattestedVerdict::Kept);
        assert_eq!(
            cfg.open_filter_reviews(),
            vec![9],
            "a decision closes every open mark on the rule, across both events"
        );
    }

    // ── the destination plane's reader (the third adjudication encoding) ──

    fn raised_row(id: &str) -> BackupDestination {
        BackupDestination {
            destination_id: id.to_string(),
            ..Default::default()
        }
    }

    /// The verdict plane decides: a decided mark answers the row, an open one
    /// raises it.
    #[test]
    fn a_decided_mark_answers_the_row_and_an_open_one_raises_it() {
        let event = ActorId([4u8; 32]);
        let row = raised_row("d1");
        let marks = vec![DestinationUnattestedMark {
            destination_id: "d1".into(),
            predecessor: event,
            verdict: UnattestedVerdict::Kept,
        }];

        assert!(!DestinationUnattestedMark::row_is_raised(&marks, &row));
        assert!(
            DestinationUnattestedMark::row_is_raised(
                &[DestinationUnattestedMark {
                    verdict: UnattestedVerdict::Open,
                    ..marks[0].clone()
                }],
                &row
            ),
            "...and an open mark raises it"
        );
    }

    /// A destination row still carrying the pre-sweep
    /// `unattested_from_predecessor` stamp does **not** raise. The stamp was a
    /// downgrade mirror written only so a build older than the mark plane could
    /// render the review; the compat-remnant sweep (program 4,
    /// `version-compatibility.md` § Dimension 2) retired it and the
    /// `row_is_raised` fallback that read it, so the key is ignored on decode
    /// and only the verdict plane raises a row.
    #[test]
    fn a_pre_sweep_row_stamp_is_ignored_and_does_not_raise() {
        let row = raised_row("d1");
        let mut value: fauna_cbor::Value =
            canonical_decode(&canonical_encode(&row).unwrap()).unwrap();
        let fauna_cbor::Value::Map(map) = &mut value else {
            panic!("a destination row encodes as a map");
        };
        map.insert(
            "unattested_from_predecessor".to_string(),
            fauna_cbor::Value::Bytes(vec![4u8; 32]),
        );
        let decoded: BackupDestination =
            canonical_decode(&canonical_encode(&value).unwrap()).unwrap();

        assert_eq!(decoded, row, "the retired stamp key is dropped on decode");
        assert!(
            !DestinationUnattestedMark::row_is_raised(&[], &decoded),
            "only an open mark raises a row; the retired stamp never does"
        );
    }

    /// The head cache never rewinds — the one rule that gives it any value,
    /// since a remembered head exists to refuse a chain that rewrites or
    /// truncates what was already seen.
    #[test]
    fn remembering_a_chain_head_is_monotonic() {
        let mut cfg = PeerAnchors::default();
        let peer = ActorId([2u8; 32]);

        assert!(
            cfg.known_chain_head(&peer).is_none(),
            "TOFU before any read"
        );
        assert!(cfg.remember_chain_head(peer, crate::recovery::ChainHead::new([0xAA; 32], 4)));
        assert!(
            !cfg.remember_chain_head(peer, crate::recovery::ChainHead::new([0xBB; 32], 2)),
            "a lower seq is refused, and reports no change so no re-seal is spent"
        );
        let held = cfg.known_chain_head(&peer).expect("head");
        assert_eq!((held.seq, held.recovery_pubkey), (4, [0xAA; 32]));

        assert!(cfg.remember_chain_head(peer, crate::recovery::ChainHead::new([0xCC; 32], 9)));
        let held = cfg.known_chain_head(&peer).expect("head");
        assert_eq!(
            (held.seq, held.recovery_pubkey),
            (9, [0xCC; 32]),
            "a genuine advance moves the seq AND the key it registered"
        );
    }

    /// The direct pin on the **production** type's catch-all: an entry key this
    /// build has no named field for — what the *next* custody field will look like
    /// to *this* build — survives decode → re-encode on `DeploymentSeedEntry`
    /// itself.
    ///
    /// ⚠ This exists because the sibling pin above does **not** cover it, which
    /// only mutation grading showed: that test routes the marker through a
    /// *stand-in* for the older client, so the catch-all it actually exercises is
    /// the stand-in's. Deleting `#[serde(flatten)]` from the real struct left it
    /// green. The two pins are complementary — that one proves the older build
    /// re-seals faithfully, this one proves the shape we ship is the one that makes
    /// the next field's older build able to.
    #[test]
    fn an_unknown_entry_key_survives_this_builds_re_seal() {
        #[derive(serde::Serialize)]
        struct FutureEntry {
            #[serde(with = "serde_bytes")]
            nest_actor_id: [u8; 32],
            #[serde(with = "serde_bytes")]
            seed: [u8; 32],
            /// A custody field some later client added.
            retired_at: u64,
        }
        let bytes = canonical_encode(&FutureEntry {
            nest_actor_id: [0x33u8; 32],
            seed: [0x33u8; 32],
            retired_at: 1_700_000_000,
        })
        .unwrap();

        let current: DeploymentSeedEntry = canonical_decode(&bytes).unwrap();
        assert_eq!(
            current.extra.get("retired_at"),
            Some(&fauna_cbor::Value::Integer(1_700_000_000)),
            "an unknown entry key must land in the entry's own catch-all"
        );
        assert_eq!(
            canonical_encode(&current).unwrap(),
            bytes,
            "re-seal must not drop a custody field this build does not know"
        );
    }

    #[test]
    fn tier_period_debug_is_redacted() {
        // The subscriptions twin of `content_key_generation_debug_is_redacted`:
        // a stray `{:?}` must never print the raw broadcast period key.
        let p = TierPeriod {
            version: 1,
            key: [0xABu8; 32].into(),
            rotated_at: 5,
            minted_by: None,
        };
        let rendered = format!("{p:?}");
        assert!(!rendered.contains("171"), "Debug leaked the period key");
        assert!(
            rendered.contains("version: 1"),
            "diagnostics survive: {rendered}"
        );
    }

    #[test]
    fn tier_period_wire_is_bare_array_compatible() {
        // At-rest compat pin (account-plane custody, alpha no-data-loss): the
        // period must encode byte-for-byte as if `key` were a bare `[u8; 32]`.
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Mirror {
            version: u64,
            #[serde(with = "serde_bytes")]
            key: [u8; 32],
            rotated_at: u64,
        }
        let p = TierPeriod {
            version: 3,
            key: [0x5Au8; 32].into(),
            rotated_at: 9,
            minted_by: None,
        };
        let bytes = canonical_encode(&p).unwrap();
        let mirror: Mirror = canonical_decode(&bytes).unwrap();
        assert_eq!(mirror.key, [0x5Au8; 32]);
        let mirror_bytes = canonical_encode(&mirror).unwrap();
        assert_eq!(
            bytes, mirror_bytes,
            "wire identical to the bare-array shape"
        );
        let back: TierPeriod = canonical_decode(&mirror_bytes).unwrap();
        assert_eq!(back, p);
    }

    /// The default subscriptions record holds no tier.
    #[test]
    fn subscriptions_config_default_has_no_tiers() {
        let decoded = SubscriptionsConfig::default();
        assert!(decoded.tiers.is_empty());
    }

    fn tier(name: &str, version: u64, key: u8, rotated_at: u64) -> TierPeriodKeys {
        TierPeriodKeys {
            tier_name: name.into(),
            current: TierPeriod {
                version,
                key: [key; 32].into(),
                rotated_at,
                minted_by: None,
            },
            prior: vec![],
        }
    }

    #[test]
    fn subscriptions_merge_unions_concurrent_tiers_no_loss() {
        // Device A created "gold", device B independently created "silver".
        // Whole-record latest-wins would drop one irrecoverable key; the
        // per-tier union must keep BOTH.
        let a = SubscriptionsConfig {
            tiers: vec![tier("gold", 1, 0x11, 1000)],
            ..Default::default()
        };
        let b = SubscriptionsConfig {
            tiers: vec![tier("silver", 1, 0x22, 2000)],
            ..Default::default()
        };
        let merged = a.merge(&b);
        let names: Vec<&str> = merged.tiers.iter().map(|t| t.tier_name.as_str()).collect();
        assert!(names.contains(&"gold") && names.contains(&"silver"));
        assert_eq!(merged.tiers.len(), 2);
        // Commutative.
        assert_eq!(merged, b.merge(&a));
    }

    #[test]
    fn subscriptions_merge_keeps_higher_version_and_retains_all_periods() {
        // Both have "gold"; A rotated it to v2 (prior v1), B is still v1.
        let mut a_gold = tier("gold", 2, 0x22, 2000);
        a_gold.prior = vec![TierPeriod {
            version: 1,
            key: [0x11; 32].into(),
            rotated_at: 1000,
            minted_by: None,
        }];
        let a = SubscriptionsConfig {
            tiers: vec![a_gold],
            ..Default::default()
        };
        let b = SubscriptionsConfig {
            tiers: vec![tier("gold", 1, 0x11, 1000)],
            ..Default::default()
        };
        let merged = a.merge(&b);
        assert_eq!(merged.tiers.len(), 1);
        let g = &merged.tiers[0];
        assert_eq!(g.current.version, 2, "higher version wins as current");
        // v1 retained for archival; not duplicated despite being on both sides.
        assert_eq!(g.prior.len(), 1);
        assert_eq!(g.prior[0].version, 1);
        assert_eq!(merged, b.merge(&a), "commutative");
    }

    #[test]
    fn subscriptions_merge_conflict_retains_loser_key() {
        // Concurrent rotation: both reached v2 but with DIFFERENT keys. The
        // loser's key must NOT be lost (irrecoverable) — it lands in prior.
        let a = SubscriptionsConfig {
            tiers: vec![tier("gold", 2, 0xAA, 2000)],
            ..Default::default()
        };
        let b = SubscriptionsConfig {
            tiers: vec![tier("gold", 2, 0xBB, 2000)],
            ..Default::default()
        };
        let merged = a.merge(&b);
        let g = &merged.tiers[0];
        // Deterministic winner (larger key bytes); loser retained in prior.
        assert_eq!(g.current.key, [0xBB; 32]);
        assert_eq!(g.prior.len(), 1);
        assert_eq!(g.prior[0].key, [0xAA; 32]);
        assert_eq!(merged, b.merge(&a), "commutative even under conflict");
    }

    fn removal(tier: &str, sub: u8, version: u64, key: u8, rotated_at: u64) -> PendingRemoval {
        PendingRemoval {
            tier_name: tier.into(),
            subscriber_id: ActorId([sub; 32]),
            new_period: TierPeriod {
                version,
                key: [key; 32].into(),
                rotated_at,
                minted_by: None,
            },
        }
    }

    #[test]
    fn subscriptions_merge_unions_pending_removals_no_loss() {
        // Each device staged a different in-flight removal (each holds an
        // irrecoverable fresh key). The union must keep BOTH; an exact
        // duplicate on both sides collapses to one.
        let shared = removal("gold", 0x01, 2, 0xAA, 2000);
        let a = SubscriptionsConfig {
            tiers: vec![],
            pending_removals: vec![shared.clone(), removal("gold", 0x02, 2, 0xBB, 2100)],
        };
        let b = SubscriptionsConfig {
            tiers: vec![],
            pending_removals: vec![shared.clone(), removal("silver", 0x03, 2, 0xCC, 2200)],
        };
        let merged = a.merge(&b);
        assert_eq!(
            merged.pending_removals.len(),
            3,
            "shared collapses; 2 unique kept"
        );
        assert!(merged.pending_removals.contains(&shared));
        assert!(
            merged
                .pending_removals
                .contains(&removal("gold", 0x02, 2, 0xBB, 2100))
        );
        assert!(
            merged
                .pending_removals
                .contains(&removal("silver", 0x03, 2, 0xCC, 2200))
        );
        // A side with no sentinel must not drop the other's staged key. The
        // merge emits canonical (hash) order, not `a`'s construction order.
        let empty = SubscriptionsConfig::default();
        let mut canonical = a.pending_removals.clone();
        canonical.sort_by_cached_key(crate::encoding::canonical_tiebreak_key);
        assert_eq!(empty.merge(&a).pending_removals, canonical);
        assert_eq!(a.merge(&empty).pending_removals, canonical);
    }

    #[test]
    fn folders_merge_unions_sets_and_keeps_all_generations() {
        use crate::folder_keys::FolderContentKeys;
        // Device A has set X at gen 2 + set Y; device B has set X at gen 1 only.
        // Merge keeps both sets; set X keeps gen 2 current + retains gen 1.
        let mut x_a = FolderContentKeys::genesis([1u8; 32], 1000);
        x_a.rotate([2u8; 32], 2000);
        let x_b = FolderContentKeys::genesis([1u8; 32], 1000);
        let y = FolderContentKeys::genesis([7u8; 32], 1500);

        let a = FoldersConfig {
            sets: vec![
                FolderKeyCustody {
                    channel_id: Some([0xAA; 32]),
                    keys: Some(x_a),
                    ..Default::default()
                },
                FolderKeyCustody {
                    channel_id: Some([0xCC; 32]),
                    keys: Some(y.clone()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let b = FoldersConfig {
            sets: vec![FolderKeyCustody {
                channel_id: Some([0xAA; 32]),
                keys: Some(x_b),
                ..Default::default()
            }],
            ..Default::default()
        };

        let merged = a.merge(&b);
        assert_eq!(merged.sets.len(), 2, "both distinct sets kept");
        let x = merged
            .sets
            .iter()
            .find(|s| s.channel_id == Some([0xAA; 32]))
            .unwrap();
        let x = x.keys.as_ref().unwrap();
        assert_eq!(x.current_version(), 2);
        assert_eq!(x.key_for(1), Some(&[1u8; 32]));
        assert_eq!(x.key_for(2), Some(&[2u8; 32]));
        // Commutative.
        assert_eq!(b.merge(&a), merged);
    }

    /// The custody entry shape before the set-nonce ruling — the two
    /// required fields only. A blob written by it must still decode.
    #[derive(Serialize)]
    struct PreNonceFolderKeyCustody {
        #[serde(with = "serde_bytes")]
        channel_id: [u8; 32],
        keys: crate::folder_keys::FolderContentKeys,
    }

    #[test]
    fn folder_custody_decodes_a_pre_nonce_entry_and_encodes_some_identically() {
        use crate::folder_keys::FolderContentKeys;
        let keys = FolderContentKeys::genesis([3u8; 32], 1_000);
        let old = canonical_encode(&PreNonceFolderKeyCustody {
            channel_id: [0xAB; 32],
            keys: keys.clone(),
        })
        .unwrap();
        let decoded: FolderKeyCustody = canonical_decode(&old).unwrap();
        assert_eq!(decoded.channel_id, Some([0xAB; 32]));
        assert_eq!(decoded.keys, Some(keys.clone()));
        assert_eq!(decoded.set_nonce, None);
        assert!(decoded.is_live());
        // `Some` encodes exactly as the required field did.
        let re = canonical_encode(&FolderKeyCustody {
            channel_id: Some([0xAB; 32]),
            keys: Some(keys),
            ..Default::default()
        })
        .unwrap();
        let old_hex = hex::encode(&old);
        let re_hex = hex::encode(&re);
        let chan =
            hex::encode(canonical_encode(&serde_bytes::ByteArray::new([0xABu8; 32])).unwrap());
        assert!(old_hex.contains(&chan) && re_hex.contains(&chan));
    }

    #[test]
    fn folder_custody_keyless_entry_round_trips() {
        let entry = FolderKeyCustody {
            set_nonce: Some([9u8; 32]),
            name: Some("docs".into()),
            created_at: 5_000,
            ..Default::default()
        };
        let bytes = canonical_encode(&entry).unwrap();
        let back: FolderKeyCustody = canonical_decode(&bytes).unwrap();
        assert_eq!(back, entry);
        assert_eq!(back.channel_id, None);
        assert_eq!(back.keys, None);
    }

    fn nonce_entry(nonce: u8, name: &str, created_at: u64) -> FolderKeyCustody {
        FolderKeyCustody {
            set_nonce: Some([nonce; 32]),
            name: Some(name.into()),
            created_at,
            ..Default::default()
        }
    }

    #[test]
    fn folders_merge_unions_sets_by_nonce_and_joins_each_field() {
        use crate::folder_keys::{FolderContentKeys, serve_custody_channel_id};
        let served = serve_custody_channel_id("docs");
        let bound = [0xBB; 32];
        let mut keys_a = FolderContentKeys::genesis([1u8; 32], 1_000);
        keys_a.rotate([2u8; 32], 2_000);
        let keys_b = FolderContentKeys::genesis([1u8; 32], 1_000);

        // Device A: served, rotated, then retired at 9_000.
        let a_entry = FolderKeyCustody {
            channel_id: Some(served),
            keys: Some(keys_a),
            retired_at: Some(9_000),
            ..nonce_entry(0x11, "docs", 100)
        };
        // Device B: the same set bound (the pseudo→real move), retired later.
        let b_entry = FolderKeyCustody {
            channel_id: Some(bound),
            keys: Some(keys_b),
            retired_at: Some(9_500),
            ..nonce_entry(0x11, "docs", 100)
        };
        // A keyless live entry for a different set, same name (a re-create).
        let other = nonce_entry(0x22, "docs", 200);
        let a = FoldersConfig {
            sets: vec![a_entry, other.clone()],
            ..Default::default()
        };
        let b = FoldersConfig {
            sets: vec![b_entry],
            ..Default::default()
        };
        let merged = a.merge(&b);
        assert_eq!(merged.sets.len(), 2, "one entry per nonce");
        let x = merged
            .sets
            .iter()
            .find(|s| s.set_nonce == Some([0x11; 32]))
            .unwrap();
        assert_eq!(x.channel_id, Some(bound), "bound dominates served");
        assert_eq!(x.retired_at, Some(9_500), "the later tombstone");
        assert_eq!(x.keys.as_ref().unwrap().current_version(), 2, "keys CRDT");
        assert_eq!(x.name.as_deref(), Some("docs"));
        assert!(merged.sets.contains(&other), "the re-created set untouched");
        // Commutative + idempotent.
        assert_eq!(b.merge(&a), merged);
        assert_eq!(merged.merge(&merged), merged);
        assert_eq!(merged.merge(&a), merged);
    }

    #[test]
    fn folders_merge_keeps_a_tombstone_over_a_stale_live_copy() {
        let live = nonce_entry(0x33, "photos", 10);
        let retired = FolderKeyCustody {
            retired_at: Some(50),
            ..live.clone()
        };
        let stale = FoldersConfig {
            sets: vec![live],
            ..Default::default()
        };
        let fresh = FoldersConfig {
            sets: vec![retired.clone()],
            ..Default::default()
        };
        assert_eq!(stale.merge(&fresh).sets, vec![retired.clone()]);
        assert_eq!(fresh.merge(&stale).sets, vec![retired]);
    }

    /// Ruling (7)(b)(ii) rule (1): the serve window is two stamps that each
    /// join to the later, so a stale copy of either flip cannot undo the
    /// later one — on a store that only ever joins.
    #[test]
    fn folders_merge_keeps_the_later_serve_flip_over_a_stale_copy() {
        let never = nonce_entry(0x41, "photos", 10);
        let one = |e: &FolderKeyCustody| FoldersConfig {
            sets: vec![e.clone()],
            ..Default::default()
        };
        assert!(!never.is_served(), "a stamp-less entry is not served");
        let mut served = never.clone();
        served.serve_on(100);
        assert!(served.is_served());
        let mut unserved = served.clone();
        unserved.serve_off(200);
        assert!(!unserved.is_served());
        // A stale served copy cannot undo the serve-off, either way round.
        let joined = one(&served).merge(&one(&unserved));
        assert_eq!(joined, one(&unserved).merge(&one(&served)));
        assert!(!joined.sets[0].is_served(), "the stale serve-on loses");
        // And a stale un-served copy cannot undo a later re-serve.
        let mut reserved = unserved.clone();
        reserved.serve_on(300);
        let joined = one(&unserved).merge(&one(&reserved));
        assert_eq!(joined, one(&reserved).merge(&one(&unserved)));
        assert!(joined.sets[0].is_served(), "the stale serve-off loses");
        assert_eq!(
            joined.merge(&one(&never)),
            joined,
            "a bare copy moves nothing"
        );
    }

    /// A clock that stepped back between two gestures still orders them: each
    /// flip stamps strictly above the other's stamp.
    #[test]
    fn serve_flips_order_across_a_clock_stepped_back() {
        let mut entry = nonce_entry(0x42, "photos", 10);
        entry.serve_on(1_000);
        entry.serve_off(5);
        assert_eq!(entry.unserved_at, Some(1_001));
        assert!(!entry.is_served());
        entry.serve_on(7);
        assert_eq!(entry.served_at, Some(1_002));
        assert!(entry.is_served());
        // A repeated serve-on never lowers the stamp it carries.
        entry.serve_on(9);
        assert_eq!(entry.served_at, Some(1_002));
    }

    /// A tie reads NOT served — the pair's safe side, where the retirement
    /// pair's tie reads live.
    #[test]
    fn a_serve_stamp_tie_reads_not_served() {
        let tie = FolderKeyCustody {
            served_at: Some(50),
            unserved_at: Some(50),
            ..nonce_entry(0x43, "photos", 10)
        };
        assert!(!tie.is_served());
        let off_only = FolderKeyCustody {
            unserved_at: Some(50),
            ..nonce_entry(0x44, "photos", 10)
        };
        assert!(!off_only.is_served());
    }

    /// A delete retires the entry and writes no serve-off: the tombstone of a
    /// set deleted while served never reads served, and a lifted one does.
    #[test]
    fn a_retired_entry_never_reads_served() {
        let mut entry = nonce_entry(0x45, "photos", 10);
        entry.serve_on(100);
        entry.retire(200);
        assert!(!entry.is_served(), "a tombstone is never served");
        entry.lifted_at = entry.retired_at;
        assert!(entry.is_served(), "the refused delete's lift restores it");
    }

    /// At rest additive: a never-served entry keeps the bytes it always had,
    /// and a stamped one round-trips.
    #[test]
    fn serve_stamps_are_emitted_only_when_set() {
        let bare = nonce_entry(0x46, "photos", 10);
        let value: serde_json::Value = serde_json::to_value(&bare).unwrap();
        assert!(value.get("served_at").is_none());
        assert!(value.get("unserved_at").is_none());
        let mut served = bare.clone();
        served.serve_on(100);
        let bytes = crate::encoding::canonical_encode(&served).unwrap();
        let back: FolderKeyCustody = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(back, served);
    }

    /// Ruling (l)(v): a refused delete's lift names the stamp it lifts, so it
    /// survives a stale copy of the retirement, and a LATER delete's tombstone
    /// survives the earlier lift — on a store that only ever joins.
    #[test]
    fn folders_merge_lifts_a_retirement_by_its_stamp_and_a_later_delete_outlives_the_lift() {
        let live = nonce_entry(0x35, "photos", 10);
        let retired = FolderKeyCustody {
            retired_at: Some(50),
            ..live.clone()
        };
        let lifted = FolderKeyCustody {
            lifted_at: Some(50),
            ..retired.clone()
        };
        let one = |e: &FolderKeyCustody| FoldersConfig {
            sets: vec![e.clone()],
            ..Default::default()
        };
        assert!(!retired.is_live());
        assert!(lifted.is_live(), "the lift names the retirement's stamp");
        let joined = one(&retired).merge(&one(&lifted));
        assert_eq!(joined, one(&lifted).merge(&one(&retired)));
        assert!(
            joined.sets[0].is_live(),
            "a stale retirement cannot undo the lift"
        );
        let deleted_again = FolderKeyCustody {
            retired_at: Some(80),
            ..live
        };
        let after = joined.merge(&one(&deleted_again));
        assert!(!after.sets[0].is_live(), "the later delete stands");
        assert_eq!(after, one(&deleted_again).merge(&joined));
    }

    #[test]
    fn folders_merge_keeps_a_nonce_less_entry_keyed_by_channel() {
        let nonce_less = FolderKeyCustody {
            channel_id: Some([0x44; 32]),
            keys: Some(crate::folder_keys::FolderContentKeys::genesis([4u8; 32], 1)),
            ..Default::default()
        };
        let cfg = FoldersConfig {
            sets: vec![nonce_less.clone()],
            ..Default::default()
        };
        assert_eq!(cfg.merge(&cfg).sets, vec![nonce_less]);
    }

    #[test]
    fn folders_merge_unions_pending_removals_no_loss() {
        use crate::folder_keys::ContentKeyGeneration;
        let staged = |chan: u8, member: u8, ver: u64, key: u8, at: u64| FolderPendingRemoval {
            channel_id: [chan; 32],
            name: format!("set-{chan}"),
            removed_member: ActorId([member; 32]),
            new_generation: ContentKeyGeneration {
                version: ver,
                key: [key; 32].into(),
                rotated_at: at,
            },
            commit: None,
            gated_attempted: false,
        };
        let shared = staged(0xAA, 0x01, 2, 0x11, 2000);
        let a = FoldersConfig {
            pending_removals: vec![shared.clone(), staged(0xAA, 0x02, 2, 0x22, 2100)],
            ..Default::default()
        };
        let b = FoldersConfig {
            pending_removals: vec![shared.clone(), staged(0xBB, 0x03, 2, 0x33, 2200)],
            ..Default::default()
        };
        let merged = a.merge(&b);
        assert_eq!(merged.pending_removals.len(), 3, "shared collapses; 2 kept");
        // An empty side must not drop the other's staged (irrecoverable) key.
        // (The merge orders its output canonically, so compare as sets.)
        let empty = FoldersConfig::default();
        let merged_with_empty = empty.merge(&a).pending_removals;
        assert_eq!(merged_with_empty.len(), a.pending_removals.len());
        assert!(
            a.pending_removals
                .iter()
                .all(|p| merged_with_empty.contains(p))
        );
    }

    /// `gated_attempted` merges as the join `false < true`: an engaged-attempt
    /// stamp from either device survives (losing it would let a byte-less
    /// sentinel that a gated attempt may already have distributed rebuild
    /// blind — the fork). Deterministic, commutative, idempotent.
    #[test]
    fn folders_merge_joins_gated_attempted() {
        use crate::folder_keys::ContentKeyGeneration;
        let staged = |flag: bool| FolderPendingRemoval {
            channel_id: [0xAA; 32],
            name: "set".into(),
            removed_member: ActorId([0x01; 32]),
            new_generation: ContentKeyGeneration {
                version: 2,
                key: [0x11; 32].into(),
                rotated_at: 2000,
            },
            commit: None,
            gated_attempted: flag,
        };
        let cfg = |flag| FoldersConfig {
            pending_removals: vec![staged(flag)],
            ..Default::default()
        };
        let join = |x: bool, y: bool| cfg(x).merge(&cfg(y)).pending_removals[0].gated_attempted;
        // The attempt stamp survives either twin, in both orders.
        assert!(join(true, false));
        assert!(join(false, true));
        assert!(join(true, true));
        assert!(!join(false, false));
    }

    /// `foreign_sets` union by `channel_id`: a record on one side survives; a
    /// record on both folds per-field — `set_name` `None` ∨ `Some` → `Some`
    /// (so a device whose accept left the name unstamped recovers the name a
    /// peer device holds), differing `Some`s and differing URLs converge
    /// deterministically. Commutative + idempotent.
    #[test]
    fn folders_merge_unions_foreign_sets() {
        let foreign = |chan: u8, url: &str, name: Option<&str>| ForeignFolder {
            channel_id: [chan; 32],
            mls_group_id: vec![chan, chan, chan],
            home_nest_url: url.into(),
            home_nest_actor_id: None,
            set_name: name.map(Into::into),
            access: None,
            content_key_floor: None,
            ..Default::default()
        };
        let a = FoldersConfig {
            foreign_sets: vec![
                foreign(0xAA, "https://home-a.example", None),
                foreign(0xBB, "https://home-b.example", Some("b-set")),
            ],
            ..Default::default()
        };
        let b = FoldersConfig {
            foreign_sets: vec![foreign(0xAA, "https://home-a.example", Some("a-set"))],
            ..Default::default()
        };
        let merged = a.merge(&b);
        assert_eq!(merged.foreign_sets.len(), 2, "union by channel");
        let aa = merged
            .foreign_sets
            .iter()
            .find(|f| f.channel_id == [0xAA; 32])
            .unwrap();
        assert_eq!(
            aa.set_name.as_deref(),
            Some("a-set"),
            "None ∨ Some joins to Some"
        );
        // Commutative + idempotent; an empty side drops nothing.
        assert_eq!(b.merge(&a), merged);
        assert_eq!(merged.merge(&merged), merged);
        let empty = FoldersConfig::default();
        assert_eq!(empty.merge(&a).foreign_sets, a.foreign_sets);
    }

    /// At a stamp tie, the advisory `access` grant folds fail-safe: a device
    /// holding `"reader"` and one holding `"writer"` converge on **`"reader"`**
    /// (lexicographic-min = lesser privilege), and an unknown `None` yields to a
    /// known grant. Under-offering a binding self-heals on the next
    /// `caller_access` refresh; over-offering would hand the user a bind that
    /// dies at the eager mint. Commutative + idempotent like every other field.
    #[test]
    fn folders_merge_folds_foreign_access_fail_safe() {
        let foreign = |chan: u8, access: Option<&str>| ForeignFolder {
            channel_id: [chan; 32],
            mls_group_id: vec![chan],
            home_nest_url: "https://home.example".into(),
            home_nest_actor_id: None,
            set_name: Some("shared".into()),
            access: access.map(Into::into),
            content_key_floor: None,
            ..Default::default()
        };
        let stale = FoldersConfig {
            foreign_sets: vec![foreign(0xAA, Some("reader")), foreign(0xBB, None)],
            ..Default::default()
        };
        let fresh = FoldersConfig {
            foreign_sets: vec![foreign(0xAA, Some("writer")), foreign(0xBB, Some("writer"))],
            ..Default::default()
        };
        let merged = stale.merge(&fresh);
        let access_of = |c: u8, cfg: &FoldersConfig| {
            cfg.foreign_sets
                .iter()
                .find(|f| f.channel_id == [c; 32])
                .unwrap()
                .access
                .clone()
        };
        assert_eq!(
            access_of(0xAA, &merged).as_deref(),
            Some("reader"),
            "two differing grants converge on the LESSER privilege"
        );
        assert_eq!(
            access_of(0xBB, &merged).as_deref(),
            Some("writer"),
            "unknown (None) yields to a known grant — None is not a claim of reader"
        );
        assert_eq!(fresh.merge(&stale), merged, "commutative");
        assert_eq!(merged.merge(&merged), merged, "idempotent");
    }

    /// The five overwritten advisory fields are latest-wins on `updated_at`
    /// (`mls-group-key-material.md` § M2 → *Custody shape of the set nonce*,
    /// ruling (l)(iv)): a later promotion `reader → writer` and a re-bind to a
    /// URL sorting AFTER the stale one both land — which the per-field minimum
    /// never lets through a merge — while `set_name` stays gain-only and the
    /// floor stays the maximum whatever the stamps. Commutative, associative
    /// and idempotent.
    #[test]
    fn folders_merge_takes_foreign_advisory_fields_from_the_later_stamp() {
        let record = |updated_at: u64, access: &str, url: &str, name: Option<&str>| ForeignFolder {
            channel_id: [0xAA; 32],
            mls_group_id: vec![0xAA],
            home_nest_url: url.into(),
            home_nest_actor_id: Some(format!("actor-{updated_at}")),
            set_name: name.map(Into::into),
            access: Some(access.into()),
            content_key_floor: Some(10 - updated_at),
            residency: None,
            owner_handle: None,
            owner_domain: None,
            accepted_at: 1,
            left_at: None,
            updated_at,
        };
        let only = |f: ForeignFolder| FoldersConfig {
            foreign_sets: vec![f],
            ..Default::default()
        };
        let stale = only(record(1, "reader", "https://a.example", Some("docs")));
        let later = only(record(2, "writer", "https://z.example", None));
        let tied = only(record(2, "reader", "https://m.example", None));
        let merged = stale.merge(&later);
        let f = &merged.foreign_sets[0];
        assert_eq!(f.access.as_deref(), Some("writer"), "the promotion lands");
        assert_eq!(f.home_nest_url, "https://z.example", "the re-bind lands");
        assert_eq!(f.home_nest_actor_id.as_deref(), Some("actor-2"));
        assert_eq!(f.updated_at, 2);
        assert_eq!(f.set_name.as_deref(), Some("docs"), "the name is gain-only");
        assert_eq!(f.content_key_floor, Some(9), "the floor is the maximum");
        // A tie folds per field, so three replicas converge in any order.
        let all = stale.merge(&later).merge(&tied);
        assert_eq!(all.foreign_sets[0].access.as_deref(), Some("reader"));
        assert_eq!(all.foreign_sets[0].home_nest_url, "https://m.example");
        assert_eq!(later.merge(&stale), merged, "commutative");
        assert_eq!(stale.merge(&later.merge(&tied)), all, "associative");
        assert_eq!(tied.merge(&stale).merge(&later), all, "associative");
        assert_eq!(all.merge(&all), all, "idempotent");
    }

    /// The owner label is the fifth latest-wins advisory pair
    /// (`federation.md` § … *The cross-nest owner label*, join rule): a later
    /// stamp's pair lands whole — a rename and a clear alike — and a tie folds
    /// the pair as one unit, so a handle never pairs with another replica's
    /// domain. Commutative and idempotent.
    #[test]
    fn folders_merge_takes_the_owner_label_pair_from_the_later_stamp() {
        let record = |updated_at: u64, owner: Option<(&str, &str)>| FoldersConfig {
            foreign_sets: vec![ForeignFolder {
                channel_id: [0xAB; 32],
                owner_handle: owner.map(|(h, _)| h.into()),
                owner_domain: owner.map(|(_, d)| d.into()),
                updated_at,
                ..Default::default()
            }],
            ..Default::default()
        };
        let pair = |c: &FoldersConfig| {
            let f = &c.foreign_sets[0];
            (f.owner_handle.clone(), f.owner_domain.clone())
        };
        let old = record(1, Some(("alice", "a.example")));
        let renamed = record(2, Some(("alicia", "a.example")));
        let merged = old.merge(&renamed);
        assert_eq!(
            pair(&merged),
            (Some("alicia".into()), Some("a.example".into()))
        );
        assert_eq!(renamed.merge(&old), merged, "commutative");
        assert_eq!(merged.merge(&merged), merged, "idempotent");
        // A newer stamp-less write (an older device) clears it whole.
        let cleared = record(3, None);
        assert_eq!(pair(&merged.merge(&cleared)), (None, None));
        // A tie: an unstamped side yields; two stamped sides keep one whole pair.
        let tie_a = record(5, Some(("bob", "z.example")));
        let tie_b = record(5, Some(("carol", "b.example")));
        assert_eq!(
            pair(&tie_a.merge(&record(5, None))),
            (Some("bob".into()), Some("z.example".into()))
        );
        let tied = tie_a.merge(&tie_b);
        assert_eq!(pair(&tied), (Some("bob".into()), Some("z.example".into())));
        assert_eq!(tie_b.merge(&tie_a), tied, "commutative on a tie");
    }

    /// A leave tombstones the record and a later accept revives it; both
    /// stamps max-join, so a stale device's pre-leave copy cannot revive a
    /// left record, and a re-accept survives a stale copy of the leave
    /// (ruling (l)(iii)).
    #[test]
    fn a_foreign_leave_tombstones_and_a_later_accept_revives() {
        let record = |accepted_at: u64, left_at: Option<u64>| FoldersConfig {
            foreign_sets: vec![ForeignFolder {
                channel_id: [0xAA; 32],
                accepted_at,
                left_at,
                ..Default::default()
            }],
            ..Default::default()
        };
        let accepted = record(10, None);
        let left = record(10, Some(20));
        let reaccepted = record(30, Some(20));
        assert!(accepted.foreign_sets[0].is_live());
        assert!(!left.foreign_sets[0].is_live());
        assert!(reaccepted.foreign_sets[0].is_live());
        assert!(
            !accepted.merge(&left).foreign_sets[0].is_live(),
            "a stale pre-leave copy cannot revive the record"
        );
        assert!(
            left.merge(&reaccepted).foreign_sets[0].is_live(),
            "the re-accept outlives the leave it follows"
        );
        assert_eq!(
            accepted.merge(&left).merge(&reaccepted),
            reaccepted.merge(&accepted).merge(&left),
            "order-free"
        );
    }

    /// A record with the additive `access` field absent must still
    /// deserialize — and land on the fail-safe `None` (⇒ treated as reader,
    /// unbindable) rather than failing the whole config open.
    #[test]
    fn foreign_folder_without_access_deserializes_as_unknown() {
        let bare = ForeignFolder {
            channel_id: [0x11; 32],
            mls_group_id: vec![1, 2, 3],
            home_nest_url: "https://home.example".into(),
            home_nest_actor_id: None,
            set_name: Some("docs".into()),
            access: None,
            content_key_floor: None,
            ..Default::default()
        };
        // Round-trip through a map with every additive `#[serde(default)]` field
        // absent entirely — a record carrying none of them (`access`, plus the
        // cross-nest trust root). All must
        // decode to the fail-safe `None`, never fail the whole config open.
        let mut value = serde_json::to_value(&bare).expect("serializes");
        let obj = value.as_object_mut().expect("object");
        obj.remove("access").expect("field was present");
        obj.remove("home_nest_actor_id").expect("field was present");
        obj.remove("content_key_floor").expect("field was present");
        for stamp in ["accepted_at", "left_at", "updated_at"] {
            obj.remove(stamp).expect("field was present");
        }
        let decoded: ForeignFolder = serde_json::from_value(value).expect("bare record decodes");
        assert!(
            decoded.is_live(),
            "a record with no stamps is a live membership"
        );
        assert_eq!(decoded, bare);
        assert!(decoded.access.is_none());
        assert!(decoded.home_nest_actor_id.is_none());
        assert!(decoded.content_key_floor.is_none());
    }

    /// The federated floor folds to the HIGHER value: the floor is monotone on
    /// the home nest (`content_key.put` stores `MAX`), so two devices holding
    /// different stamps differ only by staleness, and the higher one is the one
    /// the pre-seal hold must honour — a seal under the lower would be under a
    /// generation the owner rotated past. `None` ∨ `Some` → `Some`. Commutative
    /// + idempotent like every other field.
    #[test]
    fn folders_merge_folds_foreign_floor_to_the_higher() {
        let foreign = |chan: u8, floor: Option<u64>| ForeignFolder {
            channel_id: [chan; 32],
            mls_group_id: vec![chan],
            home_nest_url: "https://home.example".into(),
            home_nest_actor_id: None,
            set_name: Some("shared".into()),
            access: None,
            content_key_floor: floor,
            ..Default::default()
        };
        let stale = FoldersConfig {
            foreign_sets: vec![foreign(0xAA, Some(1)), foreign(0xBB, None)],
            ..Default::default()
        };
        let fresh = FoldersConfig {
            foreign_sets: vec![foreign(0xAA, Some(3)), foreign(0xBB, Some(2))],
            ..Default::default()
        };
        let merged = stale.merge(&fresh);
        let floor_of = |c: u8, cfg: &FoldersConfig| {
            cfg.foreign_sets
                .iter()
                .find(|f| f.channel_id == [c; 32])
                .unwrap()
                .content_key_floor
        };
        assert_eq!(
            floor_of(0xAA, &merged),
            Some(3),
            "two differing stamps converge on the HIGHER floor"
        );
        assert_eq!(
            floor_of(0xBB, &merged),
            Some(2),
            "unknown (None) yields to a known floor — None is not a claim of no floor"
        );
        assert_eq!(fresh.merge(&stale), merged, "commutative");
        assert_eq!(merged.merge(&merged), merged, "idempotent");
    }

    #[test]
    fn mail_config_default_is_not_yet_enabled() {
        let cfg = MailConfig::default();
        assert!(cfg.msek.is_none());
        assert!(cfg.credentials.is_empty());
        assert!(cfg.pending_rotation.is_none());
        assert!(
            !cfg.is_mail_enabled(),
            "default (no MSEK) ⇒ mail not enabled"
        );
    }

    #[test]
    fn mail_is_enabled_keys_on_the_flag_alone() {
        // Explicit email on.
        let mail = MailConfig {
            msek: Some([1u8; 32].into()),
            mail_enabled: Some(true),
            ..MailConfig::default()
        };
        assert!(mail.is_mail_enabled());

        // CalDAV-only: holds the shared MSEK but email is explicitly OFF, so the
        // mail-settings page must render "mail disabled" even though `msek` is
        // `Some` (the bug `is_mail_enabled` exists to prevent).
        let caldav_only = MailConfig {
            msek: Some([1u8; 32].into()),
            mail_enabled: Some(false),
            caldav_enabled: true,
            ..MailConfig::default()
        };
        assert!(!caldav_only.is_mail_enabled());

        // An unwritten flag is off, MSEK or not: no `msek.is_some()` fallback
        // survives the compat-remnant sweep (the merge keeps a written flag
        // beside a surviving MSEK instead — `MailConfig::merge`).
        let unwritten = MailConfig {
            msek: Some([1u8; 32].into()),
            mail_enabled: None,
            ..MailConfig::default()
        };
        assert!(
            !unwritten.is_mail_enabled(),
            "an MSEK with no flag is not email"
        );
        let empty = MailConfig {
            msek: None,
            mail_enabled: None,
            ..MailConfig::default()
        };
        assert!(!empty.is_mail_enabled());
    }

    #[test]
    fn dns_config_default_is_empty() {
        let cfg = DnsConfig::default();
        assert!(cfg.credentials.is_empty());
        assert!(cfg.managed_domains.is_empty());
    }

    // ── DelegationConfig::set_pin — the assignment-picker write (slice 4) ──

    #[test]
    fn set_pin_adds_a_row_on_empty_config() {
        let mut cfg = DelegationConfig::default();
        let to = ParticipantRef::Device {
            device_id: "dev-a".into(),
        };
        cfg.set_pin("backup-upload", Some(to.clone()));
        assert_eq!(cfg.assignments.len(), 1);
        assert_eq!(cfg.assignments[0].task_kind, "backup-upload");
        assert_eq!(cfg.assignments[0].pinned_to, Some(to));
    }

    #[test]
    fn set_pin_updates_an_existing_row_in_place() {
        let mut cfg = DelegationConfig {
            assignments: vec![TaskAssignment {
                task_kind: "backup-upload".into(),
                pinned_to: Some(ParticipantRef::Device {
                    device_id: "dev-a".into(),
                }),
            }],
        };
        let to_b = ParticipantRef::Device {
            device_id: "dev-b".into(),
        };
        cfg.set_pin("backup-upload", Some(to_b.clone()));
        // Repinned in place — not a second row.
        assert_eq!(cfg.assignments.len(), 1);
        assert_eq!(cfg.assignments[0].pinned_to, Some(to_b));
    }

    #[test]
    fn set_pin_none_removes_the_row_back_to_automatic() {
        let mut cfg = DelegationConfig {
            assignments: vec![TaskAssignment {
                task_kind: "backup-upload".into(),
                pinned_to: Some(ParticipantRef::Device {
                    device_id: "dev-a".into(),
                }),
            }],
        };
        cfg.set_pin("backup-upload", None);
        // Absent ⇒ automatic; the config returns to the minimal empty state.
        assert!(cfg.assignments.is_empty());
    }

    #[test]
    fn set_pin_none_on_absent_kind_is_a_noop() {
        let mut cfg = DelegationConfig::default();
        cfg.set_pin("backup-upload", None);
        assert!(cfg.assignments.is_empty());
    }

    #[test]
    fn set_pin_leaves_other_kinds_untouched() {
        let mut cfg = DelegationConfig {
            assignments: vec![TaskAssignment {
                task_kind: "index".into(),
                pinned_to: Some(ParticipantRef::Device {
                    device_id: "dev-z".into(),
                }),
            }],
        };
        cfg.set_pin(
            "backup-upload",
            Some(ParticipantRef::Device {
                device_id: "dev-a".into(),
            }),
        );
        cfg.set_pin("backup-upload", None);
        // The unrelated `index` pin survived both writes.
        assert_eq!(cfg.assignments.len(), 1);
        assert_eq!(cfg.assignments[0].task_kind, "index");
    }

    #[test]
    fn backup_config_default_is_empty() {
        let bc = BackupConfig::default();
        assert!(bc.destinations.is_empty());
    }
}

#[cfg(test)]
mod nest_entry_tests {
    use super::*;

    #[test]
    fn nest_entry_serialization_roundtrip() {
        let entry = NestEntry {
            nest_id: vec![1; 32],
            url: "https://alice.fauna.social".to_string(),
            roles: vec![NestRole::Social, NestRole::Mls],
        };
        let encoded = serde_json::to_string(&entry).unwrap();
        let decoded: NestEntry = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.url, "https://alice.fauna.social");
        assert_eq!(decoded.roles.len(), 2);
    }

    #[test]
    fn profile_nests_field() {
        let profile = Profile {
            actor_id: ActorId([0; 32]),
            display_name: Some("Alice".into()),
            bio: None,
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![NestEntry {
                nest_id: vec![1; 32],
                url: "https://alice.fauna.social".into(),
                roles: vec![NestRole::Social],
            }],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: None,
            updated_at: Timestamp(0),
        };
        assert_eq!(profile.nests.len(), 1);
        assert_eq!(profile.nests[0].roles[0], NestRole::Social);
    }

    /// The pre-rename `nodes` spelling is refused (its read alias was retired
    /// 2026-09-24 with the compat-remnant sweep): `nests` is required and
    /// carries no default, so the old key leaves it missing.
    #[test]
    fn profile_nodes_key_is_refused() {
        let json = r#"{
            "actor_id": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
            "display_name": null,
            "bio": null,
            "avatar": null,
            "banner": null,
            "links": [],
            "nodes": [],
            "load_hint": null,
            "inbox_mode": "Open",
            "updated_at": 0
        }"#;
        let err = serde_json::from_str::<Profile>(json)
            .expect_err("a `nodes`-keyed profile must be refused: `nests` is missing");
        assert!(err.to_string().contains("nests"), "unexpected error: {err}");
    }

    #[test]
    fn profile_admin_nests_field() {
        let profile = Profile {
            actor_id: ActorId([0; 32]),
            display_name: None,
            bio: None,
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![],
            admin_nests: vec![AdminNestEntry {
                nest_id: vec![1; 32],
                url: "https://example.com".to_string(),
                name: "My Nest".to_string(),
            }],
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: None,
            updated_at: Timestamp(0),
        };
        assert_eq!(profile.admin_nests.len(), 1);
        assert_eq!(profile.admin_nests[0].name, "My Nest");
    }

    #[test]
    fn profile_without_admin_nests_deserializes() {
        let json = r#"{
            "actor_id": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
            "display_name": null, "bio": null, "avatar": null, "banner": null,
            "links": [], "nests": [],
            "load_hint": null, "inbox_mode": "Open", "updated_at": 0
        }"#;
        let profile: Profile = serde_json::from_str(json).unwrap();
        assert!(profile.admin_nests.is_empty());
    }
}

#[cfg(test)]
mod video_tests {
    use super::*;
    use crate::encoding::{canonical_decode, canonical_encode};

    #[test]
    fn video_segment_roundtrip() {
        let seg = VideoSegment {
            hash: ContentHash::from_digest_raw([1u8; 32]),
            resolution: 720,
            codec: "h264".to_string(),
            bitrate: 2500,
            byte_size: 5_000_000,
        };
        let bytes = canonical_encode(&seg).unwrap();
        let decoded: VideoSegment = canonical_decode(&bytes).unwrap();
        assert_eq!(decoded.hash, seg.hash);
        assert_eq!(decoded.resolution, seg.resolution);
        assert_eq!(decoded.codec, seg.codec);
    }

    #[test]
    fn post_body_video_roundtrip() {
        let body = PostBody::Video {
            manifest: ContentHash::from_digest_raw([2u8; 32]),
            segments: vec![VideoSegment {
                hash: ContentHash::from_digest_raw([3u8; 32]),
                resolution: 1080,
                codec: "h264".to_string(),
                bitrate: 5000,
                byte_size: 10_000_000,
            }],
            thumbnail: ContentHash::from_digest_raw([4u8; 32]),
            duration_ms: 15_000,
            aspect_ratio: (9, 16),
            anchors: vec![VerificationAnchor {
                timestamp_ms: 5000,
                phash: [0xAB; 8],
            }],
        };
        let bytes = canonical_encode(&body).unwrap();
        let decoded: PostBody = canonical_decode(&bytes).unwrap();
        assert_eq!(decoded, body);
    }
}

#[cfg(test)]
mod reach_floor_tests {
    use super::*;

    const SUPERVISED_STRICT: SupervisedReach = SupervisedReach {
        contact_approval: true,
        federation_contact: false,
    };
    /// A fresh guardianship's untouched policy — every default is the
    /// unsupervised-equivalent value (`family-safety.md` § Wire & data shape).
    const SUPERVISED_DEFAULT: SupervisedReach = SupervisedReach {
        contact_approval: false,
        federation_contact: true,
    };

    #[test]
    fn blocked_suppresses_on_every_path() {
        for supervised in [None, Some(SUPERVISED_DEFAULT), Some(SUPERVISED_STRICT)] {
            for origin in [ArrivalOrigin::Local, ArrivalOrigin::Federation] {
                assert_eq!(
                    supervised_reach_verdict(Some(ContactStatus::Blocked), supervised, origin),
                    ReachVerdict::Suppress,
                );
            }
        }
    }

    #[test]
    fn an_unsupervised_recipient_gets_no_extra_floor() {
        // The whole point: adults' arrivals are decided by inbox mode alone,
        // so nothing here may turn a stranger away before the mode sees them.
        for status in [None, Some(ContactStatus::Pending)] {
            for origin in [ArrivalOrigin::Local, ArrivalOrigin::Federation] {
                assert_eq!(
                    supervised_reach_verdict(status, None, origin),
                    ReachVerdict::Proceed,
                );
            }
        }
    }

    #[test]
    fn established_contacts_flow_under_every_knob() {
        for status in [ContactStatus::Accepted, ContactStatus::Confirmed] {
            for origin in [ArrivalOrigin::Local, ArrivalOrigin::Federation] {
                assert_eq!(
                    supervised_reach_verdict(Some(status), Some(SUPERVISED_STRICT), origin),
                    ReachVerdict::Proceed,
                    "both reach knobs act on NEW parties only",
                );
            }
        }
    }

    #[test]
    fn dm_mode_verdict_lets_established_contacts_flow_under_every_mode() {
        for status in [ContactStatus::Accepted, ContactStatus::Confirmed] {
            for mode in [
                InboxMode::Open,
                InboxMode::AllowKnock,
                InboxMode::ContactsOnly,
                InboxMode::Closed,
            ] {
                assert_eq!(
                    dm_initiation_mode_verdict(Some(status), mode.clone()),
                    ReachVerdict::Proceed,
                    "a mode acts on NEW parties only ({mode:?})",
                );
            }
        }
    }

    #[test]
    fn dm_mode_verdict_gates_strangers_per_mode() {
        // A true stranger and a still-Pending knock are the same party:
        // "the recipient must accept before DMs flow" (direct-messages.md FAQ).
        for status in [None, Some(ContactStatus::Pending)] {
            assert_eq!(
                dm_initiation_mode_verdict(status, InboxMode::Open),
                ReachVerdict::Proceed,
            );
            assert_eq!(
                dm_initiation_mode_verdict(status, InboxMode::AllowKnock),
                ReachVerdict::Knock,
                "the default mode points the sender at the contact request",
            );
            assert_eq!(
                dm_initiation_mode_verdict(status, InboxMode::ContactsOnly),
                ReachVerdict::Suppress,
            );
            assert_eq!(
                dm_initiation_mode_verdict(status, InboxMode::Closed),
                ReachVerdict::Suppress,
            );
        }
    }

    #[test]
    fn dm_mode_verdict_suppresses_a_blocked_sender_even_on_open() {
        assert_eq!(
            dm_initiation_mode_verdict(Some(ContactStatus::Blocked), InboxMode::Open),
            ReachVerdict::Suppress,
        );
    }

    #[test]
    fn inbox_mode_from_wire_parses_exactly_the_four_stored_tokens() {
        assert_eq!(InboxMode::from_wire("open"), Some(InboxMode::Open));
        assert_eq!(
            InboxMode::from_wire("allow_knock"),
            Some(InboxMode::AllowKnock)
        );
        assert_eq!(
            InboxMode::from_wire("contacts_only"),
            Some(InboxMode::ContactsOnly)
        );
        assert_eq!(InboxMode::from_wire("closed"), Some(InboxMode::Closed));
        // Reject-don't-guess: an unknown token parses to None, and the caller
        // refuses (the knock path's own unknown-mode arm).
        assert_eq!(InboxMode::from_wire("Open"), None);
        assert_eq!(InboxMode::from_wire(""), None);
    }

    #[test]
    fn inbox_mode_to_wire_round_trips_through_from_wire() {
        for mode in [
            InboxMode::Open,
            InboxMode::AllowKnock,
            InboxMode::ContactsOnly,
            InboxMode::Closed,
        ] {
            assert_eq!(
                InboxMode::from_wire(mode.to_wire().unwrap()),
                Some(mode.clone())
            );
        }
        assert_eq!(InboxMode::Open.to_wire(), Some("open"));
        assert_eq!(InboxMode::AllowKnock.to_wire(), Some("allow_knock"));
        assert_eq!(InboxMode::ContactsOnly.to_wire(), Some("contacts_only"));
        assert_eq!(InboxMode::Closed.to_wire(), Some("closed"));
        assert_eq!(
            InboxMode::Other("Moderated".into()).to_wire(),
            None,
            "a mode this build does not name has no stored token"
        );
    }

    #[test]
    fn contact_approval_knocks_a_stranger() {
        let reach = SupervisedReach {
            contact_approval: true,
            federation_contact: true,
        };
        for status in [None, Some(ContactStatus::Pending)] {
            assert_eq!(
                supervised_reach_verdict(status, Some(reach), ArrivalOrigin::Local),
                ReachVerdict::Knock,
            );
        }
    }

    #[test]
    fn federation_contact_off_suppresses_before_the_guardian_queue() {
        // Suppress, not Knock: a cross-nest stranger the guardian has switched
        // off must not even reach the approvals queue.
        assert_eq!(
            supervised_reach_verdict(None, Some(SUPERVISED_STRICT), ArrivalOrigin::Federation),
            ReachVerdict::Suppress,
        );
        // ... but the same stranger arriving locally only knocks.
        assert_eq!(
            supervised_reach_verdict(None, Some(SUPERVISED_STRICT), ArrivalOrigin::Local),
            ReachVerdict::Knock,
        );
    }

    #[test]
    fn a_default_policy_changes_nothing() {
        for status in [None, Some(ContactStatus::Pending)] {
            for origin in [ArrivalOrigin::Local, ArrivalOrigin::Federation] {
                assert_eq!(
                    supervised_reach_verdict(status, Some(SUPERVISED_DEFAULT), origin),
                    ReachVerdict::Proceed,
                    "a fresh link with an untouched policy restricts nothing",
                );
            }
        }
    }

    #[test]
    fn system_generated_mail_is_never_gated() {
        // A bounce/NDR/security notice reaches the same ingest core as external
        // mail. Holding one would strand the ward: they'd never learn a message
        // failed to send. Every policy, known or not.
        for policy in [
            UnknownSenderMail::Allow,
            UnknownSenderMail::Hold,
            UnknownSenderMail::Reject,
        ] {
            for known in [true, false] {
                assert_eq!(
                    supervised_mail_verdict(Some(policy), known, MailIngress::System),
                    MailVerdict::Deliver,
                    "system mail must bypass {policy:?}",
                );
            }
        }
    }

    #[test]
    fn an_unsupervised_recipient_is_never_gated() {
        for known in [true, false] {
            assert_eq!(
                supervised_mail_verdict(None, known, MailIngress::Sender("stranger@ex.com")),
                MailVerdict::Deliver,
            );
        }
    }

    #[test]
    fn a_known_sender_always_flows_under_every_policy() {
        // The outbound auto-seed's whole purpose: a reply to mail the child sent
        // is never held or rejected.
        for policy in [
            UnknownSenderMail::Allow,
            UnknownSenderMail::Hold,
            UnknownSenderMail::Reject,
        ] {
            assert_eq!(
                supervised_mail_verdict(Some(policy), true, MailIngress::Sender("pal@ex.com")),
                MailVerdict::Deliver,
            );
        }
    }

    #[test]
    fn a_cold_sender_maps_to_the_policy() {
        let cold = MailIngress::Sender("stranger@ex.com");
        assert_eq!(
            supervised_mail_verdict(Some(UnknownSenderMail::Allow), false, cold),
            MailVerdict::Deliver,
        );
        assert_eq!(
            supervised_mail_verdict(Some(UnknownSenderMail::Hold), false, cold),
            MailVerdict::Hold,
        );
        assert_eq!(
            supervised_mail_verdict(Some(UnknownSenderMail::Reject), false, cold),
            MailVerdict::Reject,
        );
    }

    #[test]
    fn uncorrelated_null_path_mail_is_held_under_hold_and_reject() {
        // Anyone on the internet can claim `MAIL FROM:<>`. Without a DSN
        // correlation the message is held under BOTH gating policies — reject
        // downgrades because RCPT can't decide DSN-ness and a post-DATA
        // refusal could never be bounced to a null path. `known_sender = false`
        // is the caller's verdict for every uncorrelated shape: no report, a
        // report whose original Message-ID the ward never sent, and a report
        // carrying no extractable id at all.
        for dsn_original_msgid in [None, Some("attacker-invented@evil.com")] {
            let cold = MailIngress::NullReversePath { dsn_original_msgid };
            assert_eq!(
                supervised_mail_verdict(Some(UnknownSenderMail::Hold), false, cold),
                MailVerdict::Hold,
            );
            assert_eq!(
                supervised_mail_verdict(Some(UnknownSenderMail::Reject), false, cold),
                MailVerdict::Hold,
                "reject must downgrade to hold for the null path, never bounce",
            );
            assert_eq!(
                supervised_mail_verdict(Some(UnknownSenderMail::Allow), false, cold),
                MailVerdict::Deliver,
            );
        }
    }

    #[test]
    fn a_correlated_dsn_on_the_null_path_always_flows() {
        // The caller matched the report's *original Message-ID* against the
        // ward's sent set (known = the ward really sent that message): a
        // genuine bounce of the ward's own mail is never gated.
        let dsn = MailIngress::NullReversePath {
            dsn_original_msgid: Some("sent-1@fauna.test"),
        };
        for policy in [
            UnknownSenderMail::Allow,
            UnknownSenderMail::Hold,
            UnknownSenderMail::Reject,
        ] {
            assert_eq!(
                supervised_mail_verdict(Some(policy), true, dsn),
                MailVerdict::Deliver,
            );
        }
    }

    #[test]
    fn from_envelope_classifies_the_null_path() {
        assert_eq!(
            MailIngress::from_envelope("a@ex.com", None),
            MailIngress::Sender("a@ex.com"),
        );
        assert_eq!(
            MailIngress::from_envelope("", None),
            MailIngress::NullReversePath {
                dsn_original_msgid: None
            },
        );
        assert_eq!(
            MailIngress::from_envelope("", Some("sent-1@fauna.test")),
            MailIngress::NullReversePath {
                dsn_original_msgid: Some("sent-1@fauna.test")
            },
        );
    }

    #[test]
    fn an_unrecognized_policy_value_fails_closed_to_hold() {
        // A newer nest may write a knob value this binary doesn't know. Holding
        // is the strictest verdict that loses no mail — `Allow` would silently
        // void the guardian's policy, `Reject` would bounce it irrecoverably.
        assert_eq!(
            UnknownSenderMail::from_wire("allow"),
            UnknownSenderMail::Allow
        );
        assert_eq!(
            UnknownSenderMail::from_wire("hold"),
            UnknownSenderMail::Hold
        );
        assert_eq!(
            UnknownSenderMail::from_wire("reject"),
            UnknownSenderMail::Reject
        );
        for unknown in ["", "quarantine", "ALLOW", "reject_with_dsn"] {
            assert_eq!(
                UnknownSenderMail::from_wire(unknown),
                UnknownSenderMail::Hold,
                "{unknown:?} must fail closed",
            );
        }
    }

    #[test]
    fn an_unauthenticated_party_can_never_be_approved() {
        // No identity to put in the guardian's queue ⇒ suppress, never knock.
        assert_eq!(
            unauthenticated_reach_verdict(Some(SUPERVISED_STRICT), ArrivalOrigin::Federation),
            ReachVerdict::Suppress,
        );
        let approval_only = SupervisedReach {
            contact_approval: true,
            federation_contact: true,
        };
        assert_eq!(
            unauthenticated_reach_verdict(Some(approval_only), ArrivalOrigin::Federation),
            ReachVerdict::Suppress,
        );
        // Unsupervised + default-policy recipients keep receiving cross-nest
        // Welcomes exactly as before.
        assert_eq!(
            unauthenticated_reach_verdict(None, ArrivalOrigin::Federation),
            ReachVerdict::Proceed,
        );
        assert_eq!(
            unauthenticated_reach_verdict(Some(SUPERVISED_DEFAULT), ArrivalOrigin::Federation),
            ReachVerdict::Proceed,
        );
    }
}

#[cfg(test)]
mod post_origin_tests {
    use super::{ActorId, GatedInfo, Post, PostBody, PostOrigin, Reference, Timestamp};
    use crate::encoding::{canonical_decode, canonical_encode};
    use serde::Serialize;

    /// `Post` exactly as every build before `origin` encoded it.
    #[derive(Serialize)]
    struct PreFieldPost {
        author: ActorId,
        created_at: Timestamp,
        body: PostBody,
        references: Vec<Reference>,
        expires_at: Option<Timestamp>,
        gated: Option<GatedInfo>,
        content_warning: Option<String>,
    }

    fn text_post(origin: Option<PostOrigin>) -> Post {
        Post {
            author: ActorId([9u8; 32]),
            created_at: Timestamp(1_700_000_000_000_000),
            body: PostBody::Text {
                content: "hello".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin,
        }
    }

    fn facebook_origin() -> PostOrigin {
        PostOrigin {
            platform: crate::source::FACEBOOK.into(),
            url: Some("https://www.facebook.com/example/posts/10001".into()),
        }
    }

    /// `skip_serializing_if`: an origin-less post's canonical bytes — and so
    /// its CID and every signature over it — are byte-identical to a pre-field
    /// post's. Without this every new post would carry `origin: null` and
    /// every golden pinned on post bytes would move.
    #[test]
    fn a_post_without_origin_encodes_byte_identically_to_the_pre_field_shape() {
        let post = text_post(None);
        let pre = PreFieldPost {
            author: post.author,
            created_at: post.created_at,
            body: post.body.clone(),
            references: post.references.clone(),
            expires_at: post.expires_at,
            gated: post.gated.clone(),
            content_warning: post.content_warning.clone(),
        };
        assert_eq!(
            canonical_encode(&post).unwrap(),
            canonical_encode(&pre).unwrap(),
        );
    }

    #[test]
    fn origin_round_trips_and_pre_field_bytes_decode_with_origin_none() {
        let post = text_post(Some(facebook_origin()));
        let bytes = canonical_encode(&post).unwrap();
        let decoded: Post = canonical_decode(bytes.as_ref()).unwrap();
        assert_eq!(decoded, post);
        assert_eq!(
            decoded.origin.as_ref().unwrap().url.as_deref().unwrap(),
            "https://www.facebook.com/example/posts/10001"
        );

        // An older peer's bytes carry no `origin` key at all → None, not an error
        // (`version-compatibility.md` § I4).
        let pre = PreFieldPost {
            author: ActorId([9u8; 32]),
            created_at: Timestamp(1),
            body: PostBody::Text {
                content: "old".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
        };
        let decoded: Post = canonical_decode(canonical_encode(&pre).unwrap().as_ref()).unwrap();
        assert_eq!(decoded.origin, None);
    }

    /// Ruling 2 (`archive-import.md` § What each category becomes → *The post
    /// origin field*): the envelope identifies the platform and nothing that
    /// identifies the post on the other network. A `url`-less origin — the
    /// shape phase one authors on every post — encodes as the one key
    /// `platform`. (The `external_id` slice 2 minted here was removed before
    /// any release carried it: the production pin predates slice 2.)
    #[test]
    fn an_origin_is_the_platform_and_nothing_else_when_url_is_unset() {
        let o = PostOrigin {
            platform: crate::source::FACEBOOK.into(),
            url: None,
        };
        let bytes = canonical_encode(&o).unwrap();
        let map: std::collections::BTreeMap<String, fauna_cbor::Value> =
            canonical_decode(bytes.as_ref()).unwrap();
        assert_eq!(
            map.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["platform"]
        );
    }

    /// A slice-2-shaped map (an extra `external_id` key) still decodes — the
    /// key is unknown and ignored, so a dev-fleet post authored between the
    /// two commits is not a decode failure.
    #[test]
    fn a_slice_two_shaped_origin_still_decodes() {
        #[derive(Serialize)]
        struct SliceTwoOrigin {
            platform: String,
            external_id: String,
        }
        let bytes = canonical_encode(&SliceTwoOrigin {
            platform: crate::source::FACEBOOK.into(),
            external_id: "10001".into(),
        })
        .unwrap();
        let decoded: PostOrigin = canonical_decode(bytes.as_ref()).unwrap();
        assert_eq!(decoded.platform, crate::source::FACEBOOK);
        assert_eq!(decoded.url, None);
    }

    #[test]
    fn source_token_is_the_normalized_origin_platform_or_native() {
        assert_eq!(text_post(None).source_token(), crate::source::NATIVE);
        assert_eq!(
            text_post(Some(facebook_origin())).source_token(),
            "facebook"
        );

        let mut shouty = facebook_origin();
        shouty.platform = " Instagram ".into();
        assert_eq!(text_post(Some(shouty)).source_token(), "instagram");

        // Anything `source::normalize` refuses falls back to native — a client
        // cannot make the nest index an empty, oversized or list-shaped token.
        for bad in ["", "   ", "fauna, bluesky", "face book", &"x".repeat(33)] {
            let mut o = facebook_origin();
            o.platform = bad.to_string();
            assert_eq!(text_post(Some(o)).source_token(), "fauna", "{bad:?}");
        }

        // The vocabulary is CLOSED to `crate::source::ARCHIVE_PLATFORMS`: a
        // normalize-passing token that names a bridge (or anything else this
        // build doesn't recognize) still falls back to native — a client
        // cannot make the nest index its own signed post under a bridge
        // token and route its interactions into a bridge arm or spoof the
        // badge.
        for not_archive in ["email", "bluesky", "nostr", "activitypub", "x"] {
            let mut o = facebook_origin();
            o.platform = not_archive.to_string();
            assert_eq!(
                text_post(Some(o)).source_token(),
                "fauna",
                "{not_archive:?}"
            );
        }
    }
}
