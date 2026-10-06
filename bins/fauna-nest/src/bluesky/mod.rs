//! Bluesky (ATProto) integration routes and background tasks.
//! Gated behind the `bluesky` cargo feature.

pub mod auth_routes;
pub mod bluesky_handlers;
pub mod bridge_provider;
pub mod db_helpers;
pub mod dm_leg;
pub mod dm_worker;
// `interact_routes` no longer registers any bluesky-specific HTTP route (the
// `interact/*` twins were ripped); it stays for
// the shared `route_unified_interaction` core the unified `fauna.posts.interact`
// path calls (`crate::interact_routes`).
pub mod feed_ingest;
#[cfg(feature = "test-hooks")]
pub mod feed_test_hook;
pub mod feed_worker;
pub mod interact_routes;
pub mod media_routes;
pub mod notif_sync;
pub mod notif_worker;
pub mod storage_backend;

use crate::routes::AppState;
use axum::Router;
use std::sync::Arc;

/// **The one place this bridge's schema is applied** — `init_db` and the
/// `actor_tables` registry guards' seeding both call it, so a migration cannot
/// reach production while staying invisible to the column belts
/// (the twin of `activitypub::apply_schema`, whose doc
/// carries the full reasoning).
///
/// `fauna_bridge_atproto::db::CREATE_TABLES_SQL` is this bridge's genesis,
/// applied in the one shape every bridge shares
/// ([`crate::bridge_schema::apply_genesis`]: the block plus the additive column
/// reconciler, no hand-written `ALTER`).
pub fn apply_schema(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    crate::bridge_schema::apply_genesis(conn, fauna_bridge_atproto::db::CREATE_TABLES_SQL)
}

/// Initialize Bluesky bridge tables in the node database.
pub async fn init_db(db: &crate::db::CacheDb) -> anyhow::Result<()> {
    let conn = db.conn().await;
    apply_schema(&conn)
}

/// The public base URL this nest presents to Bluesky's OAuth authorization
/// server — `https://<identity-domain>` — or `None` when the deployment has no
/// public identity domain.
///
/// `handle_domain` is [`AppState::handle_domain_if_set`], i.e. the CLAIMED
/// identity domain and never the `"localhost"` placeholder. The publicness test
/// is the uniform one (`resolve_handle_domain(d).is_public_dns_name`, shared
/// with [`AppState::is_public_deployment`] and the ATProto hosting gate), which
/// also splits a `host:port` authority before classifying — the bare
/// `is_public_dns_name` would read `127.0.0.1:8080` as a DNS name because the
/// port defeats its IP-literal parse. The resolver already composes
/// `https://{domain}` for the public branch, so this returns its `base_url`
/// rather than re-spelling the scheme.
///
/// **Why this is derived and not configured.** The URL is the `client_id`
/// Bluesky's authorization server fetches over public HTTPS
/// (`fauna_bridge_atproto::oauth::build_client_metadata`), and it is exactly the
/// deployment's own identity domain — nobody *chooses* it, so it is bucket (1)
/// of `principles.md` § One configuration surface. It arrived here as the
/// `--bluesky-public-url` CLI flag, which no shipped launch line ever passed
/// (`docker/s6/fauna-nest/run`, `install.sh`'s `ExecStart`), leaving the bridge
/// dark on every real deployment; it was deleted for the same reason
/// `--push-relay-url` was.
///
/// A domainless / `localhost` / `.local` / IP-literal box yields `None` and the
/// bridge reports itself unavailable *with a reason* — the honest answer, since
/// such a box genuinely cannot complete an OAuth round: the authorization server
/// must resolve and fetch the client-metadata document by name.
///
/// Pure (no `AppState`, no I/O) so it unit-tests directly, mirroring
/// `discovery_core::iroh_relay_public_url`.
pub fn oauth_public_url(handle_domain: Option<&str>) -> Option<String> {
    let domain = handle_domain.filter(|d| !d.is_empty())?;
    // The emptiness guard is not belt-and-braces: the classifier answers
    // `is_public_dns_name: true` for `""` (it is not localhost, not `.local`,
    // not an IP literal), which would compose the bare `https://` as a
    // client_id. `iroh_relay_public_url` carries the same guard for the same
    // reason.
    //
    // Same reasoning for userinfo/a path/a query/a fragment/whitespace:
    // `is_public_dns_name` is a negative test, so any of those still reads as
    // "public" (`security.md` § Transport trust). Refuse
    // before deriving — this URL becomes the OAuth `client_id` Bluesky's
    // authorization server fetches by name.
    if !fauna_core::web::is_hostname_syntax(domain) {
        return None;
    }
    let target = fauna_provisioning::probe::resolve_handle_domain(domain);
    target.is_public_dns_name.then_some(target.base_url)
}

/// [`write_through_create_inner`] calls this over its own already-decoded
/// `post` (it needs the decoded value first, for
/// [`crate::db::public_servability::publishable_off_box_at_create`]).
fn write_through_text_of(
    post: fauna_core::data::Post,
) -> Option<(String, Vec<fauna_core::data::Facet>)> {
    match post.body {
        fauna_core::data::PostBody::Text { content, facets } => Some((content, facets)),
        fauna_core::data::PostBody::TextWithMedia {
            content, facets, ..
        } => Some((content, facets)),
        _ => {
            tracing::debug!("write-through: skipping non-text post");
            None
        }
    }
}

/// Spawn a background task to cross-post a Fauna post to Bluesky.
///
/// Fire-and-forget: retries 3 times with exponential backoff, then gives up.
/// Only cross-posts public text posts (Text, TextWithMedia). Skips media-only,
/// video, structured, and encrypted posts — and a post that is **already
/// cross-posted** ([`WriteThroughCreateOutcome::AlreadyCrossposted`]).
pub fn spawn_write_through(
    state: Arc<AppState>,
    actor_id: [u8; 32],
    post_id: [u8; 32],
    post_bytes: Vec<u8>,
) {
    // Generation-scoped: this task retries with backoff, so it deliberately
    // outlives the request that started it — the "long-lived worker hiding in a
    // request path" case, holding `Arc<AppState>` and cross-posting under the
    // user's PDS credential the whole time.
    let scope = Arc::clone(&state);
    scope.spawn_scoped(async move {
        if let Err(e) = write_through_create_inner(&state, actor_id, post_id, &post_bytes).await {
            tracing::warn!("write-through: {e:#}");
        }
    });
}

