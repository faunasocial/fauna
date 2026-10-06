//! User-class nest-side segment-backup grant plane
//! (`fauna.backup.nest_key.{grant,revoke}` + `fauna.backup.status`) —
//! nest-side segment backup slice 2 (design tracked internally).
//!
//! Each handler derives the owning actor from the authenticated connection
//! (`actor_id`), never a wire param, so a caller can only grant / revoke / read
//! its **own** `NestBackupKey` — the store is keyed on that actor. The nest
//! holds the granted key plaintext because it seals the owner's own segment
//! files with it (design record § Trust-domain analysis;
//! `key-material-hierarchy.md` § Path A-sibling-0). The DB layer
//! (`crate::db::nest_backup_keys`) owns the SQL; this layer owns
//! decode → gate → call-DB → encode.
//!
//! `status` reports the enrolled flag plus one row per registered destination,
//! projected by the in-process coordinator (`crate::segment_backup`) from the
//! same local state its passes advance — the uniform 7-client Backups-page read
//! (`docs/goal/behavior/backup-destinations.md` § State & data shape) that replaces the old
//! source-side FFI computation.

use std::sync::Arc;
use std::time::Duration;

use fauna_protocol::backup::{
    AttachFolderReply, AttachFolderRequest, BackupDestinationStatusItem, BackupStatusReply,
    BackupStatusRequest, CoveredFolder, CustodianCheckinReply, CustodianCheckinRequest,
    CustodyItem, CustodyListReply, CustodyListRequest, CustodyMaterializeReply,
    CustodyMaterializeRequest, CustodyRecoverReply, CustodyRecoverRequest, DestinationItem,
    DestinationListReply, DestinationListRequest, DestinationRegisterReply,
    DestinationRegisterRequest, DestinationRemoveReply, DestinationRemoveRequest,
    DetachFolderReply, DetachFolderRequest, GenerationItem, GenerationListReply,
    GenerationListRequest, GenerationRestoreReply, GenerationRestoreRequest, NestKeyGrantReply,
    NestKeyGrantRequest, NestKeyRevokeReply, NestKeyRevokeRequest, WRITER_SEAT_HELD_HOLDER,
    WriterGrantItem, WriterGrantListReply, WriterGrantListRequest, WriterGrantRegisterReply,
    WriterGrantRegisterRequest, WriterGrantRevokeReply, WriterGrantRevokeRequest,
};
use fauna_protocol::decode_strict as decode;

use crate::bridge_routing_handlers::{encode_reply, internal, malformed, require_class};
use crate::db::GenerationRestore;
use crate::db::backup_writer_grants::WriterSeatOutcome;
use crate::db::nest_backup_keys::NEST_BACKUP_KEY_LEN;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

fn nest_key_grant_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.nest_key.grant").await?;
            let req: NestKeyGrantRequest = decode(&payload).map_err(malformed)?;
            if req.nest_backup_key.len() != NEST_BACKUP_KEY_LEN {
                return Err(malformed(format!(
                    "nest_backup_key must be {NEST_BACKUP_KEY_LEN} bytes, got {}",
                    req.nest_backup_key.len()
                )));
            }
            state
                .db
                .put_nest_backup_key(&actor_id, req.nest_backup_key.as_ref())
                .await
                .map_err(internal)?;
            // The seal grant is half of `backup-upload` sufficiency — wake the
            // lease runner so the nest claims the kind without waiting out a
            // heartbeat period (delegation_runner module doc).
            state.delegation_runner_wake.notify_one();
            encode_reply(&NestKeyGrantReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn nest_key_revoke_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.nest_key.revoke").await?;
            let _req: NestKeyRevokeRequest = decode(&payload).map_err(malformed)?;
            let revoked = state
                .db
                .delete_nest_backup_key(&actor_id)
                .await
                .map_err(internal)?;
            // Revoking the seal grant un-assigns the kind: wake the runner so it
            // *releases* the lease now rather than letting it go stale.
            state.delegation_runner_wake.notify_one();
            encode_reply(&NestKeyRevokeReply {
                revoked,
                extra: Default::default(),
            })
        })
    })
}

fn backup_status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.status").await?;
            let _req: BackupStatusRequest = decode(&payload).map_err(malformed)?;

            // Two projections, one reply — because the two kinds are driven from
            // opposite ends (`behavior/backup-destinations.md` § Custodian contract, question 2).
            //
            // ⚠ This is deliberately **registry**-driven rather than
            // coordinator-gated. The coordinator exists only when the owner has
            // granted a `NestBackupKey`, and a pull-only custodian never grants
            // one — "no `NestBackupKey` grant is needed for a pull-only
            // custodian ... this custodian seals for itself"
            // (`behavior/backup-destinations.md` § Third destination kind). So an owner whose only
            // destination is their own iPad has no coordinator at all, and
            // returning early on `None` (as this handler used to) would report
            // them an empty destination list forever, no matter how many
            // check-ins arrived.
            //
            // `enrolled` keeps its ratified meaning — a stored `NestBackupKey`
            // grant — rather than being widened to "has any destination". It is
            // what tells a client whether the *nest* is uploading on its behalf,
            // and for a custodian-only owner the honest answer is that it is not.
            let coordinator = crate::segment_backup::NestBackupCoordinator::open_for_owner(
                Arc::clone(&state),
                actor_id,
            )
            .await
            .map_err(internal)?;
            let enrolled = coordinator.is_some();

            // Nest rows: today's projection, unchanged.
            let mut destinations = match &coordinator {
                Some(c) => c.status().await.map_err(internal)?,
                None => Vec::new(),
            };

            // Custodian rows: projected from what the device last told us.
            let rows = state
                .db
                .list_backup_destinations(&actor_id)
                .await
                .map_err(internal)?;
            let custodians: Vec<_> = rows
                .iter()
                .filter(|d| d.kind == fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE)
                .collect();
            if !custodians.is_empty() {
                let head = crate::segment_backup::owner_segment_head(&state, actor_id)
                    .await
                    .map_err(internal)?;
                for dest in custodians {
                    let checkin = state
                        .db
                        .get_custodian_checkin(&actor_id, &dest.destination_id)
                        .await
                        .map_err(internal)?;
                    destinations.push(custodian_status_row(&dest.destination_id, head, checkin));
                }
            }

            encode_reply(&BackupStatusReply {
                enrolled,
                destinations,
                extra: Default::default(),
            })
        })
    })
}

