//! The nest's side of the WASM plugin runner (`docs/goal/architecture/third-party.md`
//! § Execution forms → *WASM components* → *The contract as built*; § The
//! principal model → *Hosted principals*): the registry of running plugins, the
//! supervisor task each one runs in, and [`HostServices`] over the nest — every
//! import a plugin makes served under its principal rows.
//!
//! **One task per plugin** owns its [`PluginInstance`] and serves a command
//! channel (ingress, stop). Every export call runs under a wall-clock bound
//! ([`PLUGIN_CALL_MAX_WALL`] — fuel is not spent while the host awaits an
//! import, so fuel alone cannot end a call parked on a slow fetch) and under
//! the plugin's cancel token: uninstall and the developer reinstall trip it
//! before they ask for the best-effort `stop`, so a call still in flight ends
//! there and writes nothing more under a principal that is going away. A [`PluginFault`] ends the instance; the task
//! counts it, waits a backoff ([`FAULT_BACKOFF_START`] doubling to
//! [`FAULT_BACKOFF_MAX`]) and re-instantiates — `fauna.plugins.list` shows the
//! count and the last fault so the ADMIN decides whether to uninstall. A
//! `start` that answers `Err(msg)` is the plugin's own refusal: it stops and
//! shows `msg`, and is not retried.
//!
//! **Reach is the rows'.** `nest-api.call` for a bound account dispatches
//! through [`dispatch_principal`] as that account's binding row (its
//! `granted_scopes` the token scopes, so the row alone decides reach); with no
//! account it runs as the install row ([`dispatch_hosted_install`]). The
//! outbound fetch goes through the nest's one guarded fetcher
//! ([`crate::oauth_as_client::ClientMetadataFetcher::request`]) after the
//! sandbox's declared-host policy admitted the URL. The holder secret is read
//! from `plugins/<principal_id>/holder.key` once, at start, and never lowered
//! into the plugin.
//!
//! **On disk**, per plugin: `plugins/<principal_id hex>/plugin.wasm` (the
//! module the install verified) and `holder.key` (mode 0600). A plugin whose
//! files are missing, whose module no longer matches its pinned digest, or
//! that no longer compiles is listed `stopped` with the reason at boot — never
//! a crash of the nest; the admin's recovery is uninstall.
//!
//! The install leg's own memory lives here too: a verified, compiled module
//! waiting on its consent card ([`PendingInstall`]), held in memory for the
//! card's life so approval needs no second fetch.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use fauna_plugin_host::{
    BoxFut, CompiledPlugin, HolderKey, HostServices, HttpRequest, HttpResponse, IngressRequest,
    IngressResponse, LogLevel, OutboundPolicy, PluginEngine, PluginFault, PluginInstance,
    RpcRefusal,
};
use fauna_protocol::atproto_pds::ConsentInstallInfo;
use fauna_protocol::kind_manifest::IngressEntry;
use fauna_protocol::plugins::{
    PLUGIN_STATE_RESTARTING, PLUGIN_STATE_RUNNING, PLUGIN_STATE_STOPPED, PluginStatus,
};
use sha2::Digest as _;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::db::CacheDb;
use crate::db::third_party_principals::{HostedPluginMint, HostedPluginRow};
use crate::principal_handlers::{PrincipalBinding, dispatch_hosted_install, dispatch_principal};
use crate::routes::AppState;

/// The first wait before re-instantiating a faulted plugin.
pub const FAULT_BACKOFF_START: Duration = Duration::from_secs(1);
/// The longest wait between re-instantiations — a plugin faulting forever
/// costs one instantiation per five minutes until the admin uninstalls it.
pub const FAULT_BACKOFF_MAX: Duration = Duration::from_secs(5 * 60);
/// Cap on one outbound response body a plugin's `http.fetch` reads — it is
/// lowered into the plugin's memory, whose ceiling is
/// [`fauna_plugin_host::PLUGIN_MAX_MEMORY_BYTES`].
pub const PLUGIN_HTTP_MAX_BYTES: usize = 4 * 1024 * 1024;
/// How long uninstall waits for a plugin's best-effort `stop` before it drops
/// the task — and the bound on that `stop` call itself.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// The wall-clock bound on one export call (`start`, one ingress request),
/// however its time is spent: a call past it ends as a fault, the instance is
/// dropped and re-instantiated like any other. Generous beside the call's
/// fuel (about a second of CPU), since a call may await several outbound
/// fetches.
pub const PLUGIN_CALL_MAX_WALL: Duration = Duration::from_secs(30);
/// The module file under a plugin's directory.
pub const MODULE_FILE: &str = "plugin.wasm";
/// The holder secret under a plugin's directory (mode 0600).
pub const HOLDER_FILE: &str = "holder.key";
/// A development reinstall's new module, written beside [`MODULE_FILE`]
/// before the rows change and renamed over it after.
const STAGED_MODULE_FILE: &str = "plugin.wasm.new";

