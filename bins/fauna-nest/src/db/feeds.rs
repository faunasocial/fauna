//! Feed and feed contributor methods.

use super::{CacheDb, author_id_from_db, links, now_epoch_millis, now_epoch_secs};
use super::{ContributorRow, FeedPostRow, FeedRow};
use anyhow::{Context, Result};

/// Who a feed read serves, and so which posts it may return beyond the
/// moderation gate every non-author surface shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedAudience {
    /// The account itself, on its own nest: every post the feed's rules
    /// match, gated ones included (the reader may hold the tier).
    OnBox,
    /// The account, reading a timeline whose contract is public posts only —
    /// the built-in Trending feed (`gated_tier IS NULL`).
    PublicOnBox,
    /// A reader **off the box** — a third-party principal reading a public
    /// timeline under `fauna:feed:read`. Held to
    /// [`super::public_servability::PUBLIC_POST_SERVABLE`], the one predicate
    /// for content leaving the nest to a party that is not the account: a
    /// gated post, a post with no index row, and an archive import are all
    /// withheld, exactly as from the federation surfaces.
    OffBox,
}

impl FeedAudience {
    /// The SQL fragment the audience ANDs onto a feed query (`c` = `content`,
    /// `cm` = `content_meta`).
    fn guard(self) -> String {
        match self {
            FeedAudience::OnBox => super::public_servability::MODERATION_SERVABLE.to_string(),
            FeedAudience::PublicOnBox => format!(
                "{} AND cm.gated_tier IS NULL",
                super::public_servability::MODERATION_SERVABLE
            ),
            FeedAudience::OffBox => format!(
                "({})",
                super::public_servability::PUBLIC_POST_SERVABLE.as_str()
            ),
        }
    }
}

/// The `order=score` **keyset cursor**: the ordering key of the last row served
/// plus that same row's `created_at` tiebreak.
///
/// Both halves are needed because the scored sort is compound — `ORDER BY key
/// DESC, created_at DESC` — so a key alone does not identify a position in the
/// stream. See the cursor predicate in [`CacheDb::query_feed_scored`] for what
/// each half does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreCursor {
    /// The composed ordering key of the boundary row (`FeedPostRow::score`).
    pub key: f64,
    /// The boundary row's `created_at` (epoch micros) — the tiebreak half. A
    /// key-only cursor (the pre-keyset shape) is refused at the handler; it
    /// left the wire with the compat-remnant sweep.
    pub created_at: i64,
}

/// The extra `content_labels` predicate a verdict that binds **every reader**
/// adds to its label lookups: the row must **name its writer**.
///
/// A label row is a category verdict *with provenance*
/// (`content-scoring.md` § The scoring-metadata bus); `fauna.labels.attach` used
/// to write one with a zero `scanner_id` under a class-only gate, so any member's
/// opinion about anyone's post arrived indistinguishable from a sanctioned
/// position's verdict — and the two nest-side verdicts that bind every reader
/// (the mandatory spam group here, the publisher fold in
/// [`project_content_labels`]) acted on it. The door now stamps its writer
/// (`label_handlers`), which leaves this predicate as the standing rule: an
/// unattributed row — whatever wrote it, whenever — informs nobody and removes
/// nothing. It is deliberately NOT applied to a feed's **own**
/// `LabelBelow`/`LabelAbove`/`HasLabel` rules: those are the user's own filter
/// over their own view (`moderation.md` § Categories & enforcement item 1's
/// user-side plane), not a verdict anyone else inherits.
const ATTRIBUTED_LABEL: &str = " AND length(cl2.scanner_id) = 32 \
                                AND cl2.scanner_id != zeroblob(32)";

/// The post ids whose text matches the FTS expression bound at `?{param}` —
/// the one set both `BodyContains` (`IN`) and `BodyExcludes` (`NOT IN`) test
/// against, and through `BodyContains` the feed page's search too
/// (`feed_routes::search_to_filter`).
///
/// Two halves, because a post's text rests in one of two places:
///
/// * a native post's own `post/%` corpus row, keyed by its post id;
/// * a bridged post's Search-corpus row (`bridge.<id>`, keyed by
///   `blake3(content_type:natural_id)`), reached through the `post_id` its
///   `bridge_index_map` row carries (`feed.md` § The read model → *The
///   list-card preview* → Corollary: the corpus is the only text index of
///   bridged content, and the per-bridge cap forbids a second).
///
/// So a bridged post matches only while its corpus row stands: show-in-search
/// OFF (`purge_bridge_content`) or past the cap (`trim_and_prune` →
/// `prune_orphan_index_map`) drops the map row with the corpus row, as do the
/// nostr deletion triggers — no text, no match, and `BodyExcludes` no longer
/// excludes it.
fn body_match_ids(param: usize) -> String {
    format!(
        "SELECT m.content_id FROM content_fts f JOIN content_fts_map m ON m.fts_rowid = f.rowid \
         WHERE content_fts MATCH ?{param} AND f.schema LIKE 'post/%' \
         UNION \
         SELECT bim.post_id FROM content_fts f JOIN content_fts_map m ON m.fts_rowid = f.rowid \
         JOIN bridge_index_map bim ON bim.content_id = m.content_id \
         WHERE content_fts MATCH ?{param} AND f.schema LIKE 'bridge.%' AND bim.post_id IS NOT NULL"
    )
}

