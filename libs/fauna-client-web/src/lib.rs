//! Client-side surface for web-content hosting authoring (Slice 4 of
//! `docs/goal/behavior/web-content-hosting.md`).
//!
//! Two authoring controls, shared by all 7 apps (priorities #1/#2):
//! - the **admin apex-actor picker** (`admin-web` page) — designate which actor's
//!   `web` content serves `https://<domain>/` (Admin-class, nest-wide singleton);
//! - the **per-user subdomain toggle** (`web-settings` page) — opt the calling
//!   actor's `web` content into serving at `https://<handle>.<domain>/` (User-
//!   class, caller-scoped, default OFF).
//!
//! Both drive the typed `fauna.web.*` kinds through a generic [`WebClient`] over
//! any [`RpcRequester`] transport — native (linux/`fauna-ffi`) and the browser
//! `WsRpcClient` (web) alike, exactly like `fauna-client-config` /
//! `fauna-client-bridges`. The pure addressing logic (reserved labels, the
//! `<handle>.<domain>` / apex URLs) lives in [`fauna_core::web`] so the UI hint
//! never drifts from the nest's routing. A folder's **website toggle**
//! (`folder-website-toggle`, `fauna.folders.update`) is the third control, not
//! this crate.

// UniFFI scaffolding for the native FFI consumers (Apple/Android/Windows via
// `fauna-ffi`): the `uniffi::Record`/`Enum` derives on the view types below need
// the per-crate `UniFfiTag` this macro emits. Gated behind the `uniffi` feature
// so linux (native Rust) + web (wasm) build the plain types without it.
#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_client_web");

use fauna_protocol::RpcRequester;
use fauna_protocol::web::{
    PublishedPost, WebDomainGetReply, WebDomainGetRequest, WebGetApexActorReply,
    WebGetApexActorRequest, WebGetSubdomainEnabledReply, WebGetSubdomainEnabledRequest,
    WebPaywallMintTokenReply, WebPaywallMintTokenRequest, WebPublishListReply,
    WebPublishListRequest, WebPublishSetReply, WebPublishSetRequest, WebPublishUnsetReply,
    WebPublishUnsetRequest, WebSetApexActorReply, WebSetApexActorRequest,
    WebSetSubdomainEnabledReply, WebSetSubdomainEnabledRequest,
};
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

