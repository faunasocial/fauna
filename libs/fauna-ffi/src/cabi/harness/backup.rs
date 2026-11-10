//! The harness's **backup legs** — the owner-side halves of a nest-to-nest
//! backup an app runs, driven app-less so a `[nest]` outcome whose promise the
//! shared client code keeps is witnessed through this library and never
//! through a second app-less driver (`feature-catalog.md` § The two surfaces,
//! the 2026-10-13 ruling):
//!
//! - [`fauna_harness_backup_enroll`] — a nest destination added for the owner
//!   through the one shared enroll sequence
//!   ([`fauna_client_config::enroll_backup_destination`]), exactly as
//!   `backup_destination_add` composes it;
//! - [`fauna_harness_backup_audit_pass`] — one client audit pass
//!   ([`fauna_client_backup::audit::run_audit_pass`]) over the native seams
//!   `backup_audit_run_pass` builds, at an explicit clock offset, into an
//!   audit store at a path the test owns;
//! - [`fauna_harness_backup_recover`] — the lived-in recovery
//!   ([`fauna_sync_engine::recovery::recover_from_destination`]) reading the
//!   regressed sets off that store;
//! - [`fauna_harness_backup_pull_back`] — the after-loss re-seed from a
//!   surviving nest destination
//!   ([`fauna_sync_engine::reseed_pull::pull_back_from_destination`], its
//!   preflight first), the composition every app's confirmed re-seed gesture
//!   on a nest-kind row runs.
//!
//! Each export resolves its inputs the way the app face beside it does and
//! decides nothing: the destination list is the owner's own pinned
//! `fauna.state.backup` list for the box the connection is bound to, read and
//! written through a [`Seat`]'s account store (the parent module's *How an
//! app-less process reaches custody*), so every call leaves what a signed-in
//! device leaves.

use std::ffi::c_char;
use std::path::PathBuf;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_config::{ResolvedDestination, enroll_backup_destination, load_backup_state};
use fauna_core::data::BackupDestination;

use super::{Outcome, Seat, decode_call, encode_err, err, run, seat_device_id, with_seat};
use crate::FfiError;
use crate::cabi::FfiBuffer;

/// The box `nest` is bound to — the key of its per-box destination list.
async fn bound_box(nest: &Arc<NestClient>) -> Result<[u8; 32], FfiError> {
    fauna_client_account_runtime::deployment_seeds::bound_nest_id(nest)
        .await
        .map(|id| id.0)
        .map_err(|e| err(format!("resolve the nest this connection is bound to: {e}")))
}

/// The owner's pinned destination row `destination_id`, from the seat's store.
async fn pinned_destination(
    nest: &Arc<NestClient>,
    seat: &Seat,
    destination_id: &str,
) -> Result<BackupDestination, FfiError> {
    let state = load_backup_state(&seat.handle, bound_box(nest).await?)
        .await
        .map_err(|e| err(format!("read the backup destination list: {e}")))?;
    state
        .backup
        .destinations
        .into_iter()
        .find(|d| d.destination_id == destination_id)
        .ok_or_else(|| err(format!("no backup destination {destination_id}")))
}

/// The device id the seat records under, as the 32 bytes the legs take.
fn seat_device(identity: &fauna_core::identity::ActorKeypair) -> Result<[u8; 32], FfiError> {
    fauna_core::hex32::decode(&seat_device_id(identity))
        .map_err(|e| err(format!("the seat's device id: {e}")))
}

/// A reply as the export's `{"ok": …}` (a macro for the parent module's
/// reason: the concrete type reaches `canonical_encode` without this crate
/// naming serde).
macro_rules! encoded {
    ($v:expr) => {
        fauna_core::encoding::canonical_encode(&$v)
            .map(Outcome::Ok)
            .map_err(encode_err)
    };
}

