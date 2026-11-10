//! The **per-actor materialized area** — `<data_dir>/__materialized/<actor_hex>/`,
//! one directory per materialized kind beneath it (`encryption-at-rest.md`
//! § Capability tiering → *The materialization grant*, part (c)).
//!
//! **Not a segment kind.** It is absent from the segment-store kind table, the
//! `SegmentManager` never opens it, and no snapshot, compaction or backup arm
//! reads it: the hourly hot-copy and the daily logical dump copy `nest.db`
//! alone, so the area is outside every self-backup by construction — which is
//! what makes its deletion at revoke real (class 4's third bound). Pinned by
//! `the_area_is_outside_every_self_backup` below.
//!
//! Layout, per kind (`mail/`, `calendar/`, `card/` — [`MatKind::dir_name`]):
//!
//! - `index/` — the readable [`fauna_index::WorkingIndex`] of that kind, with
//!   no stored source text (a snippet is rendered at query time from the
//!   sealed record opened under the held key);
//! - `state.db` — one row: the grant id the kind's view is bound to (each
//!   kind rides its own grant, so revoking one deletes that kind's view
//!   alone), the walk cursor (mail: the last `segment_records.seq` walked; a
//!   DAV kind: the change marker of the last walk), the count of records the
//!   held wraps could not open, and the format version.
//!
//! Every function here is synchronous file work; the materializer runs it
//! under its per-(actor, kind) lock so a revoke's delete never races a flush.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fauna_index::{ContentId, ContentKind, IndexedDoc, WorkingIndex};
use fauna_mls::wrapped_blob::{RecordKind, ScopeTuple};

/// The directory every area lives under, beside `__mail` and the other
/// per-actor roots of the data dir.
pub const MATERIALIZED_ROOT: &str = "__materialized";

/// The area's format. A different stamp on disk is rebuilt from canon (the
/// view is derived state), never migrated. Version 2 split the area by kind;
/// a version-1 area (`<actor>/state.db` beside `<actor>/mail/`) is deleted
/// whole at the boot reconcile and rebuilt.
pub const AREA_FORMAT_VERSION: i64 = 2;

const STATE_DB: &str = "state.db";
const INDEX_DIR: &str = "index";

/// One own-actor kind the materializer can keep readable — the nest-side
/// twin of [`fauna_mls::wrapped_blob::MATERIALIZABLE_KINDS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MatKind {
    /// Inbound mail, under `content.read{mail}`.
    Mail,
    /// CalDAV events, under `content.read{calendar}`.
    Calendar,
    /// CardDAV cards, under `content.read{card}`.
    Card,
}

impl MatKind {
    /// Every kind, in a fixed order.
    pub const ALL: [Self; 3] = [Self::Mail, Self::Calendar, Self::Card];

    /// The grant-scope `kind` this view is materialized under.
    #[must_use]
    pub const fn scope_kind(self) -> &'static str {
        match self {
            Self::Mail => ScopeTuple::KIND_MAIL,
            Self::Calendar => ScopeTuple::KIND_CALENDAR,
            Self::Card => ScopeTuple::KIND_CARD,
        }
    }

    /// Parse a grant-scope `kind`; `None` for a kind not materialized here.
    #[must_use]
    pub fn from_scope_kind(kind: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.scope_kind() == kind)
    }

    /// The kind's directory under the actor's area.
    #[must_use]
    pub const fn dir_name(self) -> &'static str {
        self.scope_kind()
    }

    /// The index content kind its docs carry — the client builder's, so a
    /// nest hit and a device's own hit are the same document.
    #[must_use]
    pub const fn content_kind(self) -> ContentKind {
        match self {
            Self::Mail => ContentKind::Mail,
            Self::Calendar => ContentKind::Calendar,
            Self::Card => ContentKind::Contact,
        }
    }

    /// The `content_type` a `fauna.search.query` hit of this kind carries —
    /// `ContentKind::as_str`'s spelling (`"contact"` for a card, never
    /// `"card"`).
    #[must_use]
    pub fn content_type(self) -> &'static str {
        self.content_kind().as_str()
    }

    /// The sealed-record kind its records rest under.
    #[must_use]
    pub const fn record_kind(self) -> RecordKind {
        match self {
            Self::Mail => RecordKind::Mail,
            Self::Calendar => RecordKind::Calendar,
            Self::Card => RecordKind::Card,
        }
    }
}

