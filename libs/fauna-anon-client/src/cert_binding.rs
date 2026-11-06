//! Native nest-identity trust glue — the disk-backed TOFU pin store, plus a
//! stable re-export of the **shared** transport-free trust core that now lives
//! in `fauna_client_core::nest_trust` (so both this native connector and the web
//! SPA's `fauna-wasm` call one implementation — priority #2,
//! `docs/goal/architecture/security.md` § Transport trust).
//!
//! The pure channel-binding + identity-root logic ([`verify_cert_binding`],
//! [`check_identity_root`], [`IdentityOutcome`]/[`IdentityError`], the
//! [`NestIdentityPinStore`] trait, [`MemoryPinStore`]) moved to
//! `fauna-client-core` (the wasm + UniFFI shared layer); it is re-exported here
//! unchanged so every `fauna_anon_client::cert_binding::…` /
//! `fauna_client::cert_binding::…` path keeps resolving. What stays native-only
//! is [`DiskPinStore`] — the `std::fs`-backed `known_hosts` analogue a browser
//! can't have — and the SPKI capture / connect wiring in [`crate::trust`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

// The transport-free trust core — re-exported so existing native call sites
// (`crate::trust`, `fauna_client::cert_binding`, every app) are unchanged.
pub use fauna_client_core::nest_trust::{
    BindingError, IdentityError, IdentityOutcome, IdentityRoot, MemoryPinStore,
    NestIdentityPinStore, check_identity_root, pin_entry_if_absent, verify_cert_binding,
    verify_cert_binding_possession,
};

/// Canonical filename for the disk-backed nest-identity pin store. Lives in one
/// place so every app's on-disk layout is identical and a rename is a single
/// edit — linux (native Rust) and the UniFFI apps (via `fauna-ffi`) both root
/// it at their own platform config dir and append this name.
pub const NEST_IDENTITY_PIN_FILE: &str = "nest_identity_pins.json";

/// Disk-backed [`NestIdentityPinStore`] — the SSH `known_hosts` analogue that
/// survives process restarts. Backed by a JSON file (`{ "host": "<actor_id_hex>" }`)
/// the client installs at startup via [`crate::trust::install_pin_store`], rooted
/// at its data dir. An in-memory cache fronts the file; every [`set`](Self::set)
/// merges into the file under an advisory lock, written atomically
/// (write-temp-then-rename), so several writers of one install never lose
/// each other's pins. A malformed or missing
/// file loads as empty (a fresh box pins on first connect) — the store never
/// fails a connection on its own I/O.
/// `host → (actor_id, chain-accepted rotation seq)`. The seq is `None` for
/// an ordinary TOFU pin; on disk the value is `"<hex>"` or `"<hex>@<seq>"`
/// (the same in-band encoding as web's `LocalStoragePinStore`, parsed by
/// the shared `split_pin_value`) — the `@seq` form is the current pin
/// encoding and every reader parses it.
type PinCache = HashMap<String, ([u8; 32], Option<u64>)>;

pub struct DiskPinStore {
    path: PathBuf,
    /// A second directory that receives a copy of the pin file after every
    /// write — the macOS app's read replica for the sandboxed File Provider
    /// extension, which cannot reach the user-domain primary, and in a windows
    /// test build the replica for the e2e-isolated sync agent's `--data-dir`
    /// (`transport-trust.md` § Pin custody across processes, rule 1's macOS
    /// shape and *an isolated agent reads its app's replica*). `None`
    /// everywhere else.
    mirror: Option<PathBuf>,
    cache: Mutex<PinState>,
}

/// One change to one host whose write to the file failed: `Some` a pin
/// (minted or a rotation accepted), `None` a forget.
type HeldChange = Option<([u8; 32], Option<u64>)>;

/// The cache, plus the changes this process made that no write has carried to
/// the file yet. The held changes are laid over every read of the file, so a
/// store that cannot be written (an unwritable trust dir, a full disk) keeps
/// what this process minted or forgot for the session, and they are dropped
/// once a write lands — the file then holds them.
#[derive(Default)]
struct PinState {
    pins: PinCache,
    held: HashMap<String, HeldChange>,
}

impl PinState {
    /// Take `disk` as the pins, this process's held changes laid over it.
    fn refresh(&mut self, disk: PinCache) {
        self.pins = disk;
        for (host, change) in &self.held {
            match change {
                Some(pin) => self.pins.insert(host.clone(), *pin),
                None => self.pins.remove(host),
            };
        }
    }

