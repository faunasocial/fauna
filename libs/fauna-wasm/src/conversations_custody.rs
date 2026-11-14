//! The custody mint on web — the OWNER's side of the custody ceremony
//! (`docs/goal/ui/devices.md` § Custody facet and § Where logic lives): the
//! account-custody ceremony sink registered on this rail's `FaunaMlsBackend`,
//! the drive pass, and the two exports the Devices page's `custody-mint-*`
//! family calls (`custodyMintCandidatesJson`, `custodyMint`).
//!
//! Nothing here decides anything: the candidate fold
//! (`fauna_client_conversations::custody_mint_candidates_over`), the mint's two
//! refusals and the offer's shape (`fauna_client_capabilities::custody_acts::
//! mint_custody`), the sink and the drive pass
//! (`fauna_client_conversations::{StoreCustodyCeremonySink,
//! drive_ceremony_pass}`) are the same functions the six native apps run. What
//! is web's own is only the executor (`spawn_local`) and that the store is this
//! tab's account runtime, read per call (it lands after the rail is built).
//!
//! The friend's side — accept, set-budget, stop — stays native: accept binds
//! the T10 writer key and the budget writes the custody registry row, neither of
//! which a browser holds (`devices.md` § Where logic lives).

use std::sync::{Arc, Weak};

use fauna_conversations::backends::fauna_mls::FaunaMlsBackend;
use fauna_core::identity::{ActorId, ActorKeypair};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use super::WasmConversationsManager;

/// The ceremony-moved edge's web arm: a received offer, accept, deliver or
/// receipt that moved a ceremony schedules one drive pass, so the owner's
/// witness delivery and the receipt's registry row follow the friend's accept
/// without the page having to be open. Holds the backend `Weak` — the backend
/// owns the sink that owns this observer.
struct DriveOnCeremonyMove {
    nest: fauna_rpc_wasm::WsRpcClient,
    owner: [u8; 32],
    backend: Weak<FaunaMlsBackend>,
}

impl fauna_client_conversations::CustodyCeremonyObserver for DriveOnCeremonyMove {
    fn ceremony_moved(&self, _grant_id: &[u8]) {
        // A dropped backend means the rail is being torn down (sign-out) —
        // the next rail's drive re-reads durable state.
        let Some(backend) = self.backend.upgrade() else {
            return;
        };
        spawn_drive(
            self.nest.clone(),
            ActorKeypair::from_secret(self.owner),
            backend,
        );
    }
}

/// Register the account-custody ceremony sink on this rail's backend — the
/// receive half: each received offer / accept / deliver / A7 receipt is
/// verified and captured into `fauna.state.custody-ceremony` through this tab's
/// account runtime (the runtime serving `owner`'s account, resolved per payload;
/// none yet → the payload stays in channel history and the per-launch re-walk
/// heals). The web twin of `fauna_client_custody::register_ceremony_sink`.
pub(super) fn register_ceremony_sink(
    backend: &Arc<FaunaMlsBackend>,
    nest: fauna_rpc_wasm::WsRpcClient,
    owner: ActorKeypair,
) {
    let actor_hex = owner.actor_id_hex();
    let own_actor = owner.actor_id();
    let observer = DriveOnCeremonyMove {
        nest,
        owner: *owner.secret_bytes(),
        backend: Arc::downgrade(backend),
    };
    backend.set_custody_ceremony_sink(Arc::new(
        fauna_client_conversations::StoreCustodyCeremonySink::new(
            fauna_client_config::ResolvingLedgerStore::new(move || {
                crate::account_runtime::handle_for(&actor_hex)
            }),
            own_actor,
        )
        .with_observer(Arc::new(observer)),
    ));
}

/// One drive pass on the tab's executor, over the runtime serving `owner`'s
/// account — none running is a quiet no-op (the next edge re-drives).
fn spawn_drive(
    nest: fauna_rpc_wasm::WsRpcClient,
    owner: ActorKeypair,
    backend: Arc<FaunaMlsBackend>,
) {
    wasm_bindgen_futures::spawn_local(drive(nest, owner, backend));
}

async fn drive(
    nest: fauna_rpc_wasm::WsRpcClient,
    owner: ActorKeypair,
    backend: Arc<FaunaMlsBackend>,
) {
    let Some(store) = crate::account_runtime::handle_for(&owner.actor_id_hex()) else {
        return;
    };
    fauna_client_conversations::drive_ceremony_pass(nest, &store, &owner, backend).await;
}

impl WasmConversationsManager {
    /// The rail's backend and nest client, or the refusal a custody export
    /// answers when this manager was built without the MLS rail.
    fn custody_parts(&self) -> Option<(Arc<FaunaMlsBackend>, fauna_rpc_wasm::WsRpcClient)> {
        let backend = self.fauna_mls.borrow().clone()?;
        let client = self.nest_client.borrow().clone()?;
        Some((backend, client))
    }

