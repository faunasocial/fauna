//! Web-content-publishing WS-RPC handlers (bearer connection) — part of the
//! WS-RPC-everywhere migration (tracked internally). A behavior-preserving
//! transport migration of the 5 bearer-authed web-content-hosting HTTP routes
//! (`web_content::{publish_routes, domain}`).
//!
//! Six kinds, each reusing the existing core fns (no shared core needed):
//!
//! - `fauna.web.publish.set` — `publish_routes::publish_post`.
//! - `fauna.web.publish.unset` — `publish_routes::unpublish_post`.
//! - `fauna.web.publish.list` — `publish_routes::get_published_posts`.
//! - `fauna.web.domain.set` — `domain::register_domain` (error strings mapped to
//!   `fauna.web.{limit_reached,conflict}` as the twin mapped them to 429/409).
//! - `fauna.web.domain.get` — `db.get_web_domains_for_actor`.
//! - `fauna.web.domain.delete` — `db.delete_web_domain` (owner-scoped); the
//!   per-domain cert lifecycle drops the cert + dir on its next pass (web-content
//!   Slice 4). Has no HTTP twin — added with the custom-domain TLS lifecycle.
//!
//! These ride the **bearer** connection and ARE actor-scoped — the connection
//! `actor_id` is the data scope (replacing the twin's `bearer.0.0`). Gate
//! `User | Admin`. `post_id` arrives as raw 32 bytes (the twin parsed a hex path
//! param); `txt_record` is derived (`_fauna-verify.{domain}`) as the twin did.

use std::time::Duration;

use fauna_protocol::web::{
    PublishedPost, WebDomainDeleteReply, WebDomainDeleteRequest, WebDomainGetReply,
    WebDomainGetRequest, WebDomainInfo, WebDomainSetReply, WebDomainSetRequest,
    WebFilesPruneSealedReply, WebFilesPruneSealedRequest, WebGetApexActorReply,
    WebGetApexActorRequest, WebGetSubdomainEnabledReply, WebGetSubdomainEnabledRequest,
    WebPaywallMintTokenReply, WebPaywallMintTokenRequest, WebPublishListReply,
    WebPublishListRequest, WebPublishSetReply, WebPublishSetRequest, WebPublishUnsetReply,
    WebPublishUnsetRequest, WebSetApexActorReply, WebSetApexActorRequest,
    WebSetSubdomainEnabledReply, WebSetSubdomainEnabledRequest,
};
use fauna_protocol::{ByteBuf, RpcError, Value, decode_strict as decode};

use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};
use crate::web_content::{domain, publish_routes};

/// Error namespace for every `fauna.web.*` code.
const NS: &str = "web";

// ── Helpers (mirroring `files_handlers`, scoped to `web`) ────────────────────

use crate::rpc_errors::{encode_reply, malformed};

fn internal(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(NS, err)
}

fn invalid_request(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::invalid_request_ns(NS, reason)
}

fn limit_reached(reason: impl std::fmt::Display) -> RpcError {
    let mut e = RpcError::new(
        format!("fauna.{NS}.limit_reached"),
        format!("error.{NS}.limit_reached"),
    );
    e.details = Some(Box::new(Value::String(format!("{reason}"))));
    e
}

fn conflict(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::conflict_ns(NS, reason)
}

fn permission_denied(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::permission_denied_ns(NS, reason)
}

fn not_found(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::not_found_ns(NS, reason)
}

/// The `fauna.web.publish.set` authorship gate: refuse a post the caller did
/// not write (`web-content-hosting.md` § Published-post management — "one of
/// the **caller's own** posts"), mirroring `fauna.posts.delete`'s
/// author-only enforcement. `publish.unset` needs no equivalent check — its
/// upsert key already scopes a delete to the caller's own row.
async fn require_post_authorship(
    state: &AppState,
    actor_id: &[u8; 32],
    post_id: &[u8; 32],
) -> Result<(), RpcError> {
    match crate::routes::resolve_post_author(state, post_id)
        .await
        .map_err(internal)?
    {
        Some(author) if author == *actor_id => Ok(()),
        Some(_) => Err(permission_denied("only the author may publish this post")),
        None => Err(not_found("post not found")),
    }
}

