//! **The custodian-NEST serve door — how this nest hands a held copy back to
//! its owner's own fleet.**
//!
//! Owner: `docs/goal/architecture/account-replica-posture.md` § Replica
//! posture → *The custody grant + ceremony*, the custodian-NEST serve door
//! bullet (rulings 1–4 and 7); ledger: `account-data-plane.md` § Implementation
//! status today.
//!
//! The mirror image of [`crate::custody_admission`]: that module admits a
//! CUSTODIAN pulling from an owner's nest; this one admits an OWNER-FLEET
//! device reading back what this nest holds as a custodian, from the custodied
//! store the hosting pump fills (`custody_hosting_worker`), never from the
//! nest's own sync tables (which hold nothing of a custodied owner).
//!
//! | Door | Answered by |
//! |---|---|
//! | `fauna.sync.changes.list` (both item-class arms) | `fauna_peer_sync::store_serve::serve_changes_list` |
//! | `fauna.segments.list` | `fauna_peer_sync::store_serve::serve_segment_listing` |
//! | `GET /api/v1/segments/{kind}/{actor}/{id}[/meta]` | `fauna_peer_sync::store_serve::serve_segment_part` |
//!
//! — the ONE store-serving implementation the peer channel also calls, so the
//! two serve sides cannot drift (ruling 1).
//!
//! **Admission is three layers, each re-derived from rows (ruling 3):**
//!
//! 1. `fauna.auth.custodied_fleet_handshake` (`auth_core::custodied_fleet_auth_core`)
//!    authenticates the device key and writes its register row
//!    (`custody_hosting_fleet`).
//! 2. [`is_custodied_fleet_device`] — the class, on every dispatch: a register
//!    row names this key, its owner has a live hosting row here, and the key is
//!    not on the owner's exclusion set.
//! 3. [`admit_custodied_fleet_scope`] — the scope, on every request at every
//!    door: the class's own facts again, plus the scope inside the union of the
//!    live rows' witness scope sets, resolving the pair store to answer from.
//!
//! **Add a door, call [`admit_custodied_fleet_scope`] (or its segment-plane
//! spelling [`admit_custodied_fleet_segment_plane`]) — never re-derive the
//! verdict.**

use std::path::PathBuf;

use fauna_core::custody_grant::{CustodyGrant, CustodyScopeSet};
use fauna_core::encoding::EmbedAsBytes;
use fauna_protocol::RpcError;
use fauna_protocol::scope::AdmittedScopes;

use crate::db::CacheDb;
use crate::db::custody_hosting::CustodyHostingRow;
use crate::routes::AppState;

/// One of the owner's hosting rows the door may serve from: its host (which
/// names the pair store) and its owner-signed grant, decoded.
struct LiveRow {
    host: Vec<u8>,
    grant: CustodyGrant,
}

/// What the owner's hosting rows say about one device, now.
enum FleetView {
    /// No register row for the key, or its owner names no 32-byte account.
    NotFleet,
    /// The owner's grants name the device as removed (ruling 4) — refused
    /// whatever else holds.
    Excluded,
    /// The owner's live rows (witness unexpired, not refused by the owner's
    /// nest), newest `updated_at` first; empty = nothing serves.
    Live { owner: [u8; 32], rows: Vec<LiveRow> },
}

/// The current wall clock in the witness's unit (microseconds).
fn now_micros() -> fauna_core::data::Timestamp {
    fauna_core::data::Timestamp(fauna_core::data::Timestamp::now_millis().saturating_mul(1000))
}

/// Decode a hosting row's witness through the owner signature. An unreadable
/// or forged witness names nothing and authorizes nothing.
fn row_grant(row: &CustodyHostingRow) -> Option<CustodyGrant> {
    let witness: EmbedAsBytes = fauna_core::encoding::canonical_decode(&row.witness).ok()?;
    fauna_core::custody_grant::verified_custody_grant(&witness).ok()
}

