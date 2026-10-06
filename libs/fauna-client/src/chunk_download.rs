//! Fetching content-addressed blobs back off a nest's **public** download
//! routes, as an impl of the shared `fauna_core::file_download::BlobFetcher`
//! seam.
//!
//! Both routes this drives — `GET /api/v1/manifests/{hash}` and
//! `GET /api/v1/chunks/{hash}` — are open: they take no bearer, and the handler
//! (`bins/fauna-nest/src/chunk_routes.rs`) has no auth extractor at all.
//! Confidentiality is cryptographic, not transport-scoped — the bytes on the
//! byte plane are already sealed, and integrity rests on the content address
//! each blob is fetched *by*, not on who asked. The session's `AuthClient` is
//! reused anyway (its SPKI-pinned reqwest client + shared bearer cache) so this
//! opens no parallel connection and re-pins no TLS; the routes simply ignore the
//! bearer that rides along.
//!
//! Consumers today: the mail receive path
//! (`fauna-client-conversations::NestMailInboundSource`), which resolves an
//! over-frame `InboxMessage.body_ref` through
//! `fauna_mail::body_ref::resolve_referenced_mail_body`
//! (`docs/goal/behavior/smtp-server.md` § Message size limits).
//!
//! **The neutral native home.** `fauna-media-machine`'s `NativeDownloadFetcher`
//! was the same binding over the same two routes, built the same way from an
//! `AuthClient`, predating this one because media was the first caller — it
//! collapsed onto this crate (priority #2 dedup) since this crate owns
//! `NestClient` and already depends on `fauna-nest-http`.
//! `nest_api::ws_rpc::build_media_machine` in `fauna-media-machine` now
//! constructs [`NestPublicChunkFetcher`] directly. The wasm twin is
//! `fauna_core::file_download::WasmPublicChunkFetcher` (this crate is
//! native-only, so it can't hold that one — `fauna-core` compiles to wasm32
//! and both wasm consumers already depend on it).

use std::sync::Arc;

use anyhow::Context;
use fauna_core::data::ContentHash;
use fauna_core::file_download::BlobFetcher;
use fauna_nest_http::{BearerSource, NestContentApi, ReqwestNestContentApi, paths};

use crate::NestClient;

/// A streaming, actor-authenticated `GET` of one of the session's own nest
/// routes — the transport half of a download too large to buffer.
///
/// **Why this is here and not at the call site.** `fauna-nest-http`'s
/// `NestContentApi::get` is the buffered convenience every other caller wants;
/// a reader that must not hold its whole body in memory needs the response
/// object itself, and `reqwest` is this crate's dependency, not
/// `fauna-client-mail-settings`'. So the consumer (`mail-export.md` § Download
/// flow's opener, which streams a blob § Quota composition caps at 10 GiB)
/// takes slices from here and never names an HTTP type.
///
/// Rides the session's own pinned client and shared bearer cache
/// ([`AuthClient::http`] / [`AuthClient::bearer`] / [`AuthClient::nest_url`] —
/// the triple [`AuthClient::content_api`] otherwise assembles), so it opens no
/// second connection and re-pins no TLS.
pub struct NestAuthedByteStream {
    resp: reqwest::Response,
}

impl NestAuthedByteStream {
    /// The next slice of the body, or `None` at its end. Slice boundaries carry
    /// no meaning — a framed reader reassembles across them.
    pub async fn next_slice(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.resp.chunk().await?.map(|b| b.to_vec()))
    }
}

