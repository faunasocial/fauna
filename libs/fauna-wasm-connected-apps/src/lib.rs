//! WASM bindings for the Fauna connected-apps page — the page-level
//! `ConnectedAppsMachine` (the roster, the Requests tray, Connect an app,
//! per-client blocks; `docs/goal/ui/connected-apps.md`). The web route renders
//! off the shared machine via `#lib/wasm-connected-apps`, exactly as the
//! labeler-catalog route renders off `LabelerCatalogMachine` — one shared
//! surface, all 7 apps (priority #1/#2). Mirrors `fauna-wasm-labeler-catalog`.

#[cfg(target_arch = "wasm32")]
mod bindings {
    use std::sync::Arc;

    use wasm_bindgen::prelude::*;

    use fauna_client_connected_apps::{
        ConnectedAppsMachine as InnerMachine, ConnectedAppsObserver as InnerObserver,
    };
    use fauna_client_mail_settings::machine::MailSettingsMachine;
    use fauna_client_mail_settings::rpc_glue::build_mail_settings_machine;
    use fauna_core::identity::ActorKeypair;

    #[wasm_bindgen]
    extern "C" {
        pub type JsConnectedAppsObserver;
        #[wasm_bindgen(method, js_name = onChanged)]
        fn on_changed(this: &JsConnectedAppsObserver);
        #[wasm_bindgen(js_namespace = console, js_name = warn)]
        fn console_warn(message: &str);
    }

    struct ObserverShim(JsConnectedAppsObserver);
    // SAFETY: wasm32 is single-threaded; the JS object never crosses a thread.
    unsafe impl Send for ObserverShim {}
    unsafe impl Sync for ObserverShim {}
    impl InnerObserver for ObserverShim {
        fn on_changed(&self) {
            self.0.on_changed()
        }
    }

    #[wasm_bindgen]
    pub struct ConnectedAppsMachine {
        inner: Arc<InnerMachine>,
        mail: Arc<MailSettingsMachine>,
    }

    #[wasm_bindgen]
    impl ConnectedAppsMachine {
        /// Build the page machine over the SPA core chunk's socket, lent as
        /// `port` (a `SharedRpcPort` — `#lib/rpc`'s `sharedRpcPort`), so web
        /// keeps one WebSocket per actor. Throws on an object that is not a
        /// port. State starts empty; call `refresh()` to populate it.
        ///
        /// The mail app-password rows ride a mail-settings machine built HERE,
        /// over the same socket — another chunk's object cannot cross into this
        /// one, so the session's Mail & Calendar machine is out of reach. It is
        /// the one the mail page builds (`secret` the actor's 32-byte seed,
        /// `node_url` the nest the MUA login names), reading the account's mail
        /// custody and succession ledger through `account` — the tab's account
        /// port (`#lib/account-runtime`'s `sharedAccountPort`, minted for this
        /// secret's account), which reaches the core chunk's runtime. Throws on
        /// a secret that is not 32 bytes, or on an object that is not a port.
        #[wasm_bindgen(constructor)]
        pub fn new(
            observer: JsConnectedAppsObserver,
            port: fauna_rpc_wasm::JsRpcPort,
            secret: Vec<u8>,
            account: fauna_account_port::JsAccountPort,
            node_url: String,
        ) -> Result<ConnectedAppsMachine, JsValue> {
            let arr: [u8; 32] = secret
                .try_into()
                .map_err(|_| JsValue::from_str("secret must be 32 bytes"))?;
            let keypair = ActorKeypair::from_secret(arr);
            let consent_keypair = ActorKeypair::from_secret(arr);
            let observer: Arc<dyn InnerObserver> = Arc::new(ObserverShim(observer));
            let client = fauna_rpc_wasm::WsRpcClient::over_port(port.into())
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            // Neither the port nor `JsAccountTransport` is `Clone`: each seam's
            // forwarder wraps its own handle on the one JS port object.
            let ledger = fauna_client_config::succession_ledger_port::from_js_port(
                account.clone().unchecked_into(),
                keypair.actor_id(),
            )
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
            // The consent-time grant seams (`fauna:records:rw:`, `folder:read`,
            // `post.author` approves) — the same set the AT Protocol page wires,
            // over this tab's account port and this chunk's socket.
            let consent_seams = fauna_atproto_settings_machine::consent_grant_seams_over_port(
                &consent_keypair,
                client.clone(),
                &account,
            )
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
            let mail_transport = fauna_account_port::JsAccountTransport::new(account.into())
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            let mail = Arc::new(build_mail_settings_machine(
                client.clone(),
                keypair,
                Arc::new(fauna_client_config::mail_port::PortMailStore::new(
                    mail_transport,
                )),
                &node_url,
                ledger,
                // The roster never reads the served-WebDAV row.
                None,
            ));
            let inner = fauna_client_connected_apps::build_connected_apps_machine(
                client,
                observer,
                Some(Arc::clone(&mail)),
            );
            inner.set_consent_grant_seams(Arc::new(consent_seams));
            Ok(ConnectedAppsMachine { inner, mail })
        }

