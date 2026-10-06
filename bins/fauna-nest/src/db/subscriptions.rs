//! Subscription tier, subscriber, period key, and key blob methods.

use super::{CacheDb, now_epoch_secs};
use super::{SubscribeRequestRow, SubscriberRow, SubscriptionTierRow};
use anyhow::{Context, Result};
use fauna_core::encoding::{EmbedAsBytes, canonical_decode, decode_signed_bytes};
use fauna_core::subscription::types::KeyBlob;
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashSet;

/// The subscribers a tier's **current KeyBlob actually wraps** — the nest's
/// only truthful answer to "can this subscriber read this tier?".
///
/// This exists because **readability is not SQL-expressible**. Only
/// `subscribers` and `subscribe_requests` carry a `subscriber_id` column; the
/// wrap set lives in `KeyBlob.entries[].subscriber` inside `current_key_blobs.
/// blob_data`, an opaque dag-cbor `EmbedAsBytes` over signed bytes. So the
/// enqueue's "does this subscriber already hold a readable wrap?" guard
/// (`monetization.md` § Implementation status (2d)) cannot live inside its `INSERT … SELECT`; it decodes here
/// and filters in Rust instead.
///
/// **Empty means "cannot prove readability", never "definitely unreadable"** —
/// a missing blob row, the `DEFAULT x''` placeholder, or an undecodable blob
/// all return the empty set, so the caller enqueues a mint. That is the
/// fail-safe direction: the worst case is one redundant `payment_entitled`
/// row, which the author's client drains into a *correct* blob; the opposite
/// default would silently re-strand exactly the readers this guard exists to
/// heal.
///
/// Cost is **one decode per tier**, not per subscriber — callers hoist it out
/// of their subscriber loop. That is the same order the system already pays on
/// this blob (every approval rewrites it wholesale), so this adds no new
/// scaling class.
pub(super) fn wrapped_subscribers(
    conn: &Connection,
    author_id: &[u8; 32],
    tier_name: &str,
) -> HashSet<[u8; 32]> {
    let stored: Option<Vec<u8>> = conn
        .query_row(
            "SELECT blob_data FROM current_key_blobs
              WHERE author_id = ?1 AND tier_name = ?2
              ORDER BY key_version DESC LIMIT 1",
            rusqlite::params![author_id.as_slice(), tier_name],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let Some(stored) = stored else {
        return HashSet::new();
    };
    // Same two-step the roster checks use: the stored payload is dag-cbor
    // `EmbedAsBytes` (envelope + canonical bytes), and the inner KeyBlob is
    // read via `decode_signed_bytes` per sign-over-CID.
    let Ok(wire) = canonical_decode::<EmbedAsBytes>(&stored) else {
        return HashSet::new();
    };
    let Ok(blob) = decode_signed_bytes::<KeyBlob>(&wire.bytes) else {
        tracing::warn!(
            tier = tier_name,
            "current KeyBlob does not decode — treating every subscriber as unwrapped"
        );
        return HashSet::new();
    };
    blob.entries.iter().map(|e| e.subscriber.0).collect()
}

impl CacheDb {
    // ── Subscription tiers ──────────────────────────────────────────

    pub async fn create_subscription_tier(
        &self,
        author_id: &[u8; 32],
        name: &str,
        rank: i64,
        description: Option<&str>,
        price_hint: Option<&str>,
        payment_url: Option<&str>,
        auto_approve: bool,
        unlocks_post: Option<&str>,
        asking_price: Option<&fauna_protocol::subscriptions::TierAskingPrice>,
        hidden: bool,
    ) -> Result<()> {
        let now = now_epoch_secs();
        // Both halves or neither — the pair is the value, and a half-set row
        // is not a price this model can compare. Destructured from one
        // `Option` so the two columns cannot diverge here by construction;
        // `SubscriptionTierRow::asking_price` is the reading twin of this rule.
        let (price_value, price_unit) = match asking_price {
            Some(p) => (Some(p.value as i64), Some(p.unit.as_str())),
            None => (None, None),
        };
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO subscription_tiers (author_id, name, rank, description, price_hint, payment_url, auto_approve, created_at, unlocks_post, asking_price_value, asking_price_unit, hidden)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                author_id.as_slice(), name, rank, description, price_hint, payment_url,
                auto_approve as i64, now, unlocks_post, price_value, price_unit, hidden as i64
            ],
        ).context("create subscription tier")?;
        Ok(())
    }

    pub async fn list_subscription_tiers(
        &self,
        author_id: &[u8; 32],
    ) -> Result<Vec<SubscriptionTierRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT author_id, name, rank, description, price_hint, payment_url, auto_approve, created_at, unlocks_post, asking_price_value, asking_price_unit, hidden
             FROM subscription_tiers WHERE author_id = ?1 ORDER BY rank ASC"
        ).context("prepare list tiers")?;
        let rows = stmt
            .query_map(rusqlite::params![author_id.as_slice()], |row| {
                Ok(SubscriptionTierRow {
                    author_id: row.get(0)?,
                    name: row.get(1)?,
                    rank: row.get(2)?,
                    description: row.get(3)?,
                    price_hint: row.get(4)?,
                    payment_url: row.get(5)?,
                    auto_approve: row.get::<_, i64>(6)? != 0,
                    created_at: row.get(7)?,
                    unlocks_post: row.get(8)?,
                    asking_price_value: row.get(9)?,
                    asking_price_unit: row.get(10)?,
                    hidden: row.get::<_, i64>(11)? != 0,
                })
            })
            .context("list subscription tiers")?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("collect tier rows")
    }

    pub async fn get_subscription_tier(
        &self,
        author_id: &[u8; 32],
        name: &str,
    ) -> Result<Option<SubscriptionTierRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT author_id, name, rank, description, price_hint, payment_url, auto_approve, created_at, unlocks_post, asking_price_value, asking_price_unit, hidden
             FROM subscription_tiers WHERE author_id = ?1 AND name = ?2"
        ).context("prepare get tier")?;
        let mut rows = stmt
            .query(rusqlite::params![author_id.as_slice(), name])
            .context("get tier")?;
        match rows.next().context("tier row next")? {
            Some(row) => Ok(Some(SubscriptionTierRow {
                author_id: row.get(0)?,
                name: row.get(1)?,
                rank: row.get(2)?,
                description: row.get(3)?,
                price_hint: row.get(4)?,
                payment_url: row.get(5)?,
                auto_approve: row.get::<_, i64>(6)? != 0,
                created_at: row.get(7)?,
                unlocks_post: row.get(8)?,
                asking_price_value: row.get(9)?,
                asking_price_unit: row.get(10)?,
                hidden: row.get::<_, i64>(11)? != 0,
            })),
            None => Ok(None),
        }
    }

    /// The author's tier that **sells** `post_id` (hex), if any — the one
    /// resolver both the buyer's price read (`fauna.subscriptions
    /// .post_unlock.get`) and the zap purchase leg go through.
    ///
    /// **Both directions of the binding must agree**, and neither alone is
    /// authority (`monetization.md` § Per-post pay-to-unlock):
    ///
    /// * `subscription_tiers.unlocks_post` is the author's create-time-immutable
    ///   *designation* — but it is a one-way claim the nest cannot check when it
    ///   is made (`tiers.create` validates its **format** only, deliberately:
    ///   the post does not exist yet, since its id is `blake3` of a body that
    ///   names this tier). It is also **not unique** — `PRIMARY KEY
    ///   (author_id, name)` is the only constraint — so two tiers may name one
    ///   post, and any tier may name a post it has nothing to do with.
    /// * `content_meta.gated_tier` is the *authority*: it is projected by
    ///   `extract_post_metadata` from `Post.gated.tier` inside the body the post
    ///   id content-addresses, so it cannot be claimed after the fact — forging
    ///   it means forging the post id.
    ///
    /// So a tier sells a post iff the post is gated **to that tier** and that
    /// tier designates **that post**. Answering `None` therefore covers, all as
    /// ordinary states: an ungated post (`gated_tier IS NULL` ⇒ the subquery is
    /// NULL and matches nothing), a post gated to an ordinary subscription tier
    /// (that tier designates no post — the § *at-least-this-post* rule means
    /// subscription content is not individually for sale), a tier naming a post
    /// it does not gate, a post with no `content_meta` row at all (reachable —
    /// every post-write site swallows the `write_post_index` error, see
    /// `db/public_servability.rs`), and a malformed id.
    ///
    /// The old `ORDER BY rank ASC, name ASC LIMIT 1` tie-break is gone with the
    /// ambiguity it papered over: a post is gated to exactly one tier and a
    /// tier name is unique per author, so at most one row can match.
    pub async fn get_tier_selling_post(
        &self,
        author_id: &[u8; 32],
        post_id_hex: &str,
    ) -> Result<Option<SubscriptionTierRow>> {
        let mut post_id = [0u8; 32];
        if hex::decode_to_slice(post_id_hex, &mut post_id).is_err() {
            return Ok(None);
        }
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT author_id, name, rank, description, price_hint, payment_url, auto_approve, created_at, unlocks_post, asking_price_value, asking_price_unit, hidden
             FROM subscription_tiers
             WHERE author_id = ?1
               AND unlocks_post = ?2
               AND name = (SELECT cm.gated_tier FROM content_meta cm WHERE cm.content_id = ?3)"
        ).context("prepare get tier selling post")?;
        let mut rows = stmt
            .query(rusqlite::params![
                author_id.as_slice(),
                post_id_hex,
                post_id.as_slice()
            ])
            .context("get tier selling post")?;
        match rows.next().context("unlock tier row next")? {
            Some(row) => Ok(Some(SubscriptionTierRow {
                author_id: row.get(0)?,
                name: row.get(1)?,
                rank: row.get(2)?,
                description: row.get(3)?,
                price_hint: row.get(4)?,
                payment_url: row.get(5)?,
                auto_approve: row.get::<_, i64>(6)? != 0,
                created_at: row.get(7)?,
                unlocks_post: row.get(8)?,
                asking_price_value: row.get(9)?,
                asking_price_unit: row.get(10)?,
                hidden: row.get::<_, i64>(11)? != 0,
            })),
            None => Ok(None),
        }
    }

    pub async fn update_subscription_tier(
        &self,
        author_id: &[u8; 32],
        name: &str,
        description: Option<&str>,
        price_hint: Option<&str>,
        payment_url: Option<&str>,
        auto_approve: bool,
        asking_price: Option<&fauna_protocol::subscriptions::TierAskingPrice>,
    ) -> Result<bool> {
        // The caller has already merged "keep current" — every argument here
        // is the value the row must end up holding. Writing the pair
        // unconditionally (rather than conditionally appending a SET clause)
        // keeps this one statement, so the two columns can never be updated
        // apart. Re-pricing is legal, unlike re-pointing `unlocks_post`: a new
        // price binds future events only and never touches an entitlement
        // already granted (`monetization.md` § The asking price — Editability).
        let (price_value, price_unit) = match asking_price {
            Some(p) => (Some(p.value as i64), Some(p.unit.as_str())),
            None => (None, None),
        };
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "UPDATE subscription_tiers SET description = ?1, price_hint = ?2, payment_url = ?3, auto_approve = ?4,
                    asking_price_value = ?5, asking_price_unit = ?6
             WHERE author_id = ?7 AND name = ?8",
            rusqlite::params![
                description, price_hint, payment_url, auto_approve as i64,
                price_value, price_unit,
                author_id.as_slice(), name
            ],
        ).context("update subscription tier")?;
        Ok(changed > 0)
    }

    pub async fn delete_subscription_tier(&self, author_id: &[u8; 32], name: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        // Check for active subscribers first
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM subscribers WHERE author_id = ?1 AND tier_name = ?2",
                rusqlite::params![author_id.as_slice(), name],
                |row| row.get(0),
            )
            .context("count subscribers")?;
        if count > 0 {
            anyhow::bail!("tier has active subscribers");
        }
        // Delete related rows first (FK constraints)
        conn.execute(
            "DELETE FROM current_key_blobs WHERE author_id = ?1 AND tier_name = ?2",
            rusqlite::params![author_id.as_slice(), name],
        )
        .context("delete key blobs")?;
        conn.execute(
            "DELETE FROM subscribe_requests WHERE author_id = ?1 AND tier_name = ?2",
            rusqlite::params![author_id.as_slice(), name],
        )
        .context("delete subscribe requests")?;
        let changed = conn
            .execute(
                "DELETE FROM subscription_tiers WHERE author_id = ?1 AND name = ?2",
                rusqlite::params![author_id.as_slice(), name],
            )
            .context("delete subscription tier")?;
        Ok(changed > 0)
    }

    /// Grant-time rank fan-out (`monetization.md:19` + `:126`): a rank-R
    /// entitlement to `granted_tier` just landed for `subscriber_id`, so
    /// enqueue the author-client mint — a `payment_entitled` subscribe request
    /// the drain pump approves without creator judgment — for every tier of
    /// this author with rank ≤ R that the subscriber does not already
    /// (unexpiredly) hold.
    ///
    /// **Five boundary rules, each load-bearing** (`monetization.md:126`
    /// (a)–(e) — keep the numbering in step with that list):
    ///
    /// 1. **Designated→designated is the only blocked edge.** A grant of an
    ///    undesignated tier fans out to everything at rank ≤ R (that is the
    ///    ratified cascade: "a rank-R subscriber is enrolled in every tier
    ///    ≤ R"); a grant of an `unlocks_post`-designated tier fans out only to
    ///    *undesignated* tiers, so a buyer of a sold post becomes a follower
    ///    (`:126`) while one cheap post still never unlocks every sold post.
    ///    This was formerly an early return on any designated grant, which made `:126`'s rank-0 cascade sentence false at code level.
    /// 2. **The granted tier is excluded.** Its own grant belongs to the
    ///    caller, which has already written the row carrying the real paid
    ///    window; re-selecting it here would drive the `ON CONFLICT` arm below
    ///    and blank that window to `NULL`, silently turning a time-boxed
    ///    purchase perpetual.
    /// 3. **Perpetual (`valid_until = NULL`).** The paid window is stamped on
    ///    the requested tier only: lapse of a paid window must not un-follow,
    ///    and an unlock tier's
    ///    content is fixed at creation (`monetization.md` § Per-post
    ///    pay-to-unlock — *Perpetual by default*).
    ///
    /// 4. **Suppression keys on READABILITY, not on the `subscribers` row**.
    ///    A `subscribers` row is the **social edge** — it must land
    ///    immediately, and following stays an immediate follow. A *wrap* is
    ///    what makes the tier readable, and the two are not the same fact: a
    ///    roster row can stand with no wrap (the retired nest-side cascade
    ///    wrote exactly that), and keying suppression on the row let that row
    ///    suppress the very enqueue that would have delivered the wrap —
    ///    enrolled-but-unreadable, permanently.
    ///    Suppression therefore requires **both** halves: an unexpired
    ///    entitlement *and* a delivered wrap, asked **per subscriber** (see
    ///    [`wrapped_subscribers`] — a tier having *a* blob says nothing about
    ///    who is in it). Keying on readability is structural: it heals every
    ///    provenance of a wrap-less row, **including rows deployed nests have
    ///    already written**, which is the reason it beat the alternative of
    ///    making the cascade enqueue alongside its wrap-less write. Every tier
    ///    is client-minted and the `followers` self-heal covers the one tier
    ///    no `tiers.create` minted, so it cannot loop on a tier the author's
    ///    client can never mint.
    ///
    /// 5. **A `hidden` tier is never a cascade target**: ruling 4 is "not offered and
    ///    not subscribable", and unlike rules 1-4 this is a property of the
    ///    target tier itself, not a consequence of rank or designation — a
    ///    hidden tier can sit at any rank, so it cannot be derived from the
    ///    other rules and must be checked here directly.
    ///
    /// Idempotent: an existing pending row upgrades in place (the
    /// [`Self::upsert_payment_entitled_request`] shape). Returns the number of
    /// tiers enqueued or upgraded.
    pub async fn enqueue_unlock_fanout(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        granted_tier: &str,
    ) -> Result<usize> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let granted: Option<(i64, Option<String>)> = conn
            .query_row(
                "SELECT rank, unlocks_post FROM subscription_tiers
                 WHERE author_id = ?1 AND name = ?2",
                rusqlite::params![author_id.as_slice(), granted_tier],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("read granted tier for fan-out")?;
        let Some((rank, designation)) = granted else {
            return Ok(0);
        };
        // Rule 1: a designated grant may only reach undesignated targets.
        let undesignated_targets_only = i64::from(designation.is_some());
        // Rules 1-2 + the rank bound are pure SQL; `holds_unexpired` carries
        // the entitlement half of the suppression, and readability — which SQL
        // cannot see (see `wrapped_subscribers`) — is applied below.
        let mut stmt = conn
            .prepare(
                "SELECT d.name,
                        EXISTS (
                          SELECT 1 FROM subscribers s
                           WHERE s.author_id = d.author_id
                             AND s.subscriber_id = ?2
                             AND s.tier_name = d.name
                             AND (s.valid_until IS NULL OR s.valid_until > ?4))
                   FROM subscription_tiers d
                  WHERE d.author_id = ?1
                    AND d.rank <= ?3
                    AND d.name <> ?5
                    AND d.hidden = 0
                    AND (?6 = 0 OR d.unlocks_post IS NULL)",
            )
            .context("prepare unlock fan-out candidates")?;
        let candidates: Vec<(String, bool)> = stmt
            .query_map(
                rusqlite::params![
                    author_id.as_slice(),
                    subscriber_id.as_slice(),
                    rank,
                    now,
                    granted_tier,
                    undesignated_targets_only
                ],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)),
            )
            .context("query unlock fan-out candidates")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect unlock fan-out candidates")?;
        drop(stmt);

        let mut changed = 0usize;
        for (name, holds_unexpired) in candidates {
            // Only an entitled subscriber can be suppressed, and only when the
            // wrap is really there. The common path (a subscriber who holds
            // nothing yet) never decodes a blob.
            if holds_unexpired
                && wrapped_subscribers(&conn, author_id, &name).contains(subscriber_id)
            {
                continue;
            }
            changed += conn
                .execute(
                    "INSERT INTO subscribe_requests
                         (author_id, subscriber_id, tier_name, created_at, kind,
                          payment_entitled, valid_until)
                     VALUES (?1, ?2, ?3, ?4, 'subscribe', 1, NULL)
                     ON CONFLICT(author_id, subscriber_id, tier_name, kind) DO UPDATE SET
                         payment_entitled = 1,
                         valid_until = NULL",
                    rusqlite::params![author_id.as_slice(), subscriber_id.as_slice(), name, now],
                )
                .context("enqueue unlock fan-out")?;
        }
        Ok(changed)
    }

    /// Creation-time half of the rank fan-out: a designated tier was just
    /// created, so enqueue the author-client mint for every **existing**
    /// unexpired subscriber of this author's undesignated tiers at or above
    /// the new tier's rank. This is what makes `monetization.md:126` true
    /// for subscribers who predate the sale — "included in every paid
    /// subscription", not only future ones. No-op for an undesignated name.
    ///
    /// **Checks `hidden` on the target directly**: rule (f)
    /// (`monetization.md:201`) says every cascade door excludes a `hidden`
    /// tier regardless of rank, and this door's own doc comment used to argue
    /// the target could never be hidden "by construction" because
    /// `tiers_create_handler` refuses `hidden` combined with `unlocks_post`
    /// (`subscription_handlers.rs:1086-1094`) — true of that one caller, but
    /// a guard the caller must remember is not the same as this function
    /// enforcing its own precondition
    /// (`docs/goal/architecture/security/review-method.md` § *A guard whose
    /// failure is silent lives inside the consumer, not the callers*). The
    /// refusal stays as one layer of defense; the `AND hidden = 0` below is
    /// the in-consumer check rule (f) requires. `r` (the tier a subscriber
    /// already holds, which is what makes them eligible) is not filtered on
    /// `hidden` either — but by the same ruling nobody legitimately holds a
    /// `subscribers` row on a hidden tier in the first place, once rule (f)
    /// at [`Self::enqueue_unlock_fanout`] and its mirrors close every door
    /// that could have granted one.
    pub async fn enqueue_unlock_fanout_for_new_tier(
        &self,
        author_id: &[u8; 32],
        tier_name: &str,
    ) -> Result<usize> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "INSERT INTO subscribe_requests
                     (author_id, subscriber_id, tier_name, created_at, kind,
                      payment_entitled, valid_until)
                 SELECT ?1, s.subscriber_id, ?2, ?3, 'subscribe', 1, NULL
                   FROM subscription_tiers r
                   JOIN subscribers s
                     ON s.author_id = r.author_id AND s.tier_name = r.name
                  WHERE r.author_id = ?1
                    AND r.unlocks_post IS NULL
                    AND r.rank >= (SELECT rank FROM subscription_tiers
                                    WHERE author_id = ?1 AND name = ?2
                                      AND unlocks_post IS NOT NULL
                                      AND hidden = 0)
                    AND (s.valid_until IS NULL OR s.valid_until > ?3)
                    AND NOT EXISTS (
                         SELECT 1 FROM subscribers x
                          WHERE x.author_id = ?1
                            AND x.subscriber_id = s.subscriber_id
                            AND x.tier_name = ?2
                            AND (x.valid_until IS NULL OR x.valid_until > ?3))
                  GROUP BY s.subscriber_id
                 ON CONFLICT(author_id, subscriber_id, tier_name, kind) DO UPDATE SET
                     payment_entitled = 1,
                     valid_until = NULL",
                rusqlite::params![author_id.as_slice(), tier_name, now],
            )
            .context("enqueue unlock fan-out for new tier")?;
        Ok(changed)
    }

    // ── Subscribers ───────────────────────────────────────────────────

    pub async fn add_subscriber(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        tier_name: &str,
        mlkem_encaps_key: Option<&[u8]>,
    ) -> Result<()> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        // Upsert on the (author, subscriber, tier) PK: an already-present row keeps
        // its original `approved_at` but UPGRADES its `mlkem_encaps_key` when this
        // call carries a freshly published ek (post-quantum surface-B classical →
        // hybrid upgrade — e.g. the auto-approve cascade re-touching a lower tier the
        // subscriber already holds while they publish an ek for the first time).
        // `COALESCE(excluded, existing)` only overwrites with a non-NULL ek, so a
        // later classical re-subscribe never clobbers a previously published ek.
        // `valid_until` is RESET to NULL on conflict: a fresh grant is indefinite
        // unless a payment window is stamped right after (payment_core /
        // requests.approve) — without the reset, re-approving an expired paid
        // subscriber would leave the stale expiry in place and the "approval"
        // would not entitle.
        conn.execute(
            "INSERT INTO subscribers \
             (author_id, subscriber_id, tier_name, approved_at, mlkem_encaps_key)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(author_id, subscriber_id, tier_name) DO UPDATE SET
                 mlkem_encaps_key = COALESCE(excluded.mlkem_encaps_key, subscribers.mlkem_encaps_key),
                 valid_until = NULL",
            rusqlite::params![
                author_id.as_slice(),
                subscriber_id.as_slice(),
                tier_name,
                now,
                mlkem_encaps_key,
            ],
        )
        .context("add subscriber")?;
        Ok(())
    }

    /// Upgrade a subscriber's published ML-KEM ek across **all** their tier rows
    /// under `author` (the ek is identity-derived — one value for the whole
    /// subscriber, the same key the author wraps for every tier). Used when an
    /// already-subscribed subscriber re-subscribes carrying a freshly published ek
    /// (post-quantum surface-B classical → hybrid upgrade); the idempotency
    /// early-return in `subscribe_handler` would otherwise drop it, stranding the
    /// subscriber on classical (HNDL-exposed) entries forever (SUB-1). Never called
    /// with a NULL ek, so it only ever upgrades. Returns the number of rows updated
    /// (0 if the subscriber holds no tiers under this author). The author's next
    /// rotation re-wraps an `Xwing` entry off the stored ek.
    pub async fn update_subscriber_mlkem_ek(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        mlkem_encaps_key: &[u8],
    ) -> Result<u64> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE subscribers SET mlkem_encaps_key = ?3 \
                 WHERE author_id = ?1 AND subscriber_id = ?2",
                rusqlite::params![
                    author_id.as_slice(),
                    subscriber_id.as_slice(),
                    mlkem_encaps_key,
                ],
            )
            .context("update subscriber mlkem ek")?;
        Ok(changed as u64)
    }

    pub async fn remove_subscriber(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        tier_name: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "DELETE FROM subscribers WHERE author_id = ?1 AND subscriber_id = ?2 AND tier_name = ?3",
            rusqlite::params![author_id.as_slice(), subscriber_id.as_slice(), tier_name],
        ).context("remove subscriber")?;
        Ok(changed > 0)
    }

    /// Remove a subscriber from all tiers at or above a given rank.
    /// Returns the names of tiers the subscriber was removed from.
    pub async fn remove_subscriber_from_tiers_at_or_above(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        min_rank: i64,
    ) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        // Find affected tier names first
        let mut stmt = conn
            .prepare(
                "SELECT s.tier_name FROM subscribers s
             JOIN subscription_tiers t ON s.author_id = t.author_id AND s.tier_name = t.name
             WHERE s.author_id = ?1 AND s.subscriber_id = ?2 AND t.rank >= ?3",
            )
            .context("prepare tiers at or above")?;
        let affected: Vec<String> = stmt
            .query_map(
                rusqlite::params![author_id.as_slice(), subscriber_id.as_slice(), min_rank],
                |row| row.get(0),
            )
            .context("query tiers at or above")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect affected tier names")?;

        if !affected.is_empty() {
            conn.execute(
                "DELETE FROM subscribers WHERE author_id = ?1 AND subscriber_id = ?2
                 AND tier_name IN (
                     SELECT t.name FROM subscription_tiers t
                     WHERE t.author_id = ?1 AND t.rank >= ?3
                 )",
                rusqlite::params![author_id.as_slice(), subscriber_id.as_slice(), min_rank],
            )
            .context("delete subscribers at or above rank")?;
        }
        Ok(affected)
    }

    /// Entitlement gate: does the subscriber currently hold this tier? A row
    /// whose paid `valid_until` window has lapsed no longer entitles (expiry
    /// self-heals — monetization.md § Pillar 3); the row itself stays until
    /// the author's client prunes it (roster reads like [`Self::list_subscribers`]
    /// are deliberately expiry-blind — they enumerate the crypto-material set,
    /// not the entitlement).
    pub async fn is_subscriber(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        tier_name: &str,
    ) -> Result<bool> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM subscribers WHERE author_id = ?1 AND subscriber_id = ?2 AND tier_name = ?3
             AND (valid_until IS NULL OR valid_until > ?4)",
            rusqlite::params![author_id.as_slice(), subscriber_id.as_slice(), tier_name, now],
            |row| row.get(0),
        ).context("is subscriber")?;
        Ok(count > 0)
    }

    /// The paid window on a subscriber row (`None` = no row or no expiry).
    pub async fn get_subscriber_valid_until(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        tier_name: &str,
    ) -> Result<Option<i64>> {
        let conn = self.conn.lock().await;
        let row: Option<Option<i64>> = conn
            .query_row(
                "SELECT valid_until FROM subscribers
                 WHERE author_id = ?1 AND subscriber_id = ?2 AND tier_name = ?3",
                rusqlite::params![author_id.as_slice(), subscriber_id.as_slice(), tier_name],
                |row| row.get(0),
            )
            .optional()
            .context("get subscriber valid_until")?;
        Ok(row.flatten())
    }

    /// Stamp/extend/void the paid window on an existing subscriber row
    /// (payment renewal extends it; refund voids it by setting it to `now`;
    /// `None` clears expiry entirely). Returns `false` if no row exists.
    pub async fn set_subscriber_valid_until(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        tier_name: &str,
        valid_until: Option<i64>,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE subscribers SET valid_until = ?4
                 WHERE author_id = ?1 AND subscriber_id = ?2 AND tier_name = ?3",
                rusqlite::params![
                    author_id.as_slice(),
                    subscriber_id.as_slice(),
                    tier_name,
                    valid_until
                ],
            )
            .context("set subscriber valid_until")?;
        Ok(changed > 0)
    }

    pub async fn list_subscribers(
        &self,
        author_id: &[u8; 32],
        tier_name: &str,
    ) -> Result<Vec<SubscriberRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT subscriber_id, tier_name, approved_at, mlkem_encaps_key
             FROM subscribers WHERE author_id = ?1 AND tier_name = ?2",
            )
            .context("prepare list subscribers")?;
        let rows = stmt
            .query_map(rusqlite::params![author_id.as_slice(), tier_name], |row| {
                Ok(SubscriberRow {
                    subscriber_id: row.get(0)?,
                    tier_name: row.get(1)?,
                    approved_at: row.get(2)?,
                    mlkem_encaps_key: row.get(3)?,
                })
            })
            .context("list subscribers")?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("collect subscriber rows")
    }

    /// Entitlement gate (see [`Self::is_subscriber`]): tiers the subscriber
    /// currently holds — a lapsed paid window drops the tier from this set.
    pub async fn get_subscribed_tiers(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
    ) -> Result<Vec<String>> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT tier_name FROM subscribers WHERE author_id = ?1 AND subscriber_id = ?2
                 AND (valid_until IS NULL OR valid_until > ?3)",
            )
            .context("prepare get subscribed tiers")?;
        let rows = stmt
            .query_map(
                rusqlite::params![author_id.as_slice(), subscriber_id.as_slice(), now],
                |row| row.get(0),
            )
            .context("get subscribed tiers")?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("collect subscribed tier names")
    }

    /// The calling subscriber's own subscriptions across **every** creator —
    /// the consumer-side enumeration powering `fauna.subscriptions.mine.list`.
    /// Merges approved `subscribers` rows (`active`) with not-yet-approved
    /// `subscribe_requests` of kind `subscribe` (`pending`), deduped so a stale
    /// pending request for an already-active `(author, tier)` is dropped (active
    /// wins). Active rows first, then pending; each group ordered by
    /// `(author_id, tier_name)` for a stable consumer-page order.
    pub async fn list_my_subscriptions(
        &self,
        subscriber_id: &[u8; 32],
    ) -> Result<Vec<super::MySubscriptionRow>> {
        let conn = self.conn.lock().await;

        let now = now_epoch_secs();
        let mut active_stmt = conn
            .prepare(
                "SELECT author_id, tier_name, approved_at FROM subscribers
                 WHERE subscriber_id = ?1 AND (valid_until IS NULL OR valid_until > ?2)
                 ORDER BY author_id, tier_name",
            )
            .context("prepare list my active subscriptions")?;
        let mut out: Vec<super::MySubscriptionRow> = active_stmt
            .query_map(rusqlite::params![subscriber_id.as_slice(), now], |row| {
                Ok(super::MySubscriptionRow {
                    author_id: row.get(0)?,
                    tier_name: row.get(1)?,
                    status: "active".to_string(),
                    since: row.get(2)?,
                })
            })
            .context("list my active subscriptions")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect my active subscription rows")?;

        // Dedup key for the active set — a pending request for an
        // already-active `(author, tier)` is a stale leftover, not a 2nd row.
        let active_keys: std::collections::HashSet<(Vec<u8>, String)> = out
            .iter()
            .map(|r| (r.author_id.clone(), r.tier_name.clone()))
            .collect();

        let mut pending_stmt = conn
            .prepare(
                "SELECT author_id, tier_name, created_at FROM subscribe_requests
                 WHERE subscriber_id = ?1 AND kind = 'subscribe'
                 ORDER BY author_id, tier_name",
            )
            .context("prepare list my pending subscriptions")?;
        let pending: Vec<super::MySubscriptionRow> = pending_stmt
            .query_map(rusqlite::params![subscriber_id.as_slice()], |row| {
                Ok(super::MySubscriptionRow {
                    author_id: row.get(0)?,
                    tier_name: row.get(1)?,
                    status: "pending".to_string(),
                    since: row.get(2)?,
                })
            })
            .context("list my pending subscriptions")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect my pending subscription rows")?;

        for r in pending {
            if !active_keys.contains(&(r.author_id.clone(), r.tier_name.clone())) {
                out.push(r);
            }
        }
        Ok(out)
    }

    // ── Subscribe requests ────────────────────────────────────────────

    /// Enqueue a subscribe/unsubscribe intent for the author's client to
    /// commit, returning the row id.
    ///
    /// **Idempotent by construction**, and load-bearingly so: the row is keyed
    /// `UNIQUE(author_id, subscriber_id, tier_name, kind)`, and a repeat
    /// returns the *existing* row's id instead of raising the constraint. That
    /// is what `forbid_replay: false` asserts for
    /// `fauna.subscriptions.unsubscribe` (`transport.md` § Idempotency and
    /// reconnect-with-resume — the nest's idempotency cache is per-connection
    /// and cannot dedup an auto-retry, so the handler runs again for real).
    /// Its client-minted arm leaves the subscriber in the roster until the
    /// author commits (`monetization.md:61`), so a retry re-enters with
    /// byte-identical input and must land on the row the first attempt created;
    /// before this was an upsert it surfaced as `fauna.protocol.internal` for
    /// an unsubscribe that had already succeeded
    /// (`unsubscribe_replay_returns_the_same_queued_row`).
    ///
    /// `DO NOTHING`, not `DO UPDATE`: a re-subscribe carrying a freshly
    /// published ML-KEM ek is upgraded by [`Self::update_subscribe_request_mlkem_ek`]
    /// on `subscribe_handler`'s pending-request branch, which is reached
    /// *before* this call — so writing here could only clobber it with the
    /// older value. Same upsert-then-read shape as
    /// [`Self::upsert_payment_entitled_request`].
    pub async fn insert_subscribe_request(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        tier_name: &str,
        kind: &str,
        mlkem_encaps_key: Option<&[u8]>,
    ) -> Result<i64> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO subscribe_requests \
             (author_id, subscriber_id, tier_name, created_at, kind, mlkem_encaps_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(author_id, subscriber_id, tier_name, kind) DO NOTHING",
            rusqlite::params![
                author_id.as_slice(),
                subscriber_id.as_slice(),
                tier_name,
                now,
                kind,
                mlkem_encaps_key
            ],
        )
        .context("insert subscribe request")?;
        // Read the id back rather than trusting `last_insert_rowid()`: on the
        // `DO NOTHING` path no row was inserted, so that counter still names
        // whatever this connection last wrote.
        let id: i64 = conn
            .query_row(
                "SELECT id FROM subscribe_requests \
                 WHERE author_id = ?1 AND subscriber_id = ?2 AND tier_name = ?3 AND kind = ?4",
                rusqlite::params![
                    author_id.as_slice(),
                    subscriber_id.as_slice(),
                    tier_name,
                    kind
                ],
                |row| row.get(0),
            )
            .context("read enqueued subscribe request id")?;
        Ok(id)
    }

    /// Upgrade the published ML-KEM ek on a subscriber's pending `subscribe`
    /// request rows under `author` (post-quantum surface-B classical → hybrid
    /// upgrade for an enqueued-but-not-yet-approved subscriber). Mirrors
    /// [`Self::update_subscriber_mlkem_ek`] for the enqueue path: the
    /// `has_pending_subscribe_request` idempotency early-return in
    /// `subscribe_handler` would otherwise drop a freshly published ek, so a
    /// classically-enqueued subscriber would be approved classical even though they
    /// later published an ek. Only `kind='subscribe'` rows carry an ek (unsubscribe
    /// rows are always NULL). Returns rows updated.
    pub async fn update_subscribe_request_mlkem_ek(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        mlkem_encaps_key: &[u8],
    ) -> Result<u64> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE subscribe_requests SET mlkem_encaps_key = ?3 \
                 WHERE author_id = ?1 AND subscriber_id = ?2 AND kind = 'subscribe'",
                rusqlite::params![
                    author_id.as_slice(),
                    subscriber_id.as_slice(),
                    mlkem_encaps_key,
                ],
            )
            .context("update subscribe request mlkem ek")?;
        Ok(changed as u64)
    }

    pub async fn list_subscribe_requests(
        &self,
        author_id: &[u8; 32],
    ) -> Result<Vec<SubscribeRequestRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, subscriber_id, tier_name, created_at, kind, mlkem_encaps_key,
                    payment_entitled, valid_until
             FROM subscribe_requests WHERE author_id = ?1 ORDER BY created_at ASC",
            )
            .context("prepare list subscribe requests")?;
        let rows = stmt
            .query_map(rusqlite::params![author_id.as_slice()], |row| {
                Ok(SubscribeRequestRow {
                    id: row.get(0)?,
                    subscriber_id: row.get(1)?,
                    tier_name: row.get(2)?,
                    created_at: row.get(3)?,
                    kind: row.get(4)?,
                    mlkem_encaps_key: row.get(5)?,
                    payment_entitled: row.get::<_, i64>(6)? != 0,
                    valid_until: row.get(7)?,
                })
            })
            .context("list subscribe requests")?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("collect subscribe request rows")
    }

    pub async fn get_subscribe_request(
        &self,
        request_id: i64,
    ) -> Result<Option<(Vec<u8>, Vec<u8>, String, String, Option<Vec<u8>>)>> {
        let conn = self.conn.lock().await;
        let result = conn
            .query_row(
                "SELECT author_id, subscriber_id, tier_name, kind, mlkem_encaps_key \
                 FROM subscribe_requests WHERE id = ?1",
                rusqlite::params![request_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .context("get subscribe request")?;
        Ok(result)
    }

    pub async fn delete_subscribe_request(&self, request_id: i64) -> Result<bool> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "DELETE FROM subscribe_requests WHERE id = ?1",
                rusqlite::params![request_id],
            )
            .context("delete subscribe request")?;
        Ok(changed > 0)
    }

    pub async fn has_pending_subscribe_request(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        tier_name: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM subscribe_requests \
             WHERE author_id = ?1 AND subscriber_id = ?2 AND tier_name = ?3 AND kind = 'subscribe'",
                rusqlite::params![author_id.as_slice(), subscriber_id.as_slice(), tier_name],
                |row| row.get(0),
            )
            .context("has pending subscribe request")?;
        Ok(count > 0)
    }

    /// Enqueue (or mark) a payment-entitled subscribe request — the verified-
    /// payment grant source (monetization.md § Pillar 3). If the buyer already
    /// has a pending `subscribe` row for this tier, it is upgraded in place
    /// (`payment_entitled = 1`, window refreshed); otherwise a new row is
    /// inserted, exactly like a subscriber-initiated enqueue (the author's
    /// drain pump approves it on next pass). Returns the request row id.
    pub async fn upsert_payment_entitled_request(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        tier_name: &str,
        valid_until: Option<i64>,
    ) -> Result<i64> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO subscribe_requests
             (author_id, subscriber_id, tier_name, created_at, kind,
              payment_entitled, valid_until)
             VALUES (?1, ?2, ?3, ?4, 'subscribe', 1, ?5)
             ON CONFLICT(author_id, subscriber_id, tier_name, kind) DO UPDATE SET
                 payment_entitled = 1,
                 valid_until = excluded.valid_until",
            rusqlite::params![
                author_id.as_slice(),
                subscriber_id.as_slice(),
                tier_name,
                now,
                valid_until
            ],
        )
        .context("upsert payment entitled request")?;
        let id: i64 = conn
            .query_row(
                "SELECT id FROM subscribe_requests
                 WHERE author_id = ?1 AND subscriber_id = ?2 AND tier_name = ?3 AND kind = 'subscribe'",
                rusqlite::params![author_id.as_slice(), subscriber_id.as_slice(), tier_name],
                |row| row.get(0),
            )
            .context("read back payment entitled request id")?;
        Ok(id)
    }

    /// The subscribe queue's deletion leg for its SECOND person — the
    /// requesting reader, whom the registry (keyed on `author_id`) cannot
    /// reach. Runs inside the purge walk, under its lock
    /// (`actor_tables.rs::purge_orphaned_actor_rows`).
    ///
    /// A deleted reader's pending `subscribe` rows go, paid or not: each is
    /// only an instruction for the author's drain to grant a tier, a grant
    /// after the account is gone mints an entitlement to an identity refused
    /// everywhere, and the row carries the reader's ML-KEM ek. A paid row's
    /// `payment_entitled`/`valid_until` are that instruction's terms, not the
    /// payment's record, which is the provider's and the payee's. Their
    /// `unsubscribe` rows STAY: draining one is what removes the reader's
    /// `subscribers` row, which is retained and otherwise keeps their id and ek
    /// until prune. Reaches a reader hosted on this nest only; a remote
    /// reader's deletion runs on their own nest.
    pub(super) fn subscribe_requests_purge_for_deleted_subscriber(
        conn: &Connection,
        subscriber: &[u8; 32],
    ) -> rusqlite::Result<usize> {
        conn.execute(
            "DELETE FROM subscribe_requests WHERE subscriber_id = ?1 AND kind = 'subscribe'",
            rusqlite::params![subscriber.as_slice()],
        )
    }

    /// Delete a pending `subscribe` request by its natural key (the plaintext
    /// direct-grant path retires any pending manual request the payment just
    /// satisfied). Returns `true` if a row was deleted.
    pub async fn delete_pending_subscribe_request(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        tier_name: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "DELETE FROM subscribe_requests
                 WHERE author_id = ?1 AND subscriber_id = ?2 AND tier_name = ?3
                   AND kind = 'subscribe'",
                rusqlite::params![author_id.as_slice(), subscriber_id.as_slice(), tier_name],
            )
            .context("delete pending subscribe request")?;
        Ok(changed > 0)
    }

    /// Drop the verified-payment marker from a pending request (refund landed
    /// before approval). The row survives as an ordinary manual request.
    pub async fn clear_request_payment_entitlement(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        tier_name: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE subscribe_requests SET payment_entitled = 0, valid_until = NULL
                 WHERE author_id = ?1 AND subscriber_id = ?2 AND tier_name = ?3
                   AND kind = 'subscribe'",
                rusqlite::params![author_id.as_slice(), subscriber_id.as_slice(), tier_name],
            )
            .context("clear request payment entitlement")?;
        Ok(changed > 0)
    }

    /// The payment window a pending request carries: `Ok(None)` = no such
    /// row; `Some((payment_entitled, valid_until))` otherwise. Read by the
    /// approve handler so the window survives request-row deletion and lands
    /// on the `subscribers` row.
    pub async fn get_request_payment_window(
        &self,
        request_id: i64,
    ) -> Result<Option<(bool, Option<i64>)>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT payment_entitled, valid_until FROM subscribe_requests WHERE id = ?1",
            rusqlite::params![request_id],
            |row| Ok((row.get::<_, i64>(0)? != 0, row.get(1)?)),
        )
        .optional()
        .context("get request payment window")
    }

    // ── Key management ────────────────────────────────────────────────

    pub async fn upsert_current_key_blob(
        &self,
        author_id: &[u8; 32],
        tier_name: &str,
        key_version: i64,
        blob_hash: &[u8; 32],
        blob_data: &[u8],
    ) -> Result<()> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO current_key_blobs (author_id, tier_name, key_version, blob_hash, blob_data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![author_id.as_slice(), tier_name, key_version, blob_hash.as_slice(), blob_data, now],
        ).context("upsert current key blob")?;
        Ok(())
    }

    /// Returns (key_version, blob_hash, blob_data).
    pub async fn get_current_key_blob(
        &self,
        author_id: &[u8; 32],
        tier_name: &str,
    ) -> Result<Option<(i64, Vec<u8>, Vec<u8>)>> {
        let conn = self.conn.lock().await;
        let result = conn
            .query_row(
                "SELECT key_version, blob_hash, blob_data FROM current_key_blobs
             WHERE author_id = ?1 AND tier_name = ?2
             ORDER BY key_version DESC LIMIT 1",
                rusqlite::params![author_id.as_slice(), tier_name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .context("get current key blob")?;
        Ok(result)
    }

    pub async fn get_nest_keypair(&self) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        let conn = self.conn.lock().await;
        let result = conn
            .query_row(
                "SELECT secret_key, public_key FROM nest_keypair WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("get nest keypair")?;
        Ok(result)
    }

    /// Overwrite the singleton `nest_keypair` row (`id = 1`). Used by the
    /// boot-time deployment-key reconcile (`crate::deployment_key`) to restore
    /// the durable deployment signing key after a factory-reset wipe regenerated
    /// a fresh random row via migrations — so the channel-binding `nest_actor_id`
    /// a client TOFU-pins stays stable across a reset (security.md § Transport
    /// trust; key-material-hierarchy.md § Roots — "rotates only on a deliberate
    /// admin event").
    pub async fn set_nest_keypair(&self, secret_key: &[u8], public_key: &[u8]) -> Result<()> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO nest_keypair (id, secret_key, public_key, created_at)
             VALUES (1, ?1, ?2, ?3)",
            rusqlite::params![secret_key, public_key, now],
        )
        .context("set nest keypair")?;
        Ok(())
    }

    pub async fn upsert_device_authorization(
        &self,
        author_id: &[u8; 32],
        device_key: &[u8; 32],
        payload: &[u8],
    ) -> Result<()> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO device_authorizations (author_id, device_key, payload, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![author_id.as_slice(), device_key.as_slice(), payload, now],
        ).context("upsert device authorization")?;
        Ok(())
    }

    pub async fn get_device_authorization(&self, author_id: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let result = conn
            .query_row(
                "SELECT payload FROM device_authorizations WHERE author_id = ?1",
                rusqlite::params![author_id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .context("get device authorization")?;
        Ok(result)
    }
}
