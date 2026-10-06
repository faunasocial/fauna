//! Web multi-account registry glue (Stage 1 web,
//! `docs/goal/architecture/long-term-store.md` § Multi-account evolution).
//!
//! The shared account list + active pointer live in
//! `fauna_client_accounts` (priority #2); this module provides only the thin
//! [`LocalStorageSecretStore`] glue over `web_sys::Storage` — the browser twin of
//! native `CredentialStore` / `KeychainStore` — plus a `#[wasm_bindgen]`
//! [`WasmAccountRegistry`] the Svelte account-settings switcher drives.
//!
//! Web consumes [`AccountRegistry`](fauna_client_accounts::AccountRegistry)
//! **directly** for the switcher and the wizard's hand-off, and routes launch
//! through `fauna-wasm-launch`'s registry-backed `LaunchPersistence`. Every
//! localStorage key is a registry logical key verbatim (`fauna/index`, the
//! `fauna/{actor}/…` slots, the install device secret); the SPA reads its
//! identity through [`WasmAccountRegistry::session_material`] and never through
//! a single-slot key. The pre-registry `fauna_secret` / `fauna_node_url` / …
//! keys and the `mirrorActiveToLegacy` / `ensureMigrated` exports that fed them
//! are gone (2026-09-24, `long-term-store.md` § Downgrade mirror +
//! abandoned-append recovery).
//!
//! **Every mutator below is a Promise, and the reason is the cross-tab lock.**
//! Tabs are web's concurrent instances and share one `localStorage`, so every
//! registry mutator's read-modify-write of `fauna/index` races its siblings
//! exactly as native processes race a file. Native serializes inside the
//! shared registry with a file lock; a Web Lock is asynchronous and the shared
//! mutators are not, so here each mutator runs its synchronous section inside
//! [`with_web_mutation_lock`] ([`fauna_client_accounts::web_mutation_lock`],
//! the wasm twin of the native `MutationLock`) and hands JS a Promise. Reads
//! stay synchronous and never lock (`long-term-store.md` § Multi-account
//! evolution → *Cross-process mutation lock*, the web leg).

#![cfg(target_arch = "wasm32")]

use std::sync::Arc;

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

// The store lives in the shared crate (`fauna-client-accounts/src/web_store.rs`,
// feature `web-localstorage`) because `fauna-wasm-launch` is its second
// consumer — two wasm chunks share code only through a common crate.
use fauna_client_accounts::{AccountRegistry, LocalStorageSecretStore, with_web_mutation_lock};

/// The account-settings switcher's handle onto the shared [`AccountRegistry`],
/// backed by [`LocalStorageSecretStore`]. Cheap to construct (both are
/// stateless), so the Svelte layer builds one per operation.
#[wasm_bindgen]
pub struct WasmAccountRegistry {
    registry: AccountRegistry,
}

/// Run `mutate` over a fresh registry view inside the cross-tab mutation lock
/// and hand JS the outcome as a Promise (rejecting with the mutator's error
/// as a JS `Error`, exactly as the `?` of a synchronous export would throw
/// it). A fresh view rather than `self`'s because the section crosses into a
/// `'static` JS callback; the registry is stateless, so the two are one store.
fn mutate(
    mutate: impl FnOnce(&AccountRegistry) -> Result<JsValue, JsError> + 'static,
) -> js_sys::Promise {
    future_to_promise(async move {
        with_web_mutation_lock(move || {
            let registry = AccountRegistry::new(Arc::new(LocalStorageSecretStore));
            mutate(&registry)
        })
        .await
        .map_err(JsValue::from)
    })
}