/// Outcome of one write-through **create** pass — the testable seam, the
/// create twin of [`WriteThroughDeleteOutcome`] (same reasoning: there is no
/// fake ATProto PDS in the nest test harness, so the unit tests pin the
/// decision boundaries that don't need one; agent-step failures surface as
/// `Err`, exactly like `write_through_delete_inner`).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WriteThroughCreateOutcome {
    /// No consume-side Bluesky link for the author — nothing to write as.
    NotLinked,
    /// Write-through disabled (mode 0), or mode 2 without the `crosspost` tag
    /// — and the post references no Bluesky-origin post, which would have
    /// admitted it by itself (`crosspost_admitted`).
    Skipped,
    /// Undecodable or non-text body — nothing to cross-post.
    NotCrosspostable,
    /// The create-time half of the off-box servability rule refuses it
    /// (`db::public_servability::publishable_off_box_at_create`): a gated
    /// post — its plaintext body is the paywall teaser — or an
    /// archive-imported one (`archive-import.md` § Compatibility → *Slice-3
    /// rulings*, ruling 1: served on Fauna, never re-broadcast). Decided before
    /// any network step, exactly like `AlreadyCrossposted`.
    NotOffBoxPublishable,
    /// **Row 103a, the replay guard:** the mapping already
    /// names an external record for this content-addressed post. A
    /// byte-identical replay (`posts.create` is `OfflineSafe` — same content
    /// ⇒ same post id, possibly a fresh idempotency key) must not publish a
    /// SECOND record: `create_record` goes out with `rkey: None`
    /// (server-assigned), and `store_crosspost_mapping` would overwrite the
    /// mapping — orphaning the first record, which a later delete then never
    /// removes. Checked BEFORE the agent step so it is pinnable headlessly.
    AlreadyCrossposted,
    /// Posted and the mapping stored.
    Posted,
    /// Every attempt failed at the PDS; no mapping written.
    GaveUp,
}

/// Which referencing relation of a post the write-through derives
/// (`bridges.md` § Cross-posting → *A post that references a Bluesky
/// record*). Reposts, reactions and votes are not records this leg emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReferencedKind {
    /// `Reference::Reply` → `reply: {parent, root}`.
    Reply,
    /// `Reference::Quote` → `embed: app.bsky.embed.record`.
    Quote,
}

/// What the post's `Reference::Reply`/`Quote` points at, as far as the
/// account tables can say without a PDS round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedReference {
    pub kind: ReferencedKind,
    /// Hex of the target's 32-byte content digest — the
    /// `bluesky_posts.fauna_post_id` key.
    pub target_hex: String,
    /// A `bluesky_posts` row names a Bluesky record for the target, so the
    /// derived record can thread under it; unmapped → the record is
    /// standalone (the projection's rule — the target was never a Bluesky
    /// record).
    pub mapped: bool,
    /// The target's `content.source` is `bluesky` — a post the consume side
    /// ingested. **The reference is the intent:** such a post is
    /// cross-postable whatever the mode; a reference to one's own
    /// cross-posted Fauna post follows the mode.
    pub bluesky_origin: bool,
}

/// The create leg's decision, split from its execution so every gate is
/// pinnable without a PDS: [`decide_write_through`] reads the tables and the
/// post; [`write_through_create_inner`] resolves the reference, builds the
/// record and sends it.
#[derive(Debug, PartialEq)]
pub(crate) enum WriteThroughDecision {
    /// Converged before any network step, with the outcome to report.
    Skip(WriteThroughCreateOutcome),
    /// Cross-post this text, threading under `reference` when it is mapped.
    Post {
        text: String,
        facets: Vec<fauna_core::data::Facet>,
        reference: Option<PlannedReference>,
    },
}

/// The first `Reference::Reply` on the post, else its first `Reference::Quote`
/// — the one relation the derived record can carry as a thread or embed.
fn referenced_target(post: &fauna_core::data::Post) -> Option<(ReferencedKind, [u8; 32])> {
    use fauna_core::data::Reference;
    let reply = post.references.iter().find_map(|r| match r {
        Reference::Reply { post_id } => Some((ReferencedKind::Reply, post_id)),
        _ => None,
    });
    reply
        .or_else(|| {
            post.references.iter().find_map(|r| match r {
                Reference::Quote { post_id } => Some((ReferencedKind::Quote, post_id)),
                _ => None,
            })
        })
        .map(|(kind, id)| (kind, crate::db::posts::cid_to_digest(id)))
}

/// The mode gate, pure: does `write_through` admit this post? A post whose
/// reference names a Bluesky-origin post is admitted by the reference itself
/// (the user tapped reply or quote on a Bluesky post — the intent the toggle
/// exists to ask for, exactly as a nostr reply rides regardless of
/// `auto_publish`); everything else follows the mode — off, auto, or the
/// protocol-agnostic `crosspost` tag.
fn crosspost_admitted(mode: i64, has_crosspost_tag: bool, references_bluesky_origin: bool) -> bool {
    references_bluesky_origin
        || match mode {
            1 => true,
            2 => has_crosspost_tag,
            _ => false,
        }
}

