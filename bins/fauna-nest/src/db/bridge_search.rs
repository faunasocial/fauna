//! Nest-state half of the uniform per-bridge Search-corpus policy.
//!
//! Owner doc: `docs/goal/behavior/content-index.md` § Bridge content in the
//! Search corpus. The shared half (setting keys, labels, the `number` type, the
//! default cap, the content-bridge predicate, the `bridge.<id>` content type)
//! is `fauna_protocol::bridge_search_policy`; this module owns only what the
//! nest owns — where the two user choices rest, and what they mean when a nest
//! carries more than one actor.
//!
//! ## The multi-actor rule: UNION
//!
//! The two controls are per-actor (they are bridge settings, and a bridge is
//! linked per actor), but `content_fts`'s bridge corpus is one shared, public,
//! nest-wide table — the same table every actor's Search page reads. The owner
//! doc states the eviction rule as "newest-N **per bridge**" and never says what
//! two actors disagreeing means, so this module pins it:
//!
//! * indexed if **any** actor has the bridge's toggle ON, and
//! * the effective cap is the **greatest** cap among those ON actors, and
//! * toggle-off purges only when the actor turning it off was the **last** one
//!   holding it ON.
//!
//! Union, not intersection, for one load-bearing reason: the alternative lets
//! one actor destroy another actor's search corpus with a setting on their own
//! bridge — a user acting on their own control silently deleting a second
//! user's data. On the overwhelmingly common single-actor nest the two readings
//! are identical, so union costs nothing there and is the only safe answer on
//! the boxes where they differ.
//!
//! An actor with no row is on the defaults (ON, [`DEFAULT_SEARCH_POST_LIMIT`]),
//! which is why [`effective_policy`] counts actors rather than just reading
//! rows: a single explicit OFF must not stand in for every actor who never
//! touched the setting.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

use fauna_protocol::bridge_search_policy::{
    DEFAULT_SEARCH_POST_LIMIT, DEFAULT_SHOW_IN_SEARCH, clamp_post_limit,
};

/// One actor's choices for one bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchPolicy {
    pub show_in_search: bool,
    pub post_limit: u32,
}

impl Default for SearchPolicy {
    fn default() -> Self {
        Self {
            show_in_search: DEFAULT_SHOW_IN_SEARCH,
            post_limit: DEFAULT_SEARCH_POST_LIMIT,
        }
    }
}

