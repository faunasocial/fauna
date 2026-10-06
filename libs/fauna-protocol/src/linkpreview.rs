//! `fauna.linkpreview.resolve` WS-RPC payload types — the wire side of the
//! render-model § D4 link-preview embed (`docs/goal/architecture/render-model.md`).
//!
//! The shared body producer (`fauna_core::render::markdown_to_document`) emits a
//! `RenderBlock::LinkPreview { url, state: Resolving }` for a standalone bare-url
//! paragraph; the per-app page manager then resolves the preview metadata by
//! calling this **authenticated** kind and re-emits the block
//! `Resolved`/`Failed` (the lazy-resolve→rebuild-document pattern).
//!
//! **Why nest-side (render-model.md § D4, user-ratified 2026-06-26):** the metadata
//! (`og:title` / `og:description` / `og:image`) is fetched **by the nest**, not the
//! client — a client-side fetch leaks every user's IP to every linked site **and** is
//! CORS-blocked on web (so web could render no previews at all — a priority-#1 per-app
//! divergence). The nest owns the SSRF/abuse guards (http(s)-only, block private/link-local
//! IPs, size + time caps), mirroring the existing `media_proxy_routes` Bluesky-media proxy
//! posture; if it keeps the og:image it stores it as a content-addressed blob whose hash
//! is returned as `image_hash`, so the client resolves the image through its existing
//! `RemoteImage`/media path (blocked-by-default per § D3).
//!
//! One kind:
//!
//! - `fauna.linkpreview.resolve` — [`LinkPreviewResolveRequest`] carries the `url` to
//!   resolve; [`LinkPreviewResolveReply`] is the internally-tagged enum of the two
//!   outcomes (`resolved` / `failed`). The reply's `Resolved` fields mirror
//!   `fauna_core::render::PreviewState::Resolved` (the manager maps the wire reply into
//!   that presentation leaf), but the wire type is **standalone** — protocol wire types
//!   stay separate from the `fauna-core` presentation model (per the `subscriptions.rs` /
//!   `stats.rs` convention; the protocol crate carries no `fauna_core::render` import).
//!
//! Wire convention (matching `stats.rs` / `subscriptions.rs`): the request struct carries
//! the `#[serde(flatten, default)] extra` forward-compat map; the discriminated reply
//! follows the internally-tagged-enum convention (`StatsGetReply` /
//! `SubscribeReply`) and per that precedent the tagged-enum *variants* carry no `extra`
//! map. A content-addressed blob handle rides as a hex `Option<String>` (the
//! `sync::manifest_hash` precedent), `None` when the page had no usable preview image.
//!
//! **Handler is nest-side (entrusted, route 3).** This module defines only the wire types
//! and the kind metadata (`kind.rs::register_linkpreview_kinds`); the resolve handler —
//! the OG-fetcher, the by-url cache, the SSRF/size/time guards, and the image-blob store —
//! is owned on the nest side.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

/// The kind string for the link-preview resolve RPC. Registered (metadata only) in
/// `kind.rs::register_linkpreview_kinds`; the nest associates it with the handler.
pub const KIND_LINKPREVIEW_RESOLVE: &str = "fauna.linkpreview.resolve";

// ── fauna.linkpreview.resolve ───────────────────────────────────────────────

/// Resolve link-preview metadata for `url`. The nest fetches the page (with its
/// SSRF/size/time guards), parses its OpenGraph/meta tags, caches by url, and replies
/// [`LinkPreviewResolveReply`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LinkPreviewResolveRequest {
    /// The `http(s)` url to resolve a preview for — the `url` of the
    /// `RenderBlock::LinkPreview` block the manager is resolving.
    pub url: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Link-preview resolve reply — one of two outcomes. Internally tagged on `outcome`
/// (`resolved` / `failed`), the established discriminated-reply convention
/// (`StatsGetReply` on `scope`, `SubscribeReply` on `outcome`).
///
/// The manager maps `Resolved` → `fauna_core::render::PreviewState::Resolved` and `Failed`
/// → `PreviewState::Failed`. (There is no `resolving` outcome on the wire: `Resolving` is
/// the purely client-side state the producer emits before this call.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum LinkPreviewResolveReply {
    /// The page yielded usable preview metadata.
    Resolved {
        /// `og:title` (or the page `<title>`), possibly empty.
        title: String,
        /// `og:description` (or the meta description), possibly empty.
        description: String,
        /// Hex BLAKE3 hash of the og:image, stored by the nest as a content-addressed
        /// blob the client resolves through its media path (blocked-by-default per § D3);
        /// `None` when the page had no usable preview image. Hex `Option<String>` follows
        /// the `sync::manifest_hash` wire-hash precedent.
        image_hash: Option<String>,
    },
    /// No usable preview (fetch failed, blocked by an SSRF/size/time guard, or the page
    /// carried no OpenGraph/meta). A **unit** variant mirroring the render model's unit
    /// `PreviewState::Failed` — "a generic, non-retried failure for render purposes".
    Failed,
    /// An outcome a newer build added that this build cannot read
    /// (`transport.md` § Schema and forward-compat discipline, rule 3: open,
    /// collapsing). It reads as `Failed`. Never serialized: a path that would re-emit it
    /// fails instead of replacing the newer value.
    #[serde(other, skip_serializing)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::test_support::assert_round_trips;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn request_round_trips() {
        assert_round_trips(&LinkPreviewResolveRequest {
            url: "https://example.com/article".into(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn resolved_reply_round_trips_with_and_without_image() {
        assert_round_trips(&LinkPreviewResolveReply::Resolved {
            title: "An Article".into(),
            description: "Some description.".into(),
            image_hash: Some("aa01bb02".into()),
        });
        // No preview image → None → null on the wire → round-trips to None.
        let bare = LinkPreviewResolveReply::Resolved {
            title: "No Image".into(),
            description: String::new(),
            image_hash: None,
        };
        let decoded: LinkPreviewResolveReply = decode(&encode_canonical(&bare).unwrap()).unwrap();
        assert_eq!(bare, decoded);
        match decoded {
            LinkPreviewResolveReply::Resolved { image_hash, .. } => assert!(image_hash.is_none()),
            _ => panic!("expected Resolved variant"),
        }
    }

    #[test]
    fn failed_reply_round_trips() {
        assert_round_trips(&LinkPreviewResolveReply::Failed);
    }

    #[test]
    fn reply_variants_are_outcome_tag_discriminated() {
        // The `outcome` tag distinguishes the shapes on the wire — even the unit `Failed`
        // variant serializes to a map carrying `outcome: "failed"`.
        let bytes = encode_canonical(&LinkPreviewResolveReply::Failed).unwrap();
        let value: Value = decode(&bytes).unwrap();
        let outcome = match &value {
            Value::Map(entries) => match entries.get("outcome") {
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        };
        assert_eq!(outcome.as_deref(), Some("failed"));

        let bytes = encode_canonical(&LinkPreviewResolveReply::Resolved {
            title: "t".into(),
            description: "d".into(),
            image_hash: None,
        })
        .unwrap();
        let value: Value = decode(&bytes).unwrap();
        let outcome = match &value {
            Value::Map(entries) => match entries.get("outcome") {
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        };
        assert_eq!(outcome.as_deref(), Some("resolved"));
    }
}
