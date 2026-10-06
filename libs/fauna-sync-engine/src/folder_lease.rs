//! The exclusive-editing lease's **client half** — `file-sync.md` § Exclusive
//! editing.
//!
//! The nest has admitted one writing device at a time since the lease kinds
//! landed (`fauna.folders.lease.{acquire,release}`), but nothing on any client
//! ever asked for a lease and nothing ever went read-only while one was held,
//! so the promise *"while a lease is held, other devices treat the folder as
//! read-only"* was target state only. This module is
//! the seat's side of it: the posture the folder-list read installs, the window
//! an upload pass writes inside, and the pure rule that decides whether this
//! seat may write the folder at all.
//!
//! # The two reads, and why they are different reads
//!
//! **Rendering** reads [`LeasePosture::holder`] — the projection's
//! `FolderSummary::lease`, refreshed on the same tick as every other folder-row
//! posture. **Writing** takes [`LeaseWindow`], whose acquire is the atomic
//! arbiter. They can disagree for one tick and that is not a defect: between
//! reading the projection and acquiring, another seat may win, and only the
//! acquire settles it (`fauna_protocol::folders::FolderLeaseState`'s own doc
//! says the same). Never invert them — `lease.acquire` **takes** a free lease as
//! a side effect of asking, so it can never be used as a probe, and it is gated
//! on the writable-folder resolver, so a reader member (the seat that most needs
//! to know the folder is locked) cannot ask at all.
//!
//! # Fail-open, and never at the cost of a local edit
//!
//! Every degrade here resolves toward *keep working*: an unreadable flag reads
//! as un-governed (the opposite direction from content residency, deliberately —
//! `file-sync.md` § Exclusive editing), and a lease this seat could not get
//! **defers** the upload rather than refusing it. A deferred upload leaves the
//! entry `LocallyModified` on disk, which is exactly where the offline arm
//! leaves it, and the next converge pass re-drives it. No path in this module
//! may drop, overwrite or discard a local edit: the iron rule that a tracked
//! file's local modification MUST be uploaded is deferred by a lease, never
//! breached by one.

use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

/// A folder lease's TTL as the nest grants it, in seconds
/// (`bins/fauna-nest/src/folder_handlers.rs` — `try_acquire_upload_lease(.., 300)`).
///
/// Mirrored rather than shared because it is the *nest's* number: this seat
/// only needs to renew comfortably inside whatever the nest granted, and
/// reading it too large is the one direction that hurts (a renewal that fires
/// after the lease already lapsed lets another device take over mid-pass).
pub const LEASE_TTL_SECS: i64 = 300;

/// How long a hold is used before this seat renews it — half the TTL.
///
/// A renewal is just another `lease.acquire` from the same device (the nest's
/// `WHERE NOT EXISTS (… device_id != ?2)` makes the same device's re-acquire an
/// extension, costing one kind and no new state), so the only question is
/// cadence. Half the TTL leaves a full half-TTL of slack for a slow or retried
/// RPC before the lease could lapse under a long-running pass.
pub const LEASE_RENEW_AFTER_SECS: i64 = LEASE_TTL_SECS / 2;

/// Who holds a folder's exclusive-edit lease right now, as the folder-list
/// projection reported it — the engine's reduced reading of
/// [`fauna_protocol::folders::FolderLeaseState`].
///
/// Reduced on purpose: the engine needs the holder and the expiry and nothing
/// else, and dropping the wire type's open `extra` map keeps this comparable
/// (`Eq`) so a posture install can say whether anything actually changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseHolder {
    /// Hex-encoded 32-byte device id of the holding device.
    ///
    /// A surface that shows this to a user renders the device's **label** from
    /// its own devices projection, never this hex — a 64-character string is
    /// not an answer to *"why can't I edit this?"*.
    pub device_id: String,
    /// Unix epoch seconds at which the lease lapses if its holder does not
    /// renew.
    pub expires_at: i64,
}

impl LeaseHolder {
    /// Whether this reading still binds at `now` (Unix seconds).
    ///
    /// The nest sweeps expired rows **lazily** — on the next acquire for that
    /// folder — so a lapsed row can outlive its expiry in the projection, and
    /// every reader must apply the expiry itself. Treating a stale row as held
    /// would freeze a folder nobody is writing.
    pub fn is_live_at(&self, now: i64) -> bool {
        self.expires_at > now
    }

