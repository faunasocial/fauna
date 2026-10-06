//! Process-wide accessor for the shared `ConversationsManager`.
//!
//! Mirrors `ConversationsManagerHost.cs` on Windows: a single instance
//! lives for the lifetime of the app. In e2e mode (`crate::e2e_mode_enabled`
//! — AT-SPI bridge or in-process agent) the host registers a `MockRailBackend`
//! for every rail at construction so `inject_inbound_for_test` can route
//! inbound messages without the test agent having to construct backends.
//! Production wire-up of real backends lives in the per-rail follow-on slice
//! (tracked internally).

use std::sync::{Arc, OnceLock};

use fauna_conversations::ConversationsManager;

static INSTANCE: OnceLock<Arc<ConversationsManager>> = OnceLock::new();

/// Returns the singleton `ConversationsManager`, constructing it on
/// first call. In e2e mode (`crate::e2e_mode_enabled`) the construction
/// also installs mock rail backends.
pub fn manager() -> Arc<ConversationsManager> {
    INSTANCE
        .get_or_init(|| {
            let m = ConversationsManager::new();
            // Two gates, both required. `e2e_mode_enabled()` is the runtime switch
            // that picks agent-on vs agent-off *within* a test-capable build; the
            // `cfg` is the outer security boundary that keeps the seam out of the
            // release binary entirely (`docs/goal/architecture/testing.md`
            // convention 15 — "a runtime env-var gate alone is not enough").
            #[cfg(any(debug_assertions, feature = "e2e-agent"))]
            if crate::e2e_mode_enabled() {
                m.install_mock_backends_for_test();
            }
            m
        })
        .clone()
}
