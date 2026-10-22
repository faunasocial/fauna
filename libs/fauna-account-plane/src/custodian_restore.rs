//! **The owner-fleet restore walk** — how an owner's device reads back what a
//! NEST custodian holds for its account (`account-replica-posture.md`
//! § Replica posture → *The custody grant + ceremony*, the custodian-NEST serve
//! door bullet, ruling 6; *Corollary — the cold device*, its web clause: one
//! walk, one crate).
//!
//! A `custodian-endpoints` row carrying `custodian_nest_url` is not a peer
//! target: it is the custodian dial pass's NEST ARM ([`nest_custodian_arm`]).
//! Per target, in order:
//!
//! 1. the dialer's own revocation view first — a revoked grant is never dialed;
//! 2. the counterparty URL policy at device scope (the *What a custodian will
//!    dial* bullet: the accept's URL inherits the policy at its first dial
//!    site, and this is that site);
//! 3. the custodied-fleet handshake (`fauna.auth.custodied_fleet_handshake`,
//!    the device key + its root-signed `DeviceAuthorization`; ruling 7: the
//!    transport trust stack plus the accept's key↔URL binding, no nest
//!    counter-proof) — the HOST's seam ([`CustodianDial`]);
//! 4. the ordinary reading walk, pull-only — both account-state planes, then
//!    each content scope's bulk segments (the bootstrap contract's order) and
//!    its coordinate walk.
//!
//! **Two entries, one dial path** (bound (a)): the pump's registry-driven arm
//! ([`nest_custodian_arm`]) and the user's restore gesture at a typed address
//! ([`restore_at_address`]) both run [`dial_and_walk`] per target, on every
//! host — so a typed address and a registered one cannot drift, and neither
//! can web and native.
//!
//! **The dial is the host's.** Natively the pinned-TLS nest client and its
//! byte plane (`fauna_sync_engine::custodian_nest_restore`); on web an
//! anonymous browser socket that mints the fleet bearer and a one-shot bearer
//! socket whose actor is the device key, plus a streaming `fetch` of the
//! segment byte route (`fauna-wasm`'s `account_runtime`). Everything else —
//! the URL policy, the revocation check, the walk, the plane-first opener, the
//! bound-identity check, the sealed count — is this module's, once.

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use std::collections::HashSet;

use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_protocol::RpcRequester;
use fauna_protocol::scope::ContentScope;

use crate::bootstrap_source::{NestBootstrapSource, SegmentBytes};
use crate::custodian_nest_targets::{CustodianNestTarget, custodian_nest_targets};
use crate::generation_tip::{GenerationTrust, RetainedKeyCustody};
pub use crate::host_legs::CustodianRestoreOutcome;

/// One connected custodian session — the WS-RPC surface the walk reads
/// (`fauna.sync.changes.list`, `fauna.segments.list`) and the byte plane the
/// bulk half streams through, both under the fleet bearer the handshake
/// minted. Lives for one [`dial_and_walk`] call: the arm is never standing.
pub struct CustodianSession<R, S> {
    pub rpc: R,
    pub bytes: S,
}

/// The host's custodian dial — the one seam between the shared walk and a
/// platform's transports (*Corollary — the cold device*, the web clause: *the
/// dial is the host's seam*).
#[allow(async_fn_in_trait)] // static dispatch only; per-impl Send inference (native Send, web !Send)
pub trait CustodianDial {
    /// The session's requester. `Clone` because the plane-first opener reads
    /// the fleet plane through a clone of it.
    type Rpc: RpcRequester<Error: fauna_protocol::RpcErrorClass> + Clone;
    /// The session's byte plane.
    type Bytes: SegmentBytes;

    /// The custodied-fleet handshake at `url` as `writer_key` (the device's
    /// principal) under `owner`, presenting `authorization` — and a session
    /// riding the bearer it minted. `Err` is the one opaque *not admitted*:
    /// a wrong address, a nest that is down, or one holding nothing for this
    /// account.
    async fn connect(
        &self,
        url: &str,
        owner: [u8; 32],
        writer_key: &SigningKey,
        authorization: &fauna_core::encoding::EmbedAsBytes,
    ) -> Result<CustodianSession<Self::Rpc, Self::Bytes>>;