    /// Hold every change from `before` to the current pins.
    fn hold_changes_since(&mut self, before: &PinCache) {
        for (host, pin) in &self.pins {
            if before.get(host) != Some(pin) {
                self.held.insert(host.clone(), Some(*pin));
            }
        }
        for host in before.keys() {
            if !self.pins.contains_key(host) {
                self.held.insert(host.clone(), None);
            }
        }
    }
}

/// Distinguishes the temp files of the writers inside one process.
static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl DiskPinStore {
    /// Open the pin store at the canonical [`NEST_IDENTITY_PIN_FILE`] inside
    /// `dir` (each app passes its own platform config dir). The uniform
    /// entrypoint every app uses, so the filename is never duplicated.
    pub fn open_in_dir(dir: &std::path::Path) -> Self {
        Self::open(dir.join(NEST_IDENTITY_PIN_FILE))
    }

    /// [`Self::open_in_dir`] plus a read replica: every write to `dir` is
    /// followed by [`mirror_pin_file`] into `mirror_dir`, and the replica is
    /// refreshed once at open so a consumer that came up before this writer
    /// sees the current primary. The replica never mints (rule 2):
    /// the mirror is a copy of its file, never a second store. Best-effort —
    /// a mirror that cannot be written never fails a mint; the sandboxed
    /// consumer then fails closed on its stale replica (`PinRequired`) until
    /// the next write lands.
    pub fn open_in_dir_with_mirror(dir: &std::path::Path, mirror_dir: &std::path::Path) -> Self {
        let mut store = Self::open(dir.join(NEST_IDENTITY_PIN_FILE));
        store.mirror = Some(mirror_dir.to_path_buf());
        mirror_pin_file(dir, mirror_dir);
        store
    }

    /// [`Self::open_in_dir`], keeping [`Self::open_in_dir_with_mirror`]'s read
    /// replica in `mirror_dir` when one is named — the one constructor an
    /// interactive app's writer takes when whether it keeps a replica is decided
    /// elsewhere (the e2e-isolated sync agent's, `fauna_client_sync::
    /// agent_spawner::spawned_agent_pin_replica_dir`).
    pub fn open_in_dir_mirrored_to(
        dir: &std::path::Path,
        mirror_dir: Option<&std::path::Path>,
    ) -> Self {
        match mirror_dir {
            Some(mirror_dir) => Self::open_in_dir_with_mirror(dir, mirror_dir),
            None => Self::open_in_dir(dir),
        }
    }

    /// Open (or start empty for) the pin file at `path`. Best-effort load: a
    /// missing/unreadable/garbled file yields an empty store rather than an error.
    pub fn open(path: PathBuf) -> Self {
        let pins = read_pin_file(&path).unwrap_or_default();
        Self {
            path,
            mirror: None,
            cache: Mutex::new(PinState {
                pins,
                held: HashMap::new(),
            }),
        }
    }

    /// The pins on disk now, refreshed into the cache. Several interactive
    /// apps of one install write this file (rule 2), so a lookup reads what
    /// the others minted or removed since this process opened it; an
    /// unreadable file falls back to the cache. The changes this process
    /// could not write are laid over what the file says ([`PinState`]).
    /// Lookups happen once per TLS graduation, so the read is noise (the
    /// read-only consumer does the same).
    fn current(&self) -> std::sync::MutexGuard<'_, PinState> {
        let mut state = self.cache.lock().unwrap();
        if let Some(disk) = read_pin_file(&self.path) {
            state.refresh(disk);
        }
        state
    }

    /// Apply one change to the store as a MERGE into the file: under an
    /// advisory exclusive lock on a sibling lock file, re-read the file, apply
    /// `op` to what is there, write it back atomically (write-temp-then-rename)
    /// only if it changed, and refresh the cache from what was written. So two
    /// interactive apps of one install (desktop app + tui) never lose each
    /// other's pins nor resurrect each other's removals (`transport-trust.md`
    /// § Pin custody across processes, rule 2). Best-effort: an I/O error is
    /// never propagated — a failed write must never break a live connection
    /// — and the change it failed to carry is held in-process, so it still
    /// protects this session and rides the next write that lands
    /// ([`PinState`]); a lock file that cannot be opened degrades to the
    /// unlocked merge.
    fn merge<R>(&self, op: impl FnOnce(&mut PinCache) -> R) -> R {
        let mut state = self.cache.lock().unwrap();
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.path.with_extension("json.lock"))
            .ok()
            .filter(|f| f.lock().is_ok());
        if let Some(disk) = read_pin_file(&self.path) {
            state.refresh(disk);
        }
        let before = state.pins.clone();
        let result = op(&mut state.pins);
        if state.pins != before || !state.held.is_empty() {
            if self.write(&state.pins) {
                state.held.clear();
            } else {
                state.hold_changes_since(&before);
            }
        }
        drop(lock); // releases the advisory lock
        result
    }

    /// Write `cache` to the file atomically (a temp file of this writer's
    /// own, then a rename), then refresh the read replica. `true` when the
    /// file now holds `cache`. The temp name is unique to the writer, so two
    /// writers on the unlocked fallback never interleave one temp file.
    fn write(&self, cache: &PinCache) -> bool {
        let raw: HashMap<&String, String> = cache
            .iter()
            .map(|(h, (id, seq))| {
                let value = match seq {
                    Some(n) => format!("{}@{n}", fauna_core::hex32::encode(id)),
                    None => fauna_core::hex32::encode(id),
                };
                (h, value)
            })
            .collect();
        let Ok(json) = serde_json::to_string_pretty(&raw) else {
            return false;
        };
        let tmp = self.path.with_extension(format!(
            "json.tmp.{}.{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let written =
            std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, &self.path).is_ok();
        if !written {
            let _ = std::fs::remove_file(&tmp);
        }
        if let (Some(mirror), Some(dir)) = (&self.mirror, self.path.parent()) {
            mirror_pin_file(dir, mirror);
        }
        written
    }
}

