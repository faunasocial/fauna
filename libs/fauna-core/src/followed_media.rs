//! The **followed browse scope** seam — how the Media page reaches a followed
//! public folder's listing (`docs/goal/ui/media.md` § Followed public folders;
//! the follow itself: `docs/goal/behavior/folders.md` § Publicly-synced follow).
//!
//! Same split as [`crate::folder_keys::FolderKeyResolver`]: the trait and its
//! record types live here at the dependency floor so `fauna-media-machine` can
//! consume them, while the production impl lives up-stack with the transport —
//! `fauna_devices_machine`'s `StoreFollowedFoldersSource`, the one object that
//! already holds the follow records and the availability cache for the Folders
//! page. **Both machines are meant to share that one instance**, so a Media
//! browse fetch feeds the same cached availability verdict the Folders-page
//! probe reads — one mechanism, never two caches racing.
//!
//! A followed folder is deliberately **not** a set in the Media aggregate: its
//! rows live on its home nest, the follower's nest keeps no copy, and the read
//! is keyless end to end. The machine treats this seam as the *only* door to
//! followed content — never the name-keyed custody pipeline (`ui/media.md`
//! architectural rule 6).

use serde::{Deserialize, Serialize};

/// One followed public folder, as the Media page offers it: a browse scope.
///
/// `(home_nest_url, folder_id)` is the follow's full identity — `folder_id`
/// alone can collide across home nests. The identity rides this record (the
/// scope), never the listing rows inside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FollowedMediaScope {
    /// The home nest's stable `folders.id` — the pinned address every read
    /// after the first uses.
    pub folder_id: i64,
    /// The folder's home nest base URL; empty ⇒ homed on the user's own nest
    /// (the same-nest follow).
    pub home_nest_url: String,
    /// The home nest's identity (`nest_actor_id`, 64-hex) as the public-fetch
    /// reply stamped it — the trust root a cross-nest follower's byte-plane
    /// dial is verified against (`security.md` § Transport trust, the
    /// federation-granted row; `FollowedFolder::home_nest_actor_id`). `None`
    /// for a same-nest follow, or when the home stamped none (the dial then
    /// keeps the WebPKI floor). Rides the scope because the download is the
    /// scope's, never the item's.
    #[serde(default)]
    pub home_nest_actor_id: Option<String>,
    /// Hex owner actor id.
    pub owner_actor_id: String,
    /// The owner string the Folders page's followed row shows — the handle
    /// while it still names the owner, else the actor id's short form
    /// (`FollowedFolderSummary::owner_display`, whose source fills both). The
    /// filter option's label appends it, disambiguating a display name that
    /// collides with one of the user's own sets (`ui/media.md` § Followed
    /// public folders). Empty only from a source that fills none, in which
    /// case the label falls back to the short id itself.
    #[serde(default)]
    pub owner_display: String,
    /// The folder's plaintext name as of the last successful read (public
    /// names are world-readable by the ratified exception).
    pub display_name: String,
    /// Whether the home nest still served this folder at the last verdict.
    /// `false` is the revoke rendering — the scope stays offered, loudly.
    pub available: bool,
}

/// One renderable file in a followed folder's listing — the head fold of the
/// public change log (`fauna_client_folders::public_follow::listing_from_changes`
/// produces these; the type lives here so the Media machine can consume it,
/// exactly like [`crate::folder_keys::ResolvedFolderKeys`]).
///
/// The Media followed browse scope renders these as ordinary `media-item` rows
/// (`docs/goal/ui/media.md` § Followed public folders); the follow's identity
/// rides the *scope*, never this record, which is why it carries no
/// `(home_nest_url, folder_id)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowedFileEntry {
    /// Folder-relative plaintext path. Public rows carry it in the clear —
    /// `path_sealed` is one of the stripped fields, and a follower holds no key
    /// that could open a seal anyway.
    pub path: String,
    /// Hex BLAKE3 of the path — the row's stable identity (present on every
    /// change row), and the key the fold groups by.
    pub path_hash: String,
    /// Hex BLAKE3 manifest hash of the current head version — what
    /// [`crate::file_download::download_followed_file`] takes.
    pub manifest_hash: String,
    /// Stored size in bytes, as the head change recorded it.
    pub size_bytes: i64,
    /// The head change's `created_at` (unix seconds) — the listing's date.
    pub updated_at: i64,
    /// The uploader-recorded thumbnail pointer, when any. ⚠ Whether a public
    /// folder's thumbnail *blobs* rest plaintext (i.e. whether a keyless
    /// follower can render one) is NOT yet verified — `ui/media.md` § Followed
    /// public folders; carry the pointer, gate the render on that answer.
    pub thumbnail_hash: Option<String>,
    /// The head change's `seq`, should a caller want a watermark.
    pub seq: i64,
}

/// Why a followed listing fetch yielded no rows — the same two-way split every
/// follow surface keeps (`fauna_client_folders::public_follow::FollowError`):
/// the plane's own refusal is an answer about the folder, a transport fault is
/// not, and rendering them alike would show a revoke on every dropped
/// connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowedFetchError {
    /// The plane's folded `not_found` — flipped back, deleted, or never there.
    /// The scope renders its loud *no longer available* state.
    Unavailable,
    /// A transport fault (display detail). Not an answer about the folder.
    Transport(String),
}

impl std::fmt::Display for FollowedFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(f, "no public folder at that address"),
            Self::Transport(detail) => write!(f, "{detail}"),
        }
    }
}

/// The Media machine's source of followed browse scopes and their listings.
///
/// Optional and injected post-construction
/// (`MediaMachine::set_followed_media_source`, the `set_followed_folders_source`
/// pattern): unwired means no followed options — the correct render for an app
/// that has not built the surface. Best-effort like its sibling seams: an
/// unreadable config yields no scopes this pass, never a page error.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait FollowedMediaSource: crate::MaybeSendSync {
    /// The follows as of now, availability-decorated from the source's cached
    /// verdicts (the staleness-budgeted probe — the source owns the cache).
    async fn followed_scopes(&self) -> Vec<FollowedMediaScope>;

    /// One followed folder's current listing: public fetch + head fold.
    ///
    /// The fetch outcome **feeds the availability cache** — a served page is
    /// evidence of `available`, the plane's refusal is the revoke — so a browse
    /// never races a parallel probe with a contradicting verdict.
    async fn fetch_listing(
        &self,
        folder_id: i64,
        home_nest_url: &str,
    ) -> Result<Vec<FollowedFileEntry>, FollowedFetchError>;
}
