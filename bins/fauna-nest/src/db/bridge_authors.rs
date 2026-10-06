//! The face of a synthetic (bridged) author — the one cross-bridge projection
//! behind `FeedPostItem.author_display`.
//!
//! Owner doc: `docs/goal/behavior/bridges.md` § Unified feed ingestion →
//! *Bridged authors* (ruled 2026-09-26). Every bridged post rests under a
//! synthetic `ActorId` no `Profile` will ever be signed for, so
//! `fauna.profile.get` answers `not_found` for one and, before this table,
//! every app painted the raw hex. The three content bridges each already learn
//! the author's handle, display name and avatar at one transit point — the
//! ActivityPub actor cache, a followed nostr author's kind-0 metadata, the
//! `author` view on every ingested Bluesky post — and each writes it **here**,
//! in one shape, through [`upsert`]; the feed handlers read it back with
//! [`get_many`] after every local page query and project it as
//! [`fauna_protocol::feed::AuthorDisplay`].
//!
//! This module is bridge-agnostic on purpose: the per-bridge mapping (which
//! field is the handle, what the external id is, which proxy the avatar rides)
//! lives beside each bridge's writer — `activitypub::db_helpers`,
//! `nostr::inbound_lifecycle`, `bluesky::feed_ingest` — so a fourth bridge adds
//! a fourth writer and touches nothing here.
//!
//! **Newest wins.** [`upsert`] replaces a row only when the incoming
//! `updated_at` is not older than the stored one: a writer whose source carries
//! its own timestamp (a kind-0 event's `created_at`) passes it, so a lagging
//! relay's stale profile never regresses a fresher one; a writer without one
//! (an actor fetch, a AT Protocol page) passes the write instant.
//!
//! **Derived, recreatable, not user data** — see `SCHEMA_BRIDGE_AUTHORS`.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use fauna_protocol::feed::AuthorDisplay;

/// The `bridge` column's value for each content bridge — the bridge id the
/// registry and the post's `source` token already use.
pub const BRIDGE_ACTIVITYPUB: &str = "activitypub";
pub const BRIDGE_NOSTR: &str = "nostr";
pub const BRIDGE_BLUESKY: &str = "bluesky";

/// One projected row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeAuthor {
    /// The synthetic `ActorId` the bridge's posts rest under.
    pub actor_id: [u8; 32],
    /// [`BRIDGE_ACTIVITYPUB`] / [`BRIDGE_NOSTR`] / [`BRIDGE_BLUESKY`].
    pub bridge: String,
    /// The bridge's own identity for the author: the actor URI, the hex
    /// pubkey, the DID — what the synthetic id was derived from.
    pub external_id: String,
    /// The bridge's user-facing handle (`@user@host`, a NIP-05 address or the
    /// kind-0 `name`, `alice.bsky.social`).
    pub handle: Option<String>,
    pub display_name: Option<String>,
    /// Nest-relative and **already proxied** — never the remote origin.
    pub avatar_url: Option<String>,
    /// Epoch micros — the source's own timestamp when it has one, else the
    /// write instant.
    pub updated_at: i64,
}

impl BridgeAuthor {
    /// The wire face: the three display fields, trimmed, empty → absent.
    pub fn display(&self) -> AuthorDisplay {
        AuthorDisplay {
            handle: present(self.handle.as_deref()),
            display_name: present(self.display_name.as_deref()),
            avatar_url: present(self.avatar_url.as_deref()),
            extra: Default::default(),
        }
    }

    /// `true` when at least one display field is set — a row that would paint
    /// nothing is not worth a write.
    pub fn has_face(&self) -> bool {
        let d = self.display();
        d.handle.is_some() || d.display_name.is_some() || d.avatar_url.is_some()
    }
}