/// The activation both switch exports share, under the same cross-tab lock as
/// every mutator. A refusal rejects with the SHARED line as a `LocalizedText`
/// `{ key, args }` (`fauna_client_accounts::switch_refused_copy`) — never the
/// registry's debug text — naming the target by its switcher label and saying
/// the user is still where they were (`long-term-store.md` § Multi-account
/// evolution, "Activating refuses an account it cannot launch as"). The SPA
/// resolves it with `resolveLocalized`, as every wasm `LocalizedText`.
fn activate(actor_id: String, confirmed: bool) -> js_sys::Promise {
    future_to_promise(async move {
        with_web_mutation_lock(move || {
            let registry = AccountRegistry::new(Arc::new(LocalStorageSecretStore));
            let result = if confirmed {
                registry.set_active_confirmed(&actor_id)
            } else {
                registry.set_active(&actor_id)
            };
            match result {
                Ok(()) => Ok(JsValue::UNDEFINED),
                Err(err) => {
                    let label = registry
                        .list()
                        .into_iter()
                        .find(|a| a.actor_id == actor_id)
                        .map(|a| {
                            fauna_core::format::account_display_label(
                                a.handle.as_deref(),
                                &a.actor_id,
                            )
                        })
                        .unwrap_or_else(|| actor_id.clone());
                    Err(
                        crate::rpc::to_js(&fauna_client_accounts::switch_refused_copy(
                            &err, &label,
                        ))
                        .unwrap_or_else(|e| e),
                    )
                }
            }
        })
        .await
    })
}