        /// The whole renderable page in one JSON object
        /// (`{ loaded, requests, principals, blocked, error }`).
        #[wasm_bindgen(js_name = snapshotJson)]
        pub fn snapshot_json(&self) -> String {
            serde_json::to_string(&self.inner.snapshot()).unwrap_or_default()
        }

        /// A visit's re-read: hydrate this chunk's mail machine, then re-read
        /// the roster, the pending requests and the blocks. The hydrate comes
        /// first because the roster's mail read makes no nest call — it
        /// re-reads the mail machine's rows under the mail page's own gate,
        /// whose nest-sourced half only a hydrate reads (the native apps' VMs
        /// hydrate a fresh mail machine per visit the same way). It is the
        /// hydrate WITHOUT the generation heal: the heal's writes do not cross
        /// the account port, and an owed row is the mail page's to heal. A
        /// failed hydrate is not the page's error: the roster's own read
        /// reports a custody it cannot reach, and still paints every other row.
        #[wasm_bindgen(js_name = refresh)]
        pub fn refresh(&self) -> js_sys::Promise {
            let inner = Arc::clone(&self.inner);
            let mail = Arc::clone(&self.mail);
            wasm_bindgen_futures::future_to_promise(async move {
                if let Err(e) = mail.hydrate_without_heal().await {
                    console_warn(&format!("connected apps: mail hydrate failed: {e}"));
                }
                inner.refresh().await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Read the mail app password `key`'s secret on demand — resolves to
        /// the secret's text, or `null` for a row with no secret and on a
        /// failed read (which sets the page error). Never part of the snapshot.
        #[wasm_bindgen(js_name = revealSecret, unchecked_return_type = "Promise<string | null>")]
        pub fn reveal_secret(&self, key: String) -> js_sys::Promise {
            let inner = Arc::clone(&self.inner);
            wasm_bindgen_futures::future_to_promise(async move {
                Ok(match inner.reveal_secret(key).await {
                    Some(secret) => JsValue::from_str(secret.as_str()),
                    None => JsValue::NULL,
                })
            })
        }

        /// Connect an app: look up the typed code.
        #[wasm_bindgen(js_name = submitCode)]
        pub fn submit_code(&self, code: String) -> js_sys::Promise {
            let inner = Arc::clone(&self.inner);
            wasm_bindgen_futures::future_to_promise(async move {
                inner.submit_code(code).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// The same-device handoff: open the request whose PAR handle a
        /// `fauna://consent/<request_uri>` route carried.
        #[wasm_bindgen(js_name = openHandoff)]
        pub fn open_handoff(&self, request_uri: String) -> js_sys::Promise {
            let inner = Arc::clone(&self.inner);
            wasm_bindgen_futures::future_to_promise(async move {
                inner.open_handoff(request_uri).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// *Connect* on a hosted plugin's settings card: open this account's
        /// bind card for the plugin whose install row is `principal_id_hex`
        /// (`InstalledPluginText`'s / `PluginCardText`'s hex). A string that is
        /// not even-length hex is ignored — the card cannot name one.
        #[wasm_bindgen(js_name = bindPlugin)]
        pub fn bind_plugin(&self, principal_id_hex: String) -> js_sys::Promise {
            let inner = Arc::clone(&self.inner);
            wasm_bindgen_futures::future_to_promise(async move {
                if let Some(id) = fauna_core::format::hex_decode(&principal_id_hex) {
                    inner.bind_plugin(id).await;
                }
                Ok(JsValue::UNDEFINED)
            })
        }

        /// *Disconnect* on a hosted plugin's settings card: revoke this
        /// account's binding row, `PluginCardText::binding_principal_id_hex`.
        #[wasm_bindgen(js_name = revokePrincipal)]
        pub fn revoke_principal(&self, principal_id_hex: String) -> js_sys::Promise {
            let inner = Arc::clone(&self.inner);
            wasm_bindgen_futures::future_to_promise(async move {
                if let Some(id) = fauna_core::format::hex_decode(&principal_id_hex) {
                    inner.revoke_principal(id).await;
                }
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Approve or deny the pending request `consent_id_hex`.
        #[wasm_bindgen(js_name = resolveRequest)]
        pub fn resolve_request(&self, consent_id_hex: String, approved: bool) -> js_sys::Promise {
            let inner = Arc::clone(&self.inner);
            wasm_bindgen_futures::future_to_promise(async move {
                inner.resolve_request(consent_id_hex, approved).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// "Never show requests from this app" for the request `consent_id_hex`.
        #[wasm_bindgen(js_name = blockRequest)]
        pub fn block_request(&self, consent_id_hex: String) -> js_sys::Promise {
            let inner = Arc::clone(&self.inner);
            wasm_bindgen_futures::future_to_promise(async move {
                inner.block_request(consent_id_hex).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Lift the block on `client_id`.
        #[wasm_bindgen(js_name = unblock)]
        pub fn unblock(&self, client_id: String) -> js_sys::Promise {
            let inner = Arc::clone(&self.inner);
            wasm_bindgen_futures::future_to_promise(async move {
                inner.unblock(client_id).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Flip the account's external-apps switch
        /// (`atproto-external-apps-enable`) and re-read the page; the toggle
        /// paints the re-read's `externalAppsOff`, never a local guess.
        #[wasm_bindgen(js_name = setExternalAppsEnabled)]
        pub fn set_external_apps_enabled(&self, enabled: bool) -> js_sys::Promise {
            let inner = Arc::clone(&self.inner);
            wasm_bindgen_futures::future_to_promise(async move {
                inner.set_external_apps_enabled(enabled).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Revoke the roster row `key` — the machine picks the verb.
        #[wasm_bindgen(js_name = revoke)]
        pub fn revoke(&self, key: String) -> js_sys::Promise {
            let inner = Arc::clone(&self.inner);
            wasm_bindgen_futures::future_to_promise(async move {
                inner.revoke(key).await;
                Ok(JsValue::UNDEFINED)
            })
        }
    }

    /// A roster row's text, composed once in shared Rust
    /// (`fauna_client_connected_apps::compose::roster_row_text_with`): takes one
    /// snapshot `principals` row as JSON and `format_time`, a JS
    /// `(unixMillis: number) => string` (web's `localTimestamp` — wasm has no OS
    /// timezone database), and returns `{ lines, facts }` as JSON, each entry a
    /// `ComposedText` (`{ text: { key, args }, nested: { name: { key, args } } }`)
    /// the page resolves through its own string table. Throws on a row that is
    /// not a `ConnectedAppRow`.
    #[wasm_bindgen(js_name = rosterRowTextJson)]
    pub fn roster_row_text_json(
        row_json: &str,
        format_time: &js_sys::Function,
    ) -> Result<String, JsValue> {
        let row: fauna_client_connected_apps::ConnectedAppRow = serde_json::from_str(row_json)
            .map_err(|e| JsValue::from_str(&format!("not a connected-app row: {e}")))?;
        let format = |ms: i64| {
            format_time
                .call1(&JsValue::NULL, &JsValue::from_f64(ms as f64))
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default()
        };
        let text = fauna_client_connected_apps::compose::roster_row_text_with(&row, &format);
        serde_json::to_string(&text).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// What a roster row's facts line puts between its facts
    /// (`compose::FACTS_SEPARATOR`).
    #[wasm_bindgen(js_name = factsSeparator)]
    pub fn facts_separator() -> String {
        fauna_client_connected_apps::compose::FACTS_SEPARATOR.to_string()
    }

    /// A request card's lines, composed once in shared Rust
    /// (`compose::request_card_lines`): takes one snapshot `requests` entry's
    /// consent card (`ConsentCardRow`) as JSON and returns the lines as a JSON
    /// array of `ComposedText`. Throws on a card that is not a `ConsentCardRow`.
    #[wasm_bindgen(js_name = requestCardLinesJson)]
    pub fn request_card_lines_json(consent_json: &str) -> Result<String, JsValue> {
        let consent: fauna_atproto_settings_machine::ConsentCardRow =
            serde_json::from_str(consent_json)
                .map_err(|e| JsValue::from_str(&format!("not a consent card: {e}")))?;
        let lines = fauna_client_connected_apps::compose::request_card_lines(&consent);
        serde_json::to_string(&lines).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// A request card's install section, composed once in shared Rust
    /// (`compose::request_install_text`): takes one snapshot `requests` entry's
    /// consent card (`ConsentCardRow`) as JSON and returns the
    /// `InstallSectionText` as JSON, or `null` on every card but an admin's
    /// hosted-plugin install card. Throws on a card that is not a
    /// `ConsentCardRow`.
    #[wasm_bindgen(js_name = requestInstallTextJson)]
    pub fn request_install_text_json(consent_json: &str) -> Result<String, JsValue> {
        let consent: fauna_atproto_settings_machine::ConsentCardRow =
            serde_json::from_str(consent_json)
                .map_err(|e| JsValue::from_str(&format!("not a consent card: {e}")))?;
        let section = fauna_client_connected_apps::compose::request_install_text(&consent);
        serde_json::to_string(&section).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// What the install section's multi-line leaves put between their lines
    /// (`compose::LINE_SEPARATOR`).
    #[wasm_bindgen(js_name = lineSeparator)]
    pub fn line_separator() -> String {
        fauna_client_connected_apps::compose::LINE_SEPARATOR.to_string()
    }

    // Each wasm chunk is its own module with its own Rust runtime, so the panic
    // hook is installed per chunk (see the `fauna-wasm-panic-hook` crate doc).
    #[wasm_bindgen(start)]
    fn panic_hook_start() {
        fauna_wasm_panic_hook::install("fauna-wasm-connected-apps");
    }

    /// Test-only: deliberately panics, so an e2e can assert the hook above names
    /// this chunk. Compiled out of every non-`test-helpers` build.
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = panicForTestOnly)]
    pub fn panic_for_test_only() {
        panic!("deliberate test panic");
    }
}
