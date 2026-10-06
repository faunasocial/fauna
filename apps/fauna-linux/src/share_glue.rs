//! linux's **host** for the shared share-plane driver (`p2p-shared-set-build.md` § Cross-user
//! shared-set transfer → *Built — the tui app leg*, row 338): the app-shaped
//! seams only.
//!
//! Every decision the plane makes — the pump loop, the `FolderRef`-first spec
//! join, the last-known-spec hold across a nest-down window, rule
//! 7's brake composition, the advertisement decision, the surface readings —
//! lives in `fauna_sync_engine::share_glue`; the host's app-agnostic answers
//! (the roster consult, the key bindings, the advertisement send, the two nest
//! reads) and the durable sink are `fauna_client_share_host`'s, lifted
//! 2026-08-27. This module is the GTK-shaped half: which seat the
//! offline-share panel holds, the bind door's app half, the agent seams, and
//! the two `DataMessage` nudges the Folders page repaints from.
//!
//! # Where it starts, and why not earlier
//!
//! At the **account-store-ready edge** — [`crate::account_runtime::install`]'s
//! `Installed` arm — because rule 7's cached brake evidence, the discovery
//! cache's dial targets, the sink's durable write and the transfer ledger all
//! live behind that handle. Binding earlier could read only the live brake and
//! still could not pump. The seeds ([`ShareGlueSeed`]) are gathered on the GTK
//! main thread before that install spawns: the agent provisioner lives in a
//! GTK-thread `thread_local`, so a tokio task cannot reach for it later.
//!
//! # linux drives the AGENT, like tui — not an in-process engine
//!
//! The per-app note ("linux is in-process, no agent verbs") predates the
//! A3 cutover: `crate::sync::SyncDriver` is retired and resident engines live
//! in the external per-user `fauna-sync-agent` (`crate::sync_agent` module
//! docs). So this leg's serve info and ingest door are the shared agent
//! adapters (`GetShareServeInfo` / `ShareIngest`), exactly tui's.

use std::path::PathBuf;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_share_host::ShareHostSeams;
use fauna_core::feature_gate::EffectivePolicy;
use fauna_core::folder_keys::FolderEngineKeys;
use fauna_core::identity::ActorKeypair;
use fauna_sync_engine::account_runtime::AccountStoreHandle;
use fauna_sync_engine::share_glue::{
    ReplicaAccess, SharePlane, SharePlaneHost, agent_share_access, pump_interval_from_env,
    serve_status_label, transfer_name_label, transfer_progress_label, transfer_state_label,
};

use crate::app::{DataMessage, UiMessage};
use crate::client::{FaunaClient, UiSender};
use crate::offline_share::{CeremonySeat, SessionSeat};

pub use fauna_sync_engine::share_glue::{ServeStatus, SharePlaneCell, SharePlaneState};

/// Everything the plane needs, gathered on the GTK main thread while the
/// account-store assembly is still being spawned. `None` from [`gather`] means
/// a seam is genuinely absent (no agent installed, no conversations session,
/// no config dir) — a legitimate degraded session, so the plane stays down for
/// it rather than half-starting.
pub struct ShareGlueSeed {
    nest: Arc<NestClient>,
    secret_hex: String,
    conversations: Arc<fauna_conversations::ConversationsSession>,
    agent: ReplicaAccess,
    tx: UiSender,
    spool_root: PathBuf,
    runtime: tokio::runtime::Handle,
    cell: SharePlaneCell,
    session_seat: SessionSeat,
}

/// The *current* session's paint cell plus the sender that nudges a repaint,
/// set by [`start`]. The Folders page reads the cell synchronously (e2e
/// convention 11 — paint does no I/O); `None` before the first plane starts
/// and after a sign-out, which the surface renders by painting **nothing** —
/// "no plane this session" is a different fact from "no shared folders to
/// serve", and only the second one is a status line.
static PLANE: std::sync::Mutex<Option<(SharePlaneCell, UiSender)>> = std::sync::Mutex::new(None);

/// The transfer surface's current reading — a synchronous clone for paint.
pub fn state() -> Option<SharePlaneState> {
    let cell = PLANE.lock().unwrap().as_ref().map(|(c, _)| Arc::clone(c))?;
    let state = cell.lock().unwrap().clone();
    Some(state)
}