/// Every gate of the create leg that needs no PDS — the decision half of
/// [`write_through_create_inner`], in the order the gates fire. Linked account
/// first (nothing else is meaningful for an unlinked author), then the decode
/// and off-box servability rules, then the reference lookup, then the mode
/// gate the reference may override, then the replay guard.
pub(crate) async fn decide_write_through(
    state: &Arc<AppState>,
    actor_id: [u8; 32],
    post_id: [u8; 32],
    post_bytes: &[u8],
) -> anyhow::Result<WriteThroughDecision> {
    use WriteThroughCreateOutcome as O;
    use WriteThroughDecision::Skip;
    let actor_hex = hex::encode(actor_id);

    let (linked, mode) = {
        let conn = state.db.conn().await;
        (
            db_helpers::get_linked_account(&conn, &actor_hex)?.is_some(),
            db_helpers::get_write_through(&conn, &actor_hex).unwrap_or(0),
        )
    };
    if !linked {
        return Ok(Skip(O::NotLinked));
    }

    // Decode the post; the create-time half of the one off-box servability
    // rule (gated, or archive-imported — ruling 1) refuses before the text is
    // even looked at, so a paywall teaser or a backdated import never reaches
    // the PDS. The pull-side projection stream applies the SQL predicate.
    let Some(post) = crate::db::posts::decode_stored_post(post_bytes) else {
        tracing::warn!("write-through: undecodable post body");
        return Ok(Skip(O::NotCrosspostable));
    };
    if !crate::db::public_servability::publishable_off_box_at_create(&post) {
        tracing::debug!("write-through: not publishable off-box at create, skipping");
        return Ok(Skip(O::NotOffBoxPublishable));
    }

    // The reference, resolved as far as the tables go: is the target mapped
    // to a Bluesky record, and is it a Bluesky-origin post?
    let reference = match referenced_target(&post) {
        Some((kind, digest)) => {
            let target_hex = hex::encode(digest);
            let mapped = {
                let conn = state.db.conn().await;
                db_helpers::get_crosspost_uri(&conn, &target_hex)?.is_some()
            };
            let bluesky_origin = state
                .db
                .get_post_source(&digest)
                .await
                .ok()
                .flatten()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("bluesky"));
            Some(PlannedReference {
                kind,
                target_hex,
                mapped,
                bluesky_origin,
            })
        }
        None => None,
    };
    let references_bluesky_origin = reference.as_ref().is_some_and(|r| r.bluesky_origin);

    // Pull the cross-postable text + facets.
    let Some((text, facets)) = write_through_text_of(post) else {
        return Ok(Skip(O::NotCrosspostable));
    };

    // The mode gate. mode 2's opt-in is the protocol-agnostic "crosspost" tag;
    // clients don't need to know about Bluesky.
    let has_crosspost_tag = facets.iter().any(|f| {
        matches!(
            &f.feature,
            fauna_core::data::FacetFeature::Tag { name } if name == "crosspost"
        )
    });
    if !crosspost_admitted(mode, has_crosspost_tag, references_bluesky_origin) {
        tracing::debug!("write-through: mode={mode} does not admit this post, skipping");
        return Ok(Skip(O::Skipped));
    }

    // Replay guard (see `AlreadyCrossposted`): before any network step, an
    // existing mapping means this exact content-addressed post already has a
    // live external record — converge on it instead of minting a second.
    {
        let conn = state.db.conn().await;
        if db_helpers::get_crosspost_uri(&conn, &hex::encode(post_id))?.is_some() {
            tracing::debug!(
                "write-through: {} already cross-posted, skipping replay",
                hex::encode(post_id)
            );
            return Ok(Skip(O::AlreadyCrossposted));
        }
    }

    Ok(WriteThroughDecision::Post {
        text,
        facets,
        reference,
    })
}

/// The testable core of the write-through **create** leg. See
/// [`spawn_write_through`] (the production entry) and
/// [`WriteThroughCreateOutcome`] (the seam's vocabulary). The gates live in
/// [`decide_write_through`]; this half resolves a mapped reference against the
/// PDS (the parent's current CID and its own thread root — `bridges.md`
/// § Cross-posting → *A post that references a Bluesky record*), builds the
/// record and sends it.
pub(crate) async fn write_through_create_inner(
    state: &Arc<AppState>,
    actor_id: [u8; 32],
    post_id: [u8; 32],
    post_bytes: &[u8],
) -> anyhow::Result<WriteThroughCreateOutcome> {
    let actor_hex = hex::encode(actor_id);

    let (text, facets, reference) =
        match decide_write_through(state, actor_id, post_id, post_bytes).await? {
            WriteThroughDecision::Skip(outcome) => return Ok(outcome),
            WriteThroughDecision::Post {
                text,
                facets,
                reference,
            } => (text, facets, reference),
        };

    // Resolve the mapped reference at the PDS. A failed resolution is an
    // error, never a silent standalone post: publishing the words detached
    // from the thread the user replied in would be the wrong post, and the
    // caller's log line is what a later retry keys on. An unmapped reference
    // (or a mapping that vanished underneath) is the standalone case by rule.
    let mut reply: Option<fauna_bridge_atproto::outbound::ReplyRefs> = None;
    let mut quote: Option<fauna_bridge_atproto::outbound::QuoteRef> = None;
    if let Some(r) = reference.as_ref().filter(|r| r.mapped) {
        match r.kind {
            ReferencedKind::Reply => {
                reply = db_helpers::resolve_reply_refs(state, &actor_hex, &r.target_hex)
                    .await
                    .map_err(|e| anyhow::anyhow!("reply target resolution failed: {e}"))?;
            }
            ReferencedKind::Quote => {
                quote = db_helpers::resolve_uri_and_cid(state, &r.target_hex)
                    .await
                    .map_err(|e| anyhow::anyhow!("quote target resolution failed: {e}"))?
                    .map(|(uri, cid)| fauna_bridge_atproto::outbound::QuoteRef { uri, cid });
            }
        }
    }

    // Build the ATProto record
    let record = match fauna_bridge_atproto::outbound::fauna_post_to_bsky_record(
        &text,
        &facets,
        reply.as_ref(),
        quote.as_ref(),
    ) {
        Some(r) => r,
        None => return Ok(WriteThroughCreateOutcome::NotCrosspostable),
    };

    // Get the authenticated agent
    let agent = db_helpers::get_agent_for_actor(state, &actor_hex)
        .await
        .map_err(|e| anyhow::anyhow!("agent failed: {e}"))?;

    // Convert the record JSON to an atrium Unknown value
    let record_value: fauna_bridge_atproto::atrium_api::types::Unknown =
        serde_json::from_value(record.record_json)
            .map_err(|e| anyhow::anyhow!("record serialization failed: {e}"))?;

    let nsid: fauna_bridge_atproto::atrium_api::types::string::Nsid = record
        .collection
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid collection NSID"))?;

    let did = agent
        .did()
        .await
        .ok_or_else(|| anyhow::anyhow!("could not determine DID from session"))?;

    // Retry loop: 3 attempts with exponential backoff
    let delays = [1u64, 5, 30];
    for (attempt, delay) in delays.iter().enumerate() {
        let input =
            fauna_bridge_atproto::atrium_api::com::atproto::repo::create_record::InputData {
                collection: nsid.clone(),
                record: record_value.clone(),
                repo: fauna_bridge_atproto::atrium_api::types::string::AtIdentifier::Did(
                    did.clone(),
                ),
                rkey: None,
                swap_commit: None,
                validate: None,
            };

        match agent.api.com.atproto.repo.create_record(input.into()).await {
            Ok(output) => {
                let post_id_hex = hex::encode(post_id);
                let did_str = did.to_string();
                let conn = state.db.conn().await;
                let cid = output.cid.as_ref().to_string();
                let _ = db_helpers::store_crosspost_mapping(
                    &conn,
                    &post_id_hex,
                    &output.uri,
                    &did_str,
                    &cid,
                );
                drop(conn);
                tracing::info!("write-through: posted {post_id_hex} → {}", output.uri);
                return Ok(WriteThroughCreateOutcome::Posted);
            }
            Err(e) => {
                tracing::warn!(
                    "write-through attempt {}/{}: {e}",
                    attempt + 1,
                    delays.len()
                );
                if attempt < delays.len() - 1 {
                    tokio::time::sleep(std::time::Duration::from_secs(*delay)).await;
                }
            }
        }
    }
    tracing::error!(
        "write-through: gave up after {} attempts for {}",
        delays.len(),
        hex::encode(post_id)
    );
    Ok(WriteThroughCreateOutcome::GaveUp)
}