/// Typed client over the `fauna.web.*` authoring kinds. Generic over the
/// transport ([`RpcRequester`]), so the same code serves native + wasm.
pub struct WebClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> WebClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// The domain this nest routes user web content on — the **only** legitimate
    /// input to [`subdomain_view`] / [`site_link_view`], read straight from the
    /// nest rather than guessed from the session (see [`serving_domain`] for the
    /// three wrong values this replaces).
    ///
    /// Lives here rather than in each app for the same reason [`Self::domain_get`]
    /// does: seven legs sharing one call site for a read whose only consumer is
    /// those resolvers (priority #2). `fauna.nest.info` is **pre-identity**
    /// (`pre_identity_allowlist.rs`), so this is callable on any session, before
    /// or after sign-in.
    ///
    /// A nest that answers with an empty string is answering "I serve no web
    /// content", and that answer is honoured, not overridden.
    pub async fn serving_domain(&self) -> Result<String, R::Error> {
        let reply: fauna_protocol::discovery::NestInfoReply = self
            .nest
            .request(
                "fauna.nest.info",
                fauna_protocol::discovery::NestInfoRequest::default(),
            )
            .await?;
        Ok(serving_domain(reply.web_serving_domain.as_deref()))
    }

    /// Read the calling actor's subdomain opt-in (`false` ⇒ not hosting).
    /// Caller-scoped — the nest keys on the authenticated actor.
    pub async fn get_subdomain_enabled(&self) -> Result<bool, R::Error> {
        let reply: WebGetSubdomainEnabledReply = self
            .nest
            .request(
                "fauna.web.get_subdomain_enabled",
                WebGetSubdomainEnabledRequest::default(),
            )
            .await?;
        Ok(reply.enabled)
    }

    /// Flip the calling actor's own subdomain hosting on/off. Returns the
    /// resulting state echoed by the nest.
    pub async fn set_subdomain_enabled(&self, enabled: bool) -> Result<bool, R::Error> {
        let reply: WebSetSubdomainEnabledReply = self
            .nest
            .request(
                "fauna.web.set_subdomain_enabled",
                WebSetSubdomainEnabledRequest {
                    enabled,
                    ..Default::default()
                },
            )
            .await?;
        Ok(reply.enabled)
    }

    /// Read the nest-wide apex-actor designation (`None` ⇒ apex serves the
    /// built-in info page). Admin-class.
    pub async fn get_apex_actor(&self) -> Result<Option<Vec<u8>>, R::Error> {
        let reply: WebGetApexActorReply = self
            .nest
            .request(
                "fauna.web.get_apex_actor",
                WebGetApexActorRequest::default(),
            )
            .await?;
        Ok(reply.actor_id.map(ByteBuf::into_vec))
    }

    /// Designate (`Some`) or clear (`None`) the apex actor. Returns the resulting
    /// designation echoed by the nest. Admin-class.
    pub async fn set_apex_actor(
        &self,
        actor_id: Option<Vec<u8>>,
    ) -> Result<Option<Vec<u8>>, R::Error> {
        let reply: WebSetApexActorReply = self
            .nest
            .request(
                "fauna.web.set_apex_actor",
                WebSetApexActorRequest {
                    actor_id: actor_id.map(ByteBuf::from),
                    ..Default::default()
                },
            )
            .await?;
        Ok(reply.actor_id.map(ByteBuf::into_vec))
    }

    // ── Published-post management (`web-content-hosting.md`
    //    § Published-post management) ─────────────────────────────────────
    //
    // The four calls behind the ⋯-overflow verbs and the `web-settings`
    // published-posts section. All caller-scoped: the nest keys every one on
    // the authenticated actor, so a client can only publish, take down, list
    // and mint for its own posts.

    /// Publish one of the caller's own posts as a web page. `slug` `None`
    /// (or empty) takes the nest's default — the post-id hex. Returns the
    /// **effective** slug the nest published under, which is what the copy-link
    /// affordances must build their URL from (never the requested slug).
    pub async fn publish_set(
        &self,
        post_id: Vec<u8>,
        slug: Option<String>,
    ) -> Result<String, R::Error> {
        let reply: WebPublishSetReply = self
            .nest
            .request(
                "fauna.web.publish.set",
                WebPublishSetRequest {
                    post_id: ByteBuf::from(post_id),
                    slug,
                    ..Default::default()
                },
            )
            .await?;
        Ok(reply.slug)
    }

    /// Take a published post down. Idempotent and reversible — which is why the
    /// UI offers it as a one-tap verb with no destructive-confirm step
    /// (`web-content-hosting.md` § Published-post management).
    pub async fn publish_unset(&self, post_id: Vec<u8>) -> Result<bool, R::Error> {
        let reply: WebPublishUnsetReply = self
            .nest
            .request(
                "fauna.web.publish.unset",
                WebPublishUnsetRequest {
                    post_id: ByteBuf::from(post_id),
                    ..Default::default()
                },
            )
            .await?;
        Ok(reply.ok)
    }

    /// The caller's registered custom domains, narrowed to what origin
    /// resolution needs (`domain` + `status`) and handed straight to
    /// [`site_link_view`] — the **first half of the `origin` inputs** every app
    /// leg owes that surface (the other two, the subdomain flag and the handle,
    /// it already has). It lives here rather than in each app so the seven legs
    /// share one call site for a read whose only consumer is that resolver
    /// (priority #2); the registration/verification half of the domain
    /// lifecycle has no app UI at all (`web-content-hosting.md:4` — the
    /// client-side custom-domain authoring UI is deferred), so `domain.get` is
    /// the one domain kind an app drives.
    ///
    /// Caller-scoped: the nest keys on the authenticated actor
    /// (`bins/fauna-nest/src/web_handlers.rs::domain_get_handler` →
    /// `get_web_domains_for_actor`). Rows arrive in whatever order the nest
    /// stored them; `site_link_view` picks the first `active` one, so callers
    /// must not re-sort in the hope of changing which domain wins.
    pub async fn domain_get(&self) -> Result<Vec<WebDomainRow>, R::Error> {
        let reply: WebDomainGetReply = self
            .nest
            .request("fauna.web.domain.get", WebDomainGetRequest::default())
            .await?;
        Ok(reply
            .domains
            .into_iter()
            .map(|d| WebDomainRow {
                domain: d.domain,
                status: d.status,
            })
            .collect())
    }

    /// The caller's published posts — the `web-published-posts-list` rows. Each
    /// carries its slug and, additively, the tier gating it (`None` = ungated,
    /// the nest omits the key: the paywall-link
    /// affordance then simply doesn't render) — plus whether the caller's
    /// rendered pages are down ([`PublishedSite::rendered_pages_down`]).
    pub async fn publish_list(&self) -> Result<PublishedSite, R::Error> {
        let reply: WebPublishListReply = self
            .nest
            .request("fauna.web.publish.list", WebPublishListRequest::default())
            .await?;
        Ok(PublishedSite {
            posts: reply.posts,
            rendered_pages_down: reply.rendered_pages_down,
        })
    }

    /// Mint a short-lived full-access token for one of the caller's own
    /// paywalled resources — a published+gated post's `slug`, or a paywalled
    /// `web` folder `path`. Returns the token, its unix-seconds expiry, and
    /// the rendered path it is scoped to; feed that path to
    /// [`tokened_url`] rather than rebuilding it.
    ///
    /// The link is a **preview/instant-share** link, freely re-mintable — never
    /// the durable comp mechanism, which is the Pillar-3 claim code
    /// (`monetization.md` § Pillar 2 → *Creator comp-link surface*). No TTL
    /// knob exists by ratified design.
    pub async fn paywall_mint_token(
        &self,
        target: PaywallTarget,
    ) -> Result<MintedPaywallLink, R::Error> {
        let (slug, path) = match target {
            PaywallTarget::PostSlug { slug } => (slug, None),
            PaywallTarget::FilePath { path } => (String::new(), Some(path)),
        };
        let reply: WebPaywallMintTokenReply = self
            .nest
            .request(
                "fauna.web.paywall.mint_token",
                WebPaywallMintTokenRequest {
                    slug,
                    path,
                    ..Default::default()
                },
            )
            .await?;
        Ok(MintedPaywallLink {
            token: reply.token,
            expires: reply.expires,
            path: reply.path,
        })
    }
}

// ── View types (shared, rendered by every app) ───────────────────────────

/// Why a user can't opt their content into subdomain hosting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SubdomainDisabledReason {
    /// The actor has no handle yet — a handle keys the `<handle>.<domain>` host.
    NoHandle,
    /// The actor's handle is a reserved label (`mail`/`mta-sts`/`app`/`www`),
    /// which the nest never serves as user content.
    ReservedLabel,
    /// **The nest itself serves no web content at any host** — it has no serving
    /// domain (a domainless localhost/IP box). Nothing the user does on this page
    /// changes it, which is why it outranks [`Self::NoHandle`]: telling someone to
    /// pick a handle when no handle could ever serve sends them to fix the wrong
    /// thing.
    NoServingDomain,
}

/// The `web-settings` subdomain-toggle render state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SubdomainView {
    /// The current opt-in state (the toggle position).
    pub enabled: bool,
    /// The live `https://<handle>.<domain>/` URL the site serves at, or `None`
    /// when it can't (see `disabled_reason`).
    pub url: Option<String>,
    /// Set when the toggle can't meaningfully serve content yet.
    pub disabled_reason: Option<SubdomainDisabledReason>,
}

/// Build the subdomain-toggle view from the opt-in flag + the actor's handle +
/// the nest's serving domain. Pure — the same projection on every app.
///
/// `domain` **must** be [`serving_domain`]'s answer — the host the nest's own
/// resolver routes on — never the address the client dialed and never the
/// sign-in reply's `domain`. An empty `domain` means the nest serves no web
/// content at all and yields [`SubdomainDisabledReason::NoServingDomain`], so
/// the row explains itself instead of rendering blank.
pub fn subdomain_view(enabled: bool, handle: Option<&str>, domain: &str) -> SubdomainView {
    // Nest-level first: a box that serves nothing makes every per-user reason
    // moot, and a user cannot act on it from this page.
    let disabled_reason = if domain.is_empty() {
        Some(SubdomainDisabledReason::NoServingDomain)
    } else {
        match handle {
            None => Some(SubdomainDisabledReason::NoHandle),
            Some("") => Some(SubdomainDisabledReason::NoHandle),
            Some(h) if fauna_core::web::is_reserved_subdomain_label(h) => {
                Some(SubdomainDisabledReason::ReservedLabel)
            }
            Some(_) => None,
        }
    };
    let url = handle.and_then(|h| fauna_core::web::subdomain_url(h, domain));
    SubdomainView {
        enabled,
        url,
        disabled_reason,
    }
}