    /// The host's pin for `url`'s origin, if it holds one — the bound
    /// identity read's first answer
    /// ([`fauna_client_core::nest_trust::read_bound_identity`]); without one
    /// the read is a possession proof over the session itself.
    fn pinned_identity(&self, url: &str) -> Option<[u8; 32]>;

    /// Bring the session down after the walk. The default drops it.
    async fn close(&self, session: CustodianSession<Self::Rpc, Self::Bytes>) {
        drop(session);
    }
}

/// The dial of a host that has none — never constructed (an `Option` of it is
/// always `None`), the type a host's "no custodian dial" names: the seedless
/// host and the tests that run the pass alone.
#[derive(Debug, Clone, Copy)]
pub enum NoCustodianDial {}

impl std::fmt::Display for NoCustodianDial {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {}
    }
}

impl fauna_protocol::RpcErrorClass for NoCustodianDial {
    fn is_rejection(&self) -> bool {
        match *self {}
    }
}

impl RpcRequester for NoCustodianDial {
    type Error = NoCustodianDial;

    async fn request<Req, Reply>(&self, _: &'static str, _: Req) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        match *self {}
    }
}

impl SegmentBytes for NoCustodianDial {
    async fn fetch_half(
        &self,
        _: &str,
        _: &str,
        _: u32,
        _: fauna_account_store::segments::SegmentHalf,
        _: u64,
        _: &mut impl fauna_account_store::segments::SegmentSink,
    ) -> Result<()> {
        match *self {}
    }
}

impl CustodianDial for NoCustodianDial {
    type Rpc = NoCustodianDial;
    type Bytes = NoCustodianDial;

    async fn connect(
        &self,
        _: &str,
        _: [u8; 32],
        _: &SigningKey,
        _: &fauna_core::encoding::EmbedAsBytes,
    ) -> Result<CustodianSession<Self::Rpc, Self::Bytes>> {
        match *self {}
    }

    fn pinned_identity(&self, _: &str) -> Option<[u8; 32]> {
        match *self {}
    }
}

/// The restore gesture as a host whose legs hold an optional dial runs it
/// ([`crate::account_driver::HostLegs::restore_from_custodian`]): the slot's
/// enrollment witness (the one thing the fleet handshake cannot do without,
/// so its absence is an `Err` naming it), the revocation view derived from
/// the store now by [`custody_revoked_grant_ids`] (a read failure is the
/// gesture's `Err`, never a silent *Revoked*), then [`restore_at_address`]
/// with the context's seed and the slot as the generation custody, so the
/// plane-first opener keys the restored generations (bound (e)). Web's host
/// runs it; the native host keeps a standing snapshot its pass refreshes
/// (from this same fold) and calls [`restore_at_address`] over that.
pub async fn restore_with_derived_view<B: StoreBackend, D: CustodianDial>(
    dial: Option<&D>,
    actor_id: fauna_core::identity::ActorId,
    ctx: crate::account_driver::RestoreCtx<'_, B>,
    url: &str,
    content_scopes: &mut dyn FnMut() -> Vec<ContentScope>,
) -> Result<CustodianRestoreOutcome> {
    let Some(dial) = dial else {
        anyhow::bail!("restoring from a custodian nest is not available in this app yet")
    };
    let Some(witness) = ctx.slot.device_authorization() else {
        anyhow::bail!(
            "this device is not enrolled in its account yet — a custodian nest admits an \
             enrolled device only"
        );
    };
    let revoked = custody_revoked_grant_ids(ctx.store, actor_id)
        .await
        .context("custodian nest restore: the custody-revocation view")?;
    restore_at_address(
        ctx.store,
        dial,
        url,
        FleetDialIdentity {
            owner: actor_id.0,
            schedule: ctx.schedule,
            trust: ctx.trust,
            writer_key: ctx.writer_key,
            authorization: &witness.wire,
            generation_custody: Some(ctx.slot),
            seed: ctx.seed,
        },
        &|grant_id| revoked.contains(grant_id),
        content_scopes,
    )
    .await
}

/// What one run of the arm did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NestArmReport {
    /// Custodian nests dialed (past the revocation view and the URL policy).
    pub dialed: usize,
    /// Of those, admitted at the handshake with both account-state walks done.
    pub admitted: usize,
    /// Withheld before any dial: a revoked grant, or a refused URL.
    pub withheld: usize,
    /// Account-state rows applied across the admitted walks.
    pub applied: usize,
}