/// Read the pin file at `path`: `None` when it cannot be read for a reason
/// other than its absence (the caller keeps what it has); a missing file is
/// empty, and a garbled one loads as empty, as [`DiskPinStore::open`] always has.
fn read_pin_file(path: &std::path::Path) -> Option<PinCache> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(PinCache::new()),
        Err(_) => return None,
    };
    let map = serde_json::from_str::<HashMap<String, String>>(&raw).unwrap_or_default();
    Some(
        map.into_iter()
            .filter_map(|(host, value)| {
                let (hex_id, seq) = fauna_client_core::nest_trust::split_pin_value(&value);
                let id: [u8; 32] = fauna_core::hex32::decode(hex_id).ok()?;
                Some((host, (id, seq)))
            })
            .collect(),
    )
}

impl NestIdentityPinStore for DiskPinStore {
    fn get(&self, host: &str) -> Option<[u8; 32]> {
        self.current().pins.get(host).map(|(id, _)| *id)
    }
    fn set(&self, host: &str, actor_id: [u8; 32]) {
        self.merge(|pins| pins.insert(host.to_string(), (actor_id, None)));
    }
    fn remove(&self, host: &str) {
        self.merge(|pins| pins.remove(host));
    }
    fn rotation_seq(&self, host: &str) -> Option<u64> {
        self.current().pins.get(host).and_then(|(_, s)| *s)
    }
    fn set_rotation_accepted(&self, host: &str, head: [u8; 32], seq: u64) {
        self.merge(|pins| pins.insert(host.to_string(), (head, Some(seq))));
    }
    /// One lock covering the check AND the mint — this process's cache mutex
    /// and the file lock both, so the check reads every writer's pins — see
    /// the trait doc's full rationale (`fauna_client_core::nest_trust::
    /// NestIdentityPinStore::pin_if_absent`). Without this override the
    /// default's separate `get()`-then-`set()` is a genuine race between two
    /// concurrent native graduations (tui's synchronous login-time bearer mint
    /// racing its own fire-and-forget background silent challenge, both
    /// TOFU-checking the same host) — caught intermittently by
    /// `test_nest_identity_pin_post_auth.py` under load.
    fn pin_if_absent(&self, host: &str, actor_id: [u8; 32]) -> Option<[u8; 32]> {
        self.merge(|pins| pin_entry_if_absent(pins, host, actor_id))
    }
}

/// Refresh a **read replica** of the pin file: copy `primary_dir`'s pin file
/// over `replica_dir`'s (write-temp-then-rename, so a consumer never reads a
/// partial file). It OVERWRITES — the replica must follow the primary — and it
/// never empties a replica whose
/// primary file is absent. Best-effort and idempotent.
///
/// The one production use is macOS (`security.md` § Pin custody across
/// processes, rule 1): the writer's home is the user domain
/// (`~/Library/Application Support/Fauna/trust`, where the launchd agent and
/// tui read it consent-free), and the sandboxed File Provider extension —
/// which can reach only its own and the app-group containers — reads the
/// copy the app keeps at `<group container>/trust/`. Rule 2 holds:
/// the replica never mints; it is the writer's file, copied.
pub fn mirror_pin_file(primary_dir: &std::path::Path, replica_dir: &std::path::Path) {
    let src = primary_dir.join(NEST_IDENTITY_PIN_FILE);
    if !src.is_file() || primary_dir == replica_dir {
        return;
    }
    let dst = replica_dir.join(NEST_IDENTITY_PIN_FILE);
    let _ = std::fs::create_dir_all(replica_dir);
    let tmp = dst.with_extension("json.mirror-tmp");
    if std::fs::copy(&src, &tmp).is_ok() {
        let _ = std::fs::rename(&tmp, &dst);
    }
}

