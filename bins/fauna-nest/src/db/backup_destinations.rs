//! **Source-side** per-owner backup destination registry (nest-side segment
//! backup slice 3; `message-segment-store.md` § Cross-location backup protocol,
//! `backup-restore.md` § Background Tasks).
//!
//! One row per `(owner_actor_id, destination_id)` in `backup_destinations`
//! (table defined in `migrations::MIGRATIONS_BACKUP_DESTINATIONS`). This is the
//! piece that lets the source nest's in-process coordinator know **where** to
//! back an owner up: the owner's client configures its destinations in
//! the `fauna.state.backup` plane entries, which the nest only ever holds as
//! client-sealed ciphertext (the nest never holds the sealing keys), so the destination list is **not** derivable nest-side.
//! The client therefore registers each destination here explicitly, at
//! destination-enroll, over its own authed connection to its source nest
//! (`fauna.backup.destination.register`).
//!
//! **What a row carries and why:** the coordinator's nest arm dials each
//! destination over the federation channel *as itself* (`federation_channel::dial`
//! → `fauna.federation.hello`), so it needs the destination's origin URL
//! (`nest_url`) and the 32-byte nest id it must pin as the handshake's expected
//! peer (`nest_id`, = the destination's `fauna.nest.info` pubkey). The row
//! declares no capability: the destination nest marks the copy it provisions
//! itself (`folders.custody_copy` — `backup-destinations.md` § State & data
//! shape → *Capability*), so every enrolled row is acted on. None of this is secret — it is the user's own
//! chosen backup targets — so, unlike a capability grant, the row rests plaintext
//! and nest-readable.
//!
//! CRUD only — the `fauna.backup.destination.{register,remove,list}` USER-class
//! handlers and their per-class gate are the nest handler slice
//! (`backup_handlers.rs`); this module never dials or interprets a URL.

use anyhow::{Result, anyhow};

use super::{CacheDb, now_epoch_secs};

/// A destination nest id is exactly 32 bytes (its `fauna.nest.info` pubkey,
/// pinned as the federation handshake's expected peer). The store refuses any
/// other length so a malformed registration can never rest as a row the
/// coordinator would later dial with a wrong-sized expected-peer id.
pub const DESTINATION_NEST_ID_LEN: usize = 32;

/// One ordinary-folder coverage row (`backup-destinations.md` § Ordinary-folder
/// coverage), joined with the folder's display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoveredFolderRow {
    pub destination_id: String,
    pub folder_id: i64,
    /// The folder's current `folders.name`; `None` when the folder row is gone
    /// and only the coverage row remains.
    pub name: Option<String>,
    /// The covered folder's `name_hash` / `name_sealed`, verbatim — the
    /// address and sealed label a restore names the folder by once its
    /// plaintext leaves the row. `None` with the folder row, or on a row that
    /// never carried them.
    pub name_hash: Option<Vec<u8>>,
    pub name_sealed: Option<Vec<u8>>,
}

/// One configured backup destination, as the owner's client lists them and as
/// the coordinator reads them for scheduling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupDestinationRow {
    /// The client-assigned stable id (matches `BackupDestination::destination_id`).
    pub destination_id: String,
    /// Origin URL of the destination nest, dialed over the federation channel.
    /// Empty on a non-nest kind, which is never dialled.
    pub nest_url: String,
    /// The destination nest's 32-byte id, pinned as the handshake's expected peer.
    /// Empty on a non-nest kind.
    pub nest_id: Vec<u8>,
    /// Unix seconds the destination was (most recently) registered.
    pub added_at: i64,
    /// Which kind of custodian this is (`fauna_core::data::DESTINATION_KIND_*`).
    /// Pre-existing rows read `"nest"` — which is what they are.
    pub kind: String,
    /// `"client-device"` rows only: the custodian device's stable sync id, the
    /// key the status projection and the check-in authz both turn on.
    pub custodian_device_id: Option<String>,
    /// `"client-device"` rows only: the user-set cap in bytes. `None` is
    /// **uncapped**, never a zero cap.
    pub capacity_cap_bytes: Option<u64>,
}

