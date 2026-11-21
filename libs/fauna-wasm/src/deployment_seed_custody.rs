//! The deployment-seed **custody leg** on web — this tab's two edges of the
//! one shared reconcile (`nest/box-recovery.md` § The plane-era recovery
//! floor, *(c) The writes*; the leg itself is
//! `fauna_client_config::run_deployment_seed_custody_leg`).
//!
//! The leg runs whenever the session has both an authenticated connection and
//! this account's store handle: at whichever of the post-auth edge
//! (`WsRpcClient::selfHealDeploymentSeedCustody`, fired by the SPA's
//! universal post-auth hook) and the store-ready edge
//! (`crate::account_runtime::start`) lands second, and at every later
//! post-auth edge. The two land in either order, so each edge leaves its half
//! here and runs the leg only when the other half is already present — the
//! same either-order shape as the succession ledger's post-store-ready pass
//! (`crate::succession::spawn_ledger_pass`).
//!
//! The post-auth half is the SPA's warning sink for this account, handed
//! every run's `fauna_client_config::CustodyLegSignal` ([`emit_signal`]): a
//! run that ends with custody unconfirmed for an admin of the bound nest calls
//! it with the shared `LocalizedText` (`fauna_client_config::custody_leg_warning`),
//! which the SPA renders through its one resolver onto the shell's message
//! banner; a run that leaves nothing owed calls it with `null` — the cleared
//! mark — and the custody warnings it may take down, so a warning an earlier
//! run painted is withdrawn, and only that warning
//! (`fauna_client_config::is_custody_leg_warning`). The choice between the
//! warnings is never the SPA's. The seed never leaves Rust: the leg fetches it over
//! `fauna.admin.deployment_seed.get` and merges it through the plane door.

use std::cell::RefCell;

use fauna_account_plane::account_driver::AccountStoreHandle;
use fauna_client_config::{
    CUSTODY_MISMATCH_KEY, CUSTODY_UNCONFIRMED_KEY, CustodyLegSignal, DeploymentSeedCustody,
    SupersessionMarks, custody_leg_unattempted_warning,
};
use fauna_client_pair::deployment_seed::run_custody_leg_bound;
use fauna_core::localized::LocalizedText;
use fauna_rpc_wasm::WsRpcClient as InnerClient;

/// The post-auth half: the account the SPA's post-auth hook fired for, and
/// its warning sink.
struct PostAuth {
    /// Lowercase-hex actor id the hook fired for — the store-ready edge runs
    /// the leg only for the same account, so an account switch never lands
    /// one account's warning off another's connection.
    actor_id_hex: String,
    on_warning: js_sys::Function,
}

thread_local! {
    static POST_AUTH: RefCell<Option<PostAuth>> = const { RefCell::new(None) };
}

/// The leg's outcome as the token the post-auth promise resolves to (for the
/// console ring), plus the signal the run owes the SPA's sink.
pub(crate) struct LegRun {
    pub(crate) token: &'static str,
    pub(crate) signal: CustodyLegSignal,
}

/// Record the post-auth half for `actor_id_hex`, replacing any earlier one.
pub(crate) fn note_post_auth(actor_id_hex: String, on_warning: js_sys::Function) {
    POST_AUTH.with(|held| {
        *held.borrow_mut() = Some(PostAuth {
            actor_id_hex,
            on_warning,
        })
    });
}

/// The warning sink the post-auth hook left for `actor_id_hex`, if it fired
/// for that account.
fn warning_sink_for(actor_id_hex: &str) -> Option<js_sys::Function> {
    POST_AUTH.with(|held| {
        held.borrow()
            .as_ref()
            .filter(|p| p.actor_id_hex.eq_ignore_ascii_case(actor_id_hex))
            .map(|p| p.on_warning.clone())
    })
}

/// The custody warnings a cleared mark may take down — every text
/// [`fauna_client_config::is_custody_leg_warning`] counts as one — handed to
/// the sink with the mark, so the SPA compares the banner against its own
/// rendering of them and never spells the keys itself.
fn custody_warnings() -> Vec<LocalizedText> {
    [CUSTODY_MISMATCH_KEY, CUSTODY_UNCONFIRMED_KEY]
        .into_iter()
        .map(LocalizedText::key)
        .collect()
}

