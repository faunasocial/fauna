//! User-class custody-**hosting** doors — the custodian-nest runtime's stage
//! (b) registration surface (`account-data-plane.md` § Replica posture → The
//! custody grant + ceremony, the device-or-nest bullet, item 6):
//! `fauna.custody.hosting.{register,list,remove}` (remove is the
//! reclaim: stop pauses, remove frees the row and — with the pair's last
//! row — the custodied store).
//!
//! The HOST user (the account that signed a NEST-anchored custody accept)
//! deposits the hosting row on its own nest here; the nest's pump then runs
//! the pull leg against the row with no host device running. Each handler
//! derives the host from the authenticated connection (`actor_id`), never a
//! wire param — a caller only ever deposits and lists its **own** rows.
//!
//! **The register door refuses a row the pump could never use** rather than
//! letting it rest and fail on the first pass: the witness must decode,
//! verify under the owner it names, name THIS nest's identity as
//! `custodian_key` (only the host's signed accept nominates the nest — but a
//! witness binding some other principal is junk here regardless of how it was
//! minted), match the request's owner and grant id, and be unexpired; the
//! owner URL must pass the counterparty dial policy at **nest** scope (the pump re-checks every pass, this is the learn-early half),
//! and the deposit must fit the bounds: past
//! `MAX_CUSTODY_HOSTING_ROWS_PER_HOST` a *new* row is refused, and
//! `retained_bytes_cap` is clamped to `MAX_RETAINED_BYTES_CAP`. Decode → gate →
//! call-DB → encode; `crate::db::custody_hosting` owns the SQL.

use std::time::Duration;

use fauna_protocol::custody::{
    ADMIN_HOSTING_LIST_KIND, ADMIN_HOSTING_REMOVE_KIND, AdminHostingListReply,
    AdminHostingListRequest, AdminHostingRemoveReply, AdminHostingRemoveRequest, AdminHostingRow,
    HOSTING_LIST_KIND, HOSTING_REGISTER_KIND, HOSTING_REMOVE_KIND, HostingItem, HostingListReply,
    HostingListRequest, HostingRegisterReply, HostingRegisterRequest, HostingRemoveReply,
    HostingRemoveRequest,
};
use fauna_protocol::decode_strict as decode;

use crate::bridge_routing_handlers::{encode_reply, internal, malformed, require_class};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

fn parse_owner_hex(hex_str: &str) -> Result<[u8; 32], fauna_protocol::RpcError> {
    let bytes =
        hex::decode(hex_str).map_err(|_| malformed("owner_actor_id must be hex".to_string()))?;
    bytes
        .try_into()
        .map_err(|_| malformed("owner_actor_id must be 32 bytes".to_string()))
}