/// Open a streaming authenticated `GET {nest_url}{path}`.
///
/// `path` is a nest-minted absolute path (`/api/v1/...`), never one a caller
/// composes from user input. A non-2xx status is an error carrying the code:
/// the routes this drives answer `404` for "not yours", "not ready" and "not
/// there" alike — deliberately — so no caller may invent a distinction the nest
/// refused to make.
pub async fn open_authed_stream(
    auth: &crate::AuthClient,
    path: &str,
) -> anyhow::Result<NestAuthedByteStream> {
    let token = auth
        .bearer()
        .bearer()
        .await
        .map_err(|e| anyhow::anyhow!("authenticate {path}: {e}"))?;
    let url = format!("{}{}", auth.nest_url().trim_end_matches('/'), path);
    let resp = auth
        .http()
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .with_context(|| format!("GET {path}"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("GET {path}: the nest answered {status}");
    }
    Ok(NestAuthedByteStream { resp })
}

/// A [`BlobFetcher`] over a nest's public content-addressed download routes.
pub struct NestPublicChunkFetcher {
    content: ReqwestNestContentApi<Arc<dyn BearerSource>>,
}

impl NestPublicChunkFetcher {
    /// Build over `nest`'s authenticated session — its `AuthClient`'s pinned
    /// http client, shared bearer, and current nest URL.
    pub fn new(nest: &Arc<NestClient>) -> Self {
        Self {
            content: nest.auth().content_api(),
        }
    }
}

/// A [`BlobFetcher`] over a **foreign** nest's public download routes — the
/// native byte-plane leg of a cross-nest download: a foreign shared set's
/// manifest/chunk bytes live on its HOME nest (Phase 2 client read-side), and
/// so do a followed public folder's, so the fetch addresses that base URL
/// instead of the session's own nest.
///
/// Bearer-less, because the two routes are public (no auth extractor), the
/// bytes are sealed (or public by the owner's own choice), and **integrity
/// rests on the content address each blob is fetched by** — the posture the
/// module doc states for [`NestPublicChunkFetcher`]. **Not unpinned**: the
/// caller holds no account on the home nest, so no bearer handshake ever
/// graduates an SPKI pin for it, and a self-signed home (every LAN, loopback or
/// domainless box — the floor a nest always serves) would be refused by
/// WebPKI. Whatever named the home also delivered its identity —
/// `home_nest_actor_id` on the record: grant-stamped for a foreign shared set,
/// stamped by the home nest itself on the public-fetch reply for a follow
/// (which has no grant and no inviter, and no seal beneath the pin) — and
/// [`Self::for_home`] verifies the home against exactly that root before the
/// first byte is fetched — [`graduate_home_nest_pin`], the mechanism
/// `security.md` § Transport trust ratifies for the federation-granted row —
/// then dials through the pin-or-WebPKI [`crate::pinned_http_client`]. Built per
/// download by the `fauna-media-machine` foreign-fetcher factory.
pub struct ForeignPublicChunkFetcher {
    base_url: String,
    http: reqwest::Client,
    /// The record-delivered home identity, graduated lazily on the first fetch
    /// (`None` ⇒ nothing to graduate: the WebPKI floor, or a caller-supplied
    /// client).
    home_nest_actor_id: Option<String>,
    graduated: tokio::sync::OnceCell<()>,
}

impl ForeignPublicChunkFetcher {
    /// Build over the foreign nest's base URL (trailing slash trimmed), with no
    /// record-delivered identity: the pin-or-WebPKI client, which accepts a
    /// public-CA home and a home some earlier handshake already pinned, and
    /// refuses an un-graduated self-signed one. Prefer [`Self::for_home`]
    /// wherever the record carries the identity.
    pub fn new(base_url: &str) -> Self {
        Self::for_home(base_url, None)
    }

    /// Build over the foreign nest's base URL, verified against the identity
    /// the record delivered (`home_nest_actor_id`, 64-hex) on the first fetch:
    /// [`graduate_home_nest_pin`] runs the pre-identity channel-binding
    /// handshake against that root and pins the bound SPKI for the dial. A
    /// `None` identity keeps the WebPKI floor (never weaker); a malformed one
    /// fails the first fetch closed, before anything is dialed.
    pub fn for_home(base_url: &str, home_nest_actor_id: Option<&str>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: crate::pinned_http_client(base_url),
            home_nest_actor_id: home_nest_actor_id.map(str::to_string),
            graduated: tokio::sync::OnceCell::new(),
        }
    }

    /// [`Self::new`] with a caller-supplied reqwest client — for tests whose
    /// in-process "foreign nest" serves a self-signed floor cert and hands no
    /// identity to graduate against.
    pub fn with_http(base_url: &str, http: reqwest::Client) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
            home_nest_actor_id: None,
            graduated: tokio::sync::OnceCell::new(),
        }
    }

    /// Graduate the home's pin once, before the first byte is fetched. An
    /// `http://` home has no TLS to pin (the loopback test rig) and skips it,
    /// exactly as the sync agent's bind does.
    async fn ensure_graduated(&self) -> anyhow::Result<()> {
        let Some(actor_id) = self.home_nest_actor_id.as_deref() else {
            return Ok(());
        };
        if !self.base_url.starts_with("https://") {
            return Ok(());
        }
        self.graduated
            .get_or_try_init(|| graduate_home_nest_pin(&self.base_url, actor_id))
            .await
            .map(|_| ())
    }

    async fn get(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        self.ensure_graduated().await?;
        let url = format!("{}{path}", self.base_url);
        let resp = self.http.get(&url).send().await?;
        anyhow::ensure!(resp.status().is_success(), "GET {url}: {}", resp.status());
        Ok(resp.bytes().await?.to_vec())
    }
}

