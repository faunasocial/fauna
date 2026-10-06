use anyhow::{Context, Result};
use fauna_core::folder_keys::FolderRef;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum LocationMode {
    #[default]
    Always,
    OnDemand,
    /// A mode a newer agent wrote into `config.toml` that this build cannot
    /// name — the open arm of `transport.md` § Rule 3 in full (*Open,
    /// carrying*). The file is rewritten whole on every bind, so the arm holds
    /// the exact string read and re-emits it. **No engine runs for it**
    /// ([`crate::engine_driver::plan_engines`]): neither loop is the one a newer
    /// build chose, and running either could fetch, evict or upload what that
    /// mode would not — so the location sits untracked, its files untouched,
    /// until a build that names the mode serves it or the user picks a mode.
    #[serde(untagged)]
    Other(String),
}

impl LocationMode {
    /// The mode a **fresh** binding starts in — the one place the per-platform
    /// default lives (`on-demand-files.md` § On-Demand Files → *The choice is
    /// the user's*; user ruling 2026-09-26).
    ///
    /// **windows: on-demand** — uniform with apple's default-ON Finder/Files
    /// presence and with the OneDrive Files-On-Demand default a windows user
    /// expects: a bound directory becomes a cfapi sync root the moment the
    /// folder-bind reconcile serves it, a big set costs metadata until opened,
    /// and always-resident is the deliberate second step via the bound row's
    /// `folder-location-mode-toggle`. **Every other platform: always-resident**
    /// — a bound location on macOS is always-resident by design (the FP domain
    /// yields to it, § *Apple File Provider binding*), and a fresh linux
    /// binding starts always-resident too: its FUSE root serves an on-demand
    /// binding, but only where the host can mount one, and whether linux
    /// follows windows to an on-demand default is decided with that root
    /// (§ *Linux FUSE binding*, the flips rule) — the bound row's switch is the
    /// way in. The platform decision lives in [`fauna_ipc::sync::fresh_binding_mode`],
    /// shared with the apps' optimistic binding rows so an unseen row renders
    /// exactly the mode this bind will give it; unit-tested on every platform
    /// for its own answer.
    ///
    /// Deliberately NOT the serde `#[default]` above: that one is the ordinary
    /// additive-field read default for `mode`. Only NEW binds take this
    /// default; a persisted mode is never touched.
    pub fn fresh_binding_default() -> Self {
        if fauna_ipc::sync::fresh_binding_mode() == LocationMode::OnDemand.wire_str() {
            LocationMode::OnDemand
        } else {
            LocationMode::Always
        }
    }

    /// The mode as the shared wire string every app + control-plane row uses
    /// ([`fauna_ipc::sync::LocationInfo::mode`], [`fauna_ipc::sync::EngineInfo::mode`]).
    /// Kept in one place so the two projections never drift (priority #3).
    ///
    /// A carried [`LocationMode::Other`] reports its own string, so an app
    /// renders exactly what the file holds and offers no mode it cannot name.
    pub fn wire_str(&self) -> &str {
        match self {
            LocationMode::Always => "always",
            LocationMode::OnDemand => "on-demand",
            LocationMode::Other(raw) => raw,
        }
    }
}