/// The owner's hosting rows, filtered to the ones that serve `device_key` —
/// the one read every layer of the door shares.
///
/// - **Exclusion first, over EVERY row** — expired, stopped and refused rows
///   included: a removal is the owner's signed statement, which never reverses
///   (`fauna_peer_sync::admission::CustodiedExclusions::derive`, the peer
///   plane's own derivation, applied custodian-side).
/// - **Live** = witness unexpired (the same window at which the pump stops
///   dialing) and not refused by the owner's nest at the pump's last handshake
///   (`owner_refused_at`, ruling 5). A **stopped** row serves (ruling 4): stop
///   pauses the host's pull, never the owner's reach to what is already held.
/// - Each witness must name the owner the row is filed under — a row whose
///   witness another account signed serves nobody.
async fn owner_view(
    db: &CacheDb,
    owner: [u8; 32],
    device_key: &[u8; 32],
) -> anyhow::Result<FleetView> {
    let rows = db.list_custody_hosting_for_owner(&owner).await?;
    let witnesses: Vec<EmbedAsBytes> = rows
        .iter()
        .filter_map(|(_, row)| fauna_core::encoding::canonical_decode(&row.witness).ok())
        .collect();
    let exclusions = fauna_peer_sync::admission::CustodiedExclusions::derive(witnesses.iter());
    if exclusions
        .get(&owner)
        .is_some_and(|removed| removed.contains(device_key))
    {
        return Ok(FleetView::Excluded);
    }
    Ok(FleetView::Live {
        owner,
        rows: live_rows(owner, rows),
    })
}

/// **Which of an owner's hosting rows serve** — the one row selection every
/// door that answers from a custodied copy shares (this module's fleet doors,
/// and the escrow door's custodied arm through [`custodied_recovery_of`]):
/// witness verified, naming `owner`, unexpired, and not refused by the
/// owner's nest (`owner_refused_at`, ruling 5); a **stopped** row serves
/// (ruling 4). Newest first, in the order `rows` arrived
/// (`list_custody_hosting_for_owner`). Exclusion is NOT applied here — it is
/// a device's question, and the escrow door asks none.
fn live_rows(owner: [u8; 32], rows: Vec<(Vec<u8>, CustodyHostingRow)>) -> Vec<LiveRow> {
    let now = now_micros();
    rows.into_iter()
        .filter(|(_, row)| row.owner_refused_at == 0)
        .filter_map(|(host, row)| {
            let grant = row_grant(&row)?;
            (grant.owner.0 == owner && grant.expires_at.0 >= now.0)
                .then_some(LiveRow { host, grant })
        })
        .collect()
}

/// The register row's view: who the device belongs to, then [`owner_view`].
async fn device_view(db: &CacheDb, device_key: &[u8; 32]) -> anyhow::Result<FleetView> {
    let Some(fleet) = db.get_custody_hosting_fleet(device_key).await? else {
        return Ok(FleetView::NotFleet);
    };
    let Ok(owner) = <[u8; 32]>::try_from(fleet.owner_actor_id.as_slice()) else {
        return Ok(FleetView::NotFleet);
    };
    owner_view(db, owner, device_key).await
}

/// **The class predicate** — the whole basis of
/// `CallerClass::CustodiedFleet`, asked by `caller_class_for_actor` on every
/// dispatch: a register row names `device_key`, its owner has at least one
/// live hosting row on this nest, and the key is not excluded. Row-derived, no
/// session registry — so the remove teardown, an expiry, an owner-nest refusal
/// or a re-minted grant naming the device as removed each end the class at the
/// very next dispatch.
pub(crate) async fn is_custodied_fleet_device(
    db: &CacheDb,
    device_key: &[u8; 32],
) -> anyhow::Result<bool> {
    Ok(matches!(
        device_view(db, device_key).await?,
        FleetView::Live { rows, .. } if !rows.is_empty()
    ))
}

/// **The handshake's hosting check** (ruling 2): does this nest hold a live
/// hosting row for `owner` that would serve `device_key`? Asked BEFORE the
/// device has a register row — the handshake is what writes it.
pub(crate) async fn owner_hosting_serves(
    db: &CacheDb,
    owner: [u8; 32],
    device_key: &[u8; 32],
) -> anyhow::Result<bool> {
    Ok(matches!(
        owner_view(db, owner, device_key).await?,
        FleetView::Live { rows, .. } if !rows.is_empty()
    ))
}

/// Does one live row's grant admit `scope_text` for `owner`? The custody
/// door's own vocabulary (`AdmittedScopes`, the one the peer seam evaluates),
/// with the content-scope account binding: under the `Account` form a content
/// scope is covered only when its id IS the owner (the coverage-enumeration
/// ruling — `AllOfAccountSinglePrincipal` shape-checks a stranger's scope
/// string too), and a co-authored (`conv`) scope is covered only by the
/// explicit-list form naming it exactly (the shared-audience carve-out —
/// `AllOfAccountSinglePrincipal` excludes it structurally).
fn grant_admits(grant: &CustodyGrant, owner: &[u8; 32], scope_text: &str) -> bool {
    let verdict = match &grant.scopes {
        CustodyScopeSet::Account => AdmittedScopes::AllOfAccountSinglePrincipal,
        CustodyScopeSet::Scopes(named) => AdmittedScopes::Named(named.clone()),
        // A set this build cannot name covers no scope.
        CustodyScopeSet::Unknown(_) => AdmittedScopes::Named(Vec::new()),
    };
    if !verdict.admits(scope_text) {
        return false;
    }
    match scope_text.parse::<fauna_protocol::scope::Scope>() {
        Ok(fauna_protocol::scope::Scope::Content(content))
            if !fauna_protocol::scope::is_co_authored_scope(scope_text) =>
        {
            content.scope_id() == owner
        }
        _ => true,
    }
}

