//! The Nostr advertisement of a sold post — the zap purchase leg's ingress
//! (`docs/goal/behavior/monetization.md` § The Nostr advertisement of a sold
//! post).
//!
//! A sold post is gated by construction, and a gated post is never part of the
//! public off-box class, so without this act no zap receipt could ever name a
//! real sold post (the purchase leg, `zap_ingest::apply_zap_purchase`, resolves
//! only outbound-mapped events). The act is **per post, explicit and
//! reversible**: `fauna.nostr.advertise.set` writes the author's standing
//! intent (`content_meta.nostr_advertised_at`) and kicks the materializer,
//! which signs the teaser as a kind-1 note carrying the selling tier's asking
//! price, maps it `outbound` and pushes it to the account's publish relays;
//! `fauna.nostr.advertise.clear` withdraws it. The flag is declarative — the
//! materializer converges on it, so a crash between the act and the
//! publication is healed by the next sweep.
//!
//! **Lifecycle — the price in the wild must never misstate the offer** (§
//! *Lifecycle*): a changed asking price retracts the published note and the
//! materializer re-signs it with the new tag; a cleared price — or a deleted
//! selling tier — retracts and clears the flag; `clear` retracts and drops the
//! map rows, so a later zap on a cached copy resolves to no subject and is a
//! tip. Every retraction is the post-delete leg's kind 5
//! ([`crate::nostr::propagate_post_delete`]), which names every mapped event
//! of the post and drops its map rows. `expose_content` off spares the
//! advertisement (`store::delete_derived_for_pubkey`). A note whose `r` tag no
//! longer names the post's public web URL — web publication or the author's
//! site origin moved — is retracted and re-signed the same way
//! ([`reconcile_advertised_urls`], § *Lifecycle* (6)).
//!
//! **Gating** (`dynamic-features.md` § Charter members, the `zaps` row): `set`
//! is the `zaps.advertise` surface and is compiled with the `zaps` member;
//! everything else here is compiled always — `clear` is de-escalation (ruling
//! (i)), and an excised nest keeps serving and withdrawing what a full build
//! authored.

use std::sync::Arc;
use std::time::Duration;

use fauna_protocol::nostr::{AdvertiseClearReply, AdvertiseClearRequest};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::bridge_method_allowlist::require_permission_default as require_permission;
use crate::nostr::{db, key_crypto, relay_endpoint, store, sync_worker};
use crate::routes::AppState;
use crate::rpc_errors::{encode_reply, internal, malformed};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Publish freshly signed advertisement events: live to the nest relay's
/// subscribers and, through the sync worker, to each account's publish relays
/// — whatever `auto_publish` says, because the act is the publish intent
/// (§ *Reach*). An explicitly empty relay list publishes to the nest's relay
/// alone (the worker iterates zero relays), the kind-5 precedent.
///
/// Never call this from inside the sync worker's own tick: it awaits a send on
/// that worker's channel (the worker publishes its sweep's advertisements
/// itself, `SyncWorker::materialize_exposed`).
pub async fn push_advertisements(state: &AppState, ads: Vec<sync_worker::OutboundEvent>) {
    for outbound in ads {
        if let Ok(wire) = serde_json::to_string(&outbound.event) {
            let _ = state.nostr.relay_tx.send(relay_endpoint::NostrRelayEvent {
                event_json: wire,
                author_pubkey: outbound.event.pubkey.clone(),
            });
        }
        if let Err(e) = state.nostr.sync_tx.send(outbound).await {
            tracing::warn!("nostr advertise: enqueue the publish failed: {e}");
        }
    }
}