/// `Default` is deliberate: this struct grows a field per feature, and every
/// test/fixture that hand-listed all of them broke on each addition — and two
/// branches growing it independently collided on the same axis. Construct with
/// `..Default::default()` so an added field lands in one place.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocationConfig {
    pub path: String,
    #[serde(default)]
    pub mode: LocationMode,
    /// The **label** of the nest folder this location is bound to, or `None`
    /// if unbound. `None` is *not* a default name — an unbound location is
    /// logged + skipped rather than served under a manufactured name. See
    /// `docs/goal/behavior/on-demand-files.md` § *Hosting multiple on-demand
    /// folders*.
    ///
    /// Never read on its own to decide what a location is bound to: set names
    /// are unique only per owner, so the binding is this label **paired with**
    /// [`folder_id`](Self::folder_id), and [`binding`](Self::binding) is the
    /// one reader that pairs them.
    ///
    /// Spelled `folder` since phase 1b's rename from `file_set`; the read
    /// alias for an installed device's pre-rename `config.toml` was retired
    /// 2026-09-24 (the compat-remnant sweep at its universal scope,
    /// `version-compatibility.md` § Dimension 2 — no pre-rename config rests
    /// anywhere).
    #[serde(default)]
    pub folder: Option<String>,
    /// This set's `FolderRef` in wire form — the **unambiguous** identity of the
    /// set [`folder`](Self::folder) names. Set names are unique only *per
    /// owner*, so a caller who owns "docs" and is a member of someone else's
    /// "docs" cannot express which one this folder is bound to by name alone
    /// (account-data-plane.md § The ratified decisions): the agent would resolve key material, the engine-registry
    /// key and the state-DB path for whichever set came first in the pushed blob.
    ///
    /// Written together with [`folder`](Self::folder) by the one bind verb
    /// (`SetLocationFolder`, which requires it). The name-keyed binding a
    /// pre-identity client wrote without it was retired 2026-09-24 (the
    /// compat-remnant sweep at its universal scope, `version-compatibility.md`
    /// § Dimension 2): a row carrying a label but no parseable ref is read as
    /// **unbound** by [`binding`](Self::binding), never resolved by its name.
    #[serde(default)]
    pub folder_id: Option<String>,
    /// The set's write grant was **revoked** by the nest that owns it, so this
    /// binding is parked: fail-closed and *visibly* untracked rather than
    /// silently retrying forever (`file-sync.md` § Multi-writer shared sets —
    /// D4). Set when a running engine's [`AccessGate`] flips on a typed refusal
    /// at the `write_token.mint` or `changes.record` plane
    /// ([`crate::engine_driver::park_revoked_set`]).
    ///
    /// [`AccessGate`]: fauna_sync_engine::access_gate::AccessGate
    ///
    /// **The local folder and everything in it is untouched** — the row stays so
    /// the client can render the parked state and offer removal; only the engine
    /// stops. Persisted (rather than kept in memory) so an agent restart cannot
    /// silently resume a binding the owning nest has already refused; the
    /// recovery path is a fresh bind, which clears this and re-runs the eager
    /// bind-time mint verify (D3) against the authoritative nest.
    ///
    /// `#[serde(default)]` keeps it additive: an absent field decodes as `false`
    /// = live.
    #[serde(default)]
    pub access_revoked: bool,
}

/// A location's binding as every consumer reads it: the set's label and its
/// parsed [`FolderRef`] identity, which keys the engine registry, the pushed
/// key material and the per-set state DB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocationBinding<'a> {
    /// The set's name — a label for logs and the engine, never a key.
    pub folder: &'a str,
    /// The set's identity.
    pub folder_ref: FolderRef,
}

impl LocationConfig {
    /// The location's binding, or `None` when it is unbound.
    ///
    /// The one place the two at-rest fields are paired: a location is bound
    /// only when it carries a label **and** a ref that parses. Anything less —
    /// no ref, or one [`FolderRef::parse`] refuses — reads as unbound, which
    /// serves nothing, rather than as a binding resolved by a name two sets
    /// can share.
    #[must_use]
    pub fn binding(&self) -> Option<LocationBinding<'_>> {
        Some(LocationBinding {
            folder: self.folder.as_deref()?,
            folder_ref: FolderRef::parse(self.folder_id.as_deref()?)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    /// The device's bound locations.
    ///
    /// At rest this is `locations` (the identifier followed the vocabulary onto
    /// *location* in phase 1a leg 2 step 3). The `sync_folders` read alias and
    /// the bare string-list entry form that kept a pre-flip `config.toml`
    /// loading were retired 2026-09-24 (the compat-remnant sweep at its
    /// universal scope): an unknown `sync_folders` key is ignored and the
    /// device's `locations` decode as written.
    #[serde(default)]
    pub locations: Vec<LocationConfig>,
    #[serde(default = "default_sync_enabled")]
    pub sync_enabled: bool,
    /// LRU eviction limit for chunk cache in bytes. Default: 1 GB.
    pub chunk_cache_max_bytes: Option<u64>,
    /// Maximum concurrent chunk transfers. Default: 8.
    pub adaptive_concurrency_max: Option<u32>,
    /// Device-global pause: when `true` the agent serves no sync engines (and,
    /// once the segment coordinator is agent-hosted, no backup) — the uniform
    /// `Pause`/`Resume` control-plane flag (`sync-agent.md` § Control plane split).
    /// Persisted here, in the shared agent's own device-global state, so every
    /// platform shares one shape; honored by [`reconcile_engines`](crate::engine_driver::reconcile_engines).
    /// Additive: an old `config.toml` without the key deserializes to `false`.
    #[serde(default)]
    pub paused: bool,
}

fn default_sync_enabled() -> bool {
    true
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            locations: Vec::new(),
            sync_enabled: true,
            chunk_cache_max_bytes: None,
            adaptive_concurrency_max: None,
            paused: false,
        }
    }
}

impl SyncConfig {
    pub fn find_location(&self, path: &str) -> Option<&LocationConfig> {
        self.locations.iter().find(|f| f.path == path)
    }

    pub fn find_location_mut(&mut self, path: &str) -> Option<&mut LocationConfig> {
        self.locations.iter_mut().find(|f| f.path == path)
    }
}

