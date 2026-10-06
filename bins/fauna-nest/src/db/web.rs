//! Web content hosting — file store, rendered output, and custom domain CRUD.

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use super::{CacheDb, now_epoch_millis};
use crate::db::links;

// ==================== Row types ====================

/// One render's claim on one actor's rendered stores
/// (`web-content-hosting.md` § Routing, render, serving → *One render writes at
/// a time*). Minted by [`CacheDb::begin_web_render`] immediately before the
/// render's listing, and carried by every write that render makes.
///
/// It is the only key to `web_rendered` / `web_rendered_sealed`, which is what
/// makes the rule structural rather than remembered: a write has no way to name
/// an actor except through a claim, and a claim an older listing minted stops
/// working the moment a newer render or a fail-closed clear bumps the
/// generation.
#[derive(Debug, Clone, Copy)]
pub struct RenderClaim {
    actor_id: [u8; 32],
    generation: i64,
}

impl RenderClaim {
    /// The actor whose site this render is writing.
    pub fn actor_id(&self) -> &[u8; 32] {
        &self.actor_id
    }
}

/// Is `claim` still the actor's current render generation? Read on the caller's
/// own connection lock, so the answer cannot go stale before the write it
/// guards — see [`CacheDb::under_render_claim`].
fn claim_is_current(conn: &rusqlite::Connection, claim: &RenderClaim) -> Result<bool> {
    let current: Option<i64> = conn
        .query_row(
            "SELECT generation FROM web_render_generation WHERE actor_id = ?1",
            rusqlite::params![claim.actor_id.as_slice()],
            |row| row.get(0),
        )
        .optional()
        .context("read web render generation")?;
    Ok(current == Some(claim.generation))
}

/// A row from `web_files` or `web_rendered`.
///
/// `blob_hash` means different things in the two tables, and the serve path
/// discriminates on which one it came from: a `web_files` row's hash is the
/// sync change's **manifest** hash (walk manifest → chunks), while a
/// `web_rendered` row's is the rendered **body** blob, stored verbatim.
#[derive(Debug, Clone)]
pub struct WebFileRow {
    pub actor_id: Vec<u8>,
    pub path: String,
    pub blob_hash: Vec<u8>,
    pub content_type: String,
    pub updated_at: i64,
    /// `web_files` only (`None` for a `web_rendered` row): the `web`-mode file
    /// set this file was synced from — the set name (grant-scope qualifier) and
    /// `web_paywall_tier` resolve through it.
    pub folder_id: Option<i64>,
    /// `web_files` only: the M2 content-key generation the chunks are sealed
    /// under. `None` = plaintext. `Some(v)` = the row is sealed and serves ONLY
    /// through the paywall token gate (monetization.md § Pillar 2).
    pub content_key_version: Option<i64>,
}

impl WebFileRow {
    /// Whether this file's chunks are content-key-sealed (the paywalled-set
    /// path). A sealed row is never served as ciphertext — see
    /// `web_content::serve`'s fail-closed rule.
    pub fn is_sealed(&self) -> bool {
        self.content_key_version.is_some()
    }
}

/// A row from `web_rendered_sealed` (web paywall, Pillar 2): a sealed
/// rendered page plus the serve-time key-derivation inputs (`tier`,
/// `post_id`).
#[derive(Debug, Clone)]
pub struct WebRenderedSealedRow {
    pub path: String,
    pub blob_hash: Vec<u8>,
    pub content_type: String,
    pub tier: String,
    pub post_id: Vec<u8>,
}

/// One `web_rendered` row a render has produced and not yet written: the body
/// is already in the blob store, the row waits for the render's one replacing
/// transaction ([`CacheDb::replace_web_rendered`]).
#[derive(Debug, Clone)]
pub struct RenderedPage {
    pub path: String,
    pub blob_hash: [u8; 32],
    pub content_type: String,
}

/// [`RenderedPage`] for `web_rendered_sealed` — a sealed row carries the
/// serve-time key-derivation inputs besides, and rides the same transaction.
#[derive(Debug, Clone)]
pub struct RenderedSealedPage {
    pub path: String,
    pub blob_hash: [u8; 32],
    pub content_type: String,
    pub tier: String,
    pub post_id: [u8; 32],
}

/// A row from `web_domains`.
#[derive(Debug, Clone)]
pub struct WebDomainRow {
    pub actor_id: Vec<u8>,
    pub domain: String,
    pub verify_token: String,
    pub status: String,
    pub created_at: i64,
    pub verified_at: Option<i64>,
}

// ==================== Helpers ====================

/// The `web_files` column list every read below selects, in mapper order.
const WEB_FILE_COLS: &str =
    "actor_id, path, blob_hash, content_type, updated_at, folder_id, content_key_version";

fn map_web_file_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WebFileRow> {
    Ok(WebFileRow {
        actor_id: row.get(0)?,
        path: row.get(1)?,
        blob_hash: row.get(2)?,
        content_type: row.get(3)?,
        updated_at: row.get(4)?,
        folder_id: row.get(5)?,
        content_key_version: row.get(6)?,
    })
}

/// `web_rendered` has no set/seal columns — rendered output is the nest's own
/// product, always stored verbatim (a *gated* page's sealed twin lives in
/// `web_rendered_sealed`, a separate table).
fn map_web_rendered_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WebFileRow> {
    Ok(WebFileRow {
        actor_id: row.get(0)?,
        path: row.get(1)?,
        blob_hash: row.get(2)?,
        content_type: row.get(3)?,
        updated_at: row.get(4)?,
        folder_id: None,
        content_key_version: None,
    })
}

fn map_web_domain_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WebDomainRow> {
    Ok(WebDomainRow {
        actor_id: row.get(0)?,
        domain: row.get(1)?,
        verify_token: row.get(2)?,
        status: row.get(3)?,
        created_at: row.get(4)?,
        verified_at: row.get(5)?,
    })
}

/// Record that `actor_id`'s rendered site owes a render — **called inside the
/// transaction of the state change that removes content from it**, never
/// after the commit: that ordering is the whole point
/// (`web-content-hosting.md` § Routing, render, serving → *A revoke is
/// durable*). Takes the open transaction (or the locked connection of a
/// single-statement door) rather than `&CacheDb` so no caller can mark from
/// outside one. Every mark re-rolls the nonce — see
/// [`CacheDb::discharge_web_render_owed`].
pub(super) fn mark_web_render_owed(conn: &rusqlite::Connection, actor_id: &[u8]) -> Result<()> {
    conn.execute(
        "INSERT INTO web_render_owed (actor_id, nonce, owed_at) VALUES (?1, random(), ?2)
         ON CONFLICT(actor_id) DO UPDATE SET nonce = random(), owed_at = ?2",
        rusqlite::params![actor_id, now_epoch_millis()],
    )
    .context("mark web render owed")?;
    Ok(())
}

/// [`mark_web_render_owed`] for whoever has `post_id` web-published — a no-op
/// for an unpublished post, the common case. For the doors that take a POST
/// off a site (its deletion, a legal takedown): they decide off the post's own
/// `web_published` link inside their transaction, never by a read before it,
/// which a retry would find already cascaded away.
pub(super) fn mark_web_render_owed_for_post(
    conn: &rusqlite::Connection,
    post_id: &[u8; 32],
) -> Result<()> {
    let publishers: Vec<Vec<u8>> = conn
        .prepare(
            "SELECT DISTINCT actor_id FROM content_links \
             WHERE link_type = 'web_published' AND source_id = ?1 AND actor_id IS NOT NULL",
        )
        .context("prepare web_published publishers")?
        .query_map(rusqlite::params![post_id.as_slice()], |row| row.get(0))
        .context("query web_published publishers")?
        .collect::<rusqlite::Result<_>>()
        .context("read web_published publishers")?;
    for publisher in &publishers {
        mark_web_render_owed(conn, publisher)?;
    }
    Ok(())
}

/// [`mark_web_render_owed`] for **every** actor with a published web post, in
/// one statement — the mark the region tier's *policy-moving writes* carry
/// inside their own transaction (`db/region_tier.rs`: the `nest_region`
/// declaration write, `put_relay_artifact`, `retire_relay_artifact`).
///
/// A change of the content policy in force for the declared situs re-renders
/// every publishing site, so every one of them is owed that render from the
/// instant the policy moves — not from the instant the walk starts. Like
/// [`mark_web_render_owed`] it takes the open transaction rather than the
/// database handle, so no caller can mark from outside one.
pub(super) fn mark_web_render_owed_for_publishing_actors(
    conn: &rusqlite::Connection,
) -> Result<()> {
    conn.execute(
        "INSERT INTO web_render_owed (actor_id, nonce, owed_at)
         SELECT DISTINCT actor_id, random(), ?1 FROM content_links
         WHERE link_type = 'web_published' AND actor_id IS NOT NULL
         ON CONFLICT(actor_id) DO UPDATE SET nonce = random(), owed_at = ?1",
        rusqlite::params![now_epoch_millis()],
    )
    .context("mark publishing actors render owed")?;
    Ok(())
}