/// The plugin's holder key, from its directory's [`HOLDER_FILE`].
fn read_holder(dir: &std::path::Path) -> Result<HolderKey, String> {
    let secret: [u8; 32] = std::fs::read(dir.join(HOLDER_FILE))
        .map_err(|e| format!("holder key missing: {e}"))?
        .try_into()
        .map_err(|_| "holder key file is not 32 bytes".to_string())?;
    Ok(HolderKey::from_secret(secret))
}
/// The most install cards one nest holds open at once — each holds a module
/// of up to [`fauna_plugin_host::PLUGIN_MODULE_MAX_BYTES`] in memory; the
/// consent table's own per-bucket ceiling.
const MAX_PENDING_INSTALLS: usize = crate::db::atproto_pds::MAX_PENDING_CONSENTS_PER_BUCKET;

/// A verified, compiled module waiting on its install card — what approval
/// mints and runs without a second fetch.
pub struct PendingInstall {
    /// When the card expires (unix ms) — the consent row's own expiry.
    pub expires_at: i64,
    /// The admin the card is assigned to.
    pub installed_by: [u8; 32],
    /// What the mint records.
    pub mint: PendingMint,
    /// The verified module bytes.
    pub module: Vec<u8>,
    /// The module, compiled once at install so a malformed component refused
    /// before any card.
    pub compiled: Arc<CompiledPlugin>,
    /// The card's install section.
    pub info: ConsentInstallInfo,
}

/// The [`HostedPluginMint`] fields the install verified — everything but the
/// holder key and the approving admin, which approval supplies.
pub struct PendingMint {
    pub client_id: String,
    pub label: Option<String>,
    /// `None` for a development plugin, whose manifest is unsigned.
    pub publisher_key: Option<[u8; 32]>,
    pub declared_kinds: Vec<String>,
    pub requested_scopes: String,
    pub module_digest: String,
    pub hosts: Vec<String>,
    pub ingress: serde_json::Value,
    pub settings_schema: Option<serde_json::Value>,
    /// Empty for a development plugin, whose manifest is unsigned.
    pub manifest_jws: String,
}

/// Why an approved install did not complete.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// The card's verified module is no longer held (the nest restarted, or
    /// the card expired) — the admin runs the install again.
    #[error("the install card's verified module is no longer held — install again")]
    Expired,
    /// This nest has no data directory to keep a module in.
    #[error("this nest has no data directory for plugins")]
    NoDataDir,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

enum Command {
    Ingress(
        IngressRequest,
        oneshot::Sender<Result<IngressResponse, String>>,
    ),
    Stop(oneshot::Sender<()>),
}

#[derive(Debug, Clone)]
struct Status {
    state: &'static str,
    faults: u32,
    last_fault: Option<String>,
    reason: Option<String>,
}

impl Status {
    fn stopped(reason: String) -> Self {
        Self {
            state: PLUGIN_STATE_STOPPED,
            faults: 0,
            last_fault: None,
            reason: Some(reason),
        }
    }
}

struct Entry {
    /// `None` for a plugin listed `stopped` that has no task (its files are
    /// missing or its module does not compile).
    tx: Option<mpsc::Sender<Command>>,
    /// Tripped by [`PluginRunner::halt`]: ends a call in flight.
    cancel: CancellationToken,
    status: Arc<Mutex<Status>>,
    /// The path prefixes and whole-name labels its document declared
    /// (`ingress`), which [`PluginRunner::route`] and
    /// [`PluginRunner::route_host`] match — kept for a stopped plugin too, so
    /// its paths and names answer 502 rather than falling through to the
    /// nest.
    ingress: Vec<IngressEntry>,
}

/// The entries a stored `ingress` member declares. The member was validated
/// at resolution (`fauna_protocol::kind_manifest::parse_ingress`), so a
/// refusal here is a row no install could have written: logged, and the
/// plugin routes nothing.
pub(crate) fn parsed_ingress(ingress: &serde_json::Value) -> Vec<IngressEntry> {
    match fauna_protocol::kind_manifest::parse_ingress(ingress) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("plugin ingress member refused, nothing routed: {e}");
            Vec::new()
        }
    }
}

/// The path prefixes a stored `ingress` member declares.
pub(crate) fn ingress_prefixes(ingress: &serde_json::Value) -> Vec<String> {
    parsed_ingress(ingress)
        .iter()
        .filter_map(|e| e.path().map(str::to_string))
        .collect()
}

/// The whole-name labels a stored `ingress` member declares.
pub(crate) fn ingress_labels(ingress: &serde_json::Value) -> Vec<String> {
    parsed_ingress(ingress)
        .iter()
        .filter_map(|e| e.name().map(str::to_string))
        .collect()
}

/// Every whole-name label an installed plugin claims, read from the table
/// that is the authority (`hosted_plugins`), so the callers that must agree
/// with the runner's routing — the subdomain opt-in, the custom-domain
/// registration, the boot seed of the host resolver, the per-name cert
/// reconcile and the DNS matrix — read one source and never the runner's
/// in-memory mirror, which boot fills after some of them have run.
pub(crate) async fn claimed_plugin_labels(db: &CacheDb) -> anyhow::Result<Vec<String>> {
    let mut labels: Vec<String> = db
        .list_hosted_plugins()
        .await?
        .iter()
        .flat_map(|p| ingress_labels(&p.ingress))
        .collect();
    labels.sort();
    labels.dedup();
    Ok(labels)
}

