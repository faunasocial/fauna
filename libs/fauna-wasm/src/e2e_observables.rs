//! The web leg of the shared e2e observables counted in Rust: the two
//! window-claim observables the connection-gap journeys rest on —
//! `fauna_e2e_agent::CONNECTION_REPORTS_KEY` and
//! `fauna_e2e_agent::PAINTED_ERRORS_KEY` — and the knock-toast log
//! (`fauna_e2e_agent::KNOCK_NOTICES_KEY`), each counted by the natives' own
//! code (`fauna-e2e-contract`), never a TypeScript twin of it.
//!
//! Connection reports are counted inside the transport's reconnect loop
//! (`fauna_rpc_wasm::connection_reports_json`), where every report is visible:
//! the SPA's state callback fires on transitions only, which is exactly what
//! the stickiness proof must see past. Painted errors are fed by the SPA
//! (`#lib/e2e-painted-errors.ts`), one call per painted frame whose error
//! surfaces changed, since only the page knows what it painted.
//!
//! `test-helpers` only (convention 15): none of this is in a production bundle.
use std::cell::RefCell;

use wasm_bindgen::prelude::*;

#[cfg(feature = "test-helpers")]
thread_local! {
    static PAINTED_ERRORS: RefCell<fauna_e2e_contract::PaintedErrorTally> =
        RefCell::new(fauna_e2e_contract::PaintedErrorTally::default());
}

/// The `connection_reports` state value, as JSON text.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = connectionReportsForTest)]
pub fn connection_reports_for_test() -> String {
    fauna_rpc_wasm::connection_reports_json()
}

/// Record one painted frame's error surfaces: `ids[i]` painted `texts[i]`. The
/// tally filters to error surfaces and drops empty text itself, so the caller
/// may pass every candidate it found.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = paintedErrorsObserveForTest)]
pub fn painted_errors_observe_for_test(ids: Vec<String>, texts: Vec<String>) {
    PAINTED_ERRORS.with_borrow_mut(|tally| {
        tally.observe(
            ids.iter()
                .map(String::as_str)
                .zip(texts.iter().map(String::as_str)),
        );
    });
}

/// The `painted_errors` state value, as JSON text.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = paintedErrorsForTest)]
pub fn painted_errors_for_test() -> String {
    PAINTED_ERRORS.with_borrow(|tally| tally.json().to_string())
}

// The web leg of `fauna_e2e_agent::KNOCK_NOTICES_KEY` — the knock toasts this
// tab raised, the witness for `notifications` outcome 15. The shared
// `fauna_e2e_contract::KnockNotices` (tui's, linux's and android's log too),
// fed by the SPA's one firing site (`#lib/knock-toast.ts`): `observe` before
// the muted-word gate, `record` after the browser `Notification` was handed
// over, so the log means "the user was shown this".
#[cfg(feature = "test-helpers")]
thread_local! {
    static KNOCK_NOTICES: RefCell<fauna_e2e_contract::KnockNotices> =
        RefCell::new(fauna_e2e_contract::KnockNotices::default());
}

/// A knock push reached the firing arm (called before the muted-word gate).
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = knockNoticesObserveForTest)]
pub fn knock_notices_observe_for_test() {
    KNOCK_NOTICES.with_borrow_mut(|log| log.observe());
}

/// A knock toast was raised: the catalog `key` its text resolved from (`None`
/// for a verbatim text), the painted `text`, and the id it replaces under.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = knockNoticesRecordForTest)]
pub fn knock_notices_record_for_test(key: Option<String>, text: String, replace_id: Option<u32>) {
    KNOCK_NOTICES.with_borrow_mut(|log| log.record(key.as_deref(), &text, replace_id));
}

/// The `knock_notices` state value, as JSON text.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = knockNoticesForTest)]
pub fn knock_notices_for_test() -> String {
    KNOCK_NOTICES.with_borrow(|log| log.json().to_string())
}