/// The device's writer identity, as every custodian-nest dial presents it:
/// `owner` is the account the device belongs to (the handshake's claimed
/// owner); `writer_key` is the machine's device principal — the key
/// `authorization` names; `generation_custody` is attached to the
/// account-state planes as the peer leg's walks attach it; `seed` — the
/// identity seed and the attested predecessors' keypairs — is what the walk's
/// plane-first opener keys the restored generations with, into that custody
/// (`account-replica-posture.md` § *Escrow custody*, ruling (2)).
#[derive(Clone, Copy)]
pub struct FleetDialIdentity<'a> {
    pub owner: [u8; 32],
    pub schedule: &'a fauna_core::crypto::AccountStateKeySchedule,
    pub trust: &'a GenerationTrust,
    pub writer_key: &'a SigningKey,
    pub authorization: &'a fauna_core::encoding::EmbedAsBytes,
    pub generation_custody: Option<&'a dyn RetainedKeyCustody>,
    pub seed: Option<(&'a [u8; 32], &'a [fauna_core::identity::ActorKeypair])>,
}

/// The custody-revocation view, derived from the store: custody-class grant
/// ids the account's grant-event log has seen and no longer counts current —
/// the fold of the succession ledger's event rows
/// (`fauna.state.succession-ledger`) as `actor_id`'s identity folds them. An
/// id the log has never seen stays admitted: log-first mint means a store the
/// owner's event has not reached yet MAY serve for one plane-sync window —
/// T13's ratified honest bound — and so does an empty log.
///
/// Only the READ fold's events count — those the folded chain signed — so a
/// stranger's `Revoke` row, which any tip holder could write, never refuses a
/// live grant here. The native pass installs it into its withdrawal snapshot;
/// web derives it at the gesture. `Err` is a ledger the fold refuses — the
/// caller's error, never a silent empty set.
pub async fn custody_revoked_grant_ids<B: StoreBackend>(
    store: &AccountStore<B>,
    actor_id: fauna_core::identity::ActorId,
) -> Result<HashSet<Vec<u8>>> {
    use fauna_client_capabilities::custody_grants::is_custody_grant;
    let ledger = crate::succession_ledger_rows::read_succession_ledger(store, actor_id)
        .await
        .context("the account's grant log")?;
    let current: HashSet<Vec<u8>> = fauna_core::grant_event::current_grants(&ledger)
        .into_iter()
        .filter(|g| is_custody_grant(&g.scope))
        .map(|g| g.grant_id.clone())
        .collect();
    let seen: HashSet<Vec<u8>> = ledger
        .grant_events
        .iter()
        // A Revoke event is keyless/scope-empty.
        .filter(|e| is_custody_grant(&e.scope) || e.scope.is_empty())
        .map(|e| e.grant_id.clone())
        .collect();
    Ok(seen.difference(&current).cloned().collect())
}

/// Run the nest arm over every NEST-form `custodian-endpoints` row in
/// `store`. Per-target isolation: an unreachable or refusing custodian nest is
/// counted (`dialed` but not `admitted`) and the next target proceeds.
/// `withdraws` is the dialer's revocation view; inside a pass the content
/// scopes are the pass's own list. When the arm runs at all (never a standing
/// dial) is the caller's decision.
pub async fn nest_custodian_arm<B: StoreBackend, D: CustodianDial>(
    store: &AccountStore<B>,
    dial: &D,
    id: FleetDialIdentity<'_>,
    content_scopes: &[ContentScope],
    withdraws: &dyn Fn(&[u8]) -> bool,
) -> Result<NestArmReport> {
    let mut report = NestArmReport::default();
    for target in custodian_nest_targets(store).await? {
        // The dialer's own revocation view, BEFORE anything leaves the box
        // (underived refuses — the snapshot's fail-closed posture).
        if withdraws(target.grant_id.as_slice()) {
            report.withheld += 1;
            continue;
        }
        if let Err(reason) = fauna_core::counterparty_url::validate_counterparty_nest_url_scoped(
            &target.url,
            fauna_core::counterparty_url::DialScope::Device,
        ) {
            tracing::warn!(
                grant = %hex::encode(&target.grant_id),
                "custodian nest restore: URL refused by dial policy ({reason}) — not dialed"
            );
            report.withheld += 1;
            continue;
        }
        report.dialed += 1;
        match dial_and_walk(
            store,
            dial,
            &target.url,
            id,
            &mut || content_scopes.to_vec(),
            false,
        )
        .await
        {
            Ok(walked) => {
                report.admitted += 1;
                report.applied += walked.applied;
            }
            Err(e) => tracing::debug!(
                grant = %hex::encode(&target.grant_id),
                "custodian nest restore: unreachable or refused this pass: {e:#}"
            ),
        }
    }
    Ok(report)
}