    /// Whether `device_id_hex` is this holder.
    pub fn is_device(&self, device_id_hex: &str) -> bool {
        self.device_id == device_id_hex
    }
}

impl From<&fauna_protocol::folders::FolderLeaseState> for LeaseHolder {
    fn from(w: &fauna_protocol::folders::FolderLeaseState) -> Self {
        Self {
            device_id: w.device_id.clone(),
            expires_at: w.expires_at,
        }
    }
}

/// What an upload pass learned when it asked the folder for a write window.
///
/// Three of the four arms let the pass proceed or stop for one clear reason
/// each; the split between [`Self::Refused`] and [`Self::Unavailable`] is the
/// one that matters, because only the first is a *lock* and only the first may
/// render the folder read-only. An offline seat is not read-only — it records
/// locally exactly as on any other folder and uploads on the reconnect that can
/// acquire (`file-sync.md` § Exclusive editing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseWindow {
    /// The folder is not under exclusive editing — upload as always, with no
    /// nest round-trip. This is the arm every ordinary sync folder takes, and
    /// the reason the lease costs un-governed folders exactly nothing.
    NotGoverned,
    /// This device holds the folder for this pass.
    Held,
    /// Another device holds it. The pass uploads nothing; every local edit
    /// stays pending for a later pass, and the folder renders read-only.
    ///
    /// The holder is deliberately **not** carried here. The acquire refusal is
    /// a typed, payload-less `fauna.folders.conflict` whose detail string is
    /// free-form prose for a log — no client may parse it for the holder's
    /// identity. A surface that needs to name the holder reads
    /// [`LeasePosture::holder`], which owns that fact on the wire.
    Refused,
    /// The acquire could not be made at all — offline, a control plane that is
    /// not connected, or any non-`conflict` RPC failure.
    ///
    /// Defers the pass exactly as [`Self::Refused`] does, and for the same
    /// data-safety reason, but says **nothing** about who may write: an offline
    /// seat is not a locked-out one, and rendering it read-only would tell the
    /// user their own folder is someone else's when the truth is only that the
    /// nest is unreachable.
    Unavailable,
}

impl LeaseWindow {
    /// Whether the pass that opened this window may upload.
    pub fn may_write(&self) -> bool {
        matches!(self, Self::NotGoverned | Self::Held)
    }

    /// Whether closing this window owes the nest a release.
    pub fn holds_lease(&self) -> bool {
        matches!(self, Self::Held)
    }
}

/// This seat's live exclusive-editing state for one folder: what the projection
/// last said, what this device currently holds, and whether it was last refused.
///
/// One cell per engine, installed by the same folder-list read that installs the
/// mode, the audience, the residency, the accepts gate, the website toggle and
/// the selective-sync lists, so no two postures are ever composed from different
/// moments.
#[derive(Debug, Default)]
pub struct LeasePosture {
    /// Whether the folder's owner turned exclusive editing on.
    ///
    /// **Fails OPEN to un-governed** (`false`): a flag that did not parse, or a
    /// row that is not there, must never freeze a user's own folder against
    /// their own writes.
    governed: AtomicBool,
    /// The projection's last reading of who holds the folder — the ONE
    /// sanctioned way a seat learns a folder is locked, and the only place the
    /// holder's identity lives.
    holder: RwLock<Option<LeaseHolder>>,
    /// Unix seconds at which this seat was last refused the lease, or `0`.
    ///
    /// The *fresh* half of the read-only reading. A refusal is newer than any
    /// projection this seat holds (the projection refreshes on the rescan tick;
    /// the refusal happened just now), so a seat that has been told "no" renders
    /// read-only immediately instead of waiting a cadence to find out. It
    /// decays on the lease's own TTL and is cleared by the next successful
    /// acquire, so it can never wedge a folder shut.
    refused_at: AtomicI64,
    /// This device's own hold, `None` when it holds nothing.
    ///
    /// An **async** mutex, and it must be: the acquire and the release are RPCs
    /// held across an `await`, and the whole point of the lock is that two
    /// concurrently-polled upload futures in one pass cannot each decide the
    /// hold is due for renewal and fire their own acquire. A `std` mutex would
    /// either be held across that await (a deadlock waiting to happen on a
    /// current-thread runtime) or dropped around it, which is the same race
    /// with extra steps.
    hold: tokio::sync::Mutex<Option<LeaseHold>>,
    /// Unix seconds until which this device's own hold is in force, or `0`.
    ///
    /// A lock-free mirror of [`Self::hold`], written under that mutex and read
    /// without it. It exists because the per-file write guard must answer *"do
    /// we hold this folder"* while a renewal is in flight — and a `try_lock`
    /// that loses to the renewal would answer "no" and defer a file the pass
    /// has every right to upload. Racing the mirror is harmless in a way racing
    /// the mutex is not: it is only ever set by an acquire that succeeded and
    /// cleared by a release or a refusal, so it errs toward the hold this
    /// device actually has.
    hold_expires_at: AtomicI64,
}

