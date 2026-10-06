//! The Nests page's blessing door over a seat's account runtime
//! (`fauna_client_pair::BlessedNestsStore`; `docs/goal/ui/nests.md` § Expiry /
//! renewal → *Duration and blessing*) — the one impl every runtime-hosting app
//! wires into its `LinkedNestsMachine`, web included, so no app re-derives
//! where the per-nest "keep this box's grants renewed" verdict lives.
//!
//! The verdicts are the account plane's `fauna.state.blessed-nests` kind, one
//! row per nest (`config-dissolution.md` — the kinds table's row;
//! `fauna_account_plane::blessed_nest_rows` owns the door). The kind was born
//! plane-only: there is no second rail behind this seam, so an absent
//! runtime is an error, never a fallback.
//!
//! **An absent runtime.** A read answers `Err` at once (the machine renders it
//! un-blessed and renews nothing). A write — the toggle, or the one-tap trust
//! at the end of onboarding, which can run before the seat's runtime has
//! assembled — waits for the handle for up to [`WRITE_WAIT_POLLS`] ×
//! [`WRITE_WAIT_POLL`], then refuses.

use std::time::Duration;

use fauna_account_plane::account_driver::AccountStoreHandle;
use fauna_client_pair::BlessedNestsStore;

/// What an absent runtime answers.
const RUNTIME_ABSENT: &str = "the account runtime is not running";

/// How often a write re-reads the seat's handle while the runtime assembles.
pub const WRITE_WAIT_POLL: Duration = Duration::from_millis(250);

/// How many polls a write waits for the runtime before it refuses (~30 s —
/// an assembly that has not settled by then has failed, and the page says so).
pub const WRITE_WAIT_POLLS: u32 = 120;

/// [`BlessedNestsStore`] over a seat's account runtime. `source` is the seat's
/// own fresh read of its live handle — `AccountRuntimeHost::handle` on linux,
/// the `fauna-ffi` seat and web, the `App`-owned slot on tui — exactly as
/// `fauna_client_account_runtime::p2p_participation` takes it.
pub struct PlaneBlessedNests<S> {
    source: S,
}

impl<S> PlaneBlessedNests<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    pub fn new(source: S) -> Self {
        Self { source }
    }

    /// The handle, waiting out an assembly still in flight.
    async fn handle_for_write(&self) -> Result<AccountStoreHandle, String> {
        wait_for_handle(&self.source)
            .await
            .ok_or_else(|| RUNTIME_ABSENT.to_string())
    }
}

/// A seat's live handle for a WRITE, waiting out an assembly still in flight —
/// polled every [`WRITE_WAIT_POLL`], for at most [`WRITE_WAIT_POLLS`] polls,
/// then `None`. Shared by every door over a seat's runtime whose write can
/// arrive before the runtime has assembled (this one's toggle and one-tap
/// trust; [`crate::period_keys`]' custody writes).
pub(crate) async fn wait_for_handle<S>(source: &S) -> Option<AccountStoreHandle>
where
    S: Fn() -> Option<AccountStoreHandle>,
{
    for _ in 0..WRITE_WAIT_POLLS {
        if let Some(handle) = source() {
            return Some(handle);
        }
        fauna_sleep::sleep(WRITE_WAIT_POLL).await;
    }
    source()
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<S> BlessedNestsStore for PlaneBlessedNests<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    async fn is_blessed(&self, nest_id: &[u8; 32]) -> Result<bool, String> {
        let handle = (self.source)().ok_or_else(|| RUNTIME_ABSENT.to_string())?;
        handle
            .nest_blessed(*nest_id)
            .await
            .map_err(|e| format!("{e:#}"))
    }

    async fn set_blessed(
        &self,
        nest_id: &[u8; 32],
        blessed: bool,
        now: u64,
    ) -> Result<bool, String> {
        self.handle_for_write()
            .await?
            .set_nest_blessed(*nest_id, blessed, now)
            .await
            .map_err(|e| format!("{e:#}"))
    }
}
