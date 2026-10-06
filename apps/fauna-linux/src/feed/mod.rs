//! Linux glue for the shared `fauna_feed::FeedManager` snapshot — the Feed
//! page's process-wide manager (`host`) and its GTK observer bridge
//! (`observer`), the direct analogue of `crate::conversations`
//! (`docs/goal/ui/feed.md` § State & data shape, ratified 2026-06-14).
//!
//! The Feed *view* lives in `crate::views::feed`; this module owns the manager
//! instance + the notify→main-thread bridge it renders from. Unlike
//! `ConversationsManager` (a no-arg singleton), `FeedManager::new` needs the
//! authed `Arc<NestClient>` + the local actor's signing secret, so `host` is
//! init-on-auth + swappable (re-auth rebuilds it) rather than a `OnceLock`.

pub mod drafts;
pub mod host;
pub mod observer;
pub mod post_media;
pub mod viewport;