fn hosting_register_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, HOSTING_REGISTER_KIND).await?;
            let req: HostingRegisterRequest = decode(&payload).map_err(malformed)?;
            let owner = parse_owner_hex(&req.owner_actor_id)?;

            // The dial-policy gate, ingest half: a URL the
            // pump would refuse every pass is refused where the depositing
            // client can see why. The scope is NEST, not the device
            // default — the same predicate the pump re-checks with, so
            // the two doors cannot disagree about what this nest will dial.
            fauna_core::counterparty_url::validate_counterparty_nest_url_scoped(
                &req.owner_nest_url,
                fauna_core::counterparty_url::DialScope::Nest {
                    public_deployment: state.is_public_deployment(),
                },
            )
            .map_err(|reason| malformed(format!("owner_nest_url refused: {reason}")))?;

            // The witness must be one this nest's pump can actually redeem:
            // decodes, verifies under the named owner, admits THIS nest's
            // identity (a witness binding any other principal — a device, a
            // different nest — could never pass the owner-side custody
            // handshake when this nest dials), unexpired, and consistent with
            // the request's own owner + grant id (the row key must be the
            // witness's own ceremony, or the read-back and the pull leg would
            // disagree about which custody this is).
            let witness: fauna_core::encoding::EmbedAsBytes =
                fauna_core::encoding::canonical_decode(&req.witness)
                    .map_err(|e| malformed(format!("witness unreadable: {e}")))?;
            let nest_key = state.nest_identity.public_key_bytes();
            let now_ms = fauna_core::data::Timestamp::now_millis();
            let admission = fauna_core::custody_grant::verify_custody_witness(
                &witness,
                &nest_key,
                &fauna_core::identity::ActorId(owner),
                fauna_core::data::Timestamp(now_ms.saturating_mul(1000)),
            )
            .map_err(|e| malformed(format!("witness does not admit this nest: {e}")))?;
            if admission.grant_id != req.grant_id.as_slice() {
                return Err(malformed(
                    "grant_id does not match the witness's own ceremony".to_string(),
                ));
            }

            // The deposit bounds. The decision and its reasoning
            // live in `fauna_core::custody_ceremony::bound_hosting_deposit`,
            // beside the two constants and shared so a host's app can render the
            // same verdict without a second copy of the rule. The probe is ONE
            // query because the door needs both halves and two reads would race.
            let (held_rows, is_rewrite) = state
                .db
                .count_custody_hosting(&actor_id, &req.grant_id)
                .await
                .map_err(internal)?;
            let retained_bytes_cap = fauna_core::custody_ceremony::bound_hosting_deposit(
                held_rows,
                is_rewrite,
                req.retained_bytes_cap,
            )
            .map_err(malformed)?;

            // The tier-bounded accounting's learn-early half: a
            // host whose already-held bytes meet its tier bound is told so here
            // rather than depositing a row the pump would immediately squeeze to
            // nothing. A REWRITE is exempt for the same reason it is exempt from
            // the row cap — stop and re-budget are this verb, and a host over
            // bound must be able to shrink. Off (`enforce_tier_quotas`) or no
            // users row ⇒ no cap, the fail-open the sync plane's metering takes.
            if !is_rewrite && *state.enforce_tier_quotas.read().await {
                let bound = state
                    .db
                    .get_user_tier_max_storage_bytes(&actor_id)
                    .await
                    .map_err(internal)?
                    .map(|b| b.max(0) as u64);
                if let Some(bound) = bound {
                    let held = state
                        .db
                        .sum_custody_hosting_held(&actor_id)
                        .await
                        .map_err(internal)?;
                    if held >= bound {
                        return Err(malformed(format!(
                            "custody-hosting storage bound reached ({held} of {bound} bytes \
                             already held for others) — stop or remove a hosting row, or ask \
                             the admin for a larger tier"
                        )));
                    }
                }
            }

            state
                .db
                .put_custody_hosting(
                    &actor_id,
                    &req.grant_id,
                    &owner,
                    &req.witness,
                    &req.owner_nest_url,
                    &req.owner_devices,
                    retained_bytes_cap,
                    req.stopped,
                )
                .await
                .map_err(|e| malformed(e.to_string()))?;
            encode_reply(&HostingRegisterReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn hosting_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, HOSTING_LIST_KIND).await?;
            let _req: HostingListRequest = decode(&payload).map_err(malformed)?;
            let rows = state
                .db
                .list_custody_hosting(&actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&HostingListReply {
                rows: rows
                    .into_iter()
                    .map(|r| HostingItem {
                        grant_id: serde_bytes::ByteBuf::from(r.grant_id),
                        owner_actor_id: hex::encode(&r.owner_actor_id),
                        owner_nest_url: r.owner_nest_url,
                        retained_bytes_cap: r.retained_bytes_cap,
                        stopped: r.stopped,
                        updated_at: r.updated_at.max(0) as u64,
                        held_bytes: r.held_bytes,
                        last_receipt_at: r.last_receipt_at,
                        extra: Default::default(),
                    })
                    .collect(),
                extra: Default::default(),
            })
        })
    })
}

// ── The admin surface ─────────────────────────────────
// The nest-wide list + remove the per-caller doors above deliberately are
// not. Required by the *no client-causable unrecoverable nest state*
// invariant: any account holder can plant hosting rows over the User-class
// register door, so an admin must be able to see and drop them from the app —
// before these doors the only remedy was `sqlite3` on `nest.db` plus `rm -rf`
// under `{data_dir}/custody-hosting`.

fn admin_hosting_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission(
                &state.db,
                &actor_id,
                ADMIN_HOSTING_LIST_KIND,
                internal,
            )
            .await?;
            let _req: AdminHostingListRequest = decode(&payload).map_err(malformed)?;
            let rows = state
                .db
                .list_all_custody_hosting()
                .await
                .map_err(internal)?;
            encode_reply(&AdminHostingListReply {
                rows: rows
                    .into_iter()
                    .map(|(host, r)| AdminHostingRow {
                        host_actor_id: hex::encode(&host),
                        item: HostingItem {
                            grant_id: serde_bytes::ByteBuf::from(r.grant_id),
                            owner_actor_id: hex::encode(&r.owner_actor_id),
                            owner_nest_url: r.owner_nest_url,
                            retained_bytes_cap: r.retained_bytes_cap,
                            stopped: r.stopped,
                            updated_at: r.updated_at.max(0) as u64,
                            held_bytes: r.held_bytes,
                            last_receipt_at: r.last_receipt_at,
                            extra: Default::default(),
                        },
                        extra: Default::default(),
                    })
                    .collect(),
                extra: Default::default(),
            })
        })
    })
}