/// Resolve the connection actor's `CallerClass` and check the kind's allowlist
/// arm — the WS-RPC counterpart of the HTTP `BearerAuth` extractor gate.
use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// A wire `post_id` (raw bytes) → the `[u8; 32]` the core fns take; a
/// wrong-length buffer is an invalid request (the twin's "invalid post_id").
fn post_id_array(post_id: &ByteBuf) -> Result<[u8; 32], RpcError> {
    crate::rpc_errors::require_bytes32("post_id", post_id.as_ref()).map_err(invalid_request)
}

/// An optional wire `actor_id` (`Some` designates / `None` clears) → the
/// `Option<[u8; 32]>` the apex db accessors take. A wrong-length buffer is an
/// invalid request. Mirrors `set_catch_all_actor_handler`'s set/clear decode.
fn opt_actor_array(actor_id: &Option<ByteBuf>) -> Result<Option<[u8; 32]>, RpcError> {
    match actor_id {
        None => Ok(None),
        Some(b) => crate::rpc_errors::require_bytes32("actor_id", b.as_ref())
            .map(Some)
            .map_err(invalid_request),
    }
}

/// The TXT record name to set for a domain (the twin's `_fauna-verify.{domain}`).
fn txt_record(domain: &str) -> String {
    format!("_fauna-verify.{domain}")
}

// ── fauna.web.publish.set (≡ POST /api/v1/web/publish/{post_id}) ────────────