/// Project one client-device custodian's status row from its latest check-in.
///
/// The row shape is uniform across kinds; the **semantics invert**
/// (`behavior/backup-destinations.md` § Third destination kind → *Status projection inverts, same
/// rows*):
///
/// * `last_upload_time` = the last check-in at which the device reported itself
///   **caught up** — not merely the last check-in, or a permanently-lagging
///   custodian would look freshly synced on every pass.
/// * `backlog_count` = the nest's head minus the device's acked high-water,
///   **saturating**: a custodian can legitimately sit *ahead* of this figure
///   (its pass sealed segments this nest has since compacted away), and an
///   underflow there would render as a backlog of four billion.
/// * `held_bytes` / `cap_state` pass through.
///
/// * `audit_state` / `last_audit_passed_at` pass through — the custodian's own
///   verdict on its local store (`behavior/backup-destinations.md` § Custodian contract question
///   4: the owner cannot inclusion-sample a sleeping device, so it self-audits
///   and the check-in carries the answer).
///
/// A custodian that has never checked in reports `None` / full backlog / no cap
/// state — every field absent rather than zeroed, which is what lets the render
/// tell "not applicable" from "zero". Note in particular that `cap_state` and
/// `audit_state` stay `None` rather than defaulting to their healthy values: the
/// shared labels read cap-reached and audit-failed from those fields alone, and
/// a synthesized "ok" would be this nest asserting a healthy state on behalf of
/// a device that has said nothing.
fn custodian_status_row(
    destination_id: &str,
    head: u64,
    checkin: Option<crate::db::backup_destinations::CustodianCheckinRow>,
) -> BackupDestinationStatusItem {
    match checkin {
        Some(c) => BackupDestinationStatusItem {
            destination_id: destination_id.to_string(),
            last_upload_time: c.caught_up_at.map(|t| t.max(0) as u64),
            backlog_count: head.saturating_sub(c.high_water).min(u32::MAX as u64) as u32,
            held_bytes: Some(c.held_bytes),
            cap_state: Some(c.cap_state),
            audit_state: c.audit_state,
            last_audit_passed_at: c.last_audit_passed_at.map(|t| t.max(0) as u64),
            extra: Default::default(),
        },
        None => BackupDestinationStatusItem {
            destination_id: destination_id.to_string(),
            last_upload_time: None,
            backlog_count: head.min(u32::MAX as u64) as u32,
            held_bytes: None,
            cap_state: None,
            audit_state: None,
            last_audit_passed_at: None,
            extra: Default::default(),
        },
    }
}

/// `fauna.backup.custodian.checkin` — a client-device custodian acking its
/// progress after a pull pass (`message-segment-store.md` § Client-device
/// custodian (pull) → *Check-in*).
///
/// **The refusal is the security-relevant part.** Four states are refused rather
/// than recorded: an unregistered `destination_id`, a destination that is not a
/// client-device kind, a check-in that does not name its device at all, and one
/// naming a device other than the one the row names.
///
/// **What the device guard protects is the honesty of one status row** — not who
/// pulls. `custodian_device_id` has exactly one writer
/// (`put_backup_destination_of_kind`, from `destination.register`), and
/// `get_custodian_checkin` has exactly one non-test consumer: the status
/// projection above. A wrong check-in therefore cannot reassign the pull; it can
/// only make the owner's status row lie. That is still worth refusing, because
/// the lie is *sticky*: `put_custodian_checkin` carries an audit verdict forward
/// across the debounced passes that carry none, so a single planted `ok` outlives
/// the enrolled device's `failed` and silences the rot alarm indefinitely. (An
/// earlier version of this comment claimed the triple "decides who pulls as well
/// as who reports"; it does not, and the overclaim is what made the far more
/// reachable stickiness composition easy to miss.)
///
/// **Absence is refused on custodian rows** (`behavior/backup-destinations.md` § Custodian contract
/// → *Naming yourself*). The general additive-everywhere rule would accept it,
/// but the exemption is structural rather than a judgement call: a client-device
/// row cannot exist without a non-blank `custodian_device_id` (refused at
/// `db::backup_destinations::put_backup_destination_of_kind`), and this kind has
/// never been *served* by any nest whose request type lacked `device_id` — the
/// handler and the field landed in the same commit, the kind
/// having been registered a day earlier with no handler behind it. So no client
/// that ever had a working check-in is refused by this, and the guard is
/// structural instead of advisory.
fn custodian_checkin_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.custodian.checkin").await?;
            let req: CustodianCheckinRequest = decode(&payload).map_err(malformed)?;

            let rows = state
                .db
                .list_backup_destinations(&actor_id)
                .await
                .map_err(internal)?;
            // The two refusals that mean "this device is not that destination's
            // custodian" carry their own code, not `malformed`: the host reads
            // it as the signal to re-read its assignment now
            // (`segment-backup-protocol.md` § Client-device custodian (pull) →
            // *Check-in*). The remaining refusals are malformed requests.
            let not_assigned = |detail: String| {
                crate::rpc_errors::coded_ns("backup", "custodian_not_assigned", detail)
            };
            let Some(dest) = rows.iter().find(|d| d.destination_id == req.destination_id) else {
                return Err(not_assigned(format!(
                    "no registered backup destination '{}'",
                    req.destination_id
                )));
            };
            if dest.kind != fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE {
                return Err(malformed(format!(
                    "destination '{}' is a '{}' destination, which does not check in",
                    req.destination_id, dest.kind
                )));
            }
            let claimed = req
                .device_id
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .ok_or_else(|| {
                    malformed(format!(
                        "destination '{}' requires the checking-in device to name itself",
                        req.destination_id
                    ))
                })?;
            let registered = dest.custodian_device_id.as_deref().unwrap_or("").trim();
            if claimed != registered {
                return Err(not_assigned(format!(
                    "destination '{}' is registered to a different device",
                    req.destination_id
                )));
            }

            // Caught-up is *this nest's* verdict, not the device's claim: the
            // device reports where it got to, and the nest — the only party that
            // knows its own head — decides whether that is level with itself.
            let head = crate::segment_backup::owner_segment_head(&state, actor_id)
                .await
                .map_err(internal)?;
            state
                .db
                .put_custodian_checkin(
                    &actor_id,
                    &req.destination_id,
                    req.high_water,
                    req.held_bytes,
                    &req.cap_state,
                    req.high_water >= head,
                    req.audit_state.as_deref(),
                    req.last_audit_passed_at,
                )
                .await
                .map_err(internal)?;

            encode_reply(&CustodianCheckinReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.backup.writer_grant.{register,revoke,list} ──────────────────────────
//
// The **destination**-side half. These run on the nest that will *hold* the
// backup, spoken by the owner's client over its own authed connection — the
// separation that keeps revocation working with the source nest fully hostile
// (`federation.md` § Nest-writer backup plane). The owner is always the
// authenticated actor, never a wire param, so a caller can only authorize
// writers against **its own** custody.

/// Decode a hex-encoded 32-byte wire value (a nest id, a path hash, a manifest
/// hash), refusing anything else. A malformed value must never reach a store as
/// a row that can never match a real one (a silent dead grant / undialable
/// destination / unrestorable generation) — it is a client bug, reported as one.
/// `field` names the offending wire field in the error.
fn parse_hex32(field: &str, hex_str: &str) -> Result<[u8; 32], fauna_protocol::RpcError> {
    fauna_core::hex32::decode(hex_str)
        .map_err(|_| malformed(format!("{field} must be 32 hex-encoded bytes: {hex_str}")))
}

fn writer_grant_register_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.writer_grant.register").await?;
            let req: WriterGrantRegisterRequest = decode(&payload).map_err(malformed)?;
            let writer = parse_hex32("writer_nest_id", &req.writer_nest_id)?;
            let succeeds = req
                .succeeds
                .as_deref()
                .map(|s| parse_hex32("succeeds", s))
                .transpose()?;
            // No rotation chain is verified here: the caller is the
            // authenticated owner, who is the authority over the grant and over
            // the sets a handover renames (`segment-backup-protocol.md`
            // § Cross-location backup protocol → *The writer seat*).
            match state
                .db
                .register_backup_writer(&actor_id, &writer, succeeds.as_ref())
                .await
                .map_err(internal)?
            {
                WriterSeatOutcome::Seated => encode_reply(&WriterGrantRegisterReply {
                    ok: true,
                    extra: Default::default(),
                }),
                WriterSeatOutcome::Held { holder } => Err(writer_seat_held(holder)),
            }
        })
    })
}