#[wasm_bindgen]
impl WasmAccountRegistry {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        let registry = AccountRegistry::new(Arc::new(LocalStorageSecretStore));
        Self { registry }
    }

    /// The active account's actor id (hex), or `None`.
    #[wasm_bindgen(js_name = activeActorId)]
    pub fn active_actor_id(&self) -> Option<String> {
        self.registry.active()
    }

    /// The account list as a JSON array of `AccountEntry`
    /// (`{actor_id, handle, domain, tier, require_confirm_to_activate}`), in add
    /// order — the switcher renders one `account-switcher-item` per entry.
    #[wasm_bindgen(js_name = listJson)]
    pub fn list_json(&self) -> String {
        serde_json::to_string(&self.registry.list()).unwrap_or_else(|_| "[]".to_string())
    }

    /// Make `actor_id` the active account (the switch primitive). The caller
    /// follows with an SPA re-init, which re-reads
    /// [`Self::session_material`] for the now-active account.
    #[wasm_bindgen(js_name = setActive, unchecked_return_type = "Promise<void>")]
    pub fn set_active(&self, actor_id: &str) -> js_sys::Promise {
        activate(actor_id.to_string(), false)
    }

    /// [`Self::set_active`], asserting the user has **just completed** a
    /// re-auth confirmation for this activation (Stage 2 — the account's
    /// `require_confirm_to_activate` flag is set, which makes plain
    /// `setActive` refuse). Only ever call adjacent to the re-auth prompt;
    /// call sites are the audit surface for the gate.
    #[wasm_bindgen(js_name = setActiveConfirmed, unchecked_return_type = "Promise<void>")]
    pub fn set_active_confirmed(&self, actor_id: &str) -> js_sys::Promise {
        activate(actor_id.to_string(), true)
    }

    /// Add (or update) an account from its secret hex + optional slots. Derives
    /// the actor id; the first account becomes active. Resolves to the actor id.
    /// Append-mode "Add account" calls this on onboarding success.
    #[wasm_bindgen(js_name = addAccount, unchecked_return_type = "Promise<string>")]
    pub fn add_account(
        &self,
        secret_hex: &str,
        nest_url: Option<String>,
        device_id: Option<String>,
    ) -> js_sys::Promise {
        let secret_hex = secret_hex.to_string();
        mutate(move |r| {
            let id = r.add_account(&secret_hex, nest_url.as_deref(), device_id.as_deref())?;
            Ok(JsValue::from(id))
        })
    }

    /// Persist the predecessor seeds a phrase-only restore recovered
    /// (`predecessors_json` is the wizard's `restoredPredecessorsJson()`,
    /// `[{actor_id_hex, seed_hex}]`), linked to `restored_actor` — the shared
    /// `AccountRegistry::persist_restored_predecessors` tui and linux call.
    /// Empty JSON is a no-op; malformed JSON is logged and lands nothing
    /// (the account itself is already back).
    #[wasm_bindgen(js_name = persistRestoredPredecessors, unchecked_return_type = "Promise<void>")]
    pub fn persist_restored_predecessors(
        &self,
        restored_actor: Option<String>,
        predecessors_json: &str,
    ) -> js_sys::Promise {
        #[derive(serde::Deserialize)]
        struct Seed {
            actor_id_hex: String,
            seed_hex: fauna_core::secret::SecretString,
        }
        let seeds: Vec<Seed> = match serde_json::from_str(predecessors_json) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("restored predecessors unreadable: {e}");
                return js_sys::Promise::resolve(&JsValue::UNDEFINED);
            }
        };
        mutate(move |r| {
            r.persist_restored_predecessors(
                restored_actor.as_deref(),
                seeds
                    .iter()
                    .map(|s| (s.seed_hex.as_str(), s.actor_id_hex.as_str())),
            );
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Moment 1 — the confirm-identity commit point, through the shared
    /// `fauna_client_accounts::persist_confirmed_identity`: creates the
    /// per-actor account, **reads the secret back** (refusing a store that
    /// silently kept nothing — the one write whose silent failure destroys an
    /// account outright), activates, and retracts the previous run's abandoned
    /// identity. Resolves to the actor id.
    ///
    /// `append` is the "Add account" wizard over a live session: then the call
    /// **writes nothing** and only derives the actor id — the appended identity
    /// stays in the wizard machine (`effectiveSecret()`) until its own terminal
    /// registers and switches, so an abandoned append can neither leave a
    /// half-account nor shadow the active one. The rule lives in shared Rust,
    /// not in this caller.
    #[wasm_bindgen(js_name = persistConfirmedIdentity, unchecked_return_type = "Promise<string>")]
    pub fn persist_confirmed_identity(&self, secret_hex: &str, append: bool) -> js_sys::Promise {
        let secret_hex = secret_hex.to_string();
        mutate(move |r| {
            let id = fauna_client_accounts::persist_confirmed_identity(r, &secret_hex, append)?;
            Ok(JsValue::from(id))
        })
    }

    /// The `LoggedIn` terminal — the wizard's success exit — through the shared
    /// `fauna_client_accounts::persist_logged_in` moment: `add_account` with the
    /// home nest URL, activate, and spend the pending-invite and awaiting-DNS
    /// slots (both launch rows are evaluated *before* the silent-challenge row,
    /// so a survivor would pin every later launch on the invite or "Almost
    /// ready" surface for a nest the user is already on; the awaiting slot is
    /// cleared HERE and nowhere earlier — `onboarding.md` § Long-term store
    /// contract). Resolves to the actor id. Idempotent, so a re-entered wizard
    /// or a resumed claim is safe.
    ///
    /// ⚠ **Append mode must NOT call this** — the "Add account" wizard's own
    /// terminal registers and switches, and activating here would move `active`
    /// off the live account before that runs (`onboarding.md` § Long-term store
    /// contract → *Append mode is exempt*).
    ///
    /// This is the ONLY place the home nest is recorded: the per-actor
    /// `nest_url` row is what the next launch's routing tuple reads, and there
    /// is no single-slot key beside it any more.
    #[wasm_bindgen(js_name = persistLoggedIn, unchecked_return_type = "Promise<string>")]
    pub fn persist_logged_in(
        &self,
        secret_hex: &str,
        nest_url: &str,
        device_id: Option<String>,
        reach_ipv4: Option<String>,
    ) -> js_sys::Promise {
        let secret_hex = secret_hex.to_string();
        let nest_url = nest_url.to_string();
        mutate(move |r| {
            let id = fauna_client_accounts::persist_logged_in(
                r,
                &secret_hex,
                &nest_url,
                device_id.as_deref(),
                reach_ipv4.as_deref(),
            )?;
            Ok(JsValue::from(id))
        })
    }

    /// This browser's device id for `actor_id` — the account's persisted id,
    /// else `derive_device_id(install_secret, actor_id)` over the install
    /// device secret in the same `localStorage` (web's sign-out is `clearAll`
    /// alone, which never names the secret). One id per account, never one for
    /// every account on the browser (`sync-agent-credentials.md` § Credential
    /// model, the 2026-09-20 ruling, property (2)). `$lib/device-id`'s
    /// `getDeviceId` is the one caller.
    #[wasm_bindgen(js_name = deviceIdForActor)]
    pub fn device_id_for_actor(&self, actor_id: &str) -> Result<String, JsError> {
        Ok(self
            .registry
            .device_id_for_actor(&LocalStorageSecretStore, actor_id)?)
    }

    /// Boot step (`accountsBoot`): mint the install device secret if this
    /// browser has none, inside the cross-tab mutation lock, so two tabs
    /// opened together end on ONE secret — the synchronous
    /// [`Self::device_id_for_actor`] cannot take the async Web Lock itself,
    /// which is why the mint is a separate, guarded step.
    #[wasm_bindgen(js_name = ensureInstallDeviceSecret, unchecked_return_type = "Promise<void>")]
    pub fn ensure_install_device_secret(&self) -> js_sys::Promise {
        mutate(|r| {
            r.ensure_install_device_secret(&LocalStorageSecretStore)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Remove an account: its per-actor slots and index entry, then everything
    /// else this origin holds for it — its account store
    /// (`crate::account_scope`; registry first, then the erase, the native
    /// `account_scope::remove_account` order). If it was active, the first
    /// remaining account becomes active.
    ///
    /// The account is named in the sign-out record before the removal, so a
    /// store the erase could not remove — or a tab closed between the two —
    /// is erased at a later load; a removal the registry refuses leaves the
    /// store untouched and the record clean.
    #[wasm_bindgen(unchecked_return_type = "Promise<void>")]
    pub fn remove(&self, actor_id: &str) -> js_sys::Promise {
        let actor_id = actor_id.to_string();
        future_to_promise(async move {
            fauna_client_accounts::SignOutRecord::record_removal(
                &LocalStorageSecretStore,
                &actor_id,
            );
            let removed = with_web_mutation_lock({
                let actor_id = actor_id.clone();
                move || {
                    AccountRegistry::new(Arc::new(LocalStorageSecretStore))
                        .remove(&actor_id)
                        .map_err(JsError::from)
                }
            })
            .await;
            crate::account_scope::finish_removed_account(&actor_id).await;
            removed.map(|()| JsValue::UNDEFINED).map_err(JsValue::from)
        })
    }

    /// Set the per-account "require confirmation to activate" flag (Stage 2) —
    /// the `account-require-confirm-toggle` write path. Marks the flag
    /// user-set, so the admin auto-default never overrides it afterwards.
    #[wasm_bindgen(js_name = setRequireConfirm, unchecked_return_type = "Promise<void>")]
    pub fn set_require_confirm(&self, actor_id: &str, require: bool) -> js_sys::Promise {
        let actor_id = actor_id.to_string();
        mutate(move |r| {
            r.set_require_confirm(&actor_id, require)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The admin auto-default: flip the flag ON iff the user has never touched
    /// this account's toggle (`long-term-store.md` § Multi-account evolution).
    /// Idempotent; call on every `am-i-admin = true` observation for the
    /// active account. Resolves to whether this call flipped it.
    #[wasm_bindgen(js_name = autoEnableRequireConfirm, unchecked_return_type = "Promise<boolean>")]
    pub fn auto_enable_require_confirm(&self, actor_id: &str) -> js_sys::Promise {
        let actor_id = actor_id.to_string();
        mutate(move |r| Ok(JsValue::from(r.auto_enable_require_confirm(&actor_id)?)))
    }

    /// Sign-out's credential wipe: erase every account's per-actor slots and
    /// the index — `long-term-store.md` § Cleanup contract, the same erase
    /// linux gets from wiping its libsecret namespace. `store.ts` `logout()`
    /// calls it after the runtime's sign-out stop has returned, so the next
    /// load routes to case 3 (fresh onboarding).
    ///
    /// It is the credential half only. The accounts' stores are erased behind
    /// the sign-out record by `crate::account_scope` (`signOutFinish`), which
    /// `logout()` calls next and which runs this same wipe itself when a later
    /// load finds a sign-out a closed tab left unfinished.
    ///
    /// The credential read-back the native seats paint (`CredentialSweep`) is
    /// dropped here, and web is ruled OUT of that residue class for the
    /// credential half rather than behind on it (`account-scoping.md`
    /// § Erasure follows scope): its store is `localStorage`, whose
    /// `removeItem` has no failure mode a later read could disagree with — an
    /// inaccessible storage refuses the read the same way. The store half is a
    /// residue class on web, and its line is not painted yet.
    #[wasm_bindgen(js_name = clearAll, unchecked_return_type = "Promise<void>")]
    pub fn clear_all(&self) -> js_sys::Promise {
        mutate(|r| {
            let _ = r.clear_all();
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Update an account's server-data cache (handle/domain/tier) shown in the
    /// switcher rows. `store.ts`'s `refreshFromServer` calls it for the active
    /// account after a silent sign-in.
    #[wasm_bindgen(js_name = updateCache, unchecked_return_type = "Promise<void>")]
    pub fn update_cache(
        &self,
        actor_id: &str,
        handle: Option<String>,
        domain: Option<String>,
        tier: Option<String>,
    ) -> js_sys::Promise {
        let actor_id = actor_id.to_string();
        mutate(move |r| {
            r.update_cache(
                &actor_id,
                handle.as_deref(),
                domain.as_deref(),
                tier.as_deref(),
            )?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Set the per-actor nest-URL slot (raw slot write).
    #[wasm_bindgen(js_name = setNestUrl, unchecked_return_type = "Promise<void>")]
    pub fn set_nest_url(&self, actor_id: &str, nest_url: &str) -> js_sys::Promise {
        let actor_id = actor_id.to_string();
        let nest_url = nest_url.to_string();
        mutate(move |r| {
            r.set_nest_url(&actor_id, &nest_url);
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Walk away from `actor_id`'s nest, keeping the identity: clear its
    /// (nest_url, device_id) slots — the registry-routed form of "use a
    /// different nest" / a factory-reset walk-away
    /// (`account-scoping.md` § Concurrent instances, the delete corollary).
    /// See `AccountRegistry::clear_nest_binding`.
    #[wasm_bindgen(js_name = clearNestBinding, unchecked_return_type = "Promise<void>")]
    pub fn clear_nest_binding(&self, actor_id: &str) -> js_sys::Promise {
        let actor_id = actor_id.to_string();
        mutate(move |r| {
            r.clear_nest_binding(&actor_id)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The persisted last-known supervision snapshot for `actor_id`, parsed by
    /// its single format owner (`fauna_client_family::SupervisionSnapshot`) and
    /// handed to JS as `{ supervised_by, content_policy, content_notify,
    /// screen_time }` — or `undefined` when there is nothing to enforce: the
    /// slot is absent or malformed ("no information", the ruled fail
    /// direction; family-safety.md § Content policy clause 2), or its last
    /// read said unsupervised (a floor may only ever be restored under the
    /// guardianship that owns it — the same guardian-less refusal the FFI twin
    /// `FfiAccountRegistry::supervision_snapshot` makes unrepresentable). The
    /// layout restores from this at launch, ahead of the first `familyStatus`
    /// read; the write side is that read's own success path
    /// (`rpc.rs::family_status`), so JS never touches the slot format.
    #[wasm_bindgen(js_name = supervisionSnapshot)]
    pub fn supervision_snapshot(&self, actor_id: &str) -> JsValue {
        self.registry
            .supervision_snapshot_json(actor_id)
            .and_then(|raw| fauna_client_family::SupervisionSnapshot::from_json(&raw))
            .filter(fauna_client_family::SupervisionSnapshot::is_supervised)
            .and_then(|snap| crate::rpc::to_js(&snap).ok())
            .unwrap_or(JsValue::UNDEFINED)
    }

    /// The one-read session material for `actor_id` — the browser twin of
    /// `FfiAccountRegistry::session_material`, handed to JS as
    /// `{ actor_id, secret_hex, nest_url, device_id, handle, domain, tier }`,
    /// or `undefined` when the account has no resolvable secret (unknown, or
    /// removed out from under a running tab — the fail-closed answer; a session
    /// must never fall back to another account's material).
    ///
    /// This is the **session-identity read** of `account-scoping.md`
    /// § Concurrent instances → *Session identity resolves through the session's
    /// account* — what every tab reads, pinned (its own account) or primary
    /// (the active account). There is no origin-scoped single slot beside it
    /// any more, so nothing a second tab could share by accident.
    #[wasm_bindgen(js_name = sessionMaterial)]
    pub fn session_material(&self, actor_id: &str) -> JsValue {
        #[derive(serde::Serialize)]
        struct JsSessionMaterial<'a> {
            actor_id: &'a str,
            /// Exposed deliberately: the tab builds its authenticated session
            /// from it, exactly as every native leg does from
            /// `FfiSessionMaterial`. It never leaves the tab's own JS realm.
            secret_hex: &'a str,
            nest_url: Option<&'a str>,
            device_id: Option<&'a str>,
            handle: Option<&'a str>,
            domain: Option<&'a str>,
            tier: Option<&'a str>,
        }

        self.registry
            .session_material(actor_id)
            .and_then(|m| {
                crate::rpc::to_js(&JsSessionMaterial {
                    actor_id: &m.actor_id,
                    secret_hex: m.secret_hex.as_str(),
                    nest_url: m.nest_url.as_deref(),
                    device_id: m.device_id.as_deref(),
                    handle: m.handle.as_deref(),
                    domain: m.domain.as_deref(),
                    tier: m.tier.as_deref(),
                })
                .ok()
            })
            .unwrap_or(JsValue::UNDEFINED)
    }

    /// Resolve a **tab pin** through the succession chain: the terminal
    /// successor of `actor_id`, or `actor_id` itself when it never succeeded.
    ///
    /// The browser twin of `AccountRegistry::resolve_launch_binding`'s rider-2
    /// half (`account-scoping.md` § Concurrent instances → *The binding follows
    /// the account across a succession*, rider 2). A pin names an **account** by
    /// the id that identified it when the tab was pinned; if a sibling tab or
    /// another device has since run the ceremony, the account is the successor,
    /// and refusing would strand a pin the pinner cannot correct. The native
    /// method reads the process-global launch binding (an env var plus a process
    /// cell) and is therefore `cfg`-ed off wasm entirely; a tab's binding lives
    /// in `sessionStorage` instead, so the id arrives as an argument and the
    /// caller re-writes its own pin — the *same* chain walk, over the one shared
    /// `terminal_successor_of`, never a second implementation of the rule.
    #[wasm_bindgen(js_name = resolvePinnedAccount)]
    pub fn resolve_pinned_account(&self, actor_id: &str) -> String {
        self.registry
            .terminal_successor_of(actor_id)
            .unwrap_or_else(|| actor_id.to_string())
    }

    /// Which account-index verdict is active, if any
    /// (`version-compatibility.md` § 5 item 9), as JSON — `undefined` when
    /// there is none. An additive query, not a new rejection shape on any
    /// mutator above: every one of them still flattens
    /// `AccountError::IndexUnreadable`/`IndexMalformed` to an opaque
    /// `JsError` string (wasm-bindgen's blanket `From<E: Error>`), because
    /// `JsError` carries no variant to distinguish on. A caller whose
    /// mutation just failed opaquely can ask this instead of parsing the
    /// message. The wasm twin of `FfiAccountRegistry::index_refusal`; the
    /// snapshot already carries this in the field
    /// `LaunchSnapshot::account_index_refusal` — this is the equivalent
    /// off-launch read, for a mutation the app UI drives outside the launch
    /// path.
    #[wasm_bindgen(js_name = indexRefusal)]
    pub fn index_refusal(&self) -> JsValue {
        self.registry
            .index_refusal()
            .and_then(|refusal| crate::rpc::to_js(&refusal).ok())
            .unwrap_or(JsValue::UNDEFINED)
    }
}

impl Default for WasmAccountRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    //! Run by `just wasm-test-check` (headless Firefox — `critical_alerts.rs`
    //! holds this crate's one `run_in_browser` configure). The lock's contract
    //! is a browser fact, so this is the one place it can be pinned in Rust;
    //! the two-tab witness is `test_account_registry_mutation_lock_web.py`.

    use std::cell::Cell;
    use std::rc::Rc;

    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::{JsFuture, spawn_local};
    use wasm_bindgen_test::wasm_bindgen_test;

    use fauna_client_accounts::{WEB_MUTATION_LOCK_NAME, with_web_mutation_lock};

    /// One macrotask turn: lets a queued lock grant (or anything else the
    /// browser scheduled) run before the test looks again.
    async fn yield_turn() {
        let window = web_sys::window().unwrap();
        let turn = js_sys::Promise::new(&mut |resolve, _reject| {
            window
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 10)
                .unwrap();
        });
        let _ = JsFuture::from(turn).await;
    }

    /// A mutation queues behind a held registry lock and runs once the holder
    /// lets go — the wrapper asks for the lock at all, and does not run its
    /// section unguarded while another request holds the name.
    #[wasm_bindgen_test]
    async fn a_mutation_queues_behind_a_held_registry_lock_and_runs_on_release() {
        // `navigator.locks` by property lookup, as the wrapper itself reaches
        // it (`web_sys`'s Web Locks types are behind its unstable-APIs cfg).
        let window = web_sys::window().unwrap();
        let navigator = js_sys::Reflect::get(&window, &"navigator".into()).unwrap();
        let locks = js_sys::Reflect::get(&navigator, &"locks".into()).unwrap();
        let request: js_sys::Function = js_sys::Reflect::get(&locks, &"request".into())
            .unwrap()
            .dyn_into()
            .unwrap();

        // Hold the lock: a request whose callback stays pending until `release`.
        let mut release: Option<js_sys::Function> = None;
        let held_until_released = js_sys::Promise::new(&mut |resolve, _reject| {
            release = Some(resolve);
        });
        let release = release.expect("the promise executor runs synchronously");
        let holding = Rc::new(Cell::new(false));
        let holding_seen = Rc::clone(&holding);
        let hold = Closure::once(move |_lock: JsValue| {
            holding_seen.set(true);
            JsValue::from(held_until_released)
        });
        let hold_request = js_sys::Promise::from(
            request
                .call2(
                    &locks,
                    &JsValue::from_str(WEB_MUTATION_LOCK_NAME),
                    hold.as_ref(),
                )
                .unwrap(),
        );
        while !holding.get() {
            yield_turn().await;
        }

        // A mutation now queues.
        let ran = Rc::new(Cell::new(false));
        let ran_by_section = Rc::clone(&ran);
        let done = Rc::new(Cell::new(false));
        let done_by_task = Rc::clone(&done);
        spawn_local(async move {
            with_web_mutation_lock(move || ran_by_section.set(true)).await;
            done_by_task.set(true);
        });
        for _ in 0..5 {
            yield_turn().await;
        }
        assert!(
            !ran.get(),
            "the section ran while another request held the registry lock"
        );

        // Release → the queued section runs and the wrapper resolves.
        let _ = release.call0(&JsValue::UNDEFINED);
        let _ = JsFuture::from(hold_request).await;
        drop(hold);
        while !done.get() {
            yield_turn().await;
        }
        assert!(ran.get(), "the queued section never ran after the release");
    }
}
