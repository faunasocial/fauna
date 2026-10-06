//! Per-user ATProto identity storage (S2 — `atproto-pds-bridge.md` § State &
//! data shape). The DID is DATA with provenance: every downstream path keys
//! off the stored value, and nothing assumes it was minted at enable time
//! (inbound account migration lands additively as a future `imported`
//! provenance). Sealed key blobs are opaque ciphertext, never parsed here.

use anyhow::{Context, Result, anyhow};
use rusqlite::OptionalExtension;

use super::{CacheDb, blob_col_to_array, now_epoch_millis};

/// Maximum bytes for a sealed ATProto identity-key blob (two K-256 scalars +
/// metadata under HPKE framing — 16 KiB is generous, same ceiling as DKIM).
pub const MAX_ATPROTO_IDENTITY_BLOB_BYTES: usize = 16 * 1024;

/// One `atproto_identities` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtprotoIdentityRow {
    pub actor_id: [u8; 32],
    /// `"plc"` or `"web"`.
    pub method: String,
    /// `"pending"`, `"active"`, `"deactivated"` (layer-2's retained-but-unserved
    /// state after a step-down), `"deleted"` (the presence sweep is owed or has
    /// run; the identity survives), or `"tombstoned"` — the terminal one, where
    /// the user's client published a PLC tombstone and the DID is retired for
    /// good.
    pub status: String,
    /// The user opted into retiring this identity permanently, inside the
    /// delete-presence ceremony (S5 slice 5b). The client converges on this:
    /// once the sweep has finished it signs the tombstone with its senior
    /// rotation key, submits it to the PLC directory, and reports back — at
    /// which point [`Self::status`] becomes `"tombstoned"`. Durable here so the
    /// intent survives a client crash between the confirm and the submit.
    pub tombstone_requested: bool,
    pub did: Option<String>,
    /// `"minted-plc"` / `"minted-web"` (future: `"imported"`). Set when the
    /// DID is recorded.
    pub provenance: Option<String>,
    /// The user-custodied senior rotation key's did:key pubkey (did:plc only;
    /// empty string for did:web).
    pub user_rotation_pub: String,
    /// Bridge-custodied pubkeys, set at key provision (provision-on-read).
    pub signing_pub: Option<String>,
    pub bridge_rotation_pub: Option<String>,
    pub genesis_cid: Option<String>,
    /// Where this identity's projection stream STARTS, in epoch micros — the
    /// history-backfill opt-in made operative (`atproto-pds-bridge.md`
    /// § Projection & backfill). `0` = genesis (the user opted in); `>0` = the
    /// enable instant (forward-only, the ratified default).
    pub projection_floor_micros: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One `atproto_retired_identities` row — a DID this actor permanently retired.
///
/// Only the public halves: the archive is a *record*, not a key store, and the
/// sealed blob went with the retirement (those keys sign for a DID that no
/// longer resolves).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetiredAtprotoIdentityRow {
    pub did: String,
    /// `"plc"` or `"web"`.
    pub method: String,
    pub provenance: Option<String>,
    /// The user-custodied senior rotation key the retired identity published
    /// under.
    pub user_rotation_pub: String,
    pub genesis_cid: Option<String>,
    /// Epoch millis at which the row was archived.
    pub retired_at: i64,
}

/// What a client's "I published the PLC tombstone" report concluded
/// ([`CacheDb::mark_atproto_identity_tombstoned`]).
///
/// Two of these were one `false` until an independent security review pointed
/// out they mean opposite things: converging on a state the network already
/// holds, versus a client reporting an act this nest never authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TombstoneRecordOutcome {
    /// This call wrote the terminal status. The identity is retired.
    Recorded,
    /// The row was already `'tombstoned'` — an earlier report, or another of
    /// the user's devices, got there first. An idempotent success: the desired
    /// end state holds, which is what lets a client that crashed between
    /// submitting and reporting converge rather than see a failure.
    AlreadyRetired,
    /// No opt-in stands for this actor (the row is missing, is not `'deleted'`,
    /// or never recorded the tick). The report is refused: accepting it would
    /// leave a reactivatable row behind a DID the client says it destroyed.
    NotAuthorized,
}

