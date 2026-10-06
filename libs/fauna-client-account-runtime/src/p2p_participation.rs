//! The devices page's peer-participation door over a seat's account runtime
//! (`fauna_devices_machine::P2pParticipation`; `docs/goal/behavior/p2p.md`
//! § Per-device participation) — the one shared impl every native app wires
//! beside its fleet-removal door, so no app re-derives what the toggle on this
//! device's own row does.
//!
//! Three reads/writes, all against the runtime handle: the device-local row
//! ([`AccountStoreHandle::p2p_participation`]), the switch
//! ([`AccountStoreHandle::set_p2p_participation`]) and the enrolled roster row
//! ([`AccountStoreHandle::enrolled_device_row`], the same read
//! `device-this-mark-badge` prefers). The switch also makes the same-account
//! listener react NOW, wherever the engine lives: a `reconcile_now` runs the
//! pass whose ensure step reads the row (`peer_leg::ensure_bound`) when this
//! process holds the engine, and when another process holds it — the
//! co-located sync agent on a desktop — the door asks that holder for the same
//! pass through the seat's [`EngineHolderNudge`]. The share seat in this
//! process drops at once either way (the driver's participation watch).

use std::sync::{Arc, Mutex, OnceLock, Weak};

use fauna_devices_machine::P2pParticipation;
use fauna_sync_engine::account_runtime::AccountStoreHandle;

/// What an absent runtime answers.
const RUNTIME_ABSENT: &str = "the account runtime is not running";

/// The other process that may hold this account store's engine role — on a
/// desktop, the co-located sync agent — asked to run one full pass now.
///
/// Best-effort by contract: the row the pass reads already rests in the
/// store, so a nudge that never arrives costs only the holder's backstop
/// latency, never correctness; an implementation swallows its own failure.
#[async_trait::async_trait]
pub trait EngineHolderNudge: Send + Sync {
    async fn nudge_engine_holder(&self);
}

/// The desktop arm: the app's own control channel to its co-located agent
/// (`RequestMethod::ReconcileAccountRuntime`, a plain request — never the
/// ensuring one: the agent's backstop re-drives what a missed nudge left).
#[async_trait::async_trait]
impl<R, B> EngineHolderNudge for fauna_client_sync::agent::SyncAgentProvisioner<R, B>
where
    R: fauna_protocol::RpcRequester + Clone + Send + Sync + 'static,
    R::Error: std::fmt::Display,
    B: fauna_client_sync::agent::ProvisioningBearerSource + 'static,
{
    async fn nudge_engine_holder(&self) {
        if let Err(e) = self.reconcile_account_runtime().await {
            tracing::debug!(error = %e, "participation: agent pass nudge not delivered");
        }
    }
}

/// Where a seat publishes the [`EngineHolderNudge`] it drives and its door
/// reads it — a weak, cloneable slot, so the door never keeps a provisioner
/// alive past its session and a torn-down one reads absent with no clear
/// step. [`Self::seat`] is the process's one slot, the one every host
/// publishes into and wires its door with: a process drives at most one agent
/// control channel at a time.
#[derive(Clone, Default)]
pub struct EngineHolderNudgeSlot(Arc<Mutex<Option<Weak<dyn EngineHolderNudge>>>>);

impl EngineHolderNudgeSlot {
    /// This process's slot.
    pub fn seat() -> Self {
        static SEAT: OnceLock<EngineHolderNudgeSlot> = OnceLock::new();
        SEAT.get_or_init(Self::default).clone()
    }

    /// Publish `nudge` as the live one; it reads absent again once the
    /// caller's last strong reference drops.
    pub fn publish(&self, nudge: &Arc<dyn EngineHolderNudge>) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(Arc::downgrade(nudge));
        }
    }

    fn current(&self) -> Option<Arc<dyn EngineHolderNudge>> {
        self.0.lock().ok()?.as_ref()?.upgrade()
    }
}

/// [`P2pParticipation`] over a seat's account runtime. `source` is the seat's
/// own fresh read of its live handle — `AccountRuntimeHost::handle` on linux
/// and the `fauna-ffi` seat, the `App`-owned slot on tui — exactly as
/// [`crate::fleet_removal::RuntimeFleetRemoval`] takes it.
pub struct RuntimeP2pParticipation<S> {
    source: S,
    holder_nudge: EngineHolderNudgeSlot,
}

impl<S> RuntimeP2pParticipation<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    /// The door with no other process to nudge. Every host adds
    /// [`Self::with_holder_nudge`].
    pub fn new(source: S) -> Self {
        Self {
            source,
            holder_nudge: EngineHolderNudgeSlot::default(),
        }
    }

    /// Nudge the engine holder published in `slot` whenever this process does
    /// not hold the engine itself ([`EngineHolderNudgeSlot::seat`] in every
    /// host).
    pub fn with_holder_nudge(mut self, slot: EngineHolderNudgeSlot) -> Self {
        self.holder_nudge = slot;
        self
    }

    fn handle(&self) -> Result<AccountStoreHandle, String> {
        (self.source)().ok_or_else(|| RUNTIME_ABSENT.to_string())
    }
}

#[async_trait::async_trait]
impl<S> P2pParticipation for RuntimeP2pParticipation<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    async fn local(&self) -> Result<bool, String> {
        self.handle()?
            .p2p_participation()
            .await
            .map(|row| row.effective())
            .map_err(|e| e.to_string())
    }

    async fn set_local(&self, on: bool) -> Result<(), String> {
        let handle = self.handle()?;
        handle
            .set_p2p_participation(on)
            .await
            .map_err(|e| e.to_string())?;
        // The same-account listener lives in the engine holder's pump. Run
        // that pass now — here, or in the holder's process — so rule 5's
        // "off ⇒ no socket" holds within the gesture rather than at the next
        // backstop. The row rested above, so the pass's own errors are its
        // report's and a lost nudge is the backstop's, never this gesture's.
        if handle.is_engine_holder() {
            let _ = handle.reconcile_now().await;
        } else if let Some(nudge) = self.holder_nudge.current() {
            nudge.nudge_engine_holder().await;
        }
        Ok(())
    }

    async fn own_row(&self) -> Result<Option<String>, String> {
        self.handle()?
            .enrolled_device_row()
            .await
            .map_err(|e| e.to_string())
    }
}