/// The domain a client composes `<handle>.<domain>` site URLs on.
///
/// **This function exists because three plausible-looking values are all wrong**,
/// and every app had picked one of them (`web-content-hosting.md`
/// § Published-post management → *Implementation status today*):
///
/// | value | why it produces a dead link |
/// |---|---|
/// | the address the client **dialed** | wrong the moment a nest is reached by IP or by a second DNS name |
/// | the sign-in reply's `domain` | same chain as the nest's, but with a **`"localhost"` placeholder** on a domainless box — and `.localhost` is exactly the suffix the resolver never strips |
/// | `nest.info`'s `registration.handle_domain` | the **handle** domain, not the web apex — `"unknown"` on a domainless box where the apex is the empty string (it was also the stale *boot seed* until 2026-09-02) |
///
/// The one right answer is the nest's own
/// [`NestInfoReply::web_serving_domain`](fauna_protocol::discovery::NestInfoReply::web_serving_domain),
/// which is read from the same accessor its `HostResolver` keys off.
///
/// The answer is used **verbatim, including the empty string**, which is the
/// nest's way of saying "I serve no web content" — substituting any other host
/// there is precisely the dead-link bug. An absent field reads the same as the
/// empty answer: every nest sends it, and the dialed-host fallback an older
/// nest once licensed is gone with the compat-remnant sweep
/// (`version-compatibility.md` § Dimension 2, the fourth ratified exception).
pub fn serving_domain(nest_reported: Option<&str>) -> String {
    nest_reported.unwrap_or_default().to_string()
}

/// The `admin-web` apex-picker render state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ApexView {
    /// The currently-designated apex actor id, or `None` (info page).
    pub current_actor: Option<Vec<u8>>,
    /// The `https://<domain>/` URL the apex serves at.
    pub apex_url: String,
}

/// Build the apex-picker view from the current designation + the nest's domain.
pub fn apex_view(current_actor: Option<Vec<u8>>, domain: &str) -> ApexView {
    ApexView {
        current_actor,
        apex_url: fauna_core::web::apex_url(domain),
    }
}

// ── Published-post management: the link surface ─────────────────────────────

/// Which paywalled resource a mint is for. Exactly one target reaches the wire
/// — the nest refuses a request carrying both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum PaywallTarget {
    /// A published, gated post — its `fauna.web.publish.set` slug.
    PostSlug { slug: String },
    /// A file inside a paywalled `web` folder, e.g. `downloads/report.pdf`.
    FilePath { path: String },
}

/// One `fauna.web.domain.get` row, narrowed to what origin resolution needs.
/// Named rather than a `(String, String)` tuple so no caller has to remember
/// which half is the status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct WebDomainRow {
    pub domain: String,
    /// `pending` / `verified` / `active` — only `active` has a live cert.
    pub status: String,
}

/// A freshly minted paywall link's parts. `path` is the nest's own scoping
/// answer — always build the URL from it via [`tokened_url`], never by
/// re-deriving `post/{slug}.html` client-side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MintedPaywallLink {
    /// The `?token=` value. URL-safe base64 (no padding) — safe to concatenate
    /// into a query string without escaping.
    pub token: String,
    /// Unix seconds. Short-lived by ratified design (~10 min) and freely
    /// re-mintable; the UI states the validity rather than offering a knob.
    pub expires: u64,
    /// The rendered path the token is scoped to, e.g. `post/my-slug.html`.
    pub path: String,
}

/// Why the actor's content has no public origin, so every copy affordance must
/// disable with a reason rather than hand out a dead link
/// (`web-content-hosting.md` § Published-post management: "publishing with no
/// serving origin is legal but unreachable — the UI must say so").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SiteLinkDisabledReason {
    /// No active custom domain, and subdomain hosting is switched off. The one
    /// reason the user can fix from this very page.
    SubdomainDisabled,
    /// Subdomain hosting is on, but the actor has no handle to key the host on.
    NoHandle,
    /// The actor's handle is a reserved label the nest never serves as user
    /// content.
    ReservedLabel,
    /// **The nest serves no web content at any host** — no serving domain at all.
    /// Outranks every per-user reason (see
    /// [`SubdomainDisabledReason::NoServingDomain`]); a custom domain is the one
    /// thing that still wins over it, because an `active` custom domain has its
    /// own cert and its own resolver entry.
    NoServingDomain,
}

/// Where the actor's published content is reachable, and why not when it isn't.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SiteLinkView {
    /// The serving origin with a trailing slash (`https://alice.example.com/`),
    /// or `None` — see `disabled_reason`.
    pub origin: Option<String>,
    /// Set exactly when `origin` is `None`.
    pub disabled_reason: Option<SiteLinkDisabledReason>,
}

