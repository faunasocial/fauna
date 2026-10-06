//! The shared-Rust **author seam** for browser-openable share links
//! (`docs/goal/behavior/share-links.md` § Where logic lives): everything an app
//! needs to make a link to one of its files, list the links it made, copy one
//! again, and kill one — so no per-app code ever touches a `ShareToken`.
//!
//! Two layers, like its `fauna-client-*` siblings:
//! - **Pure functions** — [`mint_link`] (mint + sign the token, seal its
//!   filename, build the registration and the URL), [`link_url`] (the verified
//!   re-derivation Copy uses), [`link_rows`] (a list reply → rendered rows),
//!   [`link_state`], [`share_link_eligible`], and the [`EXPIRY_OPTIONS`]. These
//!   are what a page machine composes, so the reveal-after-registration rule
//!   ([`mint_link`]'s doc) stays a property of the flow, not of a transport.
//! - **The typed calls** — [`ShareClient`], a thin `RpcRequester` wrapper over
//!   `fauna.share.{create,list,revoke}` (wasm-clean; native passes
//!   `Arc<NestClient>`, the SPA its `WsRpcClient`).
//!
//! Two kinds of link: a public-audience folder's file ([`mint_link`]), and a
//! private (owner-only) folder's file behind a fragment-keyed link
//! ([`mint_private_link`], `share-links.md` § The private-file extension) —
//! the nest holds only ciphertext and a key envelope sealed under a key that
//! lives in the URL fragment. The viewer side of the same link is
//! [`open_envelope`] + [`KeyEnvelope::open_file`], re-exported here so the
//! viewer's wasm and every app call one crate; the browser viewer page's whole
//! flow — what it reads off the address bar, what it fetches, what it may
//! preview, what it says — is [`viewer`].

use fauna_core::chunk::ChunkManifest;
use fauna_core::crypto::BackupKey;
use fauna_core::file_download::FileDownloadKeys;
use fauna_core::identity::ActorKeypair;
use fauna_core::label_custody;
use fauna_core::path_crypto::{LabelRoot, SealedLabelRender};
use fauna_core::share::{ShareToken, token_id_from_base64url};
use fauna_protocol::RpcRequester;
use fauna_protocol::share::{
    ShareCreateReply, ShareCreateRequest, ShareListReply, ShareListRequest, ShareRecord,
    ShareRevokeReply, ShareRevokeRequest,
};

pub use fauna_core::share::{DEFAULT_EXPIRY_SECS, KeyEnvelope, LINK_KEY_LEN, decode_link_key};
pub use fauna_protocol::share;

pub mod viewer;

// ── Expiry ──────────────────────────────────────────────────────────────────

/// One choice of the create surface's `share-link-expiry-select`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpiryOption {
    /// The stable select value (`"1d"`, `"7d"`, `"30d"`, `"1y"`) — also the
    /// i18n key suffix apps label it with.
    pub value: &'static str,
    /// The link lifetime, seconds.
    pub secs: u64,
}

const DAY: u64 = 24 * 60 * 60;

/// The four expiries, shortest first (`share-links.md` § Expiry). There is
/// deliberately no "never": a link whose registry row is lost is bounded only
/// by its expiry.
pub const EXPIRY_OPTIONS: [ExpiryOption; 4] = [
    ExpiryOption {
        value: "1d",
        secs: DAY,
    },
    ExpiryOption {
        value: "7d",
        secs: DEFAULT_EXPIRY_SECS,
    },
    ExpiryOption {
        value: "30d",
        secs: 30 * DAY,
    },
    ExpiryOption {
        value: "1y",
        secs: 365 * DAY,
    },
];

/// The default choice's value — the option whose lifetime is
/// [`DEFAULT_EXPIRY_SECS`].
pub const DEFAULT_EXPIRY: &str = "7d";

/// The lifetime a select value names, or `None` for a value that is not one of
/// the [`EXPIRY_OPTIONS`] (never a silent default — the caller refuses it).
pub fn expiry_secs(value: &str) -> Option<u64> {
    EXPIRY_OPTIONS
        .iter()
        .find(|o| o.value == value)
        .map(|o| o.secs)
}

// ── Eligibility ─────────────────────────────────────────────────────────────

