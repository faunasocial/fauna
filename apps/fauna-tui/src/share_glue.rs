//! tui's **host** for the shared share-plane driver (`p2p.md` § Cross-user
//! shared-set transfer): the app-shaped seams only.
//!
//! The plane's decisions — the pump loop, the `FolderRef`-first spec join
//!, the last-known-spec hold across a nest-down window, the brake
//! composition, the advertisement decision, the surface readings — live in
//! `fauna_sync_engine::share_glue`, lifted there 2026-08-21 so the six apps
//! still owed this plane inherit them instead of re-deriving them; the host's
//! app-agnostic answers (the roster consult, the key bindings, the
//! advertisement send, the two nest reads) and the durable sink are
//! `fauna_client_share_host`'s, lifted 2026-08-27. What is left here is what
//! genuinely differs per app: which seat type the offline-share panel holds,
//! the bind door's device label + transport factory, and the two `UiMessage`
//! nudges its paint needs.
//!
//! # Why the glue starts at `AccountStoreReady`, not the post-auth hook
//!
//! Rule 7's cached brake evidence, the discovery cache's dial targets, the
//! sink's durable write, and the transfer ledger all live behind the account
//! store runtime's handle, which arrives asynchronously
//! (`session::establish` spawns the assembly). Binding earlier could use only
//! the live brake read and still could not pump, so the ready edge IS the
//! earliest honest start; a failed assembly leaves the plane down for the
//! session — the same degraded posture every other store consumer takes. The
//! shared loop exits the way `session::account_store_watch` does: when the
//! runtime's `data_version` read errs (sign-out's deterministic shutdown),
//! dropping its seat clone with it.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use fauna_client::NestClient;
use fauna_client_share_host::ShareHostSeams;
use fauna_core::feature_gate::EffectivePolicy;
use fauna_core::folder_keys::FolderEngineKeys;
use fauna_sync_engine::account_runtime::AccountStoreHandle;
use fauna_sync_engine::share_glue::{
    SharePlane, SharePlaneHost, pump_interval_from_env, serve_status_label, transfer_name_label,
    transfer_progress_label, transfer_state_label,
};

use crate::app::{DataMessage, UiMessage};
use crate::offline_share::{CeremonySeat, SessionSeat};

// The shared surface this app's paint and its settings state name. Re-served
// under the old spellings so the call sites read unchanged. (The serve-info
// TYPES left with the agent adapters: both are shared now, composed by
// `agent_share_access` rather than re-implemented per app.)
pub(crate) use fauna_sync_engine::share_glue::{ReplicaAccess, ServeStatus, SharePlaneCell};

/// Everything the glue composes over, gathered on the main thread at the
/// `AccountStoreReady` edge.
pub(crate) struct ShareGlue {
    pub nest: Arc<NestClient>,
    pub secret_hex: String,
    pub account: AccountStoreHandle,
    pub agent: ReplicaAccess,
    pub conversations: Arc<fauna_conversations::ConversationsSession>,
    pub tx: UnboundedSender<UiMessage>,
    /// This sign-in's seat slot — the panel's own
    /// (`SettingsState.offline_share`), so the plane and the panel bind one
    /// actor-keyed endpoint between them.
    pub session_seat: SessionSeat,
    pub cell: SharePlaneCell,
    /// Spool ground for pulled bodies (`spool/manifests/<hex>` +
    /// `spool/chunks/<hex>` under here). Transient by contract: a lost body
    /// skips its row and the next pass re-pulls.
    pub spool_root: PathBuf,
}

/// tui's [`SharePlaneHost`] — the app half of the driver. The app-agnostic
/// answers are one-line delegations to the shared [`ShareHostSeams`]; what this
/// struct adds is the seat type, the bind door's app half, and the nudges.
struct TuiShareHost {
    seams: ShareHostSeams,
    tx: UnboundedSender<UiMessage>,
    session_seat: SessionSeat,
}

#[async_trait::async_trait]
impl SharePlaneHost for TuiShareHost {
    type Seat = CeremonySeat;

    fn membership(&self) -> Arc<dyn fauna_peer_share::admission::SetMembership + Send + Sync> {
        self.seams.membership()
    }

    async fn bind_seat(
        &self,
        evidence: Option<Vec<String>>,
        membership: &Arc<dyn fauna_peer_share::admission::SetMembership + Send + Sync>,
        group_roster: &Arc<dyn fauna_peer_share::admission::GroupRosterState + Send + Sync>,
    ) -> Result<(Arc<CeremonySeat>, Vec<std::net::SocketAddr>), String> {
        crate::offline_share::bind_share_plane_seat(
            &self.session_seat,
            &self.seams.secret_hex,
            evidence,
            membership,
            group_roster,
        )
        .await
    }

