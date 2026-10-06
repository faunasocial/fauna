//! The **viewer side** of a fragment-keyed private link
//! (`docs/goal/behavior/share-links.md` § The private-file extension → *The
//! viewer is a browser page and needs no account*): everything the browser page
//! `GET /share/<token>` serves decides, so the page itself is a thin shell over
//! this module's wasm (`libs/fauna-wasm-share`) and no rule of the four lives
//! in TypeScript.
//!
//! The flow is [`viewer_start`] (the address bar → a link to open, or the
//! generic page an unfurler gets) → fetch [`manifest_path`] → [`open_share`]
//! (verifies the token's signature, that the manifest is the one the token
//! names, and opens the envelope under the fragment's key) → fetch every
//! [`chunk_path`] → [`OpenedShare::assemble`] (every chunk against its
//! plaintext hash, the whole against the file hash). The paths are
//! same-origin and carry no fragment, so nothing the viewer requests can carry
//! the key (rule 1). [`preview_kind`] is rule 2's allow-list, decided over
//! [`fauna_core::share::content_type_for_filename`], the one type oracle.

use fauna_core::chunk::ChunkManifest;
use fauna_core::data::ContentHash;
use fauna_core::share::{KeyEnvelope, ShareToken, content_type_for_filename};
use fauna_i18n::strings::{share_viewer, size};

use crate::open_envelope;

/// Where the page starts, read off the address bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerStart {
    /// No link to open: no fragment (an unfurler, a chat app's pre-fetch, a
    /// person who copied only the path) or no `/share/<token>` path. The page
    /// shows the generic "a file shared with Fauna" text and nothing about
    /// the file.
    Generic,
    /// A link to open: the token from the path and the key from the fragment.
    Open { token: String, fragment: String },
}

/// Read the page's address: `pathname` is `location.pathname`, `hash` is
/// `location.hash` (a leading `#` tolerated). The page never writes either
/// back (rule 4) — this only reads.
pub fn viewer_start(pathname: &str, hash: &str) -> ViewerStart {
    let fragment = hash.strip_prefix('#').unwrap_or(hash);
    let token = pathname
        .strip_prefix("/share/")
        .map(|rest| rest.trim_end_matches('/'))
        .filter(|t| !t.is_empty() && !t.contains('/'));
    match token {
        Some(token) if !fragment.is_empty() => ViewerStart::Open {
            token: token.to_string(),
            fragment: fragment.to_string(),
        },
        _ => ViewerStart::Generic,
    }
}

/// The same-origin path of a private link's manifest + envelope.
pub fn manifest_path(token: &str) -> String {
    format!("/share/{token}/manifest")
}

/// The same-origin path of a private link's `index`-th ciphertext chunk.
pub fn chunk_path(token: &str, index: usize) -> String {
    format!("/share/{token}/chunk/{index}")
}

/// How the viewer may show a file inline (rule 2): only what a browser
/// renders without executing anything. Everything else is download-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    /// An `<img>` over an object URL.
    Image,
    /// An `<audio>` over an object URL.
    Audio,
    /// A `<video>` over an object URL.
    Video,
    /// The bytes as text in a text element (never parsed as markup).
    Text,
    /// Download only.
    None,
}

impl PreviewKind {
    /// The stable name the page switches on.
    pub fn as_str(self) -> &'static str {
        match self {
            PreviewKind::Image => "image",
            PreviewKind::Audio => "audio",
            PreviewKind::Video => "video",
            PreviewKind::Text => "text",
            PreviewKind::None => "none",
        }
    }
}

/// Rule 2's allow-list over the one type oracle. SVG is an image type that is
/// also a document a browser can script, so it is download-only; of the text
/// types only `text/plain` previews (HTML, XML, CSS, CSV and Markdown are
/// documents or render as such).
pub fn preview_kind(filename: &str) -> PreviewKind {
    let content_type = content_type_for_filename(filename);
    if content_type == "image/svg+xml" {
        PreviewKind::None
    } else if content_type.starts_with("image/") {
        PreviewKind::Image
    } else if content_type.starts_with("audio/") {
        PreviewKind::Audio
    } else if content_type.starts_with("video/") {
        PreviewKind::Video
    } else if content_type == "text/plain" {
        PreviewKind::Text
    } else {
        PreviewKind::None
    }
}

/// Why a link could not be opened. Each maps to one plain sentence
/// ([`ViewerError::text`]); none says anything about the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerError {
    /// The token in the path does not verify, or is not a private link's.
    NotALink,
    /// The key does not open the envelope, the manifest is not the one the
    /// token names, or a chunk or the whole fails its hash — a truncated or
    /// altered link, or altered bytes.
    Damaged,
}