/// This actor's policy for this bridge — the defaults when they have never
/// touched it (works-out-of-the-box: a fresh nest indexes with no setup).
pub fn get_policy(conn: &Connection, actor_id: &str, bridge_id: &str) -> Result<SearchPolicy> {
    let row: Option<(i64, i64)> = conn
        .query_row(
            "SELECT show_in_search, post_limit FROM bridge_search_policy \
             WHERE actor_id = ?1 AND bridge_id = ?2",
            rusqlite::params![actor_id, bridge_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .context("read bridge search policy")?;
    Ok(match row {
        Some((show, limit)) => SearchPolicy {
            show_in_search: show != 0,
            post_limit: clamp_post_limit(limit.max(0) as u32),
        },
        None => SearchPolicy::default(),
    })
}

/// Write this actor's policy for this bridge. The cap is clamped nest-side, so
/// a client cannot set one that defeats the control.
pub fn set_policy(
    conn: &Connection,
    actor_id: &str,
    bridge_id: &str,
    policy: SearchPolicy,
) -> Result<()> {
    conn.execute(
        "INSERT INTO bridge_search_policy (actor_id, bridge_id, show_in_search, post_limit) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(actor_id, bridge_id) DO UPDATE SET \
             show_in_search = excluded.show_in_search, \
             post_limit = excluded.post_limit",
        rusqlite::params![
            actor_id,
            bridge_id,
            policy.show_in_search as i64,
            clamp_post_limit(policy.post_limit) as i64,
        ],
    )
    .context("write bridge search policy")?;
    Ok(())
}

/// The nest-wide effective policy for a bridge, per the union rule in this
/// module's docs: `Some(cap)` when at least one actor has it ON (the cap being
/// the greatest among them), `None` when every actor has it OFF — which is the
/// signal both to stop indexing and, at a setting write, to purge.
/// ⚠ **A SUPERSEDED IDENTITY IS NOT AN ACTOR WHO COULD EXPRESS A PREFERENCE**,
/// and counting one was a live defect until 2026-08-15 (the `bridge_*`
/// pass). `record_succession` keeps the predecessor's `users` row — handle
/// cleared, retained as an FK target for attribution rows
/// (`succession-aftermath.md` § Re-key scope) — and inserts the successor's, so
/// a bare `COUNT(*) FROM users` grows by one at every ceremony. That extra row
/// can never hold a policy row of its own (the retired identity is refused
/// everywhere and its own row moved to the successor at the same ceremony), so
/// it permanently pushes `explicit < total_actors` true and casts a phantom **ON
/// default vote** for ever. The measured consequence: on a nest where every user
/// had turned a bridge's corpus OFF, one recovery ceremony silently switched
/// indexing back ON — a privacy regression caused by the act of recovering from
/// a key theft, and one no per-table succession verdict can reach, because the
/// phantom vote comes from the `users` row rather than from this table.
pub fn effective_policy(conn: &Connection, bridge_id: &str) -> Result<Option<u32>> {
    let total_actors: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM users u
              WHERE NOT EXISTS (
                    SELECT 1 FROM actor_successions s WHERE s.old_actor_id = u.actor_id)",
            [],
            |r| r.get(0),
        )
        .context("count actors")?;

    let mut stmt = conn
        .prepare("SELECT show_in_search, post_limit FROM bridge_search_policy WHERE bridge_id = ?1")
        .context("prepare effective policy scan")?;
    let rows = stmt
        .query_map([bridge_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })
        .context("scan effective policy")?
        .collect::<Result<Vec<_>, _>>()?;

    let explicit = rows.len() as i64;
    let mut best: Option<u32> = None;
    for (show, limit) in &rows {
        if *show != 0 {
            let cap = clamp_post_limit((*limit).max(0) as u32);
            best = Some(best.map_or(cap, |b: u32| b.max(cap)));
        }
    }

    // Any actor who never expressed a preference is still on the ON default,
    // and their vote counts in the union. `explicit > total_actors` is possible
    // for a moment after an account deletion leaves an orphan row behind (a
    // known open track); `<` is the only direction that adds a default vote.
    if DEFAULT_SHOW_IN_SEARCH && explicit < total_actors {
        best = Some(best.map_or(DEFAULT_SEARCH_POST_LIMIT, |b: u32| {
            b.max(DEFAULT_SEARCH_POST_LIMIT)
        }));
    }
    Ok(best)
}

// ── The corpus itself ────────────────────────────────────────────────

/// Index one piece of a bridge's **public** content into `content_fts`, then
/// enforce the newest-N cap. Bridge-agnostic on purpose: every content bridge's
/// transit point calls this same function, which is what makes the policy
/// uniform rather than three per-bridge copies.
///
/// * `natural_id` is the bridge's own id for the item (nostr: event id;
///   bluesky: at-uri; activitypub: object id). Two transit points that see the
///   same item therefore land on the same key, so an item indexed twice is
///   replaced rather than double-surfaced — dedup by construction.
/// * `created_at` is epoch **microseconds** — the recency key the cap evicts on
///   *and* the value the Search window compares, via `COALESCE(c.created_at,
///   m.created_at)` (`db/fts.rs`). It must therefore share `content.created_at`'s
///   unit; callers whose source timestamp is in another scale convert here, not
///   downstream. (Documented as seconds until 2026-08-03, which is what put
///   every bridge-corpus hit at ~1970 against the epoch-micros `before`/`after`
///   cursors `SearchRequest` declares.)
///
/// * `post_id` is the post this transit point rested for the item, when it
///   rests one — the link the feed's text filters follow to a bridged post
///   (`feed.md` § The read model → *The list-card preview* → Corollary).
///   `None` (the nostr relay store, which rests no post) never erases a link
///   another transit point wrote for the same natural id.
///
/// A no-op when the nest-wide effective policy is OFF. Returns whether the row
/// was indexed.
#[allow(clippy::too_many_arguments)]
pub fn index_bridge_content(
    conn: &Connection,
    bridge_id: &str,
    natural_id: &str,
    author_name: &str,
    body: &str,
    created_at: i64,
    post_id: Option<&[u8; 32]>,
) -> Result<bool> {
    let Some(cap) = effective_policy(conn, bridge_id)? else {
        return Ok(false);
    };
    let content_type = fauna_protocol::bridge_search_policy::content_type_for_bridge(bridge_id);
    let key = crate::db::content_id_for_document(&content_type, natural_id);

    crate::db::search::index_content(
        conn,
        &key,
        &content_type,
        "",
        body,
        author_name,
        "",
        created_at,
    )?;
    conn.execute(
        "INSERT INTO bridge_index_map (content_type, natural_id, content_id, post_id) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(content_type, natural_id) DO UPDATE SET \
             content_id = excluded.content_id, \
             post_id = COALESCE(excluded.post_id, bridge_index_map.post_id)",
        rusqlite::params![
            content_type,
            natural_id,
            key.as_slice(),
            post_id.map(|p| p.as_slice())
        ],
    )
    .context("write bridge index map")?;

    // Eviction is enforced at ingest, so the window is a true recency window
    // rather than something a background job has to catch up with.
    trim_and_prune(conn, &content_type, cap)?;
    Ok(true)
}

/// Trim to the newest `cap` rows and drop the linkage rows the trim orphaned —
/// always together, so `bridge_index_map` can never outlive the corpus it
/// points at.
fn trim_and_prune(conn: &Connection, content_type: &str, cap: u32) -> Result<usize> {
    let n = crate::db::search::trim_schema_to_newest(conn, content_type, cap)?;
    if n > 0 {
        prune_orphan_index_map(conn, content_type)?;
    }
    Ok(n)
}

/// Drop every indexed row of a bridge — the purge a toggle-off performs once
/// the last actor holding it ON turns it off.
pub fn purge_bridge_content(conn: &Connection, bridge_id: &str) -> Result<usize> {
    let content_type = fauna_protocol::bridge_search_policy::content_type_for_bridge(bridge_id);
    let n = crate::db::search::purge_schema(conn, &content_type)?;
    conn.execute(
        "DELETE FROM bridge_index_map WHERE content_type = ?1",
        [&content_type],
    )
    .context("purge bridge index map")?;
    Ok(n)
}

/// Re-apply a bridge's effective policy to the resting corpus: purge when it is
/// OFF, prune to the (possibly lowered) cap when it is ON. Called after every
/// setting write, which is what makes a cap-lower take effect immediately
/// rather than only on the next ingest.
pub fn reconcile_bridge_corpus(conn: &Connection, bridge_id: &str) -> Result<usize> {
    match effective_policy(conn, bridge_id)? {
        None => purge_bridge_content(conn, bridge_id),
        Some(cap) => {
            let content_type =
                fauna_protocol::bridge_search_policy::content_type_for_bridge(bridge_id);
            trim_and_prune(conn, &content_type, cap)
        }
    }
}

/// Drop `bridge_index_map` rows whose FTS row is gone (evicted by the cap).
/// Keeps the linkage table from outliving the corpus it points at.
fn prune_orphan_index_map(conn: &Connection, content_type: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM bridge_index_map \
         WHERE content_type = ?1 \
           AND content_id NOT IN (SELECT content_id FROM content_fts_map)",
        [content_type],
    )
    .context("prune orphan bridge index map rows")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(actors: usize) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        // The full migration set — `users` (which the union rule counts) lives
        // in `migrations.rs`, not the unified schema block.
        crate::db::migrations::run_migrations(&conn).unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO tiers (name, max_inbox_bytes, max_storage_bytes, max_devices, max_blob_size) \
             VALUES ('free', 0, 0, 0, 0)",
            [],
        )
        .unwrap();
        for i in 0..actors {
            conn.execute(
                "INSERT INTO users (actor_id, tier, created_at) VALUES (?1, 'free', 0)",
                rusqlite::params![vec![i as u8; 32]],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn absent_row_reads_the_out_of_the_box_defaults() {
        let conn = setup(1);
        let p = get_policy(&conn, "aa", "nostr").unwrap();
        assert!(p.show_in_search);
        assert_eq!(p.post_limit, DEFAULT_SEARCH_POST_LIMIT);
        // And nest-wide: a fresh nest indexes without anyone configuring it.
        assert_eq!(
            effective_policy(&conn, "nostr").unwrap(),
            Some(DEFAULT_SEARCH_POST_LIMIT)
        );
    }

    #[test]
    fn the_only_actor_turning_it_off_stops_indexing() {
        let conn = setup(1);
        set_policy(
            &conn,
            "aa",
            "nostr",
            SearchPolicy {
                show_in_search: false,
                post_limit: 10,
            },
        )
        .unwrap();
        assert_eq!(effective_policy(&conn, "nostr").unwrap(), None);
    }

    /// ⚠ **A RECOVERY CEREMONY MUST NOT SWITCH THE CORPUS BACK ON.** The live
    /// defect the `bridge_*` pass found, red-verified against the bare
    /// `COUNT(*) FROM users` this function used until 2026-08-15.
    ///
    /// The setup is the single-user nest that has opted OUT — the one shape
    /// where the union rule has no slack. `record_succession` keeps the
    /// predecessor's `users` row as an FK target for attribution and inserts the
    /// successor's, so the actor count goes to 2 while the (moved) policy row
    /// stays at 1, `explicit < total_actors` flips true, and the retired
    /// identity — which is refused everywhere and can never express a preference
    /// — casts an ON default vote for ever.
    ///
    /// Note what this pin is NOT: it is not a test of where the policy row
    /// lives. Moving `bridge_search_policy` with the account is right and
    /// necessary, and it does not close this — the phantom vote comes from the
    /// `users` row, so no per-table succession verdict can reach it.
    #[test]
    fn a_succession_does_not_switch_a_users_own_opt_out_back_on() {
        let conn = setup(1);
        let old = vec![0u8; 32];
        let new = vec![0x5Au8; 32];
        set_policy(
            &conn,
            &hex::encode(&old),
            "nostr",
            SearchPolicy {
                show_in_search: false,
                post_limit: 10,
            },
        )
        .unwrap();
        assert_eq!(
            effective_policy(&conn, "nostr").unwrap(),
            None,
            "sanity: the sole actor's opt-out silences the corpus before the \
             ceremony, or the assert below proves nothing"
        );

        // The ceremony's own two writes on `users`, verbatim from
        // `record_succession`: the predecessor keeps a handle-less row, the
        // successor gets its own. Its policy row moves with it (the registry
        // loop), which is what the second `set_policy` stands in for.
        conn.execute(
            "INSERT INTO users (actor_id, tier, created_at) VALUES (?1, 'free', 0)",
            rusqlite::params![&new],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO actor_successions \
                 (old_actor_id, new_actor_id, statement, seq, succeeded_at) \
             VALUES (?1, ?2, ?3, 1, 1)",
            rusqlite::params![&old, &new, &b"s"[..]],
        )
        .unwrap();
        conn.execute(
            "UPDATE bridge_search_policy SET actor_id = ?2 WHERE actor_id = ?1",
            rusqlite::params![hex::encode(&old), hex::encode(&new)],
        )
        .unwrap();

        assert_eq!(
            effective_policy(&conn, "nostr").unwrap(),
            None,
            "recovering from a key theft must not re-publish the account's \
             bridge content into a nest-wide public corpus it had turned off — \
             a superseded identity is not an actor who could hold an opinion, so \
             it must not vote in the union"
        );
    }

    #[test]
    fn one_actor_off_does_not_silence_an_actor_who_never_chose() {
        // The union rule's whole point: `bb` never touched the setting, so they
        // are on the ON default and the corpus keeps indexing.
        let conn = setup(2);
        set_policy(
            &conn,
            "aa",
            "nostr",
            SearchPolicy {
                show_in_search: false,
                post_limit: 10,
            },
        )
        .unwrap();
        assert_eq!(
            effective_policy(&conn, "nostr").unwrap(),
            Some(DEFAULT_SEARCH_POST_LIMIT)
        );
    }

    #[test]
    fn both_actors_off_purges() {
        let conn = setup(2);
        for actor in ["aa", "bb"] {
            set_policy(
                &conn,
                actor,
                "nostr",
                SearchPolicy {
                    show_in_search: false,
                    post_limit: 10,
                },
            )
            .unwrap();
        }
        assert_eq!(effective_policy(&conn, "nostr").unwrap(), None);
    }

    #[test]
    fn effective_cap_is_the_most_generous_on_actor() {
        let conn = setup(2);
        set_policy(
            &conn,
            "aa",
            "nostr",
            SearchPolicy {
                show_in_search: true,
                post_limit: 50,
            },
        )
        .unwrap();
        set_policy(
            &conn,
            "bb",
            "nostr",
            SearchPolicy {
                show_in_search: true,
                post_limit: 500,
            },
        )
        .unwrap();
        assert_eq!(effective_policy(&conn, "nostr").unwrap(), Some(500));
    }

    #[test]
    fn an_off_actor_does_not_contribute_its_cap() {
        let conn = setup(2);
        set_policy(
            &conn,
            "aa",
            "nostr",
            SearchPolicy {
                show_in_search: true,
                post_limit: 50,
            },
        )
        .unwrap();
        set_policy(
            &conn,
            "bb",
            "nostr",
            SearchPolicy {
                show_in_search: false,
                post_limit: 9_000,
            },
        )
        .unwrap();
        assert_eq!(effective_policy(&conn, "nostr").unwrap(), Some(50));
    }

    #[test]
    fn a_policy_round_trips_and_the_cap_is_clamped() {
        let conn = setup(1);
        set_policy(
            &conn,
            "aa",
            "nostr",
            SearchPolicy {
                show_in_search: true,
                post_limit: u32::MAX,
            },
        )
        .unwrap();
        let p = get_policy(&conn, "aa", "nostr").unwrap();
        assert!(p.show_in_search);
        assert_eq!(
            p.post_limit,
            fauna_protocol::bridge_search_policy::MAX_SEARCH_POST_LIMIT
        );
    }

    #[test]
    fn policies_are_per_bridge() {
        let conn = setup(1);
        set_policy(
            &conn,
            "aa",
            "nostr",
            SearchPolicy {
                show_in_search: false,
                post_limit: 10,
            },
        )
        .unwrap();
        assert_eq!(effective_policy(&conn, "nostr").unwrap(), None);
        assert_eq!(
            effective_policy(&conn, "bluesky").unwrap(),
            Some(DEFAULT_SEARCH_POST_LIMIT)
        );
    }
}