/// Resolve the origin a creator's copy-link affordances should build on.
///
/// **Precedence: an active custom domain beats an enabled subdomain**
/// (`web-content-hosting.md` § Published-post management) — a creator who went
/// to the trouble of a custom domain wants that to be the link they share.
/// `custom_domains` takes the `fauna.web.domain.get` rows; only `active` counts
/// — a `pending`/`verified` domain has no cert yet, so linking to it would hand
/// out a URL that fails to load.
///
/// Apex-self detection is a deliberate v1 bound: an apex-designated actor still
/// resolves through the subdomain/custom-domain paths, because
/// `fauna.web.get_apex_actor` is Admin-class and a plain user cannot read it.
///
/// Pure — the same projection on every app.
pub fn site_link_view(
    custom_domains: &[WebDomainRow],
    subdomain_enabled: bool,
    handle: Option<&str>,
    domain: &str,
) -> SiteLinkView {
    if let Some(active) = custom_domains.iter().find(|row| row.status == "active") {
        return SiteLinkView {
            origin: Some(format!("https://{}/", active.domain)),
            disabled_reason: None,
        };
    }
    if !subdomain_enabled {
        return SiteLinkView {
            origin: None,
            disabled_reason: Some(SiteLinkDisabledReason::SubdomainDisabled),
        };
    }
    // Subdomain hosting is on — reuse the toggle's own eligibility rules so the
    // two surfaces can never disagree about whether a handle can serve.
    let sub = subdomain_view(true, handle, domain);
    match (sub.url, sub.disabled_reason) {
        (Some(url), _) => SiteLinkView {
            origin: Some(url),
            disabled_reason: None,
        },
        (None, Some(SubdomainDisabledReason::NoServingDomain)) => SiteLinkView {
            origin: None,
            disabled_reason: Some(SiteLinkDisabledReason::NoServingDomain),
        },
        (None, Some(SubdomainDisabledReason::ReservedLabel)) => SiteLinkView {
            origin: None,
            disabled_reason: Some(SiteLinkDisabledReason::ReservedLabel),
        },
        (None, _) => SiteLinkView {
            origin: None,
            disabled_reason: Some(SiteLinkDisabledReason::NoHandle),
        },
    }
}

/// The human-readable "your posts have no public address, and here's why" line
/// for a [`SiteLinkView`] with no origin. Shared decision (tui↔linux twin
/// harvest, previously hand-rolled identically on
/// both apps); `NoServingDomain` deliberately reuses the subdomain toggle's own
/// no-serving-domain string rather than minting a link-specific one.
pub fn disabled_reason_text(
    reason: Option<SiteLinkDisabledReason>,
) -> fauna_core::localized::LocalizedText {
    use fauna_core::localized::LocalizedText;
    match reason {
        Some(SiteLinkDisabledReason::SubdomainDisabled) | None => {
            LocalizedText::key("web_settings.link_disabled_subdomain_off")
        }
        Some(SiteLinkDisabledReason::NoHandle) => {
            LocalizedText::key("web_settings.link_disabled_no_handle")
        }
        Some(SiteLinkDisabledReason::ReservedLabel) => {
            LocalizedText::key("web_settings.link_disabled_reserved")
        }
        Some(SiteLinkDisabledReason::NoServingDomain) => {
            LocalizedText::key("web_settings.subdomain_no_serving_domain")
        }
    }
}

/// What `fauna.web.publish.list` says about the caller's rendered site.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PublishedSite {
    /// The management rows.
    pub posts: Vec<PublishedPost>,
    /// The nest cleared the caller's rendered pages after a failed render and
    /// has not restored them yet. It restores them by itself, so this is
    /// something to *tell* the author, never something to ask of them
    /// (`web-content-hosting.md` § Routing, render, serving → *A blanked site
    /// tells its author*). `false` when the key is omitted (pages up).
    pub rendered_pages_down: bool,
}

/// Everything one visit to the `web-settings` page reads, in one value — the
/// subdomain toggle's projected view, the nest's own serving domain, the
/// caller's custom-domain rows, and the caller's published posts. Bundled so
/// every reader shares one fetch-and-project sequence rather than each
/// growing its own (`web-content-hosting.md` § Published-post management).
#[derive(Debug, Clone, PartialEq)]
pub struct WebPageRead {
    /// The subdomain toggle's projected view.
    pub view: SubdomainView,
    /// The host the nest itself routes web content on (`fauna.nest.info`),
    /// kept so a paint-time [`site_link_view`] resolves on the same value
    /// `view` was projected from — and so flipping the toggle costs no
    /// second read. Empty ⇒ this nest serves no web content at all.
    pub serving_domain: String,
    /// `fauna.web.domain.get`, narrowed by the shared wrapper.
    pub domains: Vec<WebDomainRow>,
    /// `fauna.web.publish.list` — the management rows.
    pub posts: Vec<PublishedPost>,
    /// `fauna.web.publish.list` — [`PublishedSite::rendered_pages_down`].
    pub rendered_pages_down: bool,
}

/// The `web-settings` page's whole read, in the order the page needs it.
/// Sequential and fail-fast on purpose: a caller that got the opt-in but not
/// the domain list cannot resolve an origin, and guessing one is how a user is
/// handed a subdomain link while their active custom domain is the real
/// address.
///
/// Each failure names its own call, so an `error-message` element can say
/// which read broke rather than "load web hosting" for all four.
///
/// **The serving domain is READ, not derived** — see [`serving_domain`] for
/// the three wrong values this replaces.
///
/// Shared by every app leg so the `web-settings` page and the feed ⋯-menu's
/// lazy origin hydrate (`ui/feed.md` § User actions) read one answer instead
/// of each growing their own sequence — first lifted out of `fauna-tui` (the
/// lead app) when linux's ⋯-menu needed the exact same read.
pub async fn read_web_page<R: RpcRequester>(
    client: &WebClient<R>,
    handle: &str,
) -> Result<WebPageRead, String> {
    let serving_domain = client
        .serving_domain()
        .await
        .map_err(|e| format!("load web serving domain: {e}"))?;
    let enabled = client
        .get_subdomain_enabled()
        .await
        .map_err(|e| format!("load web hosting: {e}"))?;
    let domains = client
        .domain_get()
        .await
        .map_err(|e| format!("load web domains: {e}"))?;
    let PublishedSite {
        posts,
        rendered_pages_down,
    } = client
        .publish_list()
        .await
        .map_err(|e| format!("load published posts: {e}"))?;
    Ok(WebPageRead {
        view: subdomain_view(
            enabled,
            Some(handle).filter(|h| !h.is_empty()),
            &serving_domain,
        ),
        serving_domain,
        domains,
        posts,
        rendered_pages_down,
    })
}

/// The public page URL for a published post — the *Copy web link* value. This
/// is the link a creator shares to sell: a visitor with no token gets the
/// teaser (`web-content-hosting.md` § Paywalled serving, "no token → teaser").
pub fn post_page_url(origin: &str, slug: &str) -> String {
    format!("{}post/{}.html", ensure_trailing_slash(origin), slug)
}

