//! Static file serving and host-header routing for web content hosting.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::http::{Response, StatusCode, header};
use tokio::sync::RwLock;

use crate::blob_store::BlobStoreBackend;
use crate::db::CacheDb;
use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;

// ==================== PathSource ====================

/// Indicates which table a resolved file came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSource {
    /// File came from `web_files` (user-uploaded).
    UserFile,
    /// File came from `web_rendered` (pipeline output).
    Rendered,
}

// ==================== resolve_path ====================

/// Resolve a request path to `(row, source)`.
///
/// Resolution order:
/// 1. Exact match in `web_files` → `PathSource::UserFile`
/// 2. Exact match in `web_rendered` → `PathSource::Rendered`
/// 3. For directory-like paths (ends with `/` or has no extension):
///    try `{path}/index.html` or `{path}index.html` in user files then rendered
///
/// Returns `None` if nothing matches. The whole row comes back (not just its
/// hash) because the *source* determines how the hash must be read — a
/// `UserFile` hash is a manifest to walk and may be content-key-sealed, a
/// `Rendered` hash is a verbatim body blob.
pub async fn resolve_path(
    db: &CacheDb,
    actor_id: &[u8; 32],
    path: &str,
) -> anyhow::Result<Option<(crate::db::web::WebFileRow, PathSource)>> {
    // Strip leading `/`
    let normalized = path.trim_start_matches('/');

    // 1. Exact match in web_files
    if let Some(row) = db.get_web_file(actor_id, normalized).await? {
        return Ok(Some((row, PathSource::UserFile)));
    }

    // 2. Exact match in web_rendered
    if let Some(row) = db.get_web_rendered(actor_id, normalized).await? {
        return Ok(Some((row, PathSource::Rendered)));
    }

    // 3. Directory-like fallback — look for index.html
    let is_directory_like =
        normalized.is_empty() || normalized.ends_with('/') || !has_extension(normalized);

    if is_directory_like {
        // An extensionless page URL (`/post/my-slug` — what the built-in index
        // links) resolves to its rendered `.html` twin before the index.html
        // directory probes.
        if !normalized.is_empty() && !normalized.ends_with('/') {
            let candidate_html = format!("{normalized}.html");
            if let Some(row) = db.get_web_file(actor_id, &candidate_html).await? {
                return Ok(Some((row, PathSource::UserFile)));
            }
            if let Some(row) = db.get_web_rendered(actor_id, &candidate_html).await? {
                return Ok(Some((row, PathSource::Rendered)));
            }
        }
        // Try "{normalized}/index.html" (for paths like "blog/" or "blog")
        let candidate_slash = if normalized.is_empty() {
            "index.html".to_string()
        } else if normalized.ends_with('/') {
            format!("{normalized}index.html")
        } else {
            format!("{normalized}/index.html")
        };

        // Also try "{normalized}index.html" (for paths already ending in /)
        let candidate_direct = if normalized.ends_with('/') {
            format!("{normalized}index.html")
        } else {
            candidate_slash.clone()
        };

        // Check user files first, then rendered
        for candidate in [&candidate_slash, &candidate_direct]
            .iter()
            .collect::<Vec<_>>()
        {
            if let Some(row) = db.get_web_file(actor_id, candidate).await? {
                return Ok(Some((row, PathSource::UserFile)));
            }
        }
        for candidate in [&candidate_slash, &candidate_direct]
            .iter()
            .collect::<Vec<_>>()
        {
            if let Some(row) = db.get_web_rendered(actor_id, candidate).await? {
                return Ok(Some((row, PathSource::Rendered)));
            }
        }
    }

    Ok(None)
}

/// Returns true if the path has a file extension (a `.` after the last `/`).
fn has_extension(path: &str) -> bool {
    let filename = path.rsplit('/').next().unwrap_or(path);
    // A leading dot (hidden file) without a second dot does not count as an extension.
    if let Some(rest) = filename.strip_prefix('.') {
        rest.contains('.')
    } else {
        filename.contains('.')
    }
}

// ==================== is_rejected_extension ====================

/// Returns true if the path ends with a server-side script extension that
/// should never be served as static content.
pub fn is_rejected_extension(path: &str) -> bool {
    rejected_extension(path).is_some()
}

/// The rejected server-side extension a path matched, if any — the matched
/// value is one of these fixed public constants (never a user-chosen
/// substring), so a log line may carry it verbatim where the path itself
/// must redact (`fauna_core::log_redact`).
pub fn rejected_extension(path: &str) -> Option<&'static str> {
    const REJECTED: &[&str] = &[
        ".php", ".py", ".rb", ".cgi", ".asp", ".aspx", ".jsp", ".pl", ".sh",
    ];
    let lower = path.to_lowercase();
    REJECTED.iter().find(|ext| lower.ends_with(*ext)).copied()
}

// ==================== is_reserved_subdomain_label ====================

/// Reserved subdomain labels that must never be claimed as a user handle
/// subdomain — `mail`/`mta-sts`/`app` have their own routes/SNI (the host-level
/// `HostResolver::is_reserved_host` mirror), and `www` is reserved for a future
/// apex alias. An opted-in user whose handle equals one of these is skipped at
/// registration + cert issuance (defense-in-depth: the reserved hosts are
/// excluded in `resolve` regardless, but skipping issuance avoids minting a cert
/// for a name the resolver will never serve as user content). The `_`-prefixed
/// `_acme-challenge` host cannot collide — handles cannot start with `_`.
pub fn is_reserved_subdomain_label(label: &str) -> bool {
    // Single source of truth shared with the client `web-settings` UI
    // (priority #2; web-content-hosting.md § Architectural rules #9).
    fauna_core::web::is_reserved_subdomain_label(label)
}

// ==================== HostResolver ====================

/// Maps host headers (apex, subdomains, and custom domains) to actor IDs.
///
/// Resolution precedence (`web-content-hosting.md` § Routing): reserved hosts
/// (`mail.`/`mta-sts.`/`app.<domain>`, `_acme-challenge.*`) are **never** user
/// content; then an exact custom-domain match; then an opted-in `<handle>`
/// subdomain; then the **apex actor** as the catch-all for the node domain
/// itself, the bare/absent host, and any other unmatched host — the direct
/// analogue of the catch-all *mail* actor (the domain's unaddressed traffic).
/// A `None` apex ⇒ the caller serves the built-in nest info page.
pub struct HostResolver {
    subdomains: RwLock<HashMap<String, [u8; 32]>>,
    custom_domains: RwLock<HashMap<String, [u8; 32]>>,
    /// The admin-designated apex actor (nest-wide singleton), or `None` when no
    /// actor is designated (apex → info page). Updated live on
    /// `fauna.web.set_apex_actor` and seeded at boot from `db::web_apex`.
    apex_actor: RwLock<Option<[u8; 32]>>,
    /// The nest's apex domain (e.g. `"nest.fauna.social"`), lowercased. **Live:**
    /// seeded at boot from the resolved identity, then swapped by
    /// [`set_nest_domain`](Self::set_nest_domain) — which
    /// `identity_domain_core::apply_primary_identity` calls when a domain is
    /// claimed **post-boot** — so subdomain routing + the reserved-host guard
    /// follow the claim with no restart (`domains-and-tls-bootstrap.md` §
    /// Implementation status — web apex live). `ArcSwap`, not a plain `String`,
    /// so the lock-free per-request `resolve` hot path reads it without a lock;
    /// empty on a domainless box.
    nest_domain: ArcSwap<String>,
}

impl HostResolver {
    /// Create a new `HostResolver` for the given nest domain
    /// (e.g. `"nest.fauna.social"`). The domain is lowercased so the
    /// reserved-host check and subdomain-suffix match are case-insensitive.
    pub fn new(nest_domain: String) -> Self {
        Self {
            subdomains: RwLock::new(HashMap::new()),
            custom_domains: RwLock::new(HashMap::new()),
            apex_actor: RwLock::new(None),
            nest_domain: ArcSwap::from_pointee(nest_domain.to_ascii_lowercase()),
        }
    }

    /// Update the apex domain live — `identity_domain_core::apply_primary_identity`
    /// calls this when a domain is claimed **post-boot** (a domainless-booted box's
    /// first claim / client add-first-domain), so subdomain routing and the
    /// reserved-host guard follow the claim with no restart. Lowercased to match the
    /// constructor + the case-insensitive match. `&self` + lock-free store,
    /// so the sync `apply_primary_identity` calls it directly.
    pub fn set_nest_domain(&self, nest_domain: &str) {
        self.nest_domain
            .store(Arc::new(nest_domain.to_ascii_lowercase()));
    }

    /// The current apex domain (lowercased; empty on a domainless box). Live:
    /// follows [`set_nest_domain`](Self::set_nest_domain) swaps, so a per-tick
    /// reader (the web cert lifecycle loop) observes a post-boot claim without
    /// a restart — the cert half of the same live-apex mechanism routing uses.
    pub fn nest_domain(&self) -> Arc<String> {
        self.nest_domain.load_full()
    }

