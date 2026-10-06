//! WS-RPC handler for the `fauna.tips.*` namespace —
//! `docs/goal/behavior/monetization.md` § Tips.
//!
//! One kind: `tips.list`, the **post-addressed tip attribution read**. § Tips
//! ratifies that a tip names *(payee, post)* and that its consequence is
//! attribution/display/notification, never a grant — this is where that
//! consequence is delivered.
//!
//! **Why this is not `nostr.zaps.total`.** That kind exists, works, and is
//! keyed by *Nostr event id* — the mechanism's own identifier. A Fauna app
//! holding a `PostSummary` has a Fauna post id and nothing else, which is why
//! § Implementation status could record "no live display/attribution surface
//! on any client" while the totals already existed: they were not addressable
//! from a post. This kind is Fauna-addressed and mechanism-blind, so the
//! display legs on all 7 apps consume one read that never mentions NIP-57.
//!
//! **Nothing here re-judges trust.** Every row this reads was already
//! believed by the ingest gate (`nostr::zap_ingest`), per the ratified *at
//! ingest, at every ingress, never at read* discipline (§ Zap receipts — the
//! trust model). A filter here would be the first step back toward
//! re-applying the gate in every reader, which is exactly what that discipline
//! exists to prevent.

use std::time::Duration;

use fauna_protocol::tips::{TipItem, TipsListReply, TipsListRequest};
use fauna_protocol::{RpcError, decode_strict as decode};

use fauna_payments::tips::TipMechanism;

use crate::rpc_errors::encode_reply;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// One tip as the mechanism-specific source yields it, before it becomes a
/// wire [`TipItem`].
///
/// `allow(dead_code)`: every construction site sits behind a mechanism's own
/// feature (only `nostr` today), so a default-featured build genuinely never
/// builds one — while the handler that reads these fields is compiled
/// unconditionally, because the *kind* is unconditional (see
/// [`collect_tip_rows`]). Removing the allow would mean either gating the
/// mechanism-blind kind behind a mechanism, or letting a zero-mechanism build
/// go red; the allow is the honest third answer, and it stops applying the
/// moment a second mechanism lands unfeatured.
#[allow(dead_code)]
struct TipRow {
    sender_actor_id: Option<String>,
    sender_ref: Option<String>,
    amount_msats: Option<i64>,
    mechanism: TipMechanism,
    received_at: i64,
}

/// Gather every tip on `post_id` from every compiled-in mechanism.
///
/// **This kind is registered unconditionally while its only current source is
/// feature-gated, and that asymmetry is deliberate.** `fauna.tips.list` is
/// the mechanism-*blind* surface; a nest built without `--features nostr`
/// still answers it, with an honest empty total. Gating the kind itself
/// instead would make a client unable to tell "this nest has no tip
/// mechanism compiled in" from "this nest is too old to know the kind" —
/// both would be `unknown_kind` — and would mean the mechanism-independent
/// read blinks out of existence whenever the one mechanism is off, which is
/// the opposite of what § Tips asks for. Adding a second mechanism adds an
/// arm here and changes nothing else, anywhere.
#[allow(unused_variables)]
fn collect_tip_rows(conn: &rusqlite::Connection, post_id: &str) -> anyhow::Result<Vec<TipRow>> {
    #[allow(unused_mut)]
    let mut rows: Vec<TipRow> = Vec::new();

    // Mechanism: NIP-57 zap receipts. Its tables live behind the same feature
    // as the rest of the Nostr bridge, so both the query and its schema are
    // absent from a default-featured nest.
    #[cfg(feature = "nostr")]
    {
        rows.extend(
            crate::nostr::db::list_tips_for_fauna_post(conn, post_id)?
                .into_iter()
                .map(|r| TipRow {
                    sender_actor_id: r.sender_actor_id,
                    sender_ref: r.sender_pubkey,
                    amount_msats: r.amount_msats,
                    mechanism: TipMechanism::NostrZap,
                    received_at: r.received_at,
                }),
        );
    }

    // Newest-first across mechanisms, with a stable tiebreak so a window
    // boundary cannot straddle two same-second tips inconsistently. Each
    // source already returns its own rows sorted; this is what keeps the
    // merged order right once there is more than one of them.
    rows.sort_by(|a, b| {
        b.received_at
            .cmp(&a.received_at)
            .then_with(|| a.sender_ref.cmp(&b.sender_ref))
    });
    Ok(rows)
}