fn upsert_web_file_on(
    conn: &rusqlite::Connection,
    actor_id: &[u8; 32],
    path: &str,
    blob_hash: &[u8; 32],
    content_type: &str,
    folder_id: Option<i64>,
    content_key_version: Option<i64>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO web_files (actor_id, path, blob_hash, content_type, updated_at, folder_id, content_key_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(actor_id, path) DO UPDATE SET blob_hash = ?3, content_type = ?4, updated_at = ?5, folder_id = ?6, content_key_version = ?7",
        rusqlite::params![
            actor_id.as_slice(),
            path,
            blob_hash.as_slice(),
            content_type,
            now_epoch_millis(),
            folder_id,
            content_key_version
        ],
    )
    .context("upsert web file")?;
    Ok(())
}

fn delete_web_file_on(
    conn: &rusqlite::Connection,
    actor_id: &[u8; 32],
    path: &str,
) -> Result<bool> {
    let n = conn
        .execute(
            "DELETE FROM web_files WHERE actor_id = ?1 AND path = ?2",
            rusqlite::params![actor_id.as_slice(), path],
        )
        .context("delete web file")?;
    Ok(n > 0)
}

// ==================== CacheDb impls ====================

impl CacheDb {
    // ---- web_files ----

    /// Upsert a web file record (INSERT OR REPLACE).
    ///
    /// `blob_hash` is the change's **manifest** hash. `folder_id` is the
    /// `web`-mode set it came from; `content_key_version` is the M2 generation
    /// its chunks are sealed under (`None` = plaintext). Both are re-written on
    /// every upsert, so a set that is later paywalled (its files re-sealed and
    /// re-synced under a content key) transitions its rows plaintext → sealed —
    /// and a re-plaintexted file transitions back — with no separate migration.
    pub async fn upsert_web_file(
        &self,
        actor_id: &[u8; 32],
        path: &str,
        blob_hash: &[u8; 32],
        content_type: &str,
        folder_id: Option<i64>,
        content_key_version: Option<i64>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        upsert_web_file_on(
            &conn,
            actor_id,
            path,
            blob_hash,
            content_type,
            folder_id,
            content_key_version,
        )
    }

    /// [`Self::upsert_web_file`] for a **render input** (a `.html.hbs`
    /// template or `_site.json`), with the owed-render marker in the same
    /// transaction. An edited template can REMOVE content from the pages it
    /// renders, and the nest cannot tell an edit that adds from one that
    /// withdraws — so every render-input change is a revoking door
    /// (`web-content-hosting.md` § Routing, render, serving → *A revoke is
    /// durable*).
    pub async fn upsert_web_render_input(
        &self,
        actor_id: &[u8; 32],
        path: &str,
        blob_hash: &[u8; 32],
        content_type: &str,
        folder_id: Option<i64>,
        content_key_version: Option<i64>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction()?;
        upsert_web_file_on(
            &tx,
            actor_id,
            path,
            blob_hash,
            content_type,
            folder_id,
            content_key_version,
        )?;
        mark_web_render_owed(&tx, actor_id)?;
        tx.commit().context("commit upsert_web_render_input")
    }

