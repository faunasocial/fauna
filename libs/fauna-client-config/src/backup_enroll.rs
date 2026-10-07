//! The one shared backup-destination **enroll sequence** — the client leg of
//! the nest-side segment-backup redesign
//! (`docs/goal/behavior/backup-destinations.md` § State & data shape, the 2026-07-23 ratified
//! paragraph; `docs/goal/architecture/key-material-hierarchy.md`
//! § Path A-sibling-0).
//!
//! Before this module the sequence was written out three times — linux
//! (`views/backups/destinations.rs::add_destination`), the native FFI
//! (`fauna-ffi/src/backup_destinations.rs::backup_destination_add`) and wasm
//! (`fauna-wasm/src/rpc.rs::backup_destination_add`) — each with its own copy of
//! the `"__mail"` / `"backup"` constants under two different naming schemes.
//! Three copies is where drift starts, and the redesign grew the sequence from
//! one nest mutation to four, so it is lifted here once (priorities #2 and #4).
//!
//! **What stays per-platform, legitimately:** resolving the candidate
//! destination's identity and opening an authenticated connection to it
//! (native builds a `fauna_client::NestClient`, wasm opens a browser
//! WebSocket) and minting the opaque `destination_id` (native uuid-v4, wasm 16
//! random bytes hex — `getrandom`/`uuid` are not dependencies of this crate,
//! and taking the id as an argument keeps the sequence deterministic under
//! test). The resolved identity is handed in via [`ResolvedDestination`]; the
//! open connection as the `destination: D` parameter (see [`RpcRequester`]).
//!
//! # Ordering and crash-safety
//!
//! [`enroll_backup_destination`] performs **four** nest mutations, and the
//! order is load-bearing (`docs/goal/architecture/nest/common.md`
//! § Client-state recoverability):
//!
//! 1. `fauna.backup.nest_key.grant` (source) — hand the source nest this
//!    owner's [`NestBackupKey`] so its in-process coordinator can seal this
//!    owner's segments. Idempotent (a re-grant replaces) and **inert on its
//!    own**: a stored key with zero configured destinations backs nothing up.
//! 2. *(no call)* the `writer_nest_id` step 3 authorizes is the caller's
//!    `source_nest_id` — the source nest's identity **as its connection
//!    proved it** (the platform's bound nest id: the login's pin for the
//!    origin, else a possession proof, refused when the nest's own claim
//!    disagrees). Never the source's `fauna.nest.info` answer: the
//!    destination's writer gate keys on the handshake-verified id, so a grant
//!    registered under a claim could authorize whichever nest the claim names.
//! 3. `fauna.backup.writer_grant.register` (**destination**) — authorize the
//!    source nest to write this owner's segment-backup custody there.
//!    Idempotent (re-registering the same writer refreshes rather than
//!    duplicating) and inert alone: an authorized-but-unconfigured writer
//!    backs nothing up. Spoken to the destination over its own authenticated
//!    connection, never the federation channel
//!    (`message-segment-store.md` § Cross-location backup protocol), so
//!    revocation stays operable with the source nest fully hostile.
//! 4. `fauna.backup.destination.register` (source) — tell the source nest
//!    *where* to back this owner up. The nest cannot read the owner's
//!    destination list (a client-sealed account-plane row), so without
//!    this call the in-process coordinator has no source of truth for this
//!    owner's destinations. Idempotent on `destination_id`.
//! 5. The `fauna.state.backup` write (via [`crate::mutate_backup`]) — record
//!    the destination row in the bound box's list. This remains **the single
//!    atomic decision point**.
//!
//! Steps 1–4 are each individually idempotent and inert without step 5, so
//! their relative order is free (`behavior/backup-destinations.md` § *Source-side destination
//! registration*) — the one hard constraint is that **all four precede the
//! atomic config write**. A crash at any point before step 5 leaves the nest
//! side holding inert grants/registrations and no destination — self-healing,
//! since the user's next add re-issues all four. The reverse order (config
//! write first) would leave a *recorded destination the coordinator cannot
//! act on* — a live half-state that looks configured and does nothing. No
//! destination-side state beyond the writer grant is mutated, so a crash at
//! any point leaves nothing to clean up off-box.
//!
//! [`deregister_backup_destination`] is destination-add's remove-side twin:
//! it deregisters from the source nest's registry (`fauna.backup.destination.
//! remove`, also idempotent and preceding the config write for the same
//! reason), then drops the list row. It does not revoke the destination-side
//! writer grant — that is a distinct trust-facet action, not implied by
//! removing a destination from this owner's list.
//!
//! # The Backups-page status read, and why the heal lives here
//!
//! [`read_backup_status`] is the **one status read all 7 apps call**
//! (`behavior/backup-destinations.md` § Per-destination status read), replacing the pre-flip
//! per-app source-side `destination_status()` computation (deleted with the
//! client upload coordinator)
//! and web's degenerate wasm twin. It sits in this module rather than a read-only
//! one because it is not a pure read: a destination whose list row survives
//! without its grant and registry row — after an identity-succession rebuild,
//! or a crash between deregistration's two writes — reads as unenrolled, so the
//! nest reports `enrolled: false` with no rows and a naively-repointed page
//! would render blank for a backup that is running. The read therefore heals
//! that state in passing, via
//! [`reconcile_backup_enrollment`] — steps (1) and (4), the two the projection
//! depends on, both already idempotent. Folding the trigger in here keeps it out
//! of all 7 apps (priority #2). The destination-side writer grant (step 3) is
//! deliberately **not** healed — see [`reconcile_backup_enrollment`] for what
//! that costs and why it belongs with the slice-5 flip.

use fauna_client_backup::BackupClient;
use fauna_core::crypto::NestBackupKey;
use fauna_core::data::{
    BackupDestination, DESTINATION_KIND_CLIENT_DEVICE, DESTINATION_KIND_NEST, Timestamp,
};
use fauna_protocol::{RpcErrorClass, RpcRequester};
use serde::Serialize;

use crate::backup_store::{BackupWriteError, load_backup_state_refiled, mutate_backup};
use crate::mutate::{
    FolderCoverageLabel, add_backup_destination, attach_backup_destination_folder,
    detach_backup_destination_folder, keep_backup_destination, remove_backup_destination,
};
use crate::store_seam::{BackupStateStore, StoreError};

/// The reserved folder a v1 destination backs up. One row is recorded per
/// reserved set sharing a `destination_id`; v1 records exactly `__mail`
/// (`docs/goal/behavior/backup-destinations.md` § State & data shape).
pub const DEFAULT_BACKUP_FOLDER: &str = "__mail";

/// The platform-resolved identity of a candidate destination, plus the opaque
/// id the caller minted for it. Produced by the per-platform resolve step
/// (native `segment_backup::resolve_destination`, wasm
/// `resolve_destination_inner`) — see the module docs for why that step is not
/// shared.
#[derive(Debug, Clone)]
pub struct ResolvedDestination {
    /// Caller-minted opaque row id (native uuid-v4, wasm 16-byte hex).
    pub destination_id: String,
    /// The URL the user typed, recorded verbatim.
    pub destination_nest_url: String,
    /// The destination nest's stable 32-byte identity, from `fauna.nest.info`.
    /// A URL edit that resolves to a different pubkey is remove + re-add, not
    /// an edit.
    pub destination_actor_pubkey: [u8; 32],
    /// The destination's handle domain, used as the display-name default.
    pub domain: String,
    /// The friendly name the user typed. Blank (or whitespace) falls back to
    /// [`Self::domain`].
    pub requested_name: String,
}

/// Failure from [`enroll_backup_destination`]. `E` is the **source** nest
/// transport's error type, `DE` the **destination**'s — kept typed rather than
/// stringly; `Display` is what the per-app glue renders.
#[derive(Debug)]
pub enum EnrollError<E, DE> {
    /// The `fauna.backup.nest_key.grant` call (source) failed. Nothing was
    /// recorded — the config write had not happened yet.
    Grant(E),
    /// The `fauna.backup.writer_grant.register` call (destination) failed.
    /// Nothing was recorded — the source nest's grant from step (1) is inert
    /// without a destination row and is replaced on the next add.
    WriterGrant(DE),
    /// The `fauna.backup.destination.register` call (source) failed.
    DestinationRegister(E),
    /// Recording the destination row failed — the list is full
    /// ([`BackupWriteError::ListFull`], the one localized refusal) or the
    /// account store refused. Every nest-side mutation above may have
    /// succeeded; each is inert without the destination row and is
    /// replaced/refreshed on the next add.
    Record(BackupWriteError),
}

impl<E: core::fmt::Display, DE: core::fmt::Display> core::fmt::Display for EnrollError<E, DE> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Grant(e) => write!(f, "granting the nest backup key to the source nest: {e}"),
            Self::WriterGrant(e) => {
                write!(
                    f,
                    "registering the nest-writer grant at the destination: {e}"
                )
            }
            Self::DestinationRegister(e) => {
                write!(f, "registering the destination with the source nest: {e}")
            }
            Self::Record(e) => write!(f, "{e}"),
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug, DE: core::fmt::Display + core::fmt::Debug>
    std::error::Error for EnrollError<E, DE>
{
}

/// Enroll a resolved backup destination: grant the source nest this owner's
/// [`NestBackupKey`], register the nest-writer grant at the destination,
/// register the destination with the source nest, then record the destination
/// row. Returns the owner's full destination list after the add.
///
/// `nest` is the **source** nest transport (the user's own nest — the one that
/// will run the backup coordinator); `destination` is an already-authenticated
/// transport to the **destination** nest (the caller's resolve step already
/// minted this session to prove reachability/authorization — see the module
/// docs). `owner_secret` is the identity seed both the keypair and the
/// `NestBackupKey` derive from. `source_nest_id` is the source nest's identity
/// as `nest`'s connection **proved** it — the platform's bound nest id
/// (`fauna_client_pair::LinkedNestsMachine::bound_nest_id` and its native /
/// FFI / wasm callers), never the nest's own claim (module docs, step 2) — and
/// the box whose `fauna.state.backup` list the row lands in. `store` is the
/// account store ([`BackupStateStore`]).
///
/// See the module docs for the five-step ordering guarantee.
pub async fn enroll_backup_destination<R: RpcRequester, D: RpcRequester>(
    nest: R,
    destination: D,
    store: &dyn BackupStateStore,
    owner_secret: [u8; 32],
    resolved: ResolvedDestination,
    source_nest_id: [u8; 32],
) -> Result<Vec<BackupDestination>, EnrollError<R::Error, D::Error>>
where
    R::Error: RpcErrorClass,
{
    let display_name = if resolved.requested_name.trim().is_empty() {
        resolved.domain
    } else {
        resolved.requested_name
    };
    let dest = BackupDestination {
        destination_id: resolved.destination_id,
        destination_nest_url: resolved.destination_nest_url,
        destination_actor_pubkey: resolved.destination_actor_pubkey,
        folder_name: DEFAULT_BACKUP_FOLDER.to_string(),
        added_at: Timestamp::now_secs().max(0) as u64,
        display_name: Some(display_name),
        // Stated rather than defaulted: this is the peer-nest enroll path, and
        // naming its kind here is what makes it symmetric with the
        // client-device path (`docs/goal/behavior/backup-destinations.md` § Third destination
        // kind), whose enrollment runs on the custodian device itself.
        kind: DESTINATION_KIND_NEST.to_string(),
        ..Default::default()
    };
    enroll_nest_row(
        nest,
        destination,
        store,
        owner_secret,
        dest,
        source_nest_id,
        SeatStep::Register,
    )
    .await
}

/// How enroll step (3) takes the destination's writer seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeatStep {
    /// A plain registration — an ordinary enroll.
    Register,
    /// The restore ceremony's re-enrollment of the destination it pulled the
    /// corpus back from: read the seat's holder off the destination and, when
    /// it is not the restored nest, take the seat over from it
    /// (`segment-backup-protocol.md` § Cross-location backup protocol →
    /// *The writer seat* → *How the seat moves*, situation (2)).
    SucceedHolder,
}

/// Enroll steps (1)–(5) for one peer-nest row (module docs § Ordering).
async fn enroll_nest_row<R: RpcRequester, D: RpcRequester>(
    nest: R,
    destination: D,
    store: &dyn BackupStateStore,
    owner_secret: [u8; 32],
    dest: BackupDestination,
    source_nest_id: [u8; 32],
    seat: SeatStep,
) -> Result<Vec<BackupDestination>, EnrollError<R::Error, D::Error>>
where
    R::Error: RpcErrorClass,
{
    let source = BackupClient::new(nest);

    // (1) Grant the key to the source nest. Idempotent and inert alone, so it
    //     is safe to precede the atomic decision point — module docs § Ordering.
    source
        .nest_key_grant(NestBackupKey::derive(&owner_secret).to_bytes().to_vec())
        .await
        .map_err(EnrollError::Grant)?;

    // (2) The `writer_nest_id` step (3) authorizes is the id the source's
    //     connection proved — never its own `fauna.nest.info` claim, which
    //     could name a sibling (module docs, step 2).
    // (3) Authorize the source nest to write this owner's custody at the
    //     destination. Idempotent and inert alone (module docs § Ordering).
    let writer = hex::encode(source_nest_id);
    let destination = BackupClient::new(destination);
    let holder = match seat {
        SeatStep::Register => None,
        // The holder comes off the seat itself — never a guess, and never
        // the box the device remembers losing: a seat already carried
        // elsewhere names that box, which is then refused, not overwritten.
        SeatStep::SucceedHolder => destination
            .writer_grant_list()
            .await
            .map_err(EnrollError::WriterGrant)?
            .grants
            .into_iter()
            .map(|g| g.writer_nest_id)
            .find(|holder| !holder.eq_ignore_ascii_case(&writer)),
    };
    match holder {
        Some(holder) => destination.writer_grant_succeed(writer, holder).await,
        None => destination.writer_grant_register(writer).await,
    }
    .map_err(EnrollError::WriterGrant)?;

    // (4) Tell the source nest where to back this owner up — its own
    //     coordinator cannot read the `fauna.state.backup` destination list. Idempotent
    //     on `destination_id`, inert alone.
    source
        .destination_register(
            dest.destination_id.clone(),
            dest.destination_nest_url.clone(),
            hex::encode(dest.destination_actor_pubkey),
        )
        .await
        .map_err(EnrollError::DestinationRegister)?;

    // (5) The single atomic decision point. No client-unrecoverable state is
    //     mutated here, so a crash before this leaves nothing to clean up.
    //
    // A sibling device's `Removed` verdict rides its own mark row, so this
    // list write cannot resurrect a destination the owner removed elsewhere:
    // the read fold prunes it whatever list wins
    // (`a_concurrent_removal_is_not_resurrected_by_an_unrelated_enrollment`).
    // The returned list is the state as it now reads, not the one this
    // device composed.
    let (state, ()) = mutate_backup(store, source_nest_id, |st| add_backup_destination(st, dest))
        .await
        .map_err(EnrollError::Record)?;
    Ok(state.backup.destinations)
}