    /// Reserved hosts that must never be served as user web content
    /// (`web-content-hosting.md` § Routing / Same-origin security model
    /// invariant 3). `mail.`/`mta-sts.`/`app.<domain>` have their own
    /// routes/SNI above this fallback; `_acme-challenge.*` is the HTTP-01
    /// challenge host. Excluded here as defense-in-depth so the apex catch-all
    /// can never shadow them.
    fn is_reserved_host(&self, host: &str) -> bool {
        // `_acme-challenge.*` is a wildcard prefix; the `mail.`/`mta-sts.`/`app.`
        // sub-hosts share `fauna_core::web::is_reserved_nest_host` with the
        // custom-domain registration guard so the two never drift.
        let nest_domain = self.nest_domain.load();
        host.starts_with("_acme-challenge.")
            || fauna_core::web::is_reserved_nest_host(host, &nest_domain)
    }

    /// Resolve a host header value to an actor ID (see the type-level doc for
    /// the precedence).
    pub async fn resolve(&self, host: &str) -> Option<[u8; 32]> {
        // Normalize before matching: strip a port suffix, lowercase, and trim a
        // trailing FQDN dot — so `App.example.com` / `app.example.com.` can't bypass
        // the reserved-host check or the subdomain-suffix match. Real
        // browsers normalize Host, but an attacker-crafted request need not.
        let host = host.split(':').next().unwrap_or(host);
        let host = fauna_core::web::normalize_dns_name(host);
        let host = host.as_str();

        // Reserved hosts are never user content (defense-in-depth).
        if self.is_reserved_host(host) {
            return None;
        }

        // Check custom domains first (exact match).
        {
            let custom = self.custom_domains.read().await;
            if let Some(&id) = custom.get(host) {
                return Some(id);
            }
        }

        // Check subdomains: strip ".{nest_domain}" suffix.
        let suffix = format!(".{}", self.nest_domain.load().as_str());
        if let Some(sub) = host.strip_suffix(&suffix) {
            let subs = self.subdomains.read().await;
            if let Some(&id) = subs.get(sub) {
                return Some(id);
            }
        }

        // Apex catch-all: the node domain itself, the bare/absent host, and any
        // other unmatched (non-reserved) host resolve to the designated apex
        // actor. `None` ⇒ the caller serves the built-in info page.
        *self.apex_actor.read().await
    }

    /// Register a subdomain → actor_id mapping (the `<handle>` label, without the
    /// `.<nest_domain>` suffix). On a user's opt-in, `start_server` (boot) and
    /// the `fauna.web.set_subdomain_enabled` handler call this with the actor's
    /// handle.
    pub async fn register_subdomain(&self, subdomain: &str, actor_id: [u8; 32]) {
        let mut subs = self.subdomains.write().await;
        subs.insert(subdomain.to_string(), actor_id);
    }

    /// Remove a subdomain mapping (a user opting out of subdomain hosting). The
    /// sibling of [`remove_custom_domain`](Self::remove_custom_domain); the
    /// `fauna.web.set_subdomain_enabled(false)` handler calls it, and the
    /// per-subdomain cert lifecycle separately drops the cert.
    pub async fn remove_subdomain(&self, subdomain: &str) {
        let mut subs = self.subdomains.write().await;
        subs.remove(subdomain);
    }

    /// Remove a custom domain mapping — `fauna.web.domain.delete` calls it so a
    /// deregistered site stops being served at once, ahead of the lifecycle
    /// task's next reconcile (`web-content-hosting.md` § Per-domain TLS →
    /// Removal). There is no single-domain `register` twin on purpose: additions
    /// only ever come from the `active`-set projection below, so no caller can
    /// route a domain the table does not say is live.
    pub async fn remove_custom_domain(&self, domain: &str) {
        let mut custom = self.custom_domains.write().await;
        custom.remove(domain);
    }

    /// Replace the whole custom-domain map with `desired` — the `active`
    /// `web_domains` rows projected to `domain → owner`.
    ///
    /// The steady-state authority over this map: the domain lifecycle task runs
    /// it every pass (`domain::reconcile_custom_domain_routing_once`), and the
    /// boot seed runs it once, so a domain that goes `active` post-boot starts
    /// routing with no restart and one whose row is gone stops. Replace-all
    /// under a single write lock, not a diff: the map IS the projection, so
    /// there is no torn window where a still-active domain is briefly absent.
    ///
    /// ⚠ The caller MUST NOT pass a partial set — an empty map on a failed DB
    /// read would deroute every live site. `reconcile_custom_domain_routing_once`
    /// propagates the read error instead of reconciling (the same guard
    /// `cert::reconcile_once` documents for the cert map).
    pub async fn reconcile_custom_domains(&self, desired: HashMap<String, [u8; 32]>) {
        *self.custom_domains.write().await = desired;
    }

    /// Set (or clear, with `None`) the apex actor — `fauna.web.set_apex_actor`
    /// updates the live resolver through here, and `start_server` seeds it at
    /// boot from `db::web_apex`.
    pub async fn set_apex_actor(&self, actor_id: Option<[u8; 32]>) {
        *self.apex_actor.write().await = actor_id;
    }
}

// ==================== serve_web_content ====================

/// Built-in 404 page returned when no 404.html is found in the site.
const BUILTIN_404: &str = "<!DOCTYPE html><html><head><title>404 Not Found</title></head>\
<body><h1>404 Not Found</h1><p>The requested page does not exist.</p></body></html>";

/// Serve a web content response for the given actor and request path.
///
/// Resolution:
/// - Calls `resolve_path`; on miss tries `404.html` from user files or rendered,
///   then falls back to a built-in 404 response.
/// - Sets `Content-Type` from the stored record.
/// - Sets `Cache-Control: public, max-age=300` for HTML, `public, max-age=86400` for
///   other assets.
pub async fn serve_web_content(
    db: &CacheDb,
    blob_store: &Arc<dyn BlobStoreBackend>,
    at_rest_key: Option<&BackupKey>,
    actor_id: &[u8; 32],
    request_path: &str,
) -> Response<Body> {
    serve_web_content_with_token(
        db,
        blob_store,
        at_rest_key,
        actor_id,
        request_path,
        None,
        None,
    )
    .await
}

/// [`serve_web_content`] plus the web-paywall gate (`monetization.md`
/// § Pillar 2): when the visitor presents a valid capability token
/// (`?token=…`, verified STATELESSLY against the web-serve holder's key) for a
/// path with a sealed full render, the holder unseals it **at serve time**
/// under its live grant and serves the pre-rendered bytes. Every other
/// outcome — no/invalid/expired/forged token, no sealed row, revoked grant
/// (no period key ⇒ unseal impossible) — falls through to the normal public
/// resolution, whose canonical page for a gated post is the teaser. No cookie
/// is ever set; nothing is rendered per-request.
pub async fn serve_web_content_with_token(
    db: &CacheDb,
    blob_store: &Arc<dyn BlobStoreBackend>,
    at_rest_key: Option<&BackupKey>,
    actor_id: &[u8; 32],
    request_path: &str,
    holder: Option<&super::holder::WebServeHolder>,
    token: Option<&str>,
) -> Response<Body> {
    // Reject server-side script extensions immediately
    if is_rejected_extension(request_path) {
        return not_found_response(b"Not Found".to_vec(), "text/plain");
    }

    if let (Some(holder), Some(token)) = (holder, token)
        && let Some(resp) =
            try_serve_sealed(db, blob_store, actor_id, request_path, holder, token).await
    {
        return resp;
    }

    // Resolve the requested path
    match resolve_path(db, actor_id, request_path).await {
        Ok(Some((row, PathSource::UserFile))) => {
            serve_user_file(db, blob_store, at_rest_key, actor_id, &row, holder, token).await
        }
        // Rendered output is the nest's own product, stored verbatim.
        Ok(Some((row, PathSource::Rendered))) => {
            serve_blob(blob_store, &row.blob_hash, &row.content_type).await
        }
        Ok(None) => {
            // Try 404.html from user files then rendered. A user-file 404.html
            // is a manifest like any synced file; a sealed one is unreadable on
            // this arm (no grant here) and falls back to the built-in page.
            let body = match try_resolve_404(db, actor_id).await {
                // The rule: a user-file `404.html` is a `web_files` row like
                // any other and owes the same folder gate the main door asks —
                // `try_resolve_404` looks it up by path alone, so without this
                // the custom 404 page kept serving after its folder was
                // switched off OR flipped back to private.
                Some((row, PathSource::UserFile)) => {
                    match (
                        as_hash32(&row.blob_hash),
                        gate_web_file_folder(db, &row).await,
                    ) {
                        (Some(h), Some(_folder)) => super::file_bytes::read_file_by_manifest(
                            db,
                            blob_store,
                            at_rest_key,
                            &h,
                            &[],
                        )
                        .await
                        .ok()
                        .map(|b| (b, row.content_type)),
                        _ => None,
                    }
                }
                Some((row, PathSource::Rendered)) => Some((
                    fetch_blob_bytes(blob_store, &row.blob_hash).await,
                    row.content_type,
                )),
                None => None,
            };
            match body {
                Some((body, content_type)) => build_response(
                    StatusCode::NOT_FOUND,
                    body,
                    &content_type,
                    is_html(&content_type),
                ),
                None => build_response(
                    StatusCode::NOT_FOUND,
                    BUILTIN_404.as_bytes().to_vec(),
                    "text/html; charset=utf-8",
                    true,
                ),
            }
        }
        Err(_) => build_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            b"Internal Server Error".to_vec(),
            "text/plain",
            false,
        ),
    }
}