fn present(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

/// Write `author` unless a newer row already rests. Returns `true` when the
/// row was inserted or replaced. A faceless author (no display field) is
/// skipped without touching the table — a projection row that paints nothing
/// would only shadow a later, fuller one at the same `updated_at`.
pub fn upsert(conn: &Connection, author: &BridgeAuthor) -> Result<bool> {
    if !author.has_face() {
        return Ok(false);
    }
    let face = author.display();
    let n = conn.execute(
        "INSERT INTO bridge_authors
             (actor_id, bridge, external_id, handle, display_name, avatar_url, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(actor_id) DO UPDATE SET
             bridge = excluded.bridge,
             external_id = excluded.external_id,
             handle = excluded.handle,
             display_name = excluded.display_name,
             avatar_url = excluded.avatar_url,
             updated_at = excluded.updated_at
         WHERE excluded.updated_at >= bridge_authors.updated_at",
        params![
            author.actor_id.as_slice(),
            author.bridge,
            author.external_id,
            face.handle,
            face.display_name,
            face.avatar_url,
            author.updated_at,
        ],
    )?;
    Ok(n > 0)
}

fn row_to_author(row: &rusqlite::Row<'_>) -> rusqlite::Result<BridgeAuthor> {
    let id: Vec<u8> = row.get(0)?;
    let actor_id: [u8; 32] = id.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Blob,
            "bridge_authors.actor_id is not 32 bytes".into(),
        )
    })?;
    Ok(BridgeAuthor {
        actor_id,
        bridge: row.get(1)?,
        external_id: row.get(2)?,
        handle: row.get(3)?,
        display_name: row.get(4)?,
        avatar_url: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

const SELECT: &str =
    "SELECT actor_id, bridge, external_id, handle, display_name, avatar_url, updated_at
     FROM bridge_authors";

/// One author's row, if projected.
pub fn get(conn: &Connection, actor_id: &[u8; 32]) -> Result<Option<BridgeAuthor>> {
    Ok(conn
        .query_row(
            &format!("{SELECT} WHERE actor_id = ?1"),
            [actor_id.as_slice()],
            row_to_author,
        )
        .optional()?)
}

/// The rows for a page's distinct authors — one batched read per page, the
/// feed handlers' post-query decoration (never a JOIN in `query_feed`). Ids
/// with no row are simply absent from the map (a native author).
pub fn get_many(
    conn: &Connection,
    actor_ids: &[[u8; 32]],
) -> Result<HashMap<[u8; 32], BridgeAuthor>> {
    let mut out = HashMap::new();
    // Well under SQLite's bound-variable floor; a feed page is ≤ 200 rows.
    for chunk in actor_ids.chunks(100) {
        let marks = (1..=chunk.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = conn.prepare(&format!("{SELECT} WHERE actor_id IN ({marks})"))?;
        let ids: Vec<&[u8]> = chunk.iter().map(|id| id.as_slice()).collect();
        let rows = stmt.query_map(rusqlite::params_from_iter(ids), row_to_author)?;
        for row in rows {
            let author = row?;
            out.insert(author.actor_id, author);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::db::schema::SCHEMA_BRIDGE_AUTHORS)
            .unwrap();
        conn
    }

    fn author(id: u8, display_name: &str, updated_at: i64) -> BridgeAuthor {
        BridgeAuthor {
            actor_id: [id; 32],
            bridge: BRIDGE_ACTIVITYPUB.into(),
            external_id: format!("https://remote.example/users/{id}"),
            handle: Some(format!("@u{id}@remote.example")),
            display_name: Some(display_name.into()),
            avatar_url: None,
            updated_at,
        }
    }

    #[test]
    fn upsert_keeps_the_newest_row() {
        let c = conn();
        assert!(upsert(&c, &author(1, "Old", 10)).unwrap());
        assert!(upsert(&c, &author(1, "New", 20)).unwrap());
        assert_eq!(
            get(&c, &[1u8; 32])
                .unwrap()
                .unwrap()
                .display_name
                .as_deref(),
            Some("New")
        );
        // A lagging source (older stamp) never regresses the face.
        assert!(!upsert(&c, &author(1, "Stale", 15)).unwrap());
        assert_eq!(
            get(&c, &[1u8; 32])
                .unwrap()
                .unwrap()
                .display_name
                .as_deref(),
            Some("New")
        );
        // The same stamp is a refresh, not a regression.
        assert!(upsert(&c, &author(1, "Same", 20)).unwrap());
        assert_eq!(
            get(&c, &[1u8; 32])
                .unwrap()
                .unwrap()
                .display_name
                .as_deref(),
            Some("Same")
        );
    }

    #[test]
    fn a_faceless_author_is_not_written_and_blank_fields_read_as_absent() {
        let c = conn();
        let blank = BridgeAuthor {
            handle: Some("   ".into()),
            display_name: None,
            avatar_url: Some("".into()),
            ..author(2, "", 5)
        };
        assert!(!blank.has_face());
        assert!(!upsert(&c, &blank).unwrap());
        assert!(get(&c, &[2u8; 32]).unwrap().is_none());

        let padded = BridgeAuthor {
            handle: Some("  @pad@remote.example ".into()),
            display_name: Some(" ".into()),
            ..author(3, "", 5)
        };
        assert!(upsert(&c, &padded).unwrap());
        let face = get(&c, &[3u8; 32]).unwrap().unwrap().display();
        assert_eq!(face.handle.as_deref(), Some("@pad@remote.example"));
        assert!(face.display_name.is_none());
    }

    #[test]
    fn get_many_returns_only_the_projected_ids() {
        let c = conn();
        upsert(&c, &author(4, "Four", 1)).unwrap();
        upsert(&c, &author(5, "Five", 1)).unwrap();
        let got = get_many(&c, &[[4u8; 32], [6u8; 32], [5u8; 32], [4u8; 32]]).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[&[4u8; 32]].display_name.as_deref(), Some("Four"));
        assert_eq!(got[&[5u8; 32]].display_name.as_deref(), Some("Five"));
        assert!(!got.contains_key(&[6u8; 32]));
        assert!(get_many(&c, &[]).unwrap().is_empty());
    }
}