/// Failure from [`reconcile_backup_enrollment`]. Source-only, so one nest's
/// error type.
#[derive(Debug)]
pub enum ReconcileError<E> {
    /// The `fauna.backup.nest_key.grant` call failed.
    Grant(E),
    /// A `fauna.backup.destination.register` call failed. Earlier
    /// registrations in the same pass stand — each is independently
    /// idempotent, so the next pass simply re-issues them.
    DestinationRegister(E),
}

impl<E: core::fmt::Display> core::fmt::Display for ReconcileError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Grant(e) => write!(f, "re-granting the nest backup key to the source nest: {e}"),
            Self::DestinationRegister(e) => {
                write!(f, "re-registering a destination with the source nest: {e}")
            }
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug> std::error::Error for ReconcileError<E> {}

/// Re-issue the **source-side** half of the enroll sequence for destinations
/// the bound box's list already holds — steps (1) and (4) of
/// [`enroll_backup_destination`], the two calls the nest's status projection
/// depends on.
///
/// # Why this exists
///
/// A destination can hold a `fauna.state.backup` list row with **no**
/// `NestBackupKey` grant and **no** source-side registry row — after an
/// identity-succession rebuild, or a crash between deregistration's two writes
/// (`docs/goal/behavior/backup-destinations.md`). The nest therefore has no
/// coordinator for that owner, so `fauna.backup.status` reports
/// `enrolled: false` with an empty destination list — and a Backups page read
/// from that projection would render blank for a destination that is in fact
/// being backed up by the client-side driver. This heals that unenrolled
/// state in place, with no new user action and no new UI.
///
/// # What it deliberately does not do
///
/// It does **not** register the destination-side nest-writer grant (enroll
/// step 3). That call needs an authenticated connection to each *destination*
/// nest, which is the per-platform resolve step the shared sequence cannot do;
/// and it governs whether the **nest's own uploads** are accepted, not whether
/// the status projection is truthful. Until it runs for a healed destination,
/// the nest's uploads are refused and the projection honestly reports
/// `last_upload_time: None` with a growing `backlog_count` — while the in-app
/// driver (still present until the slice-5 flip) keeps the backup current. The
/// writer-grant half belongs with that flip, when the nest becomes the sole
/// writer.
///
/// # Idempotence
///
/// Both calls are idempotent by construction (module docs § Ordering): the
/// grant replaces, and registration keys on `destination_id`. Running this on
/// an already-enrolled owner is therefore a no-op at the nest, so callers may
/// invoke it unconditionally; the usual trigger is a `status()` reply with
/// `enrolled == false` while config holds ≥1 destination. Returns the number
/// of destinations registered.
pub async fn reconcile_backup_enrollment<R: RpcRequester>(
    nest: R,
    owner_secret: [u8; 32],
    destinations: &[BackupDestination],
) -> Result<usize, ReconcileError<R::Error>> {
    if destinations.is_empty() {
        // Nothing configured ⇒ nothing to heal. Skip the grant too: a key with
        // no destinations backs nothing up, so granting it would only make the
        // projection claim enrollment the owner never asked for.
        return Ok(0);
    }

    let source = BackupClient::new(nest);

    // The grant is for **nest** destinations only. It exists to let a nest seal
    // on the owner's behalf, and a client custodian seals for itself from the
    // seed it already holds (`behavior/backup-destinations.md` § Third destination kind →
    // *Enrollment*: "no `NestBackupKey` grant"). An owner whose only destination
    // is their own device would otherwise be handed a key that backs nothing up,
    // purely as a side effect of walking this path.
    let has_nest_destination = destinations.iter().any(|d| d.kind == DESTINATION_KIND_NEST);
    if has_nest_destination {
        source
            .nest_key_grant(NestBackupKey::derive(&owner_secret).to_bytes().to_vec())
            .await
            .map_err(ReconcileError::Grant)?;
    }

    for dest in destinations {
        // ⚠ Re-register each row **through its own kind's shape**. Sending every
        // row through the nest-shaped call is not merely untidy — for a
        // client-device row it is destructive: `destination_actor_pubkey` rests
        // at its zero sentinel and `destination_nest_url` at empty, so the
        // registry row would be replaced with `kind: "nest"` and both per-kind
        // columns dropped. After that `custodian_assignment_for` finds no row
        // for the device, and a correctly-enrolled custodian goes silently
        // unhosted — the heal breaking exactly what it ran to repair.
        match dest.kind.as_str() {
            DESTINATION_KIND_CLIENT_DEVICE => {
                let Some(device_id) = dest.custodian_device_id.as_deref() else {
                    // No device id ⇒ the row already projects `Inert`; there is
                    // nothing to register and inventing an id would be worse.
                    continue;
                };
                source
                    .destination_register_custodian(
                        dest.destination_id.clone(),
                        device_id.to_string(),
                        dest.capacity_cap_bytes,
                    )
                    .await
                    .map_err(ReconcileError::DestinationRegister)?;
            }
            _ => {
                source
                    .destination_register(
                        dest.destination_id.clone(),
                        dest.destination_nest_url.clone(),
                        hex::encode(dest.destination_actor_pubkey),
                    )
                    .await
                    .map_err(ReconcileError::DestinationRegister)?;
            }
        }
    }
    Ok(destinations.len())
}

/// Failure from [`read_backup_status`].
#[derive(Debug)]
pub enum StatusReadError<E> {
    /// The `fauna.backup.status` call failed.
    Status(E),
    /// The projection reported no enrollment, but reading the box's list to
    /// decide whether a heal was owed failed.
    State(StoreError),
    /// The heal itself failed. The status the nest did return is still
    /// accurate — it simply reports nothing, which is what prompted the heal.
    Reconcile(ReconcileError<E>),
}

impl<E: core::fmt::Display> core::fmt::Display for StatusReadError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Status(e) => write!(f, "reading the nest's backup status: {e}"),
            Self::State(e) => write!(f, "{e}"),
            Self::Reconcile(e) => write!(f, "{e}"),
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug> std::error::Error for StatusReadError<E> {}

/// Read the source nest's Backups-page projection, healing an unenrolled
/// owner in passing. **This is the one status read all 7 apps call**
/// (`docs/goal/behavior/backup-destinations.md` § Per-destination status read) — it replaces the
/// per-app source-side `destination_status()` computation (deleted with the
/// client upload coordinator) and web's degenerate wasm twin.
///
/// The heal is folded in here rather than left to each app so the trigger
/// logic exists once (priority #2): a client that only called
/// `BackupClient::status()` would render blank for any destination whose list
/// row survived without its grant and registry row (a succession rebuild, a
/// crash mid-deregistration). Sequence:
///
/// 1. `fauna.backup.status`. If `enrolled`, return it — the common path, one
///    round trip, no list read.
/// 2. Otherwise read **the bound box's** list (`source_nest` — never another
///    box's: a row under another identity is never registered here,
///    `backup-destinations.md` § *Destination data model*), re-filing a
///    rotated box's list first ([`crate::refile_rotated_box_list`]). Empty ⇒
///    return the not-enrolled reply as-is; the owner genuinely has no backups
///    on this box. An account store not up yet is the same answer: the heal
///    waits for a later read.
/// 3. Non-empty ⇒ this is the unenrolled state:
///    [`reconcile_backup_enrollment`] re-issues the grant + registrations, then
///    re-read the status so the caller gets the healed projection in the same
///    call.
///
/// Step 3 runs at most once per read, and the heal is idempotent, so a client
/// may call this on every page mount. See [`reconcile_backup_enrollment`] for
/// why the destination-side writer grant is deliberately not part of the heal
/// and what the user sees until the slice-5 flip supplies it.
pub async fn read_backup_status<R: RpcRequester + Clone>(
    nest: R,
    store: &dyn BackupStateStore,
    owner_secret: [u8; 32],
    source_nest: [u8; 32],
) -> Result<fauna_protocol::backup::BackupStatusReply, StatusReadError<R::Error>> {
    let reply = BackupClient::new(nest.clone())
        .status()
        .await
        .map_err(StatusReadError::Status)?;
    if reply.enrolled {
        return Ok(reply);
    }

    // Not enrolled. Either the owner has no backups on this box (return
    // as-is), or a list row survived without its grant and registry row and
    // the projection is blank for a backup that is in fact configured — heal,
    // then re-read.
    let state = match load_backup_state_refiled(store, &nest, source_nest).await {
        Ok(state) => state,
        Err(e) if e.is_not_ready() => return Ok(reply),
        Err(e) => return Err(StatusReadError::State(e)),
    };
    if state.backup.destinations.is_empty() {
        return Ok(reply);
    }

    reconcile_backup_enrollment(nest.clone(), owner_secret, &state.backup.destinations)
        .await
        .map_err(StatusReadError::Reconcile)?;

    BackupClient::new(nest)
        .status()
        .await
        .map_err(StatusReadError::Status)
}

/// Failure from [`deregister_backup_destination`]. Mirrors [`EnrollError`]'s
/// shape for the single-nest (source-only) remove sequence.
#[derive(Debug)]
pub enum DeregisterError<E> {
    /// The `fauna.backup.destination.remove` call (source) failed. Nothing was
    /// changed — the list row was not dropped.
    DestinationRemove(E),
    /// Writing the list failed. The source-nest deregister may have
    /// succeeded; a dangling deregistered row is harmless (the coordinator
    /// already has nothing to act on for it).
    Record(BackupWriteError),
}

impl<E: core::fmt::Display> core::fmt::Display for DeregisterError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DestinationRemove(e) => {
                write!(f, "deregistering the destination from the source nest: {e}")
            }
            Self::Record(e) => write!(f, "{e}"),
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug> std::error::Error for DeregisterError<E> {}

/// Deregister a backup destination: remove it from the source nest's own
/// registry, then drop its rows from the bound box's list (`source_nest`).
/// Returns the box's full destination list after the remove.
///
/// Does **not** revoke the destination-side nest-writer grant — see the module
/// docs. `nest` is the **source** nest transport.
pub async fn deregister_backup_destination<R: RpcRequester>(
    nest: R,
    store: &dyn BackupStateStore,
    source_nest: [u8; 32],
    destination_id: &str,
) -> Result<Vec<BackupDestination>, DeregisterError<R::Error>>
where
    R::Error: RpcErrorClass,
{
    // (1) Deregister from the source nest's own registry first — mirrors
    //     enroll's grant-before-config-write ordering (module docs § Ordering).
    let source = BackupClient::new(nest);
    source
        .destination_remove(destination_id.to_string())
        .await
        .map_err(DeregisterError::DestinationRemove)?;

    // (2) The single atomic decision point. When the row is under review the
    //     removal *is* the owner's verdict: `remove_backup_destination` records
    //     it `Removed`, and `mutate_backup` puts that mark row BEFORE the list,
    //     so a concurrent device's pre-removal list reads pruned whichever list
    //     wins (`a_concurrent_removal_is_not_resurrected_by_this_devices_write`).
    let (state, _) = mutate_backup(store, source_nest, |st| {
        remove_backup_destination(st, destination_id)
    })
    .await
    .map_err(DeregisterError::Record)?;
    Ok(state.backup.destinations)
}

/// Failure from [`attach_folder_to_destination`] /
/// [`detach_folder_from_destination`]. Mirrors [`DeregisterError`]'s shape for
/// the source-only coverage sequences.
#[derive(Debug)]
pub enum FolderCoverageError<E> {
    /// The attach/detach kind call (source nest) failed. Nothing was changed —
    /// the list row was not touched.
    Call(E),
    /// Writing the list failed (the list is full, or the account store
    /// refused). The nest-side coverage row may have landed; both verbs are
    /// idempotent, so re-running the sequence converges.
    Record(BackupWriteError),
}

impl<E: core::fmt::Display> core::fmt::Display for FolderCoverageError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Call(e) => write!(f, "folder-coverage call on the source nest: {e}"),
            Self::Record(e) => write!(f, "{e}"),
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug> std::error::Error for FolderCoverageError<E> {}

/// Attach one of the owner's ordinary folders to an enrolled destination
/// (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage):
/// the nest coverage row first (`fauna.backup.destination.attach_folder`), then
/// the list row — one more row in the bound box's list for the same
/// destination whose `folder_name` is the **reply's** `folder_set`, so the
/// `__folder/<hex>/<id>` naming rule lives on the nest and is never re-derived
/// here. Returns the box's full destination list after the attach.
///
/// Same nest-mutation-precedes-config-write ordering as enroll (module docs
/// § Ordering): the nest row alone is inert until the sweep finds content, and
/// idempotent, so a crash between the two steps re-runs cleanly.
///
/// The list row also records the folder's **name** — display name and sealed
/// label, read off the owner's own coverage listing
/// (`fauna.backup.destination.list`'s `CoveredFolder`) right after the attach.
/// After a box loss that row is the one place the label survives for a folder
/// a nest destination holds, and the nest-held pull-back names the restored
/// folder from it (`segment-backup-protocol.md` § Client-device custodian
/// (pull) → *Restore* → *Where a restored folder's name comes from*). A
/// re-attach refreshes it.
///
/// `nest` is the **source** nest transport.
pub async fn attach_folder_to_destination<R: RpcRequester>(
    nest: R,
    store: &dyn BackupStateStore,
    source_nest: [u8; 32],
    destination_id: &str,
    folder_id: i64,
) -> Result<Vec<BackupDestination>, FolderCoverageError<R::Error>>
where
    R::Error: RpcErrorClass,
{
    let source = BackupClient::new(nest);
    let reply = source
        .destination_attach_folder(destination_id.to_string(), folder_id)
        .await
        .map_err(FolderCoverageError::Call)?;
    let label = source
        .destination_list()
        .await
        .map_err(FolderCoverageError::Call)?
        .destinations
        .iter()
        .filter(|d| d.destination_id == destination_id)
        .flat_map(|d| d.covered_folders.iter())
        .find(|c| c.folder_set == reply.folder_set)
        .map(FolderCoverageLabel::of_listing)
        .unwrap_or_default();

    let (state, _) = mutate_backup(store, source_nest, |st| {
        attach_backup_destination_folder(st, destination_id, &reply.folder_set, &label)
    })
    .await
    .map_err(FolderCoverageError::Record)?;
    Ok(state.backup.destinations)
}

/// Detach a folder's destination place: the nest coverage row first
/// (`fauna.backup.destination.detach_folder` — the coordinator's next pass
/// tears the mirrored corpus down per-path), then the list row. The reply's
/// no-op flag is not consulted: both halves are idempotent, and a detach of an
/// already-detached folder must still converge the list. Returns the box's
/// full destination list after the detach.
///
/// Deliberately **not** [`deregister_backup_destination`]: that path is
/// destination-level and records a `Removed` adjudication verdict on open
/// unattested marks — a per-folder detach is not a verdict about the
/// destination (`crate::mutate::detach_backup_destination_folder`).
pub async fn detach_folder_from_destination<R: RpcRequester>(
    nest: R,
    store: &dyn BackupStateStore,
    source_nest: [u8; 32],
    destination_id: &str,
    folder_id: i64,
    folder_set: &str,
) -> Result<Vec<BackupDestination>, FolderCoverageError<R::Error>>
where
    R::Error: RpcErrorClass,
{
    let source = BackupClient::new(nest);
    source
        .destination_detach_folder(destination_id.to_string(), folder_id)
        .await
        .map_err(FolderCoverageError::Call)?;

    let (state, _) = mutate_backup(store, source_nest, |st| {
        detach_backup_destination_folder(st, destination_id, folder_set)
    })
    .await
    .map_err(FolderCoverageError::Record)?;
    Ok(state.backup.destinations)
}