impl LeasePosture {
    /// Whether the folder is under exclusive editing.
    pub fn is_governed(&self) -> bool {
        self.governed.load(Ordering::Relaxed)
    }

    /// The projection's last reading of the folder's holder, expiry unfiltered
    /// (`LeaseHolder::is_live_at` applies it).
    pub fn holder(&self) -> Option<LeaseHolder> {
        self.holder.read().unwrap().clone()
    }

    /// Install the governance flag from an authoritative folder-list read.
    ///
    /// `None` — the list itself was unreadable — keeps the armed posture, the
    /// failure discipline every posture on this read shares
    /// ([`crate::engine::install_authoritative_posture`], whose logging shape
    /// this mirrors).
    pub fn install_governed(&self, new: Option<bool>) {
        crate::engine::install_authoritative_posture(
            &self.governed,
            new,
            "folder exclusive editing",
        )
    }

    /// Install the holder reading from an authoritative folder-list read.
    ///
    /// `Some(reading)` installs it, **including `Some(None)`** — "the projection
    /// positively says nobody holds this folder" is a real answer and the one
    /// that un-freezes a seat after the holder releases. `None` (the list was
    /// unreadable) keeps the armed reading, which is safe precisely because a
    /// held lease carries its own expiry: a stale holder stops binding within
    /// one TTL whether or not this seat ever reaches the nest again.
    pub fn install_holder(&self, new: Option<Option<LeaseHolder>>) {
        let Some(new) = new else { return };
        let mut guard = self.holder.write().unwrap();
        if *guard != new {
            tracing::info!(
                was = ?guard.as_ref().map(|h| &h.device_id),
                now = ?new.as_ref().map(|h| &h.device_id),
                "folder exclusive-edit lease holder (re)resolved from the nest rows"
            );
            *guard = new;
        }
    }

    /// Install both halves of one authoritative folder-list read, and reconcile
    /// the refusal against them.
    ///
    /// The two halves install **together** because they were read together: a
    /// seat that took "governed" from one list and "unheld" from a later one
    /// could write straight through a lease taken in between.
    ///
    /// The third act is the one that is easy to leave out, and it was: **an
    /// authoritative holder reading supersedes the refusal it was bridging.**
    /// [`Self::refused_at`] exists only to cover the gap between *"the nest just
    /// told me no"* and *"my projection has caught up"*; once a read has caught
    /// up, the read is the better answer. Keeping the refusal as well would
    /// leave a seat that was refused once rendering read-only for a whole TTL
    /// after the holder released — a folder frozen against its own user by a
    /// stale *no*, which is exactly the failure the flag's fail-open direction
    /// exists to prevent. Caught by
    /// `bins/fauna-nest/tests/conformance_folder_lease_two_seats.rs`'s takeover
    /// leg, which is the only place the release and the next read meet.
    ///
    /// A reading that still names another live device clears nothing — there is
    /// no disagreement to resolve, and the read-only state stands on the
    /// reading's own strength.
    pub fn install_from_read(
        &self,
        governed: Option<bool>,
        lease: Option<Option<LeaseHolder>>,
        device_id_hex: &str,
        now: i64,
    ) {
        self.install_governed(governed);
        // Decide before the move: whether this read says the folder is held by
        // somebody else right now.
        let held_elsewhere = lease.as_ref().map(|reading| {
            reading
                .as_ref()
                .is_some_and(|h| h.is_live_at(now) && !h.is_device(device_id_hex))
        });
        self.install_holder(lease);
        if held_elsewhere == Some(false) {
            self.clear_refused();
        }
    }

    /// Record that this seat was just refused the lease.
    pub fn mark_refused(&self, now: i64) {
        self.refused_at.store(now, Ordering::Relaxed);
    }