/// The registry on [`AppState`]: one engine, the running plugins, the
/// install cards awaiting approval.
pub struct PluginRunner {
    plugins_dir: Option<PathBuf>,
    engine: OnceLock<Result<Arc<PluginEngine>, String>>,
    entries: Mutex<HashMap<Vec<u8>, Entry>>,
    pending: Mutex<HashMap<Vec<u8>, PendingInstall>>,
}

impl std::fmt::Debug for PluginRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginRunner")
            .field("plugins_dir", &self.plugins_dir)
            .finish_non_exhaustive()
    }
}

impl PluginRunner {
    /// A runner keeping plugins under `plugins_dir` (`<data dir>/plugins`);
    /// `None` on a nest without a data directory, which can list but never
    /// install.
    pub fn new(plugins_dir: Option<PathBuf>) -> Self {
        Self {
            plugins_dir,
            engine: OnceLock::new(),
            entries: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// The process's one plugin engine, built on first use.
    pub fn engine(&self) -> anyhow::Result<Arc<PluginEngine>> {
        self.engine
            .get_or_init(|| {
                PluginEngine::new()
                    .map(Arc::new)
                    .map_err(|e| format!("{e:#}"))
            })
            .clone()
            .map_err(anyhow::Error::msg)
    }

    /// Compile `bytes` off the async workers — a component of up to 16 MiB
    /// is real compiler work.
    pub async fn compile(&self, bytes: Vec<u8>) -> anyhow::Result<Arc<CompiledPlugin>> {
        let engine = self.engine()?;
        tokio::task::spawn_blocking(move || engine.compile(&bytes).map(Arc::new))
            .await
            .map_err(|e| anyhow::anyhow!("compile task failed: {e}"))?
    }

    fn plugin_dir(&self, principal_id: &[u8]) -> Option<PathBuf> {
        // An empty id would join to the plugins root itself: never name it.
        if principal_id.is_empty() {
            return None;
        }
        self.plugins_dir
            .as_ref()
            .map(|d| d.join(hex::encode(principal_id)))
    }

    // ── The install cards ────────────────────────────────────────────────

    /// Hold `install` for the card `consent_id` opened. Expired cards are
    /// swept first, then the oldest is evicted past the ceiling — the
    /// consent table's own direction, so a stale card never blocks a fresh
    /// one.
    pub fn hold_pending(&self, consent_id: Vec<u8>, install: PendingInstall) {
        let now = crate::db::now_epoch_millis();
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        pending.retain(|_, p| p.expires_at > now);
        while pending.len() >= MAX_PENDING_INSTALLS {
            let Some(oldest) = pending
                .iter()
                .min_by_key(|(_, p)| p.expires_at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            pending.remove(&oldest);
        }
        pending.insert(consent_id, install);
    }

    /// The install section of a pending card, for the card's projection.
    pub fn pending_info(&self, consent_id: &[u8]) -> Option<ConsentInstallInfo> {
        let pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        pending.get(consent_id).map(|p| p.info.clone())
    }

    /// Is `consent_id` an install card this nest holds?
    pub fn is_pending(&self, consent_id: &[u8]) -> bool {
        let pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        pending.contains_key(consent_id)
    }

    /// Take the card's verified install — the approval's or the decline's
    /// act; a decline simply drops it.
    pub fn take_pending(&self, consent_id: &[u8]) -> Option<PendingInstall> {
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        pending.remove(consent_id)
    }

    /// Complete an APPROVED install: mint the holder key, mint the install
    /// row, write the module and the holder secret, start the runner.
    /// Returns the install row's `principal_id`.
    ///
    /// The row is minted before the files, so a crash between the two leaves
    /// a row the boot walk lists `stopped` ("module file missing") — visible
    /// and uninstallable — never files no row names. A write that fails here
    /// rolls the row back.
    pub async fn complete_install(
        &self,
        state: &Arc<AppState>,
        install: PendingInstall,
    ) -> Result<Vec<u8>, InstallError> {
        if install.expires_at <= crate::db::now_epoch_millis() {
            return Err(InstallError::Expired);
        }
        if self.plugins_dir.is_none() {
            return Err(InstallError::NoDataDir);
        }
        if fauna_protocol::plugins::is_development_client_id(&install.mint.client_id)
            && let Some(existing) = state.db.get_hosted_plugin(&install.mint.client_id).await?
        {
            return self.replace_development(state, existing, install).await;
        }
        let holder = HolderKey::mint();
        let m = install.mint;
        let ingress = parsed_ingress(&m.ingress);
        let principal_id = state
            .db
            .mint_hosted_plugin(&HostedPluginMint {
                client_id: m.client_id.clone(),
                label: m.label,
                holder_x25519: *holder.public(),
                publisher_key: m.publisher_key,
                declared_kinds: m.declared_kinds,
                requested_scopes: m.requested_scopes,
                module_digest: m.module_digest,
                hosts: m.hosts.clone(),
                ingress: m.ingress,
                settings_schema: m.settings_schema,
                manifest_jws: (!m.manifest_jws.is_empty()).then_some(m.manifest_jws),
                installed_by: install.installed_by,
            })
            .await?;
        let dir = self
            .plugin_dir(&principal_id)
            .ok_or(InstallError::NoDataDir)?;
        let written = (|| -> anyhow::Result<()> {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join(MODULE_FILE), &install.module)?;
            crate::deployment_key::write_secret_file_0600(
                &dir.join(HOLDER_FILE),
                holder.secret_for_storage(),
            )?;
            Ok(())
        })();
        if let Err(e) = written {
            let _ = state.db.uninstall_hosted_plugin(&principal_id).await;
            let _ = std::fs::remove_dir_all(&dir);
            return Err(InstallError::Other(e.context("write the plugin's files")));
        }
        self.start(
            state,
            principal_id.clone(),
            m.client_id,
            &m.hosts,
            ingress,
            install.compiled,
            holder,
        );
        Ok(principal_id)
    }

    /// The developer loop's reinstall (`third-party.md` § Developer loop):
    /// an approved card for a development plugin already installed replaces
    /// it in place — stop, swap the module, start — under the same install
    /// row, so the principal id, the holder key, every user's binding and the
    /// plugin's state scope all survive the edit-run cycle. Only the
    /// development form reaches here; a published document stays installed
    /// until it is uninstalled.
    ///
    /// The new module is written beside the old before the rows change and
    /// renamed over it after, so a crash in between leaves either the old
    /// plugin whole or a row whose digest the boot walk finds off its file —
    /// listed `stopped`, and fixed by installing again.
    async fn replace_development(
        &self,
        state: &Arc<AppState>,
        existing: HostedPluginRow,
        install: PendingInstall,
    ) -> Result<Vec<u8>, InstallError> {
        let principal_id = existing.principal_id;
        let dir = self
            .plugin_dir(&principal_id)
            .ok_or(InstallError::NoDataDir)?;
        let holder = read_holder(&dir).map_err(|e| InstallError::Other(anyhow::anyhow!(e)))?;
        let staged = dir.join(STAGED_MODULE_FILE);
        std::fs::write(&staged, &install.module)
            .map_err(|e| InstallError::Other(anyhow::Error::from(e).context("stage the module")))?;
        self.halt(&principal_id).await;
        let m = install.mint;
        let client_id = m.client_id.clone();
        let hosts = m.hosts.clone();
        let ingress = parsed_ingress(&m.ingress);
        let swapped = async {
            state
                .db
                .replace_hosted_plugin(
                    &principal_id,
                    &HostedPluginMint {
                        client_id: m.client_id,
                        label: m.label,
                        holder_x25519: *holder.public(),
                        publisher_key: m.publisher_key,
                        declared_kinds: m.declared_kinds,
                        requested_scopes: m.requested_scopes,
                        module_digest: m.module_digest,
                        hosts: m.hosts,
                        ingress: m.ingress,
                        settings_schema: m.settings_schema,
                        manifest_jws: (!m.manifest_jws.is_empty()).then_some(m.manifest_jws),
                        installed_by: install.installed_by,
                    },
                )
                .await?;
            std::fs::rename(&staged, dir.join(MODULE_FILE)).map_err(anyhow::Error::from)
        }
        .await;
        if let Err(e) = swapped {
            let _ = std::fs::remove_file(&staged);
            // Bring back whatever the rows and the files now say — the old
            // plugin whole when the rows did not change, or `stopped` with
            // the boot walk's reason when only the rename failed.
            self.restart(state, &client_id).await;
            return Err(InstallError::Other(
                e.context("replace the development plugin"),
            ));
        }
        self.start(
            state,
            principal_id.clone(),
            client_id,
            &hosts,
            ingress,
            install.compiled,
            holder,
        );
        Ok(principal_id)
    }

    /// Start one installed plugin from its rows and files, as the boot walk
    /// does — `stopped` with the reason when it cannot run.
    async fn restart(&self, state: &Arc<AppState>, client_id: &str) {
        let Ok(Some(plugin)) = state.db.get_hosted_plugin(client_id).await else {
            return;
        };
        let ingress = parsed_ingress(&plugin.ingress);
        match self.load(&plugin).await {
            Ok((compiled, holder)) => self.start(
                state,
                plugin.principal_id.clone(),
                plugin.client_id.clone(),
                &plugin.hosts,
                ingress,
                compiled,
                holder,
            ),
            Err(reason) => self.register_stopped(plugin.principal_id.clone(), ingress, reason),
        }
    }

    // ── Running ──────────────────────────────────────────────────────────

    /// The boot walk: start every installed plugin. A plugin whose files are
    /// missing, whose module no longer hashes to its pinned digest, or that
    /// no longer compiles is listed `stopped` with the reason.
    pub async fn boot(&self, state: &Arc<AppState>) {
        let plugins = match state.db.list_hosted_plugins().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("plugin runner: could not list installed plugins: {e:#}");
                return;
            }
        };
        for plugin in plugins {
            let ingress = parsed_ingress(&plugin.ingress);
            match self.load(&plugin).await {
                Ok((compiled, holder)) => self.start(
                    state,
                    plugin.principal_id.clone(),
                    plugin.client_id.clone(),
                    &plugin.hosts,
                    ingress,
                    compiled,
                    holder,
                ),
                Err(reason) => {
                    tracing::warn!(
                        principal = %hex::encode(&plugin.principal_id),
                        client_id = %plugin.client_id,
                        "plugin not started: {reason}"
                    );
                    self.register_stopped(plugin.principal_id.clone(), ingress, reason);
                }
            }
        }
    }