/// `fauna.backup.writer_seat_held` — the owner's writer seat here is another
/// nest's, and the registration took nothing. The details map names the holder
/// (`WRITER_SEAT_HELD_HOLDER`), so the add flow can say which box holds the
/// backup; it is absent when a `succeeds` named a predecessor and no seat
/// exists.
fn writer_seat_held(holder: Option<[u8; 32]>) -> fauna_protocol::RpcError {
    let mut e = fauna_protocol::RpcError::new(
        "fauna.backup.writer_seat_held",
        "error.backup.writer_seat_held",
    );
    let mut details = std::collections::BTreeMap::new();
    if let Some(holder) = holder {
        details.insert(
            WRITER_SEAT_HELD_HOLDER.to_string(),
            fauna_protocol::Value::String(hex::encode(holder)),
        );
    }
    e.details = Some(Box::new(fauna_protocol::Value::Map(details)));
    e
}

fn writer_grant_revoke_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.writer_grant.revoke").await?;
            let req: WriterGrantRevokeRequest = decode(&payload).map_err(malformed)?;
            let writer = parse_hex32("writer_nest_id", &req.writer_nest_id)?;
            let revoked = state
                .db
                .revoke_backup_writer_grant(&actor_id, &writer)
                .await
                .map_err(internal)?;
            encode_reply(&WriterGrantRevokeReply {
                revoked,
                extra: Default::default(),
            })
        })
    })
}

fn writer_grant_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.writer_grant.list").await?;
            let _req: WriterGrantListRequest = decode(&payload).map_err(malformed)?;
            let rows = state
                .db
                .list_backup_writer_grants(&actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&WriterGrantListReply {
                grants: rows
                    .into_iter()
                    .map(|g| WriterGrantItem {
                        writer_nest_id: hex::encode(&g.writer_nest_id),
                        granted_at: g.granted_at,
                        revoked: g.revoked,
                        extra: Default::default(),
                    })
                    .collect(),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.backup.destination.{register,remove,list} ───────────────────────────
//
// The **source**-side destination registry. Spoken by the owner's client to its
// own *source* nest to tell it WHERE to back up — the piece that lets the
// in-process coordinator run, because the `fauna.state.backup` plane entries are
// client-sealed and the nest cannot read them (`backup-restore.md`
// § Background Tasks). Owner is always the authenticated actor; a caller only
// ever registers / removes / lists its own destinations.

fn destination_register_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.destination.register").await?;
            let req: DestinationRegisterRequest = decode(&payload).map_err(malformed)?;
            if req.destination_id.is_empty() {
                return Err(malformed("destination_id must not be empty".to_string()));
            }
            // Branch on the kind BEFORE parsing an address, because the
            // client-device kind has none at all (`behavior/backup-destinations.md` § Custodian
            // contract, question 1). Until this branch existed the kind was
            // refused only *incidentally* — `parse_hex32` on an empty
            // `destination_nest_id` failed closed before any row was stored —
            // so deleting the parse without putting this branch in its place
            // would reopen the dial-the-empty-URL path rather than fix it.
            let nest_id: Vec<u8> = if req.kind == fauna_core::data::DESTINATION_KIND_NEST {
                parse_hex32("destination_nest_id", &req.destination_nest_id)?.to_vec()
            } else {
                Vec::new()
            };
            state
                .db
                .put_backup_destination_of_kind(
                    &actor_id,
                    &req.destination_id,
                    &req.destination_nest_url,
                    &nest_id,
                    &req.kind,
                    req.custodian_device_id.as_deref(),
                    req.capacity_cap_bytes,
                )
                .await
                // The store's kind validation (a blank custodian device id, an
                // unrecognised kind) is a *caller* error, not an internal one:
                // reporting it as `malformed` is what lets an enrolling client
                // see why its row was rejected instead of a bare 500.
                .map_err(|e| malformed(e.to_string()))?;
            // A first destination completes `backup-upload` sufficiency (the
            // other half is the seal grant) — claim the lease promptly.
            state.delegation_runner_wake.notify_one();
            encode_reply(&DestinationRegisterReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn destination_remove_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.destination.remove").await?;
            let req: DestinationRemoveRequest = decode(&payload).map_err(malformed)?;
            let removed = state
                .db
                .delete_backup_destination(&actor_id, &req.destination_id)
                .await
                .map_err(internal)?;
            // Removing the last one ends sufficiency — release rather than
            // leaving a lease the nest no longer earns.
            state.delegation_runner_wake.notify_one();
            encode_reply(&DestinationRemoveReply {
                removed,
                extra: Default::default(),
            })
        })
    })
}