/// Serve a file synced into a `web`-mode folder (a `web_files` row).
///
/// The row's `blob_hash` is a **manifest** hash, so the body is assembled by
/// walking manifest → chunks (`file_bytes::read_file_by_manifest`), never by
/// emitting the stored blob verbatim.
///
/// Two classes, discriminated by `content_key_version`:
///
/// - **Plaintext** (`None`): public web hosting — serve it.
/// - **Sealed** (`Some(v)`): the file lives in a content-key-sealed set
///   (`monetization.md` § Pillar 2). It serves **only** through the paywall
///   token gate: a valid capability token + a live `content.read{folder:set}`
///   grant for generation `v` ⇒ decrypt and serve `private, no-store`.
///   Anything else ⇒ the set's paywall teaser, and **never the ciphertext**.
///
/// The fail-closed rule (`web-content-hosting.md` § Sealed static files) has
/// three arms here, and none of them can emit bytes: no paywall tier on the set
/// (a sealed file in a non-paywalled set — e.g. a shared set — is not web
/// content at all → 404), no/invalid token → teaser, no live grant → teaser.
async fn serve_user_file(
    db: &CacheDb,
    blob_store: &Arc<dyn BlobStoreBackend>,
    at_rest_key: Option<&BackupKey>,
    actor_id: &[u8; 32],
    row: &crate::db::web::WebFileRow,
    holder: Option<&super::holder::WebServeHolder>,
    token: Option<&str>,
) -> Response<Body> {
    let Some(hash) = as_hash32(&row.blob_hash) else {
        return build_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            b"Invalid hash".to_vec(),
            "text/plain",
            false,
        );
    };

    // The folder gate — BOTH gates, in the one place every door must ask
    // (see [`gate_web_file_folder`]).
    let Some(folder) = gate_web_file_folder(db, row).await else {
        return not_found_response(b"Not Found".to_vec(), "text/plain");
    };

    // ── Plaintext: today's public path (now walking the manifest, as it always
    // should have — the stored blob is CBOR, not the file body). ──
    let Some(version) = row.content_key_version else {
        return match super::file_bytes::read_file_by_manifest(
            db,
            blob_store,
            at_rest_key,
            &hash,
            &[],
        )
        .await
        {
            Ok(bytes) => build_response(
                StatusCode::OK,
                bytes,
                &row.content_type,
                is_html(&row.content_type),
            ),
            // Compelled removal is disclosed, not hidden behind the 404 below:
            // a file whose manifest or any chunk is withheld answers 451, the
            // same shape the routes onto the blob store give.
            Err(super::file_bytes::FileOpenError::Withheld) => withheld_response(),
            // A plaintext row that turns out to be sealed at the manifest level
            // (`stored_hashes`) has no key here and MUST NOT emit ciphertext.
            Err(e) => {
                tracing::warn!(error = %e, path = %row.path, "web file unreadable");
                not_found_response(b"Not Found".to_vec(), "text/plain")
            }
        };
    };

    // ── Sealed: the paywall gate. ──
    // The set carries the tier; no tier ⇒ this sealed file is not paywalled web
    // content at all. Fail closed: 404, never bytes. (The gate above already
    // resolved the folder and refused a folder-less row.)
    let set = folder;
    let Some(tier) = set.web_paywall_tier.clone() else {
        tracing::warn!(
            // Website-folder paths are public URLs, exempt from sealing by design
            // (`file-sync.md` § Sealed names & paths "Deliberate non-seals").
            // The set NAME is not exempt — a paywall-config set is a normal
            // user-chosen name, so it redacts like any other.
            path = %row.path,
            set = %fauna_core::log_redact::log_folder_name(&set.name),
            "sealed web file in a NON-paywalled set — refusing to serve (fail closed)"
        );
        return not_found_response(b"Not Found".to_vec(), "text/plain");
    };

    // A valid token + a live grant is the only path to the bytes.
    if let (Some(holder), Some(token)) = (holder, token)
        && super::token::WebPaywallToken::verify(
            token,
            &holder.ed25519_pubkey(),
            actor_id,
            &row.path,
        )
        .is_some()
    {
        let grant_set = holder.registry.current();
        // The grant names the set by its hash, the address a sealed set's row
        // keeps; a row without one has no grant to match (fail closed).
        let keys = set
            .name_hash
            .as_deref()
            .map(|name_hash| {
                grant_set.keys_for_folder(
                    actor_id,
                    name_hash,
                    version as u64,
                    crate::db::now_epoch_secs() as u64,
                )
            })
            .unwrap_or_default();
        if !keys.is_empty() {
            match super::file_bytes::read_file_by_manifest(
                db,
                blob_store,
                at_rest_key,
                &hash,
                &keys,
            )
            .await
            {
                Ok(bytes) => {
                    // Entitled content is visitor-specific: never cacheable by a
                    // shared cache (the URL carries the token). No Set-Cookie.
                    return Response::builder()
                        .status(StatusCode::OK)
                        .header("Content-Type", row.content_type.as_str())
                        .header("Cache-Control", "private, no-store")
                        .header("X-Content-Type-Options", "nosniff")
                        .body(Body::from(bytes))
                        .unwrap_or_else(|_| {
                            build_response(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                b"Internal Server Error".to_vec(),
                                "text/plain",
                                false,
                            )
                        });
                }
                // A withheld file is not a key failure and must not read as
                // a paywall teaser: the entitled visitor is told the content is
                // gone for legal reasons, exactly as an unentitled one would be
                // at the plaintext arm above.
                Err(super::file_bytes::FileOpenError::Withheld) => return withheld_response(),
                Err(e) => {
                    // Held a key but could not open — a revoked-and-rotated
                    // generation, a missing chunk, a corrupt manifest. Still
                    // never ciphertext: fall through to the teaser.
                    tracing::warn!(error = %e, path = %row.path, "sealed web file failed to open");
                }
            }
        }
    }

    // No/invalid token, no live grant, or an open failure → the teaser.
    folder_teaser_response(db, actor_id, &tier, &row.path).await
}

/// The paywall teaser for a sealed file: tier + price + payment link, rendered
/// at serve time from the set's tier (`monetization.md` § Pillar 2). **402
/// Payment Required** — the resource exists but is not free; the status is the
/// machine-readable half of the same statement the body makes, and keeps a
/// downloader from mistaking the teaser HTML for the asset it asked for.
async fn folder_teaser_response(
    db: &CacheDb,
    actor_id: &[u8; 32],
    tier: &str,
    path: &str,
) -> Response<Body> {
    let tier_row = db
        .get_subscription_tier(actor_id, tier)
        .await
        .ok()
        .flatten();
    let paywall = super::render::PaywallContext {
        tier: tier.to_string(),
        price_hint: tier_row.as_ref().and_then(|t| t.price_hint.clone()),
        payment_url: tier_row.as_ref().and_then(|t| t.payment_url.clone()),
    };
    let site = super::render::build_site_context(None, "", "");
    let body = super::render::render_folder_paywall(&site, &paywall, path).unwrap_or_else(|_| {
        "<!DOCTYPE html><html><body><h1>Subscribers only</h1></body></html>".into()
    });
    Response::builder()
        .status(StatusCode::PAYMENT_REQUIRED)
        .header("Content-Type", "text/html; charset=utf-8")
        .header("Cache-Control", "private, no-store")
        .header("X-Content-Type-Options", "nosniff")
        .body(Body::from(body))
        .unwrap_or_else(|_| {
            build_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                b"Internal Server Error".to_vec(),
                "text/plain",
                false,
            )
        })
}

/// Interpret a stored 32-byte hash column.
fn as_hash32(bytes: &[u8]) -> Option<[u8; 32]> {
    bytes.try_into().ok()
}