    async fn load(
        &self,
        plugin: &HostedPluginRow,
    ) -> Result<(Arc<CompiledPlugin>, HolderKey), String> {
        let dir = self
            .plugin_dir(&plugin.principal_id)
            .ok_or_else(|| "this nest has no data directory for plugins".to_string())?;
        let module = std::fs::read(dir.join(MODULE_FILE))
            .map_err(|e| format!("module file missing: {e}"))?;
        let digest = format!("sha256:{}", hex::encode(sha2::Sha256::digest(&module)));
        if digest != plugin.module_digest {
            return Err("module file does not match the pinned digest".into());
        }
        let holder = read_holder(&dir)?;
        let compiled = self
            .compile(module)
            .await
            .map_err(|e| format!("module no longer compiles: {e:#}"))?;
        Ok((compiled, holder))
    }

    fn register_stopped(&self, principal_id: Vec<u8>, ingress: Vec<IngressEntry>, reason: String) {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries.insert(
            principal_id,
            Entry {
                tx: None,
                cancel: CancellationToken::new(),
                status: Arc::new(Mutex::new(Status::stopped(reason))),
                ingress,
            },
        );
    }

    fn start(
        &self,
        state: &Arc<AppState>,
        principal_id: Vec<u8>,
        client_id: String,
        hosts: &[String],
        ingress: Vec<IngressEntry>,
        compiled: Arc<CompiledPlugin>,
        holder: HolderKey,
    ) {
        let engine = match self.engine() {
            Ok(e) => e,
            Err(e) => {
                self.register_stopped(
                    principal_id,
                    ingress,
                    format!("plugin engine unavailable: {e:#}"),
                );
                return;
            }
        };
        let (tx, rx) = mpsc::channel(16);
        let status = Arc::new(Mutex::new(Status {
            state: PLUGIN_STATE_RESTARTING,
            faults: 0,
            last_fault: None,
            reason: None,
        }));
        let services: Arc<dyn HostServices> = Arc::new(NestServices {
            state: state.clone(),
            principal_id: principal_id.clone(),
            client_id,
            holder,
        });
        let cancel = CancellationToken::new();
        let supervisor = Supervisor {
            engine,
            compiled,
            services,
            policy: OutboundPolicy::new(hosts),
            status: status.clone(),
            principal: hex::encode(&principal_id),
            cancel: cancel.clone(),
        };
        {
            let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            entries.insert(
                principal_id,
                Entry {
                    tx: Some(tx),
                    cancel,
                    status,
                    ingress,
                },
            );
        }
        // Scoped to the serving generation: a teardown ends every plugin with
        // it, and the next generation's boot walk starts them again.
        state.spawn_scoped(supervisor.run(rx));
    }