fn destination_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.destination.list").await?;
            let _req: DestinationListRequest = decode(&payload).map_err(malformed)?;
            let rows = state
                .db
                .list_backup_destinations(&actor_id)
                .await
                .map_err(internal)?;
            // Ordinary-folder coverage, joined per destination. The set name is
            // derived here (this nest knows its own id) so no client ever
            // re-derives the naming rule (`db::sync_storage::folder_backup_set_name`).
            let coverage = state
                .db
                .list_backup_destination_folders(&actor_id)
                .await
                .map_err(internal)?;
            let nest_id = state.nest_identity.public_key_bytes();
            encode_reply(&DestinationListReply {
                destinations: rows
                    .into_iter()
                    .map(|d| DestinationItem {
                        covered_folders: coverage
                            .iter()
                            .filter(|c| c.destination_id == d.destination_id)
                            .map(|c| CoveredFolder {
                                folder_id: c.folder_id,
                                folder_set: crate::db::sync_storage::folder_backup_set_name(
                                    &nest_id,
                                    c.folder_id,
                                ),
                                // The label a re-seed restores the folder under:
                                // the owner's own listing is the one place the
                                // owner's own device may learn it, since custody
                                // never carries it (`CoveredFolder::name`).
                                name: c.name.clone(),
                                // The address and sealed label beside it: what
                                // the custodian keeps once the plaintext name
                                // leaves the row (`CoveredFolder::name_hash`).
                                name_hash: c.name_hash.clone().map(Into::into),
                                name_sealed: c.name_sealed.clone().map(Into::into),
                                extra: Default::default(),
                            })
                            .collect(),
                        destination_id: d.destination_id,
                        destination_nest_url: d.nest_url,
                        destination_nest_id: hex::encode(&d.nest_id),
                        added_at: d.added_at,
                        // The stored per-kind columns, not a placeholder. This
                        // read-back is how the *desktop custodian host*
                        // discovers its own assignment
                        // (`bins/fauna-sync-agent/src/custodian.rs`): it is
                        // bearer-only and structurally cannot open the at-rest
                        // `fauna.state.backup` destination row, so it matches
                        // its own `SyncCapability.device_id` against
                        // `custodian_device_id` here — *policy through the nest,
                        // never over local IPC* (`apps/sync-agent.md`
                        // § Control plane split). Serving a hard-coded
                        // `kind: "nest"` here is what left every enrolled
                        // custodian silently unhosted.
                        kind: d.kind,
                        custodian_device_id: d.custodian_device_id,
                        capacity_cap_bytes: d.capacity_cap_bytes,
                        extra: Default::default(),
                    })
                    .collect(),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.backup.destination.{attach_folder,detach_folder} ────────────────────
//
// Ordinary-folder coverage (`docs/goal/behavior/backup-destinations.md`
// § Ordinary-folder coverage — destination places). Spoken by the owner's
// client to its own *source* nest: coverage lives nest-side beside the registry
// because the nest drives the push and cannot read the client-sealed config.
// Owner is always the authenticated actor.

fn destination_attach_folder_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.destination.attach_folder").await?;
            let req: AttachFolderRequest = decode(&payload).map_err(malformed)?;
            if req.destination_id.is_empty() {
                return Err(malformed("destination_id must not be empty".to_string()));
            }
            // Coverage is owner-only and `FolderRef::Local`-only by ratified
            // design — nest-side that is simply "a `folders` row this actor
            // owns". Reserved (`__`) rails stay whole-account and implicit; a
            // per-rail attach would let a user silently un-back-up their mail
            // while thinking they are managing folders, so it is refused here
            // rather than tolerated.
            let folder = state
                .db
                .get_folder_by_id(req.folder_id)
                .await
                .map_err(internal)?
                .filter(|f| f.actor_id == actor_id)
                .ok_or_else(|| {
                    crate::rpc_errors::not_found_ns(
                        "backup",
                        format!("no folder {} owned by this actor", req.folder_id),
                    )
                })?;
            if crate::db::snapshots::is_reserved_folder_name(&folder.name) {
                return Err(crate::rpc_errors::invalid_request_ns(
                    "backup",
                    "reserved sets are covered implicitly; only ordinary folders attach",
                ));
            }
            let attached = state
                .db
                .attach_backup_destination_folder(&actor_id, &req.destination_id, req.folder_id)
                .await
                // The store refuses an unregistered destination — a caller
                // error surfaced as such (the register handler's precedent).
                .map_err(|e| malformed(e.to_string()))?;
            encode_reply(&AttachFolderReply {
                attached,
                folder_set: crate::db::sync_storage::folder_backup_set_name(
                    &state.nest_identity.public_key_bytes(),
                    req.folder_id,
                ),
                extra: Default::default(),
            })
        })
    })
}