/// The paywall serve leg: verify the token statelessly, look up the sealed
/// row (exact path, plus the `.html` twin for an extensionless page URL),
/// unseal under the holder's live grant, serve. `None` on any miss/failure —
/// the caller falls through to public resolution (the teaser).
async fn try_serve_sealed(
    db: &CacheDb,
    blob_store: &Arc<dyn BlobStoreBackend>,
    actor_id: &[u8; 32],
    request_path: &str,
    holder: &super::holder::WebServeHolder,
    token: &str,
) -> Option<Response<Body>> {
    let normalized = request_path.trim_start_matches('/');
    // The sealed row is keyed at the rendered path (`post/{slug}.html`); a
    // visitor may carry the extensionless URL form.
    let mut candidates = vec![normalized.to_string()];
    if !normalized.is_empty() && !normalized.ends_with('/') && !has_extension(normalized) {
        candidates.push(format!("{normalized}.html"));
    }
    let mut sealed_row = None;
    for candidate in &candidates {
        if let Ok(Some(row)) = db.get_web_rendered_sealed(actor_id, candidate).await {
            sealed_row = Some(row);
            break;
        }
    }
    let row = sealed_row?;

    // Stateless signature + scope + expiry check against OUR holder identity.
    // The token is scoped to the sealed row's stored path.
    super::token::WebPaywallToken::verify(token, &holder.ed25519_pubkey(), actor_id, &row.path)?;

    // Unseal at serve time under the live grant — a revoked grant has no
    // period key here, so the sealed slice is dark even with a valid token.
    let post_id: [u8; 32] = row.post_id.as_slice().try_into().ok()?;
    let grant_set = holder.registry.current();
    let period_key: [u8; 32] = grant_set
        .key_for(
            actor_id,
            "content.read",
            "post",
            Some(&row.tier),
            crate::db::now_epoch_secs() as u64,
        )?
        .try_into()
        .ok()?;
    let render_key =
        zeroize::Zeroizing::new(fauna_core::subscription::crypto::derive_web_render_key(
            &period_key,
            &fauna_core::data::ContentHash::from_digest_raw(post_id),
        ));
    let hash: [u8; 32] = row.blob_hash.as_slice().try_into().ok()?;
    let sealed_bytes = blob_store
        .get(&ContentHash::from_digest_raw(hash))
        .await
        .ok()??;
    let plaintext =
        fauna_core::subscription::crypto::decrypt_content(&render_key, &sealed_bytes).ok()?;

    // Entitled content is visitor-specific: never cacheable by intermediaries
    // or shared caches (the token in the URL must not become ambient via a
    // cache). No Set-Cookie, ever.
    let resp = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", row.content_type.as_str())
        .header("Cache-Control", "private, no-store")
        .header("X-Content-Type-Options", "nosniff")
        .body(Body::from(plaintext))
        .ok()?;
    Some(resp)
}

/// Try to find a 404.html for this actor (user files first, then rendered).
/// The source comes back with the row: the two tables store their bytes
/// differently (manifest vs verbatim blob).
/// The folder gate a `web_files` row owes **every** anonymous door, in one
/// place. Returns the resolved folder when the row may serve; `None` means
/// "404, no bytes".
///
/// Two gates, both read from the folder's CURRENT row on every request:
///
/// 1. **The phase-4 website toggle** — a site its owner switched off must
///    actually stop. The `web_files` rows persist across the flip (a re-enable
///    serves again with no re-ingest), so the row's existence is not consent.
///    A row with no resolvable folder (a deleted set) refuses too: nothing
///    legitimate rests in that shape.
/// 2. **The plaintext class**, for a row with no
///    `content_key_version` — audience `public` (phase 4), the
///    same predicate the S9 path-seal exemptions use. A flip back to private
///    must revoke on the very NEXT request (the semantics `folder_public.rs`
///    already gives the authenticated door); seal-state cannot carry that,
///    because the flip-back leaves the rows plaintext until a *client-driven*
///    re-seal that a flip-unaware engine, a keyless engine, or a cloud-only
///    placeholder never runs. (The legacy `mode == "web"` half of the class
///    retired 2026-09-28 with the mode contraction.)
///
/// ⚠ **This is a CORE, not a door — every path that turns a `web_files` row
/// into bytes must call it.** There are three consumer classes:
/// [`serve_user_file`], the custom-`404.html` arm of
/// [`serve_web_content_with_token`], and the render pipeline's input reads
/// (`service.rs` — the `.html.hbs` listing, `_site.json`, and each template
/// load). The 404 arm resolves its row with a bare `get_web_file` and read
/// the manifest itself, so it bypassed BOTH gates entirely — serving a
/// private folder's page, and a switched-off site's page, to any anonymous
/// visitor — until 2026-08-20 routed it through here. The render inputs were
/// folder-blind (`WHERE actor_id` only) until the same pass, so a disabled or
/// re-classified folder's templates kept feeding every later render.
pub(super) async fn gate_web_file_folder(
    db: &CacheDb,
    row: &crate::db::web::WebFileRow,
) -> Option<crate::db::FolderRow> {
    let folder_id = row.folder_id?;
    let folder = db.get_folder_by_id(folder_id).await.ok().flatten()?;
    // A sealed row needs only the toggle — it takes the paywall token gate from
    // there, and is never a render input. A plaintext row needs the whole
    // predicate, which is the SAME one `update_folder_for_user` marks a revoke
    // on (`FolderRow::serves_plaintext_web_files`): spelled out twice, the two
    // drifted and a folder could stop serving with nothing owed.
    let serves = if row.content_key_version.is_some() {
        folder.website_enabled
    } else {
        folder.serves_plaintext_web_files()
    };
    if !serves {
        tracing::debug!(
            path = %row.path,
            website_enabled = folder.website_enabled,
            sealed = row.content_key_version.is_some(),
            "web file's folder does not serve it — refusing"
        );
        return None;
    }
    Some(folder)
}

async fn try_resolve_404(
    db: &CacheDb,
    actor_id: &[u8; 32],
) -> Option<(crate::db::web::WebFileRow, PathSource)> {
    if let Ok(Some(row)) = db.get_web_file(actor_id, "404.html").await {
        return Some((row, PathSource::UserFile));
    }
    if let Ok(Some(row)) = db.get_web_rendered(actor_id, "404.html").await {
        return Some((row, PathSource::Rendered));
    }
    None
}

/// Fetch blob bytes for a hash stored as raw bytes. Returns empty vec on error.
async fn fetch_blob_bytes(blob_store: &Arc<dyn BlobStoreBackend>, hash_bytes: &[u8]) -> Vec<u8> {
    if hash_bytes.len() != 32 {
        return Vec::new();
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(hash_bytes);
    let hash = ContentHash::from_digest_raw(arr);
    match blob_store.get(&hash).await {
        Ok(Some(data)) => data,
        _ => Vec::new(),
    }
}

/// Fetch blob and build a 200 response, or a 500 on fetch failure.
async fn serve_blob(
    blob_store: &Arc<dyn BlobStoreBackend>,
    hash_bytes: &[u8],
    content_type: &str,
) -> Response<Body> {
    if hash_bytes.len() != 32 {
        return build_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            b"Invalid hash".to_vec(),
            "text/plain",
            false,
        );
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(hash_bytes);
    let hash = ContentHash::from_digest_raw(arr);
    match blob_store.get(&hash).await {
        Ok(Some(data)) => build_response(StatusCode::OK, data, content_type, is_html(content_type)),
        Ok(None) => build_response(
            StatusCode::NOT_FOUND,
            b"Blob not found".to_vec(),
            "text/plain",
            false,
        ),
        Err(_) => build_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            b"Storage error".to_vec(),
            "text/plain",
            false,
        ),
    }
}

/// Returns true if the content_type indicates HTML.
fn is_html(content_type: &str) -> bool {
    content_type.starts_with("text/html")
}

/// Build a `Response<Body>` with the appropriate headers.
///
/// `X-Content-Type-Options: nosniff` is always set: served content sits
/// on the actor's **own isolated origin** (it cannot reach the SPA or another
/// user — § Same-origin security model), but `nosniff` still stops a browser
/// content-sniffing a mislabeled asset into active content, matching the `/app`
/// posture. We deliberately do **not** add `Content-Disposition` (the content is
/// a website meant to render inline) nor sanitize the actor's own HTML/markdown —
/// raw HTML on one's own origin is the whole point of the per-origin model.
fn build_response(
    status: StatusCode,
    body: Vec<u8>,
    content_type: &str,
    use_short_cache: bool,
) -> Response<Body> {
    let cache_control = if use_short_cache {
        "public, max-age=300"
    } else {
        "public, max-age=86400"
    };

    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, cache_control)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(body))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap()
        })
}

/// Convenience wrapper for not-found text responses.
fn not_found_response(body: Vec<u8>, content_type: &str) -> Response<Body> {
    build_response(StatusCode::NOT_FOUND, body, content_type, false)
}