/// Resolves every filesystem location the sync service owns. Two modes:
///
/// * **override** — `SyncPaths::new(Some(root))` is the `--data-dir <root>`
///   data-root override (dev/CI test isolation): `config.toml`, `device.db`, every
///   per-folder `fsid-*.db`, and the chunk-cache live directly under `root`.
/// * **default** — `SyncPaths::new(None)` is the production per-user layout:
///   `config.toml`, the state DBs, and the chunk-cache all live under
///   `%LOCALAPPDATA%\Fauna\sync` (the interactive user's profile) — the layout
///   `docs/goal/architecture/installers/windows.md` § Installation Directory Layout
///   describes. The old `%PROGRAMDATA%\Fauna\sync\config.toml` location is
///   abandoned; folder↔folder bindings are re-creatable device-local config
///   (the user re-binds folders in-client), so no user-irrecoverable data is lost.
///
/// `device.toml` is deliberately **not** resolved here: it is the machine-wide
/// `%PROGRAMDATA%\Fauna\device.toml` file written by FaunaNest and read by Bridge
/// (via [`fauna_ipc::device::DeviceConfig::default_path`]). The per-user sync
/// agent does not resolve or read it — it sources its nest URL from the live
/// provision — so this path is never folded under the per-service data-root
/// (`windows.md` § Shared device configuration; the same rule `fauna-nest-service`
/// keeps by resolving `device.toml` at `data_dir`'s parent).
#[derive(Debug, Clone)]
pub struct SyncPaths {
    /// `Some` = the `--data-dir` override root (everything under it); `None` = the
    /// production machine layout.
    override_root: Option<PathBuf>,
    /// Per-actor state scope (`on-demand-files.md` § Multi-account × File
    /// Provider, consequence 3): when set, every path under [`Self::base_dir`]
    /// resolves inside `<flat base>/<actor-id-hex>/`, so one account's engines
    /// can never read another's state. Set from the provisioned capability's
    /// actor on EVERY layout — the production base and a `--data-dir` override
    /// alike (the override only moves the flat base; the e2e exemption that
    /// kept it flat was retired 2026-09-26, see `service::apply_actor_scope`).
    /// Shared (`Arc`) across the clones every handler holds, so a
    /// provision-time re-scope reaches all of them at once.
    actor_scope: std::sync::Arc<std::sync::RwLock<Option<String>>>,
}