/// Hand a run's signal to the SPA's sink: a warning as `sink(warning)`, a JS
/// `LocalizedText`; the cleared mark as `sink(null, custodyWarnings)`. A
/// throwing (or unencodable) sink is logged, never propagated — the leg's
/// outcome is already settled.
pub(crate) fn emit_signal(sink: &js_sys::Function, signal: &CustodyLegSignal) {
    let encoded = match signal {
        CustodyLegSignal::Warning(warning) => {
            crate::rpc::to_js(warning).map(|w| (w, wasm_bindgen::JsValue::UNDEFINED))
        }
        CustodyLegSignal::Cleared => crate::rpc::to_js(&custody_warnings())
            .map(|warnings| (wasm_bindgen::JsValue::NULL, warnings)),
    };
    let (first, second) = match encoded {
        Ok(args) => args,
        Err(e) => {
            tracing::warn!("[wasm/seed-custody] the signal did not encode: {e:?}");
            return;
        }
    };
    if let Err(e) = sink.call2(&wasm_bindgen::JsValue::NULL, &first, &second) {
        tracing::warn!("[wasm/seed-custody] the warning sink threw: {e:?}");
    }
}

/// One run of the custody leg over `client` (authenticated as the account)
/// and `handle` (that account's store), through the shared
/// `fauna_client_pair::deployment_seed::run_custody_leg_bound`: the bound id
/// is the one this connection is bound to (the origin's pin or a possession
/// proof, never the box's own `nest.info` claim).
pub(crate) async fn run_leg(client: &InnerClient, handle: &AccountStoreHandle) -> LegRun {
    let identity = fauna_client_pair::build_linked_nests_machine(client.clone());
    let report = match run_custody_leg_bound(client, &identity, handle).await {
        Ok(report) => report,
        Err(e) => {
            tracing::warn!("[wasm/seed-custody] bound nest id unresolved: {e}");
            return LegRun {
                token: "bound_unresolved",
                signal: CustodyLegSignal::Warning(custody_leg_unattempted_warning()),
            };
        }
    };
    match &report.marks {
        SupersessionMarks::Checked(marked) if !marked.is_empty() => tracing::info!(
            marked = marked.len(),
            "[wasm/seed-custody] marked rotated-away boxes superseded"
        ),
        SupersessionMarks::ChainUnavailable | SupersessionMarks::StoreRefused(_) => {
            tracing::debug!(marks = ?report.marks, "[wasm/seed-custody] mark reconcile deferred")
        }
        _ => {}
    }
    let custody = &report.custody;
    let token = match custody {
        DeploymentSeedCustody::AlreadyCustodied => "already_custodied",
        DeploymentSeedCustody::NotAdmin => "not_admin",
        DeploymentSeedCustody::HandoffUnavailable => "handoff_unavailable",
        DeploymentSeedCustody::NestHoldsNoSeed => "nest_holds_no_seed",
        DeploymentSeedCustody::RefusedMismatch => "refused_mismatch",
        DeploymentSeedCustody::Captured => "captured",
        DeploymentSeedCustody::StoreRefused(_) => "store_refused",
    };
    if custody.unconfirmed() {
        tracing::warn!(
            outcome = ?custody,
            retryable = custody.retryable(),
            "[wasm/seed-custody] custody unconfirmed"
        );
    } else {
        tracing::debug!(outcome = ?custody, "[wasm/seed-custody] custody leg settled");
    }
    LegRun {
        token,
        signal: CustodyLegSignal::of(custody),
    }
}

/// The store-ready edge: run the leg for `actor_id_hex` over `client` once the
/// post-auth hook has fired for the same account; a no-op otherwise (the
/// post-auth edge then runs it, landing second).
pub(crate) fn spawn_at_store_ready(
    client: InnerClient,
    handle: AccountStoreHandle,
    actor_id_hex: &str,
) {
    let Some(sink) = warning_sink_for(actor_id_hex) else {
        return;
    };
    wasm_bindgen_futures::spawn_local(async move {
        let run = run_leg(&client, &handle).await;
        emit_signal(&sink, &run.signal);
    });
}