/// Materialize one account now (both classes when it exposes content, the
/// advertised class otherwise) and publish what it signed — the immediate kick
/// beside the periodic sweep. A missing or undecryptable key materializes
/// nothing; the act's own refusal already covers the caller-visible case.
pub async fn kick_account(state: &AppState, actor_hex: &str) {
    let account = {
        let conn = state.db.conn().await;
        db::get_account(&conn, actor_hex)
    };
    let Ok(Some(account)) = account else { return };
    let Some(enc) = account.encrypted_privkey.as_ref() else {
        return;
    };
    let nest_key = state.nest_identity.signing_key.to_bytes();
    let Some(kp) = key_crypto::decrypt_nostr_privkey(&nest_key, enc)
        .ok()
        .and_then(|s| fauna_bridge_nostr::signing::Keypair::from_secret_bytes(s).ok())
    else {
        tracing::warn!("nostr advertise: decrypt nsec failed for {actor_hex}");
        return;
    };
    match store::materialize_account(
        &state.db,
        &state.post_segments,
        actor_hex,
        &kp,
        store::MaterializeClasses::for_exposure(account.expose_content),
        &state.web_serving_domain(),
    )
    .await
    {
        Ok(m) => push_advertisements(state, m.advertisements).await,
        Err(e) => tracing::warn!("nostr advertise: materialize {actor_hex}: {e}"),
    }
}

/// The post's author and whether it is advertised right now, or `None` when
/// no post row has this id.
fn post_author_and_flag(
    conn: &rusqlite::Connection,
    post_id: &[u8; 32],
) -> anyhow::Result<Option<([u8; 32], bool)>> {
    use rusqlite::OptionalExtension;
    let row: Option<(Vec<u8>, Option<i64>)> = conn
        .query_row(
            "SELECT c.author, cm.nostr_advertised_at
               FROM content c LEFT JOIN content_meta cm ON cm.content_id = c.id
              WHERE c.id = ?1 AND c.schema LIKE 'post/%'",
            [post_id.as_slice()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(author, flag)| {
        let author: [u8; 32] = author.try_into().ok()?;
        Some((author, flag.is_some()))
    }))
}

/// Clear the flag; `true` iff it was set.
fn clear_flag(conn: &rusqlite::Connection, post_id: &[u8; 32]) -> anyhow::Result<bool> {
    let n = conn.execute(
        "UPDATE content_meta SET nostr_advertised_at = NULL
          WHERE content_id = ?1 AND nostr_advertised_at IS NOT NULL",
        [post_id.as_slice()],
    )?;
    Ok(n > 0)
}

/// Parse a wire post id; a malformed one reads as a post that does not exist.
fn parse_post_id(hex_id: &str) -> Option<[u8; 32]> {
    let mut id = [0u8; 32];
    hex::decode_to_slice(hex_id.trim(), &mut id).ok()?;
    Some(id)
}

fn not_found() -> RpcError {
    crate::rpc_errors::not_found_ns("nostr", "no such post of yours")
}

/// Withdraw an advertised post: clear the flag and retract every published
/// note (kind 5, nest relay + publish relays, map rows dropped). Shared by
/// `clear`, a cleared price and a deleted selling tier. Idempotent: a post
/// that is not advertised is left alone. Returns whether it was advertised.
pub async fn withdraw(
    state: &Arc<AppState>,
    author: &[u8; 32],
    post_id: &[u8; 32],
) -> anyhow::Result<bool> {
    let was = {
        let conn = state.db.conn().await;
        clear_flag(&conn, post_id)?
    };
    if was {
        crate::nostr::propagate_post_delete(state, author, post_id).await?;
    }
    Ok(was)
}