/// What eligibility is decided from — the facts about the file's folder.
///
/// Every fact defaults to `false` — fail-closed: a caller that does not know a
/// fact offers no link on its strength.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShareFolderFacts {
    /// The folder's audience is `public` — as VERIFIED at the control-plane
    /// seam (`MediaFolder::rests_unsealed`, the owner's attestation), never the
    /// nest's bare claim.
    pub public_audience: bool,
    /// The folder is owner-only: bound to no group, shared with no named
    /// person. A bound folder's file is never linkable — its chunks are sealed
    /// under the group's key, which a link must not hand to a stranger
    /// (`share-links.md` § The private-file extension, *Eligibility*).
    pub unbound: bool,
    /// This seat holds the root the folder's chunks are sealed under (the
    /// owner's `convergent_chunk_root`, or a predecessor's) — the root
    /// [`mint_private_link`] derives the link's chunk keys from.
    pub owner_root_held: bool,
}

/// Whether "Share a link" is offered for a file in this folder: exactly where
/// the nest will serve the result (`share-links.md` § Which files can be
/// linked): a public-audience folder ([`mint_link`]), or an owner-only folder
/// whose root this seat holds ([`mint_private_link`]).
pub fn share_link_eligible(folder: &ShareFolderFacts) -> bool {
    folder.public_audience || (folder.unbound && folder.owner_root_held)
}

// ── States ──────────────────────────────────────────────────────────────────

/// A listed link's state (`share-links.md` § Flows → List).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    Active,
    Expired,
    Revoked,
}

impl LinkState {
    /// The stable value an app keys its label (and an e2e test its assertion)
    /// on: `"active"` / `"expired"` / `"revoked"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
        }
    }
}

/// Derive a record's state from `revoked`, `expires_at` and the clock. Revoked
/// wins over expired: it is the author's own act, and the more useful record.
pub fn link_state(record: &ShareRecord, now_secs: i64) -> LinkState {
    if record.revoked {
        LinkState::Revoked
    } else if record.expires_at <= now_secs {
        LinkState::Expired
    } else {
        LinkState::Active
    }
}

// ── The author ──────────────────────────────────────────────────────────────

/// The signing + sealing identity a link is made under: the account key the
/// app session already holds, and the nest the links point at.
pub struct ShareAuthor {
    keypair: ActorKeypair,
    backup_key: BackupKey,
    predecessors: Vec<BackupKey>,
    nest_url: String,
}

impl ShareAuthor {
    /// `secret` is the account's 32-byte identity secret; `nest_url` the base
    /// URL the links are served from (`{nest_url}/share/{token}`).
    pub fn new(secret: [u8; 32], nest_url: impl Into<String>) -> Self {
        Self {
            keypair: ActorKeypair::from_secret(secret),
            backup_key: BackupKey::derive(&secret),
            predecessors: Vec::new(),
            nest_url: nest_url.into().trim_end_matches('/').to_string(),
        }
    }

    /// Offer the retired owner keys this account succeeded from as
    /// filename-open candidates (read only — a seal never uses them).
    pub fn with_predecessors(mut self, predecessors: Vec<BackupKey>) -> Self {
        self.predecessors = predecessors;
        self
    }

    fn read_keys(&self) -> FileDownloadKeys {
        label_custody::LabelCustody::owner_only(self.backup_key.clone())
            .with_predecessors(self.predecessors.clone())
            .owner_plane_read_keys()
    }

    fn url_for(&self, token: &str) -> String {
        format!("{}/share/{token}", self.nest_url)
    }

    /// The roots an owner-only folder's chunks may be sealed under: this
    /// account's, then each predecessor's (a file synced before succession).
    fn owner_chunk_roots(&self) -> impl Iterator<Item = [u8; 32]> + '_ {
        core::iter::once(&self.backup_key)
            .chain(&self.predecessors)
            .map(BackupKey::convergent_chunk_root)
    }
}

// ── Mint ────────────────────────────────────────────────────────────────────

/// The file a link names: its **current version's** manifest (a link names
/// bytes, never a path — `share-links.md` § What a link is) and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkFile {
    pub manifest_hash: [u8; 32],
    pub filename: String,
}