/// What a door answers from once a fleet request is admitted: the custodied
/// owner and the pair store's directory under the hosting root.
pub(crate) struct FleetAdmission {
    pub(crate) owner: [u8; 32],
    pub(crate) pair_dir: PathBuf,
}

/// The one refusal, in every direction — no register row, excluded, nothing
/// live, a scope outside coverage — so a refused device learns exactly what a
/// stranger learns. LOUD, never an empty page: to a walk an empty page reads
/// as "converged" (the coverage-enumeration corollary).
fn refused() -> RpcError {
    crate::rpc_errors::permission_denied_ns(
        "sync",
        "no live held custody admits this scope for this device",
    )
}

/// **The predicate every door shares** (ruling 3): resolve the device's
/// owner, re-derive the class's own facts, admit `scope_text` against the
/// union of the live rows' witness scope sets, and name the pair store of the
/// NEWEST live row admitting it — where two hosts on this nest hold one
/// owner, the door answers from that row's store, never a union (stated,
/// accepted).
pub(crate) async fn admit_custodied_fleet_scope(
    state: &AppState,
    device_key: &[u8; 32],
    scope_text: &str,
) -> Result<FleetAdmission, RpcError> {
    let view = device_view(&state.db, device_key)
        .await
        .map_err(|e| crate::rpc_errors::internal_ns("sync", format!("fleet lookup: {e}")))?;
    let FleetView::Live { owner, rows } = view else {
        return Err(refused());
    };
    // Newest first by construction (`list_custody_hosting_for_owner`).
    let Some(row) = rows
        .into_iter()
        .find(|row| grant_admits(&row.grant, &owner, scope_text))
    else {
        return Err(refused());
    };
    let Some(root) = state.custody_hosting_root.as_ref() else {
        // A nest without a hosting root holds nothing to serve.
        return Err(refused());
    };
    Ok(FleetAdmission {
        owner,
        pair_dir: root
            .join(hex::encode(&row.host))
            .join(fauna_core::hex32::encode(&owner)),
    })
}

/// The bulk plane's spelling of [`admit_custodied_fleet_scope`]: the segment
/// requests address bytes by `(kind, actor)`, where `actor` is the scope id
/// (the owner for a single-principal kind, the channel for `conv`). Translated
/// into the content scope `content:<kind>:<actor_hex>` and admitted through the
/// same predicate; a kind the scope vocabulary does not know (a placement
/// journal included — floor plaintext, served to its owner's nest alone)
/// names no covered plane and refuses. Returns the admission and the scope
/// text the custodied store files the segments under.
pub(crate) async fn admit_custodied_fleet_segment_plane(
    state: &AppState,
    device_key: &[u8; 32],
    actor: &[u8; 32],
    kind: &str,
) -> Result<(FleetAdmission, String), RpcError> {
    if fauna_sync_engine::segment_backup::is_placement_serve_kind(kind) {
        return Err(refused());
    }
    let content = fauna_protocol::scope::ContentScope::new(kind, *actor).map_err(|_| refused())?;
    let scope_text = content.to_string();
    let admission = admit_custodied_fleet_scope(state, device_key, &scope_text).await?;
    Ok((admission, scope_text))
}

/// Open the pair store an admission names, on a serving thread of its own —
/// `None` when the pump has not yet created it (nothing held yet: the door
/// answers empty rather than minting a store dir on a read).
///
/// The writer identity tag is the NEST's own key, as the pump opens it
/// (`custody_hosting_worker::open_custodied_store`): the custodied store never
/// authors. A handle per request is the first shape; a per-owner cache can
/// follow if a measurement shows it matters.
pub(crate) async fn open_pair_store(
    state: &AppState,
    admission: &FleetAdmission,
) -> anyhow::Result<Option<fauna_peer_sync::store_serve::ServeStoreHandle>> {
    if !tokio::fs::try_exists(&admission.pair_dir)
        .await
        .unwrap_or(false)
    {
        return Ok(None);
    }
    let dir = admission.pair_dir.clone();
    let owner_hex = fauna_core::hex32::encode(&admission.owner);
    let nest_pub = state.nest_identity.signing_key.verifying_key().to_bytes();
    let handle =
        fauna_peer_sync::store_serve::ServeStoreHandle::open_on_thread(move || async move {
            use anyhow::Context;
            let backend = fauna_account_store::sqlite::SqliteBackend::open(&dir)
                .context("open custodied store backend")?;
            fauna_account_store::store::AccountStore::open(
                backend,
                &owner_hex,
                fauna_account_store::types::WriterId(nest_pub),
            )
            .await
            .context("open custodied store")
        })
        .await?;
    Ok(Some(handle))
}