/// `<data_dir>/__materialized`.
pub fn root(data_dir: &Path) -> PathBuf {
    data_dir.join(MATERIALIZED_ROOT)
}

/// `<data_dir>/__materialized/<actor_hex>`.
pub fn actor_dir(data_dir: &Path, actor: &[u8; 32]) -> PathBuf {
    root(data_dir).join(hex::encode(actor))
}

/// `<data_dir>/__materialized/<actor_hex>/<kind>`.
pub fn kind_dir(data_dir: &Path, actor: &[u8; 32], kind: MatKind) -> PathBuf {
    actor_dir(data_dir, actor).join(kind.dir_name())
}

/// Every actor that has an area on disk (directory names that are not a
/// 32-byte hex id are left alone — nothing else writes here).
pub fn list_actors(data_dir: &Path) -> Vec<[u8; 32]> {
    let Ok(entries) = std::fs::read_dir(root(data_dir)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let bytes = hex::decode(name.to_str()?).ok()?;
            <[u8; 32]>::try_from(bytes.as_slice()).ok()
        })
        .collect()
}

/// Every `(actor, kind)` view on disk.
pub fn list_areas(data_dir: &Path) -> Vec<([u8; 32], MatKind)> {
    list_actors(data_dir)
        .into_iter()
        .flat_map(|actor| {
            MatKind::ALL
                .into_iter()
                .filter(move |k| kind_dir(data_dir, &actor, *k).exists())
                .map(move |k| (actor, k))
        })
        .collect()
}

/// Whether an actor's area is in the version-1 layout (one `state.db` at the
/// actor's root) — deleted whole by the boot reconcile, then rebuilt.
pub fn is_v1_layout(data_dir: &Path, actor: &[u8; 32]) -> bool {
    actor_dir(data_dir, actor).join(STATE_DB).exists()
}

/// Delete one kind's view of one actor — idempotent (absent is success), as
/// revoke, lapse and the boot reconcile need. The actor's directory goes too
/// once no kind is left in it.
pub fn destroy(data_dir: &Path, actor: &[u8; 32], kind: MatKind) -> Result<()> {
    WorkingIndex::destroy(&kind_dir(data_dir, actor, kind)).with_context(|| {
        format!(
            "delete the materialized {} view of {}",
            kind.dir_name(),
            hex::encode(actor)
        )
    })?;
    // Fails while another kind's view remains — which is the point.
    let _ = std::fs::remove_dir(actor_dir(data_dir, actor));
    Ok(())
}

/// Delete one actor's whole area, every kind — account deletion and a
/// version-1 area.
pub fn destroy_actor(data_dir: &Path, actor: &[u8; 32]) -> Result<()> {
    WorkingIndex::destroy(&actor_dir(data_dir, actor))
        .with_context(|| format!("delete the materialized area of {}", hex::encode(actor)))
}

/// Delete every area — the admin-revoked-holder case.
pub fn destroy_all(data_dir: &Path) -> Result<()> {
    WorkingIndex::destroy(&root(data_dir)).context("delete every materialized area")
}

/// The grant id a view on disk is bound to, without opening its index —
/// the boot reconcile's question. `None` when the view has no readable state
/// (a crash before the first bind, another format), which the reconcile
/// treats as unbound and deletes.
pub fn bound_grant(data_dir: &Path, actor: &[u8; 32], kind: MatKind) -> Option<Vec<u8>> {
    let path = kind_dir(data_dir, actor, kind).join(STATE_DB);
    if !path.exists() {
        return None;
    }
    let conn =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    conn.query_row(
        "SELECT grant_id FROM area WHERE id = 1 AND format_version = ?1",
        [AREA_FORMAT_VERSION],
        |r| r.get::<_, Vec<u8>>(0),
    )
    .ok()
}