fn publish_set_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.web.publish.set").await?;
            let req: WebPublishSetRequest = decode(&payload).map_err(malformed)?;
            let post_id = post_id_array(&req.post_id)?;
            require_post_authorship(&state, &actor_id, &post_id).await?;

            publish_routes::publish_post(&state.db, &actor_id, &post_id, req.slug.as_deref())
                .await
                .map_err(internal)?;
            // Re-render the actor's web site so the published post appears
            // (web-content-hosting.md § Routing/render). Best-effort: the
            // publish row is the authoritative state and already succeeded, so a
            // render failure is logged, not surfaced as an RPC error.
            if let Some(wcs) = &state.web_content_service
                && let Err(e) = wcs.render_published_posts(&actor_id).await
            {
                tracing::warn!("web render after publish.set failed: {e}");
            }
            // The effective slug, mirroring the twin's
            // `req.slug.unwrap_or_else(|| hex::encode(post_id))`.
            let slug = match req.slug {
                Some(s) if !s.is_empty() => s,
                _ => hex::encode(post_id),
            };
            encode_reply(&WebPublishSetReply {
                slug,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.web.publish.unset (≡ DELETE …/{post_id}) ──────────────────────────

fn publish_unset_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.web.publish.unset").await?;
            let req: WebPublishUnsetRequest = decode(&payload).map_err(malformed)?;
            let post_id = post_id_array(&req.post_id)?;

            publish_routes::unpublish_post(&state.db, &actor_id, &post_id)
                .await
                .map_err(internal)?;
            // Re-render so the removed post drops out of the site. On EVERY
            // call, not only when a link was removed: an unset naming a post
            // that is already gone is the one gesture that re-renders a site on
            // demand. Unlike publish.set this is a revoke, so it fails closed —
            // a render that errors clears the site rather than keep serving
            // what the author just unpublished (`web-content-hosting.md`
            // § Routing, render, serving → *A revoke is durable*). Logged, not
            // surfaced: the unpublish itself has landed.
            if let Some(wcs) = &state.web_content_service
                && let Err(e) = wcs.rerender_after_revoke(&actor_id, "publish.unset").await
            {
                tracing::warn!("web render after publish.unset: {e:#}");
            }
            encode_reply(&WebPublishUnsetReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.web.paywall.mint_token (monetization.md § Pillar 2) ───────────────

fn paywall_mint_token_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.web.paywall.mint_token").await?;
            let req: WebPaywallMintTokenRequest = decode(&payload).map_err(malformed)?;
            let file_path = req.path.as_deref().filter(|p| !p.is_empty());
            if req.slug.is_empty() && file_path.is_none() {
                return Err(invalid_request("one of slug or path must be given"));
            }
            if !req.slug.is_empty() && file_path.is_some() {
                return Err(invalid_request("slug and path are mutually exclusive"));
            }
            let Some(holder) = &state.web_serve_holder else {
                let mut e = RpcError::new("fauna.web.paywall_unavailable", "error.web.paywall");
                e.details = Some(Box::new(Value::String(
                    "web-paywall serving is not active on this nest (no web-serve holder)".into(),
                )));
                return Err(e);
            };

            // The token is scoped to a path that must actually be servable —
            // both arms below refuse up front rather than minting a token that
            // would only ever yield the teaser.
            let path = match file_path {
                // ── The folder half: a sealed file in a paywalled web set. ──
                Some(p) => {
                    let path = p.trim_start_matches('/').to_string();
                    let row = state
                        .db
                        .get_web_file(&actor_id, &path)
                        .await
                        .map_err(internal)?;
                    let paywalled = match &row {
                        Some(r) if r.is_sealed() => match r.folder_id {
                            Some(id) => state
                                .db
                                .get_folder_by_id(id)
                                .await
                                .map_err(internal)?
                                .and_then(|fs| fs.web_paywall_tier)
                                .is_some(),
                            None => false,
                        },
                        _ => false,
                    };
                    if !paywalled {
                        let mut e =
                            RpcError::new("fauna.web.not_paywalled", "error.web.not_paywalled");
                        e.details = Some(Box::new(Value::String(format!(
                            "no paywalled file at {path} — the file must be synced into a \
                             `web`-mode folder that is sealed and paywalled to a tier \
                             (`fauna.folders.set_web_paywall`)"
                        ))));
                        return Err(e);
                    }
                    path
                }
                // ── The post half: a gated, published post's sealed page. ──
                None => {
                    let path = format!("post/{}.html", req.slug);
                    let sealed = state
                        .db
                        .get_web_rendered_sealed(&actor_id, &path)
                        .await
                        .map_err(internal)?;
                    if sealed.is_none() {
                        let mut e =
                            RpcError::new("fauna.web.not_paywalled", "error.web.not_paywalled");
                        e.details = Some(Box::new(Value::String(format!(
                            "no sealed paywalled page at {path} — the post must be gated to a \
                             tier, web-published, and covered by a live capability grant to the \
                             web-serve holder"
                        ))));
                        return Err(e);
                    }
                    path
                }
            };
            let keypair = holder.actor_keypair();
            let (token, expires) =
                crate::web_content::token::WebPaywallToken::mint(&keypair, actor_id, &path)
                    .map_err(internal)?;
            encode_reply(&WebPaywallMintTokenReply {
                token,
                expires,
                path,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.web.publish.list (≡ GET /api/v1/web/publish) ──────────────────────

fn publish_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.web.publish.list").await?;
            let _req: WebPublishListRequest = decode(&payload).map_err(malformed)?;

            let pairs = publish_routes::get_published_posts(&state.db, &actor_id)
                .await
                .map_err(internal)?;
            let posts = pairs
                .into_iter()
                .map(|(post_id, slug, gated_tier)| PublishedPost {
                    post_id: ByteBuf::from(post_id),
                    slug,
                    // An ungated row omits the key (skip when `None`)
                    // (`web-content-hosting.md` § Published-post management).
                    gated_tier,
                    ..Default::default()
                })
                .collect();
            // The caller's own blanked-site state, read off the row the
            // fail-closed clear wrote (`web-content-hosting.md` § Routing,
            // render, serving → *A blanked site tells its author*).
            let rendered_pages_down = state
                .db
                .web_restore_owed_nonce(&actor_id)
                .await
                .map_err(internal)?
                .is_some();
            encode_reply(&WebPublishListReply {
                posts,
                rendered_pages_down,
                ..Default::default()
            })
        })
    })
}

