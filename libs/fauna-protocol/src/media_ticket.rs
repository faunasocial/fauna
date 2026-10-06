//! `fauna.media.playback_ticket` WS-RPC payload types — the mint half of the
//! media proxy's playback ticket (`docs/goal/architecture/render-model.md`
//! § D6c → *Inline playback*, answer 4).
//!
//! A `<video src>`, `AVPlayer`, `MediaPlayerElement` or `gtk::Video` cannot
//! carry a bearer header, so a bridged video playing through the nest's media
//! proxy (`GET /api/v1/media/proxy?url=…`) needs a credential that rides the
//! URL. The client asks its nest — over its one authenticated channel — to
//! ticket the proxied path a `ProxiedVideo` block carries; the nest appends
//! `exp` (a unix second) and `sig` (an HMAC over the proxied url and `exp`
//! under a nest-only secret) and hands the path back. The shared
//! `FeedManager::playback_source` projection consumes it, so no app builds the
//! URL by hand. The secret's custody is
//! `key-material-hierarchy.md` § Audience: deployment infrastructure →
//! *Media playback-ticket secret*.
//!
//! Wire convention (matching `linkpreview.rs`): request and reply structs carry
//! the `#[serde(flatten, default)] extra` forward-compat map. A path that is not
//! this nest's media-proxy form is refused with a typed `RpcError`, never a
//! reply.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

/// The kind string for the playback-ticket mint. Registered (metadata only) in
/// `kind.rs::register_media_kinds`; the nest associates it with the handler.
pub const KIND_MEDIA_PLAYBACK_TICKET: &str = "fauna.media.playback_ticket";

/// Ticket one proxied media path for playback.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PlaybackTicketRequest {
    /// The nest-relative proxied path the block carries —
    /// `/api/v1/media/proxy?url=<percent-encoded remote url>`.
    pub path: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The ticketed path: the request's proxied path with `exp` and `sig` query
/// parameters appended, fetchable without a bearer until `exp`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PlaybackTicketReply {
    /// `/api/v1/media/proxy?url=…&exp=<unix seconds>&sig=<base64url>`.
    pub path: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::test_support::assert_round_trips;

    #[test]
    fn request_round_trips() {
        assert_round_trips(&PlaybackTicketRequest {
            path: "/api/v1/media/proxy?url=https%3A%2F%2Fcdn.example%2Fv.mp4".into(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn reply_round_trips() {
        assert_round_trips(&PlaybackTicketReply {
            path: "/api/v1/media/proxy?url=https%3A%2F%2Fcdn.example%2Fv.mp4&exp=1&sig=AA".into(),
            extra: BTreeMap::new(),
        });
    }
}