/// Drop the session's plane state (sign-out / account switch / factory reset)
/// and repaint, so the surface stops showing the outgoing account's
/// transfers. The driver task itself ends on its own when the account
/// runtime's `data_version` read errs.
pub fn forget() {
    let previous = PLANE.lock().unwrap().take();
    if let Some((_, tx)) = previous {
        tx.send(UiMessage::Data(DataMessage::SharePlaneChanged));
    }
}

/// Gather the seams on the GTK main thread. Called by
/// [`crate::account_runtime::install`] before it spawns the assembly.
/// `session_seat` is the Folders page's own slot for this sign-in, so the
/// plane and the panel bind one endpoint between them.
pub fn gather(
    fauna_client: &std::rc::Rc<FaunaClient>,
    session_seat: SessionSeat,
) -> Option<ShareGlueSeed> {
    let (provisioner, runtime) = crate::sync_agent::provisioner_and_runtime()?;
    let conversations = crate::conversations::conv_backend::active_session()?;
    let spool_root = crate::client::fauna_config_dir()?.join("share-spool");
    let secret_hex = fauna_client.secret_hex().to_string();
    ActorKeypair::from_secret_hex(&secret_hex).ok()?;
    let nest = Arc::clone(fauna_client.nest_rpc());
    Some(ShareGlueSeed {
        nest,
        secret_hex,
        conversations,
        agent: agent_share_access(provisioner),
        tx: fauna_client.ui_sender(),
        spool_root,
        runtime,
        cell: SharePlaneCell::default(),
        session_seat,
    })
}

/// Start the plane on the account-store-ready edge: register the durable sink,
/// publish the paint cell, and hand the composed seams to the shared driver.
pub fn start(seed: ShareGlueSeed, account: AccountStoreHandle) {
    // The sixth sink, registered up front (its timing rationale is at the
    // shared definition).
    fauna_client_share_host::install_durable_sink(&seed.conversations, account.clone());

    let Ok(own) = ActorKeypair::from_secret_hex(&seed.secret_hex) else {
        tracing::debug!("[share-glue] malformed secret; the plane stays down");
        return;
    };
    *PLANE.lock().unwrap() = Some((Arc::clone(&seed.cell), seed.tx.clone()));
    // Paint the surface's opening reading now rather than at the first pass:
    // the cell exists from here, and tui's paint — which reads its cell on
    // every redraw — shows the same "no shared folders to serve" line from
    // this same moment. Waiting for the first change would leave the section
    // invisible for a whole pump cadence on a device with nothing to serve,
    // which is precisely the state rule-5 transparency wants said out loud.
    seed.tx
        .send(UiMessage::Data(DataMessage::SharePlaneChanged));

    let plane = SharePlane {
        host: Arc::new(LinuxShareHost {
            seams: ShareHostSeams {
                nest: seed.nest,
                secret_hex: seed.secret_hex,
                conversations: seed.conversations,
                folder_keys: crate::account_runtime::folder_key_store(),
            },
            tx: seed.tx,
            session_seat: seed.session_seat,
        }),
        account,
        replica: seed.agent,
        own_actor: own.actor_id(),
        cell: seed.cell,
        spool_root: seed.spool_root,
        pump_interval: pump_interval_from_env(),
    };
    seed.runtime
        .spawn(fauna_sync_engine::share_glue::run(plane));
}

/// linux's [`SharePlaneHost`] — the app half of the driver. The app-agnostic
/// answers are one-line delegations to the shared [`ShareHostSeams`]; what this
/// struct adds is the seat type, the bind door's app half, and the nudges.
struct LinuxShareHost {
    seams: ShareHostSeams,
    tx: UiSender,
    session_seat: SessionSeat,
}