/// One open view: one kind of one actor.
pub struct Area {
    dir: PathBuf,
    kind: MatKind,
    state: rusqlite::Connection,
    index: WorkingIndex,
    /// The live docs' identities, content id → the record's nest id (the
    /// mail message id / the DAV record digest): the search merge's source of
    /// the record to open for a snippet and the retire sweep's, kept beside
    /// the index so neither walks the doc store.
    ids: HashMap<Vec<u8>, Vec<u8>>,
    grant_id: Vec<u8>,
    /// Mail: the last `segment_records.seq` the walk has passed. A DAV kind:
    /// the change marker ([`crate::db::CacheDb::materializer_dav_marker`]) the
    /// last complete walk saw.
    pub cursor: i64,
    /// Records the held wraps could not open since the last full walk.
    pub skipped: u64,
    /// The live mail-record count the last retire sweep saw (in memory only:
    /// a restart sweeps once). Unused by a DAV kind, whose walk diffs the
    /// whole corpus.
    pub live_at_last_sweep: Option<i64>,
    /// A DAV kind: the record CIDs the held key could not open since the
    /// last restart (in memory only), so a walk does not retry each one
    /// until a renew or a restart resets them with [`Self::skipped`].
    pub unopened: HashSet<Vec<u8>>,
}

impl Area {
    /// Open `kind`'s view bound to `grant_id`, creating it (or rebuilding it,
    /// when it is bound to another grant, carries another format or cannot be
    /// read) — the view is derived state, so a rebuild is a re-walk of canon.
    pub fn open_bound(
        data_dir: &Path,
        actor: &[u8; 32],
        kind: MatKind,
        grant_id: &[u8],
    ) -> Result<Self> {
        let dir = kind_dir(data_dir, actor, kind);
        match Self::try_open(&dir, kind, grant_id) {
            Ok(Some(area)) => return Ok(area),
            Ok(None) => {}
            Err(e) => tracing::warn!(
                target: "materializer",
                actor = %hex::encode(actor),
                kind = kind.dir_name(),
                "rebuilding an unreadable materialized view: {e:#}"
            ),
        }
        WorkingIndex::destroy(&dir).context("clear a stale materialized view")?;
        Self::create(&dir, kind, grant_id)
    }

    /// `Ok(None)` when the view exists but is bound elsewhere or stale.
    fn try_open(dir: &Path, kind: MatKind, grant_id: &[u8]) -> Result<Option<Self>> {
        if !dir.join(STATE_DB).exists() {
            return Ok(None);
        }
        let state = rusqlite::Connection::open(dir.join(STATE_DB)).context("open state.db")?;
        let row = state
            .query_row(
                "SELECT grant_id, format_version, cursor, skipped FROM area WHERE id = 1",
                [],
                |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .context("read the area row")?;
        if row.0 != grant_id || row.1 != AREA_FORMAT_VERSION {
            return Ok(None);
        }
        let index =
            WorkingIndex::open_or_create(&dir.join(INDEX_DIR)).context("open the working index")?;
        let ids = index
            .index()
            .doc_identities()
            .context("read the index's identities")?
            .into_iter()
            .filter_map(|d| Some((d.content_id.0, d.secondary_id?)))
            .collect();
        Ok(Some(Self {
            dir: dir.to_path_buf(),
            kind,
            state,
            index,
            ids,
            grant_id: grant_id.to_vec(),
            cursor: row.2,
            skipped: u64::try_from(row.3).unwrap_or(0),
            live_at_last_sweep: None,
            unopened: HashSet::new(),
        }))
    }

    fn create(dir: &Path, kind: MatKind, grant_id: &[u8]) -> Result<Self> {
        // `WorkingIndex::open_or_create` makes the directory owner-only; the
        // kind's directory is created the same way first so `state.db` lands
        // in it.
        let index = WorkingIndex::open_or_create(&dir.join(INDEX_DIR))
            .context("create the working index")?;
        let state = rusqlite::Connection::open(dir.join(STATE_DB)).context("create state.db")?;
        state
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS area (
                     id INTEGER PRIMARY KEY CHECK (id = 1),
                     grant_id BLOB NOT NULL,
                     format_version INTEGER NOT NULL,
                     cursor INTEGER NOT NULL,
                     skipped INTEGER NOT NULL
                 );",
            )
            .context("create the area table")?;
        state
            .execute(
                "INSERT OR REPLACE INTO area (id, grant_id, format_version, cursor, skipped)
                 VALUES (1, ?1, ?2, 0, 0)",
                rusqlite::params![grant_id, AREA_FORMAT_VERSION],
            )
            .context("bind the area")?;
        Ok(Self {
            dir: dir.to_path_buf(),
            kind,
            state,
            index,
            ids: HashMap::new(),
            grant_id: grant_id.to_vec(),
            cursor: 0,
            skipped: 0,
            live_at_last_sweep: None,
            unopened: HashSet::new(),
        })
    }