impl SyncPaths {
    /// Build from the optional `--data-dir` value.
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        Self {
            override_root: data_dir,
            actor_scope: std::sync::Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// The production (no `--data-dir` override) layout. The per-actor scope
    /// no longer keys on it (it activates on every layout); the one remaining
    /// consumer is `trust::trust_dir_for_paths`, which keeps the install-scoped
    /// trust home only here and a `--data-dir` trust store inside that root.
    pub fn is_production(&self) -> bool {
        self.override_root.is_none()
    }

    /// The current per-actor scope (lowercase actor-id hex), if any.
    pub fn actor_scope(&self) -> Option<String> {
        self.actor_scope.read().expect("actor scope lock").clone()
    }

    /// Set or clear the per-actor scope. The caller (the capability
    /// restore/provision path) validates the hex via the shared
    /// [`fauna_sync_engine::db::actor_state_dir`] derivation before scoping and
    /// reloads config afterwards — the scope changes what every path means.
    // Callers: `service::apply_actor_scope` and the unprovision teardown in
    // pipe_server.rs, both #[cfg(any(target_os = "macos", windows,
    // target_os = "linux"))] — every target this crate builds for.
    pub fn set_actor_scope(&self, scope: Option<String>) {
        *self.actor_scope.write().expect("actor scope lock") = scope;
    }

    /// The unscoped flat base — the home of the log dir and the custodian
    /// store (both process-level, never per-actor).
    pub fn flat_base_dir(&self) -> PathBuf {
        match &self.override_root {
            Some(root) => root.clone(),
            None => Self::production_base_dir(),
        }
    }

    /// Shared base directory for all sync-owned files. Override → the root; default →
    /// per-OS production layout. Config, state DBs, and chunk-cache all live here — a
    /// single per-user root. `pub` so `main` can resolve the same root for
    /// `fauna_log::init` (logs land at `<base_dir>/logs/fauna.log.<date>`, alongside
    /// config + state).
    ///
    /// * **Windows** — `%LOCALAPPDATA%\Fauna\sync` (the interactive user's profile),
    ///   per `docs/goal/architecture/installers/windows.md` § Installation Directory
    ///   Layout.
    /// * **macOS** — `~/Library/Application Support/Fauna/sync`, the user-domain
    ///   root (`fauna_core::platform_ids::apple_user_domain_home`) — the macOS
    ///   twin of the linux/windows shape. **Never the app-group container**
    ///   (moved out 2026-08-25): `~/Library/Group Containers` is TCC-protected on
    ///   macOS 15+ and a launchd-spawned agent is prompted there on every
    ///   instance, no user decision ever binding the next one
    ///   (`installers/macos.md` § Identifier domain, item 5). This process never
    ///   opens the container; the container is the sandboxed File Provider
    ///   extension's root, and each set's engine state lives in its HOST's root
    ///   — a bound folder's in this one, an FP domain's in the extension's
    ///   (`on-demand-files.md` § Apple File Provider binding, *state
    ///   unification*; per-set single-writer via the one-local-presence rule).
    ///   The pre-unification `LOCALAPPDATA` fallback resolved to a relative
    ///   `C:\ProgramData` path here — unwritable under launchd, so no real macOS
    ///   agent state predates the 2026-07-19 layout, and no signed artifact ever
    ///   shipped the container layout, so nothing predates this one either.
    /// * **linux (+ other unix)** — `$XDG_CONFIG_HOME/fauna/sync` (fallback
    ///   `~/.config/fauna/sync`), the SAME dir the in-app GTK `SyncDriver` used —
    ///   plan D7's move-don't-recreate: paths identical, nothing copied (the A3
    ///   cutover's default, replacing the unwritable `LOCALAPPDATA` fallthrough).
    pub fn base_dir(&self) -> PathBuf {
        let flat = self.flat_base_dir();
        match self.actor_scope() {
            Some(hex) => flat.join(hex),
            None => flat,
        }
    }

    /// The per-OS resolution lives in ONE place since W6 (account-data-plane.md § Workstreams) —
    /// `fauna_account_store::root::platform_state_base()` (re-exported as
    /// `fauna_sync_engine::root`) — because the account store now shares
    /// this exact base with the agent's sync state, and two divergent
    /// copies of the derivation would be the journal-equivocation trap the
    /// unification closed (`account-data-plane.md` § The account store).
    /// The values are unchanged: the layouts documented on
    /// [`Self::base_dir`] above, incident history included, moved with the
    /// code into `root.rs`.
    fn production_base_dir() -> PathBuf {
        fauna_sync_engine::root::platform_state_base()
    }

    /// Path to `config.toml`.
    pub fn config_path(&self) -> PathBuf {
        self.base_dir().join("config.toml")
    }

    /// The per-set state DB for a binding, keyed by the set's identity
    /// (`fsid-<ref>.db`, [`FolderRef::db_component`]).
    ///
    /// Two sets can share a name (`UNIQUE(name, actor_id)` — unique only per
    /// owner), so a name-keyed path pointed **two** engines at one
    /// `fs-docs.db`: a shared SQLite state file where each engine's rescan sees
    /// the other's file rows as remote changes. Keying on the ref separates
    /// them. The name-keyed path a pre-identity binding used, and the rename
    /// that adopted its DB onto this one, were retired 2026-09-24 (the
    /// compat-remnant sweep): no binding carries only a name.
    pub fn sync_db_path_for_ref(&self, folder_ref: FolderRef) -> PathBuf {
        folder_ref.state_db_path(&self.base_dir())
    }

    /// Chunk-cache directory.
    #[allow(dead_code)]
    pub fn chunk_cache_dir(&self) -> PathBuf {
        self.base_dir().join("chunk-cache")
    }

    /// Load `config.toml` from [`Self::config_path`], or the default config when
    /// the file is absent.
    ///
    /// An absent file and an unreadable one are different states: the first is
    /// a fresh agent, the second (`Err`) a file this build must never save over
    /// ([`Self::save_config`]).
    pub fn load_config(&self) -> Result<SyncConfig> {
        let path = self.config_path();
        if !path.exists() {
            return Ok(SyncConfig::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&content).context("parsing sync config")
    }

    /// Persist `config` to [`Self::config_path`], creating the directory if needed.
    ///
    /// **Refuses to replace a file this build cannot read.** A failed
    /// [`Self::load_config`] leaves the agent running on the default config,
    /// and every bind and mode switch saves the config whole — so without this
    /// check the first save after a downgrade, or after any file this build
    /// cannot parse, would erase every binding in it (`transport.md` § Rule 3
    /// in full → *The store around the enum*; `version-compatibility.md`
    /// § Dimension 1). The check sits here, the one write every save site goes
    /// through, and re-reads the file at the moment of writing. The refusal
    /// surfaces as the failing bind's error; the file is left byte for byte.
    ///
    /// The write is atomic (write-then-rename), so a crash mid-save leaves the
    /// previous file rather than a torn one this check would then refuse.
    pub fn save_config(&self, config: &SyncConfig) -> Result<()> {
        let path = self.config_path();
        if path.exists() {
            let current = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            if let Err(e) = toml::from_str::<SyncConfig>(&current) {
                anyhow::bail!(
                    "refusing to overwrite {}: this build cannot read it ({e}); \
                     it is left as it is",
                    path.display()
                );
            }
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(config)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two same-named sets that motivated the identity must get **two** state
    /// DBs. Sharing one `fs-docs.db` had each engine's rescan reading the other's
    /// file rows as remote changes.
    #[test]
    fn two_same_named_sets_get_separate_state_dbs() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = SyncPaths::new(Some(tmp.path().to_path_buf()));

        let owned = paths.sync_db_path_for_ref(FolderRef::Local(7));
        let foreign = paths.sync_db_path_for_ref(FolderRef::Foreign([0xab; 32]));
        assert_ne!(owned, foreign);
        assert_eq!(owned, tmp.path().join("fsid-local-7.db"));
    }

    /// A location is bound only by a label **and** a parseable ref. A label
    /// alone — the retired name-keyed binding — or a ref `FolderRef::parse`
    /// refuses reads as unbound, never as a binding resolved by name.
    #[test]
    fn a_binding_needs_a_parseable_ref() {
        let mut loc = LocationConfig {
            path: "/d".into(),
            folder: Some("docs".into()),
            ..Default::default()
        };
        assert_eq!(loc.binding(), None, "a label alone is not a binding");
        loc.folder_id = Some("not-a-ref".into());
        assert_eq!(loc.binding(), None, "an unparseable ref is not a binding");
        loc.folder_id = Some(FolderRef::Local(7).to_wire());
        assert_eq!(
            loc.binding(),
            Some(LocationBinding {
                folder: "docs",
                folder_ref: FolderRef::Local(7)
            })
        );
        loc.folder = None;
        assert_eq!(loc.binding(), None, "a ref with no label is not a binding");
    }

    /// The pre-flip `sync_folders` key and its bare string-list entry form are
    /// no longer read (retired 2026-09-24 with the compat-remnant sweep): the
    /// key is unknown and ignored, so the device has no locations. The pin is
    /// the refusal — an alias or entry form quietly re-added would be a remnant
    /// serving nothing.
    #[test]
    fn the_pre_flip_sync_folders_key_no_longer_loads() {
        let toml = r#"
sync_folders = ["C:\\Users\\alice\\Docs", "D:\\Photos"]
sync_enabled = true
"#;
        let config: SyncConfig = toml::from_str(toml).unwrap();
        assert!(
            config.locations.is_empty(),
            "a pre-flip key must not populate locations: {:?}",
            config.locations
        );
    }

    #[test]
    fn deserialize_new_format() {
        let toml = r#"
sync_enabled = true

[[locations]]
path = "C:\\Users\\alice\\Docs"
mode = "on-demand"

[[locations]]
path = "D:\\Photos"
mode = "always"
"#;
        let config: SyncConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.locations.len(), 2);
        assert_eq!(config.locations[0].mode, LocationMode::OnDemand);
        assert_eq!(config.locations[1].mode, LocationMode::Always);
    }

    /// The pre-rename binding keys `file_set` / `file_set_id` are no longer
    /// read (their aliases were retired 2026-09-24 with the compat-remnant
    /// sweep): the location decodes UNBOUND, which the multi-root host logs and
    /// skips. The pin is the refusal.
    #[test]
    fn the_pre_rename_binding_keys_no_longer_load() {
        let legacy = r#"
sync_enabled = true

[[locations]]
path = "C:\\Users\\alice\\Docs"
mode = "on-demand"
file_set = "documents"
file_set_id = "owner:documents"
"#;
        let config: SyncConfig = toml::from_str(legacy).unwrap();
        assert_eq!(config.locations.len(), 1);
        assert!(
            config.locations[0].folder.is_none() && config.locations[0].folder_id.is_none(),
            "pre-rename keys must not bind: {:?}",
            config.locations[0]
        );
    }

    #[test]
    fn defaults_are_sane() {
        let config = SyncConfig::default();
        assert!(config.sync_enabled);
        assert!(config.locations.is_empty());
        assert!(!config.paused, "a fresh agent is not paused");
    }

    #[test]
    fn paused_defaults_false_and_round_trips() {
        // Additive field: an old `config.toml` written before `paused` existed
        // deserializes to `false`, never an error (no user-visible config loss).
        let old = "sync_enabled = true\n";
        let config: SyncConfig = toml::from_str(old).unwrap();
        assert!(!config.paused, "missing `paused` must default to false");

        // Pause persists across a save/load — the property the Pause/Resume
        // handler relies on so a paused agent stays paused across a restart.
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = SyncPaths::new(Some(tmp.path().to_path_buf()));
        let paused_cfg = SyncConfig {
            paused: true,
            ..SyncConfig::default()
        };
        paths.save_config(&paused_cfg).expect("save paused config");
        assert!(
            paths.load_config().expect("load paused config").paused,
            "paused must survive save + load"
        );
    }

    #[test]
    fn mode_wire_str_matches_the_shared_vocabulary() {
        // The one place LocationMode → wire string lives; LocationInfo::mode
        // and EngineInfo::mode must both read the same values.
        assert_eq!(LocationMode::Always.wire_str(), "always");
        assert_eq!(LocationMode::OnDemand.wire_str(), "on-demand");
    }

    /// The fresh-binding default is platform-scoped (user ruling 2026-09-26):
    /// on-demand on windows, always-resident everywhere else — and it is NOT
    /// the serde default, which stays `Always` as the ordinary additive-field
    /// read default for `mode`.
    #[test]
    fn fresh_binding_default_is_on_demand_on_windows_only() {
        let expected = if cfg!(windows) {
            LocationMode::OnDemand
        } else {
            LocationMode::Always
        };
        assert_eq!(LocationMode::fresh_binding_default(), expected);
        assert_eq!(
            LocationMode::default(),
            LocationMode::Always,
            "the serde/read fallback stays always-resident on every platform"
        );
        let row: LocationConfig = toml::from_str("path = \"/docs\"\n").unwrap();
        assert_eq!(
            row.mode,
            LocationMode::Always,
            "a persisted row lacking `mode` reads back always-resident, never the fresh-binding default"
        );
    }

    #[test]
    fn deserialize_new_format_with_folder_round_trips() {
        let toml = r#"
sync_enabled = true

[[locations]]
path = "C:\\Users\\alice\\Docs"
mode = "on-demand"
folder = "documents"
"#;
        let config: SyncConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.locations.len(), 1);
        assert_eq!(config.locations[0].mode, LocationMode::OnDemand);
        assert_eq!(config.locations[0].folder.as_deref(), Some("documents"));
    }

    #[test]
    fn entry_without_folder_yields_none_not_a_default_name() {
        // New-format entry lacking `folder` (the common case before the user
        // binds one) must deserialize to `None`, NOT a manufactured default —
        // an unbound on-demand folder is logged + skipped, never served under a
        // made-up folder name.
        let toml = r#"
sync_enabled = true

[[locations]]
path = "C:\\Users\\alice\\Docs"
mode = "on-demand"
"#;
        let config: SyncConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.locations.len(), 1);
        assert!(
            config.locations[0].folder.is_none(),
            "missing folder must be None, not a default name"
        );
    }