/// The restore gesture at a typed address — the cold device's entry and the
/// warm explicit one (`account-replica-posture.md` § *Corollary — the cold
/// device*, bounds (a)–(e); the surface is `ui/nests.md` § Trust facet —
/// restore from a custodian nest). In order:
///
/// - **(a)** the counterparty URL policy at device scope — a refusal is the
///   gesture's own outcome, and nothing is dialed;
/// - **(b)** a custody row on record whose `custodian_nest_url` names this
///   origin applies its grant to the revocation view (`withdraws`) exactly as
///   the arm does; no such row means nothing on record to withdraw — the
///   owner's gesture is the authorization;
/// - the one dial path ([`dial_and_walk`]) — a handshake refusal or an
///   unreachable nest is one opaque *not admitted*;
/// - **(d)** `content_scopes` is called after the state planes landed, so a
///   cold device walks the scopes the restore itself taught it — and, with
///   `id.seed`, after the plane-first opener keyed every generation the
///   restored escrow-wrap rows open (*Escrow custody*, ruling (2)), so the
///   content scopes walk with the keys in hand;
/// - **(c)** after the walk, a custody row for this origin must name the
///   identity the connection was bound to; a mismatch is its own outcome and
///   the rows stand;
/// - **(e)** rows held after the walk under a generation no key this device
///   holds opens are counted as sealed, never as restored.
///
/// `Err` is a store failure only; every answer the user is told is `Ok`.
pub async fn restore_at_address<B: StoreBackend, D: CustodianDial>(
    store: &AccountStore<B>,
    dial: &D,
    url: &str,
    id: FleetDialIdentity<'_>,
    withdraws: &dyn Fn(&[u8]) -> bool,
    content_scopes: &mut dyn FnMut() -> Vec<ContentScope>,
) -> Result<CustodianRestoreOutcome> {
    let url = url.trim();
    if let Err(reason) = fauna_core::counterparty_url::validate_counterparty_nest_url_scoped(
        url,
        fauna_core::counterparty_url::DialScope::Device,
    ) {
        return Ok(CustodianRestoreOutcome::AddressRefused {
            reason: reason.to_string(),
        });
    }
    let origin = fauna_core::counterparty_url::counterparty_origin(url);
    let on_record = |targets: &[CustodianNestTarget]| {
        targets
            .iter()
            .filter(|t| fauna_core::counterparty_url::counterparty_origin(&t.url) == origin)
            .cloned()
            .collect::<Vec<_>>()
    };
    let before = on_record(&custodian_nest_targets(store).await?);
    if before.iter().any(|t| withdraws(t.grant_id.as_slice())) {
        return Ok(CustodianRestoreOutcome::Revoked);
    }
    let walked = match dial_and_walk(store, dial, url, id, content_scopes, true).await {
        Ok(walked) => walked,
        Err(e) => {
            tracing::info!("custodian nest restore: {url} did not admit this device: {e:#}");
            return Ok(CustodianRestoreOutcome::NotAdmitted);
        }
    };
    if let Some(got) = walked.bound {
        let after = on_record(&custodian_nest_targets(store).await?);
        if let Some(row) = after.iter().find(|t| t.custodian_key != got) {
            tracing::warn!(
                "custodian nest restore: {url} answered as {} but the custody row names {}",
                hex::encode(got),
                hex::encode(row.custodian_key)
            );
            return Ok(CustodianRestoreOutcome::IdentityMismatch {
                expected: row.custodian_key,
                got,
            });
        }
    }
    if walked.applied == 0 {
        return Ok(CustodianRestoreOutcome::NothingNew);
    }
    let sealed_rows = sealed_rows(store, &walked.scopes, id).await?;
    Ok(CustodianRestoreOutcome::Restored {
        applied: walked.applied,
        sealed_rows,
    })
}