/// One enrolled backup destination, marked attached-or-not for one ordinary
/// folder — what a folders page's *Destination places* section renders
/// (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage).
/// Lifted out of tui's `settings/folders.rs` when linux became the second
/// consumer of the same list+config-names join (priority #2). `Serialize`
/// crosses the wasm boundary (`fauna-wasm/src/rpc.rs`'s `folderDestinations*`
/// trio) — web is the first non-linking consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FolderDestinationPlace {
    /// [`BackupDestination::destination_id`] — the attach/detach kinds' key
    /// and the attach select's round-trip value.
    pub destination_id: String,
    /// The user-facing label: the list row's `display_name`, falling back
    /// to the id when the list could not be read (a label read must never
    /// blank a real coverage read).
    pub label: String,
    /// Whether this folder is attached to the destination.
    pub attached: bool,
    /// The destination-side `__folder/<hex>/<id>` set name, present on
    /// attached rows — the detach sequence's config-row key.
    pub folder_set: Option<String>,
}

/// One ordinary folder's destination places: the nest's own coverage read
/// (`fauna.backup.destination.list` — nest-authoritative, the page's
/// non-optimistic contract) joined with the bound box's list's display names.
/// A list read failure degrades every label to its raw id rather than
/// blanking the coverage read — the same asymmetry [`read_backup_status`]
/// draws between a status truth and its cosmetic dressing.
pub async fn list_folder_destinations<R: RpcRequester + Clone>(
    nest: R,
    store: &dyn BackupStateStore,
    source_nest: [u8; 32],
    folder_id: i64,
) -> Result<Vec<FolderDestinationPlace>, R::Error> {
    let listed = BackupClient::new(nest).destination_list().await?;
    let names: std::collections::HashMap<String, String> =
        match store.backup_state(source_nest).await {
            Ok(state) => state
                .backup
                .destinations
                .iter()
                .filter_map(|d| {
                    d.display_name
                        .clone()
                        .map(|n| (d.destination_id.clone(), n))
                })
                .collect(),
            Err(_) => Default::default(),
        };
    Ok(listed
        .destinations
        .into_iter()
        .map(|d| {
            let covered = d.covered_folders.iter().find(|c| c.folder_id == folder_id);
            FolderDestinationPlace {
                label: names
                    .get(&d.destination_id)
                    .cloned()
                    .unwrap_or_else(|| d.destination_id.clone()),
                attached: covered.is_some(),
                folder_set: covered.map(|c| c.folder_set.clone()),
                destination_id: d.destination_id,
            }
        })
        .collect())
}

/// Record the owner's **Keep** verdict on a destination raised for review — the
/// other half of the post-succession adjudication pair
/// (`succession-aftermath.md` § Re-key scope → *Adjudicating what the aftermath
/// carries across*). *Remove*'s half is [`deregister_backup_destination`],
/// which reuses the row's ordinary remove button rather than minting a second
/// removal path.
///
/// Returns whether anything was actually open, so a caller can tell a real
/// adjudication from a no-op. **A no-op is not an error:** the owner may press
/// Keep on a row a concurrent device already answered, and the honest outcome is
/// the same either way — the row is no longer raised.
///
/// The verdict is one mark row's put through the plane's per-row join, in
/// which a decided verdict beats a still-open one — so a *Keep* on the laptop
/// is never undone by the phone's next write, and a Keep on an
/// already-answered row puts nothing ([`crate::mutate_backup`] writes only
/// the marks the gesture moved).
///
/// **This lives here rather than in each app** because the plane's other six
/// renderers are still owed, and
/// a per-app writer is how tui's copy drifted onto the blind path in the first
/// place: an app that does not write the verdict itself cannot get this wrong.
pub async fn keep_backup_destination_at_rest(
    store: &dyn BackupStateStore,
    source_nest: [u8; 32],
    destination_id: &str,
) -> Result<bool, BackupWriteError> {
    let (_, kept) = mutate_backup(store, source_nest, |st| {
        keep_backup_destination(st, destination_id)
    })
    .await?;
    Ok(kept)
}

/// What the shell knows about the device being enrolled as a custodian — the
/// input to [`enroll_client_custodian`].
///
/// There is no `ResolvedDestination` analogue and no reachability step: a
/// custodian has no address to resolve, and the device enrolling is the one
/// running this code (`behavior/backup-destinations.md` § Third destination kind → *Enrollment*
/// — no "add my iPad from the laptop" flow).
#[derive(Debug, Clone)]
pub struct CustodianEnrollment {
    /// Caller-minted opaque row id, exactly as the nest path mints one.
    pub destination_id: String,
    /// **This** device's stable sync `device_id` — the same id its file-sync
    /// engines present, and the key the source nest projects the custodian's
    /// status row on.
    pub custodian_device_id: String,
    /// The friendly label. Blank falls back to the device id, so a row is never
    /// nameless in the destination list.
    pub display_name: String,
    /// The user-set capacity cap in bytes — the kind's only knob. `None` is
    /// uncapped, which the pull pass reads as "fill the disk", so a shell that
    /// offers no cap control should be deliberate about passing it.
    pub capacity_cap_bytes: Option<u64>,
}

/// Failure from [`enroll_client_custodian`]. Source-nest-only — there is no
/// second nest in this sequence — so one error type, unlike
/// [`EnrollError`]'s pair.
#[derive(Debug)]
pub enum CustodianEnrollError<E> {
    /// The device id was blank. Nothing was called: a `client-device` row
    /// without one projects as [`fauna_core::data::DestinationKind::Inert`], so
    /// writing it would record a destination the user sees in their list and
    /// nothing can ever drive — the live half-state
    /// `nest/common.md` § Client-state recoverability forbids.
    MissingDeviceId,
    /// The `fauna.backup.destination.register` call failed. Nothing was
    /// recorded — the list write had not happened yet.
    DestinationRegister(E),
    /// Recording the row failed (the list is full, or the account store
    /// refused). The registration above may have succeeded; it is idempotent
    /// and inert without the list row, and the next enroll attempt re-issues
    /// it.
    Record(BackupWriteError),
}

impl<E: core::fmt::Display> core::fmt::Display for CustodianEnrollError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingDeviceId => write!(
                f,
                "this device has no sync device id yet, so it cannot be enrolled as a custodian"
            ),
            Self::DestinationRegister(e) => {
                write!(f, "registering this device with the source nest: {e}")
            }
            Self::Record(e) => write!(f, "{e}"),
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug> std::error::Error for CustodianEnrollError<E> {}

/// Enroll **this device** as a client custodian — the third destination kind's
/// three-step enrollment (`docs/goal/behavior/backup-destinations.md` § State & data shape →
/// *Third destination kind* → *Enrollment*). Returns the owner's full
/// destination list after the add.
///
/// Step (1) — the opt-in and the capacity cap — is the shell's UI, and arrives
/// here as `enrollment`. This function is steps (2) and (3):
///
/// 2. `fauna.backup.destination.register` on the **source** nest, carrying the
///    kind + this device's id, so the nest can project the destination's status
///    row from the check-ins the pull pass will write. Idempotent on
///    `destination_id` and inert on its own.
/// 3. The `fauna.state.backup` write (via [`crate::mutate_backup`])
///    recording the `BackupDestination` row in the bound box's list. **The
///    single atomic decision point.**
///
/// Most of the nest sequence is absent by design, not by omission: there is no
/// destination server, so no `fauna.nest.info` read, no destination-side writer
/// grant, and — the one worth stating — **no `NestBackupKey` grant**. That grant
/// exists to let a *nest* seal on the owner's behalf; a custodian seals for
/// itself from the seed it already holds.
///
/// # Crash-safety
///
/// A crash before step 3 leaves one inert idempotent registry row and nothing
/// else: no local store has been created, and the nest will not dial a
/// custodian. A crash after it leaves a configured destination whose first pull
/// pass builds the store from scratch — the local store is derived state,
/// re-buildable from a fresh pull, which is why it is not part of the atomic
/// decision at all.
pub async fn enroll_client_custodian<R: RpcRequester>(
    nest: R,
    store: &dyn BackupStateStore,
    source_nest: [u8; 32],
    enrollment: CustodianEnrollment,
) -> Result<Vec<BackupDestination>, CustodianEnrollError<R::Error>>
where
    R::Error: RpcErrorClass,
{
    let device_id = enrollment.custodian_device_id.trim();
    if device_id.is_empty() {
        return Err(CustodianEnrollError::MissingDeviceId);
    }

    let display_name = if enrollment.display_name.trim().is_empty() {
        device_id.to_string()
    } else {
        enrollment.display_name.trim().to_string()
    };

    let source = BackupClient::new(nest);

    // (2) Tell the source nest this device holds a replica, so its status
    //     projection has a row to invert. Idempotent, inert alone.
    source
        .destination_register_custodian(
            enrollment.destination_id.clone(),
            device_id.to_string(),
            // The registry copy is what this device's *host* reads back: on
            // desktop the pull runs in the bearer-only sync agent, which cannot
            // open the at-rest row written in step (3).
            enrollment.capacity_cap_bytes,
        )
        .await
        .map_err(CustodianEnrollError::DestinationRegister)?;

    let dest = BackupDestination {
        destination_id: enrollment.destination_id,
        folder_name: DEFAULT_BACKUP_FOLDER.to_string(),
        added_at: Timestamp::now_secs().max(0) as u64,
        display_name: Some(display_name),
        kind: DESTINATION_KIND_CLIENT_DEVICE.to_string(),
        custodian_device_id: Some(device_id.to_string()),
        capacity_cap_bytes: enrollment.capacity_cap_bytes,
        // `destination_nest_url` / `destination_actor_pubkey` stay at their
        // empty sentinels: this kind has no address, and `kind_view` is what
        // stops any call site reading them here.
        ..Default::default()
    };

    // (3) The single atomic decision point.
    let (state, ()) = mutate_backup(store, source_nest, |st| add_backup_destination(st, dest))
        .await
        .map_err(CustodianEnrollError::Record)?;
    Ok(state.backup.destinations)
}

/// The enrollment that makes this device the custodian of the nest it just
/// re-seeded: this device's own client-device row when `destinations` still
/// names it (same id, name and cap, so the registry step is the idempotent
/// re-register it is documented to be), otherwise a fresh row under
/// `fresh_destination_id` with the enrollment's own defaults.
pub fn custodian_reenrollment(
    destinations: &[BackupDestination],
    device_id: &str,
    fresh_destination_id: impl FnOnce() -> String,
) -> CustodianEnrollment {
    let device_id = device_id.trim();
    let mine = destinations.iter().find(|d| {
        matches!(
            d.kind_view(),
            fauna_core::data::DestinationKind::ClientDevice { device_id: row, .. }
                if !device_id.is_empty() && row.trim() == device_id
        )
    });
    CustodianEnrollment {
        destination_id: mine
            .map(|d| d.destination_id.clone())
            .unwrap_or_else(fresh_destination_id),
        custodian_device_id: device_id.to_string(),
        display_name: mine
            .and_then(|d| d.display_name.clone())
            .unwrap_or_default(),
        capacity_cap_bytes: mine.and_then(|d| d.capacity_cap_bytes),
    }
}

/// The re-seed's post-ceremony duty (`behavior/backup-destinations.md`
/// § Re-seed → *Where the ceremony runs*): re-enroll this device as the
/// custodian of the seeded nest, through the ordinary
/// [`enroll_client_custodian`], **only when the outcome is whole**. A custodian's
/// pull reads its target as its source, so a pull against a nest that does not
/// yet serve the whole corpus would read the missing part as deleted.
///
/// `None` when the outcome is not whole (nothing to do; the owner re-runs the
/// restore). Every app runs this one function after the ceremony, whichever
/// process drove it: the desktop agent's job or a phone's in-process driver.
pub async fn reenroll_custodian_after_reseed<R: RpcRequester>(
    nest: R,
    store: &dyn BackupStateStore,
    source_nest: [u8; 32],
    outcome: &fauna_client_backup::reseed::ReseedOutcome,
    device_id: &str,
    destinations: &[BackupDestination],
    fresh_destination_id: impl FnOnce() -> String,
) -> Option<Result<Vec<BackupDestination>, CustodianEnrollError<R::Error>>>
where
    R::Error: RpcErrorClass,
{
    if !outcome.is_whole() {
        return None;
    }
    let enrollment = custodian_reenrollment(destinations, device_id, fresh_destination_id);
    Some(enroll_client_custodian(nest, store, source_nest, enrollment).await)
}

