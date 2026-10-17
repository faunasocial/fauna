//! **The owner-fleet restore leg** — how an owner's device reads back what a
//! NEST custodian holds for its account (`account-replica-posture.md`
//! § Replica posture → *The custody grant + ceremony*, the custodian-NEST serve
//! door bullet, ruling 6).
//!
//! A `custodian-endpoints` row carrying `custodian_nest_url` is not a peer
//! target ([`fauna_peer_sync::custodian_dial_targets`] leaves it out): it is
//! the custodian dial pass's NEST ARM, which this module is. Per target, in
//! order:
//!
//! 1. the dialer's own revocation view first — a revoked grant is never dialed;
//! 2. the counterparty URL policy at device scope (the *What a custodian will
//!    dial* bullet: the accept's URL inherits the policy at its first dial
//!    site, and this is that site);
//! 3. the custodied-fleet handshake (`fauna.auth.custodied_fleet_handshake`,
//!    the device key + its root-signed `DeviceAuthorization`; ruling 7: the
//!    transport trust stack plus the accept's key↔URL binding, no nest
//!    counter-proof);
//! 4. the ordinary reading walk, pull-only — both account-state planes, then
//!    each content scope's bulk segments (the bootstrap contract's order) and
//!    its coordinate walk.
//!
//! **Never a standing dial.** The arm runs on a pass whose own-nest walk was
//! [`fauna_transport::NestPath::Unavailable`], or on an explicit restore
//! gesture ([`NestArmTrigger::RestoreGesture`] — the cold-device journey's
//! hook). While the owner's nest lives it is a superset of the custodian's
//! copy by construction (the custodian's pump pulls FROM it), so a dial then
//! would be the always-on obligation the receipt ruling refused, for no
//! convergence gain. A DEVICE custodian keeps its standing peer dial.
//!
//! **Not gated on the peer transport.** The door rides the nest wire, never
//! the peer plane (ruling 1), so a device whose peer leg is off — brake on,
//! participation off, never bound — still restores.

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use std::sync::Arc;

use fauna_account_store::{backend::StoreBackend, store::AccountStore};

use crate::peer_leg::WithdrawalSnapshot;

/// Why the arm is being asked to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NestArmTrigger {
    /// An ordinary pump pass, carrying whether its own-nest walk answered.
    Pass(fauna_transport::NestPath),
    /// An explicit restore gesture — runs whatever the own nest's state.
    RestoreGesture,
}

