//! A page's per-hash image-load-state cache — the spine every page that paints
//! fetched images hangs off (Media's `media-thumbnail`, the feed's
//! `post-image` and link-preview og:image, a revealed `doc-remote-image`).
//!
//! The bookkeeping itself is shared Rust, [`fauna_core::load_cache`], whose
//! module doc owns why a cache exists and what it deliberately is not; linux's
//! feed cards run on the same type. This module only names tui's payload: the
//! rasterized [`Thumbnail`], in both representations the two rendering arms
//! need (the cell art that turns into text, and the pixels a graphics protocol
//! emits). Each page keeps its own byte source (Media fetches a sealed
//! thumbnail by hash and decrypts under the owner `BackupKey`; the feed fetches
//! a post blob by hash and opens it through the manager) and folds the art in
//! through [`ImageCache::set`](fauna_core::load_cache::LoadCache::set) — or, where
//! the byte source can tell a transient failure apart (the feed's own-nest
//! reads), [`finish`](fauna_core::load_cache::LoadCache::finish), which forgets one.

use fauna_core::load_cache::{LoadCache, LoadState};

use crate::thumbnail::Thumbnail;

/// One image's load state — [`LoadState::Ready`] carries the rasterized art.
pub type ImageState = LoadState<Thumbnail>;

/// A page's `hash → `[`ImageState`] cache, hung off the page's state and so
/// dropped with the session.
pub type ImageCache = LoadCache<Thumbnail>;