/// The **install-scoped trust home** — the one directory where this install's
/// interactive app(s) keep the writable nest-identity pin store and every
/// pin-consumer process (File Provider extension, background sync agent) reads
/// it back (`security.md` § Pin custody across processes, rule 1). ONE
/// derivation, shared by writer and consumer, is what makes the alignment
/// structural: tui shipped for weeks writing a per-app `fauna-tui/` store its
/// own child sync-agent never read, so a tui-provisioned agent could never
/// authenticate a TOFU-rooted (self-signed) nest — `PinRequired` forever.
///
/// | platform | home | who else uses it |
/// |---|---|---|
/// | unix (non-mac) | `$XDG_CONFIG_HOME/fauna` (fallback `~/.config/fauna`) | linux app writer; agent consumer |
/// | macOS | `<home>/Library/Application Support/Fauna/trust` — the user-domain home; the app-group container's `trust/` is the app-maintained READ REPLICA for the sandboxed File Provider extension (see [`mirror_pin_file`]) | `NestTrust.installPinStore()` (macOS) + fauna-tui writers; agent consumer; the extension reads the replica |
/// | windows | `%LOCALAPPDATA%\Fauna` | C# app writer (`BackupPaths.DataDir`); agent consumer |
///
/// Always ABSOLUTE, and `None` on a unix target with no absolute home (a
/// missing/empty/relative `HOME` and no absolute `XDG_CONFIG_HOME`): the pin
/// file decides which certificate the agent trusts for its nest, so a
/// `/tmp` fallback another local account could pre-plant is REFUSED, never
/// derived (the 2026-07-20 `.pkg` incident's relative-path
/// panic is avoided the same way). Every caller logs the refusal
/// ([`fauna_core::platform_ids::NO_HOME_REFUSAL`]) and fails closed — never a
/// panic. Windows falls back to `C:\ProgramData` as before (a machine-wide,
/// ACL'd dir).
pub fn install_scoped_trust_home() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        macos_trust_home(std::env::var_os("HOME"))
    }
    #[cfg(windows)]
    {
        Some(windows_trust_home(std::env::var_os("LOCALAPPDATA")))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        unix_trust_home(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
    }
}

/// `is_absolute` here means POSIX-absolute (`fauna_core::platform_ids::
/// is_posix_absolute`), not `Path::is_absolute` — the callers below feed it
/// `XDG_CONFIG_HOME`/`HOME`-shaped POSIX paths that are unit-tested on every
/// dev box including Windows, where the native check rejects a plain
/// `/home/u` for lack of a drive prefix.
fn absolute_or_none(v: Option<std::ffi::OsString>) -> Option<PathBuf> {
    v.map(PathBuf::from)
        .filter(|p| fauna_core::platform_ids::is_posix_absolute(p))
}

/// Pure per-platform derivations — compiled everywhere so every arm is unit
/// tested on every dev box, not just its own OS.
pub fn unix_trust_home(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    Some(fauna_core::platform_ids::xdg_config_root(xdg_config_home, home)?.join("fauna"))
}

/// See [`install_scoped_trust_home`]; must equal the parent of the sync
/// agent's `…/Fauna/sync` base plus `trust/` — the user-domain home
/// (`fauna_core::platform_ids::apple_user_domain_home`), NEVER the app-group
/// container: a launchd-spawned agent reading a pin file there is prompted on
/// every instance (`installers/macos.md` § Identifier domain, item 5). The
/// sandboxed File Provider extension cannot reach this dir, so the macOS app
/// keeps a read replica for it in the container's `trust/` — [`mirror_pin_file`].
pub fn macos_trust_home(home: Option<std::ffi::OsString>) -> Option<PathBuf> {
    Some(
        fauna_core::platform_ids::apple_user_domain_home(absolute_or_none(home))?
            .join("Fauna")
            .join("trust"),
    )
}

/// See [`install_scoped_trust_home`]; must equal the windows app's
/// `BackupPaths.DataDir` (`%LOCALAPPDATA%\Fauna`). No absoluteness filter —
/// `Path::is_absolute` is platform-semantic (a `C:\…` path reads relative on
/// unix, where this arm is still compiled for tests), and the sync agent's
/// windows arm takes `%LOCALAPPDATA%` as-is too.
pub fn windows_trust_home(local_app_data: Option<std::ffi::OsString>) -> PathBuf {
    local_app_data
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("Fauna")
}

