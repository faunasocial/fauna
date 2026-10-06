//! tui runtime wiring for the shared author-side subscription reconciliation.
//!
//! **Scheduling only.** What a tick *does*, how long to wait between ticks, and
//! the loop itself are shared — [`run_author_reconcile_loop`] in
//! `fauna-client-subscriptions` (priority #2; linux and tui both already run a
//! bare tokio runtime with no FFI/wasm boundary, so unlike the other five apps
//! they can share the loop body too — see that fn's doc comment). This file
//! owns just the genuinely platform-specific part: spawning the loop at login
//! (`session::establish`) and tracking the login generation that stops it once
//! a newer login supersedes it.
//!
//! All mint / seal / rotation crypto likewise stays in shared Rust; this file is
//! transport glue only.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fauna_client::NestClient;
use fauna_client_subscriptions::orchestration::{SubscriptionsAuthor, run_author_reconcile_loop};
use fauna_core::identity::ActorKeypair;

/// Monotonic login generation. Each [`start`] bumps it and captures its own
/// value; the loop exits once a newer login supersedes it. The e2e drives
/// repeated re-auth within one process (the agent's `set_state` login per test),
/// so without this each test's loop would leak and keep polling the prior
/// (torn-down) nest.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Spawn the author-side reconcile loop for the just-authenticated actor.
/// Called from `session::establish`, alongside the other post-auth page inits.
/// `secret` is the 32-byte identity seed (the owner's account-plane custody is
/// derived from it, exactly as the Tiers-tab approve path in `profile::tiers`).
pub fn start(
    nest: Arc<NestClient>,
    secret: [u8; 32],
    period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
) {
    let my_gen = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    tokio::spawn(async move {
        let author = SubscriptionsAuthor::over(
            Arc::clone(&nest),
            ActorKeypair::from_secret(secret),
            period_keys,
        );
        run_author_reconcile_loop(&author, &GENERATION, my_gen, tokio::time::sleep).await;
    });
}