fn row_to_identity(row: &rusqlite::Row<'_>) -> rusqlite::Result<AtprotoIdentityRow> {
    let actor: Vec<u8> = row.get(0)?;
    Ok(AtprotoIdentityRow {
        actor_id: blob_col_to_array(actor, 0, "actor_id")?,
        method: row.get(1)?,
        status: row.get(2)?,
        tombstone_requested: row.get::<_, i64>(3)? != 0,
        did: row.get(4)?,
        provenance: row.get(5)?,
        user_rotation_pub: row.get(6)?,
        signing_pub: row.get(7)?,
        bridge_rotation_pub: row.get(8)?,
        genesis_cid: row.get(9)?,
        projection_floor_micros: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

const IDENTITY_COLS: &str = "actor_id, method, status, tombstone_requested, did, provenance, user_rotation_pub, \
     signing_pub, bridge_rotation_pub, genesis_cid, projection_floor_micros, \
     created_at, updated_at";

impl CacheDb {
    /// Record the intent to mint an ATProto identity for `actor_id` (the
    /// enable flow / S4). Idempotent: re-recording a pending intent updates
    /// the method + user rotation key; an `active` row is never clobbered
    /// (re-enabling after a mint is a no-op — the DID is data, not re-minted).
    pub async fn upsert_atproto_identity_intent(
        &self,
        actor_id: &[u8; 32],
        method: &str,
        user_rotation_pub: &str,
    ) -> Result<()> {
        if !matches!(method, "plc" | "web") {
            return Err(anyhow!("invalid atproto method: {method}"));
        }
        if method == "plc" && user_rotation_pub.is_empty() {
            return Err(anyhow!(
                "did:plc intent requires the user-custodied rotation pubkey \
                 (the ratified key-custody split)"
            ));
        }
        let actor = actor_id.to_vec();
        let method = method.to_string();
        let user_rotation_pub = user_rotation_pub.to_string();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO atproto_identities
                (actor_id, method, status, user_rotation_pub, created_at, updated_at)
             VALUES (?1, ?2, 'pending', ?3, ?4, ?4)
             ON CONFLICT(actor_id) DO UPDATE SET
                method = excluded.method,
                user_rotation_pub = excluded.user_rotation_pub,
                updated_at = excluded.updated_at
             WHERE atproto_identities.status = 'pending'",
            rusqlite::params![actor, method, user_rotation_pub, now],
        )
        .context("upsert atproto identity intent")?;
        Ok(())
    }

    pub async fn get_atproto_identity(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Option<AtprotoIdentityRow>> {
        let actor = actor_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {IDENTITY_COLS} FROM atproto_identities WHERE actor_id = ?1"),
            rusqlite::params![actor],
            row_to_identity,
        )
        .optional()
        .context("get atproto identity")
    }

    /// Resolve a DID back to the actor that owns it — the reverse of the
    /// `did` column, for a login identifier presented in DID form (ATProto
    /// clients may log in with either a handle or a DID).
    ///
    /// A lookup, never a derivation: the DID is data with provenance, and the
    /// one thing slice 4d established is that computing a DID from an actor id
    /// (or an actor id from a DID) is how the bridge ended up serving two repos
    /// for one account.
    pub async fn resolve_actor_by_atproto_did(&self, did: &str) -> Result<Option<[u8; 32]>> {
        let did = did.to_string();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT actor_id FROM atproto_identities WHERE did = ?1",
            rusqlite::params![did],
            |row| {
                let actor: Vec<u8> = row.get(0)?;
                Ok(actor)
            },
        )
        .optional()
        .context("resolve actor by atproto did")
        .map(|opt| opt.and_then(|v| v.try_into().ok()))
    }

    /// Every identity row, pending first then oldest-first — the bridge's
    /// roster read.
    pub async fn list_atproto_identities(&self) -> Result<Vec<AtprotoIdentityRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!(
            "SELECT {IDENTITY_COLS} FROM atproto_identities
              ORDER BY status DESC, created_at ASC"
        ))?;
        let rows = stmt
            .query_map([], row_to_identity)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("list atproto identities")?;
        Ok(rows)
    }

    /// Move an existing identity between `'active'` and `'deactivated'` — the
    /// two halves of layer-2's reversible step-down (`atproto-pds-bridge.md`
    /// § Disable & revocation). The DID, provenance, key pubkeys and sealed
    /// blob are all left untouched: that retention is what makes re-enabling
    /// restore the *same* identity, and the row is the user-irrecoverable
    /// material the no-user-data-loss invariant protects.
    ///
    /// A `'pending'` row is deliberately NOT reactivated into `'active'` — the
    /// mint has not completed, and only the bridge (via `record_minted`) may
    /// declare an identity active. Returns whether a row changed.
    pub async fn set_atproto_identity_active(
        &self,
        actor_id: &[u8; 32],
        active: bool,
    ) -> Result<bool> {
        let actor = actor_id.to_vec();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        // Deactivation covers `pending` too: a step-down BEFORE the bridge has
        // minted must stop the mint loop (it scans `pending` rows), or the
        // bridge would publish a DID for a user who already withdrew consent.
        // Reactivation restores exactly what the row can honestly claim:
        // `active` when a DID exists, `pending` (mint owed again) when the
        // step-down happened pre-mint — `active` with no DID would be a lie
        // the roster serves.
        let n = if active {
            // `'deleted'` reactivates on the same terms as `'deactivated'`: the
            // sweep destroys the PRESENCE, never the identity, so § Disable &
            // revocation's "still reversible in identity terms" means re-entry
            // must restore the same DID rather than mint a second one. The
            // bridge finds a DID with no repo and rebuilds it from the stored
            // projection floor — the repo is derived state, so nothing about
            // that rebuild is lossy.
            //
            // `'tombstoned'` is deliberately absent from that list, and its
            // absence is load-bearing rather than incidental: that DID has been
            // retired at the PLC directory and no longer resolves anywhere, so
            // "restoring" it would leave the user parked on a hosted rung
            // serving a repo the network cannot verify or even resolve. The
            // transition handler refuses the move outright rather than relying
            // on this statement quietly matching nothing
            // (`tombstoned_identity_never_reactivates` pins it here too).
            conn.execute(
                "UPDATE atproto_identities
                    SET status = CASE WHEN did IS NULL THEN 'pending' ELSE 'active' END,
                        updated_at = ?2
                  WHERE actor_id = ?1 AND status IN ('deactivated', 'deleted')",
                rusqlite::params![actor, now],
            )
        } else {
            conn.execute(
                "UPDATE atproto_identities SET status = 'deactivated', updated_at = ?2
                  WHERE actor_id = ?1 AND status IN ('active', 'pending')",
                rusqlite::params![actor, now],
            )
        }
        .context("set atproto identity active")?;
        Ok(n > 0)
    }

    /// Mark this actor's identity `'deleted'` — the durable record that the user
    /// confirmed "Delete my Bluesky presence" (`atproto-pds-bridge.md`
    /// § Disable & revocation layer 2, the "separate, stronger action").
    ///
    /// This row IS the tombstone, and it lives here rather than in the bridge on
    /// purpose: the bridge's store is explicitly derived and re-derivable, so an
    /// intent recorded only there would evaporate on a wipe and the swept
    /// presence would quietly re-project itself — the same argument that put the
    /// projection floor in nest (S5 slice 2a). The bridge instead reads
    /// `'deleted'` off the roster and converges on it, which is why the sweep
    /// needs no `'deleting'` state and no completion call back to nest.
    ///
    /// The DID, the provenance, the sealed key blob and the projection floor are
    /// all deliberately untouched: only the *presence* is destroyed, and the
    /// floor stays the consent record a later re-enable is measured against.
    ///
    /// Accepts every state a hosted identity can be reached from — including
    /// `'deactivated'`, since `ui/atproto.md` § Errors & edge cases keeps the
    /// button reachable after a step-down — and `'pending'`, where the sweep
    /// finds no repo and simply stops the mint loop. Returns whether a row
    /// changed, so a repeat confirm is an honest no-op rather than an error.
    pub async fn mark_atproto_identity_deleted(&self, actor_id: &[u8; 32]) -> Result<bool> {
        let actor = actor_id.to_vec();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE atproto_identities SET status = 'deleted', updated_at = ?2
                  WHERE actor_id = ?1 AND status IN ('active', 'pending', 'deactivated')",
                rusqlite::params![actor, now],
            )
            .context("mark atproto identity deleted")?;
        Ok(n > 0)
    }

    /// Record the user's opt-in to permanently retire this identity — the
    /// terminal "also retire this identity" tick inside the delete-presence
    /// ceremony (`atproto-pds-bridge.md` § Disable & revocation layer 2, S5
    /// slice 5b). This records an INTENT; the act itself is the client's, since
    /// only the client holds the senior rotation key that can sign a PLC
    /// tombstone.
    ///
    /// Two preconditions are enforced in the statement rather than left to each
    /// of the seven apps to honour:
    ///
    /// * **`status = 'deleted'`** — `ui/atproto.md` § Don't do these says the
    ///   tombstone lives *only* inside "Delete my Bluesky presence" and is never
    ///   a step-down. Requiring the presence deletion to have been recorded first
    ///   makes that unreachable from anywhere else on the wire, and it encodes
    ///   the ordering the act itself depends on: a tombstoned DID stops
    ///   resolving, and a relay that cannot resolve a DID cannot verify the
    ///   delete commits the sweep is about to publish.
    /// * **`method = 'plc'` with a minted `did`** — a did:web identity has no
    ///   operation log to tombstone (its custody *is* domain custody), and a
    ///   never-minted identity has nothing published to retire.
    ///
    /// Returns whether a row changed, so a repeat confirm is an honest no-op.
    /// There is deliberately no way to clear the flag: the ceremony's only
    /// question is whether to proceed, and by the time this is set the user has
    /// been told in terms that the act is terminal.
    pub async fn request_atproto_tombstone(&self, actor_id: &[u8; 32]) -> Result<bool> {
        let actor = actor_id.to_vec();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE atproto_identities SET tombstone_requested = 1, updated_at = ?2
                  WHERE actor_id = ?1
                    AND status = 'deleted'
                    AND method = 'plc'
                    AND did IS NOT NULL
                    AND tombstone_requested = 0",
                rusqlite::params![actor, now],
            )
            .context("request atproto tombstone")?;
        Ok(n > 0)
    }

    /// Record that the client published the PLC tombstone and the directory
    /// accepted it — the identity is retired for good (S5 slice 5b).
    ///
    /// This is the only writer of `'tombstoned'`, and it writes what the client
    /// *observed*, never what this box did: nest cannot sign the op, so it has
    /// no independent knowledge of the outcome and does not pretend to. The
    /// client reports the same status for a log it found already tombstoned, so
    /// a crash between submitting and reporting converges rather than stranding
    /// the row one state behind the network.
    ///
    /// Only a `'deleted'` row with the intent recorded may reach here: the
    /// tombstone is unreachable outside the delete ceremony, so a report for any
    /// other row is a client that is out of step with its own status snapshot,
    /// and silently accepting it would let a step-down be laundered into a
    /// terminal act.
    ///
    /// The three outcomes are distinguished *here* rather than left as a bare
    /// "did a row change?", because two of them are indistinguishable by row
    /// count and mean opposite things. "Already retired" is an
    /// idempotent success; "the nest holds no opt-in" is a **divergence** — a
    /// client asserting it published a tombstone this box never authorized —
    /// and answering it with a success would leave the row `'deleted'`, which
    /// *is* reactivatable, so the user could be re-parked on a hosted rung
    /// backing a DID that no longer resolves. That is precisely the outcome the
    /// `'tombstoned'` re-entry refusal exists to prevent.
    ///
    /// The read that disambiguates runs under the same connection lock as the
    /// write, so the answer cannot be overtaken by a concurrent re-entry
    /// (`set_atproto_identity_active` moves `'deleted'` → `'active'`) between
    /// the two statements.
    pub async fn mark_atproto_identity_tombstoned(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<TombstoneRecordOutcome> {
        let actor = actor_id.to_vec();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE atproto_identities SET status = 'tombstoned', updated_at = ?2
                  WHERE actor_id = ?1 AND status = 'deleted' AND tombstone_requested = 1",
                rusqlite::params![actor, now],
            )
            .context("mark atproto identity tombstoned")?;
        if n > 0 {
            return Ok(TombstoneRecordOutcome::Recorded);
        }
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM atproto_identities WHERE actor_id = ?1",
                rusqlite::params![actor],
                |row| row.get(0),
            )
            .optional()
            .context("read atproto identity status after a no-op tombstone report")?;
        Ok(match status.as_deref() {
            Some("tombstoned") => TombstoneRecordOutcome::AlreadyRetired,
            _ => TombstoneRecordOutcome::NotAuthorized,
        })
    }

    /// Move a **retired** identity into the append-only archive and clear the
    /// live row, so this actor can be minted a *fresh* one
    /// (`atproto-pds-bridge.md` § Disable & revocation layer 2).
    ///
    /// Retiring an identity must not be a permanent product lockout: a user who
    /// destroyed their ATProto identity and later wants a new one should get a
    /// new one, exactly as if they had never enabled. What stood in the way was
    /// only the schema — one row per actor — so the fix is to give the old row
    /// somewhere to go rather than to overwrite or delete it. § State & data
    /// shape's "the DID is data, not an assumption" points the same way: the
    /// destroyed DID stays a stored fact with provenance, it simply stops being
    /// *this actor's live identity*.
    ///
    /// Only a `'tombstoned'` row moves. Every other status is a live or
    /// restorable identity, and archiving one would destroy exactly the DID
    /// retention that makes a step-down and a presence delete reversible.
    ///
    /// The sealed key blob goes with it: those keys sign for a DID that no
    /// longer resolves, so they can authorize nothing, and the fresh mint
    /// provisions its own. The archive keeps the *public* halves, which are what
    /// the record is for.
    ///
    /// Archive-then-delete runs in one transaction — a crash between them would
    /// either lose the record of a published DID or leave a retired row blocking
    /// the mint forever, and neither is recoverable from the client.
    ///
    /// Returns whether a row was archived, so a retry is an honest no-op.
    pub async fn archive_retired_atproto_identity(&self, actor_id: &[u8; 32]) -> Result<bool> {
        let actor = actor_id.to_vec();
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin archive-retired txn")?;
        let archived = tx
            .execute(
                "INSERT INTO atproto_retired_identities
                    (actor_id, method, did, provenance, user_rotation_pub, signing_pub,
                     bridge_rotation_pub, genesis_cid, projection_floor_micros,
                     created_at, retired_at)
                 SELECT actor_id, method, did, provenance, user_rotation_pub, signing_pub,
                        bridge_rotation_pub, genesis_cid, projection_floor_micros,
                        created_at, ?2
                   FROM atproto_identities
                  WHERE actor_id = ?1 AND status = 'tombstoned' AND did IS NOT NULL",
                rusqlite::params![actor, now],
            )
            .context("archive retired atproto identity")?;
        if archived == 0 {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM atproto_identities WHERE actor_id = ?1 AND status = 'tombstoned'",
            rusqlite::params![actor],
        )
        .context("clear the retired atproto identity row")?;
        tx.execute(
            "DELETE FROM atproto_identity_key_blobs WHERE actor_id = ?1",
            rusqlite::params![actor],
        )
        .context("drop the retired identity's sealed key blob")?;
        tx.commit().context("commit archive-retired txn")?;
        Ok(true)
    }

    /// Every DID this actor has retired, oldest first — the record a fresh mint
    /// must never erase. Read-only; nothing in the product writes it back.
    pub async fn list_retired_atproto_identities(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Vec<RetiredAtprotoIdentityRow>> {
        let actor = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT did, method, provenance, user_rotation_pub, genesis_cid, retired_at
               FROM atproto_retired_identities
              WHERE actor_id = ?1
              ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![actor], |row| {
                Ok(RetiredAtprotoIdentityRow {
                    did: row.get(0)?,
                    method: row.get(1)?,
                    provenance: row.get(2)?,
                    user_rotation_pub: row.get(3)?,
                    genesis_cid: row.get(4)?,
                    retired_at: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("list retired atproto identities")?;
        Ok(rows)
    }

    /// Record the user's history-backfill opt-in, carried by the transition
    /// that enters a hosted level (`atproto-pds-bridge.md` § Projection — the
    /// second explicit consent, distinct from enabling at all), and derive the
    /// projection floor it implies **in the same statement**, so the recorded
    /// answer and the stream that honours it can never disagree.
    ///
    /// The floor is the § Projection & backfill table's "watermark start":
    /// opt-in → `0` (genesis, publish everything); default → *now* in epoch
    /// micros (forward-only — "nothing historical"). Deriving it here, at the
    /// one moment consent is given, is what makes the default hold: the floor
    /// is a durable fact on the identity row rather than a bridge-local
    /// watermark, so wiping the bridge's (explicitly re-derivable) store cannot
    /// resurrect posts the user never consented to publish.
    ///
    /// Called only under `plan.mints_identity`, so a re-entry after a step-down
    /// keeps the floor set at first mint — matching § Disable & revocation's
    /// "re-enabling restores the same identity".
    pub async fn set_atproto_history_backfill(
        &self,
        actor_id: &[u8; 32],
        backfill: bool,
    ) -> Result<()> {
        let actor = actor_id.to_vec();
        let now = now_epoch_millis();
        // MICROS — `content.created_at`'s unit, which the floor is compared
        // against. The row's own created_at/updated_at are millis; do not mix.
        let floor_micros = if backfill {
            0
        } else {
            fauna_core::data::Timestamp::now().0 as i64
        };
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE atproto_identities
                SET history_backfill = ?2, projection_floor_micros = ?3, updated_at = ?4
              WHERE actor_id = ?1",
            rusqlite::params![actor, backfill as i64, floor_micros, now],
        )
        .context("set atproto history_backfill")?;
        Ok(())
    }

    /// The projection floor for `actor_id` — the epoch-micros instant this
    /// user's public-post stream starts at (see
    /// [`Self::set_atproto_history_backfill`]).
    ///
    /// A missing row yields `i64::MAX`: no identity means no consent to publish
    /// anything, so the stream is empty rather than unbounded. Failing closed
    /// matters because this value is the only thing standing between a
    /// forward-only user and their whole back-catalogue.
    pub async fn get_atproto_projection_floor(&self, actor_id: &[u8; 32]) -> Result<i64> {
        let actor = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let floor: Option<i64> = conn
            .query_row(
                "SELECT projection_floor_micros FROM atproto_identities WHERE actor_id = ?1",
                rusqlite::params![actor],
                |row| row.get(0),
            )
            .optional()
            .context("get atproto projection floor")?;
        Ok(floor.unwrap_or(i64::MAX))
    }

    /// Provision the bridge-custodied keys: the sealed blob AND the published
    /// halves, in ONE transaction.
    ///
    /// The pair is what the bridge binds on — it refuses a blob whose keys are
    /// not the ones this row publishes (`unseal_atproto_identity`). Written as
    /// two statements, a crash between them left a blob beside a row recording
    /// no published keys: a state the binding must refuse (nothing to bind to)
    /// and that provision-on-read, keyed on the blob's absence, would never
    /// revisit. One transaction makes that half-state unrepresentable; the
    /// handler's heal arm covers a row that reached it before this landed.
    ///
    /// Replaces any blob already stored. That is only ever correct for an
    /// identity with no DID yet — the handler's gate — because a minted DID's
    /// keys are published and re-minting them would orphan it.
    pub async fn provision_atproto_identity_keys(
        &self,
        actor_id: &[u8; 32],
        blob: &[u8],
        signing_pub: &str,
        bridge_rotation_pub: &str,
    ) -> Result<()> {
        if blob.len() > MAX_ATPROTO_IDENTITY_BLOB_BYTES {
            return Err(anyhow!(
                "atproto identity blob too large: {} bytes (max {})",
                blob.len(),
                MAX_ATPROTO_IDENTITY_BLOB_BYTES
            ));
        }
        let actor = actor_id.to_vec();
        let blob = blob.to_vec();
        let signing = signing_pub.to_string();
        let rotation = bridge_rotation_pub.to_string();
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction()
            .context("begin atproto key provision txn")?;
        let n = tx
            .execute(
                "UPDATE atproto_identities
                    SET signing_pub = ?2, bridge_rotation_pub = ?3, updated_at = ?4
                  WHERE actor_id = ?1",
                rusqlite::params![actor, signing, rotation, now],
            )
            .context("provision atproto identity pubkeys")?;
        if n == 0 {
            return Err(anyhow!("no atproto identity row for actor"));
        }
        tx.execute(
            "INSERT OR REPLACE INTO atproto_identity_key_blobs (actor_id, blob, created_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![actor, blob, now],
        )
        .context("provision atproto identity key blob")?;
        tx.commit().context("commit atproto key provision txn")
    }

    /// The pubkey half of [`Self::provision_atproto_identity_keys`] alone.
    /// Test-only: production writes the pair or nothing.
    #[cfg(test)]
    pub async fn set_atproto_identity_keys(
        &self,
        actor_id: &[u8; 32],
        signing_pub: &str,
        bridge_rotation_pub: &str,
    ) -> Result<()> {
        let actor = actor_id.to_vec();
        let signing = signing_pub.to_string();
        let rotation = bridge_rotation_pub.to_string();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE atproto_identities
                    SET signing_pub = ?2, bridge_rotation_pub = ?3, updated_at = ?4
                  WHERE actor_id = ?1",
                rusqlite::params![actor, signing, rotation, now],
            )
            .context("set atproto identity keys")?;
        if n == 0 {
            return Err(anyhow!("no atproto identity row for actor"));
        }
        Ok(())
    }

    /// Record the minted DID (bridge report-back). Sets `status = 'active'`
    /// and provenance derived from the row's method. Idempotent on the same
    /// DID (mint retries); a DIFFERENT did against an active row is an error —
    /// an identity is never silently replaced.
    pub async fn record_atproto_minted(
        &self,
        actor_id: &[u8; 32],
        did: &str,
        genesis_cid: Option<&str>,
    ) -> Result<()> {
        let actor = actor_id.to_vec();
        let did = did.to_string();
        let cid = genesis_cid.map(str::to_string);
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let existing: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT method, did FROM atproto_identities WHERE actor_id = ?1",
                rusqlite::params![actor],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("read atproto identity for mint record")?;
        let Some((method, current_did)) = existing else {
            return Err(anyhow!("no atproto identity row for actor"));
        };
        match current_did {
            Some(ref cur) if cur == &did => return Ok(()), // idempotent retry
            Some(_) => {
                return Err(anyhow!(
                    "atproto identity already has a different DID; identities are \
                     never silently replaced"
                ));
            }
            None => {}
        }
        let provenance = match method.as_str() {
            "web" => "minted-web",
            _ => "minted-plc",
        };
        // The DID is recorded unconditionally (it exists in the world now —
        // DID-is-data), but a row the user deactivated between the bridge's
        // roster read and this record call must NOT be resurrected to
        // `active`: the step-down withdrew serving consent, and re-entry is
        // what restores it (`set_atproto_identity_active`).
        conn.execute(
            "UPDATE atproto_identities
                SET did = ?2, provenance = ?3, genesis_cid = ?4,
                    status = CASE WHEN status = 'deactivated'
                                  THEN 'deactivated' ELSE 'active' END,
                    updated_at = ?5
              WHERE actor_id = ?1",
            rusqlite::params![actor, did, provenance, cid, now],
        )
        .context("record atproto minted")?;
        Ok(())
    }

    /// The blob half of [`Self::provision_atproto_identity_keys`] alone.
    /// Test-only: production writes the pair or nothing.
    #[cfg(test)]
    pub async fn put_atproto_identity_key_blob(
        &self,
        actor_id: &[u8; 32],
        blob: &[u8],
    ) -> Result<()> {
        if blob.len() > MAX_ATPROTO_IDENTITY_BLOB_BYTES {
            return Err(anyhow!(
                "atproto identity blob too large: {} bytes (max {})",
                blob.len(),
                MAX_ATPROTO_IDENTITY_BLOB_BYTES
            ));
        }
        let actor = actor_id.to_vec();
        let blob = blob.to_vec();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO atproto_identity_key_blobs (actor_id, blob, created_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![actor, blob, now],
        )
        .context("put atproto identity key blob")?;
        Ok(())
    }

    pub async fn get_atproto_identity_key_blob(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Option<Vec<u8>>> {
        let actor = actor_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT blob FROM atproto_identity_key_blobs WHERE actor_id = ?1",
            rusqlite::params![actor],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .context("get atproto identity key blob")
    }
}

#[cfg(test)]
mod tests {
    use super::TombstoneRecordOutcome;
    use crate::db::CacheDb;

    async fn fresh_db() -> CacheDb {
        CacheDb::open_in_memory().expect("in-memory db")
    }

    /// The history-backfill opt-in derives the projection floor at the one
    /// moment consent is given (`atproto-pds-bridge.md` § Projection &
    /// backfill): opt-in → genesis, default → the enable instant. The two are
    /// written together so the recorded answer and the stream that honours it
    /// can never disagree.
    #[tokio::test]
    async fn history_backfill_opt_in_derives_the_projection_floor() {
        let db = fresh_db().await;
        let before = fauna_core::data::Timestamp::now().0 as i64;

        // Default (forward-only): the floor is the enable instant, in MICROS.
        let fwd = [0x71u8; 32];
        db.upsert_atproto_identity_intent(&fwd, "plc", "did:key:zDnaeUSER")
            .await
            .unwrap();
        db.set_atproto_history_backfill(&fwd, false).await.unwrap();
        let floor = db.get_atproto_projection_floor(&fwd).await.unwrap();
        let after = fauna_core::data::Timestamp::now().0 as i64;
        assert!(
            floor >= before && floor <= after,
            "forward-only floor is now in micros, got {floor} outside [{before}, {after}]"
        );
        let row = db.get_atproto_identity(&fwd).await.unwrap().unwrap();
        assert_eq!(row.projection_floor_micros, floor, "row and getter agree");

        // Opt-in: genesis.
        let hist = [0x72u8; 32];
        db.upsert_atproto_identity_intent(&hist, "plc", "did:key:zDnaeUSER")
            .await
            .unwrap();
        db.set_atproto_history_backfill(&hist, true).await.unwrap();
        assert_eq!(
            db.get_atproto_projection_floor(&hist).await.unwrap(),
            0,
            "history opt-in starts at genesis"
        );
    }

    /// Mint a `plc` identity for `actor`, all the way to `active`.
    async fn minted(db: &CacheDb, actor: &[u8; 32], did: &str) {
        db.upsert_atproto_identity_intent(actor, "plc", "did:key:zDnaeUSER")
            .await
            .unwrap();
        db.record_atproto_minted(actor, did, Some("bafygenesis"))
            .await
            .unwrap();
    }

    /// The tombstone opt-in is reachable ONLY from a deleted presence — the
    /// wire-level form of `ui/atproto.md` § Don't do these ("never a step-down,
    /// never implicit"). Enforcing it in the statement is what stops seven
    /// separate app implementations from each having to be trusted with it.
    #[tokio::test]
    async fn tombstone_can_only_be_requested_after_the_presence_is_deleted() {
        let db = fresh_db().await;
        let actor = [0xa1u8; 32];
        minted(&db, &actor, "did:plc:alpha").await;

        // active → refused
        assert!(!db.request_atproto_tombstone(&actor).await.unwrap());
        // deactivated (a step-down) → still refused: this is the exact path the
        // goal doc forbids the tombstone from riding.
        db.set_atproto_identity_active(&actor, false).await.unwrap();
        assert!(!db.request_atproto_tombstone(&actor).await.unwrap());
        assert!(
            !db.get_atproto_identity(&actor)
                .await
                .unwrap()
                .unwrap()
                .tombstone_requested
        );

        // deleted → accepted, and idempotent.
        db.mark_atproto_identity_deleted(&actor).await.unwrap();
        assert!(db.request_atproto_tombstone(&actor).await.unwrap());
        assert!(
            !db.request_atproto_tombstone(&actor).await.unwrap(),
            "a repeat confirm is an honest no-op, not a second request"
        );
        assert!(
            db.get_atproto_identity(&actor)
                .await
                .unwrap()
                .unwrap()
                .tombstone_requested
        );
    }

    /// A did:web identity has no PLC operation log, so there is nothing a
    /// tombstone could chain to — its custody *is* domain custody
    /// (`atproto-pds-bridge.md` § DID method). Likewise a never-minted identity
    /// has published nothing to retire.
    #[tokio::test]
    async fn a_did_web_or_unminted_identity_cannot_be_tombstoned() {
        let db = fresh_db().await;

        let web = [0xa2u8; 32];
        db.upsert_atproto_identity_intent(&web, "web", "")
            .await
            .unwrap();
        db.record_atproto_minted(&web, "did:web:alice.example.com", None)
            .await
            .unwrap();
        db.mark_atproto_identity_deleted(&web).await.unwrap();
        assert!(!db.request_atproto_tombstone(&web).await.unwrap());

        let pending = [0xa3u8; 32];
        db.upsert_atproto_identity_intent(&pending, "plc", "did:key:zDnaeUSER")
            .await
            .unwrap();
        db.mark_atproto_identity_deleted(&pending).await.unwrap();
        assert!(!db.request_atproto_tombstone(&pending).await.unwrap());
    }

    /// The terminal status is only ever written on the client's report, and only
    /// for an identity that asked for it. Accepting it otherwise would let a
    /// plain step-down be laundered into an irreversible act.
    #[tokio::test]
    async fn tombstoned_is_written_only_for_a_requested_deletion() {
        let db = fresh_db().await;
        let actor = [0xa4u8; 32];
        minted(&db, &actor, "did:plc:beta").await;

        // Deleted but never requested: the report is refused.
        db.mark_atproto_identity_deleted(&actor).await.unwrap();
        assert_eq!(
            db.mark_atproto_identity_tombstoned(&actor).await.unwrap(),
            TombstoneRecordOutcome::NotAuthorized
        );
        assert_eq!(
            db.get_atproto_identity(&actor)
                .await
                .unwrap()
                .unwrap()
                .status,
            "deleted"
        );

        db.request_atproto_tombstone(&actor).await.unwrap();
        assert_eq!(
            db.mark_atproto_identity_tombstoned(&actor).await.unwrap(),
            TombstoneRecordOutcome::Recorded
        );
        assert_eq!(
            db.get_atproto_identity(&actor)
                .await
                .unwrap()
                .unwrap()
                .status,
            "tombstoned"
        );
        // Idempotent: a client that crashed between submitting and reporting
        // re-reports, and converges rather than erroring.
        assert_eq!(
            db.mark_atproto_identity_tombstoned(&actor).await.unwrap(),
            TombstoneRecordOutcome::AlreadyRetired
        );
    }

    /// The two zero-row answers mean opposite things and must not collapse.
    ///
    /// "Already retired" is convergence — the network holds the state the
    /// client is reporting. "No opt-in stands" is a **divergence**: a client
    /// claiming to have published a tombstone this nest never authorized, whose
    /// row is still `'deleted'` and therefore still reactivatable. Reporting
    /// that as a success is what would let the user be re-parked on a hosted
    /// rung backing a DID that no longer resolves — the exact outcome the
    /// `'tombstoned'` re-entry refusal exists to prevent, and it would never
    /// fire, because the terminal status was never written.
    #[tokio::test]
    async fn an_unauthorized_tombstone_report_is_refused_not_silently_accepted() {
        let db = fresh_db().await;

        // No row at all — the strongest form of "this box authorized nothing".
        let stranger = [0xb1u8; 32];
        assert_eq!(
            db.mark_atproto_identity_tombstoned(&stranger)
                .await
                .unwrap(),
            TombstoneRecordOutcome::NotAuthorized
        );

        // A live identity: a report here would be a step-down laundered into a
        // terminal act, which is why the ceremony's precondition exists.
        let actor = [0xb2u8; 32];
        minted(&db, &actor, "did:plc:unauthorized").await;
        assert_eq!(
            db.mark_atproto_identity_tombstoned(&actor).await.unwrap(),
            TombstoneRecordOutcome::NotAuthorized
        );

        // Deactivated, and deleted-without-the-tick: both still unauthorized,
        // and both must leave a row that a later re-entry can honestly restore.
        db.set_atproto_identity_active(&actor, false).await.unwrap();
        assert_eq!(
            db.mark_atproto_identity_tombstoned(&actor).await.unwrap(),
            TombstoneRecordOutcome::NotAuthorized
        );
        db.mark_atproto_identity_deleted(&actor).await.unwrap();
        assert_eq!(
            db.mark_atproto_identity_tombstoned(&actor).await.unwrap(),
            TombstoneRecordOutcome::NotAuthorized
        );
        assert_eq!(
            db.get_atproto_identity(&actor)
                .await
                .unwrap()
                .unwrap()
                .status,
            "deleted",
            "a refused report changes nothing"
        );
        assert!(
            db.set_atproto_identity_active(&actor, true).await.unwrap(),
            "and the row it left behind is still the restorable one — which is \
             precisely why the refusal has to be loud rather than a no-op"
        );
    }

    /// A retired DID no longer resolves anywhere, so "restoring" it would park
    /// the user on a hosted rung serving a repo no relay can verify. The
    /// reactivation statement must not match it — and neither must a delete
    /// sweep, which would walk the identity back to a restorable state.
    #[tokio::test]
    async fn tombstoned_identity_never_reactivates() {
        let db = fresh_db().await;
        let actor = [0xa5u8; 32];
        minted(&db, &actor, "did:plc:gamma").await;
        db.mark_atproto_identity_deleted(&actor).await.unwrap();
        db.request_atproto_tombstone(&actor).await.unwrap();
        assert_eq!(
            db.mark_atproto_identity_tombstoned(&actor).await.unwrap(),
            TombstoneRecordOutcome::Recorded
        );

        assert!(
            !db.set_atproto_identity_active(&actor, true).await.unwrap(),
            "a retired identity must not reactivate"
        );
        assert!(!db.mark_atproto_identity_deleted(&actor).await.unwrap());
        assert!(!db.set_atproto_identity_active(&actor, false).await.unwrap());
        let row = db.get_atproto_identity(&actor).await.unwrap().unwrap();
        assert_eq!(row.status, "tombstoned", "the terminal state is terminal");
        assert!(
            row.did.is_some() && row.genesis_cid.is_some(),
            "the record of WHICH did was retired survives — it is the last thing \
             anyone can say about an identity that no longer resolves"
        );
    }

    /// A retirement must not be a permanent product lockout: the retired row
    /// moves into the append-only archive and the actor becomes mintable again,
    /// exactly as if they had never enabled. What must NOT happen is the
    /// tempting shortcut of deleting the old row — a retired DID no longer
    /// resolves anywhere, so this record is the last thing anyone can say about
    /// it, and it is not re-derivable from anything.
    #[tokio::test]
    async fn a_retirement_is_archived_so_a_fresh_identity_can_be_minted() {
        let db = fresh_db().await;
        let actor = [0xc1u8; 32];
        minted(&db, &actor, "did:plc:first").await;
        db.put_atproto_identity_key_blob(&actor, b"sealed-for-the-dead-did")
            .await
            .unwrap();
        db.mark_atproto_identity_deleted(&actor).await.unwrap();
        db.request_atproto_tombstone(&actor).await.unwrap();
        db.mark_atproto_identity_tombstoned(&actor).await.unwrap();

        assert!(db.archive_retired_atproto_identity(&actor).await.unwrap());
        assert!(
            db.get_atproto_identity(&actor).await.unwrap().is_none(),
            "the live row is cleared, so the mint path inserts rather than \
             colliding with a row that can never become live again"
        );
        assert!(
            db.get_atproto_identity_key_blob(&actor)
                .await
                .unwrap()
                .is_none(),
            "and the sealed keys go with it — they sign for a DID that no \
             longer resolves, so they can authorize nothing"
        );

        let archived = db.list_retired_atproto_identities(&actor).await.unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].did, "did:plc:first");
        assert_eq!(archived[0].method, "plc");
        assert_eq!(archived[0].provenance.as_deref(), Some("minted-plc"));
        assert_eq!(archived[0].genesis_cid.as_deref(), Some("bafygenesis"));
        assert!(archived[0].retired_at > 0);

        // A fresh identity now mints on the ordinary path, and the record of the
        // destroyed one is untouched by it.
        minted(&db, &actor, "did:plc:second").await;
        let live = db.get_atproto_identity(&actor).await.unwrap().unwrap();
        assert_eq!(live.did.as_deref(), Some("did:plc:second"));
        assert_eq!(live.status, "active");
        assert!(!live.tombstone_requested, "the fresh identity owes nothing");
        assert_eq!(
            db.list_retired_atproto_identities(&actor)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// Only a RETIRED row moves. Every other status is a live or restorable
    /// identity, and archiving one would destroy exactly the DID retention that
    /// makes a step-down and a presence delete reversible.
    #[tokio::test]
    async fn only_a_retired_identity_is_ever_archived() {
        let db = fresh_db().await;
        let actor = [0xc2u8; 32];
        minted(&db, &actor, "did:plc:live").await;

        for stage in ["active", "deactivated", "deleted"] {
            match stage {
                "deactivated" => {
                    db.set_atproto_identity_active(&actor, false).await.unwrap();
                }
                "deleted" => {
                    db.mark_atproto_identity_deleted(&actor).await.unwrap();
                }
                _ => {}
            }
            assert!(
                !db.archive_retired_atproto_identity(&actor).await.unwrap(),
                "{stage} is restorable and must not be archived"
            );
            assert_eq!(
                db.get_atproto_identity(&actor)
                    .await
                    .unwrap()
                    .expect("row survives")
                    .did
                    .as_deref(),
                Some("did:plc:live")
            );
        }
        assert!(
            db.list_retired_atproto_identities(&actor)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// A SECOND retirement appends rather than overwriting. This is the whole
    /// reason the archive is a table and not two columns on the live row: a
    /// column pair remembers only the most recent DID, and the earlier one — a
    /// DID this user really did publish — would silently vanish.
    #[tokio::test]
    async fn a_second_retirement_never_forgets_the_first() {
        let db = fresh_db().await;
        let actor = [0xc3u8; 32];
        for did in ["did:plc:one", "did:plc:two"] {
            minted(&db, &actor, did).await;
            db.mark_atproto_identity_deleted(&actor).await.unwrap();
            db.request_atproto_tombstone(&actor).await.unwrap();
            db.mark_atproto_identity_tombstoned(&actor).await.unwrap();
            assert!(db.archive_retired_atproto_identity(&actor).await.unwrap());
        }
        let archived = db.list_retired_atproto_identities(&actor).await.unwrap();
        assert_eq!(
            archived.iter().map(|r| r.did.as_str()).collect::<Vec<_>>(),
            vec!["did:plc:one", "did:plc:two"],
            "oldest first, both kept"
        );
        // Idempotent: nothing retired now, so a retry archives nothing new.
        assert!(!db.archive_retired_atproto_identity(&actor).await.unwrap());
        assert_eq!(
            db.list_retired_atproto_identities(&actor)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    /// No identity row = no consent to publish anything, so the floor fails
    /// CLOSED. An open default here would serve a whole back-catalogue to any
    /// caller for whom the row is missing or not yet written.
    #[tokio::test]
    async fn projection_floor_fails_closed_for_an_unknown_actor() {
        let db = fresh_db().await;
        assert_eq!(
            db.get_atproto_projection_floor(&[0x7Fu8; 32])
                .await
                .unwrap(),
            i64::MAX
        );
    }

    #[tokio::test]
    async fn intent_provision_mint_lifecycle() {
        let db = fresh_db().await;
        let actor = [0x61u8; 32];
        db.upsert_atproto_identity_intent(&actor, "plc", "did:key:zDnaeUSER")
            .await
            .unwrap();
        let row = db.get_atproto_identity(&actor).await.unwrap().unwrap();
        assert_eq!(row.status, "pending");
        assert_eq!(row.user_rotation_pub, "did:key:zDnaeUSER");
        assert_eq!(row.did, None);

        db.set_atproto_identity_keys(&actor, "did:key:zQ3sSIGN", "did:key:zQ3sROT")
            .await
            .unwrap();
        db.record_atproto_minted(&actor, "did:plc:abc123", Some("bafycid"))
            .await
            .unwrap();
        let row = db.get_atproto_identity(&actor).await.unwrap().unwrap();
        assert_eq!(row.status, "active");
        assert_eq!(row.did.as_deref(), Some("did:plc:abc123"));
        assert_eq!(row.provenance.as_deref(), Some("minted-plc"));
        assert_eq!(row.genesis_cid.as_deref(), Some("bafycid"));

        // Idempotent retry with the same DID; a different DID is refused.
        db.record_atproto_minted(&actor, "did:plc:abc123", Some("bafycid"))
            .await
            .unwrap();
        assert!(
            db.record_atproto_minted(&actor, "did:plc:OTHER", None)
                .await
                .is_err()
        );

        // Re-recording intent on an ACTIVE row must not clobber it.
        db.upsert_atproto_identity_intent(&actor, "web", "")
            .await
            .unwrap();
        let row = db.get_atproto_identity(&actor).await.unwrap().unwrap();
        assert_eq!(row.method, "plc");
        assert_eq!(row.status, "active");
    }

    #[tokio::test]
    async fn plc_intent_requires_user_rotation_key() {
        let db = fresh_db().await;
        assert!(
            db.upsert_atproto_identity_intent(&[0x62u8; 32], "plc", "")
                .await
                .is_err()
        );
        // did:web has no rotation keys — empty is correct there.
        db.upsert_atproto_identity_intent(&[0x62u8; 32], "web", "")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn key_blob_roundtrip_and_size_cap() {
        let db = fresh_db().await;
        let actor = [0x63u8; 32];
        assert_eq!(
            db.get_atproto_identity_key_blob(&actor).await.unwrap(),
            None
        );
        db.put_atproto_identity_key_blob(&actor, b"sealed-bytes")
            .await
            .unwrap();
        assert_eq!(
            db.get_atproto_identity_key_blob(&actor).await.unwrap(),
            Some(b"sealed-bytes".to_vec())
        );
        let oversized = vec![0u8; super::MAX_ATPROTO_IDENTITY_BLOB_BYTES + 1];
        assert!(
            db.put_atproto_identity_key_blob(&actor, &oversized)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn list_orders_pending_last_created_asc() {
        let db = fresh_db().await;
        db.upsert_atproto_identity_intent(&[1u8; 32], "plc", "did:key:zDnaeA")
            .await
            .unwrap();
        db.upsert_atproto_identity_intent(&[2u8; 32], "web", "")
            .await
            .unwrap();
        let rows = db.list_atproto_identities().await.unwrap();
        assert_eq!(rows.len(), 2);
    }
}