    #[test]
    fn sync_paths_override_root_resolves_everything_under_root() {
        // The `--data-dir` data-root override: every sync-owned path
        // (config.toml, per-folder fsid-*.db, chunk-cache) lands
        // directly under the override root — no `%PROGRAMDATA%`/`%LOCALAPPDATA%`
        // read, fully deterministic.
        let root = std::path::Path::new(r"C:\tmp\fauna-data");
        let paths = SyncPaths::new(Some(root.to_path_buf()));
        assert_eq!(paths.config_path(), root.join("config.toml"));
        assert_eq!(
            paths.sync_db_path_for_ref(FolderRef::Local(3)),
            root.join("fsid-local-3.db")
        );
        assert_eq!(paths.chunk_cache_dir(), root.join("chunk-cache"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn sync_paths_default_uses_fauna_sync_subpath() {
        // Production default (no override): config + DBs live under the per-OS
        // per-user `…/{Fauna,fauna}/sync/` base — `%LOCALAPPDATA%\Fauna\sync`
        // on windows (windows.md § Installation Directory Layout),
        // `$XDG_CONFIG_HOME|~/.config` + `fauna/sync` on linux. macOS moved to
        // the shared app-group container at the M4 state unification and has
        // its own pin (`macos_default_is_the_shared_app_group_sync_dir`).
        let paths = SyncPaths::new(None);
        #[cfg(windows)]
        let fauna_sync = std::path::Path::new("Fauna").join("sync");
        #[cfg(all(unix, not(target_os = "macos")))]
        let fauna_sync = std::path::Path::new("fauna").join("sync");
        assert!(
            paths
                .config_path()
                .ends_with(fauna_sync.join("config.toml"))
        );
        assert!(
            paths
                .sync_db_path_for_ref(FolderRef::Local(3))
                .ends_with(fauna_sync.join("fsid-local-3.db"))
        );
    }

    #[test]
    fn actor_scope_scopes_every_state_path_and_is_shared_across_clones() {
        // Per-actor state scoping (`file-sync.md` § Multi-account × File
        // Provider, consequence 3): the scope re-roots every state path under
        // `<base>/<actor-hex>/`, the flat base stays reachable for the
        // logs + custodian store, and the slot is shared across the clones
        // every handler holds so a provision-time re-scope reaches all of them.
        let root = std::path::Path::new("/tmp/fauna-agent-scope-test");
        let paths = SyncPaths::new(Some(root.to_path_buf()));
        let clone = paths.clone();
        assert_eq!(paths.base_dir(), root);
        assert_eq!(paths.actor_scope(), None);

        let hex = "ab".repeat(32);
        paths.set_actor_scope(Some(hex.clone()));
        let scoped = root.join(&hex);
        assert_eq!(paths.base_dir(), scoped);
        assert_eq!(paths.config_path(), scoped.join("config.toml"));
        assert_eq!(
            paths.sync_db_path_for_ref(FolderRef::Local(7)),
            scoped.join("fsid-local-7.db")
        );
        assert_eq!(paths.flat_base_dir(), root, "flat base is scope-blind");
        assert_eq!(
            clone.actor_scope(),
            Some(hex),
            "clones share the scope slot"
        );

        clone.set_actor_scope(None);
        assert_eq!(paths.base_dir(), root, "clearing via any clone clears all");
    }

    // The per-OS value pins (XDG/home fallbacks, the GTK-cutover
    // same-dir contract, the empty/relative-HOME absoluteness traps)
    // moved to the one derivation's own tests at W6:
    // `fauna_account_store::root::tests`. The two tests below pin what
    // stays THIS binary's contract — the real-env delegation result.
    #[test]
    fn production_base_dir_is_absolute_on_every_platform() {
        // The invariant behind both per-OS branches, pinned once: the agent runs
        // under launchd / systemd / the SCM, none of which guarantee a writable
        // cwd. A relative base dir is therefore never merely "wrong" — it panics
        // the log stack before the agent can report anything (2026-07-20).
        // This asserts the REAL process env, so it also catches a future branch
        // that reintroduces an env-var-with-empty-default resolution.
        let base = SyncPaths::new(None).base_dir();
        assert!(
            base.is_absolute(),
            "production base dir must be absolute, got {base:?}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_default_is_the_user_domain_sync_dir_never_the_container() {
        // The user-domain root (2026-08-25): this agent never opens the
        // TCC-protected app-group container — a launchd agent is prompted
        // there per instance and no decision binds the next one.
        let paths = SyncPaths::new(None);
        let base = paths.base_dir();
        assert!(base.ends_with("Library/Application Support/Fauna/sync"));
        assert!(
            !base.to_string_lossy().contains("Group Containers"),
            "the agent's base dir must never resolve under the app-group container: {base:?}"
        );
        // `ends_with` alone is NOT enough — it is satisfied by a RELATIVE
        // `Library/…/sync` an empty HOME used to produce, which is exactly how
        // the 2026-07-20 launchd crash-loop passed this test.
        assert!(base.is_absolute(), "base dir must be absolute: {base:?}");
    }

    #[test]
    fn sync_paths_default_config_and_state_share_per_user_base() {
        // Per-user layout: config.toml and the state DBs must resolve to the SAME
        // parent directory — both under `%LOCALAPPDATA%\Fauna\sync`, never split
        // between %PROGRAMDATA% (old config) and %LOCALAPPDATA% (old state).
        //
        // This test only has a genuine RED precondition when LOCALAPPDATA and
        // PROGRAMDATA are set AND distinct (always true on a live Windows session:
        // LOCALAPPDATA = C:\Users\<user>\AppData\Local, PROGRAMDATA = C:\ProgramData).
        // In a stripped-env / non-Windows harness both fall back to the same constant,
        // so the parent comparison would pass trivially and prove nothing — skip then,
        // rather than silently asserting a tautology.
        let local = std::env::var("LOCALAPPDATA").ok();
        let prog = std::env::var("PROGRAMDATA").ok();
        if local.is_none() || prog.is_none() || local == prog {
            return;
        }
        let paths = SyncPaths::new(None);
        let config_parent = paths
            .config_path()
            .parent()
            .expect("config_path has a parent")
            .to_path_buf();
        let state_parent = paths
            .sync_db_path_for_ref(FolderRef::Local(3))
            .parent()
            .expect("the state DB path has a parent")
            .to_path_buf();
        assert_eq!(
            config_parent, state_parent,
            "config.toml and the state DBs must share the same per-user base dir \
             (%LOCALAPPDATA%\\Fauna\\sync); found config under {config_parent:?} \
             but state under {state_parent:?}"
        );
    }

    #[test]
    fn sync_paths_save_and_load_round_trips_under_override_root() {
        // `save_config`/`load_config` write+read `config.toml` under the override
        // root — the seam that makes `--data-dir` actually relocate writes.
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = SyncPaths::new(Some(tmp.path().to_path_buf()));
        let config = SyncConfig {
            locations: vec![LocationConfig {
                path: r"C:\Users\alice\Docs".to_string(),
                mode: LocationMode::OnDemand,
                folder: Some("documents".to_string()),
                ..Default::default()
            }],
            ..SyncConfig::default()
        };
        paths
            .save_config(&config)
            .expect("save under override root");

        // The file lands under the override root, not the machine default.
        assert!(
            tmp.path().join("config.toml").exists(),
            "config.toml must be written under the override root"
        );

        let loaded = paths.load_config().expect("load under override root");
        assert_eq!(loaded.locations.len(), 1);
        assert_eq!(loaded.locations[0].folder.as_deref(), Some("documents"));
        assert_eq!(loaded.locations[0].mode, LocationMode::OnDemand);
    }

    /// The open arm of `transport.md` § Rule 3 in full on [`LocationMode`],
    /// proven against a test-only twin standing in for a NEWER agent — one
    /// mode the real type has never heard of — through `config.toml` itself.
    #[derive(Serialize)]
    #[serde(rename_all = "kebab-case")]
    enum NewerLocationMode {
        OnDemand,
        PinnedCloud,
    }

    #[derive(Serialize)]
    struct NewerLocation {
        path: String,
        mode: NewerLocationMode,
        folder: Option<String>,
        folder_id: Option<String>,
    }

    #[derive(Serialize)]
    struct NewerConfig {
        locations: Vec<NewerLocation>,
    }

    fn newer_config_toml() -> String {
        toml::to_string_pretty(&NewerConfig {
            locations: vec![
                NewerLocation {
                    path: "/data/pinned".into(),
                    mode: NewerLocationMode::PinnedCloud,
                    folder: Some("docs".into()),
                    folder_id: Some(FolderRef::Local(7).to_wire()),
                },
                NewerLocation {
                    path: "/data/od".into(),
                    mode: NewerLocationMode::OnDemand,
                    folder: Some("photos".into()),
                    folder_id: Some(FolderRef::Local(8).to_wire()),
                },
            ],
        })
        .unwrap()
    }

    /// A config holding a mode this build lacks still loads; the location in
    /// it runs no engine while its neighbour is served; and a save — what every
    /// bind does — re-emits the newer mode unchanged.
    #[test]
    fn an_unknown_mode_loads_serves_nothing_and_survives_a_save() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = SyncPaths::new(Some(tmp.path().to_path_buf()));
        std::fs::write(paths.config_path(), newer_config_toml()).unwrap();

        let mut config = paths.load_config().expect("the config decodes");
        assert_eq!(
            config.locations[0].mode,
            LocationMode::Other("pinned-cloud".into())
        );
        assert_eq!(config.locations[0].mode.wire_str(), "pinned-cloud");
        assert_eq!(config.locations[1].mode, LocationMode::OnDemand);

        let planned = crate::engine_driver::plan_engines(&config);
        assert_eq!(planned.len(), 1, "no engine for the unknown mode");
        assert_eq!(planned[0].path, std::path::PathBuf::from("/data/od"));

        // A bind elsewhere saves the whole file.
        config.locations.push(LocationConfig {
            path: "/data/new".into(),
            ..Default::default()
        });
        paths
            .save_config(&config)
            .expect("a readable file is saved");
        let saved: toml::Value =
            toml::from_str(&std::fs::read_to_string(paths.config_path()).unwrap()).unwrap();
        assert_eq!(
            saved["locations"][0]["mode"].as_str(),
            Some("pinned-cloud"),
            "the carried mode is re-emitted as read"
        );
    }

    /// The refuse-to-rewrite gate: a `config.toml` this build cannot read is
    /// never replaced by a save — the agent runs on the default config after a
    /// failed load (`service.rs`, `pipe_server.rs`), and the next bind's save
    /// would otherwise erase every binding in the file.
    #[test]
    fn an_unreadable_config_is_never_overwritten() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = SyncPaths::new(Some(tmp.path().to_path_buf()));
        let newer = "locations = { by_path = { \"/data/docs\" = { mode = \"always\" } } }\n";
        std::fs::write(paths.config_path(), newer).unwrap();

        assert!(paths.load_config().is_err(), "this build cannot read it");
        let fallback = SyncConfig {
            locations: vec![LocationConfig {
                path: "/data/new".into(),
                ..Default::default()
            }],
            ..SyncConfig::default()
        };
        let refused = paths
            .save_config(&fallback)
            .expect_err("the save is refused");
        assert!(
            refused.to_string().contains("refusing to overwrite"),
            "{refused}"
        );
        assert_eq!(std::fs::read_to_string(paths.config_path()).unwrap(), newer);
    }
}