/// The compelled-removal refusal: **451, no body, never cached**
/// (`moderation.md` § Legal takedown → *The blob-serve door*).
///
/// Not [`build_response`], deliberately: that stamps a day of `max-age`, and a
/// takedown is reversible — `restore=true` re-serves the very same bytes — so
/// a cached refusal would outlive the order it carried out.
fn withheld_response() -> Response<Body> {
    Response::builder()
        .status(StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::empty())
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap()
        })
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use fauna_core::data::ContentHash;

    use super::*;
    use crate::blob_store::BlobStoreBackend;
    use crate::db::CacheDb;

    // ---- In-memory blob store for tests ----

    struct MemBlobStore {
        data: std::sync::Mutex<HashMap<[u8; 32], Vec<u8>>>,
    }

    impl MemBlobStore {
        fn new() -> Self {
            Self {
                data: std::sync::Mutex::new(HashMap::new()),
            }
        }
    }

    #[async_trait]
    impl BlobStoreBackend for MemBlobStore {
        async fn put(&self, hash: &ContentHash, data: &[u8]) -> anyhow::Result<()> {
            self.data
                .lock()
                .unwrap()
                .insert(hash.digest(), data.to_vec());
            Ok(())
        }

        async fn get(&self, hash: &ContentHash) -> anyhow::Result<Option<Vec<u8>>> {
            Ok(self.data.lock().unwrap().get(&hash.digest()).cloned())
        }

        async fn exists(&self, hash: &ContentHash) -> anyhow::Result<bool> {
            Ok(self.data.lock().unwrap().contains_key(&hash.digest()))
        }

        async fn exists_batch(&self, hashes: &[ContentHash]) -> anyhow::Result<Vec<bool>> {
            let data = self.data.lock().unwrap();
            Ok(hashes
                .iter()
                .map(|h| data.contains_key(&h.digest()))
                .collect())
        }

        async fn delete(&self, hash: &ContentHash) -> anyhow::Result<()> {
            self.data.lock().unwrap().remove(&hash.digest());
            Ok(())
        }

        async fn usage_bytes(&self) -> anyhow::Result<u64> {
            Ok(self
                .data
                .lock()
                .unwrap()
                .values()
                .map(|v| v.len() as u64)
                .sum())
        }
    }

    // ---- Test helpers ----

    fn actor() -> [u8; 32] {
        [1u8; 32]
    }

    fn make_hash(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    fn db() -> CacheDb {
        CacheDb::open_in_memory().unwrap()
    }

    /// Seed a rendered row the way a render writes one — under a claim of its
    /// own ([`CacheDb::begin_web_render`]). These tests are about what the
    /// serve door resolves, not about which render won, so each seed is simply
    /// the newest render.
    async fn seed_rendered(
        db: &CacheDb,
        actor: &[u8; 32],
        path: &str,
        blob_hash: &[u8; 32],
        content_type: &str,
    ) {
        let claim = db.begin_web_render(actor).await.unwrap();
        assert!(
            db.upsert_web_rendered(&claim, path, blob_hash, content_type)
                .await
                .unwrap(),
            "a freshly opened claim is the current one"
        );
    }

    /// [`seed_rendered`]'s sealed twin.
    async fn seed_rendered_sealed(
        db: &CacheDb,
        actor: &[u8; 32],
        path: &str,
        blob_hash: &[u8; 32],
        content_type: &str,
        tier: &str,
        post_id: &[u8; 32],
    ) {
        let claim = db.begin_web_render(actor).await.unwrap();
        assert!(
            db.upsert_web_rendered_sealed(&claim, path, blob_hash, content_type, tier, post_id)
                .await
                .unwrap(),
            "a freshly opened claim is the current one"
        );
    }

    /// A website-enabled folder for `actor` — the phase-4 serve gate resolves
    /// `web_files.folder_id` and refuses a toggle-off (or folder-less) row, so
    /// every serving test seeds a real toggled-on folder and keys its rows to
    /// it.
    async fn website_folder(db: &CacheDb, actor: &[u8; 32], name: &str) -> i64 {
        let id = db
            .create_folder_with_options(name, actor, Default::default())
            .await
            .unwrap();
        db.update_folder_for_user(
            name,
            actor,
            crate::db::FolderUpdate {
                website_enabled: Some(true),
                // The audience is part of the SERVING shape, not decoration:
                // non-paywalled serving requires `public`
                // (`web-content-hosting.md` § Static site files), and since
                // the fix the plaintext arm reads it at the door on every
                // request. A toggled-on folder left at the default audience is
                // a site that legitimately serves NOTHING, so a helper that
                // omitted this was seeding a shape no real website has.
                audience: Some(Some("public")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        id
    }

    // ---- Tests ----

    #[tokio::test]
    async fn resolve_exact_user_file() {
        let db = db();
        let actor = actor();
        let hash = make_hash(10);

        db.upsert_web_file(&actor, "index.html", &hash, "text/html", None, None)
            .await
            .unwrap();

        let result = resolve_path(&db, &actor, "/index.html").await.unwrap();
        let (row, source) = result.expect("should resolve");
        let (got_hash, got_ct) = (row.blob_hash, row.content_type);
        assert_eq!(got_hash, hash.as_slice());
        assert_eq!(got_ct, "text/html");
        assert_eq!(source, PathSource::UserFile);
    }

    #[tokio::test]
    async fn resolve_rendered_file() {
        let db = db();
        let actor = actor();
        let hash = make_hash(20);

        seed_rendered(&db, &actor, "posts/hello.html", &hash, "text/html").await;

        let result = resolve_path(&db, &actor, "/posts/hello.html")
            .await
            .unwrap();
        let (row, source) = result.expect("should resolve");
        let (got_hash, got_ct) = (row.blob_hash, row.content_type);
        assert_eq!(got_hash, hash.as_slice());
        assert_eq!(got_ct, "text/html");
        assert_eq!(source, PathSource::Rendered);
    }

    #[tokio::test]
    async fn user_file_takes_precedence() {
        let db = db();
        let actor = actor();
        let user_hash = make_hash(30);
        let rendered_hash = make_hash(31);

        // Both tables have the same path
        db.upsert_web_file(&actor, "page.html", &user_hash, "text/html", None, None)
            .await
            .unwrap();
        seed_rendered(&db, &actor, "page.html", &rendered_hash, "text/html").await;

        let result = resolve_path(&db, &actor, "/page.html").await.unwrap();
        let (row, source) = result.expect("should resolve");
        let got_hash = row.blob_hash;
        // User file wins
        assert_eq!(got_hash, user_hash.as_slice());
        assert_eq!(source, PathSource::UserFile);
    }

    #[tokio::test]
    async fn resolve_directory_index() {
        let db = db();
        let actor = actor();
        let hash = make_hash(40);

        db.upsert_web_file(&actor, "blog/index.html", &hash, "text/html", None, None)
            .await
            .unwrap();

        // Path ending with `/` should resolve to index.html
        let result = resolve_path(&db, &actor, "/blog/").await.unwrap();
        let (row, source) = result.expect("should resolve /blog/");
        let got_hash = row.blob_hash;
        assert_eq!(got_hash, hash.as_slice());
        assert_eq!(source, PathSource::UserFile);

        // Path without trailing slash but no extension also resolves
        let result2 = resolve_path(&db, &actor, "/blog").await.unwrap();
        let got_hash2 = result2.expect("should resolve /blog").0.blob_hash;
        assert_eq!(got_hash2, hash.as_slice());

        // Root path resolves to index.html
        let hash_root = make_hash(41);
        db.upsert_web_file(&actor, "index.html", &hash_root, "text/html", None, None)
            .await
            .unwrap();
        let result3 = resolve_path(&db, &actor, "/").await.unwrap();
        let got_hash3 = result3.expect("should resolve /").0.blob_hash;
        assert_eq!(got_hash3, hash_root.as_slice());
    }

    #[tokio::test]
    async fn resolve_404_not_found() {
        let db = db();
        let actor = actor();

        let result = resolve_path(&db, &actor, "/does-not-exist.html")
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn rejected_extensions() {
        assert!(is_rejected_extension("script.php"));
        assert!(is_rejected_extension("app.py"));
        assert!(is_rejected_extension("server.rb"));
        assert!(is_rejected_extension("handler.cgi"));
        assert!(is_rejected_extension("page.asp"));
        assert!(is_rejected_extension("page.aspx"));
        assert!(is_rejected_extension("Servlet.jsp"));
        assert!(is_rejected_extension("prog.pl"));
        assert!(is_rejected_extension("run.sh"));
        // Case-insensitive
        assert!(is_rejected_extension("Script.PHP"));
        assert!(is_rejected_extension("App.PY"));
        // Non-rejected
        assert!(!is_rejected_extension("index.html"));
        assert!(!is_rejected_extension("style.css"));
        assert!(!is_rejected_extension("main.js"));
        assert!(!is_rejected_extension("image.png"));
    }

    #[tokio::test]
    async fn host_resolver_subdomain() {
        let resolver = HostResolver::new("nest.fauna.social".to_string());
        let actor = actor();

        resolver.register_subdomain("alice", actor).await;

        // Full host with subdomain
        let resolved = resolver.resolve("alice.nest.fauna.social").await;
        assert_eq!(resolved, Some(actor));

        // With port
        let resolved_port = resolver.resolve("alice.nest.fauna.social:443").await;
        assert_eq!(resolved_port, Some(actor));

        // Unknown subdomain
        assert!(resolver.resolve("bob.nest.fauna.social").await.is_none());

        // Exact nest domain (not a subdomain)
        assert!(resolver.resolve("nest.fauna.social").await.is_none());
    }

    #[tokio::test]
    async fn host_resolver_remove_subdomain() {
        let resolver = HostResolver::new("nest.fauna.social".to_string());
        let actor = actor();

        resolver.register_subdomain("alice", actor).await;
        assert_eq!(
            resolver.resolve("alice.nest.fauna.social").await,
            Some(actor)
        );

        // Opting out drops the mapping → no longer resolves (no apex → info page).
        resolver.remove_subdomain("alice").await;
        assert!(resolver.resolve("alice.nest.fauna.social").await.is_none());

        // Removing an absent subdomain is a harmless no-op.
        resolver.remove_subdomain("nobody").await;
    }

    #[test]
    fn reserved_subdomain_labels() {
        for reserved in ["mail", "MTA-STS", "App", "www"] {
            assert!(
                is_reserved_subdomain_label(reserved),
                "{reserved} must be reserved (case-insensitive)"
            );
        }
        for ok in ["alice", "bob", "mailbox", "apps"] {
            assert!(!is_reserved_subdomain_label(ok), "{ok} must be allowed");
        }
    }

    #[tokio::test]
    async fn host_resolver_apex_catch_all() {
        let resolver = HostResolver::new("example.com".to_string());
        let apex = make_hash(99);

        // No apex designated → node domain + bare + unknown all serve the info
        // page (resolve → None).
        assert!(resolver.resolve("example.com").await.is_none());
        assert!(resolver.resolve("").await.is_none());
        assert!(resolver.resolve("random.example.org").await.is_none());

        // Designate the apex actor.
        resolver.set_apex_actor(Some(apex)).await;
        // The node domain itself resolves to the apex actor.
        assert_eq!(resolver.resolve("example.com").await, Some(apex));
        // With a port.
        assert_eq!(resolver.resolve("example.com:443").await, Some(apex));
        // Bare/absent host → apex catch-all.
        assert_eq!(resolver.resolve("").await, Some(apex));
        // Any other unmatched (non-reserved) host → apex catch-all.
        assert_eq!(resolver.resolve("203.0.113.7").await, Some(apex));
        assert_eq!(resolver.resolve("bob.example.com").await, Some(apex));

        // A registered subdomain still wins over the apex catch-all.
        let alice = make_hash(1);
        resolver.register_subdomain("alice", alice).await;
        assert_eq!(resolver.resolve("alice.example.com").await, Some(alice));

        // Clearing reverts everything to the info page.
        resolver.set_apex_actor(None).await;
        assert!(resolver.resolve("example.com").await.is_none());
        // …but the explicit subdomain mapping is untouched.
        assert_eq!(resolver.resolve("alice.example.com").await, Some(alice));
    }

    #[tokio::test]
    async fn host_resolver_nest_domain_updates_live() {
        // A domain claimed *post-boot* must drive subdomain routing without a
        // restart: `identity_domain_core::apply_primary_identity` calls
        // `set_nest_domain`, and `resolve` must follow the swap live
        // (`domains-and-tls-bootstrap.md` § Implementation status — web apex live).
        let resolver = HostResolver::new("old.test".to_string());
        let bob = make_hash(7);
        resolver.register_subdomain("bob", bob).await;

        // At boot the resolver strips only the boot domain's suffix.
        assert_eq!(resolver.resolve("bob.old.test").await, Some(bob));
        // A subdomain under a not-yet-claimed domain doesn't route.
        assert!(resolver.resolve("bob.new.test").await.is_none());

        // Claim a domain post-boot → swap the apex domain live (mixed case
        // exercises the same lowercasing the constructor applies).
        resolver.set_nest_domain("New.Test");

        // Subdomain routing now follows the claimed domain — no reconstruction.
        assert_eq!(resolver.resolve("bob.new.test").await, Some(bob));
        // The reserved-host guard is now evaluated against the new domain.
        assert!(resolver.resolve("mail.new.test").await.is_none());
        // The stale boot-domain suffix no longer matches (→ apex info page).
        assert!(resolver.resolve("bob.old.test").await.is_none());
    }

    #[tokio::test]
    async fn host_resolver_reserved_hosts_never_resolve() {
        let resolver = HostResolver::new("example.com".to_string());
        // Even with an apex actor designated, reserved hosts never serve user
        // content (they have their own routes/SNI above the fallback).
        resolver.set_apex_actor(Some(make_hash(99))).await;
        for reserved in [
            "mail.example.com",
            "mta-sts.example.com",
            "app.example.com",
            "_acme-challenge.example.com",
            "_acme-challenge.alice.com",
        ] {
            assert!(
                resolver.resolve(reserved).await.is_none(),
                "{reserved} must never resolve to user content"
            );
        }
    }

    #[tokio::test]
    async fn host_resolver_reserved_hosts_case_and_dot_insensitive() {
        // A mixed-case or trailing-dot Host must not bypass the
        // reserved-host exclusion into the apex catch-all.
        let resolver = HostResolver::new("example.com".to_string());
        resolver.set_apex_actor(Some(make_hash(99))).await;
        for reserved in [
            "App.example.com",
            "MAIL.example.com",
            "app.example.com.",
            "mta-sts.example.com",
        ] {
            assert!(
                resolver.resolve(reserved).await.is_none(),
                "{reserved} must be treated as reserved regardless of case/trailing dot"
            );
        }
    }

    #[tokio::test]
    async fn host_resolver_normalizes_subdomain_host() {
        // Subdomain matching is case-/dot-insensitive too.
        let resolver = HostResolver::new("example.com".to_string());
        let alice = make_hash(1);
        resolver.register_subdomain("alice", alice).await;
        assert_eq!(resolver.resolve("Alice.example.com").await, Some(alice));
        assert_eq!(resolver.resolve("alice.example.com.").await, Some(alice));
        assert_eq!(resolver.resolve("ALICE.example.com:443").await, Some(alice));
    }

    #[tokio::test]
    async fn serve_web_content_sets_nosniff() {
        // Every served web-content response carries nosniff.
        let db = db();
        let actor = actor();
        let content = b"<html><body>Hi</body></html>";
        let blob_store: Arc<dyn BlobStoreBackend> = Arc::new(MemBlobStore::new());
        let hash =
            super::super::file_bytes::seed_synced_file(&blob_store, None, content, None).await;
        let site = website_folder(&db, &actor, "site").await;
        db.upsert_web_file(&actor, "index.html", &hash, "text/html", Some(site), None)
            .await
            .unwrap();

        let resp = serve_web_content(&db, &blob_store, None, &actor, "/index.html").await;
        assert_eq!(
            resp.headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|v| v.to_str().ok()),
            Some("nosniff"),
            "served web content must set X-Content-Type-Options: nosniff"
        );

        // The built-in 404 path too.
        let resp404 = serve_web_content(&db, &blob_store, None, &actor, "/missing.html").await;
        assert_eq!(resp404.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            resp404
                .headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|v| v.to_str().ok()),
            Some("nosniff"),
        );
    }

    #[tokio::test]
    async fn host_resolver_custom_domain() {
        let resolver = HostResolver::new("nest.fauna.social".to_string());
        let actor = actor();

        resolver
            .reconcile_custom_domains(HashMap::from([("alice.example.com".to_string(), actor)]))
            .await;

        let resolved = resolver.resolve("alice.example.com").await;
        assert_eq!(resolved, Some(actor));

        // With port
        let resolved_port = resolver.resolve("alice.example.com:80").await;
        assert_eq!(resolved_port, Some(actor));

        // Remove and verify gone
        resolver.remove_custom_domain("alice.example.com").await;
        assert!(resolver.resolve("alice.example.com").await.is_none());
    }

    /// A synced web file serves its **content** — not the framed CBOR manifest
    /// its `web_files.blob_hash` actually addresses. Asserting the body (not
    /// just the status/headers) is the check that was missing: every earlier
    /// test seeded a raw blob, so the manifest indirection was never exercised
    /// and `GET /index.html` shipped a `0x00`-prefixed CBOR blob as `200 OK`.
    #[tokio::test]
    async fn serve_web_content_returns_file() {
        let db = db();
        let actor = actor();
        let content = b"<html><body>Hello</body></html>";

        let blob_store: Arc<dyn BlobStoreBackend> = Arc::new(MemBlobStore::new());
        let hash =
            super::super::file_bytes::seed_synced_file(&blob_store, None, content, None).await;
        let site = website_folder(&db, &actor, "site").await;
        db.upsert_web_file(&actor, "index.html", &hash, "text/html", Some(site), None)
            .await
            .unwrap();

        let resp = serve_web_content(&db, &blob_store, None, &actor, "/index.html").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(ct, "text/html");

        let cc = resp
            .headers()
            .get(header::CACHE_CONTROL)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(cc, "public, max-age=300");

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            body.as_ref(),
            content,
            "the body must be the file, not its manifest"
        );
    }

    /// The rule: the custom-`404.html` door asks the folder gate too.
    ///
    /// `try_resolve_404` resolves its row by path alone (a bare `get_web_file`)
    /// and the caller read the manifest itself, never routing through
    /// `serve_user_file` — which is where BOTH the phase-4 website toggle and
    /// the audience gate live. So the custom 404 page kept serving its
    /// plaintext bytes to anonymous visitors after its folder was switched off,
    /// and after it was flipped back to private: the revoke landed on
    /// one of the two doors that serve a `web_files` row.
    ///
    /// Both halves are pinned here because they fail independently.
    #[tokio::test]
    async fn the_404_door_asks_the_folder_gate_too() {
        let db = db();
        let actor = actor();
        let content = b"<html><body>PROBE-391-404-MARKER</body></html>";
        let blob_store: Arc<dyn BlobStoreBackend> = Arc::new(MemBlobStore::new());
        let hash =
            super::super::file_bytes::seed_synced_file(&blob_store, None, content, None).await;
        let site = website_folder(&db, &actor, "site").await;
        db.update_folder_for_user(
            "site",
            &actor,
            crate::db::FolderUpdate {
                audience: Some(Some("public")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        db.upsert_web_file(&actor, "404.html", &hash, "text/html", Some(site), None)
            .await
            .unwrap();

        // Baseline: while public + toggled on, the custom 404 page serves.
        let resp = serve_web_content(&db, &blob_store, None, &actor, "/missing.html").await;
        let b = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&b).into_owned();
        assert!(
            text.contains("PROBE-391-404-MARKER"),
            "baseline: the custom 404 must serve while public: {text}"
        );

        // The revoke: flip the audience back to private, nest-side only.
        db.update_folder_for_user(
            "site",
            &actor,
            crate::db::FolderUpdate {
                audience: Some(Some("private")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let resp = serve_web_content(&db, &blob_store, None, &actor, "/missing.html").await;
        let b = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&b).into_owned();
        assert!(
            !text.contains("PROBE-391-404-MARKER"),
            "AUDIENCE REVOKE BYPASSED on the 404 door: {text}"
        );

        // And the phase-4 website toggle, the older gate.
        db.update_folder_for_user(
            "site",
            &actor,
            crate::db::FolderUpdate {
                audience: Some(Some("public")),
                website_enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let resp = serve_web_content(&db, &blob_store, None, &actor, "/missing.html").await;
        let b = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&b).into_owned();
        assert!(
            !text.contains("PROBE-391-404-MARKER"),
            "WEBSITE TOGGLE BYPASSED on the 404 door: {text}"
        );
    }

    /// Phase 4: the website toggle gates SERVING, not just ingest — a site the
    /// owner switched off stops (404), the rows survive the flip, and turning
    /// it back on serves again with no re-ingest. A row keyed to no resolvable
    /// folder refuses too.
    #[tokio::test]
    async fn website_toggle_off_stops_serving_and_on_resumes() {
        let db = db();
        let actor = actor();
        let content = b"<html><body>toggle-gated</body></html>";
        let blob_store: Arc<dyn BlobStoreBackend> = Arc::new(MemBlobStore::new());
        let hash =
            super::super::file_bytes::seed_synced_file(&blob_store, None, content, None).await;
        let site = website_folder(&db, &actor, "site").await;
        db.upsert_web_file(&actor, "index.html", &hash, "text/html", Some(site), None)
            .await
            .unwrap();

        // On → serves.
        let resp = serve_web_content(&db, &blob_store, None, &actor, "/index.html").await;
        assert_eq!(resp.status(), StatusCode::OK);

        // Off → 404, no bytes.
        db.update_folder_for_user(
            "site",
            &actor,
            crate::db::FolderUpdate {
                website_enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let resp = serve_web_content(&db, &blob_store, None, &actor, "/index.html").await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "a toggle-off folder's files must stop serving"
        );

        // Back on → serves again (rows persisted across the flip).
        db.update_folder_for_user(
            "site",
            &actor,
            crate::db::FolderUpdate {
                website_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let resp = serve_web_content(&db, &blob_store, None, &actor, "/index.html").await;
        assert_eq!(resp.status(), StatusCode::OK);

        // A row keyed to a folder id that resolves nothing refuses as well.
        db.upsert_web_file(&actor, "orphan.html", &hash, "text/html", Some(9999), None)
            .await
            .unwrap();
        let resp = serve_web_content(&db, &blob_store, None, &actor, "/orphan.html").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn serve_web_content_asset_cache_control() {
        let db = db();
        let actor = actor();
        let content = b"body { color: red; }";

        let blob_store: Arc<dyn BlobStoreBackend> = Arc::new(MemBlobStore::new());
        let hash =
            super::super::file_bytes::seed_synced_file(&blob_store, None, content, None).await;
        let site = website_folder(&db, &actor, "site").await;
        db.upsert_web_file(&actor, "style.css", &hash, "text/css", Some(site), None)
            .await
            .unwrap();

        let resp = serve_web_content(&db, &blob_store, None, &actor, "/style.css").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let cc = resp
            .headers()
            .get(header::CACHE_CONTROL)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(cc, "public, max-age=86400");
    }

    #[tokio::test]
    async fn serve_web_content_builtin_404() {
        let db = db();
        let actor = actor();
        let blob_store: Arc<dyn BlobStoreBackend> = Arc::new(MemBlobStore::new());

        let resp = serve_web_content(&db, &blob_store, None, &actor, "/missing.html").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn resolve_extensionless_page_url_finds_rendered_html_twin() {
        // The built-in index links `/post/{slug}` while per-post pages render
        // at `post/{slug}.html` — the extensionless URL must resolve.
        let db = db();
        let actor = actor();
        let hash = make_hash(60);
        seed_rendered(&db, &actor, "post/my-slug.html", &hash, "text/html").await;
        let resolved = resolve_path(&db, &actor, "/post/my-slug").await.unwrap();
        let (row, source) = resolved.expect("extensionless URL resolves to .html twin");
        let got_hash = row.blob_hash;
        assert_eq!(got_hash, hash.to_vec());
        assert!(matches!(source, PathSource::Rendered));
    }

    /// The full paywall serve leg: a valid capability token unseals + serves
    /// the sealed page (private, no-store, no Set-Cookie); every failure arm
    /// falls through to the public (teaser) resolution.
    #[tokio::test]
    async fn paywall_token_serves_sealed_page_and_all_failure_arms_fall_through() {
        use fauna_core::subscription::crypto::{derive_web_render_key, encrypt_content};
        use fauna_mls::wrapped_blob::{GrantWindow, ScopeTuple, build_grant_blob};

        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let actor = actor();
        let dir = tempfile::tempdir().unwrap();
        let holder = super::super::holder::WebServeHolder::init(dir.path(), db.clone())
            .await
            .unwrap()
            .expect("holder inits");
        let blob_store: Arc<dyn BlobStoreBackend> = Arc::new(MemBlobStore::new());

        // Public teaser page at the canonical path.
        let teaser_bytes = b"<html>teaser + paywall box</html>";
        let teaser_hash = *blake3::hash(teaser_bytes).as_bytes();
        blob_store
            .put(&ContentHash::from_digest_raw(teaser_hash), teaser_bytes)
            .await
            .unwrap();
        seed_rendered(&db, &actor, "post/premium.html", &teaser_hash, "text/html").await;

        // Sealed full page under the grant-derived key.
        let period_key = [42u8; 32];
        let post_id = [5u8; 32];
        let render_key = derive_web_render_key(&period_key, &ContentHash::from_digest_raw(post_id));
        let full_html = b"<html>the FULL premium content</html>";
        let sealed = encrypt_content(&render_key, full_html);
        let sealed_hash = *blake3::hash(&sealed).as_bytes();
        blob_store
            .put(&ContentHash::from_digest_raw(sealed_hash), &sealed)
            .await
            .unwrap();
        seed_rendered_sealed(
            &db,
            &actor,
            "post/premium.html",
            &sealed_hash,
            "text/html",
            "gold",
            &post_id,
        )
        .await;

        // Grant the holder the gold period key.
        let grant = build_grant_blob(
            &actor,
            &[7u8; 16],
            &holder.x25519_pubkey,
            Some(&holder.mlkem_ek),
            GrantWindow(0, u64::MAX),
            &[(
                ScopeTuple {
                    class: ScopeTuple::CLASS_CONTENT_READ.to_string(),
                    kind: Some(ScopeTuple::KIND_POST.to_string()),
                    tier: Some("gold".to_string()),
                    set: None,
                    factor: None,
                },
                Some(period_key.to_vec()),
            )],
        )
        .unwrap()
        .to_canonical_bytes()
        .unwrap();
        db.put_capability_grant(&actor, &[7u8; 16], &holder.x25519_pubkey, i64::MAX, &grant)
            .await
            .unwrap();
        holder.registry.refresh().await.unwrap();

        let (token, _) = super::super::token::WebPaywallToken::mint(
            &holder.actor_keypair(),
            actor,
            "post/premium.html",
        )
        .unwrap();

        async fn body_of(resp: Response<Body>) -> String {
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            String::from_utf8_lossy(&bytes).into_owned()
        }

        // Valid token → the unsealed FULL page, private/no-store, no cookie.
        let resp = serve_web_content_with_token(
            &db,
            &blob_store,
            None,
            &actor,
            "/post/premium.html",
            Some(&holder),
            Some(&token),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get("set-cookie").is_none(), "never a cookie");
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "private, no-store"
        );
        assert!(body_of(resp).await.contains("FULL premium content"));

        // The extensionless URL form works too (the token is path-scoped to
        // the sealed row's stored path).
        let resp = serve_web_content_with_token(
            &db,
            &blob_store,
            None,
            &actor,
            "/post/premium",
            Some(&holder),
            Some(&token),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_of(resp).await.contains("FULL premium content"));

        // No token → teaser.
        let resp = serve_web_content_with_token(
            &db,
            &blob_store,
            None,
            &actor,
            "/post/premium.html",
            Some(&holder),
            None,
        )
        .await;
        assert!(body_of(resp).await.contains("teaser"));

        // Forged token (someone else's keypair) → teaser.
        let attacker = fauna_core::identity::ActorKeypair::from_secret([13u8; 32]);
        let (forged, _) =
            super::super::token::WebPaywallToken::mint(&attacker, actor, "post/premium.html")
                .unwrap();
        let resp = serve_web_content_with_token(
            &db,
            &blob_store,
            None,
            &actor,
            "/post/premium.html",
            Some(&holder),
            Some(&forged),
        )
        .await;
        assert!(
            body_of(resp).await.contains("teaser"),
            "forged token → teaser"
        );

        // Garbage token → teaser.
        let resp = serve_web_content_with_token(
            &db,
            &blob_store,
            None,
            &actor,
            "/post/premium.html",
            Some(&holder),
            Some("garbage"),
        )
        .await;
        assert!(body_of(resp).await.contains("teaser"));

        // Revoke the grant → even a VALID token darkens to the teaser
        // (revoke-bites-at-use: no period key, no unseal).
        db.delete_capability_grant(&actor, &[7u8; 16])
            .await
            .unwrap();
        holder.registry.refresh().await.unwrap();
        let resp = serve_web_content_with_token(
            &db,
            &blob_store,
            None,
            &actor,
            "/post/premium.html",
            Some(&holder),
            Some(&token),
        )
        .await;
        assert!(
            body_of(resp).await.contains("teaser"),
            "a revoked grant must darken the sealed slice at use time"
        );
    }

    /// The **folder** paywall serve leg (`monetization.md` § Pillar 2, the
    /// folder half): a sealed static file in a paywalled `web` set serves only
    /// under a valid token + a live `content.read{folder:set}` grant. Every
    /// other arm yields the teaser — and **never the ciphertext**.
    #[tokio::test]
    async fn paywalled_folder_serves_sealed_file_only_under_token_and_grant() {
        use fauna_mls::wrapped_blob::{GrantWindow, ScopeTuple, build_grant_blob_with_epochs};

        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let actor = actor();
        let dir = tempfile::tempdir().unwrap();
        let holder = super::super::holder::WebServeHolder::init(dir.path(), db.clone())
            .await
            .unwrap()
            .expect("holder inits");
        let blob_store: Arc<dyn BlobStoreBackend> = Arc::new(MemBlobStore::new());

        // A website set, paywalled to `gold`, holding one sealed file at
        // content-key generation 1.
        let set_id = db
            .create_folder_with_options("members-site", &actor, Default::default())
            .await
            .unwrap();
        db.update_folder_for_user(
            "members-site",
            &actor,
            crate::db::FolderUpdate {
                website_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        db.set_folder_web_paywall_tier_by_id(
            db.get_folder_for_actor("members-site", &actor)
                .await
                .unwrap()
                .unwrap()
                .id,
            Some("gold"),
        )
        .await
        .unwrap();

        let content_key = [0xC1u8; 32];
        let secret_pdf = b"%PDF-1.7 the members-only report".to_vec();
        let manifest_hash = super::super::file_bytes::seed_synced_file(
            &blob_store,
            None,
            &secret_pdf,
            Some(&content_key),
        )
        .await;
        db.upsert_web_file(
            &actor,
            "downloads/report.pdf",
            &manifest_hash,
            "application/pdf",
            Some(set_id),
            Some(1),
        )
        .await
        .unwrap();

        async fn body_of(resp: Response<Body>) -> Vec<u8> {
            axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec()
        }

        // ── No token: the teaser, 402, and NOT the ciphertext. ──
        let resp = serve_web_content_with_token(
            &db,
            &blob_store,
            None,
            &actor,
            "/downloads/report.pdf",
            Some(&holder),
            None,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
        let body = body_of(resp).await;
        assert!(
            String::from_utf8_lossy(&body).contains("gold"),
            "the teaser names the gating tier"
        );
        assert!(
            !body.windows(4).any(|w| w == b"%PDF"),
            "the sealed file's plaintext must never leak into the teaser"
        );

        // A token alone is not enough — the holder must also wield the grant.
        let (token, _) = super::super::token::WebPaywallToken::mint(
            &holder.actor_keypair(),
            actor,
            "downloads/report.pdf",
        )
        .unwrap();
        let resp = serve_web_content_with_token(
            &db,
            &blob_store,
            None,
            &actor,
            "/downloads/report.pdf",
            Some(&holder),
            Some(&token),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::PAYMENT_REQUIRED,
            "a valid token with NO grant must stay dark"
        );

        // ── Grant the set's generation-1 content key → the bytes flow. ──
        let grant = build_grant_blob_with_epochs(
            &actor,
            &[9u8; 16],
            &holder.x25519_pubkey,
            Some(&holder.mlkem_ek),
            GrantWindow(0, u64::MAX),
            &[(
                ScopeTuple {
                    class: ScopeTuple::CLASS_CONTENT_READ.to_string(),
                    kind: Some(ScopeTuple::KIND_FOLDER.to_string()),
                    tier: None,
                    set: Some(ScopeTuple::folder_set_qualifier(
                        &fauna_core::path_crypto::set_name_hash("members-site"),
                    )),
                    factor: None,
                },
                vec![(Some(1), content_key.to_vec())],
            )],
        )
        .unwrap()
        .to_canonical_bytes()
        .unwrap();
        db.put_capability_grant(&actor, &[9u8; 16], &holder.x25519_pubkey, i64::MAX, &grant)
            .await
            .unwrap();
        holder.registry.refresh().await.unwrap();
        // The grant names the set by its hash, so the bytes flow even once the
        // row holds no plaintext name (a sealed set after the contraction).
        db.blank_folder_name_for_test(set_id).await;

        let resp = serve_web_content_with_token(
            &db,
            &blob_store,
            None,
            &actor,
            "/downloads/report.pdf",
            Some(&holder),
            Some(&token),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "private, no-store"
        );
        assert!(resp.headers().get("set-cookie").is_none(), "never a cookie");
        assert_eq!(
            body_of(resp).await,
            secret_pdf,
            "the entitled visitor gets the decrypted file"
        );

        // ── Revoke → the same token goes dark (revoke bites at use). ──
        db.delete_capability_grant(&actor, &[9u8; 16])
            .await
            .unwrap();
        holder.registry.refresh().await.unwrap();
        let resp = serve_web_content_with_token(
            &db,
            &blob_store,
            None,
            &actor,
            "/downloads/report.pdf",
            Some(&holder),
            Some(&token),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::PAYMENT_REQUIRED,
            "a revoked grant darkens the file even for a still-valid token"
        );
    }

    /// **Fail closed:** a sealed file whose set carries NO paywall tier (e.g. a
    /// shared-but-unpaywalled web set) is not web content at all — it 404s,
    /// and the ciphertext is never emitted. This is the seam that previously
    /// did not exist: ingest recorded no sealedness, so serving assumed
    /// plaintext and would have shipped the raw manifest.
    #[tokio::test]
    async fn sealed_file_in_a_non_paywalled_set_fails_closed() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let actor = actor();
        let blob_store: Arc<dyn BlobStoreBackend> = Arc::new(MemBlobStore::new());

        let set_id = db
            .create_folder_with_options("shared-site", &actor, Default::default())
            .await
            .unwrap();
        db.update_folder_for_user(
            "shared-site",
            &actor,
            crate::db::FolderUpdate {
                website_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // NOTE: no `set_folder_web_paywall_tier` — the set is not paywalled.

        let secret = b"ciphertext-only content".to_vec();
        let manifest_hash = super::super::file_bytes::seed_synced_file(
            &blob_store,
            None,
            &secret,
            Some(&[0xC7u8; 32]),
        )
        .await;
        db.upsert_web_file(
            &actor,
            "secret.bin",
            &manifest_hash,
            "application/octet-stream",
            Some(set_id),
            Some(1),
        )
        .await
        .unwrap();

        let resp = serve_web_content(&db, &blob_store, None, &actor, "/secret.bin").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            !body.windows(9).any(|w| w == b"ciphertex"),
            "a sealed row in a non-paywalled set must never emit its bytes"
        );
    }
}