/// A minted, NOT-yet-registered link. Its URL is private to this struct until
/// the caller has a successful registration in hand — [`Self::reveal`] takes
/// that reply as its proof. Dropping it discards the token; it is never
/// displayed and never logged (no `Debug`).
pub struct MintedLink {
    request: ShareCreateRequest,
    url: String,
}

impl MintedLink {
    /// The `fauna.share.create` payload to register.
    pub fn request(&self) -> &ShareCreateRequest {
        &self.request
    }

    /// The URL, released only against the registration's reply (which must
    /// be for this very token).
    pub fn reveal(self, reply: &ShareCreateReply) -> Option<String> {
        let id = token_id_from_base64url(&self.request.token).ok()?;
        (reply.share.token_id == hex::encode(id)).then_some(self.url)
    }
}

/// Errors minting a link (before anything reaches the wire).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MintError {
    /// The expiry is out of range for the clock (`now + lifetime` overflows).
    Expiry,
    /// Signing or sealing failed (never expected for these shapes).
    Crypto(String),
    /// A private link's manifest is not one this seat can link: a plaintext
    /// manifest (the nest refuses a fragment-keyed token over one), or sealed
    /// hashes no root this seat holds opens.
    Manifest(String),
}

impl core::fmt::Display for MintError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Expiry => f.write_str("share link expiry out of range"),
            Self::Crypto(e) => write!(f, "share link mint failed: {e}"),
            Self::Manifest(e) => write!(f, "share link manifest unusable: {e}"),
        }
    }
}

/// Sign `token`, seal `filename` for the author's list under the token id
/// (`label_custody::seal_share_filename` — every registration carries it,
/// public and private alike), and build the registration with `key_envelope`.
/// Returns the request and the bare URL.
fn sign_and_seal(
    author: &ShareAuthor,
    token: &ShareToken,
    filename: &str,
    key_envelope: Option<Vec<u8>>,
) -> Result<(ShareCreateRequest, String), MintError> {
    let token = token
        .to_base64url(&author.keypair)
        .map_err(|e| MintError::Crypto(e.to_string()))?;
    let token_id = token_id_from_base64url(&token).map_err(|e| MintError::Crypto(e.to_string()))?;
    let sealed = label_custody::seal_share_filename(
        &LabelRoot::owner_of(&author.backup_key),
        &token_id,
        filename,
    )
    .map_err(|e| MintError::Crypto(e.to_string()))?;
    let url = author.url_for(&token);
    Ok((
        ShareCreateRequest {
            token,
            filename_sealed: serde_bytes::ByteBuf::from(sealed),
            key_envelope: key_envelope.map(serde_bytes::ByteBuf::from),
            ..Default::default()
        },
        url,
    ))
}

/// Mint + sign a public link's token, seal its filename for the author's list
/// (salt = the token id; `label_custody::seal_share_filename`), and build the
/// registration. **The URL is revealed only after registration succeeds**
/// (`share-links.md` § Flows → Create, step 4): a token serves statelessly for
/// a live author whether or not it is registered, and only a registered token
/// can be revoked, so a URL shown earlier would be a link its author can never
/// kill. Hence [`MintedLink::reveal`].
pub fn mint_link(
    author: &ShareAuthor,
    file: &LinkFile,
    lifetime_secs: u64,
    now_secs: u64,
) -> Result<MintedLink, MintError> {
    let expires = now_secs
        .checked_add(lifetime_secs)
        .ok_or(MintError::Expiry)?;
    let token = ShareToken::new(
        file.manifest_hash,
        author.keypair.actor_id(),
        file.filename.clone(),
        expires,
        true,
    );
    let (request, url) = sign_and_seal(author, &token, &file.filename, None)?;
    Ok(MintedLink { request, url })
}