/// Read-only [`NestIdentityPinStore`] over the same pin file — for **pin-consumer
/// processes** (a File Provider extension, a background sync agent) that read the
/// pins the interactive app minted but must never mint or remove any themselves
/// (`security.md` § Transport trust: first-trust is a user decision; the
/// connection layer sees `read_only()` and refuses an unpinned TOFU host instead
/// of silently trusting it). Deliberately **uncached**: every [`get`](Self::get)
/// re-reads the file, so a long-lived consumer picks up pins the app writes
/// *after* the consumer launched (onboarding, a user's re-trust)
/// on its next connect retry, without waiting for a process relaunch. Pin
/// lookups happen once per TLS graduation, so the extra read is noise — and it
/// also makes the app's `remove` (the user's re-trust action) visible
/// immediately, in both directions the cached store cannot be.
pub struct ReadOnlyDiskPinStore {
    path: PathBuf,
}

impl ReadOnlyDiskPinStore {
    /// Open the consumer view of the canonical [`NEST_IDENTITY_PIN_FILE`] inside
    /// `dir` — the same dir the interactive app's [`DiskPinStore`] writes.
    pub fn open_in_dir(dir: &std::path::Path) -> Self {
        Self {
            path: dir.join(NEST_IDENTITY_PIN_FILE),
        }
    }
}