/// Graduate an SPKI pin for a cross-nest set's (or a followed folder's)
/// **home** nest before its byte plane dials it. The caller holds **no**
/// account on the home nest, so the ordinary bearer handshake cannot run there;
/// instead the pre-identity `fauna.auth.nest_handshake` proves the home nest's
/// identity against the record-delivered `home_nest_actor_id`
/// (`IdentityRoot::PreResolved`), and the shared trust core pins the bound SPKI
/// into the process-global cache that `store_pinned_reqwest_tls` reads
/// per-handshake — so a **self-signed** home's HTTPS byte plane is accepted
/// (`docs/goal/architecture/security.md` § Transport trust, the
/// federation-granted Axis-2 row).
///
/// Call it only for an `https://` home carrying a `home_nest_actor_id`: a
/// plain-`http://` home (the loopback test rig) has no TLS to pin, and an
/// absent actor id (a relay-unaware home) keeps `store_pinned_reqwest_tls`'s
/// `RequireWebPki` floor — never weaker (a WebPKI home works, a self-signed one
/// fails loud at TLS).
///
/// A failure is **fail-closed** by every caller: the ruling makes a refusal
/// after a root was delivered a hard-fail (a home serving cross-nest sets
/// answers the kind, so a rejection is the MITM's cheapest move), and a WebPKI
/// home's graduation short-circuits to success — so the only failures are a
/// genuine binding refusal or an unreachable home, both of which must leave the
/// download (or the folder, for the sync agent's bind) inert rather than dial
/// an unverified byte plane.
///
/// Shared by [`ForeignPublicChunkFetcher::for_home`] (every app's Media
/// download of a foreign set or a followed folder) and `fauna-sync-agent`'s
/// bind (`engine_driver`), which used to hold this body itself.
pub async fn graduate_home_nest_pin(
    home_nest_url: &str,
    home_nest_actor_id: &str,
) -> anyhow::Result<()> {
    let root: [u8; 32] = hex::decode(home_nest_actor_id.trim())
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .context("home_nest_actor_id is not a 32-byte hex nest_actor_id")?;
    let anon = fauna_anon_client::AnonymousNestClient::connect(home_nest_url)
        .await
        .context("connect to the home nest for byte-plane pin graduation")?;
    anon.graduate_first_contact(home_nest_url, Some(root))
        .await
        .context("graduate the home-nest SPKI pin against the grant-delivered identity")?;
    Ok(())
}