/// What one admitted dial-and-walk came to.
struct Walked {
    /// Account-state rows applied.
    applied: usize,
    /// Every scope the walk read: the two account-state planes, then the
    /// content scopes derived after them.
    scopes: Vec<String>,
    /// The identity the connection was bound to, when asked for — read on
    /// the walk's own connection, never the nest's own claim.
    bound: Option<[u8; 32]>,
}

/// One custodian nest: handshake, then the reading walk — THE per-target
/// path both entries run, on every host. The session lives for this call
/// only — the arm is never standing, so nothing is kept warm.
async fn dial_and_walk<B: StoreBackend, D: CustodianDial>(
    store: &AccountStore<B>,
    dial: &D,
    url: &str,
    id: FleetDialIdentity<'_>,
    content_scopes: &mut dyn FnMut() -> Vec<ContentScope>,
    read_bound: bool,
) -> Result<Walked> {
    let session = dial
        .connect(url, id.owner, id.writer_key, id.authorization)
        .await
        .context("custodied-fleet handshake/connect refused")?;
    let outcome = async {
        let mut walked = walk(store, &session, id, content_scopes).await?;
        if read_bound {
            match fauna_client_core::nest_trust::read_bound_identity(
                &session.rpc,
                dial.pinned_identity(url),
            )
            .await
            {
                Ok(bound) => walked.bound = Some(bound),
                // The rows walked are verified by their own in-seal
                // signatures; an unreadable binding leaves only the check
                // undone, and says so.
                Err(e) => tracing::warn!(
                    "custodian nest restore: the connection's bound identity is unreadable \
                     ({e}) — the custody-row check is skipped"
                ),
            }
        }
        Ok(walked)
    }
    .await;
    dial.close(session).await;
    outcome
}

/// The reading walk over the custodian nest's session, pull-only. The two
/// account-state planes are the restore's floor and fail the target; then,
/// with the seed in hand, the plane-first escrow opener keys every generation
/// the restored `fauna.state.escrow-wrap` rows open and both planes are
/// re-presented (the pass that keys a generation is the pass that reads it);
/// the content scopes are asked for only after that (bound (d)), and a
/// content scope the custodian does not cover (a co-authored `conv` plane
/// under an `Account` grant refuses loudly by the coverage ruling) is
/// skipped, never the whole restore.
async fn walk<B: StoreBackend, R, S>(
    store: &AccountStore<B>,
    session: &CustodianSession<R, S>,
    id: FleetDialIdentity<'_>,
    content_scopes: &mut dyn FnMut() -> Vec<ContentScope>,
) -> Result<Walked>
where
    R: RpcRequester + Clone,
    S: SegmentBytes,
{
    use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE};

    let mut walked = Walked {
        applied: 0,
        scopes: Vec::new(),
        bound: None,
    };
    let rpc = &session.rpc;
    let mut planes = Vec::with_capacity(2);
    for scope in [ACCOUNT_STATE_SCOPE, ACCOUNT_STATE_FLEET_SCOPE] {
        let mut plane = crate::account_state_plane::AccountStatePlane::new_pull_only(
            store,
            rpc,
            id.schedule,
            id.writer_key,
            id.trust,
            scope,
        )?;
        if let Some(custody) = id.generation_custody {
            plane = plane.with_generation_custody(custody);
        }
        walked.applied += plane
            .walk()
            .await
            .with_context(|| format!("custodian nest walk {scope}"))?
            .applied;
        walked.scopes.push(scope.to_string());
        planes.push(plane);
    }
    // Plane-first keying (ruling (2)): the fleet plane carries the escrow
    // wraps, so it is the plane whose walk names the candidates.
    if let Some((seed, predecessors)) = id.seed
        && let [_, fleet] = planes.as_slice()
    {
        let keyed = crate::generation_escrow_recover::open_plane_wraps(
            store,
            fleet,
            id.trust,
            id.writer_key,
            seed,
            predecessors,
        )
        .await?;
        if keyed > 0 {
            for plane in &planes {
                walked.applied += plane
                    .reconcile()
                    .await
                    .with_context(|| format!("custodian nest re-presentation {}", plane.scope()))?
                    .applied;
            }
        }
    }
    for plane in &planes {
        let relayed = store
            .relay_rows(
                plane.scope(),
                fauna_protocol::account_state::ItemClass::StateEntry.as_wire(),
                &[],
                u32::MAX,
            )
            .await?
            .len();
        tracing::info!(
            scope = plane.scope(),
            applied = walked.applied,
            relayed,
            "custodian nest restore: the state plane walked"
        );
    }
    for cs in content_scopes() {
        let scope = cs.to_string();
        if fauna_account_store::segments::ADOPTABLE_KINDS.contains(&cs.kind()) {
            let source = NestBootstrapSource::new(
                rpc,
                &session.bytes,
                crate::content_scope_plane::binding_for(&cs),
            );
            if let Err(e) = store.bootstrap_scope_segments(&scope, &source).await {
                tracing::debug!(
                    %scope,
                    "custodian nest restore: bulk segment adoption did not complete: {e:#}"
                );
            }
        }
        if let Err(e) = crate::content_scope_plane::ContentScopePlane::new(store, rpc, cs)
            .walk()
            .await
        {
            tracing::debug!(%scope, "custodian nest restore: content walk skipped: {e:#}");
        }
        walked.scopes.push(scope);
    }
    Ok(walked)
}

