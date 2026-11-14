//! The e2e harness's **app-less provisioning** export.
//!
//! A box a live `[nest]` witness needs is built by the same shared funnel the
//! apps' onboarding wizard runs: the [`OnboardingMachine`], driven through its
//! shared bridge dispatcher (`call_machine_method_async` — the very table every
//! app's `call_machine_method` delegates to), then
//! [`OnboardingMachine::run_provisioning`] to completion — the future each
//! app's Start button spawns, whose last step claims the box as the seeded
//! identity. So a test that only needs a provisioned, claimed box to observe
//! from outside launches no app driver (`feature-catalog.md` § The two
//! surfaces), and the box it gets is the box an app would have built.

use std::sync::{Arc, Mutex};

use fauna_launch_machine::{AwaitingDnsRecord, PendingProvisionStore};
use fauna_onboarding_machine::{OnboardingMachine, OnboardingObserver};
use fauna_provisioning::progress::{OverallStatus, ProvisioningSnapshot};

use super::{Outcome, encode_err, err, run, runtime};
use crate::FfiError;
use crate::cabi::{FfiBuffer, slice_to_vec};

/// The harness renders nothing, so it has nothing to re-render.
struct Unobserved;

impl OnboardingObserver for Unobserved {
    fn on_changed(&self) {}
}

/// The pending-provision slot an app keeps in its account registry, kept in
/// memory for the call's length: the machine reads the write back before it
/// builds a box (`docs/goal/behavior/onboarding.md` § 6 *The pending-provision
/// slot*), and a retry resumes the box the first run built instead of minting
/// over it.
#[derive(Default)]
struct HeldSlot(Mutex<Option<AwaitingDnsRecord>>);

impl PendingProvisionStore for HeldSlot {
    fn save_awaiting_dns(
        &self,
        _secret_hex: String,
        record: AwaitingDnsRecord,
    ) -> Option<AwaitingDnsRecord> {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        *slot = Some(record);
        slot.clone()
    }

    fn clear_awaiting_dns(&self, _secret_hex: String) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// What [`fauna_harness_provision`] is asked: the bridge calls that set the
/// wizard up, then how a failed run may be retried.
type ProvisionRequest = (Vec<(String, String)>, Option<String>, u32);

/// Apply `calls` through the shared dispatcher, run provisioning to its end,
/// and re-run it — the page's Retry — while it fails with exactly
/// `retry_error`, at most `max_retries` times. Returns the final snapshot.
async fn provision(
    calls: Vec<(String, String)>,
    retry_error: Option<String>,
    max_retries: u32,
) -> Result<ProvisioningSnapshot, FfiError> {
    let machine = OnboardingMachine::new_with_persistence(
        Arc::new(Unobserved),
        Arc::new(HeldSlot::default()),
    );
    for (name, json_arg) in calls {
        if matches!(name.as_str(), "start_provisioning" | "retry_provisioning") {
            return Err(err(format!(
                "`{name}` is this export's own act — name only the calls that set the wizard up"
            )));
        }
        let _ = Arc::clone(&machine)
            .call_machine_method_async(name, json_arg)
            .await;
    }
    let mut retries = 0;
    loop {
        Arc::clone(&machine).run_provisioning().await;
        let snap = machine.provisioning_snapshot();
        let retryable = snap.overall == OverallStatus::Failed
            && retry_error.is_some()
            && snap.final_error == retry_error;
        if !retryable || retries >= max_retries {
            return Ok(snap);
        }
        retries += 1;
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// Provision a box app-less through the onboarding machine every app runs.
///
/// `request` is the dag-cbor array `[calls, retry_error, max_retries]`:
/// `calls` a list of `[method, json_arg]` pairs applied in order through the
/// machine's shared bridge dispatcher (the same names and arguments an app
/// driver's `call_machine_method` sends — `seed_identity`, `set_current_handle`,
/// `verify_dns`, `set_vps_state_for_test`, …; `start_provisioning` and
/// `retry_provisioning` are refused, the export runs provisioning itself);
/// `retry_error` a `final_error` on which a failed run is retried (or null) and
/// `max_retries` how often. The call blocks until the run ends — minutes, on a
/// real cloud. `out` receives `{"ok": "<snapshot JSON>"}`, the machine's
/// `provisioning_snapshot` reader's own answer, whatever the run's outcome; a
/// request that does not decode is a local failure.
///
/// # Safety
///
/// `request` must point to `request_len` readable bytes and `out` to a
/// writable [`FfiBuffer`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_provision(
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let req = unsafe { slice_to_vec(request, request_len) };
        let (calls, retry_error, max_retries): ProvisionRequest = decode_request!(req)?;
        let snap = runtime()?.block_on(provision(calls, retry_error, max_retries))?;
        let json = serde_json::to_string(&snap).map_err(encode_err)?;
        fauna_core::encoding::canonical_encode(&json)
            .map(Outcome::Ok)
            .map_err(encode_err)
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The export drives the shared machine end to end: the calls reach the
    /// dispatcher (the handle names the box's domain) and the run is the real
    /// one — refusing at its first check with no server type chosen, before
    /// anything is bought.
    #[test]
    fn provision_runs_the_shared_machine_and_reports_its_snapshot() {
        let calls = vec![
            (
                "seed_identity".to_string(),
                format!("\"{}\"", "11".repeat(32)),
            ),
            (
                "set_current_handle".to_string(),
                "\"admin@provision.example\"".to_string(),
            ),
        ];
        let snap = runtime()
            .unwrap()
            .block_on(provision(calls, None, 0))
            .expect("the request is well-formed");
        assert_eq!(snap.overall, OverallStatus::Failed, "{snap:?}");
        let error = snap.final_error.unwrap_or_default();
        assert!(error.contains("no server type selected"), "{error}");
    }

    #[test]
    fn the_start_call_is_the_exports_own() {
        let calls = vec![("start_provisioning".to_string(), String::new())];
        let e = runtime()
            .unwrap()
            .block_on(provision(calls, None, 0))
            .expect_err("refused");
        assert!(e.to_string().contains("this export's own act"), "{e}");
    }
}