impl ViewerError {
    pub fn text(&self) -> &'static str {
        match self {
            ViewerError::NotALink => share_viewer::NOT_FOUND,
            ViewerError::Damaged => share_viewer::DAMAGED,
        }
    }
}

/// A private link whose envelope is open: what the page shows before the
/// chunks arrive, and the keys that open them.
#[derive(Debug, Clone)]
pub struct OpenedShare {
    envelope: KeyEnvelope,
    total_size: u64,
}

/// Open a private link: verify the token (its signature, and that it declares
/// the fragment key), check that `fragment_manifest` — the canonical
/// `ShareFragmentManifest` the manifest path answers — carries the manifest
/// whose hash the signed token names, and open the envelope under the
/// fragment's key. The integrity chain is the token's signature → the manifest
/// hash → the manifest → the envelope's hashes (whose AEAD tag is the
/// author's).
pub fn open_share(
    token: &str,
    fragment: &str,
    fragment_manifest: &[u8],
) -> Result<OpenedShare, ViewerError> {
    let token = ShareToken::from_base64url(token).map_err(|_| ViewerError::NotALink)?;
    if !token.key_in_fragment {
        return Err(ViewerError::NotALink);
    }
    let reply: crate::share::ShareFragmentManifest =
        fauna_protocol::decode_strict(fragment_manifest).map_err(|_| ViewerError::Damaged)?;
    if ContentHash::of_raw(&reply.manifest).digest() != token.manifest_hash {
        return Err(ViewerError::Damaged);
    }
    let manifest: ChunkManifest = fauna_core::encoding::canonical_decode(&reply.manifest)
        .map_err(|_| ViewerError::Damaged)?;
    let envelope =
        open_envelope(fragment, &reply.key_envelope).map_err(|_| ViewerError::Damaged)?;
    if envelope.chunk_hashes.len() != manifest.chunk_sizes.len() {
        return Err(ViewerError::Damaged);
    }
    Ok(OpenedShare {
        envelope,
        total_size: manifest.total_size,
    })
}

impl OpenedShare {
    /// The file's name, from the envelope (the token carries none).
    pub fn filename(&self) -> &str {
        &self.envelope.filename
    }

    /// The file's size, as the manifest states it.
    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    /// The size as the page shows it (`value-formatting.md` § Byte sizes).
    pub fn size_text(&self) -> String {
        fauna_core::format::byte_size(self.total_size).resolve(|key| match key {
            "size.bytes" => Some(size::BYTES),
            "size.kb" => Some(size::KB),
            "size.mb" => Some(size::MB),
            "size.gb" => Some(size::GB),
            "size.tb" => Some(size::TB),
            _ => None,
        })
    }

    /// How many chunks to fetch, indices `0..chunk_count`.
    pub fn chunk_count(&self) -> usize {
        self.envelope.chunk_hashes.len()
    }

    /// The type the download and any preview carry.
    pub fn content_type(&self) -> &'static str {
        content_type_for_filename(&self.envelope.filename)
    }

    /// Rule 2's decision for this file.
    pub fn preview(&self) -> PreviewKind {
        preview_kind(&self.envelope.filename)
    }

    /// The verified file from its ciphertext chunks, in index order — or
    /// nothing: no caller ever holds unverified bytes.
    pub fn assemble(&self, ciphertexts: &[Vec<u8>]) -> Result<Vec<u8>, ViewerError> {
        self.envelope
            .open_file(ciphertexts)
            .map_err(|_| ViewerError::Damaged)
    }
}

/// The sentence for an HTTP refusal of one of the viewer's own fetches —
/// the nest's answer said plainly, never retried.
pub fn status_text(status: u16) -> &'static str {
    match status {
        410 => share_viewer::GONE,
        451 => share_viewer::WITHHELD,
        404 | 403 => share_viewer::NOT_FOUND,
        _ => share_viewer::UNAVAILABLE,
    }
}

/// The page's fixed text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerText {
    pub title: &'static str,
    pub generic_body: &'static str,
    pub loading: &'static str,
    pub download: &'static str,
    pub keep_note: &'static str,
}

pub fn viewer_text() -> ViewerText {
    ViewerText {
        title: share_viewer::TITLE,
        generic_body: share_viewer::GENERIC_BODY,
        loading: share_viewer::LOADING,
        download: share_viewer::DOWNLOAD,
        keep_note: share_viewer::KEEP_NOTE,
    }
}