/// **Enroll a nest destination** for the owner whose seed is `secret` on the
/// owner's nest at `nest_url` — the shared
/// [`fauna_client_config::enroll_backup_destination`] over the destination's
/// connected resolve (`segment_backup::resolve_destination_connected`), the
/// sequence `backup_destination_add` runs: the source nest granted this
/// owner's `NestBackupKey`, the nest-writer grant registered at the
/// destination, the destination registered with the source nest, then the row
/// recorded in the owner's pinned list and published.
///
/// The owner must be registered on the destination nest first (an account an
/// admin added there), as in the app journeys.
///
/// `request` is the dag-cbor array `[destination_url, name]`. `out` receives
/// `{"ok": [destination_id, rows]}` — the new row's id, and the owner's whole
/// destination list after the enroll as canonical dag-cbor bytes (opaque to the
/// caller: what a device's own store holds, which a test carries across a lost
/// box into [`fauna_harness_backup_pull_back`]); any refusal is a local failure
/// carrying the enroll's own error text.
///
/// # Safety
///
/// As `fauna_harness_create_set`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_backup_enroll(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (url, name): (String, String) = decode_request!(call.req)?;
        let owner_secret = *call.identity.secret_bytes();
        with_seat(
            call.nest_url,
            call.identity,
            async |nest, _identity, seat| {
                let (resolved, destination) =
                    fauna_sync_engine::segment_backup::resolve_destination_connected(
                        owner_secret,
                        &url,
                    )
                    .await
                    .map_err(|e| err(format!("resolve the destination {url}: {e:#}")))?;
                let source_nest_id = bound_box(nest).await?;
                let destination_id = uuid::Uuid::new_v4().to_string();
                let rows = enroll_backup_destination(
                    Arc::clone(nest),
                    Arc::clone(&destination),
                    &seat.handle,
                    owner_secret,
                    ResolvedDestination {
                        destination_id: destination_id.clone(),
                        destination_nest_url: url,
                        destination_actor_pubkey: resolved.actor_pubkey,
                        domain: resolved.domain,
                        requested_name: name,
                    },
                    source_nest_id,
                )
                .await
                .map_err(|e| err(format!("enroll the destination: {e}")))?;
                destination.disconnect().await;
                seat.publish().await?;
                let rows = fauna_core::encoding::canonical_encode(&rows).map_err(encode_err)?;
                encoded!((destination_id, fauna_protocol::ByteBuf::from(rows)))
            },
        )
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// **One client audit pass** for the owner whose seed is `secret`, over the
/// owner's pinned destination list for the box at `nest_url` — the shared
/// [`fauna_client_backup::audit::run_audit_pass`] with the three native seams
/// `backup_audit_run_pass` builds (the owner's own session to each
/// destination, the inclusion source opening sampled records under the
/// owner's `NestBackupKey`, the file audit store), the covered-folder plane
/// anchored nowhere (no replica: the declared absence).
///
/// `request` is the dag-cbor array `[state_path, clock_offset_secs]`: the
/// audit store's file — the test's own, actor-scoped, carried across passes
/// and into [`fauna_harness_backup_recover`] — and the seconds added to the
/// pass's `now` (zero in a real run). `out` receives `{"ok": rows}`, one
/// `[destination_id, last_passed_at, alert_reasons, recover_offered]` per
/// destination, as `FfiDestinationAuditRow` resolves them.
///
/// # Safety
///
/// As `fauna_harness_create_set`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_backup_audit_pass(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (state_path, offset): (String, i64) = decode_request!(call.req)?;
        let owner_secret = *call.identity.secret_bytes();
        with_seat(
            call.nest_url,
            call.identity,
            async |nest, _identity, seat| {
                let source_nest = bound_box(nest).await?;
                let state = load_backup_state(&seat.handle, source_nest)
                    .await
                    .map_err(|e| err(format!("read the backup destination list: {e}")))?;
                let connector =
                    fauna_client_pair::native_backup_destination_connector(owner_secret);
                let folder_index =
                    fauna_sync_engine::segment_backup::bound_replica_folder_index(nest, None).await;
                let inclusion =
                    fauna_client_pair::native_backup_inclusion_source(owner_secret, folder_index);
                let store = fauna_client_backup::native_store::FileAuditStateStore::at(
                    PathBuf::from(state_path),
                );
                let now = fauna_core::data::Timestamp::now_secs().saturating_add(offset);
                let source_nest_url = nest.nest_url();
                let pass = fauna_client_backup::audit::run_audit_pass(
                    connector.as_ref(),
                    inclusion.as_ref(),
                    &store,
                    &state.backup.destinations,
                    &source_nest_url,
                    &source_nest,
                    None,
                    now,
                )
                .await;
                for degradation in &pass.degradations {
                    tracing::warn!("harness backup audit: {degradation}");
                }
                let rows: Vec<_> = pass
                    .records
                    .iter()
                    .map(|r| {
                        let row = crate::backup_destinations::audit_row(
                            r,
                            &state.backup.destinations,
                            "",
                            now,
                        );
                        (
                            row.destination_id,
                            row.last_passed_at,
                            row.alert_reasons,
                            row.recover_offered,
                        )
                    })
                    .collect();
                encoded!(rows)
            },
        )
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// **The lived-in recovery** from one nest destination into the owner's own
/// regressed nest at `nest_url` — the shared
/// [`fauna_sync_engine::recovery::recover_from_destination`], as
/// `backup_recover_from_destination` resolves it: the destination row from the
/// owner's pinned list, the regressed sets read off the audit store a prior
/// [`fauna_harness_backup_audit_pass`] accepted the regression into (and
/// cleared by the run's duty), the rows recorded under the seat's device id.
///
/// `request` is the dag-cbor array `[destination_id, state_path]`. `out`
/// receives `{"ok": [whole, lines]}` — the outcome's own `is_whole` and its
/// resolved result lines; a run that never reached the shared driver, or that
/// the driver stopped, is a local failure carrying its text.
///
/// # Safety
///
/// As `fauna_harness_create_set`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_backup_recover(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (destination_id, state_path): (String, String) = decode_request!(call.req)?;
        let owner_secret = *call.identity.secret_bytes();
        with_seat(
            call.nest_url,
            call.identity,
            async |nest, identity, seat| {
                let destination = pinned_destination(nest, seat, &destination_id).await?;
                let audit = fauna_client_backup::native_store::FileAuditStateStore::at(
                    PathBuf::from(state_path),
                );
                let outcome = fauna_sync_engine::recovery::recover_from_destination(
                    fauna_sync_engine::recovery::NativeRecovery {
                        source: Arc::clone(nest),
                        owner_secret,
                        device_id: seat_device(identity)?,
                        destination: &destination,
                        audit: &audit,
                    },
                )
                .await
                .map_err(|e| err(format!("recover from {destination_id}: {e}")))?;
                let result = crate::backup_destinations::recovery_result(&outcome);
                encoded!((result.whole, result.lines))
            },
        )
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// **The after-loss re-seed** from a surviving nest destination into the
/// rebuilt nest at `nest_url` — the confirmed re-seed gesture on a nest-kind
/// row, as every app composes it: the shared
/// [`fauna_sync_engine::reseed_pull::pull_back_preflight`] against this audit
/// store's pins, then [`fauna_sync_engine::reseed_pull::pull_back_from_destination`]
/// carrying that preflight as the accepted one (the delivery, each set's
/// `fauna.backup.custody.materialize`, and on a whole outcome the
/// re-enrollment, published). A torn materialize is retried by calling again:
/// the run resumes by content address and the nest completes the flip under
/// its in-flight marker.
///
/// `request` is the dag-cbor array `[destination_id, state_path, carried]`.
/// The audit store may be empty: every ledger then reads unvouched. `carried`
/// is null, or the destination-list bytes [`fauna_harness_backup_enroll`]
/// answered: the rows the owner's own device store holds, which an app's
/// surviving store re-publishes to the rebuilt box at sign-in — a box rebuilt
/// from nothing holds no list, and an app-less seat has no surviving store, so
/// the test carries it. Rows the rebuilt box's list lacks are written into it
/// through the one `mutate_backup` door and published before the leg runs; a
/// row already there is left as it is. `out` receives
/// `{"ok": [whole, lines, reenroll_error, sets]}` — `is_whole`, the resolved
/// result lines, the post-ceremony re-enrollment's failure (null when it
/// enrolled or did not run), and one `[set_name, outcome]` per delivered set,
/// the outcome its debug text (`Materialized …`, `AlreadyLive`, `Refused …`).
///
/// # Safety
///
/// As `fauna_harness_create_set`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_backup_pull_back(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (destination_id, state_path, carried): (
            String,
            String,
            Option<fauna_protocol::ByteBuf>,
        ) = decode_request!(call.req)?;
        let carried: Option<Vec<BackupDestination>> = match carried {
            None => None,
            Some(bytes) => Some(
                fauna_core::encoding::canonical_decode(&bytes)
                    .map_err(|e| err(format!("decode the carried destination list: {e}")))?,
            ),
        };
        let owner_secret = *call.identity.secret_bytes();
        with_seat(
            call.nest_url,
            call.identity,
            async |nest, identity, seat| {
                use fauna_sync_engine::reseed_pull::{
                    NativePullBack, NativePullBackPreflight, pull_back_from_destination,
                    pull_back_preflight,
                };
                if let Some(rows) = carried {
                    fauna_client_config::mutate_backup(&seat.handle, bound_box(nest).await?, |s| {
                        for row in rows {
                            if !s.backup.destinations.contains(&row) {
                                s.backup.destinations.push(row);
                            }
                        }
                    })
                    .await
                    .map_err(|e| err(format!("carry the destination list to the box: {e}")))?;
                    seat.publish().await?;
                }
                let destination = pinned_destination(nest, seat, &destination_id).await?;
                let audit = fauna_client_backup::native_store::FileAuditStateStore::at(
                    PathBuf::from(state_path),
                );
                let accepted = pull_back_preflight(NativePullBackPreflight {
                    owner_secret,
                    destination: &destination,
                    audit: &audit,
                })
                .await
                .map_err(|e| {
                    err(format!(
                        "the re-seed preflight against {destination_id}: {e}"
                    ))
                })?;
                let ran = pull_back_from_destination(NativePullBack {
                    target: Arc::clone(nest),
                    owner_secret,
                    device_id: seat_device(identity)?,
                    destination: &destination,
                    store: &seat.handle,
                    folder_keys: Arc::new(seat.custody()),
                    audit: Some(&audit),
                    accepted: Some(accepted),
                })
                .await
                .map_err(|e| err(format!("re-seed from {destination_id}: {e}")))?;
                // The re-enrollment's write reaches the nest, as an app's would.
                seat.publish().await?;
                let lines: Vec<String> = fauna_client_backup::reseed::result_lines(&ran.outcome)
                    .iter()
                    .map(|l| l.resolve_nested(fauna_i18n::strings::lookup))
                    .collect();
                let sets: Vec<(String, String)> = ran
                    .outcome
                    .sets
                    .iter()
                    .map(|s| (s.set.set_name.clone(), format!("{:?}", s.outcome)))
                    .collect();
                encoded!((ran.outcome.is_whole(), lines, ran.reenroll_error, sets))
            },
        )
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four exports decode their request before they dial anything, so a
    /// malformed one is a local failure with no nest at the far end — the
    /// shape every Python wrapper relies on to tell its own bug from a
    /// refusal.
    #[test]
    fn a_malformed_request_is_a_local_failure_before_any_dial() {
        type Export = unsafe extern "C" fn(
            *const c_char,
            *const u8,
            u32,
            *const u8,
            u32,
            *mut FfiBuffer,
        ) -> i32;
        let exports: [(&str, Export); 4] = [
            ("enroll", fauna_harness_backup_enroll),
            ("audit", fauna_harness_backup_audit_pass),
            ("recover", fauna_harness_backup_recover),
            ("pull_back", fauna_harness_backup_pull_back),
        ];
        let url = c"http://127.0.0.1:1";
        let secret = [7u8; 32];
        // dag-cbor `true` — no export's request is a bare bool.
        let req = [0xF5u8];
        for (name, export) in exports {
            let mut out = FfiBuffer {
                data: std::ptr::null_mut(),
                len: 0,
            };
            // SAFETY: every pointer is valid for the call.
            let rc =
                unsafe { export(url.as_ptr(), secret.as_ptr(), 32, req.as_ptr(), 1, &mut out) };
            assert_eq!(rc, -1, "{name}: a malformed request fails locally");
        }
    }
}
