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
//! The post-auth half is the SPA's warning sink for this account: a run that
//! ends with custody unconfirmed for an admin of the bound nest calls it with
//! `"mismatch"` or `"failed"`, and the SPA paints its existing custody
//! warning (`launch.recovery_custody_mismatch` / `…_failed`) on the shell's
//! message banner. The seed never leaves Rust: the leg fetches it over
//! `fauna.admin.deployment_seed.get` and merges it through the plane door.

use std::cell::RefCell;

use fauna_account_plane::account_driver::AccountStoreHandle;
use fauna_client_config::{
    DeploymentSeedCustody, SupersessionMarks, run_deployment_seed_custody_leg,
};
use fauna_core::identity::ActorId;
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
/// console ring), plus the warning key a run owes the admin, if any.
pub(crate) struct LegRun {
    pub(crate) token: &'static str,
    pub(crate) warning: Option<&'static str>,
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

/// Hand a warning key to the SPA's sink. A throwing sink is logged, never
/// propagated — the leg's outcome is already settled.
pub(crate) fn emit_warning(sink: &js_sys::Function, warning: &str) {
    if let Err(e) = sink.call1(
        &wasm_bindgen::JsValue::NULL,
        &wasm_bindgen::JsValue::from_str(warning),
    ) {
        tracing::warn!("[wasm/seed-custody] the warning sink threw: {e:?}");
    }
}

/// One run of the custody leg over `client` (authenticated as the account)
/// and `handle` (that account's store). The bound id is the one this
/// connection is bound to (`crate::rpc::bound_nest_id` — the origin's pin or
/// a possession proof, never the box's own `nest.info` claim).
pub(crate) async fn run_leg(client: &InnerClient, handle: &AccountStoreHandle) -> LegRun {
    let bound: ActorId = match crate::rpc::bound_nest_id(client).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("[wasm/seed-custody] bound nest id unresolved: {e:?}");
            return LegRun {
                token: "bound_unresolved",
                warning: Some("failed"),
            };
        }
    };
    let report = run_deployment_seed_custody_leg(client, bound, handle).await;
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
    let warning = match custody {
        DeploymentSeedCustody::RefusedMismatch => Some("mismatch"),
        other if other.unconfirmed() => Some("failed"),
        _ => None,
    };
    LegRun { token, warning }
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
        if let Some(warning) = run.warning {
            emit_warning(&sink, warning);
        }
    });
}