    /// The grant id this view is bound to.
    pub fn grant_id(&self) -> &[u8] {
        &self.grant_id
    }

    /// The kind this view holds.
    pub fn kind(&self) -> MatKind {
        self.kind
    }

    /// The view's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Add (or replace) one doc; its `secondary_id` must be the record's nest
    /// id.
    pub fn add_doc(&mut self, doc: IndexedDoc) -> Result<()> {
        let content_id = doc.content_id.0.clone();
        let secondary = doc.secondary_id.clone();
        self.index.add_doc(doc).context("add a doc")?;
        match secondary {
            Some(s) => {
                self.ids.insert(content_id, s);
            }
            None => {
                self.ids.remove(&content_id);
            }
        }
        Ok(())
    }

    /// Delete one doc by content id.
    pub fn delete_doc(&mut self, content_id: &[u8]) {
        self.index.delete_doc(&ContentId(content_id.to_vec()));
        self.ids.remove(content_id);
    }

    /// The live docs' `(content id, nest id)` pairs.
    pub fn ids(&self) -> impl Iterator<Item = (&Vec<u8>, &Vec<u8>)> {
        self.ids.iter()
    }

    /// The nest id a doc carries.
    pub fn nest_id_of(&self, content_id: &[u8]) -> Option<&[u8]> {
        self.ids.get(content_id).map(Vec::as_slice)
    }

    /// The readable index.
    pub fn index(&self) -> &WorkingIndex {
        &self.index
    }

    /// Commit the index so a query sees every add so far.
    pub fn commit(&mut self) -> Result<()> {
        self.index.commit().context("commit the index")
    }