    /// Stop a plugin and forget it — uninstall's act, after the rows are
    /// gone. Waits up to [`STOP_GRACE`] for the plugin's best-effort `stop`,
    /// then deletes its directory.
    pub async fn stop(&self, principal_id: &[u8]) {
        self.halt(principal_id).await;
        if let Some(dir) = self.plugin_dir(principal_id)
            && let Err(e) = std::fs::remove_dir_all(&dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(dir = %dir.display(), "plugin directory not removed: {e}");
        }
    }

    /// End a plugin's task and forget it, keeping its files — [`Self::stop`]'s
    /// first half, and the development reinstall's before it swaps the module.
    /// A call in flight ends at once (the cancel token); an idle plugin gets
    /// its best-effort `stop`, bounded by [`STOP_GRACE`]. Either way the task
    /// is over when this returns, or within that grace.
    async fn halt(&self, principal_id: &[u8]) {
        let entry = {
            let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            entries.remove(principal_id)
        };
        let Some(entry) = entry else { return };
        entry.cancel.cancel();
        if let Some(tx) = entry.tx {
            let (ack, done) = oneshot::channel();
            if tx.send(Command::Stop(ack)).await.is_ok() {
                let _ = tokio::time::timeout(STOP_GRACE, done).await;
            }
        }
    }

    /// The runner's state for one plugin, as `fauna.plugins.list` shows it.
    /// An installed plugin the runner never heard of (a nest without the boot
    /// walk, a test) reads `stopped`.
    pub fn status(&self, principal_id: &[u8]) -> PluginStatus {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let s = entries
            .get(principal_id)
            .map(|e| e.status.lock().unwrap_or_else(|p| p.into_inner()).clone())
            .unwrap_or_else(|| Status::stopped("not started".into()));
        PluginStatus {
            state: s.state.to_string(),
            faults: s.faults,
            last_fault: s.last_fault,
            reason: s.reason,
            extra: Default::default(),
        }
    }