impl BackupDestinationRow {
    /// This registry row as the shared custodian-matching input
    /// (`fauna_core::data::custodian_assignment_for`) — the same projection the
    /// wire row offers, so the nest and the device's own host agree on which row
    /// belongs to which device by construction rather than by two readings.
    pub fn custodian_row(&self) -> fauna_core::data::CustodianRowRef<'_> {
        fauna_core::data::CustodianRowRef {
            destination_id: &self.destination_id,
            kind: &self.kind,
            custodian_device_id: self.custodian_device_id.as_deref(),
            capacity_cap_bytes: self.capacity_cap_bytes,
        }
    }
}

impl CacheDb {
    /// Register (or refresh) a **peer-nest** backup destination for an owner —
    /// the v1 kind. Thin wrapper over [`Self::put_backup_destination_of_kind`]
    /// so the many existing nest-only call sites keep their shape.
    pub async fn put_backup_destination(
        &self,
        owner_actor_id: &[u8],
        destination_id: &str,
        nest_url: &str,
        nest_id: &[u8],
    ) -> Result<()> {
        self.put_backup_destination_of_kind(
            owner_actor_id,
            destination_id,
            nest_url,
            nest_id,
            fauna_core::data::DESTINATION_KIND_NEST,
            None,
            None,
        )
        .await
    }

