//! The sandbox: the engine configuration, the store limits, the host-side
//! implementation of every import, and the instance an embedder drives.

use std::sync::Arc;

use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, ResourceLimiter, Store, Trap};

use crate::services::{HostServices, HttpRequest, LogLevel, OutboundPolicy, RpcRefusal};
use crate::{
    PLUGIN_FUEL_PER_CALL, PLUGIN_FUEL_YIELD_INTERVAL, PLUGIN_LOG_LINE_MAX_BYTES,
    PLUGIN_MAX_MEMORY_BYTES, PLUGIN_MAX_TABLE_ELEMENTS, PLUGIN_MODULE_MAX_BYTES,
};

/// The generated bindings for the `fauna:plugin` world (`wit/plugin.wit`).
/// Public so an embedder can name the lifted types; the sandbox itself is
/// driven through [`PluginInstance`].
pub mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "plugin",
        imports: { default: async | trappable },
        exports: { default: async },
    });
}

use bindings::Plugin;
use bindings::exports::fauna::plugin::ingress;
use bindings::fauna::plugin::{clock, holder, http, log, nest_api, state, types};

/// One engine per process: the compiler configuration every plugin shares.
pub struct PluginEngine {
    engine: Engine,
}

impl PluginEngine {
    /// Fuel metering on, the component model on, async host imports on —
    /// and nothing else (no cache, no profiling). Each store then yields to
    /// the executor every [`PLUGIN_FUEL_YIELD_INTERVAL`] units of fuel.
    pub fn new() -> anyhow::Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true).wasm_component_model(true);
        Ok(Self {
            engine: Engine::new(&config)?,
        })
    }

    /// Compile a component. Refuses a binary over [`PLUGIN_MODULE_MAX_BYTES`]
    /// before the compiler sees it.
    pub fn compile(&self, bytes: &[u8]) -> anyhow::Result<CompiledPlugin> {
        anyhow::ensure!(
            bytes.len() <= PLUGIN_MODULE_MAX_BYTES,
            "plugin component is {} bytes, over the {PLUGIN_MODULE_MAX_BYTES}-byte ceiling",
            bytes.len()
        );
        Ok(CompiledPlugin {
            component: Component::new(&self.engine, bytes)?,
        })
    }
}

/// A compiled component, instantiable any number of times.
pub struct CompiledPlugin {
    component: Component,
}

/// Why a call into the plugin did not complete.
#[derive(Debug, thiserror::Error)]
pub enum PluginFault {
    /// The call spent its fuel budget ([`PLUGIN_FUEL_PER_CALL`]) — a runaway
    /// plugin, terminated deterministically. The instance is poisoned.
    #[error("the plugin exhausted its fuel budget")]
    OutOfFuel,
    /// The plugin trapped (an out-of-bounds access, an unreachable, a stack
    /// overflow). The instance is poisoned.
    #[error("the plugin trapped: {0}")]
    Trap(String),
    /// A host import failed (the embedder's state store errored). The
    /// instance is poisoned, since the plugin saw a call it made fail
    /// mid-way.
    #[error("a host import failed: {0}")]
    Host(String),
    /// A call after a fault: the instance must be re-created.
    #[error("the plugin instance is poisoned by an earlier fault")]
    Poisoned,
}

/// What the host holds per instance behind the imports.
struct State {
    services: Arc<dyn HostServices>,
    policy: OutboundPolicy,
    limits: InstanceLimits,
}

/// The store limiter: the caps are the INSTANCE's, summed over every linear
/// memory and every table its core instances create, so a component of
/// several memories or tables holds no more than one of each could
/// ([`PLUGIN_MAX_MEMORY_BYTES`], [`PLUGIN_MAX_TABLE_ELEMENTS`]). A refused
/// growth is a failed `memory.grow` / `table.grow` the plugin observes, never
/// a fault; a refused initial size fails the instantiation.
#[derive(Debug, Default)]
struct InstanceLimits {
    /// Bytes held by every linear memory of the instance.
    memory_bytes: usize,
    /// Elements held by every table of the instance.
    table_elements: usize,
}

impl InstanceLimits {
    /// Admit one growth of a resource from `current` to `desired` if the
    /// instance's running `total` stays within `cap`, moving the total.
    fn admit(total: &mut usize, current: usize, desired: usize, cap: usize) -> bool {
        let next = total.saturating_sub(current).saturating_add(desired);
        if next > cap {
            return false;
        }
        *total = next;
        true
    }
}

impl ResourceLimiter for InstanceLimits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(Self::admit(
            &mut self.memory_bytes,
            current,
            desired,
            PLUGIN_MAX_MEMORY_BYTES,
        ))
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(Self::admit(
            &mut self.table_elements,
            current,
            desired,
            PLUGIN_MAX_TABLE_ELEMENTS,
        ))
    }

    // A component is several core instances (the module, the canonical-ABI
    // shims), so the counts bound a small component, not one instance; the
    // byte and element caps above are what bound its size.
    fn instances(&self) -> usize {
        8
    }

    fn tables(&self) -> usize {
        8
    }

    fn memories(&self) -> usize {
        4
    }
}