/// Outcome of the write-through **delete** leg — lets `delete_post_core` (and
/// the unit tests) observe whether a cross-posted record actually existed.
#[derive(Debug, PartialEq, Eq)]
pub enum WriteThroughDeleteOutcome {
    /// The post was never cross-posted (no `bluesky_posts` mapping) — a clean
    /// no-op. The common case: most posts are never cross-posted.
    NotCrossposted,
    /// The cross-posted record was removed on the user's external DID repo and
    /// the `bluesky_posts` mapping cleared.
    Deleted,
}

/// The testable core of the Bluesky write-through **delete** leg — the delete
/// twin of `spawn_write_through` (`feed.md` § Post deletion → *Propagation*: a
/// deleted post must not outlive its Bluesky write-through record). Looks up the
/// `bluesky_posts` mapping for the deleted fauna post; when the post was
/// cross-posted, removes the record on the user's external DID repo via
/// `com.atproto.repo.deleteRecord` (the same XRPC path
/// `interact_routes::delete_interaction_record` drives for unlike/unrepost),
/// then clears the mapping.
///
/// **Retention-on-failure is deliberate:** the mapping is cleared ONLY after
/// `deleteRecord` succeeds. A transient agent/XRPC failure leaves the mapping in
/// place — never dropping the binding to a record that still lives on the
/// external PDS — so the caller's retry loop (below) can chase it again.
pub(crate) async fn write_through_delete_inner(
    state: &Arc<AppState>,
    actor_id: [u8; 32],
    post_id: [u8; 32],
) -> anyhow::Result<WriteThroughDeleteOutcome> {
    let actor_hex = hex::encode(actor_id);
    let post_id_hex = hex::encode(post_id);

    // Look up the cross-post mapping. No mapping → nothing to delete.
    let conn = state.db.conn().await;
    let at_uri = db_helpers::get_crosspost_uri(&conn, &post_id_hex)?;
    drop(conn);
    let at_uri = match at_uri {
        Some(u) => u,
        None => return Ok(WriteThroughDeleteOutcome::NotCrossposted),
    };

    // Parse at://did/collection/rkey (the shape `resolve_uri_and_cid` parses).
    let parts: Vec<&str> = at_uri
        .strip_prefix("at://")
        .unwrap_or(&at_uri)
        .splitn(3, '/')
        .collect();
    if parts.len() != 3 {
        anyhow::bail!("malformed AT-URI: {at_uri}");
    }

    let agent = db_helpers::get_agent_for_actor(state, &actor_hex)
        .await
        .map_err(|e| anyhow::anyhow!("agent error: {e}"))?;

    use fauna_bridge_atproto::atrium_api::com::atproto::repo::delete_record;
    let input = delete_record::InputData {
        collection: parts[1]
            .parse()
            .map_err(|_| anyhow::anyhow!("bad NSID in AT-URI: {at_uri}"))?,
        repo: fauna_bridge_atproto::atrium_api::types::string::AtIdentifier::Did(
            parts[0]
                .parse()
                .map_err(|_| anyhow::anyhow!("bad DID in AT-URI: {at_uri}"))?,
        ),
        rkey: parts[2]
            .parse()
            .map_err(|_| anyhow::anyhow!("bad rkey in AT-URI: {at_uri}"))?,
        swap_commit: None,
        swap_record: None,
    };

    agent
        .api
        .com
        .atproto
        .repo
        .delete_record(input.into())
        .await
        .map_err(|e| anyhow::anyhow!("deleteRecord failed: {e}"))?;

    // Cleared ONLY after the remote delete succeeded (retention-on-failure).
    let conn = state.db.conn().await;
    db_helpers::delete_crosspost_mapping(&conn, &post_id_hex)?;
    drop(conn);

    Ok(WriteThroughDeleteOutcome::Deleted)
}

/// Spawn a background task to remove a deleted fauna post's cross-posted Bluesky
/// record — the delete twin of `spawn_write_through`, hooked into
/// `routes::delete_post_core`. Fire-and-forget + non-fatal: a failed external
/// delete must never fail the author's local post delete. Retries like the
/// create twin (a user deletes a post only once, so a transient outage would
/// otherwise orphan the record on the external PDS forever); the mapping is
/// retained across failures, so each retry re-attempts cleanly.
pub fn spawn_write_through_delete(state: Arc<AppState>, actor_id: [u8; 32], post_id: [u8; 32]) {
    // Generation-scoped: the retry ladder below runs for ≥36s of sleeps alone,
    // so this task routinely outlives its request (see `spawn_write_through`).
    let scope = Arc::clone(&state);
    scope.spawn_scoped(async move {
        let delays = [1u64, 5, 30];
        for (attempt, delay) in delays.iter().enumerate() {
            match write_through_delete_inner(&state, actor_id, post_id).await {
                Ok(WriteThroughDeleteOutcome::Deleted) => {
                    tracing::info!(
                        "write-through delete: removed Bluesky record for {}",
                        hex::encode(post_id)
                    );
                    return;
                }
                Ok(WriteThroughDeleteOutcome::NotCrossposted) => return,
                Err(e) => {
                    tracing::warn!(
                        "write-through delete attempt {}/{} for {}: {e}",
                        attempt + 1,
                        delays.len(),
                        hex::encode(post_id)
                    );
                    if attempt < delays.len() - 1 {
                        tokio::time::sleep(std::time::Duration::from_secs(*delay)).await;
                    }
                }
            }
        }
        tracing::error!(
            "write-through delete: gave up after {} attempts for {}",
            delays.len(),
            hex::encode(post_id)
        );
    });
}