    /// The installed plugin whose declared `ingress` prefix `path` falls
    /// under — the longest prefix when two would match (install refuses an
    /// overlap, so only two cards approved together can produce one).
    pub fn route(&self, path: &str) -> Option<Vec<u8>> {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries
            .iter()
            .flat_map(|(id, e)| {
                e.ingress
                    .iter()
                    .filter_map(IngressEntry::path)
                    .map(move |p| (id, p))
            })
            .filter(|(_, prefix)| path.starts_with(prefix))
            .max_by_key(|(_, prefix)| prefix.len())
            .map(|(id, _)| id.clone())
    }

    /// The installed plugin whose declared whole name is `<label>.<apex>` —
    /// `label` being the one DNS label under the nest's apex the request's
    /// `Host` named (`plugin_ingress::plugin_host_label`). Install refuses a
    /// label another plugin holds, so at most one matches.
    pub fn route_host(&self, label: &str) -> Option<Vec<u8>> {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries
            .iter()
            .find(|(_, e)| {
                e.ingress
                    .iter()
                    .filter_map(IngressEntry::name)
                    .any(|n| n == label)
            })
            .map(|(id, _)| id.clone())
    }

    /// Hand one request to a plugin's `ingress.handle` (the nest-terminated
    /// ingress proxy's door). `Err` carries the fault or why nothing ran.
    pub async fn ingress(
        &self,
        principal_id: &[u8],
        req: IngressRequest,
    ) -> Result<IngressResponse, String> {
        let tx = {
            let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            entries.get(principal_id).and_then(|e| e.tx.clone())
        }
        .ok_or_else(|| "the plugin is not running".to_string())?;
        let (reply, answer) = oneshot::channel();
        tx.send(Command::Ingress(req, reply))
            .await
            .map_err(|_| "the plugin is not running".to_string())?;
        answer
            .await
            .map_err(|_| "the plugin is not running".to_string())?
    }
}

/// One plugin's task: instantiate, start, serve, and on a fault count, back
/// off and re-instantiate.
struct Supervisor {
    engine: Arc<PluginEngine>,
    compiled: Arc<CompiledPlugin>,
    services: Arc<dyn HostServices>,
    policy: OutboundPolicy,
    status: Arc<Mutex<Status>>,
    principal: String,
    /// The entry's cancel token: [`PluginRunner::halt`] trips it.
    cancel: CancellationToken,
}

/// How a served instance ended.
enum Ended {
    Stopped,
    Faulted,
}

/// Why a bounded call did not return a value.
enum CallEnd {
    /// A fault, or the wall-clock bound: the instance is done.
    Fault(String),
    /// The plugin is being halted: the task ends.
    Cancelled,
}

/// Run one export call under the wall-clock bound and the cancel token. A
/// call cut off either way leaves the instance mid-call, so it is never used
/// again.
async fn bounded<T>(
    cancel: &CancellationToken,
    limit: Duration,
    call: impl std::future::Future<Output = Result<T, PluginFault>>,
) -> Result<T, CallEnd> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(CallEnd::Cancelled),
        r = tokio::time::timeout(limit, call) => match r {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(fault)) => Err(CallEnd::Fault(fault.to_string())),
            Err(_) => Err(CallEnd::Fault(format!(
                "the call ran past its {}-second wall-clock bound",
                limit.as_secs()
            ))),
        },
    }
}

impl Supervisor {
    fn set(&self, f: impl FnOnce(&mut Status)) {
        f(&mut self.status.lock().unwrap_or_else(|p| p.into_inner()));
    }

    fn fault(&self, what: String) {
        tracing::warn!(principal = %self.principal, "plugin fault: {what}");
        self.set(|s| {
            s.state = PLUGIN_STATE_RESTARTING;
            s.faults = s.faults.saturating_add(1);
            s.last_fault = Some(what);
        });
    }

