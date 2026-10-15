//! UniFFI face of the **nest_retire page's held-DNS read**
//! (`docs/goal/behavior/nest-retirement.md` § Credential stance (b),
//! § DNS cleanup → *Several domains* (1)) — the native apps' twin of what tui
//! does with `account_store.dns()` → `HeldDnsCredential::all_from`.
//!
//! One call reads the account's `fauna.state.dns` record through this
//! process's account-store runtime ([`crate::account_runtime::handle_source`],
//! which waits for the assembly), bounded in time, and hands back exactly what
//! `NestRetireMachine::set_held_dns_json` / `set_remembered_slots_json` take —
//! so no app projects the credential bag or the remembered-name maps itself.
//! A read the first-listing gate refuses (`refused_as_not_ready`) comes back as
//! [`FfiRetireHeldInputs::not_ready_reason`] for the app to hand to
//! `NestRetireMachine::set_input_error`: not "the account holds none".

use std::collections::BTreeMap;
use std::time::Duration;

use fauna_onboarding_machine::retire::HeldDnsCredential;
use fauna_sync_engine::account_runtime::AccountStoreAccess;
use fauna_sync_engine::preference_surfaces::{plane_failure, refused_as_not_ready};

/// How long the read may hold the page's verify before it goes on with what it
/// opened with (the tui's bound).
const READ_BOUND: Duration = Duration::from_secs(15);

/// What the nest_retire page takes from the account's `fauna.state.dns`.
///
/// Every field is already the shape the machine setter takes; an all-empty
/// record with no `not_ready_reason` means the read failed or timed out
/// (best-effort — keep what the page opened with) or the account holds none.
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct FfiRetireHeldInputs {
    /// Index-paired with [`Self::held_dns_creds_json`] — the arguments of
    /// `set_held_dns_json`.
    pub held_dns_provider_ids: Vec<String>,
    /// Each held credential's field bag as a JSON object of plain strings.
    pub held_dns_creds_json: Vec<String>,
    /// `{"<domain>": ["<fqdn>", …]}` — the arguments of
    /// `set_remembered_slots_json`.
    pub remembered_dkim_json: String,
    pub remembered_atproto_json: String,
    pub remembered_fauna_self_json: String,
    /// Why the held credentials could not be read, when the first-listing gate
    /// refused: the reason `set_input_error` shows. `None` on success and on a
    /// failure the page keeps quiet about.
    pub not_ready_reason: Option<String>,
}

/// Read the held DNS credentials and remembered Fauna slot names for the
/// retire page. Never fails: the page's inputs are best-effort, and a refusal
/// travels in [`FfiRetireHeldInputs::not_ready_reason`].
///
/// `wait_for_runtime`: the admin entry runs over a live session whose runtime
/// may still be assembling, so it waits for it (bounded); the launch entry runs
/// before any session, where no runtime normally exists, so it reads only a
/// runtime that is already up and otherwise answers empty at once.
#[fauna_uniffi_async::export]
pub async fn retire_held_inputs(wait_for_runtime: bool) -> FfiRetireHeldInputs {
    let read = async {
        let store = if wait_for_runtime {
            crate::account_runtime::handle_source()
                .account_store("nest_retire held DNS read")
                .await
        } else {
            match crate::account_runtime::handle() {
                Some(store) => Ok(store),
                None => return Ok(None),
            }
        }?;
        store.dns().await.map(Some)
    };
    match tokio::time::timeout(READ_BOUND, read).await {
        Ok(Ok(None)) => FfiRetireHeldInputs::default(),
        Ok(Ok(Some(dns))) => {
            let held = HeldDnsCredential::all_from(&dns);
            FfiRetireHeldInputs {
                held_dns_provider_ids: held.iter().map(|c| c.provider_id.clone()).collect(),
                held_dns_creds_json: held
                    .iter()
                    .map(|c| {
                        let plain: BTreeMap<&str, &str> = c
                            .fields
                            .iter()
                            .map(|(k, v)| (k.as_str(), v.as_str()))
                            .collect();
                        serde_json::to_string(&plain).unwrap_or_default()
                    })
                    .collect(),
                remembered_dkim_json: serde_json::to_string(&dns.dkim_published_names)
                    .unwrap_or_default(),
                remembered_atproto_json: serde_json::to_string(&dns.atproto_published_names)
                    .unwrap_or_default(),
                remembered_fauna_self_json: serde_json::to_string(&dns.fauna_self_published_names)
                    .unwrap_or_default(),
                not_ready_reason: None,
            }
        }
        Ok(Err(e)) => {
            let reason = plane_failure(e);
            FfiRetireHeldInputs {
                not_ready_reason: refused_as_not_ready(&reason).then_some(reason),
                ..FfiRetireHeldInputs::default()
            }
        }
        Err(_) => FfiRetireHeldInputs::default(),
    }
}