/// A plugin's log line, cut to [`PLUGIN_LOG_LINE_MAX_BYTES`] at a character
/// boundary and marked, so one call cannot fill the nest's log.
fn bounded_log_line(mut message: String) -> String {
    const MARK: &str = " [truncated]";
    if message.len() <= PLUGIN_LOG_LINE_MAX_BYTES {
        return message;
    }
    let mut cut = PLUGIN_LOG_LINE_MAX_BYTES - MARK.len();
    while !message.is_char_boundary(cut) {
        cut -= 1;
    }
    message.truncate(cut);
    message.push_str(MARK);
    message
}

impl types::Host for State {}

impl nest_api::Host for State {
    async fn call(
        &mut self,
        account: Option<Vec<u8>>,
        kind: String,
        payload: Vec<u8>,
    ) -> wasmtime::Result<Result<Vec<u8>, types::RpcError>> {
        let account = match account {
            None => None,
            Some(bytes) => match <[u8; 32]>::try_from(bytes.as_slice()) {
                Ok(a) => Some(a),
                Err(_) => {
                    return Ok(Err(types::RpcError {
                        code: "invalid_request".into(),
                        message: "account must be a 32-byte actor id".into(),
                    }));
                }
            },
        };
        Ok(self
            .services
            .nest_call(account, kind, payload)
            .await
            .map_err(|RpcRefusal { code, message }| types::RpcError { code, message }))
    }

    async fn bindings(&mut self) -> wasmtime::Result<Vec<Vec<u8>>> {
        Ok(self
            .services
            .bindings()
            .await
            .into_iter()
            .map(|a| a.to_vec())
            .collect())
    }
}

impl state::Host for State {
    async fn get(&mut self, key: String) -> wasmtime::Result<Option<Vec<u8>>> {
        self.services.state_get(key).await.map_err(host_err)
    }

    async fn put(&mut self, key: String, value: Vec<u8>) -> wasmtime::Result<Result<(), String>> {
        self.services.state_put(key, value).await.map_err(host_err)
    }

    async fn delete(&mut self, key: String) -> wasmtime::Result<Result<(), String>> {
        self.services.state_delete(key).await.map_err(host_err)
    }
}

impl holder::Host for State {
    async fn public_key(&mut self) -> wasmtime::Result<Vec<u8>> {
        Ok(self.services.holder_key().public().to_vec())
    }

    async fn open_grant(
        &mut self,
        owner: Vec<u8>,
        wrapped: Vec<u8>,
    ) -> wasmtime::Result<Result<Vec<u8>, String>> {
        Ok(self.services.holder_key().open_grant(&owner, &wrapped))
    }
}

impl http::Host for State {
    async fn fetch(
        &mut self,
        req: http::Request,
    ) -> wasmtime::Result<Result<http::Response, String>> {
        // The policy decides BEFORE any dial: an undeclared host is refused
        // here, and the embedder's fetcher never sees the request.
        if let Err(why) = self.policy.allows(&req.url) {
            self.services
                .log(LogLevel::Warn, format!("outbound fetch refused: {why}"));
            return Ok(Err(why));
        }
        let reply = self
            .services
            .http_fetch(HttpRequest {
                method: req.method,
                url: req.url,
                headers: req.headers,
                body: req.body,
            })
            .await;
        Ok(reply.map(|r| http::Response {
            status: r.status,
            headers: r.headers,
            body: r.body,
        }))
    }
}

impl clock::Host for State {
    async fn now_millis(&mut self) -> wasmtime::Result<u64> {
        Ok(self.services.now_millis())
    }
}

impl log::Host for State {
    async fn log(&mut self, level: log::Level, message: String) -> wasmtime::Result<()> {
        let level = match level {
            log::Level::Debug => LogLevel::Debug,
            log::Level::Info => LogLevel::Info,
            log::Level::Warn => LogLevel::Warn,
            log::Level::Error => LogLevel::Error,
        };
        self.services.log(level, bounded_log_line(message));
        Ok(())
    }
}

/// One proxied ingress request (the WIT `ingress.request`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngressRequest {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The nest-signed identity assertion: the authenticated actor, if any.
    pub actor: Option<[u8; 32]>,
}

/// The plugin's reply (the WIT `ingress.response`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngressResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// One running plugin: a store under the caps, with the component
/// instantiated against the embedder's [`HostServices`].
pub struct PluginInstance {
    store: Store<State>,
    plugin: Plugin,
    poisoned: bool,
}

