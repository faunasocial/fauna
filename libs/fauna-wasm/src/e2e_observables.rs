//! The web leg of the two window-claim observables the connection-gap
//! journeys rest on — `fauna_e2e_agent::CONNECTION_REPORTS_KEY` and
//! `fauna_e2e_agent::PAINTED_ERRORS_KEY` — counted by the natives' own code
//! (`fauna-e2e-contract`), never a TypeScript twin of it.
//!
//! Connection reports are counted inside the transport's reconnect loop
//! (`fauna_rpc_wasm::connection_reports_json`), where every report is visible:
//! the SPA's state callback fires on transitions only, which is exactly what
//! the stickiness proof must see past. Painted errors are fed by the SPA
//! (`$lib/e2e-painted-errors.ts`), one call per painted frame whose error
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