/// The asking-price lifecycle (§ *Lifecycle* (1) and (2)), called by
/// `tiers.update`, `tiers.clear_field` and `tiers.delete` once their write has
/// landed. `unlocks_post` is the tier's designation; `before` / `after` are its
/// asking price before and after the write (`after = None` also for a deleted
/// tier). Nothing happens unless the post is advertised and the price moved:
///
/// - **cleared** (or the tier is gone) → withdraw: an advertisement without a
///   price cannot stand; the author re-prices, then re-advertises.
/// - **changed** → retract the published note and re-materialize at once with
///   the new tag (the sweep is the safety net); the zap comparison itself is
///   always against the live tier price.
///
/// Best-effort like every retraction: an error is logged, never failing the
/// tier write that already landed.
pub async fn on_tier_price_changed(
    state: &Arc<AppState>,
    author: &[u8; 32],
    unlocks_post: Option<&str>,
    before: Option<&fauna_protocol::subscriptions::TierAskingPrice>,
    after: Option<&fauna_protocol::subscriptions::TierAskingPrice>,
) {
    let Some(post_id) = unlocks_post.and_then(parse_post_id) else {
        return;
    };
    let same = match (before, after) {
        (Some(b), Some(a)) => b.value == a.value && b.unit == a.unit,
        (None, None) => true,
        _ => false,
    };
    if same {
        return;
    }
    let advertised = {
        let conn = state.db.conn().await;
        post_author_and_flag(&conn, &post_id)
    };
    match advertised {
        Ok(Some((a, true))) if a == *author => {}
        Ok(_) => return,
        Err(e) => {
            tracing::warn!("nostr advertise: read the flag on a price edit: {e}");
            return;
        }
    }
    let outcome = if after.is_none() {
        withdraw(state, author, &post_id).await.map(|_| ())
    } else {
        match crate::nostr::propagate_post_delete(state, author, &post_id).await {
            Ok(()) => {
                kick_account(state, &hex::encode(author)).await;
                Ok(())
            }
            Err(e) => Err(e),
        }
    };
    if let Err(e) = outcome {
        tracing::warn!(
            "nostr advertise: retract {} after a price edit: {e}",
            hex::encode(post_id)
        );
    }
}

// ── the `r` tag lifecycle (§ *Lifecycle* (6)) ─────────────────────