// ── fauna.web.domain.set (≡ PUT /api/v1/web/domain) ─────────────────────────

fn domain_set_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.web.domain.set").await?;
            let req: WebDomainSetRequest = decode(&payload).map_err(malformed)?;
            if req.domain.is_empty() {
                return Err(invalid_request("domain must not be empty"));
            }

            let apex = state.web_serving_domain();
            match domain::register_domain(&state.db, &actor_id, &req.domain, &apex).await {
                Ok(reg) => encode_reply(&WebDomainSetReply {
                    txt_record: txt_record(&reg.domain),
                    domain: reg.domain,
                    verify_token: reg.verify_token,
                    status: "pending".into(),
                    extra: Default::default(),
                }),
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("domain limit reached") {
                        Err(limit_reached(msg))
                    } else if msg.contains("domain already registered") {
                        Err(conflict(msg))
                    } else if msg.contains("invalid domain")
                        || msg.contains("reserved by this nest")
                    {
                        // Client-supplied domain failed validation / is a
                        // nest-owned host — a 4xx-class client error, not a 500.
                        Err(invalid_request(msg))
                    } else {
                        tracing::error!("web.domain.set error: {e}");
                        Err(internal(e))
                    }
                }
            }
        })
    })
}

// ── fauna.web.domain.get (≡ GET /api/v1/web/domain) ─────────────────────────

fn domain_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.web.domain.get").await?;
            let _req: WebDomainGetRequest = decode(&payload).map_err(malformed)?;

            let rows = state
                .db
                .get_web_domains_for_actor(&actor_id)
                .await
                .map_err(internal)?;
            let domains = rows
                .into_iter()
                .map(|r| WebDomainInfo {
                    txt_record: txt_record(&r.domain),
                    domain: r.domain,
                    verify_token: r.verify_token,
                    status: r.status,
                    created_at: r.created_at,
                    verified_at: r.verified_at,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&WebDomainGetReply {
                domains,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.web.domain.delete ─────────────────────────────────────────────────

fn domain_delete_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.web.domain.delete").await?;
            let req: WebDomainDeleteRequest = decode(&payload).map_err(malformed)?;
            if req.domain.is_empty() {
                return Err(invalid_request("domain must not be empty"));
            }

            // Ownership: only the actor who registered the domain may deregister
            // it. A missing row is an idempotent no-op (`ok: false`). The
            // per-domain cert lifecycle drops the cert + dir on its next pass.
            match state
                .db
                .get_web_domain_by_domain(&req.domain)
                .await
                .map_err(internal)?
            {
                None => encode_reply(&WebDomainDeleteReply {
                    ok: false,
                    extra: Default::default(),
                }),
                Some(row) => {
                    if row.actor_id.as_slice() != actor_id.as_slice() {
                        return Err(permission_denied("domain registered by another actor"));
                    }
                    let removed = state
                        .db
                        .delete_web_domain(&req.domain)
                        .await
                        .map_err(internal)?;
                    // Stop serving the deregistered site NOW, not within a poll
                    // interval. The domain lifecycle task's reconcile is the
                    // steady-state authority over this map and would drop it
                    // within 5 min anyway, but until it does the nest keeps
                    // serving content the owner just withdrew — and the cert
                    // stays installed for that same window, so HTTPS does not
                    // mask it. Same shape as `set_subdomain_enabled`'s live
                    // update; skipped harmlessly in the dormant pre-activation
                    // state (`host_resolver == None`).
                    if let Some(resolver) = &state.host_resolver {
                        resolver.remove_custom_domain(&req.domain).await;
                    }
                    encode_reply(&WebDomainDeleteReply {
                        ok: removed,
                        extra: Default::default(),
                    })
                }
            }
        })
    })
}

// ── fauna.web.set_apex_actor (Admin) ────────────────────────────────────────

