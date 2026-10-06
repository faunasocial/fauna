//! UniFFI façade for the `fauna.web.*` web-content authoring kinds
//! (`docs/goal/behavior/web-content-hosting.md`
//! § Admin apex hosting / § Published-post management):
//!
//! - the **per-user subdomain toggle** (`web-settings` page) — opt the calling
//!   actor's `web` content into serving at `https://<handle>.<domain>/` (User-
//!   class, caller-scoped, default OFF);
//! - the **admin apex-actor picker** (`admin-web` page) — designate which
//!   actor's `web` content serves `https://<domain>/` (Admin-class, nest-wide
//!   singleton), or clear to the built-in info page.
//!
//! [`FfiWebClient`] wraps the shared `fauna_client_web::WebClient<Arc<NestClient>>`
//! (the same thin client the Rust-native Linux app calls directly, and the
//! web SPA drives over the wasm twin `WsRpcClient::web*` methods). This seam
//! gives Apple / Windows / Android the identical surface over UniFFI — they
//! construct a client via [`build_web_client`] and render its calls + the pure
//! [`web_subdomain_view`] / [`web_apex_url`] projections. The reserved-label rule
//! and the `<handle>.<domain>` / apex URLs live in `fauna_core::web` (one source of
//! truth with the nest's routing), so no client re-derives them (priority #2).
//!
//! The `SubdomainView` / `ApexView` / `SubdomainDisabledReason` view types are
//! re-exported from `fauna_client_web` (built with its `uniffi` feature) so they
//! surface in the generated Swift / Kotlin / C# bindings, mirroring how
//! `mail_admin.rs` re-exports the mail-machine snapshot types.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_web::WebClient;
use fauna_core::localized::LocalizedText;

pub use fauna_client_web::{
    ApexView, MintedPaywallLink, PaywallTarget, SiteLinkDisabledReason, SiteLinkView,
    SubdomainDisabledReason, SubdomainView, WebDomainRow,
};

use crate::{FfiError, FfiNestClient};

/// `fauna.web.publish.list`, whole and FFI-shaped.
#[derive(uniffi::Record)]
pub struct FfiPublishedSite {
    /// The `web-published-posts-list` rows.
    pub posts: Vec<FfiPublishedPost>,
    /// The nest cleared the caller's rendered pages after a failed render and
    /// is restoring them by itself — what the `web-settings` status line says
    /// (`web-content-hosting.md` § Routing, render, serving → *A blanked site
    /// tells its author*). `false` when the site is not down.
    pub rendered_pages_down: bool,
}

/// One `web-published-posts-list` row, FFI-shaped. The wire
/// `fauna_protocol::web::PublishedPost` cannot cross UniFFI itself — it carries
/// a flattened forward-compat `extra` map of dag-cbor values — so this is the
/// same narrowing `FfiFeedPostItem` does for `FeedPostItem`.
#[derive(uniffi::Record)]
pub struct FfiPublishedPost {
    /// The 32-byte post id.
    pub post_id: Vec<u8>,
    /// The public slug the post serves under.
    pub slug: String,
    /// The tier gating this post, or `None` for an ungated row (the wire
    /// field is absent for an ungated row). Only a `Some` row offers
    /// the *Copy paywall link* affordance.
    pub gated_tier: Option<String>,
}

/// UniFFI handle for the `fauna.web.*` kinds. Construct via [`build_web_client`];
/// methods are exposed to Swift as `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiWebClient {
    nest: Arc<NestClient>,
}