    fn seat_node<'a>(
        &self,
        seat: &'a CeremonySeat,
    ) -> &'a fauna_client_capabilities::group_ceremony_node::CeremonyNode {
        &seat.node
    }

    fn session_seat(&self) -> &SessionSeat {
        &self.session_seat
    }

    fn seat_bound(&self) {
        // The seat is already the panel's — both doors bind through the
        // session's `SessionSeat` — so this is only the repaint nudge.
        let _ = self
            .tx
            .send(UiMessage::Data(DataMessage::SharePlaneSeatBound));
    }

    fn state_changed(&self) {
        let _ = self
            .tx
            .send(UiMessage::Data(DataMessage::SharePlaneChanged));
    }

    async fn live_capabilities(&self) -> Result<Vec<String>, String> {
        self.seams.live_capabilities().await
    }

    async fn transfer_policy(&self) -> Option<EffectivePolicy> {
        self.seams.transfer_policy().await
    }

    async fn key_bindings(&self) -> Result<Vec<FolderEngineKeys>, String> {
        self.seams.key_bindings().await
    }

    async fn send_share_endpoints(&self, channel_hex: &str, bytes: Vec<u8>) -> Result<(), String> {
        self.seams.send_share_endpoints(channel_hex, bytes).await
    }
}

/// The whole plane, as one spawned task: register the durable sink, then hand
/// the composed seams to the shared driver.
pub(crate) async fn run(glue: ShareGlue) {
    // The sixth sink, registered up front (its timing rationale is at the
    // shared definition).
    fauna_client_share_host::install_durable_sink(&glue.conversations, glue.account.clone());

    let Ok(own) = fauna_core::identity::ActorKeypair::from_secret_hex(&glue.secret_hex) else {
        tracing::debug!("share glue: malformed secret; the plane stays down");
        return;
    };

    fauna_sync_engine::share_glue::run(SharePlane {
        host: Arc::new(TuiShareHost {
            seams: ShareHostSeams {
                nest: glue.nest,
                secret_hex: glue.secret_hex,
                conversations: glue.conversations,
                folder_keys: {
                    let account = glue.account.clone();
                    Arc::new(
                        fauna_client_account_runtime::folder_keys::PlaneFolderKeys::new(
                            move || Some(account.clone()),
                        ),
                    )
                },
            },
            tx: glue.tx,
            session_seat: glue.session_seat,
        }),
        account: glue.account,
        replica: glue.agent,
        own_actor: own.actor_id(),
        cell: glue.cell,
        spool_root: glue.spool_root,
        pump_interval: pump_interval_from_env(),
    })
    .await;
}

/// The `share-serve-status` reading for a state.
pub(crate) fn serve_status_text(status: ServeStatus) -> String {
    crate::wizard::localized(&serve_status_label(status))
}

/// The `share-transfer-name` reading for one row: the set plus the peer's
/// short id.
pub(crate) fn outcome_name_text(outcome: &fauna_sync_engine::share_pump::SetPullOutcome) -> String {
    crate::wizard::localized(&transfer_name_label(outcome))
}

/// The `share-transfer-progress` reading for one row: what the latest pass
/// moved.
pub(crate) fn outcome_progress_text(
    outcome: &fauna_sync_engine::share_pump::SetPullOutcome,
) -> String {
    crate::wizard::localized(&transfer_progress_label(outcome))
}

/// The `share-transfer-state` reading for one outcome — including the
/// transfer gate's honest "limited by …" (Dim-3: a bound names the tier that
/// set it, never a silent stall).
///
/// `resolve_nested`, not `localized`: the limited arm's `{source}` argument is
/// itself an i18n key (the binding tier), so a plain resolve would paint the
/// raw key at the user.
pub(crate) fn outcome_state_text(
    outcome: &fauna_sync_engine::share_pump::SetPullOutcome,
) -> String {
    transfer_state_label(outcome).resolve_nested(fauna_i18n::strings::lookup)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The storage floor's reading is the limited reading with a source that
    /// is not a tier: it resolves through the same nested path, so the user
    /// reads the words and never the key.
    #[test]
    fn a_storage_limited_outcome_reads_limited_by_free_space_on_this_device() {
        let outcome = fauna_sync_engine::share_pump::SetPullOutcome {
            admitted: true,
            storage_limited: true,
            ..Default::default()
        };
        assert_eq!(
            outcome_state_text(&outcome),
            "Limited by free space on this device"
        );
    }
}