/// **The custodied recovery material this nest holds for `owner`** — what
/// the escrow door's custodied arm answers from (`identity-succession.md`
/// § Seed escrow → *The custodian copy*, ruling 3): the pair store of the
/// NEWEST live hosting row for `owner` ([`live_rows`] — the fleet doors' own
/// selection, so the two cannot drift), read on the blocking pool (the store
/// futures are `!Send`). `None` when no live row names `owner` here, or its
/// pair store was never created — both "not custodied", which the door
/// answers exactly as it answers an unknown actor.
pub(crate) async fn custodied_recovery_of(
    state: &AppState,
    owner: [u8; 32],
) -> anyhow::Result<Option<fauna_account_store::types::CustodiedRecovery>> {
    let Some(root) = state.custody_hosting_root.as_ref() else {
        return Ok(None);
    };
    let rows = state.db.list_custody_hosting_for_owner(&owner).await?;
    let Some(row) = live_rows(owner, rows).into_iter().next() else {
        return Ok(None);
    };
    let owner_hex = fauna_core::hex32::encode(&owner);
    let dir = root.join(hex::encode(&row.host)).join(&owner_hex);
    if !tokio::fs::try_exists(&dir).await.unwrap_or(false) {
        return Ok(None);
    }
    let nest_pub = state.nest_identity.signing_key.verifying_key().to_bytes();
    tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(async move {
            use anyhow::Context;
            let backend = fauna_account_store::sqlite::SqliteBackend::open(&dir)
                .context("open custodied store backend")?;
            let store = fauna_account_store::store::AccountStore::open(
                backend,
                &owner_hex,
                fauna_account_store::types::WriterId(nest_pub),
            )
            .await
            .context("open custodied store")?;
            store.custodied_recovery().await.map(Some)
        })
    })
    .await
    .map_err(|e| anyhow::anyhow!("custodied recovery read task: {e}"))?
}

// ── The doors ────────────────────────────────────────────────────────────────

/// `fauna.sync.changes.list` for a `CustodiedFleet` caller — both item-class
/// arms, answered from the pair store by the device's own frontier
/// (ruling 1). Reached from the handler BEFORE any of its own branches (the
/// federation relay, the file-row feed), so a fleet bearer can never fall
/// through to the nest's own tables.
///
/// The request carries no `of_owner` (ruling 2): the door resolves the
/// custodied account from the device, so a fleet request naming one is
/// refused loudly rather than read as a custodian's.
pub(crate) async fn serve_changes_list(
    state: &AppState,
    device_key: &[u8; 32],
    req: &fauna_protocol::sync::SyncChangesListRequest,
) -> Result<bytes::Bytes, RpcError> {
    use fauna_peer_sync::store_serve::{ServeRefusal, changes_list_scope, serve_changes_list};

    let invalid = |detail: &str| crate::rpc_errors::coded_ns("sync", "invalid_request", detail);
    let refusal = |r: ServeRefusal| match r {
        ServeRefusal::Unsupported(detail) => invalid(detail),
        ServeRefusal::Internal(e) => crate::rpc_errors::internal_ns("sync", format!("{e:#}")),
    };
    if req.of_owner.is_some() {
        return Err(invalid(
            "the custodied-fleet door resolves the owner from the device; of_owner is a \
             custodian's field",
        ));
    }
    let (item_class, scope) = changes_list_scope(req).map_err(refusal)?;
    let admission = admit_custodied_fleet_scope(state, device_key, &scope).await?;
    let reply = match open_pair_store(state, &admission)
        .await
        .map_err(|e| crate::rpc_errors::internal_ns("sync", format!("{e:#}")))?
    {
        Some(store) => serve_changes_list(&store, item_class, scope, req)
            .await
            .map_err(refusal)?,
        // Nothing pulled yet: an empty page is the honest answer for a scope
        // the grant admits and the store does not yet hold.
        None => fauna_protocol::sync::SyncChangesListReply::default(),
    };
    crate::rpc_errors::encode_reply(&reply)
}