fn malformed(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::malformed_ns("tips", reason)
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns("tips", e)
}

/// `fauna.tips.list` — every tip on one post, newest-first, plus the
/// **unbounded** totals.
///
/// Keyed on `post_id` alone. That is enough to identify the post
/// (`post_id = blake3(body)` is globally unique) and it carries the same
/// anti-enumeration property `fauna.subscriptions.post_unlock.get` relies on:
/// the id is unguessable without having seen the post, so possession of it is
/// evidence of legitimate access. USER-class, ordinary dispatch limits — an
/// indexed join needs no bespoke throttle.
///
/// **The window is capped; the totals are not.** `tips` carries at most the
/// request's effective limit, while `total_msats` / `tip_count` are computed
/// over every tip on the post. An unpaged list over a table a third party can
/// grow is the storm shape this codebase has already had to fix once (the
/// backup custody/generation list kinds); the totals are what a post card
/// renders, and no display needs every tipper at once.
fn tips_list_handler() -> RpcHandler {
    Box::new(|state, _caller_id, payload| {
        Box::pin(async move {
            let req: TipsListRequest = decode(&payload).map_err(malformed)?;
            if !fauna_core::hex32::is_hex64(&req.post_id) {
                return Err(malformed("post_id must be a 32-byte post id in hex"));
            }
            let limit = req.effective_limit() as usize;

            let conn = state.db.conn().await;
            let rows = collect_tip_rows(&conn, &req.post_id).map_err(internal)?;
            drop(conn);

            // Totals over EVERY row, before the window is applied. Summed in
            // Rust rather than by a second aggregate query so the totals and
            // the window provably describe the same row set — two queries
            // could straddle a concurrent insert and report a count the items
            // contradict.
            let tip_count = rows.len() as i64;
            let total_msats: i64 = rows
                .iter()
                // `None` is "the receipt carried no parseable amount", a real
                // state — skipped, never coerced to 0, so an amount-less tip
                // still counts above without pretending to be worth nothing.
                .filter_map(|r| r.amount_msats)
                .fold(0i64, |acc, m| acc.saturating_add(m));

            let has_more = rows.len() > limit;
            let tips = rows
                .into_iter()
                .take(limit)
                .map(|r| TipItem {
                    sender: r
                        .sender_actor_id
                        .as_deref()
                        .and_then(|hex| fauna_core::identity::ActorId::from_hex(hex).ok()),
                    sender_ref: r.sender_ref,
                    amount_msats: r.amount_msats,
                    // The mechanism token comes from the shared enum, never a
                    // string literal here: the wire value and the shared-Rust
                    // value cannot drift if there is only one of them.
                    mechanism: r.mechanism.as_str().to_string(),
                    received_at: r.received_at,
                    extra: Default::default(),
                })
                .collect();

            encode_reply(&TipsListReply {
                total_msats,
                tip_count,
                tips,
                has_more,
                extra: Default::default(),
            })
        })
    })
}

/// Register all `fauna.tips.*` WS-RPC handlers on the builder.
pub fn register_tip_handlers(b: &mut RpcRouterBuilder) {
    // `forbid_replay: false` — a pure aggregate point read. It writes
    // nothing, so a replay cannot double-apply anything, and its reply
    // survives a replay too (modulo tips arriving in between, which is
    // ordinary read freshness). Mirrored in `KindRegistry::register_tips_kinds`;
    // the parity gate `router_and_kind_registry_agree_on_every_kind` holds the
    // two in lockstep.
    b.add(
        "fauna.tips.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: tips_list_handler(),
        },
    );
}