#[async_trait::async_trait]
impl SharePlaneHost for LinuxShareHost {
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
        self.tx
            .send(UiMessage::Data(DataMessage::SharePlaneSeatBound));
    }

    fn state_changed(&self) {
        self.tx
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

/// The `share-serve-status` reading for a state.
pub fn serve_status_text(status: ServeStatus) -> String {
    serve_status_label(status).resolve(crate::i18n::strings::lookup)
}

/// The `share-transfer-name` reading for one row: the set plus the peer's
/// short id.
pub fn transfer_name_text(outcome: &fauna_sync_engine::share_pump::SetPullOutcome) -> String {
    transfer_name_label(outcome).resolve(crate::i18n::strings::lookup)
}

/// The `share-transfer-progress` reading for one row: what the latest pass
/// moved.
pub fn transfer_progress_text(outcome: &fauna_sync_engine::share_pump::SetPullOutcome) -> String {
    transfer_progress_label(outcome).resolve(crate::i18n::strings::lookup)
}

/// The `share-transfer-state` reading for one row — including the transfer
/// gate's honest "limited by …" (Dim-3: a bound names the tier that set it,
/// never a silent stall).
///
/// `resolve_nested`, not `resolve`: the limited arm's `{source}` argument is
/// itself an i18n key (the binding tier), so a plain resolve would paint the
/// raw key at the user.
pub fn transfer_state_text(outcome: &fauna_sync_engine::share_pump::SetPullOutcome) -> String {
    transfer_state_label(outcome).resolve_nested(crate::i18n::strings::lookup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_sync_engine::share_pump::SetPullOutcome;

    /// Every serve status resolves to its own English reading through
    /// linux's own lookup — the shared label's keys are in the table this app
    /// links, not just tui's.
    #[test]
    fn serve_status_readings_resolve_and_differ() {
        let texts = [
            serve_status_text(ServeStatus::NoSets),
            serve_status_text(ServeStatus::BrakeRefused),
            serve_status_text(ServeStatus::Serving(2)),
        ];
        for t in &texts {
            assert!(!t.is_empty(), "every status has a reading");
            assert!(
                !t.contains("folders.share_serve_status"),
                "a raw i18n key reached the surface: {t}"
            );
        }
        assert!(
            texts[2].contains('2'),
            "the serving reading names how many sets are routed: {}",
            texts[2]
        );
        let unique: std::collections::BTreeSet<&String> = texts.iter().collect();
        assert_eq!(unique.len(), texts.len(), "three states, three readings");
    }

    /// The four transfer states resolve distinctly, and the refusal names its
    /// binding tier as a WORD — the `resolve_nested` arm. A plain `resolve`
    /// here would paint `features.tier_admin` at the user, which is exactly
    /// the mutation this asserts against.
    #[test]
    fn transfer_state_readings_resolve_and_name_the_tier() {
        let mut outcome = SetPullOutcome {
            admitted: false,
            ..SetPullOutcome::default()
        };
        let pending = transfer_state_text(&outcome);
        outcome.admitted = true;
        let quiet = transfer_state_text(&outcome);
        outcome.rows_accepted = 3;
        let pulling = transfer_state_text(&outcome);
        outcome.refusal = Some(fauna_core::feature_gate::FeatureVerdict::Deny {
            tier: fauna_core::feature_gate::RuleTier::Admin,
        });
        let limited = transfer_state_text(&outcome);

        let states: std::collections::BTreeSet<&String> =
            [&pending, &quiet, &pulling, &limited].into_iter().collect();
        assert_eq!(states.len(), 4, "four states, four readings");
        assert!(
            !limited.contains("features.tier_"),
            "the tier must render as a word, not its key: {limited}"
        );
        let tier_word = crate::i18n::strings::lookup("features.tier_admin").unwrap_or_default();
        assert!(
            limited.contains(tier_word),
            "the refusal names the binding tier: {limited}"
        );
    }

    /// The row readings carry the set's name and the peer's SHORT id (never
    /// the full actor hex), and the progress reading carries both counters.
    #[test]
    fn transfer_row_readings_carry_the_set_the_peer_and_the_counters() {
        let outcome = SetPullOutcome {
            folder: "photos".to_string(),
            peer_hex: "0123456789abcdef0123456789abcdef".to_string(),
            materialized: 4,
            rows_accepted: 7,
            ..SetPullOutcome::default()
        };
        let name = transfer_name_text(&outcome);
        assert!(name.contains("photos"), "the row names its set: {name}");
        assert!(
            !name.contains("0123456789abcdef0123456789abcdef"),
            "the row shows the peer's short id, never the full hex: {name}"
        );
        let progress = transfer_progress_text(&outcome);
        assert!(
            progress.contains('4') && progress.contains('7'),
            "the progress reading carries both counters: {progress}"
        );
    }
}