/// The `r` tag a stored event carries, or `None` when it has none.
fn stored_r_tag(conn: &rusqlite::Connection, event_id: &str) -> anyhow::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    let raw: Option<String> = conn
        .query_row(
            "SELECT raw_json FROM nostr_events WHERE id = ?1",
            [event_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(raw) = raw else { return Ok(None) };
    let event: fauna_bridge_nostr::types::Event = serde_json::from_str(&raw)?;
    Ok(event
        .tags
        .iter()
        .rev()
        .find(|t| t.name() == Some("r"))
        .and_then(|t| t.value())
        .map(str::to_string))
}

/// Re-sign every published advertisement whose `r` tag no longer names the
/// post's current public URL (`monetization.md` § The Nostr advertisement of a
/// sold post → *Lifecycle* (6)) — `author` narrows the pass to one account,
/// `None` covers the nest. The URL moves with the post's web publication
/// (`fauna.web.publish.set` / `unset`, a changed slug) and with its author's
/// site origin (a custom domain going active or away, the subdomain toggle, a
/// handle rename, the nest's web apex claimed); every one of those reaches
/// this one comparison, so no write path has to remember the advertisement.
///
/// A stale note is retracted (the price edit's kind 5,
/// [`crate::nostr::propagate_post_delete`], which drops its map rows) and its
/// account kicked, so it is re-materialized at once with the current URL — or
/// none; the sweep is the safety net. Returns how many were retracted.
///
/// Never call this from inside the sync worker's own tick arm: the retraction
/// and the kick await sends on that worker's channel (spawn it, as the
/// takedown reconcile is).
pub async fn reconcile_advertised_urls(
    state: &Arc<AppState>,
    author: Option<&[u8; 32]>,
) -> anyhow::Result<usize> {
    let mapped: Vec<(String, String, String)> = {
        let conn = state.db.conn().await;
        let mut stmt = conn.prepare(
            "SELECT m.fauna_post_id, lower(hex(c.author)), m.nostr_event_id
               FROM nostr_event_map m
               JOIN content c ON lower(hex(c.id)) = m.fauna_post_id
               JOIN content_meta cm ON cm.content_id = c.id
              WHERE m.direction = 'outbound'
                AND cm.nostr_advertised_at IS NOT NULL
                AND (?1 IS NULL OR c.author = ?1)",
        )?;
        let rows = stmt.query_map([author.map(|a| a.to_vec())], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if mapped.is_empty() {
        return Ok(0);
    }
    let serving_domain = state.web_serving_domain();
    let mut retracted: std::collections::BTreeSet<String> = Default::default();
    let mut kick: std::collections::BTreeSet<String> = Default::default();
    for (post_hex, author_hex, event_id) in mapped {
        if retracted.contains(&post_hex) {
            continue;
        }
        let (Some(post), Some(owner)) = (parse_post_id(&post_hex), parse_post_id(&author_hex))
        else {
            continue;
        };
        let carried = {
            let conn = state.db.conn().await;
            stored_r_tag(&conn, &event_id)?
        };
        let current = crate::web_content::public_url::public_post_url(
            &state.db,
            &owner,
            &post,
            &serving_domain,
        )
        .await?;
        if carried == current {
            continue;
        }
        match crate::nostr::propagate_post_delete(state, &owner, &post).await {
            Ok(()) => {
                retracted.insert(post_hex);
                kick.insert(author_hex);
            }
            Err(e) => tracing::warn!("nostr advertise: retract {post_hex} for a moved URL: {e}"),
        }
    }
    for author_hex in kick {
        kick_account(state, &author_hex).await;
    }
    Ok(retracted.len())
}

/// The web-publication hook: `fauna.web.publish.set` / `unset` call it once
/// their write has landed, so an advertised post's note gains, changes or
/// drops its `r` tag at once rather than at the next sweep. Best-effort like
/// every retraction: an error is logged, never failing the publish write.
pub async fn on_web_publish_changed(state: &Arc<AppState>, author: &[u8; 32]) {
    if let Err(e) = reconcile_advertised_urls(state, Some(author)).await {
        tracing::warn!("nostr advertise: reconcile the r tag after a web publish change: {e}");
    }
}

// ── fauna.nostr.advertise.set ──────────────────────────────────────

/// **Gate surface `zaps.advertise`** — the payee's one send-side act on the
/// zaps plane, a solicitation of Lightning payment (one operation, no
/// counterparty, no magnitude). A `payments` deny reaches it along the subset
/// edge, and a `zaps`-only deny (the Damus shape) denies it too.
///
/// Refusals, typed and fail-closed (§ *The act*): a post that is not the
/// caller's (`not_found`, indistinguishable from a missing post); one no tier
/// sells (`fauna.nostr.not_sold`, `CacheDb::get_tier_selling_post` — both
/// directions of the binding); a selling tier with no machine price
/// (`fauna.nostr.no_asking_price`); and an account with no deposited key (the
/// bridging gate's refusals, `not_linked` / `no_custodial_key`).
/// `expose_content` is neither required nor consulted.
#[cfg(feature = "zaps")]
fn set_handler() -> RpcHandler {
    use fauna_protocol::nostr::{AdvertiseSetReply, AdvertiseSetRequest};
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.nostr.advertise.set").await?;
            let req: AdvertiseSetRequest = decode(&payload).map_err(malformed)?;
            let actor_hex = hex::encode(actor_id);

            crate::feature_gate::gate(
                &state,
                &actor_id,
                &fauna_core::feature_gate::GateOp {
                    feature: fauna_core::feature_gate::GatedFeature::Zaps,
                    surface: fauna_core::feature_gate::SURFACE_ZAPS_ADVERTISE,
                    new_counterparties: 0,
                    magnitude: 0,
                },
            )
            .await?;

            let post_id = parse_post_id(&req.post_id).ok_or_else(not_found)?;
            let post_hex = hex::encode(post_id);
            {
                let conn = state.db.conn().await;
                match post_author_and_flag(&conn, &post_id).map_err(internal)? {
                    Some((author, _)) if author == actor_id => {}
                    _ => return Err(not_found()),
                }
            }
            let tier = state
                .db
                .get_tier_selling_post(&actor_id, &post_hex)
                .await
                .map_err(internal)?
                .ok_or_else(|| {
                    crate::rpc_errors::typed_refusal(
                        RpcError::CODE_NOSTR_NOT_SOLD,
                        "no tier sells this post",
                    )
                })?;
            if tier.asking_price().is_none() {
                return Err(crate::rpc_errors::typed_refusal(
                    RpcError::CODE_NOSTR_NO_ASKING_PRICE,
                    "the tier selling this post has no asking price",
                ));
            }
            {
                let conn = state.db.conn().await;
                let account = db::get_account(&conn, &actor_hex).map_err(internal)?;
                match account {
                    None => {
                        return Err(crate::rpc_errors::typed_refusal(
                            RpcError::CODE_NOSTR_NOT_LINKED,
                            "no Nostr key is linked",
                        ));
                    }
                    Some(a) if a.encrypted_privkey.is_none() => {
                        return Err(crate::rpc_errors::typed_refusal(
                            RpcError::CODE_NOSTR_NO_CUSTODIAL_KEY,
                            "the nest holds no key to sign the advertisement with",
                        ));
                    }
                    Some(_) => {}
                }
            }

            // The flag: the first act's time is kept, so a replay is a no-op.
            let advertised_at: i64 = {
                let conn = state.db.conn().await;
                conn.execute(
                    "UPDATE content_meta
                        SET nostr_advertised_at = COALESCE(nostr_advertised_at, ?2)
                      WHERE content_id = ?1",
                    rusqlite::params![post_id.as_slice(), crate::db::now_epoch_secs()],
                )
                .map_err(internal)?;
                conn.query_row(
                    "SELECT nostr_advertised_at FROM content_meta WHERE content_id = ?1",
                    [post_id.as_slice()],
                    |r| r.get(0),
                )
                .map_err(internal)?
            };

            // The immediate kick; the sweep heals a crash between here and it.
            kick_account(&state, &actor_hex).await;

            encode_reply(&AdvertiseSetReply {
                advertised_at: advertised_at.max(0) as u64,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.nostr.advertise.clear ────────────────────────────────────

/// Stop advertising (§ *Lifecycle* (3)): clear the flag, retract the note
/// (nest relay + publish relays, best-effort as every kind 5) and drop the
/// outbound map rows, so a zap arriving later against a cached copy is a tip —
/// the author withdrew the offer before the payment. De-escalation: never
/// gated, compiled always. A post that is not the caller's is `not_found`; one
/// that is not advertised answers `cleared: false`.
fn clear_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.nostr.advertise.clear").await?;
            let req: AdvertiseClearRequest = decode(&payload).map_err(malformed)?;
            let post_id = parse_post_id(&req.post_id).ok_or_else(not_found)?;
            {
                let conn = state.db.conn().await;
                match post_author_and_flag(&conn, &post_id).map_err(internal)? {
                    Some((author, _)) if author == actor_id => {}
                    _ => return Err(not_found()),
                }
            }
            let cleared = withdraw(&state, &actor_id, &post_id)
                .await
                .map_err(internal)?;
            encode_reply(&AdvertiseClearReply {
                cleared,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ────────────────────────────────────────

/// Both are light row writes at 5 s and replay-safe (`set` keeps the first
/// act's time, `clear` of a cleared post is a no-op), matching the registry
/// (`KindRegistry::register_nostr_advertise_kinds`).
pub fn register_nostr_advertise_handlers(b: &mut RpcRouterBuilder) {
    let light = || Duration::from_secs(5);
    #[cfg(feature = "zaps")]
    b.add(
        "fauna.nostr.advertise.set",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: light(),
            handler: set_handler(),
        },
    );
    b.add(
        "fauna.nostr.advertise.clear",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: light(),
            handler: clear_handler(),
        },
    );
}