/// Translate `FilterRule`s into SQL conditions over the `content c` /
/// `content_meta cm` aliases, appending to `out`
/// with sequential `?N` placeholders continuing from `*param_idx`. The ONE
/// shared translator behind `query_feed` / `query_feed_scored_impl` /
/// `query_feed_for_authors` — both their feed-rule groups and their
/// MANDATORY (always-AND) groups: the spam guard and the search filter.
///
/// `mandatory` is set for the MANDATORY group only: its label rules bind
/// every reader of the nest, so they read only rows that name their writer
/// ([`ATTRIBUTED_LABEL`]). A feed's own rules pass `false` — the user's filter is
/// the user's to aim.
///
/// The mandatory group alone also reads `AuthorNotInSet { actors: viewer }` as
/// the viewer's block (`moderation.md` § Corollary — block also hides): the
/// authors `viewer` has a `blocked` contact edge to are excluded. Only the
/// per-viewer stage of the viewer's own feed read writes it
/// (`feed_routes::push_viewer_block`); in a feed's own rules it stays
/// unsupported, because a rule naming another actor would read their block
/// list.
fn push_rule_conditions(
    rules: &[fauna_core::scoring::FilterRule],
    out: &mut Vec<String>,
    params: &mut Vec<Box<dyn rusqlite::types::ToSql + Send>>,
    param_idx: &mut usize,
    mandatory: bool,
) -> anyhow::Result<()> {
    use fauna_core::scoring::FilterRule;

    let attributed = if mandatory { ATTRIBUTED_LABEL } else { "" };

    for rule in rules {
        match rule {
            // A self-contained test over the post's own tag links, never a join
            // on the outer query: a joined tag row drops every UNTAGGED post
            // before an `Any` group's OR is read (a post matching only another
            // rule never showed), and makes two hashtag rules under `All` test
            // the SAME joined row (a post carrying both tags never matched).
            FilterRule::HasHashtag { tags } => {
                let placeholders: Vec<String> = tags
                    .iter()
                    .map(|tag| {
                        *param_idx += 1;
                        params.push(Box::new(tag.to_lowercase()));
                        format!("?{param_idx}")
                    })
                    .collect();
                out.push(format!(
                    "EXISTS (SELECT 1 FROM content_links cl WHERE cl.source_id = c.id \
                     AND cl.link_type = 'tag' AND cl.status IN ({}))",
                    placeholders.join(", ")
                ));
            }
            FilterRule::CreatedAfter { age_microseconds } => {
                let now_us = fauna_core::data::Timestamp::now().as_i64();
                let cutoff = now_us - (*age_microseconds as i64);
                *param_idx += 1;
                params.push(Box::new(cutoff));
                out.push(format!("c.created_at > ?{param_idx}"));
            }
            FilterRule::HasMedia { required } => {
                let val = if *required { 1i64 } else { 0i64 };
                *param_idx += 1;
                params.push(Box::new(val));
                out.push(format!("cm.has_media = ?{param_idx}"));
            }
            FilterRule::IsReply { required } => {
                let val = if *required { 1i64 } else { 0i64 };
                *param_idx += 1;
                params.push(Box::new(val));
                out.push(format!("cm.is_reply = ?{param_idx}"));
            }
            FilterRule::BodyContains { terms } => {
                // Quote each term to prevent FTS5 syntax injection, then AND them
                let match_expr = terms
                    .iter()
                    .map(|t| {
                        let escaped = t.replace('"', "\"\"");
                        format!("\"{escaped}\"")
                    })
                    .collect::<Vec<_>>()
                    .join(" AND ");
                *param_idx += 1;
                params.push(Box::new(match_expr));
                out.push(format!("c.id IN ({})", body_match_ids(*param_idx)));
            }
            FilterRule::BodyExcludes { terms } => {
                // OR semantics: exclude posts matching ANY of the terms
                let match_expr = terms
                    .iter()
                    .map(|t| {
                        let escaped = t.replace('"', "\"\"");
                        format!("\"{escaped}\"")
                    })
                    .collect::<Vec<_>>()
                    .join(" OR ");
                *param_idx += 1;
                params.push(Box::new(match_expr));
                out.push(format!("c.id NOT IN ({})", body_match_ids(*param_idx)));
            }
            FilterRule::Source { protocols } => {
                let placeholders: Vec<String> = protocols
                    .iter()
                    .map(|p| {
                        *param_idx += 1;
                        params.push(Box::new(p.clone()));
                        format!("?{param_idx}")
                    })
                    .collect();
                out.push(format!("c.source IN ({})", placeholders.join(", ")));
            }
            FilterRule::LabelBelow {
                category,
                max_confidence_permille,
            } => {
                *param_idx += 1;
                params.push(Box::new(category.clone()));
                *param_idx += 1;
                // per-mille u16 → the f64 REAL the `confidence` column stores
                params.push(Box::new(*max_confidence_permille as f64 / 1000.0));
                out.push(format!(
                    "NOT EXISTS (SELECT 1 FROM content_labels cl2 \
                     WHERE cl2.content_type = 'post' \
                     AND cl2.content_id = lower(hex(c.id)) \
                     AND cl2.category = ?{} \
                     AND cl2.confidence >= ?{}{attributed})",
                    *param_idx - 1,
                    *param_idx
                ));
            }
            FilterRule::LabelAbove {
                category,
                min_confidence_permille,
            } => {
                *param_idx += 1;
                params.push(Box::new(category.clone()));
                *param_idx += 1;
                // per-mille u16 → the f64 REAL the `confidence` column stores
                params.push(Box::new(*min_confidence_permille as f64 / 1000.0));
                out.push(format!(
                    "EXISTS (SELECT 1 FROM content_labels cl2 \
                     WHERE cl2.content_type = 'post' \
                     AND cl2.content_id = lower(hex(c.id)) \
                     AND cl2.category = ?{} \
                     AND cl2.confidence >= ?{}{attributed})",
                    *param_idx - 1,
                    *param_idx
                ));
            }
            FilterRule::HasLabel { category } => {
                *param_idx += 1;
                params.push(Box::new(category.clone()));
                *param_idx += 1;
                params.push(Box::new(0.5f64));
                out.push(format!(
                    "EXISTS (SELECT 1 FROM content_labels cl2 \
                     WHERE cl2.content_type = 'post' \
                     AND cl2.content_id = lower(hex(c.id)) \
                     AND cl2.category = ?{} \
                     AND cl2.confidence >= ?{}{attributed})",
                    *param_idx - 1,
                    *param_idx
                ));
            }
            FilterRule::MinReplies { count } => {
                *param_idx += 1;
                params.push(Box::new(*count as i64));
                out.push(format!("COALESCE(cm.reply_count, 0) >= ?{param_idx}"));
            }
            FilterRule::MinReposts { count } => {
                *param_idx += 1;
                params.push(Box::new(*count as i64));
                out.push(format!("COALESCE(cm.repost_count, 0) >= ?{param_idx}"));
            }
            FilterRule::AuthorNotInSet { actors: viewer } if mandatory => {
                *param_idx += 1;
                params.push(Box::new(viewer.to_vec()));
                out.push(format!(
                    "c.author NOT IN (SELECT peer_id FROM contacts \
                     WHERE actor_id = ?{param_idx} AND status = 'blocked')"
                ));
            }
            // A rule a newer writer added (`transport.md` § Rule 3 in full):
            // stored and returned intact, evaluated as a condition that never
            // matches — false under `all` and `any` alike, so the feed never
            // shows what a known rule would have hidden.
            FilterRule::Unknown(_) => out.push("0".to_string()),
            other => {
                anyhow::bail!("unsupported filter rule: {:?}", other);
            }
        }
    }

    Ok(())
}

impl CacheDb {
    // ==================== Feeds ====================