    /// Register (or refresh) a backup destination of any kind. `INSERT OR
    /// REPLACE` on the `(owner, destination_id)` primary key, so a client retries
    /// enroll freely (`nest/common.md` § Client-state recoverability) and an edit
    /// for the same `destination_id` replaces in place.
    ///
    /// **The validation branches on kind, and both arms are load-bearing:**
    ///
    /// * A `nest` row must carry a 32-byte `nest_id` — it is pinned as the
    ///   federation handshake's expected peer, so a malformed one could never
    ///   rest as a row the coordinator would later dial.
    /// * A `client-device` row has **no address at all** (`behavior/backup-destinations.md`
    ///   § Custodian contract, question 1) and so carries an empty `nest_id`;
    ///   what it must carry instead is a non-blank `custodian_device_id`. A blank
    ///   one projects `DestinationKind::Inert`, i.e. a destination the user sees
    ///   in their list that nothing can ever drive — the live half-state
    ///   `nest/common.md` § Client-state recoverability forbids, and the same
    ///   refusal `enroll_client_custodian` already makes client-side.
    ///
    /// An unknown (future) kind is refused outright rather than stored: this
    /// build cannot state what such a row must carry, and storing it would put a
    /// row in the registry that every later reader has to guess about.
    #[allow(clippy::too_many_arguments)]
    pub async fn put_backup_destination_of_kind(
        &self,
        owner_actor_id: &[u8],
        destination_id: &str,
        nest_url: &str,
        nest_id: &[u8],
        kind: &str,
        custodian_device_id: Option<&str>,
        capacity_cap_bytes: Option<u64>,
    ) -> Result<()> {
        if destination_id.is_empty() {
            return Err(anyhow!("destination_id must not be empty"));
        }
        match kind {
            fauna_core::data::DESTINATION_KIND_NEST => {
                if nest_id.len() != DESTINATION_NEST_ID_LEN {
                    return Err(anyhow!(
                        "destination nest id must be {DESTINATION_NEST_ID_LEN} bytes, got {}",
                        nest_id.len()
                    ));
                }
            }
            fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE => {
                if custodian_device_id.map(str::trim).unwrap_or("").is_empty() {
                    return Err(anyhow!(
                        "a client-device destination must carry a custodian_device_id"
                    ));
                }
            }
            other => {
                return Err(anyhow!("unknown backup destination kind '{other}'"));
            }
        }
        let owner_actor_id = owner_actor_id.to_vec();
        let nest_id = nest_id.to_vec();
        let custodian_device_id = custodian_device_id.map(|d| d.trim().to_string());
        let capacity_cap_bytes = capacity_cap_bytes.map(|c| c as i64);
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO backup_destinations
                (owner_actor_id, destination_id, nest_url, nest_id, added_at,
                 kind, custodian_device_id, capacity_cap_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                owner_actor_id,
                destination_id,
                nest_url,
                nest_id,
                now,
                kind,
                custodian_device_id,
                capacity_cap_bytes,
            ],
        )
        .map_err(|e| anyhow!("put backup destination: {e}"))?;
        Ok(())
    }

    /// Remove a destination: delete the row. Returns whether one existed —
    /// idempotent, so a double remove is success, not an error.
    ///
    /// Any custodian check-in for that destination goes with it, in the same
    /// lock: a `destination_id` the owner later re-uses for a *different* device
    /// would otherwise inherit the old device's `high_water` and `caught_up_at`,
    /// so a brand-new custodian holding nothing would render as caught up. The
    /// check-in is pure derived progress — re-earned by the next pull pass — so
    /// dropping it destroys nothing the owner cannot recreate. The destination's
    /// folder-coverage rows go with it too — destination-remove drops all of the
    /// destination's coverage exactly as it drops the client's config rows
    /// (`backup-destinations.md` § Ordinary-folder coverage), and a re-used id
    /// must not inherit a prior enrollment's attachments.
    pub async fn delete_backup_destination(
        &self,
        owner_actor_id: &[u8],
        destination_id: &str,
    ) -> Result<bool> {
        let owner_actor_id = owner_actor_id.to_vec();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM backup_destinations
                 WHERE owner_actor_id = ?1 AND destination_id = ?2",
                rusqlite::params![owner_actor_id, destination_id],
            )
            .map_err(|e| anyhow!("delete backup destination: {e}"))?;
        conn.execute(
            "DELETE FROM backup_custodian_checkins
             WHERE owner_actor_id = ?1 AND destination_id = ?2",
            rusqlite::params![owner_actor_id, destination_id],
        )
        .map_err(|e| anyhow!("delete custodian check-in: {e}"))?;
        conn.execute(
            "DELETE FROM backup_destination_folders
             WHERE owner_actor_id = ?1 AND destination_id = ?2",
            rusqlite::params![owner_actor_id, destination_id],
        )
        .map_err(|e| anyhow!("delete destination folder coverage: {e}"))?;
        Ok(n > 0)
    }

    /// Attach one of the owner's ordinary folders to a registered destination
    /// (`backup-destinations.md` § Ordinary-folder coverage). Returns whether a
    /// coverage row was newly created — idempotent, a re-attach is a no-op.
    ///
    /// Refuses an unregistered `destination_id` rather than storing a dangling
    /// row: coverage of a destination the coordinator will never dial (and the
    /// list read never joins) would be exactly the phantom rail the design
    /// forbids. Folder-side validation (exists, owner-owned, not reserved) is
    /// the handler's — it holds the folder row already.
    pub async fn attach_backup_destination_folder(
        &self,
        owner_actor_id: &[u8],
        destination_id: &str,
        folder_id: i64,
    ) -> Result<bool> {
        let owner_actor_id = owner_actor_id.to_vec();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let registered: bool = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM backup_destinations
                    WHERE owner_actor_id = ?1 AND destination_id = ?2)",
                rusqlite::params![owner_actor_id, destination_id],
                |row| row.get(0),
            )
            .map_err(|e| anyhow!("attach coverage: registry probe: {e}"))?;
        if !registered {
            return Err(anyhow!(
                "destination '{destination_id}' is not registered for this owner"
            ));
        }
        let n = conn
            .execute(
                "INSERT OR IGNORE INTO backup_destination_folders
                    (owner_actor_id, destination_id, folder_id, added_at)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![owner_actor_id, destination_id, folder_id, now],
            )
            .map_err(|e| anyhow!("attach destination folder: {e}"))?;
        Ok(n > 0)
    }

    /// Detach a folder's destination place: delete the coverage row. Returns
    /// whether one existed — idempotent, so a double detach is success. The
    /// destination-side teardown is the coordinator's, on its next pass (the
    /// same contract as destination removal).
    pub async fn detach_backup_destination_folder(
        &self,
        owner_actor_id: &[u8],
        destination_id: &str,
        folder_id: i64,
    ) -> Result<bool> {
        let owner_actor_id = owner_actor_id.to_vec();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM backup_destination_folders
                 WHERE owner_actor_id = ?1 AND destination_id = ?2 AND folder_id = ?3",
                rusqlite::params![owner_actor_id, destination_id, folder_id],
            )
            .map_err(|e| anyhow!("detach destination folder: {e}"))?;
        Ok(n > 0)
    }

    /// Every coverage row this owner has, oldest first, each joined with the
    /// covered folder's current display name. Serves the `destination.list`
    /// join and the coordinator's folder sweep — one read for both, so they
    /// cannot disagree on what is covered.
    pub async fn list_backup_destination_folders(
        &self,
        owner_actor_id: &[u8],
    ) -> Result<Vec<CoveredFolderRow>> {
        let owner_actor_id = owner_actor_id.to_vec();
        let conn = self.conn.lock().await;
        // LEFT JOIN, not INNER: a coverage row whose folder is gone still names
        // a set the coordinator must tear down, so it stays listed — nameless.
        let mut stmt = conn.prepare(
            "SELECT c.destination_id, c.folder_id, f.name, f.name_hash, f.name_sealed
             FROM backup_destination_folders c
             LEFT JOIN folders f ON f.id = c.folder_id
             WHERE c.owner_actor_id = ?1
             ORDER BY c.added_at, c.destination_id, c.folder_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![owner_actor_id], |row| {
            Ok(CoveredFolderRow {
                destination_id: row.get(0)?,
                folder_id: row.get(1)?,
                name: row.get(2)?,
                name_hash: row.get(3)?,
                name_sealed: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every destination this owner has registered, oldest first. Serves both the
    /// client's `fauna.backup.destination.list` read and the coordinator's
    /// per-owner scheduling (which owners × destinations to back up).
    /// **Owner-scoped** — a caller only ever sees its own destinations, the same
    /// conservative direction the writer-grant list took (widening later is
    /// additive; narrowing later is a within-major break).
    pub async fn list_backup_destinations(
        &self,
        owner_actor_id: &[u8],
    ) -> Result<Vec<BackupDestinationRow>> {
        let owner_actor_id = owner_actor_id.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT destination_id, nest_url, nest_id, added_at,
                    kind, custodian_device_id, capacity_cap_bytes
             FROM backup_destinations
             WHERE owner_actor_id = ?1
             ORDER BY added_at, destination_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![owner_actor_id], |row| {
            Ok(BackupDestinationRow {
                destination_id: row.get(0)?,
                nest_url: row.get(1)?,
                nest_id: row.get(2)?,
                added_at: row.get(3)?,
                kind: row.get(4)?,
                custodian_device_id: row.get(5)?,
                capacity_cap_bytes: row.get::<_, Option<i64>>(6)?.map(|c| c as u64),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// The latest check-in a client-device custodian reported for one destination
/// (`message-segment-store.md` § Client-device custodian (pull) → *Check-in*).
/// Latest-wins, so this is a projection rather than a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodianCheckinRow {
    /// How far the device has pulled **and sealed** — the greatest segment id it
    /// holds live, plus one. The nest derives backlog as its own head minus this.
    pub high_water: u64,
    /// Bytes the custodian reported holding, for the usage render.
    pub held_bytes: u64,
    /// `fauna_core::data::CAP_STATE_{OK,REACHED}`.
    pub cap_state: String,
    /// Unix seconds of the most recent check-in, caught up or not.
    pub checked_in_at: i64,
    /// Unix seconds of the most recent check-in **at which the device was caught
    /// up**, or `None` if it never has been. This — not `checked_in_at` — is the
    /// row's `last_upload_time` (`behavior/backup-destinations.md` § Third destination kind:
    /// "the last time the device checked in *having caught up*"). A custodian
    /// that is permanently behind keeps checking in and this stays put, which is
    /// the honest reading.
    pub caught_up_at: Option<i64>,
    /// The custodian's verdict on its **own** local store, as last reported
    /// (`fauna_core::data::AUDIT_STATE_{OK,FAILED}`), or `None` if it has never
    /// reported one. `None` is *not* a failure: a device enrolled minutes ago has
    /// audited nothing, and rendering that as failing would alarm on every
    /// enrollment.
    pub audit_state: Option<String>,
    /// Unix seconds of the last **passing** self-audit the custodian reported.
    /// Carried forward verbatim from the check-in rather than stamped here — it
    /// is the device's own clock reading of when its store last verified, and
    /// only the device can know that.
    pub last_audit_passed_at: Option<i64>,
}

impl CacheDb {
    /// Record a custodian's check-in, replacing that destination's previous one.
    ///
    /// `caught_up` is the nest's own verdict at check-in time (its head reached),
    /// not the device's claim — the device reports where it got to, the nest
    /// decides whether that is level with itself. When it is not, the stored
    /// `caught_up_at` is **carried forward** rather than cleared: the row means
    /// "last time this device was level", and a device that falls behind has not
    /// un-synced the bytes it already holds.
    ///
    /// ⚠ **`audit_state` is the field whose write path needs the strictest
    /// trust, because carry-forward makes a verdict persist.** The audit is
    /// debounced to its own interval, so most passes carry no fresh verdict and
    /// an absent one means "nothing new to say" rather than "healthy" — which is
    /// what stops a rotten store appearing to recover on the next silent pass.
    /// The same property runs the other way: a verdict written here outlives
    /// every later verdict-less pass, so **one** wrong `ok` silences the rot
    /// alarm indefinitely rather than for a single pass. That is why the caller
    /// (`backup_handlers::custodian_checkin_handler`) refuses a check-in that
    /// does not name its device, and why any future writer reaching this column
    /// owes the same standard — a weaker guard here is not a weaker guard for
    /// one pass, it is a permanent one.
    pub async fn put_custodian_checkin(
        &self,
        owner_actor_id: &[u8],
        destination_id: &str,
        high_water: u64,
        held_bytes: u64,
        cap_state: &str,
        caught_up: bool,
        audit_state: Option<&str>,
        last_audit_passed_at: Option<u64>,
    ) -> Result<()> {
        let owner_vec = owner_actor_id.to_vec();
        let now = now_epoch_secs();
        let prior = self
            .get_custodian_checkin(owner_actor_id, destination_id)
            .await?;
        let (prior_caught_up, prior_audit, prior_audit_at) = match prior {
            Some(p) => (p.caught_up_at, p.audit_state, p.last_audit_passed_at),
            None => (None, None, None),
        };
        let caught_up_at = if caught_up {
            Some(now)
        } else {
            prior_caught_up
        };
        // The audit is debounced to its own interval, so most passes carry no
        // fresh verdict. An absent verdict therefore means "nothing new to say",
        // NOT "healthy" and not "unknown again" — carry the last one forward,
        // or a rotten store would appear to recover on the next silent pass.
        let audit_state = audit_state.map(str::to_string).or(prior_audit);
        let last_audit_passed_at = last_audit_passed_at.map(|t| t as i64).or(prior_audit_at);
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO backup_custodian_checkins
                (owner_actor_id, destination_id, high_water, held_bytes, cap_state,
                 checked_in_at, caught_up_at, audit_state, last_audit_passed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                owner_vec,
                destination_id,
                high_water as i64,
                held_bytes as i64,
                cap_state,
                now,
                caught_up_at,
                audit_state,
                last_audit_passed_at,
            ],
        )
        .map_err(|e| anyhow!("put custodian check-in: {e}"))?;
        Ok(())
    }

    /// The latest check-in for one `(owner, destination)`, or `None` if that
    /// custodian has never checked in — which is a real and expected state (a
    /// device enrolled minutes ago, or asleep), not an error.
    pub async fn get_custodian_checkin(
        &self,
        owner_actor_id: &[u8],
        destination_id: &str,
    ) -> Result<Option<CustodianCheckinRow>> {
        let owner_actor_id = owner_actor_id.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT high_water, held_bytes, cap_state, checked_in_at, caught_up_at,
                    audit_state, last_audit_passed_at
             FROM backup_custodian_checkins
             WHERE owner_actor_id = ?1 AND destination_id = ?2",
        )?;
        let mut rows =
            stmt.query_map(rusqlite::params![owner_actor_id, destination_id], |row| {
                Ok(CustodianCheckinRow {
                    high_water: row.get::<_, i64>(0)? as u64,
                    held_bytes: row.get::<_, i64>(1)? as u64,
                    cap_state: row.get(2)?,
                    checked_in_at: row.get(3)?,
                    caught_up_at: row.get(4)?,
                    audit_state: row.get(5)?,
                    last_audit_passed_at: row.get(6)?,
                })
            })?;
        Ok(rows.next().transpose()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER_A: [u8; 32] = [0xAA; 32];
    const OWNER_B: [u8; 32] = [0xBB; 32];
    const DEST_NEST_1: [u8; 32] = [0x51; 32];
    const DEST_NEST_2: [u8; 32] = [0x52; 32];

    #[tokio::test]
    async fn register_list_remove_round_trip() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(
            db.list_backup_destinations(&OWNER_A)
                .await
                .unwrap()
                .is_empty()
        );

        db.put_backup_destination(&OWNER_A, "dest-1", "https://d1.example", &DEST_NEST_1)
            .await
            .unwrap();
        let rows = db.list_backup_destinations(&OWNER_A).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].destination_id, "dest-1");
        assert_eq!(rows[0].nest_url, "https://d1.example");
        assert_eq!(rows[0].nest_id, DEST_NEST_1.to_vec());

        // Remove → gone, and a second remove is a no-op (idempotent).
        assert!(
            db.delete_backup_destination(&OWNER_A, "dest-1")
                .await
                .unwrap()
        );
        assert!(
            !db.delete_backup_destination(&OWNER_A, "dest-1")
                .await
                .unwrap()
        );
        assert!(
            db.list_backup_destinations(&OWNER_A)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn reregister_same_id_replaces_url_and_nest_id_in_place() {
        // A URL/nest-id edit for the same destination_id replaces rather than
        // duplicating — one row per (owner, destination_id).
        let db = CacheDb::open_in_memory().unwrap();
        db.put_backup_destination(&OWNER_A, "dest-1", "https://old.example", &DEST_NEST_1)
            .await
            .unwrap();
        db.put_backup_destination(&OWNER_A, "dest-1", "https://new.example", &DEST_NEST_2)
            .await
            .unwrap();
        let rows = db.list_backup_destinations(&OWNER_A).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].nest_url, "https://new.example");
        assert_eq!(rows[0].nest_id, DEST_NEST_2.to_vec());
    }

    #[tokio::test]
    async fn destinations_are_per_owner_isolated() {
        let db = CacheDb::open_in_memory().unwrap();
        db.put_backup_destination(&OWNER_A, "dest-1", "https://a.example", &DEST_NEST_1)
            .await
            .unwrap();
        db.put_backup_destination(&OWNER_A, "dest-2", "https://a2.example", &DEST_NEST_2)
            .await
            .unwrap();
        db.put_backup_destination(&OWNER_B, "dest-1", "https://b.example", &DEST_NEST_1)
            .await
            .unwrap();

        let a = db.list_backup_destinations(&OWNER_A).await.unwrap();
        assert_eq!(a.len(), 2, "owner A sees exactly its own two destinations");
        let b = db.list_backup_destinations(&OWNER_B).await.unwrap();
        assert_eq!(
            b.len(),
            1,
            "owner B sees only its own destination, never A's"
        );
        assert_eq!(b[0].nest_url, "https://b.example");

        // Removing one of A's leaves B's intact.
        db.delete_backup_destination(&OWNER_A, "dest-1")
            .await
            .unwrap();
        assert_eq!(
            db.list_backup_destinations(&OWNER_A).await.unwrap().len(),
            1
        );
        assert_eq!(
            db.list_backup_destinations(&OWNER_B).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn put_rejects_wrong_length_nest_id() {
        let db = CacheDb::open_in_memory().unwrap();
        let err = db
            .put_backup_destination(&OWNER_A, "dest-1", "https://d.example", &[0u8; 31])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("32 bytes"));
        assert!(
            db.list_backup_destinations(&OWNER_A)
                .await
                .unwrap()
                .is_empty()
        );
    }

    // ── the client-device custodian kind ──────────────────────────────────────

    const DEVICE_A: &str = "device-aaa";
    const DEVICE_B: &str = "device-bbb";

    async fn put_custodian(
        db: &CacheDb,
        owner: &[u8],
        destination_id: &str,
        device_id: &str,
        cap: Option<u64>,
    ) -> Result<()> {
        db.put_backup_destination_of_kind(
            owner,
            destination_id,
            "",
            &[],
            fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE,
            Some(device_id),
            cap,
        )
        .await
    }

    #[tokio::test]
    async fn a_custodian_row_stores_with_no_address_at_all() {
        // Question 1 of the custodian contract: the first kind with no address.
        // The 32-byte nest-id rule must not reach it, or the kind is
        // unregisterable — which is exactly today's incidental refusal.
        let db = CacheDb::open_in_memory().unwrap();
        put_custodian(&db, &OWNER_A, "dest-ipad", DEVICE_A, Some(2_000))
            .await
            .unwrap();

        let rows = db.list_backup_destinations(&OWNER_A).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].kind,
            fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE
        );
        assert_eq!(rows[0].custodian_device_id.as_deref(), Some(DEVICE_A));
        assert_eq!(rows[0].capacity_cap_bytes, Some(2_000));
        assert!(rows[0].nest_url.is_empty());
        assert!(rows[0].nest_id.is_empty());
    }

    #[tokio::test]
    async fn a_nest_row_reads_back_as_kind_nest_with_no_per_kind_fields() {
        // The backfill claim: a row written through the v1 nest-only path is a
        // nest, and its per-kind columns are absent rather than zeroed.
        let db = CacheDb::open_in_memory().unwrap();
        db.put_backup_destination(&OWNER_A, "dest-1", "https://d1.example", &DEST_NEST_1)
            .await
            .unwrap();
        let rows = db.list_backup_destinations(&OWNER_A).await.unwrap();
        assert_eq!(rows[0].kind, fauna_core::data::DESTINATION_KIND_NEST);
        assert_eq!(rows[0].custodian_device_id, None);
        assert_eq!(rows[0].capacity_cap_bytes, None);
    }

    #[tokio::test]
    async fn an_absent_cap_reads_as_uncapped_never_as_a_zero_cap() {
        // A zero cap would report CAP_STATE_REACHED forever having stored
        // nothing; `None` must survive the round trip as `None`.
        let db = CacheDb::open_in_memory().unwrap();
        put_custodian(&db, &OWNER_A, "dest-ipad", DEVICE_A, None)
            .await
            .unwrap();
        let rows = db.list_backup_destinations(&OWNER_A).await.unwrap();
        assert_eq!(rows[0].capacity_cap_bytes, None, "absent cap is uncapped");
    }

    #[tokio::test]
    async fn a_custodian_row_without_a_device_id_is_refused() {
        // A blank device id projects `Inert`: a destination the user sees and
        // nothing can ever drive.
        let db = CacheDb::open_in_memory().unwrap();
        for blank in ["", "   "] {
            let err = put_custodian(&db, &OWNER_A, "dest-ipad", blank, None)
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("custodian_device_id"),
                "unexpected error: {err}"
            );
        }
        assert!(
            db.list_backup_destinations(&OWNER_A)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn an_unknown_kind_is_refused_rather_than_stored() {
        // This build cannot say what such a row must carry; storing it would
        // leave every later reader guessing.
        let db = CacheDb::open_in_memory().unwrap();
        let err = db
            .put_backup_destination_of_kind(&OWNER_A, "dest-s3", "", &[], "s3", None, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown backup destination kind"));
        assert!(
            db.list_backup_destinations(&OWNER_A)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_nest_row_still_refuses_a_short_nest_id() {
        // The kind branch must not weaken the nest arm — the pinned expected
        // peer is what the federation dial trusts.
        let db = CacheDb::open_in_memory().unwrap();
        let err = db
            .put_backup_destination_of_kind(
                &OWNER_A,
                "dest-1",
                "https://d.example",
                &[0u8; 31],
                fauna_core::data::DESTINATION_KIND_NEST,
                None,
                None,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("32 bytes"));
    }

    #[tokio::test]
    async fn the_stored_row_feeds_the_shared_assignment_rule() {
        // The nest's registry row and the device's own host must agree on which
        // row belongs to which device — one rule, two carriers.
        let db = CacheDb::open_in_memory().unwrap();
        put_custodian(&db, &OWNER_A, "dest-ipad", DEVICE_A, Some(4_096))
            .await
            .unwrap();
        let rows = db.list_backup_destinations(&OWNER_A).await.unwrap();

        let mine = fauna_core::data::custodian_assignment_for(
            rows.iter().map(|r| r.custodian_row()),
            DEVICE_A,
        )
        .expect("device A holds this assignment");
        assert_eq!(mine.destination_id, "dest-ipad");
        assert_eq!(mine.capacity_cap_bytes, Some(4_096));

        assert!(
            fauna_core::data::custodian_assignment_for(
                rows.iter().map(|r| r.custodian_row()),
                DEVICE_B,
            )
            .is_none(),
            "device B is not assigned this row"
        );
    }

    #[tokio::test]
    async fn a_check_in_round_trips_and_latest_wins() {
        let db = CacheDb::open_in_memory().unwrap();
        db.put_custodian_checkin(&OWNER_A, "dest-ipad", 7, 1_024, "ok", true, None, None)
            .await
            .unwrap();
        let row = db
            .get_custodian_checkin(&OWNER_A, "dest-ipad")
            .await
            .unwrap()
            .expect("checked in");
        assert_eq!(row.high_water, 7);
        assert_eq!(row.held_bytes, 1_024);
        assert_eq!(row.cap_state, "ok");
        assert!(row.caught_up_at.is_some());

        db.put_custodian_checkin(
            &OWNER_A,
            "dest-ipad",
            9,
            2_048,
            "cap-reached",
            true,
            None,
            None,
        )
        .await
        .unwrap();
        let row = db
            .get_custodian_checkin(&OWNER_A, "dest-ipad")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.high_water, 9, "latest check-in wins");
        assert_eq!(row.cap_state, "cap-reached");
    }

    #[tokio::test]
    async fn a_behind_check_in_carries_the_last_caught_up_time_forward() {
        // `last_upload_time` means "last time this device was LEVEL". A device
        // that falls behind has not un-synced what it already holds, so the
        // stamp must neither advance nor clear.
        let db = CacheDb::open_in_memory().unwrap();
        db.put_custodian_checkin(&OWNER_A, "dest-ipad", 5, 100, "ok", true, None, None)
            .await
            .unwrap();
        let caught_up = db
            .get_custodian_checkin(&OWNER_A, "dest-ipad")
            .await
            .unwrap()
            .unwrap()
            .caught_up_at
            .expect("caught up once");

        db.put_custodian_checkin(&OWNER_A, "dest-ipad", 5, 100, "ok", false, None, None)
            .await
            .unwrap();
        let row = db
            .get_custodian_checkin(&OWNER_A, "dest-ipad")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.caught_up_at,
            Some(caught_up),
            "a behind check-in neither advances nor clears the caught-up stamp"
        );
    }

    #[tokio::test]
    async fn a_never_caught_up_custodian_has_no_caught_up_stamp() {
        let db = CacheDb::open_in_memory().unwrap();
        db.put_custodian_checkin(&OWNER_A, "dest-ipad", 2, 50, "ok", false, None, None)
            .await
            .unwrap();
        let row = db
            .get_custodian_checkin(&OWNER_A, "dest-ipad")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.caught_up_at, None);
        assert!(row.checked_in_at > 0, "it did check in, just not level");
    }

    #[tokio::test]
    async fn removing_a_destination_drops_its_check_in() {
        // Otherwise a re-used destination_id inherits the old device's progress
        // and a brand-new custodian holding nothing renders as caught up.
        let db = CacheDb::open_in_memory().unwrap();
        put_custodian(&db, &OWNER_A, "dest-ipad", DEVICE_A, None)
            .await
            .unwrap();
        db.put_custodian_checkin(&OWNER_A, "dest-ipad", 9, 2_048, "ok", true, None, None)
            .await
            .unwrap();

        db.delete_backup_destination(&OWNER_A, "dest-ipad")
            .await
            .unwrap();
        assert_eq!(
            db.get_custodian_checkin(&OWNER_A, "dest-ipad")
                .await
                .unwrap(),
            None,
            "the check-in went with its destination"
        );
    }

    #[tokio::test]
    async fn put_rejects_empty_destination_id() {
        let db = CacheDb::open_in_memory().unwrap();
        let err = db
            .put_backup_destination(&OWNER_A, "", "https://d.example", &DEST_NEST_1)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
        assert!(
            db.list_backup_destinations(&OWNER_A)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
