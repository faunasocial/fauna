//! `#[wasm_bindgen]` face over the admin audit log section on `admin-logs`
//! (`fauna_client_admin::AuditLog` + `audit_log_page`; `admin.md` § The admin
//! audit log on `admin-logs`) — the web twin of the UniFFI `FfiAuditLog`
//! (`libs/fauna-ffi/src/admin.rs`). The entries fetched so far stay in wasm;
//! each read resolves to the folded section (`AuditLogView`: `rows[]` of
//! `{ id, ts_ms, text }`, `integrity`, `integrity_text`, `more_before_id`), so
//! the SPA holds no hash and re-derives no check.

use std::cell::RefCell;
use std::rc::Rc;

use fauna_client_admin::{AdminClient, AuditLog, audit_log_page};
use js_sys::Promise;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use crate::rpc::{WsRpcClient, err_to_js, to_js};

/// The section's state, held in wasm while `admin-logs` is open.
#[wasm_bindgen]
pub struct WasmAuditLog {
    nest: fauna_rpc_wasm::WsRpcClient,
    log: Rc<RefCell<AuditLog>>,
}

#[wasm_bindgen]
impl WasmAuditLog {
    /// A fresh, empty section over `client`'s nest; call [`Self::load`].
    #[wasm_bindgen(constructor)]
    pub fn new(client: &WsRpcClient) -> WasmAuditLog {
        WasmAuditLog {
            nest: client.inner_client(),
            log: Rc::new(RefCell::new(AuditLog::default())),
        }
    }

    /// Read the newest page (entering the page, or a refresh), replacing what
    /// was held.
    pub fn load(&self) -> Promise {
        self.read(None)
    }

    /// `admin-audit-more-button` — read the page older than the oldest entry
    /// held; with the whole record already shown, resolve to the held view.
    pub fn more(&self) -> Promise {
        match self.log.borrow().view().more_before_id {
            Some(id) => self.read(Some(id)),
            None => {
                let view = self.log.borrow().view();
                future_to_promise(async move { to_js(&view) })
            }
        }
    }

    fn read(&self, before_id: Option<i64>) -> Promise {
        let nest = self.nest.clone();
        let log = Rc::clone(&self.log);
        future_to_promise(async move {
            let page = audit_log_page(&AdminClient::new(nest), before_id)
                .await
                .map_err(err_to_js)?;
            let mut log = log.borrow_mut();
            log.absorb(page);
            to_js(&log.view())
        })
    }
}