pub fn routes() -> Router<Arc<AppState>> {
    // Consume-side bluesky control-plane was ripped to WS-RPC / unified
    // surfaces. The thread view moved onto the
    // `bluesky.feed.thread` WS-RPC kind (`bluesky_handlers.rs`) and its two HTTP
    // twins (`feed/thread/{uri}` + `thread?post_id=`) were deleted. What remains
    // on HTTP here:
    //   - auth_routes: the OAuth residue (`auth/callback`,
    //     `.well-known/atproto-oauth-client`) — the far end is Bluesky's OAuth
    //     server. The `auth/{start,status}` / `auth` (DELETE) / `profile` twins
    //     are deleted; `fauna.bridges.*` is the only surface.
    //   - media_routes: the CDN image/video proxy (byte-bulk residue, stays HTTP).
    Router::new()
        .merge(auth_routes::routes())
        .merge(media_routes::routes())
}

#[cfg(test)]
mod write_through_decode_tests {
    //! The create-side write-through's decode boundary: BOTH stored post shapes
    //! must yield the cross-postable text. Native app posts are
    //! embed-as-bytes (`fauna-client-core::post` — `sign_and_pack`); bridge
    //! posts are bare. Regression pin for the dark break where the bare-only
    //! `canonical_decode::<Post>` silently no-opped the write-through on every
    //! native post (`bridges.md` § Cross-posting).
    use super::write_through_text_of;
    use fauna_core::data::{Post, PostBody, Timestamp};
    use fauna_core::identity::{ActorId, ActorKeypair};

    /// [`write_through_create_inner`](super::write_through_create_inner)'s own
    /// decode-then-extract composition, so this pin exercises exactly what
    /// production does: [`crate::db::posts::decode_stored_post`] (both stored
    /// shapes — native app posts are embed-as-bytes, bridge-ingested posts are
    /// bare) followed by [`write_through_text_of`]. Returns `None` for
    /// undecodable bodies and non-text bodies (media-only, video, structured).
    fn write_through_text(post_bytes: &[u8]) -> Option<(String, Vec<fauna_core::data::Facet>)> {
        crate::db::posts::decode_stored_post(post_bytes).and_then(write_through_text_of)
    }

    fn test_post(content: &str) -> Post {
        Post {
            author: ActorId([7u8; 32]),
            created_at: Timestamp(1_710_892_800_000_000),
            body: PostBody::Text {
                content: content.into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        }
    }

    #[test]
    fn native_embed_as_bytes_post_decodes() {
        let keypair = ActorKeypair::from_secret([9u8; 32]);
        let wire = fauna_core::encoding::sign_and_pack(&keypair, &test_post("hello bluesky"))
            .expect("sign_and_pack");
        let (text, facets) = write_through_text(&wire)
            .expect("embed-as-bytes (native app) post must decode for write-through");
        assert_eq!(text, "hello bluesky");
        assert!(facets.is_empty());
    }

    #[test]
    fn bare_post_still_decodes() {
        let bare =
            fauna_core::encoding::canonical_encode(&test_post("bare shape")).expect("encode");
        let (text, _) = write_through_text(&bare).expect("bare post must keep decoding");
        assert_eq!(text, "bare shape");
    }

    #[test]
    fn non_text_post_is_skipped() {
        let mut post = test_post("");
        post.body = PostBody::Media {
            items: vec![],
            alt_text: None,
        };
        let keypair = ActorKeypair::from_secret([9u8; 32]);
        let wire = fauna_core::encoding::sign_and_pack(&keypair, &post).expect("sign_and_pack");
        assert!(
            write_through_text(&wire).is_none(),
            "media-only posts don't cross-post"
        );
    }
}

#[cfg(test)]
mod write_through_delete_tests {
    //! Headless-testable invariants of the Bluesky write-through delete leg.
    //! The `deleteRecord` XRPC itself needs a real linked Bluesky account (the
    //! same external boundary the create-side `spawn_write_through` lives with —
    //! there is no fake ATProto PDS in the nest test harness); these tests pin
    //! the two decision boundaries that DON'T need it: skip-when-not-crossposted,
    //! and retain-the-mapping-when-the-remote-delete-fails.
    use super::*;
    use crate::db::CacheDb;

    const CROSSPOST_URI: &str = "at://did:plc:example/app.bsky.feed.post/abc123";

    async fn test_state() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        init_db(&db).await.unwrap();
        Arc::new(AppState::for_test(db))
    }

    /// A post that was never cross-posted has no `bluesky_posts` mapping, so the
    /// delete leg is a clean no-op — it MUST NOT error. The leg fires on every
    /// post delete, and the overwhelming majority of posts are never
    /// cross-posted; a spurious error here would be logged on every deletion.
    #[tokio::test]
    async fn no_mapping_is_a_clean_noop() {
        let state = test_state().await;
        let outcome = write_through_delete_inner(&state, [0x11u8; 32], [0x22u8; 32])
            .await
            .expect("no-mapping delete must not error");
        assert_eq!(outcome, WriteThroughDeleteOutcome::NotCrossposted);
    }