    /// Write the index snapshot and the cursor — the index first, so a crash
    /// between the two re-walks records already indexed (an idempotent
    /// upsert), never skips one.
    pub fn flush(&mut self) -> Result<()> {
        self.index.flush().context("flush the index")?;
        self.state
            .execute(
                "UPDATE area SET cursor = ?1, skipped = ?2 WHERE id = 1",
                rusqlite::params![self.cursor, i64::try_from(self.skipped).unwrap_or(i64::MAX)],
            )
            .context("save the area cursor")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_index::{FieldKind, IndexedField};

    fn doc(kind: ContentKind, id: &str, nest: u8, body: &str) -> IndexedDoc {
        IndexedDoc {
            kind,
            content_id: ContentId(id.as_bytes().to_vec()),
            timestamp_ns: 1,
            sender_actor_id: None,
            secondary_id: Some(vec![nest; 32]),
            fields: vec![IndexedField {
                kind: FieldKind::Body,
                text: body.into(),
            }],
        }
    }

    #[test]
    fn bind_cursor_round_trip_and_delete() {
        let data = tempfile::tempdir().unwrap();
        let actor = [7u8; 32];
        let mut area = Area::open_bound(data.path(), &actor, MatKind::Mail, b"g1").unwrap();
        area.add_doc(doc(ContentKind::Mail, "<a@x>", 1, "quarterly harvest"))
            .unwrap();
        area.cursor = 42;
        area.skipped = 3;
        area.flush().unwrap();
        drop(area);

        assert_eq!(
            bound_grant(data.path(), &actor, MatKind::Mail),
            Some(b"g1".to_vec())
        );
        assert_eq!(list_actors(data.path()), vec![actor]);
        assert_eq!(list_areas(data.path()), vec![(actor, MatKind::Mail)]);
        let area = Area::open_bound(data.path(), &actor, MatKind::Mail, b"g1").unwrap();
        assert_eq!(area.cursor, 42);
        assert_eq!(area.skipped, 3);
        assert_eq!(area.nest_id_of(b"<a@x>"), Some(&[1u8; 32][..]));
        let hits = area
            .index()
            .query("harvest", &[ContentKind::Mail], None, 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        drop(area);

        destroy(data.path(), &actor, MatKind::Mail).unwrap();
        assert!(!actor_dir(data.path(), &actor).exists());
        destroy(data.path(), &actor, MatKind::Mail).unwrap();
        assert_eq!(bound_grant(data.path(), &actor, MatKind::Mail), None);
    }

    /// Each kind is its own view under its own grant: deleting the calendar
    /// view leaves the card view (and the actor's directory) in place.
    #[test]
    fn each_kind_is_its_own_view_and_deletes_alone() {
        let data = tempfile::tempdir().unwrap();
        let actor = [9u8; 32];
        let mut cal = Area::open_bound(data.path(), &actor, MatKind::Calendar, b"gc").unwrap();
        cal.add_doc(doc(ContentKind::Calendar, "aa", 2, "harvest festival"))
            .unwrap();
        cal.flush().unwrap();
        drop(cal);
        let mut card = Area::open_bound(data.path(), &actor, MatKind::Card, b"gk").unwrap();
        card.add_doc(doc(ContentKind::Contact, "bb", 3, "Ada Lovelace"))
            .unwrap();
        card.flush().unwrap();
        drop(card);
        assert_eq!(
            list_areas(data.path()),
            vec![(actor, MatKind::Calendar), (actor, MatKind::Card)]
        );
        destroy(data.path(), &actor, MatKind::Calendar).unwrap();
        assert_eq!(list_areas(data.path()), vec![(actor, MatKind::Card)]);
        assert_eq!(
            bound_grant(data.path(), &actor, MatKind::Card),
            Some(b"gk".to_vec())
        );
        destroy(data.path(), &actor, MatKind::Card).unwrap();
        assert!(!actor_dir(data.path(), &actor).exists());
    }

    #[test]
    fn rebinding_to_another_grant_rebuilds_from_empty() {
        let data = tempfile::tempdir().unwrap();
        let actor = [8u8; 32];
        let mut area = Area::open_bound(data.path(), &actor, MatKind::Mail, b"g1").unwrap();
        area.add_doc(doc(ContentKind::Mail, "<a@x>", 1, "words"))
            .unwrap();
        area.cursor = 9;
        area.flush().unwrap();
        drop(area);
        let area = Area::open_bound(data.path(), &actor, MatKind::Mail, b"g2").unwrap();
        assert_eq!(area.grant_id(), b"g2");
        assert_eq!(area.cursor, 0);
        assert_eq!(area.ids().count(), 0);
    }

    #[test]
    fn a_card_hit_is_a_contact_and_kinds_round_trip() {
        assert_eq!(MatKind::Card.content_type(), "contact");
        assert_eq!(MatKind::Calendar.content_type(), "calendar");
        assert_eq!(MatKind::Mail.content_type(), "mail");
        for k in MatKind::ALL {
            assert_eq!(MatKind::from_scope_kind(k.scope_kind()), Some(k));
        }
        assert_eq!(MatKind::from_scope_kind("post"), None);
        // The nest-side list is exactly the shared materializable list.
        assert_eq!(
            MatKind::ALL.map(MatKind::scope_kind).to_vec(),
            fauna_mls::wrapped_blob::MATERIALIZABLE_KINDS.to_vec()
        );
    }

    /// Class 4's third bound: the area is outside every self-backup. The
    /// hourly hot-copy and the daily logical dump are both copies of
    /// `nest.db` (a SQLite backup / a row dump), never a walk of the data
    /// dir — so a view resting beside them cannot ride into a backup. Pinned
    /// by name, so a backup arm that starts walking `__materialized` has to
    /// delete this test to land.
    #[test]
    fn the_area_is_outside_every_self_backup() {
        let sources = [
            include_str!("../backup/service.rs"),
            include_str!("../export/logical.rs"),
            include_str!("../segments/backup_source.rs"),
        ];
        for src in sources {
            assert!(
                !src.contains(MATERIALIZED_ROOT),
                "a backup source names the materialized area"
            );
        }
    }
}