impl FfiWebClient {
    fn client(&self) -> WebClient<Arc<NestClient>> {
        WebClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiWebClient {
    /// `fauna.web.get_subdomain_enabled` — the calling actor's per-user subdomain
    /// opt-in (`false` ⇒ not hosting). Caller-scoped, pure read.
    pub async fn get_subdomain_enabled(&self) -> Result<bool, FfiError> {
        Ok(self
            .client()
            .get_subdomain_enabled()
            .await
            .map_err(|e| e.to_string())?)
    }

    /// `fauna.web.set_subdomain_enabled` — flip the calling actor's own subdomain
    /// hosting; returns the nest-confirmed state (the UI renders off this echo,
    /// non-optimistically).
    pub async fn set_subdomain_enabled(&self, enabled: bool) -> Result<bool, FfiError> {
        Ok(self
            .client()
            .set_subdomain_enabled(enabled)
            .await
            .map_err(|e| e.to_string())?)
    }

    /// `fauna.web.get_apex_actor` — the nest-wide apex designation (`None` ⇒ the
    /// built-in info page) as the 32-byte actor id. Admin-class, pure read.
    pub async fn get_apex_actor(&self) -> Result<Option<Vec<u8>>, FfiError> {
        Ok(self
            .client()
            .get_apex_actor()
            .await
            .map_err(|e| e.to_string())?)
    }

    /// `fauna.web.set_apex_actor` — designate (`Some`) or clear (`None`) the
    /// deployment apex actor; returns the nest-confirmed designation. Admin-class.
    pub async fn set_apex_actor(
        &self,
        actor_id: Option<Vec<u8>>,
    ) -> Result<Option<Vec<u8>>, FfiError> {
        Ok(self
            .client()
            .set_apex_actor(actor_id)
            .await
            .map_err(|e| e.to_string())?)
    }

    // ── Published-post management ───────────────────────────────────────

    /// `fauna.web.publish.set` — publish one of the caller's own posts.
    /// `slug` `None` takes the nest's post-id-hex default; the returned
    /// **effective** slug is what the copy-link affordance must use.
    pub async fn publish_set(
        &self,
        post_id: Vec<u8>,
        slug: Option<String>,
    ) -> Result<String, FfiError> {
        Ok(self
            .client()
            .publish_set(post_id, slug)
            .await
            .map_err(|e| e.to_string())?)
    }

    /// `fauna.web.publish.unset` — take a published post down. Idempotent and
    /// reversible, hence a one-tap verb with no destructive-confirm step.
    pub async fn publish_unset(&self, post_id: Vec<u8>) -> Result<bool, FfiError> {
        Ok(self
            .client()
            .publish_unset(post_id)
            .await
            .map_err(|e| e.to_string())?)
    }

    /// `fauna.nest.info` — **the domain this nest actually routes web content
    /// on**, and the only legitimate `domain` input to [`web_subdomain_view`] /
    /// [`web_site_link_view`]. An EMPTY result is a real answer ("this nest
    /// serves no web content") and must be passed through unchanged; do not
    /// substitute a cached or dialed host for it.
    pub async fn serving_domain(&self) -> Result<String, FfiError> {
        Ok(self
            .client()
            .serving_domain()
            .await
            .map_err(|e| e.to_string())?)
    }

    /// `fauna.web.domain.get` — the caller's registered custom domains, already
    /// narrowed to what [`web_site_link_view`] takes. Pass the result straight
    /// through: it is the custom-domain half of the `origin` inputs a client leg
    /// owes that resolver, and only an `active` row wins.
    pub async fn domain_get(&self) -> Result<Vec<WebDomainRow>, FfiError> {
        Ok(self
            .client()
            .domain_get()
            .await
            .map_err(|e| e.to_string())?)
    }

    /// `fauna.web.publish.list` — the caller's published posts, each with the
    /// tier gating it (if any). The `web-published-posts-list` rows.
    ///
    /// Superseded by [`Self::published_site`], which carries the same rows plus
    /// the rendered-pages-down status; kept until the last app leg has moved.
    pub async fn publish_list(&self) -> Result<Vec<FfiPublishedPost>, FfiError> {
        Ok(self.published_site().await?.posts)
    }

    /// `fauna.web.publish.list`, whole — the rows and whether the caller's
    /// rendered pages are down. One read; never call this and
    /// [`Self::publish_list`] for the same paint.
    pub async fn published_site(&self) -> Result<FfiPublishedSite, FfiError> {
        let site = self
            .client()
            .publish_list()
            .await
            .map_err(|e| e.to_string())?;
        Ok(FfiPublishedSite {
            posts: site
                .posts
                .into_iter()
                .map(|p| FfiPublishedPost {
                    post_id: p.post_id.into_vec(),
                    slug: p.slug,
                    gated_tier: p.gated_tier,
                })
                .collect(),
            rendered_pages_down: site.rendered_pages_down,
        })
    }

    /// `fauna.web.paywall.mint_token` — mint a short-lived full-access link for
    /// one of the caller's own paywalled resources. Feed the returned `path`
    /// straight to [`web_tokened_url`]; never rebuild it client-side.
    pub async fn paywall_mint_token(
        &self,
        target: PaywallTarget,
    ) -> Result<MintedPaywallLink, FfiError> {
        Ok(self
            .client()
            .paywall_mint_token(target)
            .await
            .map_err(|e| e.to_string())?)
    }
}

/// Build an [`FfiWebClient`] over `nest`'s WS-RPC connection — the constructor
/// the per-app (windows/macos/ios/android) web-settings + admin-web pages
/// call, mirroring `mail_admin::build_mail_settings_machine`.
#[uniffi::export]
pub fn build_web_client(nest: Arc<FfiNestClient>) -> Arc<FfiWebClient> {
    Arc::new(FfiWebClient {
        nest: nest.nest_arc(),
    })
}

/// `fauna_client_web::subdomain_view` → the `web-settings` toggle render state
/// (`enabled` + the live `<handle>.<domain>` URL or a `disabled_reason`). Pure;
/// the same projection every app renders. The native twin of the wasm
/// `webSubdomainView`.
///
/// ⚠ `domain` **must** be [`FfiWebClient::serving_domain`]'s answer. The cached
/// sign-in `domain` every native app used to pass is *not* it: on a domainless
/// box that value is the `"localhost"` placeholder, and `<handle>.localhost` is
/// exactly the host the nest's resolver never strips.
#[uniffi::export]
pub fn web_subdomain_view(enabled: bool, handle: Option<String>, domain: String) -> SubdomainView {
    fauna_client_web::subdomain_view(enabled, handle.as_deref(), &domain)
}

/// `fauna_core::web::apex_url` → the `https://<domain>/` the apex serves at, for
/// the `admin-web` apex-picker info line. The native twin of the wasm `webApexUrl`.
#[uniffi::export]
pub fn web_apex_url(domain: String) -> String {
    fauna_core::web::apex_url(&domain)
}

/// `fauna_client_web::apex_view` → the `admin-web` apex-picker render state
/// (the current designation + the `https://<domain>/` URL). Pure; the same
/// projection every app renders.
#[uniffi::export]
pub fn web_apex_view(current_actor: Option<Vec<u8>>, domain: String) -> ApexView {
    fauna_client_web::apex_view(current_actor, &domain)
}

/// `fauna_client_web::site_link_view` → where the actor's published content is
/// reachable (active custom domain > enabled subdomain), or the reason it
/// isn't. Pure; the native twin of the wasm `webSiteLinkView`. Pass the
/// `fauna.web.domain.get` rows through as `domains`.
///
/// ⚠ `domain` **must** be [`FfiWebClient::serving_domain`]'s answer — see
/// [`web_subdomain_view`].
#[uniffi::export]
pub fn web_site_link_view(
    domains: Vec<WebDomainRow>,
    subdomain_enabled: bool,
    handle: Option<String>,
    domain: String,
) -> SiteLinkView {
    fauna_client_web::site_link_view(&domains, subdomain_enabled, handle.as_deref(), &domain)
}

/// `fauna_client_web::post_page_url` → the tokenless public page URL for a
/// published post: the *Copy web link* value. The native twin of the wasm
/// `webPostPageUrl`.
#[uniffi::export]
pub fn web_post_page_url(origin: String, slug: String) -> String {
    fauna_client_web::post_page_url(&origin, &slug)
}

/// `fauna_client_web::tokened_url` → the full-access URL for a minted paywall
/// link: the *Copy paywall link* value. `path` is the mint reply's own `path`.
/// The native twin of the wasm `webTokenedUrl`.
#[uniffi::export]
pub fn web_tokened_url(origin: String, path: String, token: String) -> String {
    fauna_client_web::tokened_url(&origin, &path, &token)
}

/// `fauna_client_web::disabled_reason_text` → the "your posts have no public
/// address, and here's why" `LocalizedText` `{ key, args }` for a
/// [`SiteLinkView`] with no origin — the client resolves it through its i18n
/// pipeline. The native twin of the wasm `webDisabledReasonText`.
#[uniffi::export]
pub fn web_disabled_reason_text(reason: Option<SiteLinkDisabledReason>) -> LocalizedText {
    fauna_client_web::disabled_reason_text(reason)
}