    /// Create a new feed. Returns the generated feed_id (hex string).
    /// `composition` is the canonical dag-cbor `Vec<CompositionEntry>`
    /// (`None` = no composition — frame § Composition).
    pub async fn create_feed(
        &self,
        owner: &[u8; 32],
        name: &str,
        rules: &[u8],
        combination: &str,
        scope: &str,
        contributor_seeds: &str,
        composition: Option<&[u8]>,
    ) -> Result<String> {
        let feed_id = fauna_core::identity::random_hex(16);
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO feeds (feed_id, owner, name, rules, combination, created_at, scope, contributor_seeds, composition)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![feed_id, owner.as_slice(), name, rules, combination, now, scope, contributor_seeds, composition],
        )
        .context("create feed")?;
        Ok(feed_id)
    }

    /// Get a feed by its feed_id. Returns None if not found.
    pub async fn get_feed(&self, feed_id: &str) -> Result<Option<FeedRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT feed_id, owner, name, rules, combination, created_at, scope, contributor_seeds, composition
                 FROM feeds WHERE feed_id = ?1",
            )
            .context("prepare get_feed")?;
        let mut rows = stmt
            .query_map(rusqlite::params![feed_id], |row| {
                Ok(FeedRow {
                    feed_id: row.get(0)?,
                    owner: row.get(1)?,
                    name: row.get(2)?,
                    rules: row.get(3)?,
                    combination: row.get(4)?,
                    created_at: row.get(5)?,
                    scope: row.get(6)?,
                    contributor_seeds: row.get(7)?,
                    composition: row.get(8)?,
                })
            })
            .context("query get_feed")?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// List all feeds, ordered by created_at DESC.
    pub async fn list_feeds(&self) -> Result<Vec<FeedRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT feed_id, owner, name, rules, combination, created_at, scope, contributor_seeds, composition
                 FROM feeds ORDER BY created_at DESC",
            )
            .context("prepare list_feeds")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(FeedRow {
                    feed_id: row.get(0)?,
                    owner: row.get(1)?,
                    name: row.get(2)?,
                    rules: row.get(3)?,
                    combination: row.get(4)?,
                    created_at: row.get(5)?,
                    scope: row.get(6)?,
                    contributor_seeds: row.get(7)?,
                    composition: row.get(8)?,
                })
            })
            .context("query list_feeds")?;
        let mut feeds = Vec::new();
        for row in rows {
            feeds.push(row?);
        }
        Ok(feeds)
    }

    /// List feeds owned by a specific owner, ordered by created_at DESC.
    pub async fn list_feeds_by_owner(&self, owner: &[u8; 32]) -> Result<Vec<FeedRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT feed_id, owner, name, rules, combination, created_at, scope, contributor_seeds, composition
                 FROM feeds WHERE owner = ?1 ORDER BY created_at DESC",
            )
            .context("prepare list_feeds_by_owner")?;
        let rows = stmt
            .query_map(rusqlite::params![owner.as_slice()], |row| {
                Ok(FeedRow {
                    feed_id: row.get(0)?,
                    owner: row.get(1)?,
                    name: row.get(2)?,
                    rules: row.get(3)?,
                    combination: row.get(4)?,
                    created_at: row.get(5)?,
                    scope: row.get(6)?,
                    contributor_seeds: row.get(7)?,
                    composition: row.get(8)?,
                })
            })
            .context("query list_feeds_by_owner")?;
        let mut feeds = Vec::new();
        for row in rows {
            feeds.push(row?);
        }
        Ok(feeds)
    }

    /// Count feeds owned by a specific owner.
    pub async fn count_feeds_by_owner(&self, owner: &[u8; 32]) -> Result<i64> {
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM feeds WHERE owner = ?1",
                rusqlite::params![owner.as_slice()],
                |row| row.get(0),
            )
            .context("count feeds by owner")?;
        Ok(count)
    }

    /// Update a feed. Only the owner can update. Returns true if a row was updated.
    ///
    /// `composition` is tri-state (the wire's `None` = leave-unchanged rule —
    /// an older client editing name/rules must not destroy a composition it
    /// cannot see): outer `None` = keep the stored value, `Some(None)` =
    /// clear to NULL, `Some(Some(bytes))` = overwrite.
    pub async fn update_feed(
        &self,
        feed_id: &str,
        owner: &[u8; 32],
        name: &str,
        rules: &[u8],
        combination: &str,
        composition: Option<Option<&[u8]>>,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let changed = match composition {
            None => conn
                .execute(
                    "UPDATE feeds SET name = ?1, rules = ?2, combination = ?3
                     WHERE feed_id = ?4 AND owner = ?5",
                    rusqlite::params![name, rules, combination, feed_id, owner.as_slice()],
                )
                .context("update feed")?,
            Some(composition) => conn
                .execute(
                    "UPDATE feeds SET name = ?1, rules = ?2, combination = ?3, composition = ?6
                     WHERE feed_id = ?4 AND owner = ?5",
                    rusqlite::params![
                        name,
                        rules,
                        combination,
                        feed_id,
                        owner.as_slice(),
                        composition
                    ],
                )
                .context("update feed")?,
        };
        Ok(changed > 0)
    }

    /// Delete a feed. Only the owner can delete. Returns true if a row was deleted.
    pub async fn delete_feed(&self, feed_id: &str, owner: &[u8; 32]) -> Result<bool> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "DELETE FROM feeds WHERE feed_id = ?1 AND owner = ?2",
                rusqlite::params![feed_id, owner.as_slice()],
            )
            .context("delete feed")?;
        Ok(changed > 0)
    }

    // ==================== Global factor set ====================

    /// Read a user's global factor set (frame § Composition — the entries
    /// folded into every one of their feeds' composed orderings), factor-
    /// ordered for a deterministic wire shape.
    pub async fn get_global_factors(
        &self,
        owner: &[u8; 32],
    ) -> Result<Vec<fauna_core::scoring::CompositionEntry>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT factor, weight_permille FROM feed_global_factors
                 WHERE owner = ?1 ORDER BY factor",
            )
            .context("prepare get_global_factors")?;
        let rows = stmt
            .query_map(rusqlite::params![owner.as_slice()], |row| {
                Ok(fauna_core::scoring::CompositionEntry {
                    factor: row.get(0)?,
                    weight_permille: row.get(1)?,
                })
            })
            .context("query get_global_factors")?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }

    /// Replace a user's global factor set (idempotent whole-set overwrite —
    /// the `fauna.feed.factors.set` contract; empty = clear). Caller
    /// validates via `fauna_core::scoring::validate_composition`.
    pub async fn set_global_factors(
        &self,
        owner: &[u8; 32],
        entries: &[fauna_core::scoring::CompositionEntry],
    ) -> Result<()> {
        let now = now_epoch_secs();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin set_global_factors")?;
        tx.execute(
            "DELETE FROM feed_global_factors WHERE owner = ?1",
            rusqlite::params![owner.as_slice()],
        )
        .context("clear global factors")?;
        for e in entries {
            tx.execute(
                "INSERT INTO feed_global_factors (owner, factor, weight_permille, updated_at)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![owner.as_slice(), e.factor, e.weight_permille, now],
            )
            .context("insert global factor")?;
        }
        tx.commit().context("commit set_global_factors")
    }

    // ==================== Feed Contributors ====================

    pub async fn upsert_contributor(
        &self,
        feed_id: &str,
        nest_url: &str,
        author_id: Option<&[u8; 32]>,
        discovered_via: &str,
    ) -> Result<()> {
        let author_blob: Vec<u8> = author_id.map(|a| a.to_vec()).unwrap_or_default();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let channel_priority = match discovered_via {
            "manual" => 3,
            "referral" => 2,
            _ => 1,
        };
        conn.execute(
            "INSERT INTO feed_contributors (feed_id, nest_url, author_id, discovered_via, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(feed_id, nest_url, author_id) DO UPDATE SET
               discovered_via = CASE
                 WHEN ?6 > (CASE feed_contributors.discovered_via
                   WHEN 'manual' THEN 3 WHEN 'referral' THEN 2 ELSE 1 END)
                 THEN ?4 ELSE feed_contributors.discovered_via END",
            rusqlite::params![feed_id, nest_url, author_blob, discovered_via, now, channel_priority],
        ).context("upsert contributor")?;
        Ok(())
    }

    /// Timestamp is epoch **microseconds** (matches recalculate_priorities).
    pub async fn record_contributor_hit(
        &self,
        feed_id: &str,
        nest_url: &str,
        author_id: Option<&[u8; 32]>,
        timestamp: i64,
    ) -> Result<()> {
        let author_blob: Vec<u8> = author_id.map(|a| a.to_vec()).unwrap_or_default();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE feed_contributors SET hit_count = hit_count + 1, last_seen = ?1
             WHERE feed_id = ?2 AND nest_url = ?3 AND author_id = ?4",
            rusqlite::params![timestamp, feed_id, nest_url, author_blob],
        )
        .context("record contributor hit")?;
        Ok(())
    }

    pub async fn list_contributors(&self, feed_id: &str) -> Result<Vec<ContributorRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT feed_id, nest_url, author_id, hit_count, last_seen,
                    poll_priority, discovered_via, created_at
             FROM feed_contributors WHERE feed_id = ?1
             ORDER BY CASE poll_priority WHEN 'hot' THEN 1 WHEN 'warm' THEN 2 ELSE 3 END,
                      last_seen DESC",
            )
            .context("prepare list_contributors")?;
        let rows = stmt
            .query_map(rusqlite::params![feed_id], |row| {
                Ok(ContributorRow {
                    feed_id: row.get(0)?,
                    nest_url: row.get(1)?,
                    author_id: author_id_from_db(row.get(2)?),
                    hit_count: row.get(3)?,
                    last_seen: row.get(4)?,
                    poll_priority: row.get(5)?,
                    discovered_via: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })
            .context("query list_contributors")?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    pub async fn list_contributors_by_priority(
        &self,
        feed_id: &str,
        priority: &str,
    ) -> Result<Vec<ContributorRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT feed_id, nest_url, author_id, hit_count, last_seen,
                    poll_priority, discovered_via, created_at
             FROM feed_contributors WHERE feed_id = ?1 AND poll_priority = ?2
             ORDER BY last_seen DESC",
            )
            .context("prepare list_contributors_by_priority")?;
        let rows = stmt
            .query_map(rusqlite::params![feed_id, priority], |row| {
                Ok(ContributorRow {
                    feed_id: row.get(0)?,
                    nest_url: row.get(1)?,
                    author_id: author_id_from_db(row.get(2)?),
                    hit_count: row.get(3)?,
                    last_seen: row.get(4)?,
                    poll_priority: row.get(5)?,
                    discovered_via: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })
            .context("query list_contributors_by_priority")?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    pub async fn remove_contributor(
        &self,
        feed_id: &str,
        nest_url: &str,
        author_id: Option<&[u8; 32]>,
    ) -> Result<bool> {
        let author_blob: Vec<u8> = author_id.map(|a| a.to_vec()).unwrap_or_default();
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "DELETE FROM feed_contributors WHERE feed_id = ?1 AND nest_url = ?2 AND author_id = ?3",
            rusqlite::params![feed_id, nest_url, author_blob],
        ).context("remove contributor")?;
        Ok(changed > 0)
    }

    pub async fn delete_contributors_for_feed(&self, feed_id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM feed_contributors WHERE feed_id = ?1",
            rusqlite::params![feed_id],
        )
        .context("delete contributors for feed")?;
        Ok(())
    }

    pub async fn recalculate_priorities(&self, feed_id: &str) -> Result<()> {
        let now_us = fauna_core::data::Timestamp::now().as_i64();
        let one_day = 24 * 60 * 60 * 1_000_000i64;
        let seven_days = 7 * one_day;
        let thirty_days = 30 * one_day;
        let conn = self.conn.lock().await;

        // Evict Cold contributors with last_seen > 30 days
        conn.execute(
            "DELETE FROM feed_contributors WHERE feed_id = ?1 AND poll_priority = 'cold' AND last_seen > 0 AND last_seen < ?2",
            rusqlite::params![feed_id, now_us - thirty_days],
        ).context("evict stale contributors")?;

        // Hot: last_seen within 24h
        conn.execute(
            "UPDATE feed_contributors SET poll_priority = 'hot'
             WHERE feed_id = ?1 AND last_seen >= ?2",
            rusqlite::params![feed_id, now_us - one_day],
        )
        .context("set hot priority")?;

        // Warm: last_seen within 7 days but not hot
        conn.execute(
            "UPDATE feed_contributors SET poll_priority = 'warm'
             WHERE feed_id = ?1 AND last_seen >= ?2 AND last_seen < ?3",
            rusqlite::params![feed_id, now_us - seven_days, now_us - one_day],
        )
        .context("set warm priority")?;

        // Cold: last_seen > 7 days
        conn.execute(
            "UPDATE feed_contributors SET poll_priority = 'cold'
             WHERE feed_id = ?1 AND last_seen > 0 AND last_seen < ?2",
            rusqlite::params![feed_id, now_us - seven_days],
        )
        .context("set cold priority")?;

        Ok(())
    }

    pub async fn reset_contributor_stats_for_feed(&self, feed_id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE feed_contributors SET hit_count = 0, poll_priority = 'warm'
             WHERE feed_id = ?1",
            rusqlite::params![feed_id],
        )
        .context("reset contributor stats")?;
        Ok(())
    }

    /// Insert a post into the content + content_meta tables, ignoring duplicates.
    /// Creates a minimal content row (no payload) for feed query visibility.
    /// Returns `true` if a new row was inserted, `false` if the post already existed.
    ///
    /// A thin wrapper over [`Self::insert_post_index_entry_with_origin`] with
    /// `origin_nest_url = None` — every caller that indexes a post this nest
    /// itself hosts (local creation, a bridge write, an archive import).
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_post_index_entry(
        &self,
        post_id: &[u8; 32],
        author: &[u8; 32],
        created_at: i64,
        has_media: bool,
        is_reply: bool,
        source: &str,
        tags: &[String],
    ) -> Result<bool> {
        self.insert_post_index_entry_with_origin(
            post_id, author, created_at, has_media, is_reply, source, tags, None,
        )
        .await
    }

    /// [`Self::insert_post_index_entry`], additionally recording the nest a
    /// discovery-fetched post was indexed FROM (`origin_nest_url`, `schema.rs`
    /// `SCHEMA_CONTENT`). The discovery poller
    /// (`discovery.rs`'s two `insert_post_index_entry_*` call sites) is the
    /// one production caller passing `Some(_)`: `source` there carries the
    /// peer's real advertised protocol token, never a fetch URL, and this
    /// column is where the "where did we get this" fact now lives instead —
    /// `resolve_nest_from_post_index` and `get_post_references` read it back.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_post_index_entry_with_origin(
        &self,
        post_id: &[u8; 32],
        author: &[u8; 32],
        created_at: i64,
        has_media: bool,
        is_reply: bool,
        source: &str,
        tags: &[String],
        origin_nest_url: Option<&str>,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;

        // Ensure a content row exists (INSERT OR IGNORE — don't overwrite existing payload)
        conn.execute(
            "INSERT OR IGNORE INTO content (id, schema, author, created_at, payload, source, origin_nest_url)
             VALUES (?1, 'post/text', ?2, ?3, x'', ?4, ?5)",
            rusqlite::params![
                post_id.as_slice(),
                author.as_slice(),
                created_at,
                source,
                origin_nest_url,
            ],
        )
        .context("insert content stub")?;

        // One id, one plane: the stub is OR IGNORE, so an id another plane's row
        // already holds keeps that row — and a peer's candidate may name any id.
        // Attach no post projection to it.
        let schema: String = conn
            .query_row(
                "SELECT schema FROM content WHERE id = ?1",
                [post_id.as_slice()],
                |row| row.get(0),
            )
            .context("read content schema")?;
        if !super::content::is_post_schema(&schema) {
            return Ok(false);
        }

        let changed = conn
            .execute(
                "INSERT OR IGNORE INTO content_meta (content_id, score, has_media, is_reply)
             VALUES (?1, 0, ?2, ?3)",
                rusqlite::params![post_id.as_slice(), has_media as i64, is_reply as i64,],
            )
            .context("insert content_meta entry")?;

        // Post-arrival List-labeler join (labeler-registry design Block A,
        // D12d) — same best-effort posture as `meta::upsert_meta`.
        if let Err(e) = super::labelers::join_list_labeler_scores_for_content(&conn, post_id) {
            tracing::warn!("list-labeler post-arrival join failed (content write succeeded): {e}");
        }

        if changed > 0 {
            let now = now_epoch_millis();
            for tag in tags {
                let _ = links::insert_link(
                    &conn,
                    "tag",
                    Some(post_id.as_slice()),
                    None,
                    None,
                    Some(tag.to_lowercase().as_str()),
                    None,
                    now,
                );
            }
        }
        Ok(changed > 0)
    }

    /// Test-only: seed one public post + one `content_scores` bus row —
    /// `insert_post_index_entry` then `insert_content_scores` with a single
    /// `factor`/`score` pair, no owner scope. The shape `feed_routes`'s and
    /// `composed_feed.rs`'s own local seed helpers each hand-copied.
    #[cfg(any(test, debug_assertions, feature = "test-hooks"))]
    pub async fn seed_scored_post_for_test(
        &self,
        post_id: &[u8; 32],
        author: &[u8; 32],
        created_at: i64,
        factor: &str,
        score: i64,
    ) -> Result<()> {
        self.insert_post_index_entry(post_id, author, created_at, false, false, "fauna", &[])
            .await?;
        self.insert_content_scores(
            post_id,
            "post",
            None,
            created_at,
            &[fauna_core::scoring::ScoreEntry {
                factor: factor.to_string(),
                score,
                tier: fauna_core::scoring::TIER_COMMUNITY,
                scorer_version: 1,
            }],
        )
        .await
    }

    /// Query the post index using filter rules, returning matching posts
    /// ordered by created_at DESC with cursor-based pagination.
    ///
    /// `rules` are the FEED's own criteria, joined by `combination`;
    /// `mandatory` are constraints that AND unconditionally on top of that
    /// group — the spam guard and the search filter. They MUST stay separate:
    /// pushed into `rules` under an `Any` combination they become mere
    /// OR-alternatives, so search could never narrow (the month-old
    /// `test_feed_search_filters_posts` apple red — the default e2e feed is
    /// empty-rules + `any`) and the spam guard was bypassed by any post
    /// matching one content rule.
    pub async fn query_feed(
        &self,
        rules: &[fauna_core::scoring::FilterRule],
        combination: fauna_core::scoring::FilterCombination,
        mandatory: &[fauna_core::scoring::FilterRule],
        cursor: Option<i64>,
        limit: i64,
    ) -> Result<Vec<FeedPostRow>> {
        self.query_feed_impl(
            rules,
            combination,
            mandatory,
            cursor,
            limit,
            FeedAudience::OnBox,
        )
        .await
    }

    /// [`Self::query_feed`] for an **off-box** reader — a third-party
    /// principal reading a public timeline (`authorization-server.md` § Scope
    /// grammar → *The Fauna family, exactly*, the first arm). See
    /// [`FeedAudience::OffBox`].
    pub async fn query_feed_off_box(
        &self,
        mandatory: &[fauna_core::scoring::FilterRule],
        cursor: Option<i64>,
        limit: i64,
    ) -> Result<Vec<FeedPostRow>> {
        self.query_feed_impl(
            &[],
            fauna_core::scoring::FilterCombination::All,
            mandatory,
            cursor,
            limit,
            FeedAudience::OffBox,
        )
        .await
    }

    async fn query_feed_impl(
        &self,
        rules: &[fauna_core::scoring::FilterRule],
        combination: fauna_core::scoring::FilterCombination,
        mandatory: &[fauna_core::scoring::FilterRule],
        cursor: Option<i64>,
        limit: i64,
        audience: FeedAudience,
    ) -> Result<Vec<FeedPostRow>> {
        use fauna_core::scoring::FilterCombination;

        let mut conditions: Vec<String> = Vec::new();
        let mut must_conditions: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql + Send>> = Vec::new();
        let mut param_idx: usize = 0;

        push_rule_conditions(rules, &mut conditions, &mut params, &mut param_idx, false)?;
        push_rule_conditions(
            mandatory,
            &mut must_conditions,
            &mut params,
            &mut param_idx,
            true,
        )?;

        // Cursor condition — mandatory (a page boundary, never a feed criterion).
        if let Some(cursor_ts) = cursor {
            param_idx += 1;
            params.push(Box::new(cursor_ts));
            must_conditions.push(format!("c.created_at < ?{param_idx}"));
        }

        // Build the SQL query

        // WHERE = every mandatory condition ANDed, plus the feed's rule group
        // joined by its combination (parenthesized so an `Any` group can't
        // swallow the mandatory constraints — the old shape that let a
        // catch-all `any` feed defeat search and the spam guard).
        let where_clause = {
            let joiner = match combination {
                FilterCombination::All => " AND ",
                FilterCombination::Any => " OR ",
            };
            let mut groups = must_conditions;
            if !conditions.is_empty() {
                if matches!(combination, FilterCombination::Any) && conditions.len() > 1 {
                    groups.push(format!("({})", conditions.join(joiner)));
                } else {
                    groups.push(conditions.join(joiner));
                }
            }
            if groups.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", groups.join(" AND "))
            }
        };

        // Unconditionally exclude quarantined/suppressed posts, and posts taken
        // down under a legal obligation (moderation.md § Categories & enforcement
        // item 1 — a taken-down post is withheld from every feed surface; the
        // visible tombstone lives at the direct-read path) — plus whatever the
        // reader's audience adds ([`FeedAudience::guard`]).
        let guard = audience.guard();
        let where_clause = if where_clause.is_empty() {
            format!("WHERE {guard}")
        } else {
            format!("{where_clause} AND {guard}")
        };

        param_idx += 1;
        params.push(Box::new(limit));
        let limit_param = format!("?{param_idx}");

        let sql = format!(
            "SELECT DISTINCT c.id, c.author, c.created_at, cm.has_media, cm.is_reply, c.source, \
             COALESCE(cm.preview, ''), \
             COALESCE(cm.like_count, 0), COALESCE(cm.reply_count, 0), COALESCE(cm.repost_count, 0), COALESCE(cm.quote_count, 0), \
             cm.gated_tier, cm.gated_room \
             FROM content c JOIN content_meta cm ON cm.content_id = c.id {where_clause} \
             ORDER BY c.created_at DESC LIMIT {limit_param}"
        );

        let conn = self.conn.lock().await;

        // Collect the main query rows
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|p| p.as_ref() as &dyn rusqlite::types::ToSql)
            .collect();
        let mut stmt = conn.prepare(&sql).context("prepare query_feed")?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, bool>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<Vec<u8>>>(12)?,
                ))
            })
            .context("query query_feed")?;

        let mut results: Vec<FeedPostRow> = Vec::new();
        for row in rows {
            let (
                post_id,
                author,
                created_at,
                has_media,
                is_reply,
                source,
                body,
                like_count,
                reply_count,
                repost_count,
                quote_count,
                gated_tier,
                gated_room,
            ) = row?;
            results.push(FeedPostRow {
                post_id,
                author,
                body,
                created_at,
                has_media,
                is_reply,
                like_count,
                reply_count,
                repost_count,
                quote_count,
                gated_tier,
                gated_room,
                tags: Vec::new(),
                quoted_post_id: None,
                reposted_post_id: None,
                viewer_repost_id: None,
                viewer_liked: false,
                source,
                score: None,
                web_slug: None,
                labels: Vec::new(),
            });
        }
        drop(stmt);

        // Fetch tags for each result
        let mut tag_stmt = conn
            .prepare("SELECT status FROM content_links WHERE source_id = ?1 AND link_type = 'tag'")
            .context("prepare tag lookup")?;
        for result in &mut results {
            let tag_rows = tag_stmt
                .query_map(rusqlite::params![result.post_id], |row| {
                    row.get::<_, String>(0)
                })
                .context("query tags")?;
            for tag in tag_rows {
                result.tags.push(tag?);
            }
        }

        // Quoted-post projection: the 32-byte target id from the
        // `content_links link_type='quote'` row (`feed.md` § The read model —
        // this read never touches `content.payload`). At most one per post.
        let mut quote_stmt = conn
            .prepare(
                "SELECT target_id FROM content_links \
                 WHERE source_id = ?1 AND link_type = 'quote' LIMIT 1",
            )
            .context("prepare quote lookup")?;
        for result in &mut results {
            let mut quote_rows = quote_stmt
                .query_map(rusqlite::params![result.post_id], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .context("query quote")?;
            if let Some(row) = quote_rows.next() {
                result.quoted_post_id = Some(row.context("read quote target")?);
            }
        }

        // Reposted-post projection — the quote twin (`feed.md` § Interaction
        // bar → Repost, ratified 2026-08-10). Same index, same discipline:
        // never touches `content.payload`. The viewer pair is deliberately NOT
        // read here — this fn is viewer-independent; `augment_viewer_state`
        // fills it for the local query paths only.
        let mut repost_stmt = conn
            .prepare(
                "SELECT target_id FROM content_links \
                 WHERE source_id = ?1 AND link_type = 'repost' LIMIT 1",
            )
            .context("prepare repost lookup")?;
        for result in &mut results {
            let mut repost_rows = repost_stmt
                .query_map(rusqlite::params![result.post_id], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .context("query repost")?;
            if let Some(row) = repost_rows.next() {
                result.reposted_post_id = Some(row.context("read repost target")?);
            }
        }

        // Per-row content-label projection (`moderation.md` § Per-row badge
        // data path): one entry per category, the highest-confidence row.
        for result in &mut results {
            result.labels = project_content_labels(&conn, &result.post_id)?;
            result.web_slug = project_web_slug(&conn, &result.post_id, &result.author)?;
        }

        Ok(results)
    }

    /// Query the post index using filter rules, returning matching posts
    /// ordered by the score key DESC (with `created_at DESC` tiebreaker) for
    /// score-based discovery feeds. Uses a score-based cursor for pagination.
    ///
    /// With an empty `composition` the key is the single
    /// nest-computed recency/engagement score (`content_meta.score`). With a
    /// non-empty one (frame § Composition) the key is the weighted sum
    /// `Σ (weight_permille · factor_value) / 1000` where a factor value is
    /// its `content_scores` bus row (JOINed by `content_id` alone — public
    /// posts carry `actor_id = NULL`; a factor with no row contributes 0,
    /// which is also the nest-side seam for sealed tier-1 factors the nest
    /// cannot read) and [`fauna_core::scoring::factor::ENGAGEMENT`] is
    /// `content_meta.score` expressed per-mille. The composed key rides the
    /// same `score` field / cursor contract as the single-score path.
    ///
    /// Includes gated (paywall) posts — a custom feed renders them as paywall
    /// cards client-side. The **Trending** virtual feed reads public posts only
    /// via [`Self::query_feed_scored_public`].
    /// Fill the per-viewer pair (`viewer_repost_id`, `viewer_liked`) on
    /// already-queried feed rows, for the **connection actor** (`feed.md`
    /// § Interaction bar → Repost, ratified 2026-08-10). A separate pass —
    /// never part of the query fns — so the query fns stay viewer-independent
    /// and the federated `remote_query_feed` path (which authenticates a peer
    /// nest, not the end viewer) simply never calls it.
    ///
    /// Two indexed lookups per row: the actor-keyed `content_links 'repost'`
    /// row minted by `write_post_index` (latest wins when devices raced the
    /// toggle — each repost post is individually unrepostable, so the pair
    /// converges), and the `engagement_events` like-toggle primary key
    /// (`compute_toggle_event_id`, the row `like`/`unlike` maintain).
    pub async fn augment_viewer_state(
        &self,
        viewer: &[u8; 32],
        rows: &mut [FeedPostRow],
    ) -> Result<()> {
        use rusqlite::OptionalExtension;
        let conn = self.conn.lock().await;
        let mut repost_stmt = conn
            .prepare(
                "SELECT source_id FROM content_links \
                 WHERE target_id = ?1 AND link_type = 'repost' AND actor_id = ?2 \
                 ORDER BY id DESC LIMIT 1",
            )
            .context("prepare viewer repost lookup")?;
        let mut like_stmt = conn
            .prepare("SELECT 1 FROM engagement_events WHERE event_id = ?1")
            .context("prepare viewer like lookup")?;
        let viewer_actor = fauna_core::identity::ActorId(*viewer);
        for row in rows.iter_mut() {
            row.viewer_repost_id = repost_stmt
                .query_row(rusqlite::params![row.post_id, viewer.as_slice()], |r| {
                    r.get::<_, Vec<u8>>(0)
                })
                .optional()
                .context("query viewer repost")?;
            let Ok(digest) = <[u8; 32]>::try_from(row.post_id.as_slice()) else {
                continue;
            };
            let like_event = fauna_core::engagement::compute_toggle_event_id(
                &viewer_actor,
                &fauna_core::data::ContentHash::from_digest_raw(digest),
                "like",
            );
            row.viewer_liked = like_stmt
                .query_row(
                    rusqlite::params![like_event.digest().as_slice()],
                    |_| Ok(()),
                )
                .optional()
                .context("query viewer like")?
                .is_some();
        }
        Ok(())
    }

    pub async fn query_feed_scored(
        &self,
        rules: &[fauna_core::scoring::FilterRule],
        combination: fauna_core::scoring::FilterCombination,
        mandatory: &[fauna_core::scoring::FilterRule],
        cursor: Option<ScoreCursor>,
        limit: i64,
        composition: &[fauna_core::scoring::CompositionEntry],
    ) -> Result<Vec<FeedPostRow>> {
        self.query_feed_scored_impl(
            rules,
            combination,
            mandatory,
            cursor,
            limit,
            composition,
            FeedAudience::OnBox,
        )
        .await
    }

    /// Public-audience-only variant of [`Self::query_feed_scored`]: additionally
    /// restricts to `content_meta.gated_tier IS NULL`. The built-in **Trending**
    /// virtual feed (`trending.md` § The Trending feed) serves public posts only
    /// — a restricted post never carries a `trending` row and is not the
    /// network's business — so it reads through this variant.
    pub async fn query_feed_scored_public(
        &self,
        rules: &[fauna_core::scoring::FilterRule],
        combination: fauna_core::scoring::FilterCombination,
        mandatory: &[fauna_core::scoring::FilterRule],
        cursor: Option<ScoreCursor>,
        limit: i64,
        composition: &[fauna_core::scoring::CompositionEntry],
    ) -> Result<Vec<FeedPostRow>> {
        self.query_feed_scored_impl(
            rules,
            combination,
            mandatory,
            cursor,
            limit,
            composition,
            FeedAudience::PublicOnBox,
        )
        .await
    }

    /// [`Self::query_feed_scored_public`] for an **off-box** reader — a
    /// third-party principal reading the Trending timeline. See
    /// [`FeedAudience::OffBox`].
    pub async fn query_feed_scored_off_box(
        &self,
        mandatory: &[fauna_core::scoring::FilterRule],
        cursor: Option<ScoreCursor>,
        limit: i64,
        composition: &[fauna_core::scoring::CompositionEntry],
    ) -> Result<Vec<FeedPostRow>> {
        self.query_feed_scored_impl(
            &[],
            fauna_core::scoring::FilterCombination::All,
            mandatory,
            cursor,
            limit,
            composition,
            FeedAudience::OffBox,
        )
        .await
    }

    /// Shared implementation behind [`Self::query_feed_scored`] and its
    /// audience-narrowed variants ([`FeedAudience`]).
    async fn query_feed_scored_impl(
        &self,
        rules: &[fauna_core::scoring::FilterRule],
        combination: fauna_core::scoring::FilterCombination,
        mandatory: &[fauna_core::scoring::FilterRule],
        cursor: Option<ScoreCursor>,
        limit: i64,
        composition: &[fauna_core::scoring::CompositionEntry],
        audience: FeedAudience,
    ) -> Result<Vec<FeedPostRow>> {
        use fauna_core::scoring::FilterCombination;

        let mut conditions: Vec<String> = Vec::new();
        let mut must_conditions: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql + Send>> = Vec::new();
        let mut param_idx: usize = 0;

        push_rule_conditions(rules, &mut conditions, &mut params, &mut param_idx, false)?;
        push_rule_conditions(
            mandatory,
            &mut must_conditions,
            &mut params,
            &mut param_idx,
            true,
        )?;

        // The ordering key: the single score, or the composed weighted
        // sum over bus factors. Numbered placeholders are reused wherever
        // the expression repeats (SELECT / cursor WHERE / ORDER BY).
        let ordering_key = if composition.is_empty() {
            "cm.score".to_string()
        } else {
            let mut terms = Vec::with_capacity(composition.len());
            for entry in composition {
                param_idx += 1;
                params.push(Box::new(entry.weight_permille));
                let w = param_idx;
                if entry.factor == fauna_core::scoring::factor::ENGAGEMENT {
                    // engagement expressed per-mille:
                    // (w/1000) · (cm.score · 1000) = w · cm.score
                    terms.push(format!("(?{w} * COALESCE(cm.score, 0))"));
                } else {
                    param_idx += 1;
                    params.push(Box::new(entry.factor.clone()));
                    terms.push(format!(
                        "(?{w} * COALESCE((SELECT cs.score FROM content_scores cs \
                         WHERE cs.content_id = c.id AND cs.factor = ?{param_idx}), 0) / 1000.0)"
                    ));
                }
            }
            format!("({})", terms.join(" + "))
        };

        // Keyset cursor on the compound sort `{ordering_key} DESC, created_at
        // DESC`: resume strictly *after* the boundary row, which means "a lower
        // key, OR the same key and an older post". The tiebreak is what keeps
        // rows tied on the key from being skipped — and what lets a *flat* key
        // paginate at all (a composition of only sealed factors scores every row
        // 0 nest-side, so `key < 0` would match nothing and the feed would
        // dead-end after one page; with the tiebreak it degenerates to plain
        // chronological keyset pagination, which is the right order for a flat
        // key).
        //
        // The `=` on an f64 key is exact, not a float-equality hazard: both
        // sides are the *same SQL expression over the same row* (SQLite REAL is
        // an IEEE double, and the cursor value was read from this very
        // expression on the previous page), so the boundary row compares equal
        // to itself bit-for-bit.
        if let Some(ScoreCursor { key, created_at }) = cursor {
            param_idx += 1;
            params.push(Box::new(key));
            let key_param = param_idx;
            param_idx += 1;
            params.push(Box::new(created_at));
            must_conditions.push(format!(
                "({ordering_key} < ?{key_param} \
                 OR ({ordering_key} = ?{key_param} AND c.created_at < ?{param_idx}))"
            ));
        }

        // Build the SQL query

        // WHERE = mandatory conditions ANDed + the feed's rule group joined by
        // its combination (parenthesized) — same shape as `query_feed`.
        let where_clause = {
            let joiner = match combination {
                FilterCombination::All => " AND ",
                FilterCombination::Any => " OR ",
            };
            let mut groups = must_conditions;
            if !conditions.is_empty() {
                if matches!(combination, FilterCombination::Any) && conditions.len() > 1 {
                    groups.push(format!("({})", conditions.join(joiner)));
                } else {
                    groups.push(conditions.join(joiner));
                }
            }
            if groups.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", groups.join(" AND "))
            }
        };

        // Unconditionally exclude quarantined/suppressed posts, and posts taken
        // down under a legal obligation (moderation.md § Categories & enforcement
        // item 1 — a taken-down post is withheld from every feed surface; the
        // visible tombstone lives at the direct-read path) — plus whatever the
        // reader's audience adds ([`FeedAudience::guard`]).
        let guard = audience.guard();
        let where_clause = if where_clause.is_empty() {
            format!("WHERE {guard}")
        } else {
            format!("{where_clause} AND {guard}")
        };

        param_idx += 1;
        params.push(Box::new(limit));
        let limit_param = format!("?{param_idx}");

        let sql = format!(
            "SELECT DISTINCT c.id, c.author, c.created_at, cm.has_media, cm.is_reply, c.source, {ordering_key}, \
             COALESCE(cm.preview, ''), \
             COALESCE(cm.like_count, 0), COALESCE(cm.reply_count, 0), COALESCE(cm.repost_count, 0), COALESCE(cm.quote_count, 0), \
             cm.gated_tier, cm.gated_room \
             FROM content c JOIN content_meta cm ON cm.content_id = c.id {where_clause} \
             ORDER BY {ordering_key} DESC, c.created_at DESC LIMIT {limit_param}"
        );

        let conn = self.conn.lock().await;

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|p| p.as_ref() as &dyn rusqlite::types::ToSql)
            .collect();
        let mut stmt = conn.prepare(&sql).context("prepare query_feed_scored")?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, bool>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, f64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<Vec<u8>>>(13)?,
                ))
            })
            .context("query query_feed_scored")?;

        let mut results: Vec<FeedPostRow> = Vec::new();
        for row in rows {
            let (
                post_id,
                author,
                created_at,
                has_media,
                is_reply,
                source,
                score,
                body,
                like_count,
                reply_count,
                repost_count,
                quote_count,
                gated_tier,
                gated_room,
            ) = row?;
            results.push(FeedPostRow {
                post_id,
                author,
                body,
                created_at,
                has_media,
                is_reply,
                like_count,
                reply_count,
                repost_count,
                quote_count,
                gated_tier,
                gated_room,
                tags: Vec::new(),
                quoted_post_id: None,
                reposted_post_id: None,
                viewer_repost_id: None,
                viewer_liked: false,
                source,
                score: Some(score),
                web_slug: None,
                labels: Vec::new(),
            });
        }
        drop(stmt);

        // Fetch tags for each result
        let mut tag_stmt = conn
            .prepare("SELECT status FROM content_links WHERE source_id = ?1 AND link_type = 'tag'")
            .context("prepare tag lookup")?;
        for result in &mut results {
            let tag_rows = tag_stmt
                .query_map(rusqlite::params![result.post_id], |row| {
                    row.get::<_, String>(0)
                })
                .context("query tags")?;
            for tag in tag_rows {
                result.tags.push(tag?);
            }
        }

        // Quoted-post projection: the 32-byte target id from the
        // `content_links link_type='quote'` row (`feed.md` § The read model —
        // this read never touches `content.payload`). At most one per post.
        let mut quote_stmt = conn
            .prepare(
                "SELECT target_id FROM content_links \
                 WHERE source_id = ?1 AND link_type = 'quote' LIMIT 1",
            )
            .context("prepare quote lookup")?;
        for result in &mut results {
            let mut quote_rows = quote_stmt
                .query_map(rusqlite::params![result.post_id], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .context("query quote")?;
            if let Some(row) = quote_rows.next() {
                result.quoted_post_id = Some(row.context("read quote target")?);
            }
        }

        // Reposted-post projection — the quote twin (`feed.md` § Interaction
        // bar → Repost, ratified 2026-08-10). Same index, same discipline:
        // never touches `content.payload`. The viewer pair is deliberately NOT
        // read here — this fn is viewer-independent; `augment_viewer_state`
        // fills it for the local query paths only.
        let mut repost_stmt = conn
            .prepare(
                "SELECT target_id FROM content_links \
                 WHERE source_id = ?1 AND link_type = 'repost' LIMIT 1",
            )
            .context("prepare repost lookup")?;
        for result in &mut results {
            let mut repost_rows = repost_stmt
                .query_map(rusqlite::params![result.post_id], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .context("query repost")?;
            if let Some(row) = repost_rows.next() {
                result.reposted_post_id = Some(row.context("read repost target")?);
            }
        }

        // Per-row content-label projection (`moderation.md` § Per-row badge
        // data path): one entry per category, the highest-confidence row.
        for result in &mut results {
            result.labels = project_content_labels(&conn, &result.post_id)?;
            result.web_slug = project_web_slug(&conn, &result.post_id, &result.author)?;
        }

        Ok(results)
    }

    /// Query the post index with filter rules, scoped to specific authors.
    /// Used by the remote query (`fauna.federation.feed.query`).
    pub async fn query_feed_for_authors(
        &self,
        rules: &[fauna_core::scoring::FilterRule],
        combination: fauna_core::scoring::FilterCombination,
        mandatory: &[fauna_core::scoring::FilterRule],
        authors: &[Vec<u8>],
        cursor: Option<i64>,
        limit: i64,
    ) -> Result<Vec<FeedPostRow>> {
        use fauna_core::scoring::FilterCombination;

        if authors.is_empty() {
            return Ok(vec![]);
        }

        let mut rule_conditions: Vec<String> = Vec::new();
        let mut must_conditions: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql + Send>> = Vec::new();
        let mut param_idx: usize = 0;

        // Author params (always AND-ed)
        let author_placeholders: Vec<String> = authors
            .iter()
            .map(|a| {
                param_idx += 1;
                params.push(Box::new(a.clone()));
                format!("?{param_idx}")
            })
            .collect();

        // Process filter rules (same logic as query_feed)
        push_rule_conditions(
            rules,
            &mut rule_conditions,
            &mut params,
            &mut param_idx,
            false,
        )?;
        push_rule_conditions(
            mandatory,
            &mut must_conditions,
            &mut params,
            &mut param_idx,
            true,
        )?;

        // Cursor condition (always AND-ed)
        let cursor_cond = if let Some(cursor_ts) = cursor {
            param_idx += 1;
            params.push(Box::new(cursor_ts));
            Some(format!("c.created_at < ?{param_idx}"))
        } else {
            None
        };

        // Build WHERE clause — author IN and cursor are always AND-ed,
        // only the rule-derived conditions use the combination mode.

        let mut where_parts: Vec<String> = vec![
            format!("c.author IN ({})", author_placeholders.join(", ")),
            // The same moderation gate as every other feed read. "Author-scoped"
            // names the FILTER (posts by these authors), not the audience: the
            // one caller is the federation feed query (`remote_query_feed_core`),
            // which answers a PEER nest. So all three flags bind — until
            // 2026-09-10 this read carried the takedown arm alone, and a peer
            // naming an author was handed that author's quarantined and
            // suppressed posts, with the references extracted from their bodies
            // (moderation.md § Legal takedown → *Posts*).
            super::public_servability::MODERATION_SERVABLE.to_string(),
        ];
        where_parts.extend(must_conditions);
        if !rule_conditions.is_empty() {
            let joiner = match combination {
                FilterCombination::All => " AND ",
                FilterCombination::Any => " OR ",
            };
            where_parts.push(format!("({})", rule_conditions.join(joiner)));
        }
        if let Some(cc) = cursor_cond {
            where_parts.push(cc);
        }

        let where_clause = format!("WHERE {}", where_parts.join(" AND "));

        param_idx += 1;
        params.push(Box::new(limit));
        let limit_param = format!("?{param_idx}");

        // `c.source`: this projection feeds `remote_query_feed_core`'s
        // AUTHOR-SCOPED branch exclusively — the referral-chain query
        // (`discovery.rs::follow_references` always passes a single specific
        // author) — so a candidate served off this path used to carry a
        // hard-coded empty `source` (below), silently swallowed as harmless
        // while `parse_peer_candidate` ignored the wire `source` field
        // entirely. Now that it threads the peer's real token through
        // `fauna_core::source::normalize`, an empty string fails normalization
        // and drops the candidate — which would have silently broken EVERY
        // referral-chain discovery in production. Select it exactly like
        // `query_feed` does.
        let sql = format!(
            "SELECT DISTINCT c.id, c.author, c.created_at, cm.has_media, cm.is_reply, c.source, \
             COALESCE(cm.preview, ''), \
             COALESCE(cm.like_count, 0), COALESCE(cm.reply_count, 0), COALESCE(cm.repost_count, 0), COALESCE(cm.quote_count, 0), \
             cm.gated_tier, cm.gated_room \
             FROM content c JOIN content_meta cm ON cm.content_id = c.id {where_clause} \
             ORDER BY c.created_at DESC LIMIT {limit_param}"
        );

        let conn = self.conn.lock().await;

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|p| p.as_ref() as &dyn rusqlite::types::ToSql)
            .collect();
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare query_feed_for_authors")?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, bool>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<Vec<u8>>>(12)?,
                ))
            })
            .context("query query_feed_for_authors")?;

        let mut results: Vec<FeedPostRow> = Vec::new();
        for row in rows {
            let (
                post_id,
                author,
                created_at,
                has_media,
                is_reply,
                source,
                body,
                like_count,
                reply_count,
                repost_count,
                quote_count,
                gated_tier,
                gated_room,
            ) = row?;
            results.push(FeedPostRow {
                post_id,
                author,
                body,
                created_at,
                has_media,
                is_reply,
                like_count,
                reply_count,
                repost_count,
                quote_count,
                gated_tier,
                gated_room,
                tags: Vec::new(),
                quoted_post_id: None,
                reposted_post_id: None,
                viewer_repost_id: None,
                viewer_liked: false,
                source,
                score: None,
                web_slug: None,
                labels: Vec::new(),
            });
        }
        drop(stmt);

        // Fetch tags for each result
        let mut tag_stmt = conn
            .prepare("SELECT status FROM content_links WHERE source_id = ?1 AND link_type = 'tag'")
            .context("prepare tag lookup")?;
        for result in &mut results {
            let tag_rows = tag_stmt
                .query_map(rusqlite::params![result.post_id], |row| {
                    row.get::<_, String>(0)
                })
                .context("query tags")?;
            for tag in tag_rows {
                result.tags.push(tag?);
            }
        }

        // Quoted-post projection: the 32-byte target id from the
        // `content_links link_type='quote'` row (`feed.md` § The read model —
        // this read never touches `content.payload`). At most one per post.
        let mut quote_stmt = conn
            .prepare(
                "SELECT target_id FROM content_links \
                 WHERE source_id = ?1 AND link_type = 'quote' LIMIT 1",
            )
            .context("prepare quote lookup")?;
        for result in &mut results {
            let mut quote_rows = quote_stmt
                .query_map(rusqlite::params![result.post_id], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .context("query quote")?;
            if let Some(row) = quote_rows.next() {
                result.quoted_post_id = Some(row.context("read quote target")?);
            }
        }

        // Reposted-post projection — the quote twin (`feed.md` § Interaction
        // bar → Repost, ratified 2026-08-10). Same index, same discipline:
        // never touches `content.payload`. The viewer pair is deliberately NOT
        // read here — this fn is viewer-independent; `augment_viewer_state`
        // fills it for the local query paths only.
        let mut repost_stmt = conn
            .prepare(
                "SELECT target_id FROM content_links \
                 WHERE source_id = ?1 AND link_type = 'repost' LIMIT 1",
            )
            .context("prepare repost lookup")?;
        for result in &mut results {
            let mut repost_rows = repost_stmt
                .query_map(rusqlite::params![result.post_id], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .context("query repost")?;
            if let Some(row) = repost_rows.next() {
                result.reposted_post_id = Some(row.context("read repost target")?);
            }
        }

        // Per-row content-label projection (`moderation.md` § Per-row badge
        // data path): one entry per category, the highest-confidence row.
        for result in &mut results {
            result.labels = project_content_labels(&conn, &result.post_id)?;
            result.web_slug = project_web_slug(&conn, &result.post_id, &result.author)?;
        }

        Ok(results)
    }

    /// One post's labels reduced to one entry per category — the same
    /// highest-confidence projection the feed cards carry, for the one nest
    /// surface that folds labels itself: the public web render of a published
    /// post (`region-blocking.md` § The content plane → *The nest-as-publisher
    /// leg*). Sharing [`project_content_labels`] is what keeps a post's labels
    /// reading the same through both doors.
    pub async fn post_label_entries(
        &self,
        post_id: &[u8; 32],
    ) -> Result<Vec<fauna_core::content_category::ContentLabelEntry>> {
        let conn = self.conn.lock().await;
        project_content_labels(&conn, post_id)
    }
}