    /// The candidate list, as the shared fold derives it from this rail.
    fn custody_mint_candidate_views(
        &self,
        backend: &FaunaMlsBackend,
        own: &ActorId,
    ) -> Vec<fauna_client_capabilities::custody_view::CustodyMintCandidateView> {
        fauna_client_conversations::custody_mint_candidates_over(
            backend,
            &self.manager.snapshot().threads,
            own,
        )
        .iter()
        .map(fauna_client_conversations::CustodyMintCandidate::view)
        .collect()
    }
}

#[wasm_bindgen]
impl WasmConversationsManager {
    /// `custody-mint-host-select`'s options — who the owner could ask: one
    /// per existing 1:1 conversation, as the shared fold derives it
    /// (`custody_mint_candidates_over`: the two-member filter, the
    /// self-exclusion, one per host, the label fallback, the sort). A JSON
    /// array of `{ host: number[32], channel_hex, label }` — the
    /// `CustodyMintCandidateView` boundary row the native apps get over
    /// UniFFI. Empty = no one to ask yet: the page refuses to open the flow and
    /// says so (`devices.custody_mint_no_contacts`), never an empty picker.
    #[wasm_bindgen(js_name = custodyMintCandidatesJson)]
    pub fn custody_mint_candidates_json(&self, secret_hex: String) -> Result<JsValue, JsValue> {
        let own = fauna_rpc_wasm::keypair_from_secret_hex(&secret_hex)?.actor_id();
        let views = match self.custody_parts() {
            Some((backend, _)) => self.custody_mint_candidate_views(&backend, &own),
            None => Vec::new(),
        };
        crate::rpc::to_js(&views)
    }

    /// `custody-mint-confirm-button` — ask the friend behind `host` (32 bytes,
    /// a candidate row's `host`) to hold encrypted copies, over that row's
    /// `channelHex`. Records the offer (Account scope, the default window)
    /// through the shared `custody_acts::mint_custody` — which refuses a pair
    /// the candidate list never offered, and a manager with no MLS rail — then
    /// runs one drive pass, which posts it. Resolves to the error string for
    /// the page's `error-message`, or `null` once the offer is recorded and
    /// the post attempted — never a silent drop (e2e convention 11). Re-run
    /// `custodyFacetLoad` afterwards: the pending row joins the holder group.
    ///
    /// The offer's device seed is empty: a browser holds no T10 writer key,
    /// and the seed only narrows discovery — the owner's nest URL is the
    /// always-on anchor (`OfferParams::owner_nest_url`).
    #[wasm_bindgen(js_name = custodyMint, unchecked_return_type = "Promise<string | null>")]
    pub fn custody_mint(
        &self,
        secret_hex: String,
        host: Vec<u8>,
        channel_hex: String,
    ) -> Result<js_sys::Promise, JsValue> {
        let owner = fauna_rpc_wasm::keypair_from_secret_hex(&secret_hex)?;
        let host = ActorId(
            <[u8; 32]>::try_from(host.as_slice())
                .map_err(|_| JsValue::from_str("a custodian host must be exactly 32 bytes"))?,
        );
        let parts = self.custody_parts();
        let candidates = parts
            .as_ref()
            .map(|(backend, _)| self.custody_mint_candidate_views(backend, &owner.actor_id()));
        Ok(future_to_promise(async move {
            let Some(store) = crate::account_runtime::handle_for(&owner.actor_id_hex()) else {
                return Ok(JsValue::from_str(fauna_client_config::LEDGER_NOT_READY));
            };
            let nest_url = parts
                .as_ref()
                .map(|(_, client)| client.nest_url())
                .unwrap_or_default();
            let error = fauna_client_capabilities::custody_acts::mint_custody(
                &store,
                &owner,
                candidates.as_deref(),
                host,
                channel_hex,
                Vec::new(),
                nest_url,
                fauna_core::data::Timestamp::now(),
            )
            .await;
            if let Some(error) = error {
                return Ok(JsValue::from_str(&error));
            }
            // Recorded (unposted) — this pass posts it. `parts` is `Some`
            // here: `mint_custody` refuses a `None` candidate list.
            if let Some((backend, client)) = parts {
                drive(client, owner, backend).await;
            }
            Ok(JsValue::NULL)
        }))
    }

    /// One custody-ceremony drive pass — the Devices page's load edge, so a
    /// friend's accept and the receipt converge while the owner looks
    /// (`devices.md` § Where logic lives: the drive fires on session start,
    /// store ready and every ceremony-moved edge). Resolves when the pass is
    /// done; no rail or no runtime is a quiet no-op.
    #[wasm_bindgen(js_name = custodyDrive, unchecked_return_type = "Promise<void>")]
    pub fn custody_drive(&self, secret_hex: String) -> Result<js_sys::Promise, JsValue> {
        let owner = fauna_rpc_wasm::keypair_from_secret_hex(&secret_hex)?;
        let parts = self.custody_parts();
        Ok(future_to_promise(async move {
            if let Some((backend, client)) = parts {
                drive(client, owner, backend).await;
            }
            Ok(JsValue::UNDEFINED)
        }))
    }
}