/// Mint a **fragment-keyed private link** to a file in an owner-only folder
/// (`share-links.md` § The private-file extension). `manifest` is the
/// version's manifest as fetched — the sealed wire form, whose hashes open
/// under the owner root this seat holds (or a predecessor's).
///
/// A fresh random link key seals a [`KeyEnvelope`] of exactly this file's
/// chunk keys, hashes and name; the token declares `key_in_fragment` and names
/// no file (its path is in every request log — the name rides the envelope and
/// the sealed list field instead). The registration carries the envelope; the
/// key appears only in the revealed URL's fragment (`<url>#<key>`), which a
/// browser never sends — so the nest that serves the link holds ciphertext
/// and an envelope it cannot open. The reveal-after-registration rule is
/// [`mint_link`]'s.
pub fn mint_private_link(
    author: &ShareAuthor,
    file: &LinkFile,
    manifest: ChunkManifest,
    lifetime_secs: u64,
    now_secs: u64,
) -> Result<MintedLink, MintError> {
    let expires = now_secs
        .checked_add(lifetime_secs)
        .ok_or(MintError::Expiry)?;
    if manifest.sealed_hashes.is_none() {
        return Err(MintError::Manifest(
            "a plaintext manifest takes a public link, not a fragment-keyed one".into(),
        ));
    }
    let (root, opened) = author
        .owner_chunk_roots()
        .find_map(|root| Some((root, manifest.clone().unseal_hashes(&root).ok()?)))
        .ok_or_else(|| MintError::Manifest("no owner root this seat holds opens it".into()))?;
    let envelope = KeyEnvelope::derive(
        &root,
        file.filename.clone(),
        opened.file_hash,
        opened.chunk_hashes,
    );
    let link_key = fauna_core::share::generate_link_key();
    let sealed = envelope
        .seal(&link_key)
        .map_err(|e| MintError::Crypto(e.to_string()))?;
    let token = ShareToken::fragment_keyed(file.manifest_hash, author.keypair.actor_id(), expires);
    let (request, url) = sign_and_seal(author, &token, &file.filename, Some(sealed))?;
    let url = format!("{url}#{}", fauna_core::share::encode_link_key(&link_key));
    Ok(MintedLink { request, url })
}

/// The viewer's first step: open a private link's stored envelope with the
/// key from the URL fragment (a leading `#` is tolerated). Then
/// [`KeyEnvelope::open_file`] over the ciphertext chunks, in manifest order,
/// yields the verified file. Fails closed on a malformed or wrong key.
pub fn open_envelope(fragment: &str, sealed_envelope: &[u8]) -> Result<KeyEnvelope, String> {
    let key = decode_link_key(fragment).map_err(|e| e.to_string())?;
    KeyEnvelope::open(&key, sealed_envelope).map_err(|e| e.to_string())
}

// ── List + re-derive ────────────────────────────────────────────────────────

/// A registered link's URL, **re-derived and verified** — the nest keeps a
/// token's id, never the token, so Copy re-mints it from the row's fields
/// (signatures are deterministic) and hands it out only when its id equals the
/// row's `token_id` (`share-links.md` § Flows → List). `None` on any mismatch —
/// a token minted by a future client with fields this one does not know — and
/// for every link whose URL cannot be re-derived at all: a fragment-keyed one
/// (its key lives only in the URL the author copied), one not by this author,
/// or one with no renderable name.
pub fn link_url(author: &ShareAuthor, record: &ShareRecord, filename: &str) -> Option<String> {
    if record.key_in_fragment || !record.public {
        return None;
    }
    let manifest_hash: [u8; 32] = hex::decode(&record.manifest_hash).ok()?.try_into().ok()?;
    let expires = u64::try_from(record.expires_at).ok()?;
    let mut token = ShareToken::new(
        manifest_hash,
        author.keypair.actor_id(),
        filename.to_string(),
        expires,
        record.public,
    );
    token.key_in_fragment = record.key_in_fragment;
    let encoded = token.to_base64url(&author.keypair).ok()?;
    let id = token_id_from_base64url(&encoded).ok()?;
    (hex::encode(id) == record.token_id).then(|| author.url_for(&encoded))
}

/// One rendered list row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRow {
    /// The registry id (hex) — the revoke key.
    pub token_id: String,
    /// The file's name, rendered seal-first.
    pub filename: String,
    /// Expiry, Unix seconds.
    pub expires_at: i64,
    pub state: LinkState,
    /// The verified re-derived URL — `Some` only on an Active row whose URL
    /// re-derives ([`link_url`]); the Copy control's presence.
    pub url: Option<String>,
}