    /// One decodable text post as stored bytes (the bare canonical `Post`
    /// shape `decode_stored_post`'s fallback accepts).
    fn text_post_bytes() -> Vec<u8> {
        let post = fauna_core::data::Post {
            author: fauna_core::identity::ActorId([0x55u8; 32]),
            created_at: fauna_core::data::Timestamp::now(),
            body: fauna_core::data::PostBody::Text {
                content: "hello bluesky".to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        fauna_core::encoding::canonical_encode(&post).unwrap()
    }

    /// Row 103a's replay guard: a post whose mapping already names an external
    /// record is NOT re-published — the create leg converges on
    /// `AlreadyCrossposted` before any network step, and the mapping keeps
    /// pointing at the FIRST record (a second `create_record` with a
    /// server-assigned rkey would mint a duplicate, and the mapping overwrite
    /// would orphan the original so a later delete removes only the newest).
    /// Mutation-graded: deleting the guard reds exactly this test while the
    /// control below stays green.
    #[tokio::test]
    async fn an_already_crossposted_post_is_not_republished_on_replay() {
        let state = test_state().await;
        let actor = [0x66u8; 32];
        let post_id = [0x77u8; 32];
        let post_id_hex = hex::encode(post_id);

        {
            let conn = state.db.conn().await;
            // `set_write_through` is an UPDATE — the linked-account row must
            // exist first (production links the account before any mode set).
            db_helpers::upsert_linked_account(
                &conn,
                &hex::encode(actor),
                "did:plc:example",
                "example.bsky.social",
            )
            .unwrap();
            db_helpers::set_write_through(&conn, &hex::encode(actor), 1).unwrap();
            db_helpers::store_crosspost_mapping(
                &conn,
                &post_id_hex,
                CROSSPOST_URI,
                "did:plc:example",
                "bafy-crosspost",
            )
            .unwrap();
        }

        let outcome = write_through_create_inner(&state, actor, post_id, &text_post_bytes())
            .await
            .expect("the replay guard must resolve before any fallible network step");
        assert_eq!(outcome, WriteThroughCreateOutcome::AlreadyCrossposted);

        // The mapping still names the FIRST record.
        let conn = state.db.conn().await;
        let still = db_helpers::get_crosspost_uri(&conn, &post_id_hex).unwrap();
        assert_eq!(still.as_deref(), Some(CROSSPOST_URI));
    }

    /// A post whose envelope carries an archive `origin` — re-authored from a
    /// Facebook export — as stored bytes.
    fn archive_origin_post_bytes() -> Vec<u8> {
        let post = fauna_core::data::Post {
            author: fauna_core::identity::ActorId([0x55u8; 32]),
            created_at: fauna_core::data::Timestamp(1_600_000_000_000_000),
            body: fauna_core::data::PostBody::Text {
                content: "a post from 2020, imported today".to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: Some(fauna_core::data::PostOrigin {
                platform: fauna_core::source::FACEBOOK.into(),
                url: None,
            }),
        };
        fauna_core::encoding::canonical_encode(&post).unwrap()
    }

    /// A gated post as stored bytes: its plaintext body is the paywall teaser.
    fn gated_post_bytes() -> Vec<u8> {
        let post = fauna_core::data::Post {
            author: fauna_core::identity::ActorId([0x55u8; 32]),
            created_at: fauna_core::data::Timestamp::now(),
            body: fauna_core::data::PostBody::Text {
                content: "subscriber-only teaser".to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: Some(fauna_core::subscription::types::GatedInfo {
                encrypted_ref: fauna_cbor::Cid::from_digest_dag_cbor([0x33u8; 32]),
                key_access: fauna_core::subscription::types::KeyAccess::Broadcast {
                    key_blob_ref: fauna_cbor::Cid::from_digest_dag_cbor([0x44u8; 32]),
                },
                tier: "premium".into(),
                tier_rank: 1,
                seal_id: fauna_core::data::ContentHash::from_digest_raw([0x5e; 32]),
                attachment_refs: vec![],
            }),
            content_warning: None,
            origin: None,
        };
        fauna_core::encoding::canonical_encode(&post).unwrap()
    }

    /// Write-through mode 1 on a linked account — the state in which an
    /// ordinary text post proceeds to the agent step.
    async fn link_with_write_through(state: &Arc<AppState>, actor: [u8; 32]) {
        let conn = state.db.conn().await;
        db_helpers::upsert_linked_account(
            &conn,
            &hex::encode(actor),
            "did:plc:example",
            "example.bsky.social",
        )
        .unwrap();
        db_helpers::set_write_through(&conn, &hex::encode(actor), 1).unwrap();
    }

    /// Ruling 1 (`archive-import.md` § Compatibility → *Slice-3 rulings*) on
    /// the write-through leg: an archive-imported public post is served on
    /// Fauna and never cross-posted. The projection stream applies
    /// `PUBLIC_POST_SERVABLE`; this leg fires at create time on the decoded
    /// post, so it applies the predicate's create-time twin itself and
    /// converges before any network step — the control below proves the same
    /// account and mode carry a native post through to the agent step.
    #[tokio::test]
    async fn an_archive_origin_post_is_never_cross_posted() {
        let state = test_state().await;
        let actor = [0x66u8; 32];
        link_with_write_through(&state, actor).await;
        let outcome =
            write_through_create_inner(&state, actor, [0x79u8; 32], &archive_origin_post_bytes())
                .await
                .expect("refused before any fallible network step");
        assert_eq!(outcome, WriteThroughCreateOutcome::NotOffBoxPublishable);
        let conn = state.db.conn().await;
        assert_eq!(
            db_helpers::get_crosspost_uri(&conn, &hex::encode([0x79u8; 32])).unwrap(),
            None,
            "no mapping is written for a post that was never published"
        );
    }

    /// A gated post's plaintext body is its paywall teaser; cross-posting it
    /// would republish the teaser as an ordinary free post. The same
    /// create-time twin the ActivityPub push applies refuses it here.
    #[tokio::test]
    async fn a_gated_post_is_never_cross_posted() {
        let state = test_state().await;
        let actor = [0x66u8; 32];
        link_with_write_through(&state, actor).await;
        let outcome = write_through_create_inner(&state, actor, [0x7Au8; 32], &gated_post_bytes())
            .await
            .expect("refused before any fallible network step");
        assert_eq!(outcome, WriteThroughCreateOutcome::NotOffBoxPublishable);
    }

    /// The control for the guard: with NO mapping, the same inputs proceed
    /// past the guard and fail at the agent step (Bluesky unconfigured in this
    /// harness) — proving `AlreadyCrossposted` discriminates on the mapping,
    /// not on some earlier gate.
    #[tokio::test]
    async fn an_uncrossposted_post_proceeds_to_the_agent_step() {
        let state = test_state().await;
        let actor = [0x66u8; 32];
        {
            let conn = state.db.conn().await;
            db_helpers::upsert_linked_account(
                &conn,
                &hex::encode(actor),
                "did:plc:example",
                "example.bsky.social",
            )
            .unwrap();
            db_helpers::set_write_through(&conn, &hex::encode(actor), 1).unwrap();
        }
        let err = write_through_create_inner(&state, actor, [0x78u8; 32], &text_post_bytes())
            .await
            .expect_err("no OAuth agent => the create must fail at the agent step");
        assert!(
            format!("{err:#}").contains("agent"),
            "failure should be at the agent step, got: {err:#}"
        );
    }

    /// With a cross-post mapping present but Bluesky unconfigured (no OAuth), the
    /// delete fails at the agent step — and the mapping is RETAINED, never
    /// dropped, so a later retry can still chase the orphaned external record.
    #[tokio::test]
    async fn mapping_retained_when_remote_delete_fails() {
        let state = test_state().await;
        let post_id = [0x44u8; 32];
        let post_id_hex = hex::encode(post_id);

        {
            let conn = state.db.conn().await;
            db_helpers::store_crosspost_mapping(
                &conn,
                &post_id_hex,
                CROSSPOST_URI,
                "did:plc:example",
                "bafy-crosspost",
            )
            .unwrap();
        }

        let err = write_through_delete_inner(&state, [0x33u8; 32], post_id)
            .await
            .expect_err("no OAuth agent => the remote delete must fail");
        assert!(
            format!("{err:#}").contains("agent"),
            "failure should be at the agent step, got: {err:#}"
        );

        // Retention-on-failure: the mapping survives so a retry re-attempts.
        let conn = state.db.conn().await;
        let still = db_helpers::get_crosspost_uri(&conn, &post_id_hex).unwrap();
        assert_eq!(
            still.as_deref(),
            Some(CROSSPOST_URI),
            "the crosspost mapping must be retained when the remote delete fails"
        );
    }
}

#[cfg(test)]
mod oauth_public_url_tests {
    //! The derivation that replaced `--bluesky-public-url`. Pure, so the whole
    //! host-class matrix is pinned here rather than through a booted nest —
    //! the same shape `discovery_core::iroh_relay_public_url` is tested in.
    use super::oauth_public_url;

    #[test]
    fn a_public_domain_derives_an_https_client_id_base() {
        assert_eq!(
            oauth_public_url(Some("nest.example.com")),
            Some("https://nest.example.com".to_string()),
        );
    }

    #[test]
    fn a_domainless_box_derives_nothing() {
        assert_eq!(oauth_public_url(None), None);
    }

    #[test]
    fn localhost_and_its_subdomains_derive_nothing() {
        // The `handle_domain()` placeholder must never become a client_id:
        // Bluesky's authorization server has to fetch the metadata document by
        // name over public HTTPS.
        assert_eq!(oauth_public_url(Some("localhost")), None);
        assert_eq!(oauth_public_url(Some("nest.localhost")), None);
    }

    #[test]
    fn an_mdns_local_name_derives_nothing() {
        assert_eq!(oauth_public_url(Some("pi.local")), None);
    }

    #[test]
    fn ip_literals_derive_nothing_with_or_without_a_port() {
        // The port case is why this goes through `resolve_handle_domain` rather
        // than the bare `is_public_dns_name`: that predicate parses the whole
        // string as an `IpAddr`, so `127.0.0.1:8080` fails the parse and reads
        // as a DNS name. `resolve_handle_domain` splits the authority first.
        assert_eq!(oauth_public_url(Some("127.0.0.1")), None);
        assert_eq!(oauth_public_url(Some("127.0.0.1:13011")), None);
        assert_eq!(oauth_public_url(Some("192.168.1.10")), None);
        assert_eq!(oauth_public_url(Some("[::1]")), None);
    }

    #[test]
    fn an_empty_domain_derives_nothing() {
        assert_eq!(oauth_public_url(Some("")), None);
    }

    #[test]
    fn a_domain_carrying_userinfo_a_path_a_query_a_fragment_or_whitespace_derives_nothing() {
        // `is_public_dns_name` is a negative test, so each of these still
        // classified as "public" — the client_id must
        // never compose from a string that isn't a bare hostname.
        assert_eq!(
            oauth_public_url(Some("nest.example.com@attacker.example")),
            None
        );
        assert_eq!(oauth_public_url(Some("nest.example.com/../../evil")), None);
        assert_eq!(oauth_public_url(Some(" ")), None);
        assert_eq!(oauth_public_url(Some("nest.example.com?x=1")), None);
        assert_eq!(oauth_public_url(Some("nest.example.com#frag")), None);
        assert_eq!(oauth_public_url(Some("user:pass@127.0.0.1:8080")), None);
    }
}

#[cfg(test)]
mod schema_tests {
    /// The bridge's genesis is reconciled, not ALTERed: every additive column
    /// a long-lived database lacks comes back through `apply_schema`.
    #[test]
    fn apply_schema_reconciles_every_droppable_column() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        super::apply_schema(&conn).unwrap();
        let dropped = crate::bridge_schema::drop_additive_columns_and_reapply(
            &conn,
            fauna_bridge_atproto::db::CREATE_TABLES_SQL,
            super::apply_schema,
        );
        assert!(
            dropped >= 3,
            "the probe must actually drop columns ({dropped})"
        );
    }
}

#[cfg(test)]
mod write_through_reference_tests {
    //! The referenced-record rulings of the create leg (`bridges.md`
    //! § Cross-posting → *A post that references a Bluesky record*), pinned at
    //! the decision seam — no PDS needed — plus the executor's one observable
    //! without one: a mapped reference is RESOLVED (the agent step fails
    //! loudly), never silently dropped into a standalone post.
    use super::*;
    use crate::db::CacheDb;
    use fauna_core::data::{Post, PostBody, PostId, Reference, Timestamp};
    use fauna_core::identity::ActorId;

    const AUTHOR: [u8; 32] = [0x55u8; 32];
    const TARGET: [u8; 32] = [0x61u8; 32];
    const TARGET_URI: &str = "at://did:plc:someone/app.bsky.feed.post/tgt";

    async fn test_state() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        init_db(&db).await.unwrap();
        Arc::new(AppState::for_test(db))
    }

    fn post_bytes(body: PostBody, references: Vec<Reference>) -> Vec<u8> {
        let post = Post {
            author: ActorId(AUTHOR),
            created_at: Timestamp::now(),
            body,
            references,
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        fauna_core::encoding::canonical_encode(&post).unwrap()
    }

    fn text(content: &str) -> PostBody {
        PostBody::Text {
            content: content.into(),
            facets: vec![],
        }
    }

    fn reply_to(target: [u8; 32]) -> Vec<u8> {
        post_bytes(
            text("well said"),
            vec![Reference::Reply {
                post_id: PostId::from_digest_dag_cbor(target),
            }],
        )
    }

    fn quote_of(target: [u8; 32]) -> Vec<u8> {
        post_bytes(
            text("look at this"),
            vec![Reference::Quote {
                post_id: PostId::from_digest_dag_cbor(target),
            }],
        )
    }

    async fn link(state: &Arc<AppState>, mode: i64) {
        let conn = state.db.conn().await;
        db_helpers::upsert_linked_account(
            &conn,
            &hex::encode(AUTHOR),
            "did:plc:example",
            "example.bsky.social",
        )
        .unwrap();
        db_helpers::set_write_through(&conn, &hex::encode(AUTHOR), mode).unwrap();
    }

    /// The target as a stored row of the given `source` — `bluesky` for a post
    /// the consume side ingested, `fauna` for one of the author's own.
    async fn store_target(state: &Arc<AppState>, source: &str) {
        state
            .db
            .put_post_with_source(&TARGET, &post_bytes(text("the target"), vec![]), source)
            .await
            .unwrap();
    }

    async fn map_target(state: &Arc<AppState>) {
        let conn = state.db.conn().await;
        db_helpers::store_crosspost_mapping(
            &conn,
            &hex::encode(TARGET),
            TARGET_URI,
            "did:plc:someone",
            "bafy-target",
        )
        .unwrap();
    }

    fn reference_of(decision: WriteThroughDecision) -> PlannedReference {
        match decision {
            WriteThroughDecision::Post {
                reference: Some(r), ..
            } => r,
            other => panic!("expected a Post decision carrying a reference, got {other:?}"),
        }
    }

    /// **The reference is the intent.** Mode 0 (off) — and a reply to an
    /// ingested Bluesky post is admitted anyway: the user tapped reply on a
    /// Bluesky post, which is the very thing the toggle exists to ask for.
    #[tokio::test]
    async fn a_reply_to_a_bluesky_origin_post_is_admitted_under_mode_off() {
        let state = test_state().await;
        link(&state, 0).await;
        store_target(&state, "bluesky").await;
        map_target(&state).await;

        let decision = decide_write_through(&state, AUTHOR, [0x70u8; 32], &reply_to(TARGET))
            .await
            .unwrap();
        assert_eq!(
            reference_of(decision),
            PlannedReference {
                kind: ReferencedKind::Reply,
                target_hex: hex::encode(TARGET),
                mapped: true,
                bluesky_origin: true,
            }
        );
    }

    /// The quote twin: `Reference::Quote` on a Bluesky-origin post rides too.
    #[tokio::test]
    async fn a_quote_of_a_bluesky_origin_post_is_admitted_too() {
        let state = test_state().await;
        link(&state, 0).await;
        store_target(&state, "bluesky").await;
        map_target(&state).await;

        let decision = decide_write_through(&state, AUTHOR, [0x71u8; 32], &quote_of(TARGET))
            .await
            .unwrap();
        let r = reference_of(decision);
        assert_eq!(r.kind, ReferencedKind::Quote);
        assert!(r.mapped && r.bluesky_origin);
    }

    /// A self-thread — a reply under one's own cross-posted post — is NOT a
    /// Bluesky conversation: it follows the mode like any other post (off →
    /// skipped), and when the mode admits it, it carries the thread refs.
    #[tokio::test]
    async fn a_self_thread_reply_follows_the_mode() {
        let state = test_state().await;
        link(&state, 0).await;
        store_target(&state, "fauna").await;
        map_target(&state).await;

        let off = decide_write_through(&state, AUTHOR, [0x72u8; 32], &reply_to(TARGET))
            .await
            .unwrap();
        assert_eq!(
            off,
            WriteThroughDecision::Skip(WriteThroughCreateOutcome::Skipped),
            "mode 0 + a fauna-origin target: the mode decides, and it says no"
        );

        {
            let conn = state.db.conn().await;
            db_helpers::set_write_through(&conn, &hex::encode(AUTHOR), 1).unwrap();
        }
        let auto = decide_write_through(&state, AUTHOR, [0x72u8; 32], &reply_to(TARGET))
            .await
            .unwrap();
        let r = reference_of(auto);
        assert!(r.mapped, "the self-thread threads under the mapped parent");
        assert!(
            !r.bluesky_origin,
            "one's own post is not a Bluesky-origin post"
        );
    }

    /// A reference to a post that maps to no Bluesky record cross-posts
    /// standalone — the projection's rule: the target was never a Bluesky
    /// record, so there is nothing to thread under.
    #[tokio::test]
    async fn an_unmapped_reference_cross_posts_standalone() {
        let state = test_state().await;
        link(&state, 1).await;
        store_target(&state, "fauna").await;

        let decision = decide_write_through(&state, AUTHOR, [0x73u8; 32], &reply_to(TARGET))
            .await
            .unwrap();
        let r = reference_of(decision);
        assert!(!r.mapped && !r.bluesky_origin);
    }

    /// No consume-side link → nothing to write as; converges before decode.
    #[tokio::test]
    async fn an_unlinked_author_is_skipped_before_anything() {
        let state = test_state().await;
        store_target(&state, "bluesky").await;
        map_target(&state).await;

        let decision = decide_write_through(&state, AUTHOR, [0x74u8; 32], &reply_to(TARGET))
            .await
            .unwrap();
        assert_eq!(
            decision,
            WriteThroughDecision::Skip(WriteThroughCreateOutcome::NotLinked)
        );
    }

    /// The intent admits the post past the mode, not past the record rules:
    /// a media-only reply still has nothing this leg can cross-post.
    #[tokio::test]
    async fn a_bluesky_origin_reply_without_text_is_not_crosspostable() {
        let state = test_state().await;
        link(&state, 0).await;
        store_target(&state, "bluesky").await;
        map_target(&state).await;
        let media_reply = post_bytes(
            PostBody::Media {
                items: vec![],
                alt_text: None,
            },
            vec![Reference::Reply {
                post_id: PostId::from_digest_dag_cbor(TARGET),
            }],
        );

        let decision = decide_write_through(&state, AUTHOR, [0x75u8; 32], &media_reply)
            .await
            .unwrap();
        assert_eq!(
            decision,
            WriteThroughDecision::Skip(WriteThroughCreateOutcome::NotCrosspostable)
        );
    }

    /// The mode gate as a table. The reference overrides only the mode: mode
    /// 2's tag opt-in still binds a post that references nothing Bluesky.
    #[test]
    fn the_mode_gate_table() {
        // (mode, tag, references_bluesky_origin) → admitted
        for (mode, tag, origin, want) in [
            (0, false, false, false),
            (0, false, true, true),
            (1, false, false, true),
            (2, false, false, false),
            (2, true, false, true),
            (2, false, true, true),
            (7, false, false, false),
        ] {
            assert_eq!(
                crosspost_admitted(mode, tag, origin),
                want,
                "mode={mode} tag={tag} bluesky_origin={origin}"
            );
        }
    }

    /// The executor's half: a mapped reference is RESOLVED at the PDS before
    /// anything is built — with no OAuth client configured that step fails,
    /// and the failure is an error, never a silent standalone post. Mode 0 +
    /// a Bluesky-origin target, so this also proves the executor honours the
    /// decision's override rather than re-reading the mode.
    #[tokio::test]
    async fn the_executor_resolves_a_mapped_reference_rather_than_posting_standalone() {
        let state = test_state().await;
        link(&state, 0).await;
        store_target(&state, "bluesky").await;
        map_target(&state).await;

        let err = write_through_create_inner(&state, AUTHOR, [0x76u8; 32], &reply_to(TARGET))
            .await
            .expect_err("no OAuth client => resolving the parent must fail, loudly");
        assert!(
            err.to_string().contains("reply target resolution failed"),
            "the failure must be the resolution step, got: {err:#}"
        );
    }
}