impl PluginInstance {
    /// Instantiate `compiled` with `services` behind its imports and `policy`
    /// over its outbound fetches. The component's own start function, if any,
    /// runs under one fuel budget.
    pub async fn instantiate(
        engine: &PluginEngine,
        compiled: &CompiledPlugin,
        services: Arc<dyn HostServices>,
        policy: OutboundPolicy,
    ) -> anyhow::Result<Self> {
        let mut linker = Linker::<State>::new(&engine.engine);
        Plugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        let limits = InstanceLimits::default();
        let mut store = Store::new(
            &engine.engine,
            State {
                services,
                policy,
                limits,
            },
        );
        store.limiter(|s| &mut s.limits);
        store.set_fuel(PLUGIN_FUEL_PER_CALL)?;
        // Every call is cooperative: it yields its worker at this interval
        // instead of holding it for the whole budget.
        store.fuel_async_yield_interval(Some(PLUGIN_FUEL_YIELD_INTERVAL))?;
        let plugin = Plugin::instantiate_async(&mut store, &compiled.component, &linker).await?;
        Ok(Self {
            store,
            plugin,
            poisoned: false,
        })
    }

    /// `lifecycle.start`. The outer error is a fault; the inner `Err` is the
    /// plugin's own refusal to start, shown to the admin.
    pub async fn start(&mut self) -> Result<Result<(), String>, PluginFault> {
        self.budget()?;
        let r = self
            .plugin
            .fauna_plugin_lifecycle()
            .call_start(&mut self.store)
            .await;
        self.settle(r)
    }

    /// `lifecycle.stop`, best-effort.
    pub async fn stop(&mut self) -> Result<(), PluginFault> {
        self.budget()?;
        let r = self
            .plugin
            .fauna_plugin_lifecycle()
            .call_stop(&mut self.store)
            .await;
        self.settle(r)
    }

    /// `ingress.handle` for one proxied request.
    pub async fn handle_ingress(
        &mut self,
        req: IngressRequest,
    ) -> Result<IngressResponse, PluginFault> {
        self.budget()?;
        let lifted = ingress::Request {
            method: req.method,
            path: req.path,
            query: req.query,
            headers: req.headers,
            body: req.body,
            actor: req.actor.map(|a| a.to_vec()),
        };
        let r = self
            .plugin
            .fauna_plugin_ingress()
            .call_handle(&mut self.store, &lifted)
            .await;
        self.settle(r).map(|r| IngressResponse {
            status: r.status,
            headers: r.headers,
            body: r.body,
        })
    }

    /// Fuel left from the last call's budget — a test's observable.
    pub fn fuel_remaining(&self) -> u64 {
        self.store.get_fuel().unwrap_or(0)
    }

    /// Has a fault ended this instance?
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    fn budget(&mut self) -> Result<(), PluginFault> {
        if self.poisoned {
            return Err(PluginFault::Poisoned);
        }
        self.store
            .set_fuel(PLUGIN_FUEL_PER_CALL)
            .map_err(|e| PluginFault::Host(e.to_string()))
    }

    fn settle<T>(&mut self, r: wasmtime::Result<T>) -> Result<T, PluginFault> {
        match r {
            Ok(v) => Ok(v),
            Err(e) => {
                self.poisoned = true;
                Err(classify(e))
            }
        }
    }
}

fn classify(e: wasmtime::Error) -> PluginFault {
    match e.downcast_ref::<Trap>() {
        Some(Trap::OutOfFuel) => PluginFault::OutOfFuel,
        Some(trap) => PluginFault::Trap(trap.to_string()),
        None => PluginFault::Host(format!("{e:#}")),
    }
}

/// An embedder's error, lifted into the sandbox's own error type: the call
/// faults ([`PluginFault::Host`]), with the embedder's message intact.
fn host_err(e: anyhow::Error) -> wasmtime::Error {
    wasmtime::Error::msg(format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The memory cap is the instance's: two memories share it.
    #[test]
    fn memories_share_one_instance_cap() {
        let mut l = InstanceLimits::default();
        let half = PLUGIN_MAX_MEMORY_BYTES / 2;
        assert!(l.memory_growing(0, half, None).unwrap());
        assert!(
            l.memory_growing(0, half, None).unwrap(),
            "a second memory up to the cap"
        );
        assert!(
            !l.memory_growing(0, 64 * 1024, None).unwrap(),
            "a third memory past the instance's cap"
        );
        assert!(
            !l.memory_growing(half, half + 64 * 1024, None).unwrap(),
            "growing one past the instance's cap"
        );
        assert_eq!(l.memory_bytes, PLUGIN_MAX_MEMORY_BYTES);
    }

    #[test]
    fn tables_share_one_element_cap() {
        let mut l = InstanceLimits::default();
        assert!(
            l.table_growing(0, PLUGIN_MAX_TABLE_ELEMENTS - 1, None)
                .unwrap()
        );
        assert!(l.table_growing(0, 1, None).unwrap());
        assert!(!l.table_growing(0, 1, None).unwrap());
        assert!(!l.table_growing(1, 2, None).unwrap());
    }

    #[test]
    fn a_long_log_line_is_cut_at_a_character_boundary() {
        assert_eq!(bounded_log_line("short".into()), "short");
        let cut = bounded_log_line("é".repeat(PLUGIN_LOG_LINE_MAX_BYTES));
        assert!(cut.len() <= PLUGIN_LOG_LINE_MAX_BYTES, "{}", cut.len());
        assert!(cut.ends_with(" [truncated]"));
        assert!(cut.starts_with('é'));
    }
}