#[async_trait::async_trait]
impl BlobFetcher for ForeignPublicChunkFetcher {
    async fn fetch_manifest(&self, hash: &ContentHash) -> anyhow::Result<Vec<u8>> {
        let path = paths::chunk_store::manifest_by_hash(&hex::encode(hash.digest()));
        self.get(&path).await.with_context(|| format!("GET {path}"))
    }

    async fn fetch_chunks(
        &self,
        store_keys: &[ContentHash],
        relative_path: &str,
    ) -> anyhow::Result<Vec<Vec<u8>>> {
        // Sequential like `NestPublicChunkFetcher` — reqwest pools the
        // connection; parallelism is this method's future prerogative.
        let mut out = Vec::with_capacity(store_keys.len());
        for key in store_keys {
            let path = paths::chunk_store::chunk_by_hash(&hex::encode(key.digest()));
            let bytes = self
                .get(&path)
                .await
                .with_context(|| format!("GET {path} (chunk of {relative_path})"))?;
            out.push(bytes);
        }
        Ok(out)
    }
}

#[async_trait::async_trait]
impl BlobFetcher for NestPublicChunkFetcher {
    async fn fetch_manifest(&self, hash: &ContentHash) -> anyhow::Result<Vec<u8>> {
        let path = paths::chunk_store::manifest_by_hash(&hex::encode(hash.digest()));
        self.content
            .get(&path)
            .await
            .map(|b| b.to_vec())
            .with_context(|| format!("GET {path}"))
    }

    async fn fetch_chunks(
        &self,
        store_keys: &[ContentHash],
        relative_path: &str,
    ) -> anyhow::Result<Vec<Vec<u8>>> {
        // Sequential: reqwest already pools the one pinned connection, and the
        // seam hands over the whole batch precisely so a target can choose — if
        // this ever needs parallelism, a bounded `buffer_unordered` here is the
        // change and no shared code moves.
        let mut out = Vec::with_capacity(store_keys.len());
        for key in store_keys {
            let path = paths::chunk_store::chunk_by_hash(&hex::encode(key.digest()));
            let bytes = self
                .content
                .get(&path)
                .await
                .map(|b| b.to_vec())
                .with_context(|| format!("GET {path} (chunk of {relative_path})"))?;
            out.push(bytes);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `graduate_home_nest_pin` hex-decodes the record-delivered
    /// `home_nest_actor_id` BEFORE dialing, so a malformed / wrong-length id
    /// fails fast at the decode guard and never even contacts the (here
    /// unreachable) home nest. The graduation-against-a-real-self-signed-home
    /// half is proven in `fauna-nest`'s `tls_channel_binding_roundtrip`
    /// byte-plane test and, through an app, by the tui cross-nest followed
    /// download e2e.
    #[tokio::test]
    async fn graduate_home_nest_pin_rejects_a_malformed_actor_id_before_dialing() {
        // TEST-NET-1 (RFC 5737) — never routable, so a hang here would mean the
        // decode guard failed to short-circuit.
        let unreachable = "https://192.0.2.1:1";
        for bad in ["not-hex", "abcd", &"ab".repeat(31), &"ab".repeat(33)] {
            let err = graduate_home_nest_pin(unreachable, bad)
                .await
                .expect_err("a malformed/wrong-length actor id must fail at the decode guard");
            assert!(
                format!("{err:?}").contains("32-byte hex"),
                "error should name the hex-decode failure, got: {err:?}"
            );
        }
    }

    /// The same guard through the fetcher: a malformed root fails the first
    /// fetch closed, before anything is dialed.
    #[tokio::test]
    async fn a_fetcher_with_a_malformed_home_identity_fails_closed_before_dialing() {
        let fetcher = ForeignPublicChunkFetcher::for_home("https://192.0.2.1:1", Some("junk"));
        let err = fetcher
            .fetch_manifest(&ContentHash::of_raw(b"anything"))
            .await
            .expect_err("a malformed identity never reaches the wire");
        assert!(format!("{err:?}").contains("32-byte hex"), "got: {err:?}");
    }
}