/// Project a post's web-publish slug — the `content_links`
/// `link_type='web_published'` row's `status` column, which is where
/// [`CacheDb::publish_web_post`](crate::db::CacheDb::publish_web_post) stores
/// the slug. `None` = the post is not published to the web.
///
/// Read from the link index exactly like the `quote` projection above, so the
/// feed read never touches `content.payload` (`feed.md` § The read model).
/// Drives the wire `FeedPostItem.web_slug`, from which the apps derive the
/// own-post web verbs on the ⋯ overflow (`ui/feed.md` § User actions).
///
/// ⚠ **Keyed on `author`, not on `source_id` alone.** The publish row's upsert
/// key is `(link_type, source_id, actor_id)` (`links::upsert_link`) and
/// `fauna.web.publish.set` does **not** verify that the caller authored the
/// post — so several actors can each hold a `web_published` row for the same
/// post. An unfiltered `LIMIT 1` would then hand the author's own ⋯ menu a
/// *stranger's* slug: "Copy web link" would yield someone else's page, and
/// "Unpublish" would silently no-op against a row the caller doesn't own.
/// Keying on the post's author makes the projection the post's canonical page,
/// deterministic regardless of who else republished it.
fn project_web_slug(
    conn: &rusqlite::Connection,
    post_id: &[u8],
    author: &[u8],
) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT status FROM content_links \
             WHERE source_id = ?1 AND link_type = 'web_published' AND actor_id = ?2 \
             LIMIT 1",
        )
        .context("prepare web-publish slug lookup")?;
    let mut rows = stmt
        .query_map(rusqlite::params![post_id, author], |row| {
            row.get::<_, Option<String>>(0)
        })
        .context("query web-publish slug")?;
    match rows.next() {
        Some(row) => Ok(row.context("read web-publish slug")?),
        None => Ok(None),
    }
}

