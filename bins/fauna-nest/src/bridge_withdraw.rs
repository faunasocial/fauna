//! Tear down a **bridge-translated** post's projection + segment record — the
//! destructive half every inbound-bridge retraction path shares.
//!
//! A content bridge that rests foreign content on the nest owes a removal arm
//! for it: `docs/goal/behavior/content-index.md` § Bridge content in the Search
//! corpus states that indexing rides each bridge's transit point *in lockstep*,
//! and `docs/goal/ui/feed.md` § State & data shape → *Post deletion* fixes the
//! teardown order a removal must follow. Every bridge answers that with the
//! same two writes, so they live here once rather than per bridge:
//!
//! * ActivityPub — `Undo{Like|Announce}` withdrawing a synthetic reaction, and
//!   a remote author's `Delete` of a Note we ingested.
//! * Nostr — a followed author's NIP-09 kind-5 deletion, and the NIP-40 expiry
//!   of a swept event (`nostr::inbound_lifecycle`).
//!
//! Mirrors `routes::delete_post_core`'s projection + segment teardown, **minus**
//! the counter reversal (no inbound bridge path calls
//! `record_reference_engagements`) and the propagation legs (foreign content is
//! never itself pushed or replicated onward).
//!
//! **Best-effort and non-fatal by design.** The caller has already performed the
//! authoritative bookkeeping — AP tombstones its map row, nostr deletes its —
//! so the item is unreachable regardless, and a projection hiccup must not fail
//! the activity or wedge a worker tick.
//!
//! **Callers own the authz decision.** This function destroys whatever post id
//! it is handed; deciding that the id belongs to *foreign* content, and that
//! the remote asking is entitled to retract it, happens at each call site. A
//! local account's own post is user data, and `fauna.posts.delete` (three
//! author checks) is the only verb that may destroy it.

use crate::db::CacheDb;

/// Withdraw one bridge-translated post: remove its feed-index projection, then
/// tombstone its `__post` segment record.
///
/// `bridge` / `natural_id` are log context only (`"nostr"` + the event id,
/// `"activitypub"` + the object URL) — they identify the item in the warning a
/// best-effort failure emits, and nothing branches on them.
pub async fn withdraw_translated_post(
    db: &CacheDb,
    post_id_hex: &str,
    bridge: &str,
    natural_id: &str,
    verb: &str,
) {
    let Ok(digest) = fauna_core::hex32::decode(post_id_hex) else {
        return;
    };
    if let Err(e) = db.delete_post_projection(&digest).await {
        tracing::warn!(
            bridge,
            natural_id,
            verb,
            "withdraw projection failed: {e:#}"
        );
    }
    match crate::segments::post::lookup_scope_by_post_id(db, &digest).await {
        Ok(Some((scope, seg_id))) => {
            let cid = fauna_cbor::Cid::from_digest_dag_cbor(digest);
            if let Err(e) = crate::segments::post::tombstone_by_cid(db, &scope, seg_id, &cid).await
            {
                tracing::warn!(bridge, natural_id, verb, "tombstone segment failed: {e:#}");
            }
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(
                bridge,
                natural_id,
                verb,
                "segment scope lookup failed: {e:#}"
            )
        }
    }
}