/// Render a `fauna.share.list` reply's records (newest first, as the nest
/// orders them) into rows: the name opened seal-first through the shared
/// label seam, the state derived, the URL re-derived for Active rows. A row
/// whose name this reader cannot render at all is omitted (the ratified
/// `SealedLabelRender::Omit` degrade), never shown nameless.
pub fn link_rows(author: &ShareAuthor, records: &[ShareRecord], now_secs: i64) -> Vec<LinkRow> {
    let keys = author.read_keys();
    records
        .iter()
        .filter_map(|record| {
            let token_id: [u8; 32] = hex::decode(&record.token_id).ok()?.try_into().ok()?;
            let filename = match label_custody::render_share_filename(
                &keys,
                &record.filename_sealed,
                &token_id,
            ) {
                SealedLabelRender::Sealed(s) | SealedLabelRender::Plaintext(s) => s,
                SealedLabelRender::Omit => return None,
            };
            let state = link_state(record, now_secs);
            let url = match state {
                LinkState::Active => link_url(author, record, &filename),
                _ => None,
            };
            Some(LinkRow {
                token_id: record.token_id.clone(),
                filename,
                expires_at: record.expires_at,
                state,
                url,
            })
        })
        .collect()
}

// ── Typed calls ─────────────────────────────────────────────────────────────

/// Typed `fauna.share.*` call surface, generic over the WS-RPC transport.
/// Errors propagate as the transport's `R::Error`.
pub struct ShareClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> ShareClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.share.create` — register a minted link.
    pub async fn register(
        &self,
        request: ShareCreateRequest,
    ) -> Result<ShareCreateReply, R::Error> {
        self.nest.request("fauna.share.create", request).await
    }

    /// `fauna.share.list` — the caller's registered links, newest first.
    pub async fn list(&self) -> Result<Vec<ShareRecord>, R::Error> {
        let reply: ShareListReply = self
            .nest
            .request(
                "fauna.share.list",
                ShareListRequest {
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.shares)
    }

    /// `fauna.share.revoke` — kill one of the caller's links (no un-revoke).
    pub async fn revoke_link(&self, token_id: &str) -> Result<(), R::Error> {
        let _: ShareRevokeReply = self
            .nest
            .request(
                "fauna.share.revoke",
                ShareRevokeRequest {
                    token_id: token_id.to_string(),
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// The whole create flow over this transport: mint, register, and reveal
    /// the URL only on success. On a failed registration the minted token is
    /// dropped here — never returned, displayed or logged.
    pub async fn create_link(
        &self,
        author: &ShareAuthor,
        file: &LinkFile,
        lifetime_secs: u64,
        now_secs: u64,
    ) -> Result<(ShareRecord, String), CreateError<R::Error>> {
        let minted = mint_link(author, file, lifetime_secs, now_secs).map_err(CreateError::Mint)?;
        self.register_and_reveal(minted).await
    }

    /// [`Self::create_link`]'s private-file twin over [`mint_private_link`]:
    /// the revealed URL carries the link key in its fragment.
    pub async fn create_private_link(
        &self,
        author: &ShareAuthor,
        file: &LinkFile,
        manifest: ChunkManifest,
        lifetime_secs: u64,
        now_secs: u64,
    ) -> Result<(ShareRecord, String), CreateError<R::Error>> {
        let minted = mint_private_link(author, file, manifest, lifetime_secs, now_secs)
            .map_err(CreateError::Mint)?;
        self.register_and_reveal(minted).await
    }

    async fn register_and_reveal(
        &self,
        minted: MintedLink,
    ) -> Result<(ShareRecord, String), CreateError<R::Error>> {
        let reply = self
            .register(minted.request().clone())
            .await
            .map_err(CreateError::Rpc)?;
        let record = reply.share.clone();
        let url = minted.reveal(&reply).ok_or(CreateError::Mismatch)?;
        Ok((record, url))
    }

    /// The whole list flow: fetch, then render ([`link_rows`]).
    pub async fn list_links(
        &self,
        author: &ShareAuthor,
        now_secs: i64,
    ) -> Result<Vec<LinkRow>, R::Error> {
        Ok(link_rows(author, &self.list().await?, now_secs))
    }
}

/// Why [`ShareClient::create_link`] failed.
#[derive(Debug)]
pub enum CreateError<E> {
    Mint(MintError),
    Rpc(E),
    /// The nest answered for a different token than the one sent — the URL is
    /// withheld (it would be a link whose registration is not the one shown).
    Mismatch,
}

#[cfg(test)]
mod tests;