/// The one teardown both remove doors share (the Admin door and the host's
/// own User door): drop the `(host, grant)` row and — only when it was the
/// `(host, owner)` pair's LAST row, since every grant of the pair reads one
/// store dir — the custodied store beneath it. Returns
/// `(removed, store_dropped)`; an absent row is `(false, false)`, the honest
/// no-op.
async fn remove_hosting_row_and_store(
    state: &crate::routes::AppState,
    host: &[u8; 32],
    grant_id: &[u8],
) -> Result<(bool, bool), fauna_protocol::RpcError> {
    // Pre-read for the owner: the custodied store dir is keyed
    // `(host, owner)` and shared by every grant of the pair.
    let Some(row) = state
        .db
        .get_custody_hosting(host, grant_id)
        .await
        .map_err(internal)?
    else {
        return Ok((false, false));
    };

    let removed = state
        .db
        .delete_custody_hosting(host, grant_id)
        .await
        .map_err(internal)?;

    // The store falls only with the pair's LAST row — another grant
    // for the same (host, owner) still reads it.
    let mut store_dropped = false;
    if removed
        && state
            .db
            .count_custody_hosting_for_host_owner(host, &row.owner_actor_id)
            .await
            .map_err(internal)?
            == 0
        && let Some(root) = state.custody_hosting_root.as_ref()
    {
        let owner: Option<[u8; 32]> = row.owner_actor_id.as_slice().try_into().ok();
        if let Some(owner) = owner {
            let dir = root
                .join(hex::encode(host))
                .join(fauna_core::hex32::encode(&owner));
            match tokio::fs::remove_dir_all(&dir).await {
                Ok(()) => store_dropped = true,
                // Never held any bytes — the pump may not have run.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    // The ROW is gone (the pump will not redial); the
                    // orphaned dir is reported, not fatal — a re-run
                    // of remove cannot recreate the row to retry, so
                    // failing here would trade a stray dir for a
                    // confusing error on an already-successful drop.
                    tracing::warn!(
                        dir = %dir.display(),
                        error = %e,
                        "custody-hosting store teardown failed; the row is removed"
                    );
                }
            }
        }
    }
    Ok((removed, store_dropped))
}

/// The host's own reclaim: stop is a pause, THIS is what frees the
/// bytes. Host-derived from the authenticated connection like register/list —
/// a grant id under any other host simply does not match `(host, grant)` and
/// answers `removed: false`, so cross-host deletion is unrepresentable.
fn hosting_remove_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, HOSTING_REMOVE_KIND).await?;
            let req: HostingRemoveRequest = decode(&payload).map_err(malformed)?;
            let (removed, store_dropped) =
                remove_hosting_row_and_store(&state, &actor_id, &req.grant_id).await?;
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "custody_hosting.remove",
                    Some(&hex::encode(actor_id)),
                    Some(&hex::encode(&req.grant_id)),
                )
                .await;
            encode_reply(&HostingRemoveReply {
                removed,
                store_dropped,
                extra: Default::default(),
            })
        })
    })
}

fn admin_hosting_remove_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission(
                &state.db,
                &actor_id,
                ADMIN_HOSTING_REMOVE_KIND,
                internal,
            )
            .await?;
            let req: AdminHostingRemoveRequest = decode(&payload).map_err(malformed)?;
            let host: [u8; 32] = hex::decode(&req.host_actor_id)
                .ok()
                .and_then(|v| v.try_into().ok())
                .ok_or_else(|| malformed("host_actor_id must be 64-char hex".to_string()))?;

            let (removed, store_dropped) =
                remove_hosting_row_and_store(&state, &host, &req.grant_id).await?;

            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "custody_hosting.remove",
                    Some(&req.host_actor_id),
                    Some(&hex::encode(&req.grant_id)),
                )
                .await;
            encode_reply(&AdminHostingRemoveReply {
                removed,
                store_dropped,
                extra: Default::default(),
            })
        })
    })
}

pub fn register_custody_hosting_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        HOSTING_REGISTER_KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: hosting_register_handler(),
        },
    );
    b.add(
        HOSTING_LIST_KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: hosting_list_handler(),
        },
    );
    b.add(
        HOSTING_REMOVE_KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: hosting_remove_handler(),
        },
    );
    b.add(
        ADMIN_HOSTING_LIST_KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: admin_hosting_list_handler(),
        },
    );
    b.add(
        ADMIN_HOSTING_REMOVE_KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: admin_hosting_remove_handler(),
        },
    );
}