fn set_apex_actor_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.web.set_apex_actor").await?;
            let req: WebSetApexActorRequest = decode(&payload).map_err(malformed)?;
            let apex = opt_actor_array(&req.actor_id)?;

            // Persist the designation (the authoritative nest-wide singleton).
            match apex {
                Some(a) => state.db.set_apex_actor(&a).await.map_err(internal)?,
                None => state.db.clear_apex_actor().await.map_err(internal)?,
            }
            // Update the live resolver if serving is active. Boot seeds the
            // resolver from the same db row, so in the dormant pre-activation
            // state (`host_resolver == None`) this is harmlessly skipped.
            if let Some(resolver) = &state.host_resolver {
                resolver.set_apex_actor(apex).await;
            }
            encode_reply(&WebSetApexActorReply {
                actor_id: apex.map(|a| ByteBuf::from(a.to_vec())),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.web.get_apex_actor (Admin) ────────────────────────────────────────

fn get_apex_actor_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.web.get_apex_actor").await?;
            let _req: WebGetApexActorRequest = decode(&payload).map_err(malformed)?;

            let apex = state.db.get_apex_actor().await.map_err(internal)?;
            encode_reply(&WebGetApexActorReply {
                actor_id: apex.map(|a| ByteBuf::from(a.to_vec())),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.web.set_subdomain_enabled (User, caller-scoped) ───────────────────

fn set_subdomain_enabled_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.web.set_subdomain_enabled").await?;
            let req: WebSetSubdomainEnabledRequest = decode(&payload).map_err(malformed)?;

            // Persist the per-actor opt-in (presence-as-flag; the authoritative
            // state the boot seed + cert lifecycle reconcile from).
            if req.enabled {
                state
                    .db
                    .set_subdomain_enabled(&actor_id)
                    .await
                    .map_err(internal)?;
            } else {
                state
                    .db
                    .clear_subdomain_enabled(&actor_id)
                    .await
                    .map_err(internal)?;
            }

            // Update the live `HostResolver` `<handle>` → actor map if serving is
            // active. The user's handle keys the subdomain; an actor with no
            // handle persists the flag but registers nothing live (boot + the
            // 5-min cert reconcile pick it up once a handle exists). A handle
            // colliding with a reserved label is skipped (`resolve` excludes
            // reserved hosts regardless). The per-subdomain cert is issued/dropped
            // separately by the cert lifecycle loop.
            if let Some(resolver) = &state.host_resolver
                && let Some(handle) = state.db.get_handle(&actor_id).await.map_err(internal)?
                && !handle.is_empty()
                && !crate::web_content::serve::is_reserved_subdomain_label(&handle)
            {
                if req.enabled {
                    resolver.register_subdomain(&handle, actor_id).await;
                } else {
                    resolver.remove_subdomain(&handle).await;
                }
            }
            encode_reply(&WebSetSubdomainEnabledReply {
                enabled: req.enabled,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.web.get_subdomain_enabled (User, caller-scoped) ───────────────────

fn get_subdomain_enabled_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.web.get_subdomain_enabled").await?;
            let _req: WebGetSubdomainEnabledRequest = decode(&payload).map_err(malformed)?;

            let enabled = state
                .db
                .is_subdomain_enabled(&actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&WebGetSubdomainEnabledReply {
                enabled,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.web.files.prune_sealed ────────────────────────────────────────────

/// The owner's client declaring one folder's complete live website corpus, so
/// the nest can drop the **sealed** `web_files` rows no longer in
/// it.
///
/// Owner-only: the folder is resolved through `get_folder_for_actor`, so the
/// declaration can only ever touch the caller's own projection — a *member* of
/// someone else's shared folder resolves nothing here, exactly as they cannot
/// become a seat for it.
///
/// The set is taken at face value **because of where the client sends it**:
/// straight after a fully successful re-record walk, which fails loudly rather
/// than reporting a partial one. The nest cannot second-guess the set's
/// *contents* — it holds no names for this class, which is the whole reason
/// the kind exists. An **empty** set is different: that is a judgement about
/// the message's *shape*, needing no names at all, so it gets one — duplicating the refusal `SyncEngine::declare_live_web_corpus`
/// already makes one crate away (it never sends one), because from inside a
/// syncing seat "nothing is live" and "this device has not caught up" are the
/// same observation, and the nest has no way to tell them apart either.
fn files_prune_sealed_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.web.files.prune_sealed").await?;
            let req: WebFilesPruneSealedRequest = decode(&payload).map_err(malformed)?;
            let name_hash = crate::routes::parse_name_hash(&req.name_hash, |m| invalid_request(m))?;
            if req.folder.is_empty() && name_hash.is_none() {
                return Err(invalid_request("folder is required"));
            }
            if req.paths.is_empty() {
                return Err(invalid_request(
                    "an empty declaration is indistinguishable from a seat that has not caught up",
                ));
            }
            // Hash-first (S5b), like every owner-scoped folder door.
            let Some(row) = match name_hash {
                Some(h) => {
                    state
                        .db
                        .get_folder_for_actor_by_name_hash(&h, &actor_id)
                        .await
                }
                None => state.db.get_folder_for_actor(&req.folder, &actor_id).await,
            }
            .map_err(internal)?
            else {
                // Indistinguishable from "no such folder", like every other
                // owner-scoped folder door.
                return Err(invalid_request("no such folder"));
            };
            let keep: std::collections::HashSet<String> = req.paths.into_iter().collect();
            let dropped = state
                .db
                .prune_sealed_web_files(&actor_id, row.id, &keep)
                .await
                .map_err(internal)?;
            if dropped > 0 {
                tracing::info!(
                    dropped,
                    folder = %fauna_core::log_redact::log_folder_name(&req.folder),
                    "pruned sealed web_files rows the owner's corpus no longer holds"
                );
            }
            encode_reply(&WebFilesPruneSealedReply {
                dropped: dropped as u32,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ────────────────────────────────────────────────

/// Register the authenticated web-content-publishing surface on the **bearer**
/// router. Per-kind replay semantics + rationale: see
/// `KindRegistry::register_web_kinds`. Reads + idempotent publish mutations are
/// `forbid_replay = false` @5 s; `domain.set` (non-idempotent write) is @30 s.
pub fn register_web_handlers(b: &mut RpcRouterBuilder) {
    let read = |handler| RpcKindMeta {
        forbid_replay: false,
        default_deadline: Duration::from_secs(5),
        handler,
    };
    b.add("fauna.web.publish.set", read(publish_set_handler()));
    b.add("fauna.web.publish.unset", read(publish_unset_handler()));
    b.add("fauna.web.publish.list", read(publish_list_handler()));
    b.add(
        "fauna.web.paywall.mint_token",
        read(paywall_mint_token_handler()),
    );
    b.add(
        "fauna.web.files.prune_sealed",
        read(files_prune_sealed_handler()),
    );
    b.add(
        "fauna.web.domain.set",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: domain_set_handler(),
        },
    );
    b.add("fauna.web.domain.get", read(domain_get_handler()));
    b.add(
        "fauna.web.domain.delete",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: domain_delete_handler(),
        },
    );
    // Admin apex-actor designation (idempotent singleton upsert/clear + a pure
    // read) — `web-content-hosting.md` § Admin apex hosting.
    b.add("fauna.web.set_apex_actor", read(set_apex_actor_handler()));
    b.add("fauna.web.get_apex_actor", read(get_apex_actor_handler()));
    // Per-user subdomain-hosting opt-in (idempotent caller-scoped upsert/clear +
    // a pure read) — `web-content-hosting.md` § Routing + Architectural rule 8.
    b.add(
        "fauna.web.set_subdomain_enabled",
        read(set_subdomain_enabled_handler()),
    );
    b.add(
        "fauna.web.get_subdomain_enabled",
        read(get_subdomain_enabled_handler()),
    );
}