/// The nest-held pull-back's post-ceremony duty (`segment-backup-protocol.md`
/// § Client-device custodian (pull) → *Restore* → *The nest-held pull-back*):
/// make the surviving destination the corpus was pulled back from a backup
/// destination of the **restored** nest, through the ordinary five-step
/// enroll sequence, **only when the outcome is whole** — a destination whose
/// copy the restored nest does not yet serve whole would be overwritten by
/// the restored nest's first pass with the hole in it.
///
/// `nest` is the restored nest (the new source); `destination` the owner's own
/// connection to the destination named by `row`, a peer-nest row of the lost
/// box's list; `restored_nest_id` the restored nest's identity as its
/// connection proved it, and the box whose list the row lands in.
///
/// **The writer seat moves with the ceremony** (`segment-backup-protocol.md`
/// § Cross-location backup protocol → *The writer seat* → *How the seat
/// moves*, situation (2)): a destination keeps one nest-writer per owner, and
/// after a rebuild onto a fresh identity its seat still names the lost box, so
/// a plain registration would be refused `writer_seat_held`. Step (3)
/// therefore reads the seat's holder off the destination and names it as
/// `succeeds` — for this one destination, inside this one completed ceremony.
/// A same-identity rebuild finds the seat already its own and refreshes it.
///
/// `None` when the outcome is not whole (nothing to do; the owner re-runs the
/// restore).
pub async fn reenroll_nest_destination_after_reseed<R: RpcRequester, D: RpcRequester>(
    nest: R,
    destination: D,
    store: &dyn BackupStateStore,
    owner_secret: [u8; 32],
    restored_nest_id: [u8; 32],
    row: &BackupDestination,
    outcome: &fauna_client_backup::reseed::ReseedOutcome,
) -> Option<Result<Vec<BackupDestination>, EnrollError<R::Error, D::Error>>>
where
    R::Error: RpcErrorClass,
{
    if !outcome.is_whole() {
        return None;
    }
    let dest = BackupDestination {
        destination_id: row.destination_id.clone(),
        destination_nest_url: row.destination_nest_url.clone(),
        destination_actor_pubkey: row.destination_actor_pubkey,
        folder_name: DEFAULT_BACKUP_FOLDER.to_string(),
        added_at: Timestamp::now_secs().max(0) as u64,
        display_name: row.display_name.clone(),
        kind: DESTINATION_KIND_NEST.to_string(),
        ..Default::default()
    };
    Some(
        enroll_nest_row(
            nest,
            destination,
            store,
            owner_secret,
            dest,
            restored_nest_id,
            SeatStep::SucceedHolder,
        )
        .await,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use fauna_client_backup::{
        KIND_DESTINATION_ATTACH_FOLDER, KIND_DESTINATION_DETACH_FOLDER, KIND_DESTINATION_LIST,
        KIND_DESTINATION_REGISTER, KIND_DESTINATION_REMOVE, KIND_NEST_KEY_GRANT, KIND_STATUS,
        KIND_WRITER_GRANT_LIST, KIND_WRITER_GRANT_REGISTER,
    };
    use fauna_core::backup_state::{BackupDestinationsRow, BackupState};
    use fauna_core::data::{BackupConfig, DestinationUnattestedMark, UnattestedVerdict};
    use fauna_core::identity::{ActorId, ActorKeypair};
    use fauna_protocol::RpcError;
    use fauna_protocol::backup::{
        AttachFolderReply, AttachFolderRequest, BackupStatusReply, DestinationRegisterReply,
        DestinationRegisterRequest, DestinationRemoveReply, DestinationRemoveRequest,
        DetachFolderReply, DetachFolderRequest, NestKeyGrantReply, NestKeyGrantRequest,
        WriterGrantRegisterReply, WriterGrantRegisterRequest,
    };
    use fauna_protocol::discovery::{ModerationInfo, NestInfoReply};
    use fauna_protocol::nest_rotation::{NestRotation, ROTATION_CHAIN_KIND, RotationChainReply};
    use fauna_protocol::requester::RpcErrorClass;

    use fauna_client_testkit::block_on;

    use crate::backup_store::{
        BackupWriteError, load_backup_state_refiled, raise_succession_destination_marks,
        refile_rotated_box_list,
    };
    use crate::test_helpers::{BACKUP_STATE_READ, BACKUP_STATE_WRITE, FakeBackupStateStore};

    use super::*;

    const OWNER_SEED: [u8; 32] = [7u8; 32];
    const SOURCE_NEST_ID_HEX_DEFAULT: &str = "aa";
    /// The source nest's identity as its connection proved it — what every
    /// enroll call in these tests hands in as `source_nest_id`, and the box
    /// whose `fauna.state.backup` list the sequences read and write.
    const SOURCE_BOUND_ID: [u8; 32] = [0xaa; 32];
    /// Another box the account keeps a list on — never this connection's.
    const OTHER_BOX_ID: [u8; 32] = [0xbb; 32];
    /// The source's own `fauna.nest.info` kind — the fakes still answer it, so
    /// an enroll that asked it would be observable.
    const KIND_NODE_INFO: &str = "fauna.nest.info";

    /// A fake transport failure: the message, plus the wire [`RpcError`] when
    /// this models a server rejection carrying a code the client matches on.
    #[derive(Debug)]
    struct FakeError(String, Option<RpcError>);

    impl FakeError {
        /// A plain refusal with no code the client branches on.
        fn new(msg: &str) -> Self {
            Self(msg.into(), None)
        }
    }

    impl core::fmt::Display for FakeError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    /// Every fake failure here is a server *rejection*.
    impl RpcErrorClass for FakeError {
        fn is_rejection(&self) -> bool {
            true
        }
        fn as_rpc_error(&self) -> Option<&RpcError> {
            self.1.as_ref()
        }
    }

    /// A minimal valid `fauna.nest.info` reply carrying `nest_id` — mirrors
    /// `store.rs`'s test helper of the same shape.
    fn nest_info_reply(nest_id_hex: &str) -> NestInfoReply {
        NestInfoReply {
            domain: "source.example".into(),
            nest_id: nest_id_hex.into(),
            version: "test".into(),
            software: "fauna".into(),
            protocols: vec!["fauna".into()],
            capabilities: vec![],
            iroh_relay_url: None,
            subhandles: false,
            registration: None,
            moderation: ModerationInfo {
                extra: Default::default(),
            },
            ..Default::default()
        }
    }

    /// In-memory source nest plus the account's `fauna.state.backup` store,
    /// with ONE call log both write into — the nest's kinds in order, and the
    /// store's reads/writes ([`BACKUP_STATE_READ`] / [`BACKUP_STATE_WRITE`])
    /// between them — which is what the ordering guarantees are asserted on.
    /// `own_nest_id_hex` is what `fauna.nest.info` reports; the `fail_*` flags
    /// make the matching call reject; `rotation_chain` is what
    /// `fauna.auth.rotation_chain` answers (`None`: the call fails).
    struct FakeSourceNest {
        kinds: Arc<Mutex<Vec<&'static str>>>,
        /// The account store the sequences run over, logging into `kinds`.
        store: FakeBackupStateStore,
        granted_key: Mutex<Option<Vec<u8>>>,
        registered_destination: Mutex<Option<DestinationRegisterRequest>>,
        removed_destination_id: Mutex<Option<String>>,
        /// What `fauna.backup.status` reports for `enrolled`. Flipped to true
        /// by a `nest_key.grant`, mirroring the real nest, where the stored
        /// grant is precisely what makes a coordinator openable.
        enrolled: Mutex<bool>,
        own_nest_id_hex: String,
        fail_grant: bool,
        fail_destination_register: bool,
        fail_destination_remove: bool,
        attached_folder: Mutex<Option<AttachFolderRequest>>,
        detached_folder: Mutex<Option<DetachFolderRequest>>,
        rotation_chain: Mutex<Option<RotationChainReply>>,
        /// The display name `fauna.backup.destination.list` reports for the
        /// attached folder; `None` lists it with no plaintext name (a set
        /// sealed past it), still carrying its sealed pair.
        listed_folder_name: Mutex<Option<String>>,
    }

    impl Default for FakeSourceNest {
        fn default() -> Self {
            Self::sharing(&FakeBackupStateStore::empty())
        }
    }

    impl FakeSourceNest {
        /// A second connection over the same account store — another device,
        /// or the same one after a reconnect. Its own call log.
        fn sharing(store: &FakeBackupStateStore) -> Self {
            let kinds = Arc::new(Mutex::new(Vec::new()));
            Self {
                store: store.logging_into(kinds.clone()),
                kinds,
                granted_key: Mutex::new(None),
                registered_destination: Mutex::new(None),
                removed_destination_id: Mutex::new(None),
                enrolled: Mutex::new(false),
                own_nest_id_hex: SOURCE_NEST_ID_HEX_DEFAULT.to_string(),
                fail_grant: false,
                fail_destination_register: false,
                fail_destination_remove: false,
                attached_folder: Mutex::new(None),
                detached_folder: Mutex::new(None),
                rotation_chain: Mutex::new(None),
                listed_folder_name: Mutex::new(Some("Photos".to_string())),
            }
        }

        fn kinds(&self) -> Vec<&'static str> {
            self.kinds.lock().unwrap().clone()
        }
    }

    impl RpcRequester for FakeSourceNest {
        type Error = FakeError;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.kinds.lock().unwrap().push(kind);
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            let reply = match kind {
                KIND_NEST_KEY_GRANT => {
                    if self.fail_grant {
                        return Err(FakeError::new("source nest refused the grant"));
                    }
                    let req: NestKeyGrantRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode grant request");
                    *self.granted_key.lock().unwrap() = Some(req.nest_backup_key.into_vec());
                    // A stored grant is exactly what makes the nest's
                    // coordinator openable, so the projection now reports
                    // enrolled — the real handler's `open_for_owner` contract.
                    *self.enrolled.lock().unwrap() = true;
                    fauna_protocol::encode_canonical(&NestKeyGrantReply {
                        ok: true,
                        extra: Default::default(),
                    })
                }
                KIND_STATUS => fauna_protocol::encode_canonical(&BackupStatusReply {
                    enrolled: *self.enrolled.lock().unwrap(),
                    // Row contents are the nest's business (tier_3-proven
                    // there); this crate's contract is the enrolled flag and
                    // the heal it drives.
                    destinations: Vec::new(),
                    extra: Default::default(),
                }),
                KIND_NODE_INFO => {
                    fauna_protocol::encode_canonical(&nest_info_reply(&self.own_nest_id_hex))
                }
                KIND_DESTINATION_REGISTER => {
                    if self.fail_destination_register {
                        return Err(FakeError::new(
                            "source nest refused the destination register",
                        ));
                    }
                    let req: DestinationRegisterRequest = fauna_protocol::decode_strict(&bytes)
                        .expect("decode dest register request");
                    *self.registered_destination.lock().unwrap() = Some(req);
                    fauna_protocol::encode_canonical(&DestinationRegisterReply {
                        ok: true,
                        extra: Default::default(),
                    })
                }
                KIND_DESTINATION_REMOVE => {
                    if self.fail_destination_remove {
                        return Err(FakeError::new("source nest refused the destination remove"));
                    }
                    let req: DestinationRemoveRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode dest remove request");
                    *self.removed_destination_id.lock().unwrap() = Some(req.destination_id);
                    fauna_protocol::encode_canonical(&DestinationRemoveReply {
                        removed: true,
                        extra: Default::default(),
                    })
                }
                KIND_DESTINATION_ATTACH_FOLDER => {
                    let req: AttachFolderRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode attach request");
                    // The real nest derives the set name from ITS OWN identity
                    // — the client must record the reply verbatim, so this fake
                    // uses a hex the client cannot have derived itself.
                    let folder_set = format!("__folder/{}/{}", self.own_nest_id_hex, req.folder_id);
                    *self.attached_folder.lock().unwrap() = Some(req);
                    fauna_protocol::encode_canonical(&AttachFolderReply {
                        attached: true,
                        folder_set,
                        extra: Default::default(),
                    })
                }
                KIND_DESTINATION_LIST => {
                    // The attached folder, listed under the destination the
                    // attach named, with the label the source's row carries.
                    let attached = self.attached_folder.lock().unwrap().clone();
                    let destinations = attached
                        .map(|a| fauna_protocol::backup::DestinationItem {
                            destination_id: a.destination_id.clone(),
                            covered_folders: vec![fauna_protocol::backup::CoveredFolder {
                                folder_id: a.folder_id,
                                folder_set: format!(
                                    "__folder/{}/{}",
                                    self.own_nest_id_hex, a.folder_id
                                ),
                                name: self.listed_folder_name.lock().unwrap().clone(),
                                name_hash: Some(serde_bytes::ByteBuf::from(vec![0x4E; 32])),
                                name_sealed: Some(serde_bytes::ByteBuf::from(b"sealed".to_vec())),
                                ..Default::default()
                            }],
                            ..Default::default()
                        })
                        .into_iter()
                        .collect();
                    fauna_protocol::encode_canonical(
                        &fauna_protocol::backup::DestinationListReply {
                            destinations,
                            ..Default::default()
                        },
                    )
                }
                KIND_DESTINATION_DETACH_FOLDER => {
                    let req: DetachFolderRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode detach request");
                    *self.detached_folder.lock().unwrap() = Some(req);
                    fauna_protocol::encode_canonical(&DetachFolderReply {
                        detached: true,
                        extra: Default::default(),
                    })
                }
                ROTATION_CHAIN_KIND => match self.rotation_chain.lock().unwrap().clone() {
                    Some(reply) => fauna_protocol::encode_canonical(&reply),
                    None => return Err(FakeError::new("this nest serves no rotation chain")),
                },
                other => panic!("unexpected kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    /// In-memory destination nest: records every `fauna.backup.writer_grant.
    /// register` call. `fail_writer_grant` makes it reject.
    #[derive(Default)]
    struct FakeDestinationNest {
        kinds: Mutex<Vec<&'static str>>,
        registered_writer_nest_id: Mutex<Option<String>>,
        fail_writer_grant: bool,
        /// The writer seat's holder, as `fauna.backup.writer_grant.list`
        /// reports it; a register moves it per the seat's rule.
        seat: Mutex<Option<String>>,
        /// The `succeeds` each register named, in order.
        succeeds: Mutex<Vec<Option<String>>>,
    }

    impl RpcRequester for FakeDestinationNest {
        type Error = FakeError;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.kinds.lock().unwrap().push(kind);
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            let reply = match kind {
                KIND_WRITER_GRANT_REGISTER => {
                    if self.fail_writer_grant {
                        return Err(FakeError::new("destination refused the writer grant"));
                    }
                    let req: WriterGrantRegisterRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode writer grant request");
                    self.succeeds.lock().unwrap().push(req.succeeds.clone());
                    // The seat's rule: a plain register by another box while
                    // the seat is held is refused, a `succeeds` naming the
                    // holder moves it, one naming anyone else is refused.
                    let mut seat = self.seat.lock().unwrap();
                    let allowed = match (&*seat, &req.succeeds) {
                        (None, None) => true,
                        (Some(holder), None) => *holder == req.writer_nest_id,
                        (Some(holder), Some(named)) => holder == named,
                        (None, Some(_)) => false,
                    };
                    if !allowed {
                        return Err(FakeError::new("fauna.backup.writer_seat_held"));
                    }
                    *seat = Some(req.writer_nest_id.clone());
                    *self.registered_writer_nest_id.lock().unwrap() = Some(req.writer_nest_id);
                    fauna_protocol::encode_canonical(&WriterGrantRegisterReply {
                        ok: true,
                        extra: Default::default(),
                    })
                }
                KIND_WRITER_GRANT_LIST => fauna_protocol::encode_canonical(
                    &fauna_protocol::backup::WriterGrantListReply {
                        grants: self
                            .seat
                            .lock()
                            .unwrap()
                            .iter()
                            .map(|holder| fauna_protocol::backup::WriterGrantItem {
                                writer_nest_id: holder.clone(),
                                ..Default::default()
                            })
                            .collect(),
                        ..Default::default()
                    },
                ),
                other => panic!("unexpected kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    fn resolved(name: &str) -> ResolvedDestination {
        ResolvedDestination {
            destination_id: "dest-1".into(),
            destination_nest_url: "https://dest.example/".into(),
            destination_actor_pubkey: [9u8; 32],
            domain: "dest.example".into(),
            requested_name: name.into(),
        }
    }

    /// Enroll `resolved` over `nest` and its store, bound to the source box.
    fn enroll(
        nest: &Arc<FakeSourceNest>,
        resolved: ResolvedDestination,
    ) -> Result<Vec<BackupDestination>, EnrollError<FakeError, FakeError>> {
        block_on(enroll_backup_destination(
            nest.clone(),
            Arc::new(FakeDestinationNest::default()),
            &nest.store,
            OWNER_SEED,
            resolved,
            SOURCE_BOUND_ID,
        ))
    }

    fn ids(destinations: &[BackupDestination]) -> Vec<&str> {
        destinations
            .iter()
            .map(|d| d.destination_id.as_str())
            .collect()
    }

    #[test]
    fn enroll_grants_the_derived_key_then_records_the_destination() {
        let nest = Arc::new(FakeSourceNest::default());
        let list = enroll(&nest, resolved("My backup box")).expect("enroll succeeds");

        // The granted bytes are exactly the seed-derived NestBackupKey — the
        // whole point of the grant leg. A drift here would seal segments under a
        // key the owner's client cannot re-derive at restore.
        assert_eq!(
            nest.granted_key.lock().unwrap().as_deref(),
            Some(&NestBackupKey::derive(&OWNER_SEED).to_bytes()[..]),
        );

        assert_eq!(list.len(), 1);
        let dest = &list[0];
        assert_eq!(dest.destination_id, "dest-1");
        assert_eq!(dest.destination_actor_pubkey, [9u8; 32]);
        assert_eq!(dest.folder_name, DEFAULT_BACKUP_FOLDER);
        assert_eq!(dest.display_name.as_deref(), Some("My backup box"));
        // The row landed in the bound box's list, and in no other box's.
        assert_eq!(nest.store.state(SOURCE_BOUND_ID).backup.destinations, list);
        assert_eq!(nest.store.lists().len(), 1);
    }

    /// The ordering guarantee from the module docs: all four nest mutations
    /// precede the list write, so a crash among them leaves inert nest-side
    /// state rather than a destination whose backups silently never seal.
    #[test]
    fn all_nest_mutations_precede_the_list_write() {
        let nest = Arc::new(FakeSourceNest::default());
        let destination = Arc::new(FakeDestinationNest::default());
        block_on(enroll_backup_destination(
            nest.clone(),
            destination.clone(),
            &nest.store,
            OWNER_SEED,
            resolved(""),
            SOURCE_BOUND_ID,
        ))
        .expect("enroll");

        let source_kinds = nest.kinds();
        let put_at = source_kinds
            .iter()
            .position(|k| *k == BACKUP_STATE_WRITE)
            .expect("the list must be written");
        for kind in [KIND_NEST_KEY_GRANT, KIND_DESTINATION_REGISTER] {
            let at = source_kinds
                .iter()
                .position(|k| *k == kind)
                .unwrap_or_else(|| panic!("{kind} must be called; source kinds: {source_kinds:?}"));
            assert_precedes(at, put_at, kind, &source_kinds);
        }
        assert!(
            destination
                .kinds
                .lock()
                .unwrap()
                .contains(&KIND_WRITER_GRANT_REGISTER),
            "the writer grant must be registered at the destination"
        );
    }

    // ── reconcile_backup_enrollment ─────────────────────────────────────────
    //
    // The unenrolled heal: a destination whose list row survived without its
    // grant + source-side registry row (a succession rebuild, a crash between
    // deregistration's two writes) makes `fauna.backup.status` report
    // `enrolled: false`, and the repointed Backups page would render blank for
    // a live backup.

    /// A list row with none of the nest-side enrollment beside it — the
    /// state this heal exists for.
    fn unenrolled_destination(id: &str) -> BackupDestination {
        BackupDestination {
            destination_id: id.into(),
            destination_nest_url: format!("https://{id}.example/"),
            destination_actor_pubkey: [9u8; 32],
            folder_name: DEFAULT_BACKUP_FOLDER.to_string(),
            added_at: 1_700_000_000,
            display_name: Some(id.into()),
            ..Default::default()
        }
    }

    #[test]
    fn reconcile_regrants_then_registers_every_configured_destination() {
        let nest = Arc::new(FakeSourceNest::default());
        let destinations = [
            unenrolled_destination("dest-1"),
            unenrolled_destination("dest-2"),
        ];

        let healed = block_on(reconcile_backup_enrollment(
            nest.clone(),
            OWNER_SEED,
            &destinations,
        ))
        .expect("reconcile");

        assert_eq!(healed, 2);
        assert_eq!(
            nest.kinds().as_slice(),
            &[
                KIND_NEST_KEY_GRANT,
                KIND_DESTINATION_REGISTER,
                KIND_DESTINATION_REGISTER,
            ],
            "reconcile issues exactly the grant + one register per destination"
        );
    }

    /// The grant must carry the same derived key enroll grants, or the nest
    /// would seal this owner's segments under a key their client cannot open.
    #[test]
    fn reconcile_grants_the_same_derived_key_as_enroll() {
        let nest = Arc::new(FakeSourceNest::default());
        block_on(reconcile_backup_enrollment(
            nest.clone(),
            OWNER_SEED,
            &[unenrolled_destination("dest-1")],
        ))
        .expect("reconcile");

        let granted = nest.granted_key.lock().unwrap().clone();
        assert_eq!(
            granted.as_deref(),
            Some(NestBackupKey::derive(&OWNER_SEED).to_bytes().as_slice()),
            "reconcile must re-grant the seed-derived NestBackupKey"
        );
    }

    /// Zero destinations ⇒ no calls at all. Granting a key with nothing to back
    /// up would make the projection claim an enrollment the owner never made.
    #[test]
    fn reconcile_with_no_destinations_touches_the_nest_not_at_all() {
        let nest = Arc::new(FakeSourceNest::default());
        let healed = block_on(reconcile_backup_enrollment(nest.clone(), OWNER_SEED, &[]))
            .expect("reconcile");

        assert_eq!(healed, 0);
        assert!(
            nest.kinds().is_empty(),
            "an unconfigured owner must not be enrolled by a status read"
        );
    }

    /// Registration keys on `destination_id`, so a second pass over an
    /// already-healed owner re-issues the same calls rather than duplicating
    /// rows — which is what lets callers run this unconditionally.
    #[test]
    fn reconcile_is_repeatable_with_the_same_registrations() {
        let nest = Arc::new(FakeSourceNest::default());
        let destinations = [unenrolled_destination("dest-1")];

        for _ in 0..2 {
            block_on(reconcile_backup_enrollment(
                nest.clone(),
                OWNER_SEED,
                &destinations,
            ))
            .expect("reconcile");
        }

        let registered = nest.registered_destination.lock().unwrap().clone();
        assert_eq!(
            registered
                .expect("a destination was registered")
                .destination_id,
            "dest-1"
        );
        let registers = nest
            .kinds()
            .iter()
            .filter(|k| **k == KIND_DESTINATION_REGISTER)
            .count();
        assert_eq!(registers, 2, "each pass re-issues the idempotent register");
    }

    // ── read_backup_status ──────────────────────────────────────────────────

    fn status(nest: &Arc<FakeSourceNest>, source_nest: [u8; 32]) -> BackupStatusReply {
        block_on(read_backup_status(
            nest.clone(),
            &nest.store,
            OWNER_SEED,
            source_nest,
        ))
        .expect("status")
    }

    /// The common path: an enrolled owner costs exactly one round trip and
    /// never reads the list. Cheap enough to call on every page mount.
    #[test]
    fn status_read_of_an_enrolled_owner_is_one_round_trip() {
        let nest = Arc::new(FakeSourceNest::default());
        *nest.enrolled.lock().unwrap() = true;

        let reply = status(&nest, SOURCE_BOUND_ID);

        assert!(reply.enrolled);
        assert_eq!(
            nest.kinds().as_slice(),
            &[KIND_STATUS],
            "an enrolled owner must not trigger a list read or a heal"
        );
    }

    /// An owner with no destinations is not enrolled and must stay that way —
    /// a status read must never mint an enrollment.
    #[test]
    fn status_read_without_destinations_does_not_enroll() {
        let nest = Arc::new(FakeSourceNest::default());

        let reply = status(&nest, SOURCE_BOUND_ID);

        assert!(!reply.enrolled);
        let kinds = nest.kinds();
        assert!(
            !kinds.contains(&KIND_NEST_KEY_GRANT),
            "no grant may be issued for an owner with no destinations; kinds: {kinds:?}"
        );
    }

    /// The regression this whole path exists to prevent: a destination whose
    /// list row survived without its enrollment must not render as a blank
    /// page. The read heals it — from the bound box's own list, and only that
    /// list — and returns the *healed* projection.
    #[test]
    fn status_read_heals_an_unenrolled_destination_and_rereads() {
        let nest = Arc::new(FakeSourceNest::default());
        // The box's list holds a destination; the nest knows nothing about it.
        // Another box's list rests beside it and must not ride the heal.
        nest.store
            .seed_list(SOURCE_BOUND_ID, vec![unenrolled_destination("dest-1")]);
        nest.store
            .seed_list(OTHER_BOX_ID, vec![unenrolled_destination("elsewhere")]);

        let reply = status(&nest, SOURCE_BOUND_ID);

        let kinds = nest.kinds();
        assert!(
            kinds.contains(&KIND_NEST_KEY_GRANT) && kinds.contains(&KIND_DESTINATION_REGISTER),
            "the unenrolled state must be healed; kinds: {kinds:?}"
        );
        assert_eq!(
            kinds
                .iter()
                .filter(|k| **k == KIND_DESTINATION_REGISTER)
                .count(),
            1,
            "only the bound box's own destination is registered; kinds: {kinds:?}"
        );
        assert_eq!(
            nest.registered_destination
                .lock()
                .unwrap()
                .as_ref()
                .map(|r| r.destination_id.as_str()),
            Some("dest-1")
        );
        assert_eq!(
            kinds.iter().filter(|k| **k == KIND_STATUS).count(),
            2,
            "status is re-read after the heal so the caller gets the healed rows"
        );
        assert!(
            reply.enrolled,
            "the returned projection must be the post-heal one"
        );
    }

    /// **The per-box rule at the heal.** The account keeps
    /// a list for box A; this connection is bound to box B, whose nest
    /// reports no enrollment. B's own list is empty, so NOTHING is registered
    /// at B — a destination the owner configured for another box must never
    /// be granted or registered on this one (`backup-destinations.md`
    /// § *Destination data model*) — and the reply is the honest
    /// not-enrolled one.
    #[test]
    fn the_heal_never_registers_another_boxs_list() {
        let nest = Arc::new(FakeSourceNest::default());
        nest.store
            .seed_list(OTHER_BOX_ID, vec![unenrolled_destination("box-a-dest")]);
        // B answers its rotation chain honestly: it never rotated.
        *nest.rotation_chain.lock().unwrap() = Some(RotationChainReply::default());

        let reply = status(&nest, SOURCE_BOUND_ID);

        assert!(!reply.enrolled, "the not-enrolled reply comes back as-is");
        let kinds = nest.kinds();
        assert!(
            !kinds.contains(&KIND_NEST_KEY_GRANT) && !kinds.contains(&KIND_DESTINATION_REGISTER),
            "box A's list must not be granted or registered at box B; kinds: {kinds:?}"
        );
        assert!(
            nest.registered_destination.lock().unwrap().is_none(),
            "nothing registered at B"
        );
        assert_eq!(nest.store.writes(), 0, "and nothing re-filed under B");
        assert!(
            nest.store
                .state(SOURCE_BOUND_ID)
                .backup
                .destinations
                .is_empty()
        );
    }

    /// An account store not up yet reads as "no heal owed this time": the
    /// not-enrolled reply, no heal, no error — the next page read heals.
    #[test]
    fn status_read_with_the_store_not_ready_returns_the_reply_as_is() {
        let nest = Arc::new(FakeSourceNest::default());
        nest.store
            .seed_list(SOURCE_BOUND_ID, vec![unenrolled_destination("dest-1")]);
        nest.store.set_not_ready(true);

        let reply = status(&nest, SOURCE_BOUND_ID);

        assert!(!reply.enrolled);
        assert!(!nest.kinds().contains(&KIND_NEST_KEY_GRANT));
    }

    /// Small helper so `all_nest_mutations_precede_the_list_write` reads as a
    /// flat loop rather than four near-identical assert blocks.
    fn assert_precedes(at: usize, put_at: usize, kind: &str, kinds: &[&'static str]) {
        assert!(
            at < put_at,
            "{kind} must precede the list write; kinds seen: {kinds:?}"
        );
    }

    /// The `writer_nest_id` the destination sees is the id the source's
    /// connection PROVED, never the source's own `fauna.nest.info` claim — a
    /// source claiming a sibling's id must not get the sibling authorized. The claim is not even asked.
    #[test]
    fn writer_grant_carries_the_bound_id_not_the_sources_claim() {
        let nest = Arc::new(FakeSourceNest {
            own_nest_id_hex: "cc".repeat(32),
            ..Default::default()
        });
        let destination = Arc::new(FakeDestinationNest::default());
        block_on(enroll_backup_destination(
            nest.clone(),
            destination.clone(),
            &nest.store,
            OWNER_SEED,
            resolved("x"),
            SOURCE_BOUND_ID,
        ))
        .expect("enroll");

        assert_eq!(
            destination
                .registered_writer_nest_id
                .lock()
                .unwrap()
                .as_deref(),
            Some(hex::encode(SOURCE_BOUND_ID).as_str()),
        );
        assert!(
            !nest.kinds().contains(&KIND_NODE_INFO),
            "the enroll never asks the source who it is"
        );
    }

    /// The source nest's own registry gets the destination's hex-encoded
    /// pubkey, URL, and mode — the fields its coordinator needs to dial out.
    #[test]
    fn destination_register_carries_the_resolved_fields() {
        let nest = Arc::new(FakeSourceNest::default());
        enroll(&nest, resolved("x")).expect("enroll");

        let registered = nest
            .registered_destination
            .lock()
            .unwrap()
            .clone()
            .expect("destination.register must be called");
        assert_eq!(registered.destination_id, "dest-1");
        assert_eq!(registered.destination_nest_url, "https://dest.example/");
        assert_eq!(registered.destination_nest_id, hex::encode([9u8; 32]));
    }

    #[test]
    fn blank_name_falls_back_to_the_resolved_domain() {
        let nest = Arc::new(FakeSourceNest::default());
        let list = enroll(&nest, resolved("   ")).expect("enroll");
        assert_eq!(list[0].display_name.as_deref(), Some("dest.example"));
    }

    /// A refused grant records nothing: the list write is never reached, so
    /// the user does not end up with a destination the nest cannot seal for.
    #[test]
    fn a_refused_grant_records_no_destination() {
        let nest = Arc::new(FakeSourceNest {
            fail_grant: true,
            ..Default::default()
        });
        let err = enroll(&nest, resolved("x")).expect_err("grant refusal propagates");
        assert!(matches!(err, EnrollError::Grant(_)), "got {err:?}");
        assert_eq!(
            nest.store.writes(),
            0,
            "no list may be written when the grant failed"
        );
        assert!(
            !nest.kinds().contains(&BACKUP_STATE_WRITE),
            "the list write must not be attempted after a refused grant"
        );
    }

    /// A refused writer-grant registration at the destination also records
    /// nothing — the source nest would otherwise think it can back this owner
    /// up to a destination that never authorized it.
    #[test]
    fn a_refused_writer_grant_records_no_destination() {
        let nest = Arc::new(FakeSourceNest::default());
        let destination = Arc::new(FakeDestinationNest {
            fail_writer_grant: true,
            ..Default::default()
        });
        let err = block_on(enroll_backup_destination(
            nest.clone(),
            destination,
            &nest.store,
            OWNER_SEED,
            resolved("x"),
            SOURCE_BOUND_ID,
        ))
        .expect_err("writer-grant refusal propagates");
        assert!(matches!(err, EnrollError::WriterGrant(_)), "got {err:?}");
        assert_eq!(
            nest.store.writes(),
            0,
            "no list may be written when the writer grant was refused"
        );
        assert!(
            nest.registered_destination.lock().unwrap().is_none(),
            "the source nest must not learn about a destination that refused the writer grant"
        );
    }

    /// Enrolling a second destination re-grants/re-registers (idempotent on
    /// the nest) and appends rather than replacing — the multi-destination
    /// shape.
    #[test]
    fn a_second_enroll_appends_and_regrants() {
        let nest = Arc::new(FakeSourceNest::default());
        enroll(&nest, resolved("first")).expect("first");

        let mut second = resolved("second");
        second.destination_id = "dest-2".into();
        second.destination_nest_url = "https://other.example/".into();
        let list = enroll(&nest, second).expect("second");

        assert_eq!(list.len(), 2);
        assert_eq!(list[1].destination_id, "dest-2");
        assert_eq!(
            nest.kinds()
                .iter()
                .filter(|k| **k == KIND_NEST_KEY_GRANT)
                .count(),
            2,
            "each enroll re-grants; the nest treats a re-grant as a replace"
        );
    }

    // ── The list's size bound ────────────────────────────────────────────────

    /// A minimal row — the smallest entry a list can hold, so a list filled
    /// with them is full for any real entry.
    fn filler(i: usize) -> BackupDestination {
        BackupDestination {
            destination_id: format!("f{i}"),
            ..Default::default()
        }
    }

    /// Seed `source_nest`'s list with `keep` and then as many minimal rows as
    /// the row's byte bound admits at the widest stamp the door checks —
    /// after this, no real entry fits.
    fn fill_to_the_brim(
        store: &FakeBackupStateStore,
        source_nest: [u8; 32],
        keep: Vec<BackupDestination>,
    ) {
        let fits = |destinations: &[BackupDestination]| {
            BackupDestinationsRow {
                source_nest,
                backup: BackupConfig {
                    destinations: destinations.to_vec(),
                },
                updated_at: fauna_core::data::Timestamp(u64::MAX),
            }
            .check_bounds()
            .is_ok()
        };
        let mut destinations = keep;
        let mut i = 0;
        loop {
            destinations.push(filler(i));
            if !fits(&destinations) {
                destinations.pop();
                break;
            }
            i += 1;
        }
        store.seed_list(source_nest, destinations);
    }

    /// **A full list refuses with the one localized refusal and writes
    /// nothing.** The row's byte bound (`config-dissolution.md` *Bounded rows*
    /// → *The backup state*) is checked before any put, and the error every
    /// app renders through `Display` is the shared i18n string — no per-app
    /// sentinel.
    #[test]
    fn an_enroll_into_a_full_list_is_refused_with_the_shared_copy_and_writes_nothing() {
        let nest = Arc::new(FakeSourceNest::default());
        fill_to_the_brim(&nest.store, SOURCE_BOUND_ID, Vec::new());
        let before = nest.store.snapshot();

        let err = enroll(&nest, resolved("one too many")).expect_err("a full list refuses");

        assert!(
            matches!(err, EnrollError::Record(BackupWriteError::ListFull)),
            "got {err:?}"
        );
        assert_eq!(
            err.to_string(),
            fauna_i18n::strings::backups::BACKUP_DESTINATIONS_FULL,
            "the refusal renders the one shared localized string"
        );
        assert_eq!(nest.store.writes(), 0, "nothing was written");
        assert_eq!(nest.store.snapshot(), before, "the rows are untouched");
    }

    /// The attach twin: a covered folder is one more entry, so it meets the
    /// same bound and the same refusal.
    #[test]
    fn an_attach_into_a_full_list_is_refused_with_the_shared_copy_and_writes_nothing() {
        let nest = Arc::new(FakeSourceNest {
            own_nest_id_hex: "dd".repeat(32),
            ..Default::default()
        });
        fill_to_the_brim(
            &nest.store,
            SOURCE_BOUND_ID,
            vec![unenrolled_destination("dest-1")],
        );
        let before = nest.store.snapshot();

        let err = block_on(attach_folder_to_destination(
            nest.clone(),
            &nest.store,
            SOURCE_BOUND_ID,
            "dest-1",
            7,
        ))
        .expect_err("a full list refuses the coverage row");

        assert!(
            matches!(err, FolderCoverageError::Record(BackupWriteError::ListFull)),
            "got {err:?}"
        );
        assert_eq!(
            err.to_string(),
            fauna_i18n::strings::backups::BACKUP_DESTINATIONS_FULL
        );
        assert_eq!(nest.store.writes(), 0);
        assert_eq!(nest.store.snapshot(), before);
    }

    // ── Post-succession adjudication, across devices ─────────────────────────

    /// Enroll `dest-1` + `dest-2`, then raise both as carried across a
    /// succession through the pass's own raise — the state the adjudication
    /// pair exists for.
    fn two_raised_destinations(nest: &Arc<FakeSourceNest>) -> ActorId {
        enroll(nest, resolved("first")).expect("enroll first");

        let mut second = resolved("second");
        second.destination_id = "dest-2".into();
        second.destination_nest_url = "https://other.example/".into();
        enroll(nest, second).expect("enroll second");

        let predecessor = ActorId([0x5e; 32]);
        assert!(
            block_on(raise_succession_destination_marks(&nest.store, predecessor))
                .expect("raise the marks")
        );
        predecessor
    }

    /// The mark `id` carries for `predecessor`, as the store holds it.
    fn mark_of(
        store: &FakeBackupStateStore,
        id: &str,
        predecessor: ActorId,
    ) -> DestinationUnattestedMark {
        store
            .marks()
            .into_iter()
            .find(|m| m.destination_id == id && m.predecessor == predecessor)
            .unwrap_or_else(|| panic!("no mark at rest for {id}; marks: {:?}", store.marks()))
    }

    /// **The cross-device property.** Two devices of one owner each *Remove*
    /// a destination they were asked to adjudicate; device A's read predates
    /// device B's removal, so A's newer list write still CARRIES B's row.
    /// There is no CAS on the plane: what keeps B's removal is that its
    /// `Removed` verdict rests in its own mark row, and the read fold prunes
    /// every list — the newer one included — against it.
    ///
    /// ⚠ Both destinations must be **raised** for this to be about the verdict
    /// plane. An ordinary (unmarked) removal records no verdict by design and
    /// rides the list's whole-record latest-wins, where the later writer simply
    /// wins — accepted behaviour, not this property.
    #[test]
    fn a_concurrent_removal_is_not_resurrected_by_this_devices_write() {
        let nest = Arc::new(FakeSourceNest::default());
        let predecessor = two_raised_destinations(&nest);

        // Device A reads first; device B's removal then rests.
        let before_race = nest.store.snapshot();
        block_on(deregister_backup_destination(
            nest.clone(),
            &nest.store,
            SOURCE_BOUND_ID,
            "dest-2",
        ))
        .expect("device B removes dest-2");
        nest.store.serve_next_read_from(before_race);

        // Device A removes dest-1 from its pre-B view.
        block_on(deregister_backup_destination(
            nest.clone(),
            &nest.store,
            SOURCE_BOUND_ID,
            "dest-1",
        ))
        .expect("device A removes dest-1");

        let raw = nest
            .store
            .lists()
            .into_iter()
            .find(|row| row.source_nest == SOURCE_BOUND_ID)
            .expect("the box's list row");
        assert_eq!(
            ids(&raw.backup.destinations),
            ["dest-2"],
            "precondition: A's newer list write carries B's removed row — the \
             prune below is what stops it"
        );
        let state = nest.store.state(SOURCE_BOUND_ID);
        assert!(
            state.backup.destinations.is_empty(),
            "device B's removal was resurrected by device A's newer write — the \
             owner adjudicated this destination away and it is back. rows: {:?}",
            ids(&state.backup.destinations)
        );
        // And the verdicts are both at rest: a row that came back *clean* is
        // the failure the `Removed` prune exists to make impossible.
        for id in ["dest-1", "dest-2"] {
            assert_eq!(
                mark_of(&nest.store, id, predecessor).verdict,
                UnattestedVerdict::Removed,
                "{id}'s removal must be recorded as the owner's verdict, not \
                 merely applied — the verdict is what prunes the next list"
            );
        }
    }

    /// The Keep twin of the removal property above: a *Keep* on this device
    /// must not be undone by a concurrent device's removal, and must not undo
    /// it either. Both verdicts are the owner's answers to different
    /// questions, each in its own mark row.
    #[test]
    fn a_keep_and_a_concurrent_removal_both_survive() {
        let nest = Arc::new(FakeSourceNest::default());
        let predecessor = two_raised_destinations(&nest);

        let before_race = nest.store.snapshot();
        block_on(deregister_backup_destination(
            nest.clone(),
            &nest.store,
            SOURCE_BOUND_ID,
            "dest-2",
        ))
        .expect("device B removes dest-2");
        nest.store.serve_next_read_from(before_race);

        let adjudicated = block_on(keep_backup_destination_at_rest(
            &nest.store,
            SOURCE_BOUND_ID,
            "dest-1",
        ))
        .expect("device A keeps dest-1");
        assert!(adjudicated, "dest-1 was raised, so Keep must answer it");

        assert_eq!(
            mark_of(&nest.store, "dest-1", predecessor).verdict,
            UnattestedVerdict::Kept,
            "the Keep must survive the concurrent write — an adjudication that \
             loses a race re-asks the owner a question they already answered"
        );
        let state = nest.store.state(SOURCE_BOUND_ID);
        assert_eq!(
            ids(&state.backup.destinations),
            ["dest-1"],
            "Keep closes the raising event and does not retire the row; the other \
             device's Remove still holds"
        );
    }

    /// **An unrelated gesture must not resurrect a removal either.** Enrolling
    /// a destination also writes the list — the very record a `Removed`
    /// verdict prunes — so an ordinary "add my new backup box" on one device,
    /// from a view predating a sibling's adjudication, writes a newer list
    /// that still carries the destination the owner just removed.
    ///
    /// ⚠ **What this pins is the removal, NOT that both writes survive.** The
    /// list is whole-record latest-wins; the prune is unconditional, so an
    /// adjudicated-away row cannot come back whichever list wins.
    #[test]
    fn a_concurrent_removal_is_not_resurrected_by_an_unrelated_enrollment() {
        let nest = Arc::new(FakeSourceNest::default());
        let predecessor = two_raised_destinations(&nest);

        let before_race = nest.store.snapshot();
        block_on(deregister_backup_destination(
            nest.clone(),
            &nest.store,
            SOURCE_BOUND_ID,
            "dest-2",
        ))
        .expect("device B removes dest-2");
        nest.store.serve_next_read_from(before_race);

        // Device A, meanwhile, adds a new backup box from its pre-B view.
        let mut third = resolved("third box");
        third.destination_id = "dest-3".into();
        third.destination_nest_url = "https://third.example/".into();
        let list = enroll(&nest, third).expect("device A enrolls dest-3");

        assert!(
            !list.iter().any(|d| d.destination_id == "dest-2"),
            "the destination device B adjudicated away came back because an \
             unrelated enrollment wrote its pre-race view — and a resurrected \
             row carries no open mark, so the owner is never asked again about a \
             box a seed thief could have planted. destinations: {:?}",
            ids(&list)
        );
        assert_eq!(ids(&list), ["dest-1", "dest-3"]);
        assert_eq!(
            mark_of(&nest.store, "dest-2", predecessor).verdict,
            UnattestedVerdict::Removed,
            "B's Removed verdict must be at rest — it is what keeps the prune firing"
        );
    }

    /// A Keep on a row with nothing open writes nothing — the door writes
    /// only the marks a gesture moved.
    #[test]
    fn a_keep_with_nothing_open_writes_nothing() {
        let nest = Arc::new(FakeSourceNest::default());
        two_raised_destinations(&nest);
        assert!(
            block_on(keep_backup_destination_at_rest(
                &nest.store,
                SOURCE_BOUND_ID,
                "dest-1"
            ))
            .expect("first keep"),
            "the first Keep answers the open mark"
        );
        let writes = nest.store.writes();
        let after_first = nest.store.snapshot();

        assert!(
            !block_on(keep_backup_destination_at_rest(
                &nest.store,
                SOURCE_BOUND_ID,
                "dest-1"
            ))
            .expect("second keep"),
            "a Keep on an already-answered row reports no adjudication"
        );
        assert_eq!(nest.store.writes(), writes, "and it must not have written");
        assert_eq!(nest.store.snapshot(), after_first);
    }

    #[test]
    fn deregister_removes_from_the_source_registry_then_drops_the_list_row() {
        let nest = Arc::new(FakeSourceNest::default());
        enroll(&nest, resolved("x")).expect("enroll");

        let list = block_on(deregister_backup_destination(
            nest.clone(),
            &nest.store,
            SOURCE_BOUND_ID,
            "dest-1",
        ))
        .expect("deregister");

        assert!(list.is_empty(), "the destination row must be dropped");
        assert_eq!(
            nest.removed_destination_id.lock().unwrap().as_deref(),
            Some("dest-1"),
        );

        let kinds = nest.kinds();
        let remove_at = kinds
            .iter()
            .rposition(|k| *k == KIND_DESTINATION_REMOVE)
            .expect("destination.remove must be called");
        let put_at = kinds
            .iter()
            .rposition(|k| *k == BACKUP_STATE_WRITE)
            .expect("the list must be written");
        assert!(
            remove_at < put_at,
            "destination.remove must precede the list write; kinds seen: {kinds:?}"
        );
    }

    /// A refused source-registry deregister leaves the list row in place —
    /// the source nest must not silently keep acting on a destination the
    /// client believes it removed.
    #[test]
    fn a_refused_destination_remove_leaves_the_list_row() {
        let nest = Arc::new(FakeSourceNest::default());
        enroll(&nest, resolved("x")).expect("enroll");

        // A second connection over the same account store, this time refusing
        // the destination-remove call (e.g. a since-revoked capability).
        let removing = Arc::new(FakeSourceNest {
            fail_destination_remove: true,
            ..FakeSourceNest::sharing(&nest.store)
        });
        let err = block_on(deregister_backup_destination(
            removing.clone(),
            &removing.store,
            SOURCE_BOUND_ID,
            "dest-1",
        ))
        .expect_err("remove refusal propagates");
        assert!(
            matches!(err, DeregisterError::DestinationRemove(_)),
            "got {err:?}"
        );
        assert!(
            !removing.kinds().contains(&BACKUP_STATE_WRITE),
            "the list write must not be attempted after a refused destination remove"
        );
        assert_eq!(
            ids(&nest.store.state(SOURCE_BOUND_ID).backup.destinations),
            ["dest-1"]
        );
    }

    // ---- the client-device custodian's three-step enrollment (slice 3c) ----

    fn custodian(name: &str, cap: Option<u64>) -> CustodianEnrollment {
        CustodianEnrollment {
            destination_id: "cust-1".into(),
            custodian_device_id: "device-abc".into(),
            display_name: name.into(),
            capacity_cap_bytes: cap,
        }
    }

    fn enroll_custodian(
        nest: &Arc<FakeSourceNest>,
        enrollment: CustodianEnrollment,
    ) -> Result<Vec<BackupDestination>, CustodianEnrollError<FakeError>> {
        block_on(enroll_client_custodian(
            nest.clone(),
            &nest.store,
            SOURCE_BOUND_ID,
            enrollment,
        ))
    }

    #[test]
    fn enrolling_this_device_records_a_drivable_client_device_row() {
        let nest = Arc::new(FakeSourceNest::default());
        let list = enroll_custodian(&nest, custodian("My iPad", Some(2 << 40)))
            .expect("custodian enroll succeeds");

        assert_eq!(list.len(), 1);
        let dest = &list[0];
        assert_eq!(dest.kind, DESTINATION_KIND_CLIENT_DEVICE);
        assert_eq!(dest.custodian_device_id.as_deref(), Some("device-abc"));
        assert_eq!(dest.capacity_cap_bytes, Some(2 << 40));
        assert_eq!(dest.display_name.as_deref(), Some("My iPad"));
        assert_eq!(dest.folder_name, DEFAULT_BACKUP_FOLDER);

        // The row must project as a *drivable* custodian, not `Inert`. This is
        // the assertion that would have caught a missing device id or a kind
        // typo — both of which write a row the user sees in their destination
        // list and nothing can ever act on.
        assert!(
            matches!(
                dest.kind_view(),
                fauna_core::data::DestinationKind::ClientDevice { device_id, capacity_cap_bytes }
                    if device_id == "device-abc" && capacity_cap_bytes == Some(2 << 40)
            ),
            "enrollment wrote a row that projects as {:?}",
            dest.kind_view()
        );

        // The registration the nest keys its status projection on.
        let reg = nest.registered_destination.lock().unwrap().clone();
        let reg = reg.expect("the source nest was never told about this device");
        assert_eq!(reg.kind, DESTINATION_KIND_CLIENT_DEVICE);
        assert_eq!(reg.custodian_device_id.as_deref(), Some("device-abc"));
        assert!(
            reg.destination_nest_url.is_empty(),
            "a custodian has no address; sending one would invite the nest to dial it"
        );
    }

    /// No `NestBackupKey` grant — the ratified difference from the nest kind
    /// (`behavior/backup-destinations.md` § Third destination kind: the grant exists to let a
    /// *nest* seal on the owner's behalf; a custodian seals for itself).
    /// Granting anyway would hand the source nest a key it has no need for.
    #[test]
    fn a_custodian_enrollment_grants_the_source_nest_nothing() {
        let nest = Arc::new(FakeSourceNest::default());
        enroll_custodian(&nest, custodian("", None)).expect("enroll");

        assert!(
            nest.granted_key.lock().unwrap().is_none(),
            "a pull-only custodian must not trigger a NestBackupKey grant"
        );
        assert_eq!(
            nest.kinds(),
            [
                KIND_DESTINATION_REGISTER,
                BACKUP_STATE_READ,
                BACKUP_STATE_WRITE
            ],
            "the three-step enrollment grew a call"
        );
    }

    /// A configured client-device row, as the heal below finds it in the list.
    fn custodian_row_in_list(cap: Option<u64>) -> BackupDestination {
        BackupDestination {
            destination_id: "cust-1".into(),
            kind: DESTINATION_KIND_CLIENT_DEVICE.to_string(),
            custodian_device_id: Some("device-abc".into()),
            capacity_cap_bytes: cap,
            ..Default::default()
        }
    }

    /// ⚠ The unenrolled heal must re-register each row **through its own
    /// kind's shape**.
    ///
    /// This is the failure it prevents, and it is silent: a custodian-only owner
    /// grants no `NestBackupKey`, so `fauna.backup.status` answers
    /// `enrolled: false`, so `read_backup_status` always reaches the heal. If the
    /// heal then sent every row through the nest-shaped register — whose
    /// arguments are `destination_nest_url` and `destination_actor_pubkey`, both
    /// resting at empty/zero sentinels on a custodian row — the registry row
    /// would come back as `kind: "nest"` with both per-kind columns dropped.
    /// `custodian_assignment_for` would then find no row for the device, and a
    /// correctly-enrolled custodian would go **silently unhosted**: the heal
    /// breaking precisely what it ran to repair.
    #[test]
    fn the_heal_re_registers_a_custodian_through_its_own_kind_never_as_a_nest() {
        let nest = Arc::new(FakeSourceNest::default());
        let n = block_on(reconcile_backup_enrollment(
            nest.clone(),
            OWNER_SEED,
            &[custodian_row_in_list(Some(64 << 30))],
        ))
        .expect("heal");
        assert_eq!(n, 1);

        let reg = nest
            .registered_destination
            .lock()
            .unwrap()
            .clone()
            .expect("the heal must re-register the row");
        assert_eq!(
            reg.kind, DESTINATION_KIND_CLIENT_DEVICE,
            "the heal must not rewrite a custodian into a nest destination"
        );
        assert_eq!(reg.custodian_device_id.as_deref(), Some("device-abc"));
        assert_eq!(
            reg.capacity_cap_bytes,
            Some(64 << 30),
            "the cap rides the registry row — it is what the custodian's own \
             bearer-only host reads back to learn its cap"
        );
    }

    /// The grant is for nest destinations. An owner whose only destination is
    /// their own device must not be handed a `NestBackupKey` as a side effect of
    /// walking the heal — it would back nothing up
    /// (`behavior/backup-destinations.md` § Third destination kind: a custodian seals for itself).
    #[test]
    fn the_heal_grants_no_nest_key_to_a_custodian_only_owner() {
        let nest = Arc::new(FakeSourceNest::default());
        block_on(reconcile_backup_enrollment(
            nest.clone(),
            OWNER_SEED,
            &[custodian_row_in_list(None)],
        ))
        .expect("heal");
        assert!(
            nest.granted_key.lock().unwrap().is_none(),
            "a custodian-only owner needs no NestBackupKey"
        );
    }

    /// ...but an owner who *does* have a peer nest still gets one: the two kinds
    /// coexist in one list, and the nest half must keep working.
    #[test]
    fn the_heal_still_grants_when_a_peer_nest_destination_is_configured() {
        let nest = Arc::new(FakeSourceNest::default());
        block_on(reconcile_backup_enrollment(
            nest.clone(),
            OWNER_SEED,
            &[
                custodian_row_in_list(None),
                BackupDestination {
                    destination_id: "dest-1".into(),
                    destination_nest_url: "https://dest.example/".into(),
                    kind: DESTINATION_KIND_NEST.to_string(),
                    ..Default::default()
                },
            ],
        ))
        .expect("heal");
        assert!(
            nest.granted_key.lock().unwrap().is_some(),
            "a configured peer nest still needs the seal grant"
        );
    }

    /// The registration precedes the list write, for the same reason the nest
    /// sequence's four mutations do: a crash between them leaves one inert
    /// idempotent registry row, never a configured destination the nest has
    /// never heard of.
    #[test]
    fn a_refused_registration_records_no_destination() {
        let nest = Arc::new(FakeSourceNest {
            fail_destination_register: true,
            ..Default::default()
        });
        let err = enroll_custodian(&nest, custodian("My iPad", None))
            .expect_err("a refused registration must not enroll");
        assert!(
            matches!(err, CustodianEnrollError::DestinationRegister(_)),
            "got {err:?}"
        );
        assert!(
            !nest.kinds().contains(&BACKUP_STATE_WRITE),
            "the list write must not be attempted after a refused registration"
        );
    }

    /// **Mutation-verified rule.** A blank device id is refused before any call.
    /// Writing the row anyway is the failure that hides: `kind_view()` sends a
    /// device-id-less `client-device` row to `Inert`, so the user would see a
    /// destination in their Backups list that no pull pass can ever drive and
    /// no status row can ever describe.
    #[test]
    fn a_device_with_no_sync_id_is_refused_before_anything_is_written() {
        let nest = Arc::new(FakeSourceNest::default());
        let err = enroll_custodian(
            &nest,
            CustodianEnrollment {
                custodian_device_id: "   ".into(),
                ..custodian("My iPad", None)
            },
        )
        .expect_err("an id-less device must not enroll");
        assert!(
            matches!(err, CustodianEnrollError::MissingDeviceId),
            "got {err:?}"
        );
        assert!(
            nest.kinds().is_empty(),
            "nothing at all may be called for an unenrollable device, got {:?}",
            nest.kinds()
        );
    }

    /// A nameless row in the destination list is a usability failure the shell
    /// cannot fix later — the id is a poor label but an honest one.
    #[test]
    fn a_blank_display_name_falls_back_to_the_device_id() {
        let nest = Arc::new(FakeSourceNest::default());
        let list = enroll_custodian(&nest, custodian("  ", None)).expect("enroll");
        assert_eq!(list[0].display_name.as_deref(), Some("device-abc"));
    }

    // ---- the re-seed's post-ceremony re-enrollment ----

    fn reseed_outcome(whole: bool) -> fauna_client_backup::reseed::ReseedOutcome {
        use fauna_client_backup::reseed::{
            DeliveredCorpus, DeliveredSet, ReseedOutcome, SetAxis, SetOutcome, SetRefusal,
            SetResult,
        };
        let mail = DeliveredSet {
            set_name: "__mail".into(),
            axis: SetAxis::Segment,
            folder_display_name: None,
            folder_label: None,
        };
        let outcome = if whole {
            SetOutcome::Materialized {
                segments: vec![0],
                records: 1,
            }
        } else {
            SetOutcome::Refused {
                refusal: SetRefusal::CustodyIncomplete,
                detail: "short".into(),
            }
        };
        ReseedOutcome {
            delivered: DeliveredCorpus {
                sets: vec![mail.clone()],
                ..DeliveredCorpus::default()
            },
            sets: vec![SetResult { set: mail, outcome }],
        }
    }

    /// A row the list still has for this device is re-registered as itself —
    /// same id, name and cap — so the registry step is the idempotent one.
    #[test]
    fn a_reenrollment_keeps_this_devices_own_row() {
        let mut other = custodian_row_in_list(Some(5));
        other.destination_id = "someone-else".into();
        other.custodian_device_id = Some("device-zzz".into());
        let mut mine = custodian_row_in_list(Some(9 << 30));
        mine.display_name = Some("Laptop".into());
        let got = custodian_reenrollment(&[other, mine], " device-abc ", || {
            panic!("a row that already names this device must not mint a new id")
        });
        assert_eq!(got.destination_id, "cust-1");
        assert_eq!(got.custodian_device_id, "device-abc");
        assert_eq!(got.display_name, "Laptop");
        assert_eq!(got.capacity_cap_bytes, Some(9 << 30));
    }

    /// After a box loss the rebuilt nest's list is empty: a fresh row, with the
    /// enrollment's own defaults (a blank name falls back to the device id).
    #[test]
    fn a_reenrollment_with_no_row_of_its_own_mints_a_fresh_one() {
        let got = custodian_reenrollment(&[], "device-abc", || "fresh-id".into());
        assert_eq!(got.destination_id, "fresh-id");
        assert_eq!(got.custodian_device_id, "device-abc");
        assert_eq!(got.display_name, "");
        assert_eq!(got.capacity_cap_bytes, None);
    }

    #[test]
    fn a_whole_reseed_reenrolls_this_device() {
        let nest = Arc::new(FakeSourceNest::default());
        let list = block_on(reenroll_custodian_after_reseed(
            nest.clone(),
            &nest.store,
            SOURCE_BOUND_ID,
            &reseed_outcome(true),
            "device-abc",
            &[],
            || "fresh-id".into(),
        ))
        .expect("a whole outcome re-enrolls")
        .expect("the enrollment succeeds");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].destination_id, "fresh-id");
        assert_eq!(list[0].custodian_device_id.as_deref(), Some("device-abc"));
    }

    /// The ordering rule: a part-restored nest would read the missing part as
    /// deleted to a custodian that pulls from it, so nothing is called at all.
    #[test]
    fn a_part_restored_reseed_reenrolls_nothing() {
        let nest = Arc::new(FakeSourceNest::default());
        let got = block_on(reenroll_custodian_after_reseed(
            nest.clone(),
            &nest.store,
            SOURCE_BOUND_ID,
            &reseed_outcome(false),
            "device-abc",
            &[],
            || panic!("nothing is minted for a part-restored nest"),
        ));
        assert!(got.is_none());
        assert!(
            nest.kinds().is_empty(),
            "a part-restored nest must see no enrollment call"
        );
    }

    // ---- the nest-held pull-back's post-ceremony re-enrollment ----

    /// The destination the corpus was pulled back from, as the lost box's list
    /// holds it.
    fn pulled_from() -> BackupDestination {
        BackupDestination {
            display_name: Some("Off-site".into()),
            ..unenrolled_destination("dest-1")
        }
    }

    fn reenroll_nest(
        nest: &Arc<FakeSourceNest>,
        destination: &Arc<FakeDestinationNest>,
        whole: bool,
    ) -> Option<Result<Vec<BackupDestination>, EnrollError<FakeError, FakeError>>> {
        block_on(reenroll_nest_destination_after_reseed(
            nest.clone(),
            destination.clone(),
            &nest.store,
            OWNER_SEED,
            SOURCE_BOUND_ID,
            &pulled_from(),
            &reseed_outcome(whole),
        ))
    }

    /// **A restore onto a fresh-identity nest carries the destination's seat
    /// from the box it replaced** (`segment-backup-protocol.md` § *The writer
    /// seat*, situation (2)): the seat still names the lost box, so the
    /// re-enrollment reads that holder off the seat and names it as
    /// `succeeds` — and the restored nest's list records the destination.
    #[test]
    fn a_whole_pull_back_onto_a_fresh_identity_takes_the_seat_from_the_lost_box() {
        let nest = Arc::new(FakeSourceNest::default());
        let lost = "ee".repeat(32);
        let destination = Arc::new(FakeDestinationNest {
            seat: Mutex::new(Some(lost.clone())),
            ..Default::default()
        });
        let list = reenroll_nest(&nest, &destination, true)
            .expect("a whole outcome re-enrolls")
            .expect("the seat moves and the row lands");

        let restored = hex::encode(SOURCE_BOUND_ID);
        assert_eq!(*destination.succeeds.lock().unwrap(), vec![Some(lost)]);
        assert_eq!(
            destination.seat.lock().unwrap().as_deref(),
            Some(restored.as_str()),
            "the seat now names the restored nest alone"
        );
        assert!(nest.granted_key.lock().unwrap().is_some());
        assert_eq!(
            nest.registered_destination
                .lock()
                .unwrap()
                .as_ref()
                .map(|r| r.destination_id.as_str()),
            Some("dest-1")
        );
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].destination_id, "dest-1");
        assert_eq!(list[0].folder_name, DEFAULT_BACKUP_FOLDER);
        assert_eq!(list[0].display_name.as_deref(), Some("Off-site"));
    }

    /// A rebuild under the lost box's own identity finds the seat already its
    /// own: a plain refresh, never a `succeeds` naming itself.
    #[test]
    fn a_same_identity_rebuild_refreshes_its_own_seat() {
        let nest = Arc::new(FakeSourceNest::default());
        let destination = Arc::new(FakeDestinationNest {
            seat: Mutex::new(Some(hex::encode(SOURCE_BOUND_ID))),
            ..Default::default()
        });
        reenroll_nest(&nest, &destination, true)
            .expect("re-enrolls")
            .expect("succeeds");
        assert_eq!(*destination.succeeds.lock().unwrap(), vec![None]);
    }

    /// A part-restored nest re-enrolls nothing: its first backup pass would
    /// overwrite the destination's whole copy with the restore's hole.
    #[test]
    fn a_part_restored_pull_back_reenrolls_no_destination() {
        let nest = Arc::new(FakeSourceNest::default());
        let destination = Arc::new(FakeDestinationNest {
            seat: Mutex::new(Some("ee".repeat(32))),
            ..Default::default()
        });
        assert!(reenroll_nest(&nest, &destination, false).is_none());
        assert!(nest.kinds().is_empty());
        assert!(destination.kinds.lock().unwrap().is_empty());
    }

    /// Removal is kind-agnostic: the existing shared deregister drops a
    /// custodian row exactly as it drops a nest row, so no shell needs a second
    /// remove path. (What it deliberately does **not** do is delete this
    /// device's local sealed store — that corpus is the owner's only offline
    /// copy, so reclaiming it is a separate, explicit gesture.)
    #[test]
    fn deregister_removes_a_custodian_row_like_any_other() {
        let nest = Arc::new(FakeSourceNest::default());
        enroll_custodian(&nest, custodian("My iPad", None)).expect("enroll");

        let removing = Arc::new(FakeSourceNest::sharing(&nest.store));
        let list = block_on(deregister_backup_destination(
            removing.clone(),
            &removing.store,
            SOURCE_BOUND_ID,
            "cust-1",
        ))
        .expect("deregister");
        assert!(
            list.is_empty(),
            "the custodian row survived removal: {list:?}"
        );
    }

    // ── ordinary-folder coverage (attach/detach) ──────────────────────────────

    /// Attach calls the nest first, then writes the list, and the coverage
    /// row's `folder_name` is the **reply's** set name verbatim — a hex this
    /// client never derived (the naming rule lives on the nest).
    #[test]
    fn attach_calls_the_nest_first_and_records_the_replied_set_name() {
        let nest = Arc::new(FakeSourceNest {
            own_nest_id_hex: "dd".repeat(32),
            ..Default::default()
        });
        nest.store
            .seed_list(SOURCE_BOUND_ID, vec![unenrolled_destination("dest-1")]);

        let attach = || {
            block_on(attach_folder_to_destination(
                nest.clone(),
                &nest.store,
                SOURCE_BOUND_ID,
                "dest-1",
                7,
            ))
        };
        let destinations = attach().expect("attach");

        let kinds = nest.kinds();
        let attach_at = kinds
            .iter()
            .position(|k| *k == KIND_DESTINATION_ATTACH_FOLDER)
            .expect("attach kind called");
        let put_at = kinds
            .iter()
            .position(|k| *k == BACKUP_STATE_WRITE)
            .expect("list written");
        assert_precedes(attach_at, put_at, KIND_DESTINATION_ATTACH_FOLDER, &kinds);

        let expected_set = format!("__folder/{}/7", "dd".repeat(32));
        let coverage = destinations
            .iter()
            .find(|d| d.folder_name == expected_set)
            .expect("the coverage row exists");
        assert_eq!(coverage.destination_id, "dest-1");
        // Identity fields clone from the enrolled row, never from thin air.
        assert_eq!(coverage.destination_nest_url, "https://dest-1.example/");
        assert_eq!(coverage.destination_actor_pubkey, [9u8; 32]);
        // The original per-rail row survives untouched.
        assert!(
            destinations
                .iter()
                .any(|d| d.folder_name == DEFAULT_BACKUP_FOLDER)
        );
        // The folder's name, off the owner's coverage listing — what the
        // nest-held pull-back names the restored folder from after a box loss.
        assert_eq!(coverage.folder_display_name.as_deref(), Some("Photos"));
        assert_eq!(
            coverage.folder_label,
            Some(fauna_core::data::CoveredFolderLabel {
                name_hash: [0x4E; 32],
                name_sealed: b"sealed".to_vec(),
            })
        );
        // The enrollment row is not a folder's and carries no folder name.
        let enrolled = destinations
            .iter()
            .find(|d| d.folder_name == DEFAULT_BACKUP_FOLDER)
            .unwrap();
        assert_eq!(
            (&enrolled.folder_display_name, &enrolled.folder_label),
            (&None, &None)
        );
        // A re-run converges rather than duplicating (both halves idempotent).
        let destinations = attach().expect("re-attach");
        assert_eq!(
            destinations
                .iter()
                .filter(|d| d.folder_name == expected_set)
                .count(),
            1
        );
    }

    /// **A re-attach refreshes the folder's name, and a listing that carries
    /// none erases nothing** — the custodian store's rule for the same label
    /// (`segment-backup-protocol.md` § *Where a restored folder's name comes
    /// from*): a renamed folder's next attach records the new name, and a set
    /// sealed past its plaintext name keeps the last name this row learned.
    #[test]
    fn a_re_attach_refreshes_the_folders_name_and_an_unnamed_listing_erases_none() {
        let nest = Arc::new(FakeSourceNest::default());
        nest.store
            .seed_list(SOURCE_BOUND_ID, vec![unenrolled_destination("dest-1")]);
        let attach = || {
            block_on(attach_folder_to_destination(
                nest.clone(),
                &nest.store,
                SOURCE_BOUND_ID,
                "dest-1",
                7,
            ))
            .expect("attach")
        };
        let name_of = |list: &[BackupDestination]| {
            list.iter()
                .find(|d| d.folder_name.ends_with("/7"))
                .and_then(|d| d.folder_display_name.clone())
        };
        assert_eq!(name_of(&attach()).as_deref(), Some("Photos"));

        *nest.listed_folder_name.lock().unwrap() = Some("Pictures".to_string());
        assert_eq!(name_of(&attach()).as_deref(), Some("Pictures"));

        *nest.listed_folder_name.lock().unwrap() = None;
        assert_eq!(
            name_of(&attach()).as_deref(),
            Some("Pictures"),
            "a listing with no plaintext name keeps the recorded one"
        );
    }

    /// A list that has lost the destination's enrolled row gains no orphan
    /// coverage row — there is no identity to clone, and a row invented from
    /// nothing would make one "destination" dial two nests.
    #[test]
    fn attach_without_an_enrolled_row_writes_no_coverage() {
        let nest = Arc::new(FakeSourceNest::default());
        let destinations = block_on(attach_folder_to_destination(
            nest.clone(),
            &nest.store,
            SOURCE_BOUND_ID,
            "dest-ghost",
            3,
        ))
        .expect("attach call itself succeeds");
        assert!(destinations.is_empty(), "no template row ⇒ nothing written");
        assert_eq!(nest.store.writes(), 0);
    }

    /// Detach removes exactly the `(destination, folder_set)` row — the
    /// destination's other rows survive, and an OPEN unattested mark stays
    /// open: a per-folder detach is not an adjudication verdict about the
    /// destination (the trap `remove_backup_destination` would spring).
    #[test]
    fn detach_removes_only_the_coverage_row_and_records_no_verdict() {
        let nest = Arc::new(FakeSourceNest {
            own_nest_id_hex: "ee".repeat(32),
            ..Default::default()
        });
        let folder_set = format!("__folder/{}/9", "ee".repeat(32));
        // Seed the enrolled row, its coverage row, and an open mark under review.
        let mut seeded = BackupState::default();
        add_backup_destination(&mut seeded, unenrolled_destination("dest-1"));
        assert!(attach_backup_destination_folder(
            &mut seeded,
            "dest-1",
            &folder_set,
            &FolderCoverageLabel::default()
        ));
        nest.store
            .seed_list(SOURCE_BOUND_ID, seeded.backup.destinations);
        nest.store.seed_marks(&[DestinationUnattestedMark {
            destination_id: "dest-1".into(),
            predecessor: ActorId([3u8; 32]),
            verdict: UnattestedVerdict::Open,
        }]);

        let destinations = block_on(detach_folder_from_destination(
            nest.clone(),
            &nest.store,
            SOURCE_BOUND_ID,
            "dest-1",
            9,
            &folder_set,
        ))
        .expect("detach");

        assert!(
            destinations.iter().all(|d| d.folder_name != folder_set),
            "the coverage row is gone"
        );
        assert!(
            destinations
                .iter()
                .any(|d| d.destination_id == "dest-1" && d.folder_name == DEFAULT_BACKUP_FOLDER),
            "the destination's own enrolled row survives a per-folder detach"
        );
        let detached = nest.detached_folder.lock().unwrap().clone();
        assert_eq!(
            detached.map(|d| (d.destination_id, d.folder_id)).unwrap(),
            ("dest-1".to_string(), 9)
        );
        // The verdict property: the destination's review stays open.
        assert!(
            nest.store
                .marks()
                .iter()
                .any(|m| m.destination_id == "dest-1" && m.verdict.is_open()),
            "a per-folder detach must not record a Removed verdict"
        );
    }

    // ── The rotated-box re-file ─────────────────────────

    /// Box P's key, rotated to box B's.
    fn rotated_keys() -> (ActorKeypair, ActorKeypair) {
        (
            ActorKeypair::from_secret([0x11; 32]),
            ActorKeypair::from_secret([0x22; 32]),
        )
    }

    /// The signed hop `old → new`, minted with both real keys exactly as the
    /// rotation transaction mints it.
    fn hop(
        old: &ActorKeypair,
        new: &ActorKeypair,
        seq: u64,
    ) -> fauna_protocol::nest_rotation::SignedNestRotation {
        NestRotation {
            old_nest_actor_id: old.actor_id().0,
            new_nest_actor_id: new.actor_id().0,
            seq,
            rotated_at: 1_800_000_000 + seq as i64,
        }
        .sign(old.signing_key(), new.signing_key())
        .expect("sign the hop")
    }

    /// **A rotated box keeps its list** (`backup-destinations.md`
    /// § *Destination data model*): a device bound to the new identity B,
    /// finding no list under B but P's list under P, fetches B's chain,
    /// verifies the hop P → B, and re-files P's list under B — so the Backups
    /// page's first read after the rotation renders it.
    #[test]
    fn a_verified_rotation_re_files_the_predecessors_list_under_the_new_box() {
        let (p, b) = rotated_keys();
        let nest = Arc::new(FakeSourceNest::default());
        nest.store
            .seed_list(p.actor_id().0, vec![unenrolled_destination("dest-1")]);
        *nest.rotation_chain.lock().unwrap() = Some(RotationChainReply {
            chain: vec![hop(&p, &b, 1)],
            ..Default::default()
        });

        let state = block_on(load_backup_state_refiled(
            &nest.store,
            &nest,
            b.actor_id().0,
        ))
        .expect("the page read");

        assert_eq!(ids(&state.backup.destinations), ["dest-1"]);
        assert!(nest.kinds().contains(&ROTATION_CHAIN_KIND));
        assert_eq!(
            ids(&nest.store.state(b.actor_id().0).backup.destinations),
            ["dest-1"],
            "the list now rests under the new identity"
        );
        // A second read finds B's row and asks no chain again.
        nest.kinds.lock().unwrap().clear();
        block_on(load_backup_state_refiled(
            &nest.store,
            &nest,
            b.actor_id().0,
        ))
        .expect("the second read");
        assert!(!nest.kinds().contains(&ROTATION_CHAIN_KIND));
    }

    /// **The re-file rewrites a covered folder's `folder_name`**
    /// (`backup-destinations.md` § *A rotated box keeps its list*): the
    /// re-filed list's coverage rows name the successor's set, in the same
    /// write — and a row naming a box no verified chain reaches is copied as
    /// it is.
    #[test]
    fn the_re_filed_lists_covered_folder_rows_name_the_successors_set() {
        let (p, b) = rotated_keys();
        let nest = Arc::new(FakeSourceNest::default());
        let mut seeded = BackupState::default();
        add_backup_destination(&mut seeded, unenrolled_destination("dest-1"));
        let ours = fauna_core::data::folder_backup_set_name(&p.actor_id().0, 9);
        let foreign = fauna_core::data::folder_backup_set_name(&OTHER_BOX_ID, 4);
        assert!(attach_backup_destination_folder(
            &mut seeded,
            "dest-1",
            &ours,
            &FolderCoverageLabel::default()
        ));
        assert!(attach_backup_destination_folder(
            &mut seeded,
            "dest-1",
            &foreign,
            &FolderCoverageLabel::default()
        ));
        nest.store
            .seed_list(p.actor_id().0, seeded.backup.destinations);
        *nest.rotation_chain.lock().unwrap() = Some(RotationChainReply {
            chain: vec![hop(&p, &b, 1)],
            ..Default::default()
        });

        assert!(block_on(refile_rotated_box_list(
            &nest.store,
            &nest,
            b.actor_id().0
        )));

        let names: Vec<String> = nest
            .store
            .state(b.actor_id().0)
            .backup
            .destinations
            .into_iter()
            .map(|d| d.folder_name)
            .collect();
        assert_eq!(
            names,
            [
                DEFAULT_BACKUP_FOLDER.to_string(),
                fauna_core::data::folder_backup_set_name(&b.actor_id().0, 9),
                foreign,
            ],
        );
        assert_eq!(nest.store.writes(), 1, "one write carries the rename");
    }

    /// A list under an identity no verified hop links to the bound box is
    /// another box's, and stays that box's.
    #[test]
    fn an_unrelated_boxs_list_is_not_re_filed() {
        let (p, b) = rotated_keys();
        let nest = Arc::new(FakeSourceNest::default());
        nest.store
            .seed_list(OTHER_BOX_ID, vec![unenrolled_destination("elsewhere")]);
        *nest.rotation_chain.lock().unwrap() = Some(RotationChainReply {
            chain: vec![hop(&p, &b, 1)],
            ..Default::default()
        });

        assert!(!block_on(refile_rotated_box_list(
            &nest.store,
            &nest,
            b.actor_id().0
        )));
        assert_eq!(nest.store.writes(), 0);
        assert!(
            nest.store
                .state(b.actor_id().0)
                .backup
                .destinations
                .is_empty()
        );
    }

    /// A chain the bound nest cannot serve re-files nothing — never a guess.
    #[test]
    fn a_chain_fetch_failure_re_files_nothing() {
        let (p, b) = rotated_keys();
        let nest = Arc::new(FakeSourceNest::default());
        nest.store
            .seed_list(p.actor_id().0, vec![unenrolled_destination("dest-1")]);
        // `rotation_chain` stays `None`: the call fails.

        assert!(!block_on(refile_rotated_box_list(
            &nest.store,
            &nest,
            b.actor_id().0
        )));
        assert!(nest.kinds().contains(&ROTATION_CHAIN_KIND));
        assert_eq!(nest.store.writes(), 0);
    }
}