/// The full-access URL for a minted paywall link — the *Copy paywall link*
/// value. `path` is [`MintedPaywallLink::path`] verbatim; the token is URL-safe
/// base64, so it needs no escaping.
pub fn tokened_url(origin: &str, path: &str, token: &str) -> String {
    format!(
        "{}{}?token={}",
        ensure_trailing_slash(origin),
        path.trim_start_matches('/'),
        token
    )
}

fn ensure_trailing_slash(origin: &str) -> String {
    if origin.ends_with('/') {
        origin.to_string()
    } else {
        format!("{origin}/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{FailedRequest, FailingRequester};
    use std::cell::{Cell, RefCell};

    #[test]
    fn subdomain_view_enabled_with_handle() {
        let v = subdomain_view(true, Some("alice"), "example.com");
        assert!(v.enabled);
        assert_eq!(v.url.as_deref(), Some("https://alice.example.com/"));
        assert_eq!(v.disabled_reason, None);
    }

    #[test]
    fn subdomain_view_no_handle() {
        let v = subdomain_view(false, None, "example.com");
        assert_eq!(v.url, None);
        assert_eq!(v.disabled_reason, Some(SubdomainDisabledReason::NoHandle));
        let empty = subdomain_view(false, Some(""), "example.com");
        assert_eq!(
            empty.disabled_reason,
            Some(SubdomainDisabledReason::NoHandle)
        );
    }

    #[test]
    fn subdomain_view_reserved_label() {
        let v = subdomain_view(true, Some("www"), "example.com");
        assert_eq!(v.url, None);
        assert_eq!(
            v.disabled_reason,
            Some(SubdomainDisabledReason::ReservedLabel)
        );
    }

    #[test]
    fn apex_view_shape() {
        let v = apex_view(Some(vec![1u8; 32]), "example.com");
        assert_eq!(v.current_actor, Some(vec![1u8; 32]));
        assert_eq!(v.apex_url, "https://example.com/");
        assert_eq!(apex_view(None, "example.com").current_actor, None);
    }

    // A minimal in-memory transport so the kind strings + request/reply shapes
    // are proven without a real nest (the tier_3 e2e proves them end-to-end too).
    #[derive(Default)]
    struct FakeNest {
        subdomain: Cell<bool>,
        apex: RefCell<Option<Vec<u8>>>,
        kinds: RefCell<Vec<String>>,
        published: RefCell<Vec<String>>,
        minted_for: RefCell<Vec<String>>,
        /// `Some` ⇒ `publish.list` carries `rendered_pages_down`; `None` omits
        /// the key (pages up).
        pages_down: Cell<Option<bool>>,
    }

    impl RpcRequester for FakeNest {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.kinds.borrow_mut().push(kind.to_string());
            let payload = serde_json::to_value(payload).unwrap();
            let reply = match kind {
                "fauna.web.get_subdomain_enabled" => {
                    serde_json::json!({ "enabled": self.subdomain.get() })
                }
                "fauna.web.set_subdomain_enabled" => {
                    let enabled = payload["enabled"].as_bool().unwrap();
                    self.subdomain.set(enabled);
                    serde_json::json!({ "enabled": enabled })
                }
                "fauna.web.get_apex_actor" => {
                    serde_json::json!({ "actor_id": *self.apex.borrow() })
                }
                "fauna.web.set_apex_actor" => {
                    let actor: Option<Vec<u8>> =
                        serde_json::from_value(payload["actor_id"].clone()).unwrap();
                    *self.apex.borrow_mut() = actor.clone();
                    serde_json::json!({ "actor_id": actor })
                }
                "fauna.web.publish.set" => {
                    // The nest answers with the EFFECTIVE slug: empty/absent
                    // request slug falls back to the post-id hex.
                    let requested = payload["slug"].as_str().unwrap_or("");
                    let post_id: Vec<u8> =
                        serde_json::from_value(payload["post_id"].clone()).unwrap();
                    let effective = if requested.is_empty() {
                        fauna_core::format::hex_full(&post_id)
                    } else {
                        requested.to_string()
                    };
                    self.published.borrow_mut().push(effective.clone());
                    serde_json::json!({ "slug": effective })
                }
                "fauna.web.publish.unset" => {
                    self.published.borrow_mut().clear();
                    serde_json::json!({ "ok": true })
                }
                // The FULL `WebDomainInfo` row shape the nest sends — the
                // wrapper's job is narrowing it to `WebDomainRow`, so the fake
                // must carry the fields it drops, not a pre-narrowed pair.
                "fauna.web.domain.get" => serde_json::json!({
                    "domains": [
                        {
                            "domain": "pending.example",
                            "verify_token": "tok-pending",
                            "txt_record": "_fauna-verify.pending.example",
                            "status": "pending",
                            "created_at": 1_700_000_000i64,
                            "verified_at": null,
                        },
                        {
                            "domain": "live.example",
                            "verify_token": "tok-live",
                            "txt_record": "_fauna-verify.live.example",
                            "status": "active",
                            "created_at": 1_700_000_001i64,
                            "verified_at": 1_700_000_002i64,
                        },
                    ]
                }),
                "fauna.web.publish.list" => {
                    let mut reply = serde_json::json!({
                        "posts": [
                            { "post_id": vec![0x01u8; 32], "slug": "gated-one", "gated_tier": "gold" },
                            // No `gated_tier` key at all — the ungated
                            // shape the wrapper must accept.
                            { "post_id": vec![0x02u8; 32], "slug": "public-one" },
                        ]
                    });
                    if let Some(down) = self.pages_down.get() {
                        reply["rendered_pages_down"] = down.into();
                    }
                    reply
                }
                "fauna.web.paywall.mint_token" => {
                    let slug = payload["slug"].as_str().unwrap_or("");
                    let path = payload["path"].as_str();
                    let scoped = match path {
                        Some(p) => p.to_string(),
                        None => format!("post/{slug}.html"),
                    };
                    self.minted_for.borrow_mut().push(scoped.clone());
                    serde_json::json!({
                        "token": "AbC-_123",
                        "expires": 1_700_000_600u64,
                        "path": scoped,
                    })
                }
                other => panic!("unexpected kind {other}"),
            };
            Ok(serde_json::from_value(reply).unwrap())
        }
    }

    #[tokio::test]
    async fn web_client_roundtrips_subdomain_and_apex() {
        let client = WebClient::new(FakeNest::default());

        assert!(!client.get_subdomain_enabled().await.unwrap());
        assert!(client.set_subdomain_enabled(true).await.unwrap());
        assert!(client.get_subdomain_enabled().await.unwrap());

        assert_eq!(client.get_apex_actor().await.unwrap(), None);
        let actor = vec![7u8; 32];
        assert_eq!(
            client.set_apex_actor(Some(actor.clone())).await.unwrap(),
            Some(actor.clone())
        );
        assert_eq!(client.get_apex_actor().await.unwrap(), Some(actor));
        assert_eq!(client.set_apex_actor(None).await.unwrap(), None);
    }

    #[tokio::test]
    async fn domain_get_narrows_the_nest_rows_and_feeds_site_link_view() {
        // The wrapper exists to hand `site_link_view` its custom-domain half,
        // so the assertion is end-to-end: narrow the reply, then resolve an
        // origin from it. A `pending` row sorts FIRST in the fake, which is
        // what proves the resolver picks by `status` and not by position.
        let client = WebClient::new(FakeNest::default());
        let rows = client.domain_get().await.unwrap();
        assert_eq!(
            rows,
            vec![
                WebDomainRow {
                    domain: "pending.example".to_string(),
                    status: "pending".to_string(),
                },
                WebDomainRow {
                    domain: "live.example".to_string(),
                    status: "active".to_string(),
                },
            ]
        );
        // Subdomain OFF and no handle — the two things that would otherwise
        // disable every copy affordance — so the only thing that can produce an
        // origin here is the active custom domain.
        let view = site_link_view(&rows, false, None, "example.com");
        assert_eq!(view.origin.as_deref(), Some("https://live.example/"));
        assert_eq!(view.disabled_reason, None);
    }

    #[tokio::test]
    async fn domain_get_with_no_rows_leaves_the_subdomain_path_to_decide() {
        // The common case: an actor who never registered a custom domain. The
        // wrapper must answer an empty vec (not an error), so the resolver
        // falls through to the subdomain rules rather than reporting a failure
        // the user cannot act on.
        struct NoDomains;
        impl RpcRequester for NoDomains {
            type Error = std::convert::Infallible;
            async fn request<Req, Reply>(
                &self,
                kind: &'static str,
                _payload: Req,
            ) -> Result<Reply, Self::Error>
            where
                Req: Serialize,
                Reply: serde::de::DeserializeOwned,
            {
                assert_eq!(kind, "fauna.web.domain.get");
                Ok(serde_json::from_value(serde_json::json!({ "domains": [] })).unwrap())
            }
        }
        let rows = WebClient::new(NoDomains).domain_get().await.unwrap();
        assert!(rows.is_empty());
        assert_eq!(
            site_link_view(&rows, false, Some("alice"), "example.com").disabled_reason,
            Some(SiteLinkDisabledReason::SubdomainDisabled)
        );
        assert_eq!(
            site_link_view(&rows, true, Some("alice"), "example.com")
                .origin
                .as_deref(),
            Some("https://alice.example.com/")
        );
    }

    #[tokio::test]
    async fn get_apex_actor_returns_none_after_clear() {
        // Distinct from the round-trip test's final assertion: pins that a
        // cleared apex actor reads back as `None`, not just that
        // `set_apex_actor(None)` echoes `None`.
        let client = WebClient::new(FakeNest::default());
        client.set_apex_actor(Some(vec![9u8; 32])).await.unwrap();
        client.set_apex_actor(None).await.unwrap();
        assert_eq!(client.get_apex_actor().await.unwrap(), None);
    }

    // `arc_with_non_send_sync`: `FakeNest`'s interior mutability is `Cell`/
    // `RefCell` (a single-threaded test double, used by value everywhere else),
    // so the `Arc` here is not Send+Sync — but `Arc` is exactly what
    // fauna-protocol's blanket `RpcRequester` impl is written for, and this test
    // runs on one thread. Making the double `Mutex`-backed to satisfy the lint
    // would rewrite every other test in this module for no coverage.
    #[allow(clippy::arc_with_non_send_sync)]
    #[tokio::test]
    async fn requests_carry_the_exact_fauna_web_kind_strings() {
        // `FakeNest.kinds` was recorded but never asserted — nothing pinned
        // the four `fauna.web.*` kind strings against a silent rename.
        // `Arc` (fauna-protocol blanket-impls `RpcRequester for Arc<T>`) lets
        // the test keep a handle to the transport after handing a clone to
        // the client.
        let nest = std::sync::Arc::new(FakeNest::default());
        let client = WebClient::new(nest.clone());
        client.get_subdomain_enabled().await.unwrap();
        client.set_subdomain_enabled(true).await.unwrap();
        client.get_apex_actor().await.unwrap();
        client.set_apex_actor(None).await.unwrap();
        assert_eq!(
            nest.kinds.borrow().as_slice(),
            [
                "fauna.web.get_subdomain_enabled",
                "fauna.web.set_subdomain_enabled",
                "fauna.web.get_apex_actor",
                "fauna.web.set_apex_actor",
            ]
        );
    }

    #[tokio::test]
    async fn get_subdomain_enabled_propagates_transport_error_unchanged() {
        let client = WebClient::new(FailingRequester::new("transport unreachable"));
        assert_eq!(
            client.get_subdomain_enabled().await,
            Err(FailedRequest("transport unreachable".to_string()))
        );
    }

    #[tokio::test]
    async fn set_subdomain_enabled_propagates_transport_error_unchanged() {
        let client = WebClient::new(FailingRequester::new("transport unreachable"));
        assert_eq!(
            client.set_subdomain_enabled(true).await,
            Err(FailedRequest("transport unreachable".to_string()))
        );
    }

    #[tokio::test]
    async fn get_apex_actor_propagates_transport_error_unchanged() {
        let client = WebClient::new(FailingRequester::new("transport unreachable"));
        assert_eq!(
            client.get_apex_actor().await,
            Err(FailedRequest("transport unreachable".to_string()))
        );
    }

    #[tokio::test]
    async fn set_apex_actor_propagates_transport_error_unchanged() {
        let client = WebClient::new(FailingRequester::new("transport unreachable"));
        assert_eq!(
            client.set_apex_actor(Some(vec![1u8; 32])).await,
            Err(FailedRequest("transport unreachable".to_string()))
        );
    }

    #[tokio::test]
    async fn domain_get_propagates_transport_error_unchanged() {
        // An unreadable domain list must NOT degrade to "no custom domains" —
        // that would silently resolve the origin to the subdomain and hand out
        // the wrong link. The error has to reach the page.
        let client = WebClient::new(FailingRequester::new("transport unreachable"));
        assert_eq!(
            client.domain_get().await,
            Err(FailedRequest("transport unreachable".to_string()))
        );
    }

    // ── Published-post management ───────────────────────────────────────

    #[tokio::test]
    async fn publish_set_returns_the_effective_slug_not_the_requested_one() {
        let client = WebClient::new(FakeNest::default());

        // An explicit slug comes back as-is…
        assert_eq!(
            client
                .publish_set(vec![0x11; 32], Some("my-post".into()))
                .await
                .unwrap(),
            "my-post"
        );
        // …but `None` takes the nest's default (post-id hex), and THAT is what
        // the copy-link affordance must build its URL from — a client that
        // echoed its own `None` would produce a dead link.
        assert_eq!(
            client.publish_set(vec![0xab; 32], None).await.unwrap(),
            "ab".repeat(32)
        );
    }

    #[tokio::test]
    async fn publish_unset_is_a_plain_ok() {
        let client = WebClient::new(FakeNest::default());
        assert!(client.publish_unset(vec![0x11; 32]).await.unwrap());
    }

    #[tokio::test]
    async fn publish_list_accepts_rows_with_and_without_a_tier() {
        let client = WebClient::new(FakeNest::default());
        let posts = client.publish_list().await.unwrap().posts;
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0].slug, "gated-one");
        assert_eq!(
            posts[0].gated_tier.as_deref(),
            Some("gold"),
            "a gated row carries the tier that gates the paywall-link affordance"
        );
        assert_eq!(posts[1].slug, "public-one");
        assert_eq!(
            posts[1].gated_tier, None,
            "a row with no tier key at all (ungated) must \
             decode, not fail — the affordance simply doesn't render"
        );
    }

    #[tokio::test]
    async fn publish_list_carries_whether_the_rendered_pages_are_down() {
        let client = WebClient::new(FakeNest::default());
        assert!(
            !client.publish_list().await.unwrap().rendered_pages_down,
            "an omitted key reads as healthy"
        );

        client.nest.pages_down.set(Some(true));
        let site = client.publish_list().await.unwrap();
        assert!(site.rendered_pages_down);
        assert_eq!(site.posts.len(), 2, "a dark site still lists its posts");
    }

    #[tokio::test]
    async fn paywall_mint_token_scopes_a_post_slug_and_a_file_path() {
        let client = WebClient::new(FakeNest::default());

        let post = client
            .paywall_mint_token(PaywallTarget::PostSlug {
                slug: "my-post".into(),
            })
            .await
            .unwrap();
        assert_eq!(post.path, "post/my-post.html");
        assert_eq!(post.token, "AbC-_123");
        assert_eq!(post.expires, 1_700_000_600);

        let file = client
            .paywall_mint_token(PaywallTarget::FilePath {
                path: "downloads/report.pdf".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            file.path, "downloads/report.pdf",
            "a file target is scoped to its path verbatim, never wrapped in post/*.html"
        );
    }

    // ── site_link_view: the origin precedence ───────────────────────────

    fn active(domain: &str) -> Vec<WebDomainRow> {
        vec![WebDomainRow {
            domain: domain.to_string(),
            status: "active".to_string(),
        }]
    }

    #[test]
    fn site_link_view_prefers_an_active_custom_domain_over_the_subdomain() {
        let v = site_link_view(
            &active("blog.example.com"),
            true,
            Some("alice"),
            "example.com",
        );
        assert_eq!(v.origin.as_deref(), Some("https://blog.example.com/"));
        assert_eq!(v.disabled_reason, None);
    }

    #[test]
    fn site_link_view_ignores_a_custom_domain_that_is_not_active_yet() {
        // `pending`/`verified` have no cert — linking there hands out a URL
        // that fails to load, so the subdomain wins until the domain goes live.
        for status in ["pending", "verified"] {
            let domains = vec![WebDomainRow {
                domain: "blog.example.com".to_string(),
                status: status.to_string(),
            }];
            let v = site_link_view(&domains, true, Some("alice"), "example.com");
            assert_eq!(
                v.origin.as_deref(),
                Some("https://alice.example.com/"),
                "status {status} must not win the precedence"
            );
        }
    }

    #[test]
    fn site_link_view_falls_back_to_the_subdomain() {
        let v = site_link_view(&[], true, Some("alice"), "example.com");
        assert_eq!(v.origin.as_deref(), Some("https://alice.example.com/"));
        assert_eq!(v.disabled_reason, None);
    }

    #[test]
    fn site_link_view_disables_with_a_reason_when_there_is_no_origin() {
        // Subdomain off — the one reason the user can fix from this page.
        let off = site_link_view(&[], false, Some("alice"), "example.com");
        assert_eq!(off.origin, None);
        assert_eq!(
            off.disabled_reason,
            Some(SiteLinkDisabledReason::SubdomainDisabled)
        );

        // On, but nothing to key the host on.
        let no_handle = site_link_view(&[], true, None, "example.com");
        assert_eq!(no_handle.origin, None);
        assert_eq!(
            no_handle.disabled_reason,
            Some(SiteLinkDisabledReason::NoHandle)
        );
        assert_eq!(
            site_link_view(&[], true, Some(""), "example.com").disabled_reason,
            Some(SiteLinkDisabledReason::NoHandle)
        );

        // On, but the handle is a label the nest never serves.
        let reserved = site_link_view(&[], true, Some("www"), "example.com");
        assert_eq!(reserved.origin, None);
        assert_eq!(
            reserved.disabled_reason,
            Some(SiteLinkDisabledReason::ReservedLabel)
        );
    }

    /// The defect this whole surface was rebuilt around: a nest that serves no
    /// web content must produce **no origin and a reason**, never a composed
    /// URL. Before the fix this returned `origin: None, disabled_reason: None`
    /// — a blank row that says nothing — while each app separately substituted
    /// a host of its own (the dialed address, or `handle_domain()`'s
    /// `"localhost"` placeholder) and handed out a link that cannot load.
    #[test]
    fn no_serving_domain_disables_with_a_reason_rather_than_composing_a_dead_link() {
        let v = subdomain_view(true, Some("alice"), "");
        assert_eq!(
            v.url, None,
            "an empty serving domain must compose NO url — this is the dead link"
        );
        assert_eq!(
            v.disabled_reason,
            Some(SubdomainDisabledReason::NoServingDomain),
            "and it must SAY so: a blank row with no reason is the failure mode \
             `web-content-hosting.md` § Published-post management forbids"
        );

        let link = site_link_view(&[], true, Some("alice"), "");
        assert_eq!(link.origin, None);
        assert_eq!(
            link.disabled_reason,
            Some(SiteLinkDisabledReason::NoServingDomain)
        );
    }

    /// A nest-level "I serve nothing" outranks every per-user reason, because
    /// no per-user action changes it — a user told "you have no handle" on a
    /// domainless box would go set a handle and still have no site.
    #[test]
    fn no_serving_domain_outranks_the_per_user_reasons() {
        for handle in [None, Some(""), Some("www"), Some("alice")] {
            assert_eq!(
                subdomain_view(true, handle, "").disabled_reason,
                Some(SubdomainDisabledReason::NoServingDomain),
                "handle {handle:?} on a nest with no serving domain"
            );
        }
    }

    /// …but an **active custom domain** still wins, because it carries its own
    /// cert and its own resolver entry — it does not route through the nest's
    /// apex/subdomain path at all.
    #[test]
    fn an_active_custom_domain_serves_even_when_the_nest_has_no_serving_domain() {
        let rows = [WebDomainRow {
            domain: "alice.com".into(),
            status: "active".into(),
        }];
        let v = site_link_view(&rows, true, Some("alice"), "");
        assert_eq!(v.origin.as_deref(), Some("https://alice.com/"));
        assert_eq!(v.disabled_reason, None);
    }

    /// `Some("")` is an ANSWER ("I serve nothing") and must be honoured, and
    /// an absent field reads the same way — never as a licence to substitute
    /// the dialed host (the older-nest fallback the compat-remnant sweep
    /// removed).
    #[test]
    fn serving_domain_honours_the_answer_and_reads_silence_as_serving_nothing() {
        assert_eq!(serving_domain(Some("web.test")), "web.test");
        assert_eq!(
            serving_domain(Some("")),
            "",
            "an empty answer is the nest saying it serves nothing — substituting \
             the dialed host here re-creates the exact dead link this fixes"
        );
        assert_eq!(
            serving_domain(None),
            "",
            "an absent field is not a licence to guess a host"
        );
    }

    #[test]
    fn site_link_view_agrees_with_the_subdomain_toggle_on_eligibility() {
        // The two surfaces read the same rules; a handle the toggle refuses to
        // serve must never yield a copyable link.
        for handle in [Some("mail"), Some("www"), Some("app"), None, Some("")] {
            let sub = subdomain_view(true, handle, "example.com");
            let link = site_link_view(&[], true, handle, "example.com");
            assert_eq!(
                sub.url.is_some(),
                link.origin.is_some(),
                "toggle and link surface disagree for handle {handle:?}"
            );
        }
    }

    // ── URL builders ────────────────────────────────────────────────────

    #[test]
    fn post_page_url_is_the_tokenless_public_link() {
        assert_eq!(
            post_page_url("https://alice.example.com/", "my-post"),
            "https://alice.example.com/post/my-post.html"
        );
        // Defensive: an origin without the trailing slash must not produce a
        // double-slash-free concatenation like `…fanpost/…`.
        assert_eq!(
            post_page_url("https://alice.example.com", "my-post"),
            "https://alice.example.com/post/my-post.html"
        );
    }

    #[test]
    fn tokened_url_appends_the_minted_token_to_the_nest_scoped_path() {
        assert_eq!(
            tokened_url(
                "https://alice.example.com/",
                "post/my-post.html",
                "AbC-_123"
            ),
            "https://alice.example.com/post/my-post.html?token=AbC-_123"
        );
        // A leading slash on the nest's path must not double up.
        assert_eq!(
            tokened_url("https://alice.example.com/", "/downloads/r.pdf", "T"),
            "https://alice.example.com/downloads/r.pdf?token=T"
        );
    }

    #[tokio::test]
    async fn the_two_copy_links_differ_only_by_the_token() {
        // The pair a creator copies from one row: the selling link (teaser) and
        // the preview link (full body). Same page, one carries the capability.
        let client = WebClient::new(FakeNest::default());
        let origin = site_link_view(&[], true, Some("alice"), "example.com")
            .origin
            .unwrap();
        let public = post_page_url(&origin, "my-post");
        let minted = client
            .paywall_mint_token(PaywallTarget::PostSlug {
                slug: "my-post".into(),
            })
            .await
            .unwrap();
        let full = tokened_url(&origin, &minted.path, &minted.token);
        assert_eq!(full, format!("{public}?token={}", minted.token));
    }

    #[test]
    fn disabled_reason_text_picks_a_distinct_real_key_per_reason() {
        let cases = [
            (None, "web_settings.link_disabled_subdomain_off"),
            (
                Some(SiteLinkDisabledReason::SubdomainDisabled),
                "web_settings.link_disabled_subdomain_off",
            ),
            (
                Some(SiteLinkDisabledReason::NoHandle),
                "web_settings.link_disabled_no_handle",
            ),
            (
                Some(SiteLinkDisabledReason::ReservedLabel),
                "web_settings.link_disabled_reserved",
            ),
            (
                Some(SiteLinkDisabledReason::NoServingDomain),
                "web_settings.subdomain_no_serving_domain",
            ),
        ];
        for (reason, key) in cases {
            let lt = disabled_reason_text(reason);
            assert_eq!(lt.key, key, "reason {reason:?}");
            assert!(
                fauna_i18n::strings::lookup(key).is_some(),
                "key {key} must resolve through the real string table"
            );
        }
    }

    #[test]
    fn disabled_reason_text_resolves_to_the_real_english_copy() {
        assert_eq!(
            disabled_reason_text(Some(SiteLinkDisabledReason::NoHandle))
                .resolve(fauna_i18n::strings::lookup),
            "Set a handle first — your published posts need a web address before they can be linked to."
        );
    }
}