    /// Clear the refusal — this seat just acquired the lease.
    pub fn clear_refused(&self) {
        self.refused_at.store(0, Ordering::Relaxed);
    }

    /// Lock this device's own hold for the duration of an acquire, a renewal
    /// or a release.
    ///
    /// The caller holds the guard **across the RPC** — that serialization is
    /// the lock's whole job (see the field's doc). Returning the guard rather
    /// than wrapping each operation keeps the RPC in the engine, which is where
    /// the nest client and the folder name live, without this module growing a
    /// second copy of either.
    pub async fn lock_hold(&self) -> tokio::sync::MutexGuard<'_, Option<LeaseHold>> {
        self.hold.lock().await
    }

    /// Record that this device now holds the lease, acquired at `now` — called
    /// under the [`Self::lock_hold`] guard, which is what keeps the mirror and
    /// the hold itself from disagreeing.
    pub fn mark_held(&self, now: i64) {
        self.hold_expires_at
            .store(now + LEASE_TTL_SECS, Ordering::Relaxed);
    }

    /// Record that this device holds nothing — a release, a refusal, or an
    /// acquire that could not be made. Called under the same guard.
    pub fn mark_unheld(&self) {
        self.hold_expires_at.store(0, Ordering::Relaxed);
    }

    /// Whether this device's own hold is still in force at `now` — the
    /// lock-free read the per-file write guard asks (the `hold_expires_at`
    /// mirror's own doc says why it is not a `try_lock` on the hold).
    pub fn holds_lease_now(&self, now: i64) -> bool {
        self.hold_expires_at.load(Ordering::Relaxed) > now
    }

    /// Whether a refusal is still fresh at `now`.
    fn refused_recently(&self, now: i64) -> bool {
        let at = self.refused_at.load(Ordering::Relaxed);
        at != 0 && now - at < LEASE_TTL_SECS
    }

    /// Whether this seat must treat the folder as **read-only** right now —
    /// the sentence § Folders actually promises.
    ///
    /// See [`read_only_for_lease`] for the rule; this is it applied to the live
    /// cell.
    pub fn is_read_only(&self, device_id_hex: &str, now: i64) -> bool {
        read_only_for_lease(
            self.is_governed(),
            self.holder().as_ref(),
            self.refused_recently(now),
            device_id_hex,
            now,
        )
    }
}

/// The pure read-only rule, split out of the cell so it is testable without an
/// engine, a nest or a clock (the `accepts_from_seat` lesson — a rule that can
/// only be exercised through I/O is a rule nobody exercises).
///
/// A seat is read-only for the lease when the folder is governed **and** either
/// the projection names a live holder that is not this device, or this seat was
/// itself refused within the lease's TTL. Both halves are needed: the projection
/// alone lags by up to a rescan cadence, and the refusal alone cannot name the
/// holder. Neither half may fire on an un-governed folder — which is what makes
/// this cost the ordinary case nothing.
pub fn read_only_for_lease(
    governed: bool,
    holder: Option<&LeaseHolder>,
    refused_recently: bool,
    device_id_hex: &str,
    now: i64,
) -> bool {
    if !governed {
        return false;
    }
    let held_elsewhere = holder.is_some_and(|h| h.is_live_at(now) && !h.is_device(device_id_hex));
    held_elsewhere || refused_recently
}

/// Whether an RPC error is the nest's typed *"another device holds this
/// folder's lease"* refusal.
///
/// The code is the whole contract. The detail string beside it is free-form
/// prose for a log and carries **no** payload — a client that parsed it for the
/// holder's device id would be inventing a second, disagreeing owner for a fact
/// the projection already owns.
pub fn is_lease_conflict<E: fauna_protocol::requester::RpcErrorClass>(e: &E) -> bool {
    e.as_rpc_error()
        .is_some_and(|rpc| rpc.code == LEASE_CONFLICT_CODE)
}

/// The typed refusal `fauna.folders.lease.acquire` answers a second device with
/// (`bins/fauna-nest/src/folder_handlers.rs::lease_acquire_handler`).
pub const LEASE_CONFLICT_CODE: &str = "fauna.folders.conflict";