/// Project a post's `content_labels` rows into one [`ContentLabelEntry`] per
/// category — the highest-confidence row (`moderation.md` § Per-row badge data
/// path). `post_id` is the raw 32-byte content id; `content_labels.content_id`
/// is TEXT lowercase hex (the same `lower(hex(c.id))` bridge the feed filter
/// predicates use, `FilterRule::HasLabel` et al. above).
///
/// **Attributed rows only** ([`ATTRIBUTED_LABEL`]). Both doors this projection
/// serves speak to an audience that did not choose the label: the feed card's
/// badge, shown to every reader of the post, and the nest-as-publisher region
/// fold, which turns a category verdict into a blocked public page
/// (`region-blocking.md` § The nest-as-publisher leg). Neither may relay a
/// verdict whose writer the row does not name — the residue of the old
/// class-only `fauna.labels.attach` door, which let any member write one about
/// anyone's post. Keeping the predicate HERE rather than at one caller is what
/// keeps a post's labels reading the same through both doors.
fn project_content_labels(
    conn: &rusqlite::Connection,
    post_id: &[u8],
) -> Result<Vec<fauna_core::content_category::ContentLabelEntry>> {
    let mut stmt = conn
        .prepare(
            "SELECT category, MAX(confidence) FROM content_labels cl2 \
             WHERE content_type = 'post' AND content_id = lower(hex(?1)) \
             AND length(cl2.scanner_id) = 32 AND cl2.scanner_id != zeroblob(32) \
             GROUP BY category ORDER BY MAX(confidence) DESC",
        )
        .context("prepare label lookup")?;
    let rows = stmt
        .query_map(rusqlite::params![post_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })
        .context("query labels")?;
    let mut labels = Vec::new();
    for row in rows {
        let (category, confidence) = row?;
        let confidence_per_mille = (confidence * 1000.0).round().clamp(0.0, 1000.0) as u16;
        labels.push(fauna_core::content_category::ContentLabelEntry {
            category,
            confidence_per_mille,
        });
    }
    Ok(labels)
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    /// One id, one plane — on the discovery poller's index stub too. A peer's
    /// feed-query candidate names whatever id it likes; one naming another
    /// plane's stored row (an inbox message here) must not mint a
    /// `content_meta` row for it — the row a takedown matches while that
    /// plane's own reads ignore it, and the row the trend fetch probes as
    /// "seen".
    #[tokio::test]
    async fn a_discovery_index_stub_never_attaches_to_another_planes_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let (message, author) = ([2u8; 32], [3u8; 32]);
        {
            let conn = db.conn().await;
            crate::db::content::insert_content(
                &conn,
                &message,
                "inbox/message",
                &author,
                1_000,
                b"members-only",
                None,
                "fauna",
                None,
            )
            .unwrap();
        }

        let inserted = db
            .insert_post_index_entry(
                &message,
                &[9u8; 32],
                1_000,
                false,
                false,
                "https://peer.example/api/v1/posts/candidate",
                &[],
            )
            .await
            .unwrap();

        assert!(!inserted, "no post index entry for another plane's id");
        assert!(
            !db.content_meta_exists(&message).await.unwrap(),
            "no content_meta row attached to another plane's row"
        );
        let conn = db.conn().await;
        assert_eq!(
            crate::db::content::get_content(&conn, &message)
                .unwrap()
                .map(|(p, _)| p),
            Some(b"members-only".to_vec()),
            "the row's payload is untouched"
        );
    }
}