impl NestIdentityPinStore for ReadOnlyDiskPinStore {
    fn get(&self, host: &str) -> Option<[u8; 32]> {
        let raw = std::fs::read_to_string(&self.path).ok()?;
        let map: HashMap<String, String> = serde_json::from_str(&raw).ok()?;
        // `"<hex>@<seq>"` after the app accepted a rotation chain — the
        // consumer must keep reading the pin (it follows the app's trust
        // decisions; failing here would strand it on `PinRequired`).
        let (hex_id, _) = fauna_client_core::nest_trust::split_pin_value(map.get(host)?);
        fauna_core::hex32::decode(hex_id).ok()
    }
    fn set(&self, host: &str, _actor_id: [u8; 32]) {
        tracing::warn!("read-only pin store: dropping pin mint for {host} (consumer process)");
    }
    fn remove(&self, host: &str) {
        tracing::warn!("read-only pin store: dropping pin removal for {host} (consumer process)");
    }
    fn read_only(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_pin_store_persists_across_reopen() {
        // Unique temp path (no tempfile dep — getrandom is already pulled in).
        let mut suffix = [0u8; 8];
        getrandom::fill(&mut suffix).unwrap();
        let path = std::env::temp_dir().join(format!("fauna-pins-{}.json", hex::encode(suffix)));

        let id = [0x7eu8; 32];
        {
            let store = DiskPinStore::open(path.clone());
            assert_eq!(store.get("pi.local"), None);
            store.set("pi.local", id);
            assert_eq!(store.get("pi.local"), Some(id));
        }
        // A fresh store over the same file sees the persisted pin.
        let reopened = DiskPinStore::open(path.clone());
        assert_eq!(reopened.get("pi.local"), Some(id), "pin survives reopen");
        // A garbled file loads as empty rather than erroring.
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(DiskPinStore::open(path.clone()).get("pi.local"), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rotation_seq_persists_across_reopen_and_a_plain_set_clears_it() {
        // The chain-accepted `(head, seq)` pair (`box-recovery.md` § Client
        // acceptance) must survive a restart — the seq is what arms fork
        // detection — and a later ordinary pin write (an explicit re-trust →
        // re-TOFU) returns the host to a plain pin.
        let mut suffix = [0u8; 8];
        getrandom::fill(&mut suffix).unwrap();
        let path = std::env::temp_dir().join(format!("fauna-pins-{}.json", hex::encode(suffix)));

        let head = [0x11u8; 32];
        {
            let store = DiskPinStore::open(path.clone());
            store.set_rotation_accepted("rotated.local", head, 3);
            assert_eq!(store.get("rotated.local"), Some(head));
            assert_eq!(store.rotation_seq("rotated.local"), Some(3));
        }
        let reopened = DiskPinStore::open(path.clone());
        assert_eq!(reopened.get("rotated.local"), Some(head), "pin survives");
        assert_eq!(
            reopened.rotation_seq("rotated.local"),
            Some(3),
            "seq survives"
        );

        // The read-only consumer view keeps reading the pin through the
        // `@seq` value form (it follows the app's trust decisions).
        let consumer = ReadOnlyDiskPinStore { path: path.clone() };
        assert_eq!(consumer.get("rotated.local"), Some(head));

        // An explicit re-TOFU (ordinary `set`) clears the accepted seq.
        reopened.set("rotated.local", [0x22u8; 32]);
        assert_eq!(reopened.rotation_seq("rotated.local"), None);

        // Plain hex is the ordinary TOFU pin form: it loads with no seq — and
        // only ever writes what it holds.
        std::fs::write(
            &path,
            format!("{{\"old.local\": \"{}\"}}", hex::encode([0x33u8; 32])),
        )
        .unwrap();
        let plain = DiskPinStore::open(path.clone());
        assert_eq!(plain.get("old.local"), Some([0x33u8; 32]));
        assert_eq!(plain.rotation_seq("old.local"), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_in_dir_uses_canonical_filename() {
        // Locks the cross-app on-disk layout: `<dir>/nest_identity_pins.json`.
        // Renaming would silently orphan every app's learned pins, so a pin
        // written via `open_in_dir` must be readable via the explicit path.
        let mut suffix = [0u8; 8];
        getrandom::fill(&mut suffix).unwrap();
        let dir = std::env::temp_dir().join(format!("fauna-pindir-{}", hex::encode(suffix)));
        std::fs::create_dir_all(&dir).unwrap();
        let id = [0x5au8; 32];
        DiskPinStore::open_in_dir(&dir).set("pi.local", id);
        let explicit = DiskPinStore::open(dir.join(NEST_IDENTITY_PIN_FILE));
        assert_eq!(explicit.get("pi.local"), Some(id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_only_store_sees_live_writes_and_never_writes() {
        let mut suffix = [0u8; 8];
        getrandom::fill(&mut suffix).unwrap();
        let dir = std::env::temp_dir().join(format!("fauna-ro-pindir-{}", hex::encode(suffix)));
        std::fs::create_dir_all(&dir).unwrap();

        let consumer = ReadOnlyDiskPinStore::open_in_dir(&dir);
        assert!(consumer.read_only());
        assert_eq!(consumer.get("pi.local"), None);

        // The consumer is uncached: a pin the app's writer store mints AFTER the
        // consumer opened is visible on the very next lookup — this is what lets
        // a live appex pick up the app's onboarding/migration write on its next
        // connect retry instead of waiting for a process relaunch.
        let id = [0x7eu8; 32];
        let writer = DiskPinStore::open_in_dir(&dir);
        writer.set("pi.local", id);
        assert_eq!(consumer.get("pi.local"), Some(id), "live write visible");

        // …and the app's `remove` (the user's re-trust action) is visible too.
        writer.remove("pi.local");
        assert_eq!(consumer.get("pi.local"), None, "live removal visible");

        // set/remove on the consumer are dropped: the file never changes.
        writer.set("pi.local", id);
        consumer.set("pi.local", [0x99u8; 32]);
        consumer.remove("other.local");
        assert_eq!(
            DiskPinStore::open_in_dir(&dir).get("pi.local"),
            Some(id),
            "consumer writes must not reach disk"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unix_trust_home_prefers_absolute_xdg_then_home() {
        assert_eq!(
            unix_trust_home(Some("/xdg".into()), Some("/home/u".into())),
            Some(PathBuf::from("/xdg/fauna"))
        );
        // Relative XDG is ignored (a launchd/systemd child may run with cwd /).
        assert_eq!(
            unix_trust_home(Some("rel".into()), Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.config/fauna"))
        );
    }

    #[test]
    fn unix_trust_home_is_refused_without_an_absolute_home_never_tmp() {
        // A pin file another local account pre-planted under a
        // `/tmp` fallback would decide which certificate the agent trusts.
        for home in [None, Some("".into()), Some("rel".into())] {
            assert_eq!(unix_trust_home(None, home.clone()), None);
            assert_eq!(unix_trust_home(Some("rel".into()), home), None);
        }
    }

    #[test]
    fn macos_trust_home_is_the_user_domain_trust_dir() {
        // Must equal the macOS app's writer dir (`NestTrust.installPinStore()`
        // takes it from `install_scoped_trust_home` over the FFI) and the sync
        // agent's `…/Fauna/sync`-parent + `trust/` — the agent + tui read here.
        assert_eq!(
            macos_trust_home(Some("/Users/u".into())),
            Some(PathBuf::from(
                "/Users/u/Library/Application Support/Fauna/trust"
            ))
        );
        // No `/tmp/Library/…` fallback for a missing/empty/relative home.
        for home in [None, Some("".into()), Some("rel".into())] {
            assert_eq!(macos_trust_home(home), None);
        }
        // Never the TCC-protected container — the agent would be prompted on
        // every instance there (installers/macos.md § Identifier domain, item 5).
        assert!(
            !macos_trust_home(Some("/Users/u".into()))
                .unwrap()
                .to_string_lossy()
                .contains("Group Containers")
        );
    }

    /// The macOS app keeps the sandboxed extension's read replica current on
    /// every persist: a mint lands in the primary AND the mirror, and a mirror
    /// that cannot be written never fails the mint (best-effort, the consumer
    /// fails closed on a stale replica — `PinRequired`, then retry).
    #[test]
    fn a_mirrored_store_lands_every_persist_in_both_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("trust");
        let mirror = tmp.path().join("container").join("trust");
        // The writer's dir exists before the store opens (the FFI wrapper
        // `install_nest_identity_pin_store_with_mirror` creates it); the
        // replica dir does NOT — `mirror_pin_file` creates that one.
        std::fs::create_dir_all(&primary).unwrap();
        let store = DiskPinStore::open_in_dir_with_mirror(&primary, &mirror);
        store.set("nest.example", [7u8; 32]);
        let reader = ReadOnlyDiskPinStore::open_in_dir(&mirror);
        assert_eq!(reader.get("nest.example"), Some([7u8; 32]));
        store.remove("nest.example");
        let reader = ReadOnlyDiskPinStore::open_in_dir(&mirror);
        assert_eq!(reader.get("nest.example"), None);
        // Both files are the canonical name — a consumer opens either dir with
        // the ordinary `open_in_dir`.
        assert!(primary.join(NEST_IDENTITY_PIN_FILE).is_file());
        assert!(mirror.join(NEST_IDENTITY_PIN_FILE).is_file());
    }

    /// `mirror_pin_file` is a plain refresh (overwrite), unlike the adoption
    /// copy, which is first-writer-wins — the replica must follow the primary
    /// even when it already exists.
    #[test]
    fn mirror_pin_file_overwrites_a_stale_replica() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("trust");
        let mirror = tmp.path().join("container").join("trust");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&mirror).unwrap();
        std::fs::write(primary.join(NEST_IDENTITY_PIN_FILE), b"{\"a\":\"1\"}").unwrap();
        std::fs::write(mirror.join(NEST_IDENTITY_PIN_FILE), b"{\"stale\":\"0\"}").unwrap();
        mirror_pin_file(&primary, &mirror);
        assert_eq!(
            std::fs::read(mirror.join(NEST_IDENTITY_PIN_FILE)).unwrap(),
            b"{\"a\":\"1\"}"
        );
        // No primary file → the replica is left alone (never emptied).
        std::fs::remove_file(primary.join(NEST_IDENTITY_PIN_FILE)).unwrap();
        mirror_pin_file(&primary, &mirror);
        assert_eq!(
            std::fs::read(mirror.join(NEST_IDENTITY_PIN_FILE)).unwrap(),
            b"{\"a\":\"1\"}"
        );
    }

    #[test]
    fn windows_trust_home_is_the_localappdata_fauna_root() {
        assert_eq!(
            windows_trust_home(Some(r"C:\Users\u\AppData\Local".into())),
            PathBuf::from(r"C:\Users\u\AppData\Local").join("Fauna")
        );
    }

    /// Two interactive apps of one install (the desktop app and tui) both open
    /// a writer on the one install-scoped store (`transport-trust.md` § Pin
    /// custody across processes, rule 2): each write must MERGE into the file,
    /// so neither loses the other's pins nor resurrects the other's removals
    /// .
    #[test]
    fn two_writers_on_one_store_merge_never_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c, d) = ([0xa1u8; 32], [0xb2u8; 32], [0xc3u8; 32], [0xd4u8; 32]);

        // A pin one writer mints survives the other's later write.
        let tui = DiskPinStore::open_in_dir(dir.path());
        let gui = DiskPinStore::open_in_dir(dir.path());
        tui.set("a.local", a);
        gui.set("b.local", b);
        let fresh = DiskPinStore::open_in_dir(dir.path());
        assert_eq!(fresh.get("a.local"), Some(a), "tui's pin kept");
        assert_eq!(fresh.get("b.local"), Some(b), "gui's pin kept");

        // A removal one writer makes is not undone by the other's later write.
        DiskPinStore::open_in_dir(dir.path()).set("c.local", c);
        let tui2 = DiskPinStore::open_in_dir(dir.path());
        let gui2 = DiskPinStore::open_in_dir(dir.path());
        tui2.remove("c.local");
        gui2.set("d.local", d);
        let fresh = DiskPinStore::open_in_dir(dir.path());
        assert_eq!(fresh.get("c.local"), None, "tui's forget kept");
        assert_eq!(fresh.get("d.local"), Some(d));

        // A writer reads the other's pins: its own lookup and its TOFU check
        // see a pin it never minted, so a different identity there is a
        // mismatch, never a second first use.
        let other = DiskPinStore::open_in_dir(dir.path());
        tui.set("e.local", a);
        assert_eq!(other.get("e.local"), Some(a));
        assert_eq!(other.pin_if_absent("e.local", b), Some(a));
        assert_eq!(fresh.get("e.local"), Some(a));

        // A removal the cache never saw still reaches the file.
        gui.remove("e.local");
        assert_eq!(DiskPinStore::open_in_dir(dir.path()).get("e.local"), None);
    }

    /// A write that fails (an unwritable trust dir, a full disk) must never
    /// cost this process the change it just made: the pin it minted holds
    /// for the session, and a pin it forgot stays forgotten, though the file
    /// still says otherwise . A later write that lands carries the
    /// held changes to the file.
    #[cfg(unix)]
    #[test]
    fn a_failed_write_keeps_this_process_change_for_the_session() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (x, y) = ([0x31u8; 32], [0x42u8; 32]);
        let store = DiskPinStore::open_in_dir(dir.path());
        store.set("y.local", y);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        // Root writes through 0500: the scenario cannot be staged there.
        let staged = std::fs::write(dir.path().join("probe"), b"").is_err();
        if staged {
            store.set("x.local", x);
            assert_eq!(store.get("x.local"), Some(x), "a failed mint holds");
            store.remove("y.local");
            assert_eq!(store.get("y.local"), None, "a failed forget holds");
            assert_eq!(
                ReadOnlyDiskPinStore::open_in_dir(dir.path()).get("y.local"),
                Some(y),
                "the file was not written"
            );
        }
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        if staged {
            // The next write that lands carries the held changes with it.
            store.set("z.local", x);
            let fresh = DiskPinStore::open_in_dir(dir.path());
            assert_eq!(fresh.get("x.local"), Some(x));
            assert_eq!(fresh.get("y.local"), None);
            assert_eq!(fresh.get("z.local"), Some(x));
        }
    }

    /// The file lock is what makes concurrent writers merge: several writers
    /// on one dir, each its own store, minting distinct hosts at once, and
    /// the file ends holding every host .
    #[test]
    fn concurrent_writers_on_one_store_lose_no_pin() {
        const WRITERS: usize = 4;
        const HOSTS: usize = 40;
        let dir = tempfile::tempdir().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(WRITERS));
        let threads: Vec<_> = (0..WRITERS)
            .map(|w| {
                let dir = dir.path().to_path_buf();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let store = DiskPinStore::open_in_dir(&dir);
                    barrier.wait();
                    for h in 0..HOSTS {
                        store.set(&format!("w{w}-h{h}.local"), [w as u8; 32]);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let fresh = ReadOnlyDiskPinStore::open_in_dir(dir.path());
        let lost: Vec<String> = (0..WRITERS)
            .flat_map(|w| (0..HOSTS).map(move |h| format!("w{w}-h{h}.local")))
            .filter(|host| fresh.get(host).is_none())
            .collect();
        assert!(lost.is_empty(), "pins lost to a concurrent write: {lost:?}");
    }

    /// The held overlay never outlives the write that lands it: once the
    /// file holds this process's change, another minter's later forget of
    /// that host stands, at this process's lookups and in the file after its
    /// next write .
    #[cfg(unix)]
    #[test]
    fn a_landed_write_drops_the_held_change_so_a_later_forget_stands() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let x = [0x31u8; 32];
        let app = DiskPinStore::open_in_dir(dir.path());
        app.set("seed.local", x);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        // Root writes through 0500: the scenario cannot be staged there.
        let staged = std::fs::write(dir.path().join("probe"), b"").is_err();
        if staged {
            app.set("h.local", x);
        }
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        if staged {
            app.set("z.local", x); // lands, carrying h
            DiskPinStore::open_in_dir(dir.path()).remove("h.local");
            assert_eq!(
                app.get("h.local"),
                None,
                "a landed change is no longer held"
            );
            app.set("w.local", x);
            assert_eq!(
                ReadOnlyDiskPinStore::open_in_dir(dir.path()).get("h.local"),
                None,
                "the next write does not resurrect the other minter's forget"
            );
        }
    }
}