    /// Get a single web file by actor and path.
    pub async fn get_web_file(
        &self,
        actor_id: &[u8; 32],
        path: &str,
    ) -> Result<Option<WebFileRow>> {
        let actor_id = *actor_id;
        let path = path.to_string();
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {WEB_FILE_COLS} FROM web_files WHERE actor_id = ?1 AND path = ?2"),
            rusqlite::params![actor_id.as_slice(), path],
            map_web_file_row,
        )
        .optional()
        .context("get web file")
    }

    /// List all web files for an actor.
    pub async fn list_web_files(&self, actor_id: &[u8; 32]) -> Result<Vec<WebFileRow>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {WEB_FILE_COLS} FROM web_files WHERE actor_id = ?1 ORDER BY path"
            ))
            .context("prepare list_web_files")?;
        let rows = stmt
            .query_map(rusqlite::params![actor_id.as_slice()], map_web_file_row)
            .context("query web_files")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("read web file row")?);
        }
        Ok(out)
    }

    /// List web files for an actor whose path ends with the given extension.
    /// `ext` should include the leading dot, e.g. `".hbs"` or `".html"`.
    pub async fn list_web_files_by_ext(
        &self,
        actor_id: &[u8; 32],
        ext: &str,
    ) -> Result<Vec<WebFileRow>> {
        let actor_id = *actor_id;
        let pattern = format!("%{ext}");
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {WEB_FILE_COLS} FROM web_files \
                 WHERE actor_id = ?1 AND path LIKE ?2 ORDER BY path"
            ))
            .context("prepare list_web_files_by_ext")?;
        let rows = stmt
            .query_map(
                rusqlite::params![actor_id.as_slice(), pattern],
                map_web_file_row,
            )
            .context("query web_files by ext")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("read web file row")?);
        }
        Ok(out)
    }

    /// Delete a web file by actor and path. Returns true if a row was removed.
    pub async fn delete_web_file(&self, actor_id: &[u8; 32], path: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        delete_web_file_on(&conn, actor_id, path)
    }

    /// [`Self::delete_web_file`] for a **render input**, with the owed-render
    /// marker in the same transaction when a row really went — a deleted
    /// template's rendered page must not outlive it (see
    /// [`Self::upsert_web_render_input`]).
    pub async fn delete_web_render_input(&self, actor_id: &[u8; 32], path: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction()?;
        let removed = delete_web_file_on(&tx, actor_id, path)?;
        if removed {
            mark_web_render_owed(&tx, actor_id)?;
        }
        tx.commit().context("commit delete_web_render_input")?;
        Ok(removed)
    }

    /// Drop the folder's **sealed** `web_files` rows whose path is not in
    /// `keep` — the sealed twin of `reconcile_web_files_projection`'s drop
    /// half. Returns how many rows went.
    ///
    /// **Why the caller's set, and not the nest's own evidence.** The nest
    /// reconciles the *plaintext* class from `live_plaintext_heads_for_folder`,
    /// which is real evidence. A **sealed** head rests no plaintext path (S9),
    /// so that fold can never name one — which is why the reconcile's drop half
    /// deliberately skips sealed rows: their absence from it is ignorance, not
    /// evidence. The owner's own client is the only party that can enumerate
    /// them, and this is it doing so.
    ///
    /// **Sealed-only is a bound, not an oversight.** A caller cannot use this to
    /// wipe the plaintext projection — that class has its own reconcile with its
    /// own evidence, and a client's view of it is not authoritative.
    pub async fn prune_sealed_web_files(
        &self,
        actor_id: &[u8; 32],
        folder_id: i64,
        keep: &std::collections::HashSet<String>,
    ) -> Result<usize> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {WEB_FILE_COLS} FROM web_files \
                 WHERE actor_id = ?1 AND folder_id = ?2 AND content_key_version IS NOT NULL"
            ))
            .context("prepare prune_sealed_web_files")?;
        let doomed: Vec<String> = stmt
            .query_map(
                rusqlite::params![actor_id.as_slice(), folder_id],
                map_web_file_row,
            )
            .context("query sealed web_files")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("read sealed web file rows")?
            .into_iter()
            .filter(|row| !keep.contains(&row.path))
            .map(|row| row.path)
            .collect();
        drop(stmt);
        let mut dropped = 0usize;
        for path in &doomed {
            dropped += conn
                .execute(
                    "DELETE FROM web_files \
                     WHERE actor_id = ?1 AND path = ?2 AND folder_id = ?3 \
                       AND content_key_version IS NOT NULL",
                    rusqlite::params![actor_id.as_slice(), path, folder_id],
                )
                .context("delete stale sealed web file")?;
        }
        Ok(dropped)
    }

    // ---- web_render_generation (one render writes at a time) ----

    /// Open this render's **claim** on `actor_id`'s rendered stores: bump the
    /// actor's render generation and hand back the value every write of this
    /// render carries (`web-content-hosting.md` § Routing, render, serving →
    /// *One render writes at a time*).
    ///
    /// Called once per render, immediately BEFORE its listing. The listing is
    /// what a render's whole output is a function of, so the render that listed
    /// LAST is the one whose pages are current — whatever order the two happen
    /// to finish in, and however long the older one has been running.
    ///
    /// The bump also deletes the actor's staged bodies, in the same transaction:
    /// they belong to the render this claim supersedes, which can no longer
    /// commit them, so they are orphans the blob sweep should collect past grace
    /// (`backup-restore.md` § 9 step 2h).
    pub async fn begin_web_render(&self, actor_id: &[u8; 32]) -> Result<RenderClaim> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction()?;
        let generation: i64 = tx
            .query_row(
                "INSERT INTO web_render_generation (actor_id, generation) VALUES (?1, 1)
                 ON CONFLICT(actor_id) DO UPDATE SET generation = generation + 1
                 RETURNING generation",
                rusqlite::params![actor_id.as_slice()],
                |row| row.get(0),
            )
            .context("begin web render")?;
        tx.execute(
            "DELETE FROM web_render_staged WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
        )
        .context("begin web render: drop the superseded render's staged bodies")?;
        tx.commit().context("commit begin_web_render")?;
        Ok(RenderClaim {
            actor_id,
            generation,
        })
    }

    /// **Stage one body this render is about to store**, under its claim: the
    /// blob sweep's reference for a body the render has written to the store but
    /// not yet committed to the site (`backup-restore.md` § 9 step 2h). Called
    /// BEFORE the blob write, so no sweep that could see the body's
    /// `blob_metadata` row can miss its reference; [`Self::replace_web_rendered`]
    /// deletes the rows in the transaction that hands the reference to the
    /// rendered rows.
    ///
    /// `Ok(false)` = superseded, nothing staged — and the caller stores nothing
    /// either. Being the claim check the render reads before each blob write is
    /// deliberate: a render that has lost the site should neither pin nor spend
    /// IO on a page nobody will serve.
    pub async fn stage_web_render_body(
        &self,
        claim: &RenderClaim,
        blob_hash: &[u8; 32],
    ) -> Result<bool> {
        let actor_id = claim.actor_id;
        let blob_hash = *blob_hash;
        self.under_render_claim(claim, move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO web_render_staged (actor_id, blob_hash) VALUES (?1, ?2)",
                rusqlite::params![actor_id.as_slice(), blob_hash.as_slice()],
            )
            .context("stage web render body")?;
            Ok(())
        })
        .await
    }

    /// Every body hash a rendered site references: the live pages of both
    /// stores, public and sealed, plus the bodies a render still holding its
    /// site has staged. The blob sweep's step 2h reads this — a body listed here
    /// is never a deletion candidate (`backup-restore.md` § 9 step 2h).
    pub async fn list_web_render_referenced_hashes(&self) -> Result<Vec<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT blob_hash FROM web_rendered
                 UNION SELECT blob_hash FROM web_rendered_sealed
                 UNION SELECT blob_hash FROM web_render_staged",
            )
            .context("prepare list_web_render_referenced_hashes")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .context("query list_web_render_referenced_hashes")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("read list_web_render_referenced_hashes row")?);
        }
        Ok(out)
    }

    /// Whether a newer render's listing — or a fail-closed clear — has taken
    /// `claim`'s site over.
    ///
    /// **Advisory, for abandoning EARLY**: a superseded render should stop
    /// before spending another template's 5-second budget or another blob
    /// write, since everything it would write is about to be, or has already
    /// been, replaced. What actually WITHHOLDS a stale write is
    /// [`Self::under_render_claim`], which re-checks under the connection lock
    /// it writes through — so no check-then-write race can slip past it.
    pub async fn web_render_superseded(&self, claim: &RenderClaim) -> Result<bool> {
        let conn = self.conn.lock().await;
        Ok(!claim_is_current(&conn, claim)?)
    }

    /// Run one write against the rendered stores **only while `claim` is still
    /// the actor's current render generation**, holding the connection lock
    /// across the check and the write so no other render can slip between them.
    /// `Ok(false)` = superseded, nothing written.
    ///
    /// ⚠ **Every** mutation of `web_rendered` / `web_rendered_sealed` goes
    /// through here — the clears as much as the upserts. A write added beside
    /// this helper instead of through it reopens the defect this fence
    /// closes: a render that
    /// listed before a revoke putting the revoked page back after the revoke's
    /// own render cleared it. The one deliberate exception is
    /// [`Self::clear_web_rendered_owing_restore`], the fail-closed clear, which
    /// carries no claim because it is the safety act that supersedes every
    /// render in flight.
    async fn under_render_claim<F>(&self, claim: &RenderClaim, write: F) -> Result<bool>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<()>,
    {
        let conn = self.conn.lock().await;
        if !claim_is_current(&conn, claim)? {
            return Ok(false);
        }
        write(&conn)?;
        Ok(true)
    }

    // ---- web_rendered ----

    /// Upsert a rendered file record, under this render's claim (see
    /// [`Self::under_render_claim`]). `Ok(false)` = a newer render or a
    /// fail-closed clear owns the site now and this page was NOT written.
    ///
    /// **Not the render's writer**: a render lands every row at once through
    /// [`Self::replace_web_rendered`], so no reader sees a site half-written.
    /// This one-row form is what a test seeds a rendered site with.
    pub async fn upsert_web_rendered(
        &self,
        claim: &RenderClaim,
        path: &str,
        blob_hash: &[u8; 32],
        content_type: &str,
    ) -> Result<bool> {
        let actor_id = claim.actor_id;
        let blob_hash = *blob_hash;
        let path = path.to_string();
        let content_type = content_type.to_string();
        let now = now_epoch_millis();
        self.under_render_claim(claim, move |conn| {
            conn.execute(
                "INSERT INTO web_rendered (actor_id, path, blob_hash, content_type, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(actor_id, path) DO UPDATE SET blob_hash = ?3, content_type = ?4, updated_at = ?5",
                rusqlite::params![actor_id.as_slice(), path, blob_hash.as_slice(), content_type, now],
            )
            .context("upsert web rendered")?;
            Ok(())
        })
        .await
    }

    /// Get a single rendered file by actor and path.
    pub async fn get_web_rendered(
        &self,
        actor_id: &[u8; 32],
        path: &str,
    ) -> Result<Option<WebFileRow>> {
        let actor_id = *actor_id;
        let path = path.to_string();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT actor_id, path, blob_hash, content_type, updated_at
             FROM web_rendered WHERE actor_id = ?1 AND path = ?2",
            rusqlite::params![actor_id.as_slice(), path],
            map_web_rendered_row,
        )
        .optional()
        .context("get web rendered")
    }

    /// **Replace an actor's whole rendered site — both stores — in ONE
    /// transaction, under this render's claim**: a render's last act, and the
    /// only write it makes (`web-content-hosting.md` § Routing, render, serving
    /// → *A reader sees a whole site or none*). A request arriving mid-render
    /// therefore reads the site the last completed render wrote, then this
    /// one's, and never a mixture — until this was one statement the render
    /// cleared first and upserted page by page behind awaits, and every reader
    /// in between saw a cleared-but-unwritten site.
    ///
    /// Both stores in the one transaction, for the reason one clear always
    /// covered both: a public page replaced while a formerly-gated post's
    /// sealed page still stands behind it is the hole that one call closed.
    ///
    /// `Ok(false)` = superseded: nothing deleted, nothing written. A later row
    /// at a path an earlier one took wins, as the page-by-page upserts did.
    ///
    /// The same transaction deletes the actor's staged bodies
    /// ([`Self::stage_web_render_body`]): the rows it writes take their
    /// references over, so the blob sweep never sees a body of this render
    /// named by neither (`backup-restore.md` § 9 step 2h).
    ///
    /// ⚠ Do not split this — a delete in one lock acquisition and the inserts
    /// in another — however convenient: nothing can park a reader between two
    /// acquisitions, so NO pin would redden, and the defect would be back.
    pub async fn replace_web_rendered(
        &self,
        claim: &RenderClaim,
        pages: &[RenderedPage],
        sealed: &[RenderedSealedPage],
    ) -> Result<bool> {
        let actor_id = claim.actor_id;
        let now = now_epoch_millis();
        self.under_render_claim(claim, move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "DELETE FROM web_rendered WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
            )
            .context("replace web rendered: clear")?;
            tx.execute(
                "DELETE FROM web_rendered_sealed WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
            )
            .context("replace web rendered: clear sealed")?;
            for page in pages {
                tx.execute(
                    "INSERT INTO web_rendered (actor_id, path, blob_hash, content_type, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(actor_id, path) DO UPDATE SET blob_hash = ?3, content_type = ?4, updated_at = ?5",
                    rusqlite::params![
                        actor_id.as_slice(),
                        page.path,
                        page.blob_hash.as_slice(),
                        page.content_type,
                        now
                    ],
                )
                .context("replace web rendered: page")?;
            }
            for page in sealed {
                tx.execute(
                    "INSERT INTO web_rendered_sealed
                        (actor_id, path, blob_hash, content_type, tier, post_id, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT(actor_id, path) DO UPDATE SET blob_hash = ?3,
                        content_type = ?4, tier = ?5, post_id = ?6, updated_at = ?7",
                    rusqlite::params![
                        actor_id.as_slice(),
                        page.path,
                        page.blob_hash.as_slice(),
                        page.content_type,
                        page.tier,
                        page.post_id.as_slice(),
                        now
                    ],
                )
                .context("replace web rendered: sealed page")?;
            }
            tx.execute(
                "DELETE FROM web_render_staged WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
            )
            .context("replace web rendered: hand the staged bodies to the rows")?;
            tx.commit().context("commit replace_web_rendered")
        })
        .await
    }

    /// The **deadline clear**: delete an actor's rendered output — **both
    /// stores**, public and sealed — under this render's claim, and record **in
    /// the same transaction** that the site is owed its restore, returning that
    /// row's nonce for the render to discharge when it commits
    /// (`web-content-hosting.md` § Routing, render, serving → *A reader sees a
    /// whole site or none*: a render answering an owed revoke that outlasts the
    /// withdrawal deadline withdraws the old site and carries on). One
    /// transaction for the reason [`Self::clear_web_rendered_owing_restore`] is
    /// one: no dark site may exist without the row that brings it back. Unlike
    /// that clear it carries a claim, bumps nothing and discharges nothing — it
    /// is the render's own act, refused once the render is superseded.
    ///
    /// `Ok(None)` = superseded, nothing cleared, which matters as much as
    /// withholding a write does: a stale render that cleared a site another
    /// render had just rebuilt correctly would take it dark for nothing.
    pub async fn clear_web_rendered(&self, claim: &RenderClaim) -> Result<Option<i64>> {
        let actor_id = claim.actor_id;
        let mut nonce = None;
        let landed = &mut nonce;
        self.under_render_claim(claim, move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "DELETE FROM web_rendered WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
            )
            .context("clear web rendered")?;
            tx.execute(
                "DELETE FROM web_rendered_sealed WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
            )
            .context("clear web rendered sealed")?;
            *landed = Some(
                tx.query_row(
                    "INSERT INTO web_restore_owed (actor_id, nonce, owed_at) VALUES (?1, random(), ?2)
                     ON CONFLICT(actor_id) DO UPDATE SET nonce = random(), owed_at = ?2
                     RETURNING nonce",
                    rusqlite::params![actor_id.as_slice(), now_epoch_millis()],
                    |row| row.get(0),
                )
                .context("mark web restore owed")?,
            );
            tx.commit().context("commit clear_web_rendered")
        })
        .await?;
        Ok(nonce)
    }

    // ---- web_rendered_sealed (web paywall, Pillar 2) ----

    /// Upsert a sealed rendered page: `blob_hash` points at ChaCha20-Poly1305
    /// ciphertext sealed under `derive_web_render_key(tier period_key,
    /// post_id)`; `tier` + `post_id` are the serve-time derive inputs. Like
    /// [`Self::upsert_web_rendered`], a test's seeding form — the render writes
    /// through [`Self::replace_web_rendered`].
    pub async fn upsert_web_rendered_sealed(
        &self,
        claim: &RenderClaim,
        path: &str,
        blob_hash: &[u8; 32],
        content_type: &str,
        tier: &str,
        post_id: &[u8; 32],
    ) -> Result<bool> {
        let actor_id = claim.actor_id;
        let blob_hash = *blob_hash;
        let post_id = *post_id;
        let path = path.to_string();
        let content_type = content_type.to_string();
        let tier = tier.to_string();
        let now = now_epoch_millis();
        self.under_render_claim(claim, move |conn| {
            conn.execute(
                "INSERT INTO web_rendered_sealed
                    (actor_id, path, blob_hash, content_type, tier, post_id, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(actor_id, path) DO UPDATE SET blob_hash = ?3,
                    content_type = ?4, tier = ?5, post_id = ?6, updated_at = ?7",
                rusqlite::params![
                    actor_id.as_slice(),
                    path,
                    blob_hash.as_slice(),
                    content_type,
                    tier,
                    post_id.as_slice(),
                    now
                ],
            )
            .context("upsert web rendered sealed")?;
            Ok(())
        })
        .await
    }

    /// Get a sealed rendered page by actor and path.
    pub async fn get_web_rendered_sealed(
        &self,
        actor_id: &[u8; 32],
        path: &str,
    ) -> Result<Option<WebRenderedSealedRow>> {
        let actor_id = *actor_id;
        let path = path.to_string();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT actor_id, path, blob_hash, content_type, tier, post_id, updated_at
             FROM web_rendered_sealed WHERE actor_id = ?1 AND path = ?2",
            rusqlite::params![actor_id.as_slice(), path],
            |row| {
                Ok(WebRenderedSealedRow {
                    path: row.get(1)?,
                    blob_hash: row.get(2)?,
                    content_type: row.get(3)?,
                    tier: row.get(4)?,
                    post_id: row.get(5)?,
                })
            },
        )
        .optional()
        .context("get web rendered sealed")
    }

    // ---- web_render_owed (the revoke's durability) ----

    /// The marker's current nonce for `actor_id` — `None` when no render is
    /// owed. Read BEFORE a render begins; [`Self::discharge_web_render_owed`]
    /// takes it back afterwards.
    pub async fn web_render_owed_nonce(&self, actor_id: &[u8; 32]) -> Result<Option<i64>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT nonce FROM web_render_owed WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
            |row| row.get(0),
        )
        .optional()
        .context("read web render owed")
    }

    /// Discharge the marker a completed render (or fail-closed clear) answered
    /// — only while it still carries `nonce`. A revoke committed after that
    /// read re-rolled the nonce, and stays owed.
    pub async fn discharge_web_render_owed(&self, actor_id: &[u8; 32], nonce: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM web_render_owed WHERE actor_id = ?1 AND nonce = ?2",
            rusqlite::params![actor_id.as_slice(), nonce],
        )
        .context("discharge web render owed")?;
        Ok(())
    }

    /// Every actor a render is still owed to, ascending — what the boot drain
    /// walks. Deliberately NOT [`Self::list_web_publishing_actors`]: that list
    /// reads `content_links`, so it drops exactly the commonest owed site, the
    /// one whose last published post was just deleted.
    pub async fn list_web_render_owed(&self) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT actor_id FROM web_render_owed ORDER BY actor_id")
            .context("prepare list_web_render_owed")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .context("query list_web_render_owed")?;
        let mut out = Vec::new();
        for row in rows {
            let raw = row.context("read list_web_render_owed row")?;
            // A malformed actor id names no site a render could rebuild.
            if let Ok(actor) = <[u8; 32]>::try_from(raw.as_slice()) {
                out.push(actor);
            }
        }
        Ok(out)
    }

    // ---- web_restore_owed (a blanked site is owed its restore) ----

    /// The fail-closed clear: delete `actor_id`'s rendered site, public and
    /// sealed, and record **in the same transaction** that the site is owed
    /// its restore (`web-content-hosting.md` § Routing, render, serving → *A
    /// blanked site is owed its restore*). One transaction so that no site a
    /// failed render took dark can exist without the row that brings it back.
    /// Every call re-rolls the nonce — see [`Self::discharge_web_restore_owed`].
    ///
    /// **It also bumps the actor's render generation, in the same transaction**
    /// (§ *One render writes at a time*): this is the safety act, so it carries
    /// no claim and supersedes every render in flight. Without the bump, a
    /// render that listed before the failed revoke would resume after the clear
    /// and write the withdrawn pages straight back onto a site whose restore is
    /// still owed — the site would look healthy while carrying exactly what the
    /// revoke withdrew. Every render it supersedes loses its staged bodies with
    /// the bump, as at [`Self::begin_web_render`].
    pub async fn clear_web_rendered_owing_restore(&self, actor_id: &[u8; 32]) -> Result<()> {
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO web_render_generation (actor_id, generation) VALUES (?1, 1)
             ON CONFLICT(actor_id) DO UPDATE SET generation = generation + 1",
            rusqlite::params![actor_id.as_slice()],
        )
        .context("supersede renders in flight")?;
        tx.execute(
            "DELETE FROM web_render_staged WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
        )
        .context("drop the superseded renders' staged bodies")?;
        tx.execute(
            "DELETE FROM web_rendered WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
        )
        .context("clear web rendered")?;
        tx.execute(
            "DELETE FROM web_rendered_sealed WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
        )
        .context("clear web rendered sealed")?;
        tx.execute(
            "INSERT INTO web_restore_owed (actor_id, nonce, owed_at) VALUES (?1, random(), ?2)
             ON CONFLICT(actor_id) DO UPDATE SET nonce = random(), owed_at = ?2",
            rusqlite::params![actor_id.as_slice(), now_epoch_millis()],
        )
        .context("mark web restore owed")?;
        tx.commit()
            .context("commit clear_web_rendered_owing_restore")
    }

    /// The owed restore's current nonce for `actor_id` — `None` when the site
    /// is not owed one. Read BEFORE a render begins.
    pub async fn web_restore_owed_nonce(&self, actor_id: &[u8; 32]) -> Result<Option<i64>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT nonce FROM web_restore_owed WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
            |row| row.get(0),
        )
        .optional()
        .context("read web restore owed")
    }

    /// Discharge the restore a completed render answered — only while it still
    /// carries `nonce`. A clear that landed after that read re-rolled the
    /// nonce, and the site it blanked stays owed.
    pub async fn discharge_web_restore_owed(&self, actor_id: &[u8; 32], nonce: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM web_restore_owed WHERE actor_id = ?1 AND nonce = ?2",
            rusqlite::params![actor_id.as_slice(), nonce],
        )
        .context("discharge web restore owed")?;
        Ok(())
    }

    /// Every actor whose blanked site is still owed its restore, ascending.
    pub async fn list_web_restore_owed(&self) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT actor_id FROM web_restore_owed ORDER BY actor_id")
            .context("prepare list_web_restore_owed")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .context("query list_web_restore_owed")?;
        let mut out = Vec::new();
        for row in rows {
            let raw = row.context("read list_web_restore_owed row")?;
            // A malformed actor id names no site a render could rebuild.
            if let Ok(actor) = <[u8; 32]>::try_from(raw.as_slice()) {
                out.push(actor);
            }
        }
        Ok(out)
    }

    // ---- web_domains ----

    /// Insert a new domain registration (fails if domain already exists).
    pub async fn insert_web_domain(
        &self,
        actor_id: &[u8; 32],
        domain: &str,
        verify_token: &str,
    ) -> Result<()> {
        let actor_id = *actor_id;
        let domain = domain.to_string();
        let verify_token = verify_token.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_millis();
        conn.execute(
            "INSERT INTO web_domains (actor_id, domain, verify_token, status, created_at, verified_at)
             VALUES (?1, ?2, ?3, 'pending', ?4, NULL)",
            rusqlite::params![actor_id.as_slice(), domain, verify_token, now],
        )
        .context("insert web domain")?;
        Ok(())
    }

    /// Look up a domain registration by domain name.
    pub async fn get_web_domain_by_domain(&self, domain: &str) -> Result<Option<WebDomainRow>> {
        let domain = domain.to_string();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT actor_id, domain, verify_token, status, created_at, verified_at
             FROM web_domains WHERE domain = ?1",
            rusqlite::params![domain],
            map_web_domain_row,
        )
        .optional()
        .context("get web domain by domain")
    }

    /// List all domain registrations for an actor.
    pub async fn get_web_domains_for_actor(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Vec<WebDomainRow>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT actor_id, domain, verify_token, status, created_at, verified_at
                 FROM web_domains WHERE actor_id = ?1 ORDER BY created_at",
            )
            .context("prepare get_web_domains_for_actor")?;
        let rows = stmt
            .query_map(rusqlite::params![actor_id.as_slice()], map_web_domain_row)
            .context("query web_domains for actor")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("read web domain row")?);
        }
        Ok(out)
    }

    /// List every domain registration across all actors (for the cert resolver).
    pub async fn list_all_web_domains(&self) -> Result<Vec<WebDomainRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT actor_id, domain, verify_token, status, created_at, verified_at
                 FROM web_domains ORDER BY domain",
            )
            .context("prepare list_all_web_domains")?;
        let rows = stmt
            .query_map([], map_web_domain_row)
            .context("query all web_domains")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("read web domain row")?);
        }
        Ok(out)
    }

    /// Update the status of a domain (e.g. 'pending' → 'verified').
    /// Also records `verified_at` when status becomes 'verified'.
    pub async fn update_web_domain_status(&self, domain: &str, status: &str) -> Result<()> {
        let domain = domain.to_string();
        let status = status.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_millis();
        conn.execute(
            "UPDATE web_domains
             SET status = ?1,
                 verified_at = CASE WHEN ?1 = 'verified' THEN ?2 ELSE verified_at END
             WHERE domain = ?3",
            rusqlite::params![status, now, domain],
        )
        .context("update web domain status")?;
        Ok(())
    }

    /// Delete a domain registration by domain name. Returns true if removed.
    pub async fn delete_web_domain(&self, domain: &str) -> Result<bool> {
        let domain = domain.to_string();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM web_domains WHERE domain = ?1",
                rusqlite::params![domain],
            )
            .context("delete web domain")?;
        Ok(n > 0)
    }

    /// Count how many domains an actor has registered.
    pub async fn count_web_domains_for_actor(&self, actor_id: &[u8; 32]) -> Result<i64> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM web_domains WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
                |row| row.get(0),
            )
            .context("count web domains for actor")?;
        Ok(count)
    }

    // ---- web_published links ----

    /// Publish a post: upsert a `web_published` content_links row.
    /// `slug` is stored in the `status` column and used as the URL path.
    /// Returns the link id.
    pub async fn publish_web_post(
        &self,
        actor_id: &[u8; 32],
        post_id: &[u8; 32],
        slug: &str,
    ) -> Result<i64> {
        let actor_id = *actor_id;
        let post_id = *post_id;
        let slug = slug.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_millis();
        let id = links::upsert_link(
            &conn,
            "web_published",
            Some(post_id.as_slice()),
            None,
            Some(actor_id.as_slice()),
            Some(&slug),
            None,
            now,
        )
        .context("publish_web_post upsert_link")?;
        Ok(id)
    }

    /// Unpublish a post: delete the `web_published` content_links row for
    /// the given actor and post. A link that was really removed leaves the
    /// rendered site owing a render, recorded in the same transaction
    /// ([`mark_web_render_owed`]).
    pub async fn unpublish_web_post(&self, actor_id: &[u8; 32], post_id: &[u8; 32]) -> Result<()> {
        let actor_id = *actor_id;
        let post_id = *post_id;
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction()?;
        let removed = tx
            .execute(
                "DELETE FROM content_links WHERE link_type = 'web_published' AND source_id = ?1 AND actor_id = ?2",
                rusqlite::params![post_id.as_slice(), actor_id.as_slice()],
            )
            .context("unpublish_web_post delete")?;
        if removed > 0 {
            mark_web_render_owed(&tx, &actor_id)?;
        }
        tx.commit().context("commit unpublish_web_post")?;
        Ok(())
    }

    /// List all published posts for an actor (the client-facing listing — the
    /// `fauna.web.publish.list` projection — so it is intentionally **uncapped**,
    /// and deliberately **not** moderation-gated: this is the author's own
    /// management surface, where a taken-down post must stay visible so they
    /// can unpublish it). The render pipeline must instead use
    /// [`list_web_published_servable_capped`] so a large publish set can't blow
    /// up the render context and a flagged post never reaches a page.
    ///
    /// Returns `(post_id_bytes, slug, gated_tier)` triples. The tier is
    /// LEFT-JOINed from `content_meta` — the management surface renders the
    /// *Copy paywall link* affordance only on published **and** gated rows
    /// (`web-content-hosting.md` § Published-post management). A `None` tier is
    /// an ungated post; the join must never drop such a row.
    #[allow(clippy::type_complexity)]
    pub async fn list_web_published(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Vec<(Vec<u8>, String, Option<String>)>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT cl.source_id, cl.status, cm.gated_tier \
                 FROM content_links cl \
                 LEFT JOIN content_meta cm ON cm.content_id = cl.source_id \
                 WHERE cl.actor_id = ?1 AND cl.link_type = 'web_published' \
                 ORDER BY cl.created_at DESC",
            )
            .context("prepare list_web_published")?;
        let rows = stmt
            .query_map(rusqlite::params![actor_id.as_slice()], |row| {
                Ok((
                    row.get::<_, Option<Vec<u8>>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .context("query list_web_published")?;
        let mut out = Vec::new();
        for row in rows {
            let (source, slug, tier) = row.context("read list_web_published row")?;
            // A row with no source/slug is not a publish record — same
            // filter_map contract as `list_web_published_servable_capped`.
            if let (Some(source), Some(slug)) = (source, slug) {
                out.push((source, slug, tier));
            }
        }
        Ok(out)
    }

    /// The published posts the site may **render**: [`list_web_published`]
    /// minus every post a moderation flag withholds, bounded to at most `limit`
    /// newest rows. The one enumeration behind
    /// `WebContentService::render_published_posts`, which calls it with
    /// [`MAX_RENDERED_POSTS`](crate::web_content::service::MAX_RENDERED_POSTS).
    ///
    /// **It is the web site's moderation gate.** The render turns each post it
    /// lists into `post/{slug}.html`, the index and `feed.xml` — pages served
    /// to anyone on the open internet — and it reads bodies through the
    /// deliberately flag-blind `load_post_body`. So the withholding has to
    /// happen here, on the enumeration, the way the off-box surfaces do it
    /// (`db::public_servability`): a post taken down, quarantined or suppressed
    /// is simply never listed. Until 2026-09-10 this read applied no flag at
    /// all (`moderation.md` § Legal takedown → *Posts*). The gate is
    /// [`MODERATION_SERVABLE`](super::public_servability::MODERATION_SERVABLE)
    /// and not the off-box predicate: the render serves a gated post behind
    /// its paywall and an archive import on the author's own site, both on
    /// purpose. Filtering before the cap also means a flagged post can never
    /// crowd a servable one out of the render.
    ///
    /// The cap keeps an actor with an unbounded publish set from amplifying a
    /// single render into unbounded loop iterations / output (the
    /// "≤1000 iterations" half of `web-content-hosting.md` § Routing/render's
    /// Safety limits).
    pub async fn list_web_published_servable_capped(
        &self,
        actor_id: &[u8; 32],
        limit: u32,
    ) -> Result<Vec<(Vec<u8>, String)>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT cl.source_id, cl.status \
                 FROM content_links cl \
                 LEFT JOIN content_meta cm ON cm.content_id = cl.source_id \
                 WHERE cl.actor_id = ?1 AND cl.link_type = 'web_published' \
                   AND {moderation} \
                 ORDER BY cl.created_at DESC LIMIT ?2",
                moderation = super::public_servability::MODERATION_SERVABLE,
            ))
            .context("prepare list_web_published_servable_capped")?;
        let rows = stmt
            .query_map(rusqlite::params![actor_id.as_slice(), limit], |row| {
                Ok((
                    row.get::<_, Option<Vec<u8>>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            })
            .context("query list_web_published_servable_capped")?;
        let mut out = Vec::new();
        for row in rows {
            let (source, slug) = row.context("read list_web_published_servable_capped row")?;
            // A row with no source/slug is not a publish record.
            if let (Some(source), Some(slug)) = (source, slug) {
                out.push((source, slug));
            }
        }
        Ok(out)
    }

    /// Every actor with at least one published web post, ascending — the set a
    /// nest-wide re-render walks when an input every public post page depends
    /// on moves: the situs's region content policy (`region-blocking.md` § The
    /// content plane → *The nest-as-publisher leg*). An actor with no published
    /// post has nothing that fold could change.
    pub async fn list_web_publishing_actors(&self) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT actor_id FROM content_links \
                 WHERE link_type = 'web_published' ORDER BY actor_id",
            )
            .context("prepare list_web_publishing_actors")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .context("query list_web_publishing_actors")?;
        let mut out = Vec::new();
        for row in rows {
            let raw = row.context("read list_web_publishing_actors row")?;
            // A malformed actor id names no site a render could rebuild.
            if let Ok(actor) = <[u8; 32]>::try_from(raw.as_slice()) {
                out.push(actor);
            }
        }
        Ok(out)
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn actor() -> [u8; 32] {
        [1u8; 32]
    }

    fn blob() -> [u8; 32] {
        [2u8; 32]
    }

    fn db() -> CacheDb {
        CacheDb::open_in_memory().unwrap()
    }

    #[tokio::test]
    async fn upsert_and_get_web_file() {
        let db = db();
        let actor = actor();
        let hash = blob();

        db.upsert_web_file(&actor, "/index.html", &hash, "text/html", None, None)
            .await
            .unwrap();

        let row = db
            .get_web_file(&actor, "/index.html")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.path, "/index.html");
        assert_eq!(row.blob_hash, hash.as_slice());
        assert_eq!(row.content_type, "text/html");
        assert_eq!(row.actor_id, actor.as_slice());

        // Upsert again with different hash — should update
        let hash2 = [3u8; 32];
        db.upsert_web_file(&actor, "/index.html", &hash2, "text/html", None, None)
            .await
            .unwrap();
        let row2 = db
            .get_web_file(&actor, "/index.html")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row2.blob_hash, hash2.as_slice());

        // Non-existent path returns None
        assert!(
            db.get_web_file(&actor, "/missing.html")
                .await
                .unwrap()
                .is_none()
        );
    }

    /// **The prune's three bounds**, each one load-bearing.
    ///
    /// The declaration comes from the owner's own client and names only what is
    /// still live, so what it must NOT be able to do matters as much as what it
    /// does: it touches **only sealed rows** (the plaintext class has its own
    /// nest-side reconcile, built from real evidence — a client's view of it is
    /// not authoritative), **only this folder's** (a site is not one corpus),
    /// and **only this actor's**.
    ///
    /// Red-verify: drop `content_key_version IS NOT NULL` from either statement
    /// and the plaintext survivor goes; drop `folder_id = ?2` and the sibling
    /// folder's row goes.
    #[tokio::test]
    async fn prune_sealed_web_files_touches_only_this_folders_sealed_rows() {
        let db = db();
        let actor = actor();
        let other_actor = [0x99u8; 32];
        let hash = blob();

        // This folder (1): one sealed survivor, one sealed casualty, one
        // plaintext row the caller has no standing to judge.
        for (path, ckv) in [
            ("index.html", Some(1)),
            ("chapter-two.html", Some(1)),
            ("free.css", None),
        ] {
            db.upsert_web_file(&actor, path, &hash, "text/html", Some(1), ckv)
                .await
                .unwrap();
        }
        // A sibling folder (2) of the same actor, and another actor entirely.
        db.upsert_web_file(
            &actor,
            "other-site/index.html",
            &hash,
            "text/html",
            Some(2),
            Some(1),
        )
        .await
        .unwrap();
        db.upsert_web_file(
            &other_actor,
            "index.html",
            &hash,
            "text/html",
            Some(1),
            Some(1),
        )
        .await
        .unwrap();

        let keep: std::collections::HashSet<String> = ["index.html".to_string()].into();
        assert_eq!(
            db.prune_sealed_web_files(&actor, 1, &keep).await.unwrap(),
            1,
            "exactly the one sealed row this folder no longer holds"
        );

        let mine: Vec<String> = db
            .list_web_files(&actor)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.path)
            .collect();
        assert_eq!(
            mine,
            vec![
                "free.css".to_string(),
                "index.html".to_string(),
                "other-site/index.html".to_string(),
            ],
            "the plaintext row and the sibling folder's sealed row both survive"
        );
        assert_eq!(
            db.list_web_files(&other_actor).await.unwrap().len(),
            1,
            "another actor's identically-named row is untouched"
        );

        // Idempotent: replaying the same declaration drops nothing more.
        assert_eq!(
            db.prune_sealed_web_files(&actor, 1, &keep).await.unwrap(),
            0
        );
    }

    /// An **empty** `keep` set is a real statement **at this, the `CacheDb`,
    /// layer** — "this folder's sealed corpus is now empty" — and the store
    /// honours it faithfully rather than reading it as "unspecified": it holds
    /// no names for this class and must not silently start second-guessing a
    /// caller who does.
    ///
    /// ⚠ **The `fauna.web.files.prune_sealed` handler now refuses an empty
    /// `paths` before ever calling down to here** —
    /// duplicating the refusal `SyncEngine::declare_live_web_corpus` already
    /// makes one crate away (the shipped client never sends one, because from
    /// there "nothing is live" and "this device has not caught up" are the
    /// same observation, and one of them would take a live site down). This
    /// pin stays anyway: the primitive's own contract — faithful to what it is
    /// told, never second-guessing — is worth asserting at the layer that owns
    /// it, independently of whichever guards its callers add above it.
    #[tokio::test]
    async fn an_empty_declaration_clears_the_folders_sealed_rows_at_the_db_layer() {
        let db = db();
        let actor = actor();
        let hash = blob();
        db.upsert_web_file(&actor, "gone.html", &hash, "text/html", Some(1), Some(1))
            .await
            .unwrap();
        db.upsert_web_file(&actor, "kept.css", &hash, "text/css", Some(1), None)
            .await
            .unwrap();

        let empty = std::collections::HashSet::new();
        assert_eq!(
            db.prune_sealed_web_files(&actor, 1, &empty).await.unwrap(),
            1
        );
        let left: Vec<String> = db
            .list_web_files(&actor)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.path)
            .collect();
        assert_eq!(left, vec!["kept.css".to_string()]);
    }

    #[tokio::test]
    async fn list_web_files() {
        let db = db();
        let actor = actor();
        let hash = blob();

        db.upsert_web_file(&actor, "/a.html", &hash, "text/html", None, None)
            .await
            .unwrap();
        db.upsert_web_file(&actor, "/b.css", &hash, "text/css", None, None)
            .await
            .unwrap();
        db.upsert_web_file(&actor, "/c.html", &hash, "text/html", None, None)
            .await
            .unwrap();

        let all = db.list_web_files(&actor).await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].path, "/a.html");
        assert_eq!(all[1].path, "/b.css");
        assert_eq!(all[2].path, "/c.html");
    }

    #[tokio::test]
    async fn delete_web_file() {
        let db = db();
        let actor = actor();
        let hash = blob();

        db.upsert_web_file(&actor, "/index.html", &hash, "text/html", None, None)
            .await
            .unwrap();

        let deleted = db.delete_web_file(&actor, "/index.html").await.unwrap();
        assert!(deleted);

        // Second delete returns false
        let deleted_again = db.delete_web_file(&actor, "/index.html").await.unwrap();
        assert!(!deleted_again);

        assert!(
            db.get_web_file(&actor, "/index.html")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn upsert_and_get_web_rendered() {
        let db = db();
        let actor = actor();
        let hash = blob();

        let render = db.begin_web_render(&actor).await.unwrap();
        assert!(
            db.upsert_web_rendered(&render, "/index.html", &hash, "text/html")
                .await
                .unwrap()
        );

        let row = db
            .get_web_rendered(&actor, "/index.html")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.path, "/index.html");
        assert_eq!(row.blob_hash, hash.as_slice());

        // Distinct from web_files — web_files should be empty
        assert!(
            db.get_web_file(&actor, "/index.html")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn clear_web_rendered() {
        let db = db();
        let actor = actor();
        let hash = blob();

        let render = db.begin_web_render(&actor).await.unwrap();
        db.upsert_web_rendered(&render, "/a.html", &hash, "text/html")
            .await
            .unwrap();
        db.upsert_web_rendered(&render, "/b.html", &hash, "text/html")
            .await
            .unwrap();

        let next = db.begin_web_render(&actor).await.unwrap();
        assert!(db.clear_web_rendered(&next).await.unwrap().is_some());

        assert!(
            db.get_web_rendered(&actor, "/a.html")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_web_rendered(&actor, "/b.html")
                .await
                .unwrap()
                .is_none()
        );
    }

    /// **The fence at its own layer** (`web-content-hosting.md` § Routing,
    /// render, serving → *One render writes at a time*). Two renders of one
    /// actor overlap; the one whose listing came first may write nothing — not
    /// a page, and not the clear either. The clear matters as much: a stale
    /// render allowed to clear and then withheld from writing would take a
    /// site the newer render had just rebuilt dark, with nothing owed to bring
    /// it back.
    #[tokio::test]
    async fn only_the_newest_listing_may_write_the_rendered_stores() {
        let db = db();
        let actor = actor();
        let hash = blob();

        let older = db.begin_web_render(&actor).await.unwrap();
        let newer = db.begin_web_render(&actor).await.unwrap();

        assert!(
            db.upsert_web_rendered(&newer, "/index.html", &hash, "text/html")
                .await
                .unwrap(),
            "the newest listing owns the site"
        );
        assert!(
            !db.upsert_web_rendered(&older, "/stale.html", &hash, "text/html")
                .await
                .unwrap(),
            "a render that listed earlier must not write a page back"
        );
        assert!(
            !db.upsert_web_rendered_sealed(
                &older,
                "/stale.html",
                &hash,
                "text/html",
                "gold",
                &hash
            )
            .await
            .unwrap(),
            "and the sealed store is fenced with the public one"
        );
        assert!(
            db.clear_web_rendered(&older).await.unwrap().is_none(),
            "nor may it clear the site the newer render just wrote"
        );
        assert!(
            db.get_web_rendered(&actor, "/index.html")
                .await
                .unwrap()
                .is_some(),
            "the newer render's page survives the older one's whole pass"
        );
        assert!(
            db.web_render_superseded(&older).await.unwrap(),
            "and the older render can see it has been superseded, so it abandons early"
        );
        assert!(!db.web_render_superseded(&newer).await.unwrap());
    }

    /// The **fail-closed clear** supersedes every render in flight: it carries
    /// no claim because it is the safety act. Without this, a render that
    /// listed before a failed revoke would write the withdrawn pages straight
    /// back onto a site whose restore is still owed.
    #[tokio::test]
    async fn a_fail_closed_clear_supersedes_the_renders_in_flight() {
        let db = db();
        let actor = actor();
        let hash = blob();

        let in_flight = db.begin_web_render(&actor).await.unwrap();
        db.clear_web_rendered_owing_restore(&actor).await.unwrap();

        assert!(
            db.web_render_superseded(&in_flight).await.unwrap(),
            "the clear takes the site over"
        );
        assert!(
            !db.upsert_web_rendered(&in_flight, "/withdrawn.html", &hash, "text/html")
                .await
                .unwrap(),
            "a render already running may not write onto the blanked site"
        );
        assert!(
            db.get_web_rendered(&actor, "/withdrawn.html")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.web_restore_owed_nonce(&actor).await.unwrap().is_some(),
            "and the site is still owed its restore — a withheld write pays nothing"
        );
    }

    /// One clear, both stores. `web_rendered_sealed` is not a second thing to
    /// remember: a render that cleared the public rows and was superseded
    /// before clearing the sealed ones would leave a formerly-gated post's
    /// sealed page standing.
    #[tokio::test]
    async fn clearing_a_render_clears_the_sealed_store_with_it() {
        let db = db();
        let actor = actor();
        let hash = blob();

        let first = db.begin_web_render(&actor).await.unwrap();
        db.upsert_web_rendered(&first, "post/premium.html", &hash, "text/html")
            .await
            .unwrap();
        db.upsert_web_rendered_sealed(
            &first,
            "post/premium.html",
            &hash,
            "text/html",
            "gold",
            &hash,
        )
        .await
        .unwrap();

        let second = db.begin_web_render(&actor).await.unwrap();
        assert!(db.clear_web_rendered(&second).await.unwrap().is_some());

        assert!(
            db.get_web_rendered(&actor, "post/premium.html")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_web_rendered_sealed(&actor, "post/premium.html")
                .await
                .unwrap()
                .is_none(),
            "the sealed page goes with the public one"
        );
        assert!(
            db.web_restore_owed_nonce(&actor).await.unwrap().is_some(),
            "and the clear owes the restore in its own transaction — no dark site \
             without the row that brings it back"
        );
    }

    /// **A render's one write** (`web-content-hosting.md` § Routing, render,
    /// serving → *A reader sees a whole site or none*): the replacement drops
    /// every row the last render left — in BOTH stores — and lands every row of
    /// this one, and a superseded render's replacement touches nothing.
    #[tokio::test]
    async fn a_replacement_swaps_both_stores_whole_or_not_at_all() {
        let db = db();
        let actor = actor();
        let hash = blob();
        let page = |path: &str| RenderedPage {
            path: path.into(),
            blob_hash: hash,
            content_type: "text/html".into(),
        };
        let sealed_page = |path: &str| RenderedSealedPage {
            path: path.into(),
            blob_hash: hash,
            content_type: "text/html".into(),
            tier: "gold".into(),
            post_id: hash,
        };

        let first = db.begin_web_render(&actor).await.unwrap();
        assert!(
            db.replace_web_rendered(
                &first,
                &[page("index.html"), page("post/old.html")],
                &[sealed_page("post/old.html")]
            )
            .await
            .unwrap()
        );

        let stale = db.begin_web_render(&actor).await.unwrap();
        let second = db.begin_web_render(&actor).await.unwrap();
        assert!(
            db.replace_web_rendered(
                &second,
                &[page("index.html"), page("post/new.html")],
                &[sealed_page("post/new.html")]
            )
            .await
            .unwrap()
        );
        assert!(
            !db.replace_web_rendered(&stale, &[page("post/stale.html")], &[])
                .await
                .unwrap(),
            "a superseded render's replacement is refused"
        );

        for (path, public, sealed) in [
            ("index.html", true, false),
            ("post/new.html", true, true),
            ("post/old.html", false, false),
            ("post/stale.html", false, false),
        ] {
            assert_eq!(
                db.get_web_rendered(&actor, path).await.unwrap().is_some(),
                public,
                "web_rendered {path}"
            );
            assert_eq!(
                db.get_web_rendered_sealed(&actor, path)
                    .await
                    .unwrap()
                    .is_some(),
                sealed,
                "web_rendered_sealed {path}"
            );
        }
    }

    /// **The replacement hands a render's staged bodies to its rendered rows**
    /// (`backup-restore.md` § 9 step 2h): the transaction that writes the rows
    /// deletes the actor's staged rows, so a staged row only ever names a body
    /// of a render still in flight — never the live site the rendered rows
    /// already reference.
    #[tokio::test]
    async fn the_replacement_hands_the_staged_bodies_to_the_rendered_rows() {
        async fn staged(db: &CacheDb, actor: &[u8; 32]) -> i64 {
            db.conn
                .lock()
                .await
                .query_row(
                    "SELECT COUNT(*) FROM web_render_staged WHERE actor_id = ?1",
                    rusqlite::params![actor.as_slice()],
                    |row| row.get(0),
                )
                .unwrap()
        }
        let db = db();
        let actor = actor();
        let hash = blob();

        let claim = db.begin_web_render(&actor).await.unwrap();
        assert!(db.stage_web_render_body(&claim, &hash).await.unwrap());
        assert_eq!(staged(&db, &actor).await, 1);
        let page = RenderedPage {
            path: "index.html".into(),
            blob_hash: hash,
            content_type: "text/html".into(),
        };
        assert!(db.replace_web_rendered(&claim, &[page], &[]).await.unwrap());
        assert_eq!(
            staged(&db, &actor).await,
            0,
            "the committed page's row took the staged reference over"
        );
        assert!(
            db.get_web_rendered(&actor, "index.html")
                .await
                .unwrap()
                .is_some()
        );
    }

    /// **A label on a web-published post owes its publisher a render, in the
    /// label write's own transaction** (`web-content-hosting.md` § Routing,
    /// render, serving → *A revoke is durable*).
    ///
    /// The nest-as-publisher fold folds every post's labels through the region
    /// content policy in force, so an arriving label can turn a shown post into
    /// a blocked or collapsed one on its own page, the index, the feed and any
    /// template that lists it (`region-blocking.md` § The nest-as-publisher
    /// leg). The write neither marked nor rendered, so that change waited for
    /// some unrelated render of the actor. Latent while the compiled-in
    /// registry enrols no authority — marked anyway, because the census is
    /// decided on what a state change CAN do.
    #[tokio::test]
    async fn a_label_on_a_published_post_owes_its_publisher_a_render() {
        let db = db();
        let actor = actor();
        let post = [0x5a_u8; 32];

        db.publish_web_post(&actor, &post, "the-post")
            .await
            .unwrap();
        assert!(
            db.list_web_render_owed().await.unwrap().is_empty(),
            "precondition: publish.set is the purely additive door and marks nothing"
        );

        db.upsert_content_label(
            "post",
            &hex::encode(post),
            "region/blocked",
            1.0,
            0,
            &actor,
            1,
            0,
            None,
            None,
            0,
            &actor,
            &[],
        )
        .await
        .unwrap();

        assert_eq!(
            db.list_web_render_owed().await.unwrap(),
            vec![actor],
            "the label's own transaction records the render its publisher is owed"
        );
    }

    /// The same write marks nothing when it cannot change a rendered page: a
    /// label on an UNPUBLISHED post, and a label on something that is not a
    /// post at all (the behavioral-anomaly writer's `channel` rows).
    #[tokio::test]
    async fn a_label_that_no_site_carries_owes_nothing() {
        let db = db();
        let actor = actor();

        // Unpublished post: no `web_published` link, so no publisher.
        db.upsert_content_label(
            "post",
            &hex::encode([0x5b_u8; 32]),
            "spam/generic",
            0.9,
            0,
            &actor,
            1,
            0,
            None,
            None,
            0,
            &actor,
            &[],
        )
        .await
        .unwrap();
        assert!(db.list_web_render_owed().await.unwrap().is_empty());

        // A channel label names no post — it must not even be looked up as one.
        db.publish_web_post(&actor, &[0x5c_u8; 32], "published")
            .await
            .unwrap();
        db.upsert_content_label(
            "channel",
            &hex::encode([0x5c_u8; 32]),
            "spam/behavioral",
            0.5,
            0,
            &actor,
            1,
            0,
            None,
            None,
            0,
            &actor,
            &[],
        )
        .await
        .unwrap();
        assert!(
            db.list_web_render_owed().await.unwrap().is_empty(),
            "a channel label is not a post label, even when the hex collides with one"
        );
    }

    #[tokio::test]
    async fn web_domain_lifecycle() {
        let db = db();
        let actor = actor();

        // Insert
        db.insert_web_domain(&actor, "example.com", "tok123")
            .await
            .unwrap();

        // Lookup by domain
        let row = db
            .get_web_domain_by_domain("example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.domain, "example.com");
        assert_eq!(row.verify_token, "tok123");
        assert_eq!(row.status, "pending");
        assert!(row.verified_at.is_none());

        // Count
        let count = db.count_web_domains_for_actor(&actor).await.unwrap();
        assert_eq!(count, 1);

        // List for actor
        let domains = db.get_web_domains_for_actor(&actor).await.unwrap();
        assert_eq!(domains.len(), 1);
        assert_eq!(domains[0].domain, "example.com");

        // List all
        let all = db.list_all_web_domains().await.unwrap();
        assert_eq!(all.len(), 1);

        // Update status to verified
        db.update_web_domain_status("example.com", "verified")
            .await
            .unwrap();
        let row2 = db
            .get_web_domain_by_domain("example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row2.status, "verified");
        assert!(row2.verified_at.is_some());

        // Delete
        let deleted = db.delete_web_domain("example.com").await.unwrap();
        assert!(deleted);
        assert!(
            db.get_web_domain_by_domain("example.com")
                .await
                .unwrap()
                .is_none()
        );

        // Second delete returns false
        assert!(!db.delete_web_domain("example.com").await.unwrap());
    }

    #[tokio::test]
    async fn list_hbs_files() {
        let db = db();
        let actor = actor();
        let hash = blob();

        db.upsert_web_file(
            &actor,
            "/templates/home.hbs",
            &hash,
            "text/plain",
            None,
            None,
        )
        .await
        .unwrap();
        db.upsert_web_file(
            &actor,
            "/templates/about.hbs",
            &hash,
            "text/plain",
            None,
            None,
        )
        .await
        .unwrap();
        db.upsert_web_file(&actor, "/static/style.css", &hash, "text/css", None, None)
            .await
            .unwrap();

        let hbs = db.list_web_files_by_ext(&actor, ".hbs").await.unwrap();
        assert_eq!(hbs.len(), 2);
        for f in &hbs {
            assert!(f.path.ends_with(".hbs"));
        }

        let css = db.list_web_files_by_ext(&actor, ".css").await.unwrap();
        assert_eq!(css.len(), 1);
        assert_eq!(css[0].path, "/static/style.css");

        let none = db.list_web_files_by_ext(&actor, ".js").await.unwrap();
        assert!(none.is_empty());
    }

    /// The marker rides the unpublish's transaction, and only a link that was
    /// really removed owes a render.
    #[tokio::test]
    async fn unpublish_marks_the_render_owed_only_when_a_link_went() {
        let db = db();
        let post = [9u8; 32];
        db.unpublish_web_post(&actor(), &post).await.unwrap();
        assert!(
            db.list_web_render_owed().await.unwrap().is_empty(),
            "an unpublish that removed nothing owes nothing"
        );

        db.publish_web_post(&actor(), &post, "slug").await.unwrap();
        db.unpublish_web_post(&actor(), &post).await.unwrap();
        assert_eq!(db.list_web_render_owed().await.unwrap(), vec![actor()]);
    }

    /// A revoke committed WHILE a render is running must stay owed: that
    /// render's listing may predate it. The discharge therefore removes only
    /// the nonce it read before the render began.
    #[tokio::test]
    async fn a_mark_during_a_render_survives_that_renders_discharge() {
        let db = db();
        let (first, second) = ([1u8; 32], [2u8; 32]);
        db.publish_web_post(&actor(), &first, "first")
            .await
            .unwrap();
        db.publish_web_post(&actor(), &second, "second")
            .await
            .unwrap();

        db.unpublish_web_post(&actor(), &first).await.unwrap();
        let read_before_render = db.web_render_owed_nonce(&actor()).await.unwrap().unwrap();
        // … the render is running; a second revoke commits …
        db.unpublish_web_post(&actor(), &second).await.unwrap();
        // … and the render completes, discharging what it read.
        db.discharge_web_render_owed(&actor(), read_before_render)
            .await
            .unwrap();
        assert_eq!(
            db.list_web_render_owed().await.unwrap(),
            vec![actor()],
            "the second revoke re-rolled the nonce, so the first render's discharge \
             must not clear it"
        );

        let current = db.web_render_owed_nonce(&actor()).await.unwrap().unwrap();
        db.discharge_web_render_owed(&actor(), current)
            .await
            .unwrap();
        assert!(db.list_web_render_owed().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_web_published_servable_capped_bounds_the_render_context() {
        // The render path must read a bounded number of published posts so
        // an actor with a large publish set can't blow up a single render.
        let db = db();
        let actor = actor();
        for i in 0..5u8 {
            let post_id = [i + 10; 32];
            db.publish_web_post(&actor, &post_id, &format!("slug-{i}"))
                .await
                .unwrap();
        }

        // The uncapped listing (client-facing) returns everything…
        let all = db.list_web_published(&actor).await.unwrap();
        assert_eq!(
            all.len(),
            5,
            "uncapped listing returns every published post"
        );

        // …but the capped render query never exceeds the limit.
        let capped = db
            .list_web_published_servable_capped(&actor, 2)
            .await
            .unwrap();
        assert_eq!(capped.len(), 2, "render query is bounded to the cap");
    }

    #[tokio::test]
    async fn list_web_published_joins_the_gated_tier() {
        // The `fauna.web.publish.list` projection carries `content_meta.gated_tier`
        // so the `web-settings` management rows know which rows offer the
        // *Copy paywall link* affordance (`web-content-hosting.md`
        // § Published-post management — published **and** gated only).
        let db = db();
        let actor = actor();
        let gated_id = [0x31u8; 32];
        let public_id = [0x32u8; 32];
        db.publish_web_post(&actor, &gated_id, "gated-one")
            .await
            .unwrap();
        db.publish_web_post(&actor, &public_id, "public-one")
            .await
            .unwrap();

        // Only the first post is gated to a tier.
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO content_meta (content_id, gated_tier) VALUES (?1, ?2)",
                rusqlite::params![gated_id.as_slice(), "gold"],
            )
            .unwrap();
        }

        let rows = db.list_web_published(&actor).await.unwrap();
        assert_eq!(rows.len(), 2);
        let tier_of = |slug: &str| -> Option<String> {
            rows.iter()
                .find(|(_, s, _)| s == slug)
                .unwrap_or_else(|| panic!("no row for {slug}"))
                .2
                .clone()
        };
        assert_eq!(
            tier_of("gated-one").as_deref(),
            Some("gold"),
            "a gated published post carries its tier"
        );
        assert_eq!(
            tier_of("public-one"),
            None,
            "an ungated published post has no tier — the LEFT JOIN must not drop it"
        );
    }
}