impl NestArmTrigger {
    /// Whether the arm dials at all (ruling 6: never a standing dial).
    pub fn runs(self) -> bool {
        match self {
            NestArmTrigger::Pass(nest) => nest == fauna_transport::NestPath::Unavailable,
            NestArmTrigger::RestoreGesture => true,
        }
    }
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

/// Run the nest arm over every NEST-form `custodian-endpoints` row in
/// `store`. Per-target isolation: an unreachable or refusing custodian nest is
/// counted (`dialed` but not `admitted`) and the next target proceeds.
///
/// `owner` is the account the device belongs to (the handshake's claimed
/// owner); `writer_key` is the machine's device principal — the key
/// `authorization` names; `generation_custody` is attached to the
/// account-state planes as the peer leg's walks attach it.
#[allow(clippy::too_many_arguments)] // the writer-identity bundle, spelled out (mirrors the peer leg)
pub async fn nest_custodian_arm<B: StoreBackend>(
    store: &AccountStore<B>,
    owner: [u8; 32],
    schedule: &fauna_core::crypto::AccountStateKeySchedule,
    trust: &crate::generation_tip::GenerationTrust,
    writer_key: &SigningKey,
    authorization: &fauna_core::encoding::EmbedAsBytes,
    generation_custody: Option<&dyn crate::generation_tip::RetainedKeyCustody>,
    content_scopes: &[fauna_protocol::scope::ContentScope],
    revoked: &WithdrawalSnapshot<Vec<u8>>,
    trigger: NestArmTrigger,
) -> Result<NestArmReport> {
    let mut report = NestArmReport::default();
    if !trigger.runs() {
        return Ok(report);
    }
    for target in fauna_peer_sync::custodian_nest_targets(store).await? {
        // The dialer's own revocation view, BEFORE anything leaves the box
        // (underived refuses — the snapshot's fail-closed posture).
        if revoked.withdraws(target.grant_id.as_slice()) {
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
        match restore_from(
            store,
            &target.url,
            owner,
            schedule,
            trust,
            writer_key,
            authorization,
            generation_custody,
            content_scopes,
        )
        .await
        {
            Ok(applied) => {
                report.admitted += 1;
                report.applied += applied;
            }
            Err(e) => tracing::debug!(
                grant = %hex::encode(&target.grant_id),
                "custodian nest restore: unreachable or refused this pass: {e:#}"
            ),
        }
    }
    Ok(report)
}

/// One custodian nest: handshake, then the reading walk. The session lives
/// for this call only — the arm is never standing, so nothing is kept warm.
#[allow(clippy::too_many_arguments)] // see nest_custodian_arm
async fn restore_from<B: StoreBackend>(
    store: &AccountStore<B>,
    url: &str,
    owner: [u8; 32],
    schedule: &fauna_core::crypto::AccountStateKeySchedule,
    trust: &crate::generation_tip::GenerationTrust,
    writer_key: &SigningKey,
    authorization: &fauna_core::encoding::EmbedAsBytes,
    generation_custody: Option<&dyn crate::generation_tip::RetainedKeyCustody>,
    content_scopes: &[fauna_protocol::scope::ContentScope],
) -> Result<usize> {
    let client = fauna_client::ws_custodied_fleet_bearer::custodied_fleet_nest_client(
        url,
        owner,
        writer_key.clone(),
        authorization.clone(),
    );
    client
        .connect()
        .await
        .map_err(|e| anyhow::anyhow!("custodied-fleet handshake/connect refused: {e}"))?;
    let outcome = walk(
        store,
        &client,
        writer_key,
        schedule,
        trust,
        generation_custody,
        content_scopes,
    )
    .await;
    client.disconnect().await;
    outcome
}

/// The production reading walk over the custodian nest's session, pull-only.
/// The two account-state planes are the restore's floor and fail the target;
/// a content scope the custodian does not cover (a co-authored `conv` plane
/// under an `Account` grant refuses loudly by the coverage ruling) is skipped,
/// never the whole restore.
async fn walk<B: StoreBackend>(
    store: &AccountStore<B>,
    client: &Arc<fauna_client::NestClient>,
    writer_key: &SigningKey,
    schedule: &fauna_core::crypto::AccountStateKeySchedule,
    trust: &crate::generation_tip::GenerationTrust,
    generation_custody: Option<&dyn crate::generation_tip::RetainedKeyCustody>,
    content_scopes: &[fauna_protocol::scope::ContentScope],
) -> Result<usize> {
    use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE};

    let mut applied = 0usize;
    for scope in [ACCOUNT_STATE_SCOPE, ACCOUNT_STATE_FLEET_SCOPE] {
        let mut plane = crate::account_state_plane::AccountStatePlane::new_pull_only(
            store, client, schedule, writer_key, trust, scope,
        )?;
        if let Some(custody) = generation_custody {
            plane = plane.with_generation_custody(custody);
        }
        applied += plane
            .walk()
            .await
            .with_context(|| format!("custodian nest walk {scope}"))?
            .applied;
    }
    let device_id = writer_key.verifying_key().to_bytes();
    let bytes = crate::nest_client::SyncClient::new(Arc::clone(client.auth()), &device_id);
    for cs in content_scopes {
        let scope = cs.to_string();
        if fauna_account_store::segments::ADOPTABLE_KINDS.contains(&cs.kind()) {
            let source = crate::bootstrap_source::NestBootstrapSource::new(
                client.as_ref(),
                &bytes,
                crate::content_scope_plane::binding_for(cs),
            );
            if let Err(e) = store.bootstrap_scope_segments(&scope, &source).await {
                tracing::debug!(
                    %scope,
                    "custodian nest restore: bulk segment adoption did not complete: {e:#}"
                );
            }
        }
        if let Err(e) =
            crate::content_scope_plane::ContentScopePlane::new(store, client, cs.clone())
                .walk()
                .await
        {
            tracing::debug!(%scope, "custodian nest restore: content walk skipped: {e:#}");
        }
    }
    Ok(applied)
}
