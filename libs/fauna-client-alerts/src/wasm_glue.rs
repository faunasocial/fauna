//! The JS-observer-shim + registry-cell boilerplate every wasm chunk hosting
//! a [`CriticalAlerts`] registry hand-copied verbatim
//! (`fauna-wasm-atproto-settings`, `fauna-wasm::critical_alerts`) — shared
//! once here, though the registry instance itself deliberately stays
//! per-chunk: each lazy-loaded wasm chunk is a **separately compiled binary
//! with its own linear memory**, so an `Arc<CriticalAlerts>` cannot cross
//! from one chunk's module into another's (the chunk-level doc comments this
//! module's two callers carry explain the split in full). Declaring an
//! [`AlertsRegistryCell`] `static` in each chunk still gives each its own
//! private instance, since each chunk statically links this crate's code
//! independently.

use std::sync::{Arc, OnceLock, Weak};

use wasm_bindgen::prelude::*;

use crate::{CriticalAlerts, CriticalAlertsObserver};

#[wasm_bindgen]
extern "C" {
    pub type JsCriticalAlertsObserver;
    #[wasm_bindgen(method, catch, js_name = onChanged)]
    fn on_changed(this: &JsCriticalAlertsObserver) -> Result<(), JsValue>;
}

/// `Weak`, not `Arc`: the registry owns this shim strongly (in its
/// `observers` list), so a strong back-reference here would be a reference
/// cycle — the registry, and everything it holds, would never drop.
struct AlertsObserverShim {
    observer: JsCriticalAlertsObserver,
    registry: Weak<CriticalAlerts>,
}
// SAFETY: wasm32 is single-threaded; the JS object never crosses a thread.
unsafe impl Send for AlertsObserverShim {}
unsafe impl Sync for AlertsObserverShim {}
impl CriticalAlertsObserver for AlertsObserverShim {
    fn on_changed(&self) {
        // `catch`: a throwing JS observer must not unwind through the posting
        // feeder — the wasm half of `CriticalAlerts::notify`'s panic
        // containment, which cannot catch across the JS boundary itself
        // (`critical-alerts.md` § Mechanism owns the dispatch contract). A
        // throw that reaches here must still be COUNTED — `notify`'s own
        // `catch_unwind` around this call never sees it (it never panics),
        // so this is the only place the wasm half of a faulted hand-off is
        // observable at all.
        if self.observer.on_changed().is_err()
            && let Some(registry) = self.registry.upgrade()
        {
            registry.note_observer_fault();
        }
    }
}

/// A chunk-private `CriticalAlerts` registry cell. Declare one `static` of
/// this type per wasm chunk that hosts a feeder or aggregates alerts —
/// despite the shared type, each chunk's `static` is its own instance
/// (see the module doc).
pub struct AlertsRegistryCell(OnceLock<Arc<CriticalAlerts>>);

impl AlertsRegistryCell {
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }

    /// The registry this chunk's feeders/shell post to and read, constructed
    /// once per loaded module instance (an app session, on web).
    pub fn get(&self) -> Arc<CriticalAlerts> {
        self.0
            .get_or_init(|| Arc::new(CriticalAlerts::new()))
            .clone()
    }
}

impl Default for AlertsRegistryCell {
    fn default() -> Self {
        Self::new()
    }
}

/// `subscribeCriticalAlerts`'s shared body: wire a JS observer object into
/// `registry`, with the same throw-containment every chunk needs.
pub fn subscribe(registry: &AlertsRegistryCell, observer: JsCriticalAlertsObserver) {
    let reg = registry.get();
    let weak = Arc::downgrade(&reg);
    reg.subscribe(Arc::new(AlertsObserverShim {
        observer,
        registry: weak,
    }));
}

/// `criticalAlertsActive`'s shared body: the active alerts as a JSON array of
/// `{ key, lines: [{ key, args }] }`, parsed by the caller and each line
/// resolved through `resolveLocalized`.
pub fn active_json(registry: &AlertsRegistryCell) -> String {
    serde_json::to_string(&registry.get().active()).unwrap_or_default()
}

/// `clearAllCriticalAlerts`'s shared body: drop every active alert on this
/// chunk's registry — the identity-teardown boundary (`critical-alerts.md`
/// § Mechanism → *Lifetime*).
pub fn clear_all(registry: &AlertsRegistryCell) {
    registry.get().clear_all();
}