    async fn run(self, mut rx: mpsc::Receiver<Command>) {
        let mut backoff = FAULT_BACKOFF_START;
        loop {
            match self.instance_life(&mut rx).await {
                Ended::Stopped => return,
                Ended::Faulted => {}
            }
            // The backoff, answering the channel meanwhile: a stop ends the
            // task, an ingress is refused rather than queued behind a wait
            // of up to five minutes.
            let wait = tokio::time::sleep(backoff);
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    () = &mut wait => break,
                    cmd = rx.recv() => match cmd {
                        None => return,
                        Some(Command::Stop(ack)) => {
                            self.set(|s| s.state = PLUGIN_STATE_STOPPED);
                            let _ = ack.send(());
                            return;
                        }
                        Some(Command::Ingress(_, reply)) => {
                            let _ = reply.send(Err("the plugin is restarting after a fault".into()));
                        }
                    },
                }
            }
            backoff = (backoff * 2).min(FAULT_BACKOFF_MAX);
        }
    }

    /// A halt ended the call in flight: the task ends, listed stopped.
    fn cancelled(&self) -> Ended {
        self.set(|s| s.state = PLUGIN_STATE_STOPPED);
        Ended::Stopped
    }

    /// One instance, from instantiation to a fault or a stop.
    async fn instance_life(&self, rx: &mut mpsc::Receiver<Command>) -> Ended {
        let instantiated = bounded(&self.cancel, PLUGIN_CALL_MAX_WALL, async {
            PluginInstance::instantiate(
                &self.engine,
                &self.compiled,
                self.services.clone(),
                self.policy.clone(),
            )
            .await
            .map_err(|e| PluginFault::Host(format!("instantiation failed: {e:#}")))
        })
        .await;
        let mut instance = match instantiated {
            Ok(i) => i,
            Err(CallEnd::Cancelled) => return self.cancelled(),
            Err(CallEnd::Fault(what)) => {
                self.fault(what);
                return Ended::Faulted;
            }
        };
        match bounded(&self.cancel, PLUGIN_CALL_MAX_WALL, instance.start()).await {
            Ok(Ok(())) => self.set(|s| {
                s.state = PLUGIN_STATE_RUNNING;
                s.reason = None;
            }),
            Ok(Err(msg)) => {
                // The plugin's own refusal: shown, never retried.
                self.set(|s| {
                    s.state = PLUGIN_STATE_STOPPED;
                    s.reason = Some(msg);
                });
                return self.serve_stopped(rx).await;
            }
            Err(CallEnd::Cancelled) => return self.cancelled(),
            Err(CallEnd::Fault(what)) => {
                self.fault(what);
                return Ended::Faulted;
            }
        }
        while let Some(cmd) = rx.recv().await {
            match cmd {
                Command::Ingress(req, reply) => match bounded(
                    &self.cancel,
                    PLUGIN_CALL_MAX_WALL,
                    instance.handle_ingress(req),
                )
                .await
                {
                    Ok(resp) => {
                        let _ = reply.send(Ok(resp));
                    }
                    Err(CallEnd::Cancelled) => {
                        let _ = reply.send(Err("the plugin was stopped".into()));
                        return self.cancelled();
                    }
                    Err(CallEnd::Fault(what)) => {
                        // Counted before the caller hears of it, so whoever
                        // reads the status after the reply sees this fault.
                        self.fault(what.clone());
                        let _ = reply.send(Err(what));
                        return Ended::Faulted;
                    }
                },
                Command::Stop(ack) => {
                    // Best-effort and bounded; the cancel token is already
                    // tripped by the halt that sent this, so it is not
                    // consulted here.
                    match tokio::time::timeout(STOP_GRACE, instance.stop()).await {
                        Ok(Ok(())) => {}
                        Ok(Err(fault)) => tracing::info!(
                            principal = %self.principal,
                            "plugin stop faulted: {fault}"
                        ),
                        Err(_) => tracing::info!(
                            principal = %self.principal,
                            "plugin stop ran past its grace"
                        ),
                    }
                    self.set(|s| s.state = PLUGIN_STATE_STOPPED);
                    let _ = ack.send(());
                    return Ended::Stopped;
                }
            }
        }
        Ended::Stopped
    }

    /// A plugin that refused to start: answer the channel until stopped.
    async fn serve_stopped(&self, rx: &mut mpsc::Receiver<Command>) -> Ended {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                Command::Ingress(_, reply) => {
                    let _ = reply.send(Err("the plugin refused to start".into()));
                }
                Command::Stop(ack) => {
                    let _ = ack.send(());
                    break;
                }
            }
        }
        Ended::Stopped
    }
}

/// [`HostServices`] over the nest, for one installed plugin.
struct NestServices {
    state: Arc<AppState>,
    principal_id: Vec<u8>,
    client_id: String,
    holder: HolderKey,
}

fn refusal(e: fauna_protocol::RpcError) -> RpcRefusal {
    let message = match e.details.as_deref() {
        Some(fauna_protocol::Value::String(why)) => format!("{}: {why}", e.message.key),
        _ => e.message.key.clone(),
    };
    RpcRefusal {
        code: e.code,
        message,
    }
}