/// Bound (e)'s count: the rows held in `scopes` that are sealed under a
/// generation no key this device holds opens. A bulk-adopted segment pair is
/// filed byte-for-byte and keyed by nothing (`fauna_account_store::segments`),
/// so what can come back sealed is a relay row — counted per row, never per
/// segment.
async fn sealed_rows<B: StoreBackend>(
    store: &AccountStore<B>,
    scopes: &[String],
    id: FleetDialIdentity<'_>,
) -> Result<usize> {
    let mut sealed = 0usize;
    for scope in scopes {
        for generation in store.relay_generations(scope).await? {
            let keyed = crate::generation_tip::generation_key_for(
                store,
                &generation,
                id.writer_key,
                id.generation_custody,
                None,
            )
            .await?
            .is_some();
            if !keyed {
                sealed += store
                    .relay_rows_sealed_under(scope, &generation)
                    .await?
                    .len();
            }
        }
    }
    Ok(sealed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation_fixture_test_support::{Fixture, fixture};
    use std::cell::Cell;

    /// The session's requester in these tests: every request is a refusal,
    /// and none is expected to be sent.
    #[derive(Clone)]
    struct Unreached;

    #[derive(Debug)]
    struct Never;
    impl std::fmt::Display for Never {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "no custodian in this test")
        }
    }
    impl fauna_protocol::RpcErrorClass for Never {
        fn is_rejection(&self) -> bool {
            false
        }
    }
    impl RpcRequester for Unreached {
        type Error = Never;
        async fn request<Req, Reply>(&self, _: &'static str, _: Req) -> Result<Reply, Never>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            Err(Never)
        }
    }
    impl SegmentBytes for Unreached {
        async fn fetch_half(
            &self,
            _: &str,
            _: &str,
            _: u32,
            _: fauna_account_store::segments::SegmentHalf,
            _: u64,
            _: &mut impl fauna_account_store::segments::SegmentSink,
        ) -> Result<()> {
            anyhow::bail!("no custodian in this test")
        }
    }

    /// A dial that counts its connects and refuses every one — the
    /// handshake's opaque *not admitted*.
    #[derive(Default)]
    struct Refusing {
        connects: Cell<usize>,
    }

    impl CustodianDial for Refusing {
        type Rpc = Unreached;
        type Bytes = Unreached;
        async fn connect(
            &self,
            _: &str,
            _: [u8; 32],
            _: &SigningKey,
            _: &fauna_core::encoding::EmbedAsBytes,
        ) -> Result<CustodianSession<Unreached, Unreached>> {
            self.connects.set(self.connects.get() + 1);
            anyhow::bail!("handshake refused")
        }
        fn pinned_identity(&self, _: &str) -> Option<[u8; 32]> {
            None
        }
    }

    /// No dial in these tests gets as far as presenting it.
    fn no_authorization() -> fauna_core::encoding::EmbedAsBytes {
        fauna_core::encoding::EmbedAsBytes {
            envelope: Vec::new(),
            bytes: Vec::new(),
            signer_auth: None,
        }
    }

    fn id<'a>(
        f: &'a Fixture,
        authorization: &'a fauna_core::encoding::EmbedAsBytes,
    ) -> FleetDialIdentity<'a> {
        FleetDialIdentity {
            owner: [0x0A; 32],
            schedule: &f.schedule,
            trust: &f.trust,
            writer_key: &f.writer_key,
            authorization,
            generation_custody: None,
            seed: None,
        }
    }

    async fn record_nest_custodian(f: &Fixture, grant: u8, url: &str) {
        let row = fauna_core::custodian_endpoints::CustodianEndpoints {
            grant_id: vec![grant; 16],
            custodian_nest_url: Some(url.into()),
            ..Default::default()
        };
        f.put(fauna_account_store::types::StateEntry {
            kind: fauna_protocol::merge_policy::KIND_CUSTODIAN_ENDPOINTS.to_string(),
            key: fauna_core::custody_grant::custody_entry_key(&row.grant_id),
            scope: fauna_protocol::account_state::ACCOUNT_STATE_SCOPE.to_string(),
            value: fauna_core::encoding::canonical_encode(&row).unwrap(),
            merge_meta: None,
            entry_version: 0,
            tombstone: false,
        })
        .await;
    }

    /// Bound (a): a plaintext address off loopback is refused by the URL
    /// policy before anything is dialed, on every host.
    #[tokio::test]
    async fn custodian_restore_refuses_a_plaintext_address_before_any_dial() {
        let f = fixture().await;
        let auth = no_authorization();
        let dial = Refusing::default();
        let outcome = restore_at_address(
            &f.store,
            &dial,
            "http://10.0.0.1:1",
            id(&f, &auth),
            &|_| false,
            &mut Vec::new,
        )
        .await
        .unwrap();
        assert!(
            matches!(outcome, CustodianRestoreOutcome::AddressRefused { .. }),
            "{outcome:?}"
        );
        assert_eq!(dial.connects.get(), 0, "nothing was dialed");
    }

    /// Bound (b): a row on record for this origin whose grant the view
    /// withdraws answers *Revoked*, undialed; with the view admitting it, the
    /// same address is dialed and the refusal is the opaque *not admitted*.
    #[tokio::test]
    async fn custodian_restore_withholds_a_withdrawn_grant_on_record() {
        let f = fixture().await;
        record_nest_custodian(&f, 0xD0, "https://custodian.example/").await;
        let auth = no_authorization();
        let dial = Refusing::default();
        let withdrawn = |g: &[u8]| g == [0xD0; 16].as_slice();
        let outcome = restore_at_address(
            &f.store,
            &dial,
            "https://custodian.example",
            id(&f, &auth),
            &withdrawn,
            &mut Vec::new,
        )
        .await
        .unwrap();
        assert_eq!(outcome, CustodianRestoreOutcome::Revoked);
        assert_eq!(dial.connects.get(), 0);

        let outcome = restore_at_address(
            &f.store,
            &dial,
            "https://custodian.example",
            id(&f, &auth),
            &|_| false,
            &mut Vec::new,
        )
        .await
        .unwrap();
        assert_eq!(outcome, CustodianRestoreOutcome::NotAdmitted);
        assert_eq!(dial.connects.get(), 1, "the one dial path ran once");
    }

    /// The registry arm runs the same per-target sequence: a withdrawn grant
    /// and a refused URL are withheld undialed, a live one is dialed and its
    /// refusal counted, never fatal.
    #[tokio::test]
    async fn custodian_restore_arm_withholds_then_dials_per_target() {
        let f = fixture().await;
        record_nest_custodian(&f, 0xA1, "https://withdrawn.example/").await;
        record_nest_custodian(&f, 0xA2, "http://10.0.0.1:1/").await;
        record_nest_custodian(&f, 0xA3, "https://live.example/").await;
        let auth = no_authorization();
        let dial = Refusing::default();
        let withdrawn = |g: &[u8]| g == [0xA1; 16].as_slice();
        let report = nest_custodian_arm(&f.store, &dial, id(&f, &auth), &[], &withdrawn)
            .await
            .unwrap();
        assert_eq!(
            report,
            NestArmReport {
                dialed: 1,
                admitted: 0,
                withheld: 2,
                applied: 0
            }
        );
        assert_eq!(dial.connects.get(), 1);
    }

    /// An empty grant log is a derived answer: nothing withdrawn.
    #[tokio::test]
    async fn custodian_restore_revocation_fold_of_an_empty_log_withdraws_nothing() {
        let f = fixture().await;
        let revoked = custody_revoked_grant_ids(
            &f.store,
            crate::generation_fixture_test_support::root().actor_id(),
        )
        .await
        .unwrap();
        assert!(revoked.is_empty());
    }
}
