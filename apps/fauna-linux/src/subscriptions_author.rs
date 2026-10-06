//! Linux runtime wiring for the shared author-side subscription reconciliation.
//!
//! **Scheduling only.** What a tick *does*, how long to wait between ticks, and
//! the loop itself are shared — [`run_author_reconcile_loop`] in
//! `fauna-client-subscriptions` (priority #2; linux and tui both already run a
//! bare tokio runtime with no FFI/wasm boundary, so unlike the other five apps
//! they can share the loop body too — see that fn's doc comment). This file
//! owns just the genuinely platform-specific part: spawning the loop on the GTK
//! app's tokio runtime at login (the `AuthSuccess` handler in `app.rs`,
//! alongside `conversations::conv_backend::start_conversations_session` and
//! `conversations::drafts::start`) and tracking the login generation that stops
//! it once a newer login supersedes it.
//!
//! All mint / seal / rotation crypto likewise stays in shared Rust; this file is
//! transport glue only.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fauna_client::NestClient;
use fauna_client_subscriptions::orchestration::{SubscriptionsAuthor, run_author_reconcile_loop};
use fauna_core::identity::ActorKeypair;

/// Monotonic login generation. Each [`start`] bumps it and captures its own
/// value; the loop exits once a newer login supersedes it. Native apps build
/// one loop per app launch, but the linux e2e session-cached driver re-injects
/// the authenticated session repeatedly within one process (reset → re-auth per
/// test), so without this each test's loop would leak and keep polling the prior
/// (torn-down) nest — the same accumulation the conversations loop's `Weak`
/// liveness handle guards against.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Spawn the author-side reconcile loop for the just-authenticated actor.
/// Called from the `AuthSuccess` handler in `app.rs`, alongside
/// [`crate::conversations::conv_backend::start_conversations_session`] and
/// [`crate::conversations::drafts::start`]. `secret` is the 32-byte identity
/// seed (the owner's account-plane custody + the `ManageSubscribers`
/// self-delegation are derived from it, exactly as the Tiers-tab approve path in
/// `views/profile/tiers.rs`).
pub fn start(nest: Arc<NestClient>, secret: [u8; 32], runtime: &tokio::runtime::Handle) {
    let my_gen = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    runtime.spawn(async move {
        let author = SubscriptionsAuthor::over(
            Arc::clone(&nest),
            ActorKeypair::from_secret(secret),
            crate::account_runtime::period_key_store(),
        );
        run_author_reconcile_loop(&author, &GENERATION, my_gen, tokio::time::sleep).await;
    });
}