/// `fauna.segments.list` for a `CustodiedFleet` caller: the adopted segments
/// of `kind` the pair store holds for the scope `(kind, actor)` names. `Err`
/// is the predicate's refusal, which the caller maps onto the segment plane's
/// one `not_owner` answer so the door stays no oracle.
pub(crate) async fn serve_segments_list(
    state: &AppState,
    device_key: &[u8; 32],
    actor: &[u8; 32],
    kind: &str,
) -> Result<fauna_protocol::segments::SegmentsListReply, FleetSegmentError> {
    let (admission, scope) = admit_custodied_fleet_segment_plane(state, device_key, actor, kind)
        .await
        .map_err(|_| FleetSegmentError::Refused)?;
    let Some(store) = open_pair_store(state, &admission)
        .await
        .map_err(FleetSegmentError::Internal)?
    else {
        return Ok(fauna_protocol::segments::SegmentsListReply {
            segments: Vec::new(),
            next_segment_id: 0,
            extra: Default::default(),
        });
    };
    fauna_peer_sync::store_serve::serve_segment_listing(&store, scope, kind.to_string())
        .await
        .map_err(FleetSegmentError::Internal)
}

/// One half of a held segment pair for a `CustodiedFleet` caller — the byte
/// route's fleet arm. `Ok(None)` = not held (404).
pub(crate) async fn serve_segment_part(
    state: &AppState,
    device_key: &[u8; 32],
    actor: &[u8; 32],
    kind: &str,
    segment_id: u32,
    part: fauna_peer_sync::store_serve::SegmentPart,
) -> Result<Option<fauna_peer_sync::store_serve::SegmentPartBody>, FleetSegmentError> {
    let (admission, scope) = admit_custodied_fleet_segment_plane(state, device_key, actor, kind)
        .await
        .map_err(|_| FleetSegmentError::Refused)?;
    let Some(store) = open_pair_store(state, &admission)
        .await
        .map_err(FleetSegmentError::Internal)?
    else {
        return Ok(None);
    };
    fauna_peer_sync::store_serve::serve_segment_part(
        &store,
        scope,
        kind.to_string(),
        segment_id,
        part,
    )
    .await
    .map_err(FleetSegmentError::Internal)
}

/// The bulk plane's two outcomes besides an answer — each surface spells them
/// in its own vocabulary (`fauna.segments.not_owner` / `internal` on the WS
/// list, `403` / `500` on the byte route).
pub(crate) enum FleetSegmentError {
    Refused,
    Internal(anyhow::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::identity::ActorKeypair;

    fn grant(owner: &ActorKeypair, scopes: CustodyScopeSet) -> CustodyGrant {
        CustodyGrant {
            grant_id: vec![0x11; 16],
            owner: owner.actor_id(),
            custodian_key: [0x22; 32],
            scopes,
            minted_at: fauna_core::data::Timestamp(0),
            expires_at: fauna_core::data::Timestamp(u64::MAX),
            removed_devices: Vec::new(),
        }
    }

    /// The account binding: an `Account` grant covers the owner's own
    /// single-principal content and account-state scopes, never a stranger's
    /// content scope and never a co-authored one; the explicit list covers
    /// exactly what it names, a `conv` channel included.
    #[test]
    fn the_grant_admits_the_owners_scopes_and_nothing_else() {
        let owner = ActorKeypair::generate();
        let o = owner.actor_id().0;
        let stranger = [0x5Au8; 32];
        let account = grant(&owner, CustodyScopeSet::Account);
        let own_post = fauna_protocol::scope::ContentScope::new("post", o)
            .unwrap()
            .to_string();
        let their_post = fauna_protocol::scope::ContentScope::new("post", stranger)
            .unwrap()
            .to_string();
        let conv = fauna_protocol::scope::ContentScope::new("conv", stranger)
            .unwrap()
            .to_string();
        assert!(grant_admits(&account, &o, &own_post));
        assert!(grant_admits(
            &account,
            &o,
            fauna_protocol::account_state::ACCOUNT_STATE_SCOPE
        ));
        assert!(
            !grant_admits(&account, &o, &their_post),
            "a stranger's plane"
        );
        assert!(!grant_admits(&account, &o, &conv), "the carve-out");

        let named = grant(&owner, CustodyScopeSet::Scopes(vec![conv.clone()]));
        assert!(grant_admits(&named, &o, &conv), "named exactly");
        assert!(!grant_admits(&named, &o, &own_post), "not named");
    }
}