fn destination_detach_folder_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.destination.detach_folder").await?;
            let req: DetachFolderRequest = decode(&payload).map_err(malformed)?;
            // No folder-side validation, deliberately: a detach must succeed
            // after the folder itself is gone, or a deleted folder's coverage
            // could never be cleaned up by its owner. The destination-side
            // teardown is the coordinator's, on its next pass.
            let detached = state
                .db
                .detach_backup_destination_folder(&actor_id, &req.destination_id, req.folder_id)
                .await
                .map_err(internal)?;
            encode_reply(&DetachFolderReply {
                detached,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.backup.generation.{list,restore} ────────────────────────────────────
//
// The **destination**-side custody grace window (T =
// `crate::backup::gc::BACKUP_CUSTODY_GRACE_SECS`). Spoken by the owner's client
// over its own authed connection to the destination, deliberately NOT through
// the source nest: the source is the custody *writer*, so it is exactly the
// party a rogue-source recovery must be able to route around
// (`message-segment-store.md` § Cross-location backup protocol). Owner is always
// the authenticated actor, so a caller only ever lists / restores custody held
// for itself.

/// Parse the opaque page cursor the two list handlers mint: `"{key}:{rowid}"`,
/// where `key` is the serve order's leading column (`superseded_at` /
/// `updated_at`). Opaque to clients — only a value a reply's `next_cursor`
/// handed out is meaningful, and a malformed one refuses loudly rather than
/// silently restarting from the first page.
fn parse_list_cursor(cursor: &str) -> Option<(i64, i64)> {
    let (key, rowid) = cursor.split_once(':')?;
    Some((key.parse().ok()?, rowid.parse().ok()?))
}

/// Row-fetch bound per list page — a handler **memory** bound, not the page
/// size: a realistic encoded row is ≥ ~250 bytes, so the frame byte budget
/// (`crate::segments::SERVE_PAGE_BUDGET_BYTES`) binds first. If every fetched
/// row nonetheless fits the budget, the minted `next_cursor` simply yields one
/// extra (possibly empty) page — never a lost row.
const BACKUP_LIST_FETCH_CAP: i64 = 8192;

/// Cut one list page at the ratified frame budget (`transport.md` § Max frame
/// corollary — close early, never skip-and-continue) and mint the resume
/// cursor. `fetch_bound` is the row bound the DB query ran with — a page that
/// filled it may have more rows behind it even when everything fit the budget.
/// `key_of` yields each row's `(order_key, rowid)` cursor pair; `wire_len` its
/// encoded reply size. Returns `(page, next_cursor)`.
///
/// A head row alone over the budget freezes the page **loudly** (an internal
/// error, which the client projections render as "could not ask") — an empty
/// page with no cursor would read as "drained", hiding every row behind the
/// freeze, and skipping it is the one thing the corollary forbids.
fn cut_list_page<T>(
    rows: Vec<T>,
    fetch_bound: i64,
    key_of: impl Fn(&T) -> (i64, i64),
    wire_len: impl Fn(&T) -> usize,
) -> Result<(Vec<T>, Option<String>), fauna_protocol::RpcError> {
    let fetched = rows.len();
    let (page, rest) = crate::segments::take_page_within_budget(rows, wire_len);
    if page.is_empty() && !rest.is_empty() {
        return Err(internal(
            "a single backup list row exceeds the serve page budget — page frozen",
        ));
    }
    let more = !rest.is_empty() || fetched as i64 == fetch_bound;
    let next_cursor = match (more, page.last()) {
        (true, Some(last)) => {
            let (key, rowid) = key_of(last);
            Some(format!("{key}:{rowid}"))
        }
        _ => None,
    };
    Ok((page, next_cursor))
}

fn generation_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.generation.list").await?;
            let req: GenerationListRequest = decode(&payload).map_err(malformed)?;
            let after = match req.cursor.as_deref() {
                None => None,
                Some(c) => Some(
                    parse_list_cursor(c)
                        .ok_or_else(|| malformed("unparseable generation.list cursor"))?,
                ),
            };
            let fetch = if req.limit > 0 {
                req.limit.min(BACKUP_LIST_FETCH_CAP)
            } else {
                BACKUP_LIST_FETCH_CAP
            };
            let rows = state
                .db
                .list_backup_custody_generations(&actor_id, after, fetch)
                .await
                .map_err(internal)?;

            let items: Vec<(GenerationItem, i64)> = rows
                .into_iter()
                .map(|g| {
                    (
                        GenerationItem {
                            folder_name: g.folder_name,
                            path: g.path,
                            path_hash: hex::encode(&g.path_hash),
                            manifest_hash: hex::encode(&g.manifest_hash),
                            size_bytes: g.size_bytes,
                            superseded_at: g.superseded_at,
                            extra: Default::default(),
                        },
                        g.rowid,
                    )
                })
                .collect();
            let (page, next_cursor) = cut_list_page(
                items,
                fetch,
                |(item, rowid)| (item.superseded_at, *rowid),
                |(item, _)| {
                    fauna_protocol::encode_canonical(item)
                        .map(|b| b.len())
                        .unwrap_or(crate::segments::SERVE_PAGE_BUDGET_BYTES + 1)
                },
            )?;
            encode_reply(&GenerationListReply {
                generations: page.into_iter().map(|(item, _)| item).collect(),
                grace_secs: crate::backup::gc::BACKUP_CUSTODY_GRACE_SECS,
                next_cursor,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.backup.custody.list` — the **live** latest-per-path custody this nest
/// holds for the authenticated owner.
///
/// The audit-loop counterpart to [`generation_list_handler`]: that one reports
/// what can be rolled back, this one reports what is actually here. A client
/// auditing its backup cannot get this from `fauna.backup.status`, because that
/// projection is computed by the **source** nest from its own upload bookkeeping
/// — the source is the custody writer, so asking it whether it wrote is asking
/// the suspect to vouch for itself. Spoken over the owner's own authed
/// connection to the destination for exactly the same reason the generation
/// kinds are (`message-segment-store.md` § Cross-location backup protocol).
fn custody_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.custody.list").await?;
            let req: CustodyListRequest = decode(&payload).map_err(malformed)?;
            let after = match req.cursor.as_deref() {
                None => None,
                Some(c) => Some(
                    parse_list_cursor(c)
                        .ok_or_else(|| malformed("unparseable custody.list cursor"))?,
                ),
            };
            let fetch = if req.limit > 0 {
                req.limit.min(BACKUP_LIST_FETCH_CAP)
            } else {
                BACKUP_LIST_FETCH_CAP
            };
            let rows = state
                .db
                .list_backup_custody(&actor_id, after, fetch)
                .await
                .map_err(internal)?;

            let items: Vec<(CustodyItem, i64)> = rows
                .into_iter()
                .map(|c| {
                    (
                        CustodyItem {
                            folder_name: c.folder_name,
                            path: c.path,
                            path_hash: hex::encode(&c.path_hash),
                            manifest_hash: hex::encode(&c.manifest_hash),
                            size_bytes: c.size_bytes,
                            updated_at: c.updated_at,
                            extra: Default::default(),
                        },
                        c.rowid,
                    )
                })
                .collect();
            let (page, next_cursor) = cut_list_page(
                items,
                fetch,
                |(item, rowid)| (item.updated_at, *rowid),
                |(item, _)| {
                    fauna_protocol::encode_canonical(item)
                        .map(|b| b.len())
                        .unwrap_or(crate::segments::SERVE_PAGE_BUDGET_BYTES + 1)
                },
            )?;
            encode_reply(&CustodyListReply {
                items: page.into_iter().map(|(item, _)| item).collect(),
                next_cursor,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.backup.custody.materialize` — phase 3 of the re-seed ceremony.
///
/// The whole authorization story is the first two lines: the class gate, and
/// `actor_id` coming from the authenticated connection rather than the payload.
/// A caller therefore names a *set*, never an owner — the set is resolved inside
/// their own custody or not at all, exactly like `custody.list` and
/// `generation.restore` beside it.
///
/// Each refusal keeps its own wire code so a client can tell "this destination
/// has not caught up yet, try again after its next pass" from "this account is
/// already live, you are pointing at the wrong nest" — the two the owner would
/// otherwise have to guess between, and the ones with opposite next steps.
/// `fauna.backup.target_not_empty` and `fauna.backup.target_not_fresh` are the
/// empty-target rule's two wire faces — records the ceremony did not write, and
/// an audience it must not publish into — both refused, typed, with no force arm
/// to offer instead.
fn custody_materialize_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.custody.materialize").await?;
            let req: CustodyMaterializeRequest = decode(&payload).map_err(malformed)?;
            // The set name picks the axis, and it is the only thing that can:
            // the two namespaces are disjoint by construction (a folder set is
            // `__folder/<nest>/<id>`, which no segment-axis derivation produces),
            // and each parser re-derives its own name rather than matching a
            // prefix, so a name cannot be read as both.
            let outcome =
                if fauna_core::data::parse_folder_backup_set_name(&req.set_name).is_some() {
                    crate::backup::materialize::materialize_folder_set(
                        &state,
                        &actor_id,
                        &req.set_name,
                        req.folder_display_name.as_deref(),
                        req.folder_name_hash.as_deref().map(|h| &h[..]),
                        req.signer_key.as_deref().map(|k| &k[..]),
                        &req.signatures,
                    )
                    .await
                } else {
                    crate::backup::materialize::materialize_segment_set(
                        &state,
                        &actor_id,
                        &req.set_name,
                    )
                    .await
                }
                .map_err(materialize_error)?;
            encode_reply(&CustodyMaterializeReply {
                segments: outcome.segments,
                records: outcome.records,
                // The set's bytes are live in the scope now, so the custody copy
                // is redundant — but it survives. Reclaiming it is the owner's
                // separate, deliberate set-delete gesture (the ceremony deletes
                // nothing, on success as on refusal).
                custody_redundant: true,
                placements: outcome.placements,
                remaining: outcome.remaining,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.backup.custody.recover` — the lived-in recovery, materialize's
/// sibling (`segment-backup-protocol.md` § Client-device custodian (pull) →
/// *Restore* → *Recovery into the lived-in nest that regressed*).
///
/// The same two-line authorization story as materialize — the class gate, and
/// the owner taken from the connection — and the same refusal catalogue, minus
/// the empty-target rule: this verb never refuses a target for what it holds.
fn custody_recover_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.custody.recover").await?;
            let req: CustodyRecoverRequest = decode(&payload).map_err(malformed)?;
            let outcome =
                crate::backup::recover::recover_segment_set(&state, &actor_id, &req.set_name, None)
                    .await
                    .map_err(materialize_error)?;
            encode_reply(&CustodyRecoverReply {
                recovered: outcome.recovered,
                already_held: outcome.already_held,
                filed: outcome.filed,
                inboxed: outcome.inboxed,
                unplaceable: outcome.unplaceable,
                floor: outcome.floor,
                extra: Default::default(),
            })
        })
    })
}

/// One typed refusal → one wire code, all under the `fauna.backup.` namespace.
/// Shared by materialize and recover: the refusals they have in common mean the
/// same thing and carry the same next step on both.
fn materialize_error(
    refusal: crate::backup::materialize::MaterializeRefusal,
) -> fauna_protocol::RpcError {
    use crate::backup::materialize::MaterializeRefusal as R;
    let detail = refusal.to_string();
    match refusal {
        R::NotASegmentSet { .. } | R::UnsupportedKind { .. } => {
            crate::rpc_errors::invalid_params_ns("backup", detail)
        }
        R::NotEnrolled => crate::rpc_errors::coded_ns("backup", "not_enrolled", detail),
        // The empty-target rule's one wire face, whichever axis raised it: both
        // mean "you are pointing at a lived-in target", both offer no force arm,
        // and a client that learned to render one has learned to render both.
        //
        // `PlacementNotFresh` is the same face for the same reason, even though
        // the account it names may hold no live record at all: its mailboxes
        // have held mail, so it IS a lived-in account and the next step is the
        // same one — point at another nest. (Contrast `FolderNotFresh` below,
        // whose next step differs and whose code therefore does too.)
        R::ScopeNotEmpty { .. } | R::FolderNotEmpty { .. } | R::PlacementNotFresh { .. } => {
            crate::rpc_errors::coded_ns("backup", "target_not_empty", detail)
        }
        // The empty-target rule's OTHER half, and its own code on purpose: the
        // target here is genuinely empty, so `target_not_empty` would send the
        // owner hunting for records that are not there. This one says "the
        // folder you named has an audience" — a different next step (name
        // another folder, or clear the property), which is precisely the
        // distinction this catalogue exists to draw.
        R::FolderNotFresh { .. } => {
            crate::rpc_errors::coded_ns("backup", "target_not_fresh", detail)
        }
        // The folder axis's two malformed-input refusals. A missing display name
        // and a reserved one are both "fix the request and retry", which is what
        // `invalid_params` means everywhere else on this plane.
        R::FolderNameRequired { .. }
        | R::FolderNameReserved { .. }
        | R::FolderNameHashMismatch { .. } => {
            crate::rpc_errors::invalid_params_ns("backup", detail)
        }
        // Its own code, and deliberately not `custody_incomplete`: that one says
        // "this destination has not finished delivering, wait for its next pass"
        // and needs no action from the owner, while this says "the custodian
        // holds these bytes under a name it never sent" — the fix is a fresh
        // pull pass on the custodian, then a re-delivery, and it moves no bytes.
        // The folder arm never mints an unsigned row (`writer-signed-change-
        // records.md` ruling (7)(a)(ii)): the record door's own code.
        R::FolderRehomeUnsigned { .. } => {
            crate::rpc_errors::coded_ns("backup", "signature_required", detail)
        }
        // The ceremony prepares the target set before materializing (ruling
        // (7)(a)(i)); its own code so the driver branches on it.
        R::FolderTargetMissing { .. } => {
            crate::rpc_errors::coded_ns("backup", "target_missing", detail)
        }
        // The record door's own codes — `signature_invalid` /
        // `author_mismatch` — under this namespace.
        R::RehomeSignature { refusal, .. } => refusal.into_rpc("backup"),
        R::RehomePageMalformed { .. } => crate::rpc_errors::invalid_params_ns("backup", detail),
        R::CustodyPathUnsealed { .. } => {
            crate::rpc_errors::coded_ns("backup", "custody_unsealed", detail)
        }
        // All three mean "the corpus you are pointing at is not whole yet", which is
        // exactly what `custody_incomplete` says on the segment axis — an empty
        // folder set is this plane's `NoManifestMirror`, custody rows being its
        // corpus rather than a mirror file.
        R::CustodyPathUnaddressable { .. }
        | R::CustodyManifestUnreadable { .. }
        | R::EmptyFolderSet { .. } => {
            crate::rpc_errors::coded_ns("backup", "custody_incomplete", detail)
        }
        // The ratified honest outcome for a corpus larger than the fresh nest's
        // quota — the ordinary code, raised in the admin app like any other,
        // never a re-seed special case.
        R::Quota(e) => crate::sync_handlers::quota_err(e),
        // Its own code, not `target_not_empty`: that one says "you are pointing
        // at a live account" and means do not retry, while this says "something
        // is in the way" and means reclaim, then retry. Same reason every other
        // refusal here is typed per cause.
        R::SegmentPathOccupied { .. } => {
            crate::rpc_errors::coded_ns("backup", "segment_path_occupied", detail)
        }
        // Both say the corpus is not yet whole and the remedy is the same —
        // let the source finish delivering.
        R::NoManifestMirror { .. } | R::IncompleteSegment { .. } => {
            crate::rpc_errors::coded_ns("backup", "custody_incomplete", detail)
        }
        R::Integrity(e) => {
            // The detail text is the owner's own corpus talking about itself, so
            // it is safe to hand back — and it is the only thing that makes a
            // failed recovery diagnosable from the app.
            tracing::error!(error = %e, "custody materialize failed verification");
            crate::rpc_errors::coded_ns("backup", "custody_unreadable", detail)
        }
    }
}

fn generation_restore_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.backup.generation.restore").await?;
            let req: GenerationRestoreRequest = decode(&payload).map_err(malformed)?;
            let path_hash = parse_hex32("path_hash", &req.path_hash)?;
            let manifest_hash = parse_hex32("manifest_hash", &req.manifest_hash)?;
            let restored = match state
                .db
                .restore_backup_custody_generation(
                    &actor_id,
                    &req.folder_name,
                    &path_hash,
                    &manifest_hash,
                )
                .await
                .map_err(internal)?
            {
                GenerationRestore::Restored => true,
                GenerationRestore::NotFound => false,
                // Its own code, not `restored: false`: the generation exists
                // and is this owner's, and retrying changes nothing.
                GenerationRestore::BeforeSeat => {
                    return Err(crate::rpc_errors::coded_ns(
                        "backup",
                        "generation_before_seat",
                        "this generation was written by a previous holder of the writer \
                         seat; restoring it would mix two boxes' backups in one set",
                    ));
                }
            };
            encode_reply(&GenerationRestoreReply {
                restored,
                extra: Default::default(),
            })
        })
    })
}