/// Unix seconds now — the one clock this module reads, so a test can reason
/// about the rule with [`read_only_for_lease`] instead of racing it.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// This device's own live hold on a folder, and when it was last (re)acquired.
#[derive(Debug, Clone, Copy)]
pub struct LeaseHold {
    /// Unix seconds of the acquire (or the most recent renewal).
    pub acquired_at: i64,
}

impl LeaseHold {
    /// Whether this hold is fresh enough to write under without renewing.
    pub fn has_headroom_at(&self, now: i64) -> bool {
        now - self.acquired_at < LEASE_RENEW_AFTER_SECS
    }

    /// Whether the nest's grant for this hold can still be in force at `now`.
    ///
    /// Distinct from [`Self::has_headroom_at`] on purpose: *headroom* is the
    /// renewal cadence (half the TTL, with slack for a slow RPC), while this is
    /// the hard edge past which the nest would have let another device take
    /// over. The per-file write guard asks this one, so a pass whose renewal
    /// silently failed stops writing at the real boundary rather than at the
    /// conservative one.
    pub fn is_live_at(&self, now: i64) -> bool {
        now - self.acquired_at < LEASE_TTL_SECS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(device: &str, expires_at: i64) -> LeaseHolder {
        LeaseHolder {
            device_id: device.to_string(),
            expires_at,
        }
    }

    #[test]
    fn an_ungoverned_folder_is_never_read_only() {
        // The whole point of the per-folder opt-in: a folder nobody put under
        // exclusive editing pays nothing and is never frozen, whatever a stale
        // projection or an old refusal says.
        assert!(!read_only_for_lease(
            false,
            Some(&holder("beef", 9_999)),
            true,
            "cafe",
            100
        ));
    }

    #[test]
    fn a_live_holder_elsewhere_makes_this_seat_read_only() {
        assert!(read_only_for_lease(
            true,
            Some(&holder("beef", 9_999)),
            false,
            "cafe",
            100
        ));
    }

    #[test]
    fn our_own_live_hold_does_not_make_us_read_only() {
        // The holder IS this device — it may write, which is the entire
        // purpose of having taken the lease.
        assert!(!read_only_for_lease(
            true,
            Some(&holder("cafe", 9_999)),
            false,
            "cafe",
            100
        ));
    }

    #[test]
    fn an_expired_holder_does_not_bind() {
        // The nest sweeps expired lease rows lazily, so the projection can carry
        // a lapsed row indefinitely. A reader that did not apply the expiry
        // itself would freeze a folder nobody is writing.
        assert!(!read_only_for_lease(
            true,
            Some(&holder("beef", 99)),
            false,
            "cafe",
            100
        ));
    }

    #[test]
    fn a_fresh_refusal_is_read_only_before_the_projection_catches_up() {
        // The refusal is newer than any projection this seat holds: it happened
        // just now, the projection refreshes on the rescan tick. Without this
        // half a refused seat would render writable for a whole cadence.
        assert!(read_only_for_lease(true, None, true, "cafe", 100));
    }

    #[test]
    fn a_refusal_decays_on_the_lease_ttl() {
        let posture = LeasePosture::default();
        posture.install_governed(Some(true));
        posture.mark_refused(100);
        assert!(posture.is_read_only("cafe", 100 + LEASE_TTL_SECS - 1));
        // Past the TTL the lease it refused us over has lapsed by its own terms,
        // so the refusal may not keep the folder shut a moment longer.
        assert!(!posture.is_read_only("cafe", 100 + LEASE_TTL_SECS));
    }

    #[test]
    fn an_authoritative_unheld_reading_supersedes_a_refusal() {
        // The takeover leg, in miniature: this seat was refused while another
        // device held the folder, the holder then released, and the next
        // folder-list read says so. Without this the refusal would keep the
        // folder read-only for a whole TTL after it was free — a folder frozen
        // against its own user by a stale *no*. Caught in the two-seat
        // conformance test before it was caught here.
        let posture = LeasePosture::default();
        posture.install_from_read(Some(true), Some(Some(holder("beef", 9_999))), "cafe", 100);
        posture.mark_refused(100);
        assert!(posture.is_read_only("cafe", 101));

        posture.install_from_read(Some(true), Some(None), "cafe", 101);
        assert!(
            !posture.is_read_only("cafe", 101),
            "a read that says nobody holds the folder must retire the refusal it bridged"
        );
    }

    #[test]
    fn a_reading_that_still_names_another_holder_keeps_the_refusal() {
        // No disagreement to resolve — the read-only state stands on the
        // reading's own strength, and clearing here would be a no-op that
        // hides a real hold if the reading later lapses.
        let posture = LeasePosture::default();
        posture.install_from_read(Some(true), Some(Some(holder("beef", 9_999))), "cafe", 100);
        posture.mark_refused(100);
        posture.install_from_read(Some(true), Some(Some(holder("beef", 9_999))), "cafe", 101);
        assert!(posture.is_read_only("cafe", 101));
    }

    #[test]
    fn an_unreadable_read_leaves_a_refusal_standing() {
        // `None` is "the list could not be read", which supersedes nothing.
        // Clearing on it would let an OFFLINE tick un-freeze a seat that the
        // nest had genuinely refused.
        let posture = LeasePosture::default();
        posture.install_from_read(Some(true), Some(None), "cafe", 100);
        posture.mark_refused(100);
        posture.install_from_read(None, None, "cafe", 101);
        assert!(posture.is_read_only("cafe", 101));
    }

    #[test]
    fn a_reading_naming_this_device_retires_the_refusal_too() {
        // This seat won the folder on a later pass; a refusal from an earlier
        // one must not outlive its own acquire.
        let posture = LeasePosture::default();
        posture.install_from_read(Some(true), Some(Some(holder("beef", 9_999))), "cafe", 100);
        posture.mark_refused(100);
        posture.install_from_read(Some(true), Some(Some(holder("cafe", 9_999))), "cafe", 101);
        assert!(!posture.is_read_only("cafe", 101));
    }

    #[test]
    fn a_successful_acquire_clears_the_refusal() {
        let posture = LeasePosture::default();
        posture.install_governed(Some(true));
        posture.mark_refused(100);
        assert!(posture.is_read_only("cafe", 101));
        posture.clear_refused();
        assert!(!posture.is_read_only("cafe", 101));
    }

    #[test]
    fn an_unreadable_list_keeps_the_armed_posture() {
        // `None` on either install is "the list was unreadable", never "off" /
        // "unheld" — the same discipline the mode, the audience, the residency
        // and the selective-sync lists all follow.
        let posture = LeasePosture::default();
        posture.install_governed(Some(true));
        posture.install_holder(Some(Some(holder("beef", 9_999))));
        posture.install_governed(None);
        posture.install_holder(None);
        assert!(posture.is_governed());
        assert_eq!(posture.holder().unwrap().device_id, "beef");
    }

    #[test]
    fn a_positively_unheld_reading_installs_and_unfreezes_the_seat() {
        // `Some(None)` is a real answer — the holder released — and it is the
        // one that lets a refused seat write again.
        let posture = LeasePosture::default();
        posture.install_governed(Some(true));
        posture.install_holder(Some(Some(holder("beef", 9_999))));
        assert!(posture.is_read_only("cafe", 100));
        posture.install_holder(Some(None));
        assert!(!posture.is_read_only("cafe", 100));
    }

    #[test]
    fn a_hold_renews_at_half_the_ttl() {
        let hold = LeaseHold { acquired_at: 1_000 };
        assert!(hold.has_headroom_at(1_000 + LEASE_RENEW_AFTER_SECS - 1));
        assert!(!hold.has_headroom_at(1_000 + LEASE_RENEW_AFTER_SECS));
    }

    #[test]
    fn the_window_arms_say_who_may_write_and_who_owes_a_release() {
        assert!(LeaseWindow::NotGoverned.may_write());
        assert!(!LeaseWindow::NotGoverned.holds_lease());
        assert!(LeaseWindow::Held.may_write());
        assert!(LeaseWindow::Held.holds_lease());
        // Both deferral arms stop the pass; neither owes a release, because
        // neither ever took the lease.
        assert!(!LeaseWindow::Refused.may_write());
        assert!(!LeaseWindow::Refused.holds_lease());
        assert!(!LeaseWindow::Unavailable.may_write());
        assert!(!LeaseWindow::Unavailable.holds_lease());
    }

    #[test]
    fn a_wire_lease_state_reduces_to_the_engines_holder() {
        let wire = fauna_protocol::folders::FolderLeaseState {
            device_id: "beef".to_string(),
            expires_at: 4_242,
            extra: Default::default(),
        };
        assert_eq!(LeaseHolder::from(&wire), holder("beef", 4_242));
    }
}