impl NestServices {
    async fn call_as_binding(
        &self,
        account: [u8; 32],
        kind: String,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, RpcRefusal> {
        let not_bound = || RpcRefusal {
            code: "fauna.bridges.permission_denied".into(),
            message: "the account has not bound this plugin".into(),
        };
        let internal = |e: anyhow::Error| RpcRefusal {
            code: "fauna.bridges.internal".into(),
            message: format!("{e:#}"),
        };
        let principal_id = self
            .state
            .db
            .get_third_party_principal_id(&account, &self.client_id)
            .await
            .map_err(internal)?
            .ok_or_else(not_bound)?;
        let (granted, _, _) = self
            .state
            .db
            .get_third_party_principal_reach(&account, &principal_id)
            .await
            .map_err(internal)?
            .ok_or_else(not_bound)?;
        // The row alone decides reach: a hosted plugin holds no token, so
        // the binding's granted scopes stand in for a token's.
        let binding = PrincipalBinding {
            account,
            principal_id,
            token_scopes: granted.split_whitespace().map(str::to_string).collect(),
            family_id: None,
        };
        dispatch_principal(self.state.clone(), &binding, &kind, Bytes::from(payload))
            .await
            .map(|b| b.to_vec())
            .map_err(refusal)
    }
}

impl HostServices for NestServices {
    fn nest_call(
        &self,
        account: Option<[u8; 32]>,
        kind: String,
        payload: Vec<u8>,
    ) -> BoxFut<'_, Result<Vec<u8>, RpcRefusal>> {
        Box::pin(async move {
            match account {
                Some(account) => self.call_as_binding(account, kind, payload).await,
                None => dispatch_hosted_install(
                    self.state.clone(),
                    &self.principal_id,
                    &kind,
                    Bytes::from(payload),
                )
                .await
                .map(|b| b.to_vec())
                .map_err(refusal),
            }
        })
    }

    fn bindings(&self) -> BoxFut<'_, Vec<[u8; 32]>> {
        Box::pin(async move {
            match self.state.db.get_hosted_plugin(&self.client_id).await {
                Ok(Some(row)) => row.bound_accounts,
                Ok(None) => Vec::new(),
                Err(e) => {
                    tracing::warn!("plugin bindings unreadable: {e:#}");
                    Vec::new()
                }
            }
        })
    }

    fn state_get(&self, key: String) -> BoxFut<'_, anyhow::Result<Option<Vec<u8>>>> {
        Box::pin(async move {
            self.state
                .db
                .plugin_state_get(&self.principal_id, &key)
                .await
        })
    }

    fn state_put(
        &self,
        key: String,
        value: Vec<u8>,
    ) -> BoxFut<'_, anyhow::Result<Result<(), String>>> {
        Box::pin(async move {
            Ok(self
                .state
                .db
                .plugin_state_put(&self.principal_id, &key, &value)
                .await?
                .map_err(|refused| refused.to_string()))
        })
    }

    fn state_delete(&self, key: String) -> BoxFut<'_, anyhow::Result<Result<(), String>>> {
        Box::pin(async move {
            Ok(self
                .state
                .db
                .plugin_state_delete(&self.principal_id, &key)
                .await?
                .map_err(|refused| refused.to_string()))
        })
    }

    fn holder_key(&self) -> &HolderKey {
        &self.holder
    }

    fn http_fetch(&self, req: HttpRequest) -> BoxFut<'_, Result<HttpResponse, String>> {
        Box::pin(async move {
            self.state
                .oauth_as
                .fetcher
                .request(req, PLUGIN_HTTP_MAX_BYTES)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn now_millis(&self) -> u64 {
        u64::try_from(crate::db::now_epoch_millis()).unwrap_or(0)
    }

    fn log(&self, level: LogLevel, message: String) {
        let principal = hex::encode(&self.principal_id);
        match level {
            LogLevel::Debug => tracing::debug!(target: "plugin", %principal, "{message}"),
            LogLevel::Info => tracing::info!(target: "plugin", %principal, "{message}"),
            LogLevel::Warn => tracing::warn!(target: "plugin", %principal, "{message}"),
            LogLevel::Error => tracing::error!(target: "plugin", %principal, "{message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    ///  A call parked on an await — no fuel spent — ends at the
    /// wall-clock bound as a fault, and at once when the plugin is halted.
    #[tokio::test(start_paused = true)]
    async fn a_parked_call_ends_at_its_bound_or_its_halt() {
        let parked = || async {
            std::future::pending::<()>().await;
            Ok::<(), PluginFault>(())
        };
        let cancel = CancellationToken::new();
        let r = bounded(&cancel, PLUGIN_CALL_MAX_WALL, parked()).await;
        assert!(
            matches!(&r, Err(CallEnd::Fault(w)) if w.contains("wall-clock")),
            "the bound ends a parked call"
        );

        let halted = CancellationToken::new();
        // spawn-ok(test): the parked call, awaited below inside this test.
        let call = tokio::spawn({
            let halted = halted.clone();
            async move { bounded(&halted, PLUGIN_CALL_MAX_WALL, parked()).await }
        });
        tokio::task::yield_now().await;
        halted.cancel();
        assert!(matches!(call.await.unwrap(), Err(CallEnd::Cancelled)));

        let done = bounded(&cancel, PLUGIN_CALL_MAX_WALL, async {
            Ok::<_, PluginFault>(7)
        })
        .await;
        assert!(matches!(done, Ok(7)));
    }

    /// An empty id would name the plugins root itself.
    #[test]
    fn plugin_dir_never_names_the_root() {
        let runner = PluginRunner::new(Some(PathBuf::from("/tmp/plugins-row-3927")));
        assert_eq!(runner.plugin_dir(&[]), None);
        assert_eq!(
            runner.plugin_dir(&[0xAB; 16]),
            Some(PathBuf::from(format!(
                "/tmp/plugins-row-3927/{}",
                "ab".repeat(16)
            )))
        );
    }
}
