//! **The owner-fleet restore leg, natively** — the custodian dial pass's NEST
//! arm and the restore gesture over this crate's transports
//! (`account-replica-posture.md` § Replica posture → *The custody grant +
//! ceremony*, the custodian-NEST serve door bullet, ruling 6; *Corollary — the
//! cold device*).
//!
//! The walk itself — the URL policy, the revocation check, the handshake-then-
//! pull-only walk, the plane-first opener, the bound-identity check, the
//! sealed count — is [`fauna_account_plane::custodian_restore`]'s, once for
//! every host, re-exported here at its old path. This module is the native
//! seam ([`NativeCustodianDial`]: the pinned-TLS custodied-fleet nest client,
//! its byte plane, the process pin store) and the pass-side wrappers that
//! read this leg's [`WithdrawalSnapshot`].
//!
//! **Never a standing dial.** The arm runs on a pass whose own-nest walk was
//! [`fauna_transport::NestPath::Unavailable`], or on an explicit restore
//! gesture ([`NestArmTrigger::RestoreGesture`]). While the owner's nest lives
//! it is a superset of the custodian's copy by construction (the custodian's
//! pump pulls FROM it), so a dial then would be the always-on obligation the
//! receipt ruling refused, for no convergence gain. A DEVICE custodian keeps
//! its standing peer dial.
//!
//! **Not gated on the peer transport.** The door rides the nest wire, never
//! the peer plane (ruling 1), so a device whose peer leg is off — brake on,
//! participation off, never bound — still restores.

use anyhow::Result;
use ed25519_dalek::SigningKey;
use std::sync::Arc;

use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_protocol::scope::ContentScope;

pub use fauna_account_plane::custodian_restore::{
    CustodianDial, CustodianRestoreOutcome, CustodianSession, FleetDialIdentity, NestArmReport,
};

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

/// The native custodian dial: the custodied-fleet bearer over the pinned-TLS
/// nest client (`fauna_client::ws_custodied_fleet_bearer`), its byte plane
/// ([`crate::nest_client::SyncClient`] under the same bearer) and this
/// process's pin store for the bound-identity read.
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeCustodianDial;

impl CustodianDial for NativeCustodianDial {
    type Rpc = Arc<fauna_client::NestClient>;
    type Bytes = crate::nest_client::SyncClient;

    async fn connect(
        &self,
        url: &str,
        owner: [u8; 32],
        writer_key: &SigningKey,
        authorization: &fauna_core::encoding::EmbedAsBytes,
    ) -> Result<CustodianSession<Self::Rpc, Self::Bytes>> {
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
        let device_id = writer_key.verifying_key().to_bytes();
        let bytes = crate::nest_client::SyncClient::new(Arc::clone(client.auth()), &device_id);
        Ok(CustodianSession { rpc: client, bytes })
    }

    fn pinned_identity(&self, url: &str) -> Option<[u8; 32]> {
        fauna_client::trust::pinned_identity(&fauna_client::trust::authority_of(url))
    }

    async fn close(&self, session: CustodianSession<Self::Rpc, Self::Bytes>) {
        session.rpc.disconnect().await;
    }
}

/// Run the nest arm over every NEST-form `custodian-endpoints` row in
/// `store`, natively — the shared arm
/// ([`fauna_account_plane::custodian_restore::nest_custodian_arm`]) over
/// [`NativeCustodianDial`] and this leg's revocation snapshot, when `trigger`
/// lets it run at all.
#[allow(clippy::too_many_arguments)] // the writer-identity bundle, spelled out (mirrors the peer leg)
pub async fn nest_custodian_arm<B: StoreBackend>(
    store: &AccountStore<B>,
    owner: [u8; 32],
    schedule: &fauna_core::crypto::AccountStateKeySchedule,
    trust: &crate::generation_tip::GenerationTrust,
    writer_key: &SigningKey,
    authorization: &fauna_core::encoding::EmbedAsBytes,
    generation_custody: Option<&dyn crate::generation_tip::RetainedKeyCustody>,
    content_scopes: &[ContentScope],
    revoked: &WithdrawalSnapshot<Vec<u8>>,
    trigger: NestArmTrigger,
) -> Result<NestArmReport> {
    if !trigger.runs() {
        return Ok(NestArmReport::default());
    }
    let id = FleetDialIdentity {
        owner,
        schedule,
        trust,
        writer_key,
        authorization,
        generation_custody,
        // Inside a pass the pass's own escrow-recovery step keys what the
        // walk brought back (the seed-leg role's holder's), door and all.
        seed: None,
    };
    fauna_account_plane::custodian_restore::nest_custodian_arm(
        store,
        &NativeCustodianDial,
        id,
        content_scopes,
        &revoked.custody_revoked_view(),
    )
    .await
}

/// The restore gesture at a typed address, natively — the shared gesture
/// ([`fauna_account_plane::custodian_restore::restore_at_address`]) over
/// [`NativeCustodianDial`] and this leg's revocation snapshot.
pub async fn restore_at_address<B: StoreBackend>(
    store: &AccountStore<B>,
    url: &str,
    id: FleetDialIdentity<'_>,
    revoked: &WithdrawalSnapshot<Vec<u8>>,
    content_scopes: &mut dyn FnMut() -> Vec<ContentScope>,
) -> Result<CustodianRestoreOutcome> {
    fauna_account_plane::custodian_restore::restore_at_address(
        store,
        &NativeCustodianDial,
        url,
        id,
        &revoked.custody_revoked_view(),
        content_scopes,
    )
    .await
}