pub fn register_backup_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.backup.nest_key.grant",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: nest_key_grant_handler(),
        },
    );
    b.add(
        "fauna.backup.nest_key.revoke",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: nest_key_revoke_handler(),
        },
    );
    b.add(
        "fauna.backup.status",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: backup_status_handler(),
        },
    );
    // `forbid_replay: false` audited 2026-07-31, not inherited. Under
    // `transport.md` § Idempotency and reconnect-with-resume the flag is an
    // assertion that the handler is *naturally idempotent* under a repeated
    // same-key call — the nest's idempotency cache is per-`RpcConnection`, so
    // it can never deduplicate an auto-retry, which always lands on a fresh
    // connection. It holds here: a repeat finds the writer the first attempt
    // seated and lands on the holder's refresh arm
    // (`db/backup_writer_grants.rs`), a completed `succeeds` handover
    // included. Only `granted_at` is refreshed — an audit timestamp, not
    // authority: the seat a repeat lands on is the one the first attempt
    // already made.
    b.add(
        "fauna.backup.writer_grant.register",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: writer_grant_register_handler(),
        },
    );
    // `forbid_replay: false` audited 2026-07-31 (same rule as above). The
    // *state* is idempotent — `DELETE … WHERE owner = ?1 AND writer = ?2`, and
    // the dangerous direction (a revoked writer coming back) is unrepresentable
    // by a repeat. The *reply* is not: `revoked` is `n > 0`, so a retry after a
    // successful-but-unacknowledged first attempt reports `revoked: false` for
    // a grant its own caller just removed. That is a misleading answer, never a
    // double-apply, and it is the exact case the idempotency cache used to be
    // credited with covering — so if this ever needs to read true across a
    // reconnect, the fix is a lookup, not `forbid_replay: true`.
    b.add(
        "fauna.backup.writer_grant.revoke",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: writer_grant_revoke_handler(),
        },
    );
    b.add(
        "fauna.backup.writer_grant.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: writer_grant_list_handler(),
        },
    );
    b.add(
        "fauna.backup.destination.register",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: destination_register_handler(),
        },
    );
    b.add(
        "fauna.backup.destination.remove",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: destination_remove_handler(),
        },
    );
    b.add(
        "fauna.backup.destination.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: destination_list_handler(),
        },
    );
    // Idempotent by handler construction (INSERT OR IGNORE / DELETE keyed on
    // the full coverage tuple), so an auto-retry on a fresh connection lands
    // on the identical row — the same audit the register/remove pair carries.
    b.add(
        "fauna.backup.destination.attach_folder",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: destination_attach_folder_handler(),
        },
    );
    b.add(
        "fauna.backup.destination.detach_folder",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: destination_detach_folder_handler(),
        },
    );
    b.add(
        "fauna.backup.generation.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(15),
            handler: generation_list_handler(),
        },
    );
    // `forbid_replay: false` audited 2026-07-31 (same rule as the writer-grant
    // pair above), and this is the one that had to be traced rather than read:
    // the handler promotes a retained generation over the live one, which is
    // not idempotent-looking at all. It is idempotent by *consumption* — a
    // successful restore ends with `unretain_custody_generation_in_conn`
    // (`db/sync_storage.rs`) deleting the row it just promoted from
    // `backup_custody_generations`, so a same-key repeat fails the retained
    // lookup and returns `Ok(false)` before touching custody, the displaced
    // generation, or `storage_bytes_used`. ⚠️ That makes the `unretain` call
    // load-bearing for replay safety, not just for the retained-set invariant
    // its own comment cites: a change that leaves the promoted row retained
    // would make a repeat re-run the promotion, and this flag would then be
    // wrong.
    b.add(
        "fauna.backup.generation.restore",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(15),
            handler: generation_restore_handler(),
        },
    );
    b.add(
        "fauna.backup.custody.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(15),
            handler: custody_list_handler(),
        },
    );
    // `forbid_replay: false` — a repeat is refused, not re-run. The first call
    // leaves the scope holding live records, so the empty-target rule turns
    // every subsequent same-key call into `fauna.backup.target_not_empty` before
    // a byte is read. That makes the kind idempotent by *refusal* rather than by
    // convergence, which is the stronger of the two: an auto-retry on a fresh
    // connection cannot double-write a segment area, and cannot silently succeed
    // twice either. The 300s deadline matches the router-side registration in
    // `fauna_protocol::kind` — a corpus-sized reconstitution, not a page read.
    b.add(
        "fauna.backup.custody.materialize",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(300),
            handler: custody_materialize_handler(),
        },
    );
    // `forbid_replay: false` — idempotent by convergence: held now implies
    // filed, so a repeat on a fresh connection recovers nothing and replies
    // with every record `already_held`. The 300s deadline is materialize's,
    // for the same corpus-sized read.
    b.add(
        "fauna.backup.custody.recover",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(300),
            handler: custody_recover_handler(),
        },
    );
    // `forbid_replay: false` — the write is `INSERT OR REPLACE` keyed on
    // `(owner, destination_id)`, so a repeat lands on the identical row with the
    // identical payload. The one derived field, `caught_up_at`, is computed from
    // the same `high_water` against a head that only ever moves forward, so a
    // replay can re-stamp it a few seconds later but can never flip it from set
    // to unset — the direction that would matter. (`transport.md` § Idempotency
    // and reconnect-with-resume: the flag asserts natural idempotence under a
    // repeated same-key call, which the per-connection cache cannot supply for
    // an auto-retry on a fresh connection.)
    b.add(
        "fauna.backup.custodian.checkin",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: custodian_checkin_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::materialize_error;
    use crate::backup::materialize::MaterializeRefusal as R;
    use fauna_protocol::RpcError;

    /// The materialize refusals a ceremony driver branches on are emitted under
    /// exactly the shared `fauna-protocol` constants its classifier matches
    /// (`fauna_client_backup::reseed`). The emitter formats the code from a
    /// namespace and a suffix, so without this pin a renamed suffix would
    /// silently turn a typed remedy into an "other" refusal on every app.
    #[test]
    fn materialize_refusals_use_the_shared_codes() {
        for (refusal, code) in [
            (
                R::ScopeNotEmpty {
                    kind: "mail",
                    records: 1,
                },
                RpcError::CODE_BACKUP_TARGET_NOT_EMPTY,
            ),
            (
                R::FolderNotEmpty {
                    folder: "f".into(),
                    records: 1,
                },
                RpcError::CODE_BACKUP_TARGET_NOT_EMPTY,
            ),
            (
                R::FolderNotFresh {
                    folder: "f".into(),
                    property: "audience",
                },
                RpcError::CODE_BACKUP_TARGET_NOT_FRESH,
            ),
            (
                R::CustodyPathUnsealed {
                    path_hash: "aa".into(),
                },
                RpcError::CODE_BACKUP_CUSTODY_UNSEALED,
            ),
            (R::NotEnrolled, RpcError::CODE_BACKUP_NOT_ENROLLED),
            (
                R::FolderTargetMissing { folder: "f".into() },
                RpcError::CODE_BACKUP_TARGET_MISSING,
            ),
            (
                R::FolderRehomeUnsigned {
                    set_name: "__folder/aa/1".into(),
                },
                RpcError::CODE_BACKUP_SIGNATURE_REQUIRED,
            ),
        ] {
            assert_eq!(materialize_error(refusal).code, code);
        }
    }

    /// A corpus larger than the fresh nest's quota answers the ordinary quota
    /// code — the ratified honest outcome — under the same shared constant.
    #[test]
    fn a_materialize_over_quota_uses_the_shared_quota_code() {
        let e = materialize_error(R::Quota(
            crate::db::sync_storage::StorageQuotaError::Exceeded {
                used: 0,
                requested: 1,
                max: 0,
            },
        ));
        assert_eq!(e.code, RpcError::CODE_SYNC_STORAGE_QUOTA_EXCEEDED);
    }
}
