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
/// rewrites the file atomically (write-temp-then-rename). A malformed or missing
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
    /// persist — the macOS app's read replica for the sandboxed File Provider
    /// extension, which cannot reach the user-domain primary
    /// (`security.md` § Pin custody across processes, rule 1's macOS shape).
    /// `None` everywhere else.
    mirror: Option<PathBuf>,
    cache: Mutex<PinCache>,
}

impl DiskPinStore {
    /// Open the pin store at the canonical [`NEST_IDENTITY_PIN_FILE`] inside
    /// `dir` (each app passes its own platform config dir). The uniform
    /// entrypoint every app uses, so the filename is never duplicated.
    pub fn open_in_dir(dir: &std::path::Path) -> Self {
        Self::open(dir.join(NEST_IDENTITY_PIN_FILE))
    }

    /// [`Self::open_in_dir`] plus a read replica: every persist to `dir` is
    /// followed by [`mirror_pin_file`] into `mirror_dir`, and the replica is
    /// refreshed once at open so a consumer that came up before this writer
    /// sees the current primary. The writer is still the ONE minter (rule 2):
    /// the mirror is a copy of its file, never a second store. Best-effort —
    /// a mirror that cannot be written never fails a mint; the sandboxed
    /// consumer then fails closed on its stale replica (`PinRequired`) until
    /// the next persist lands.
    pub fn open_in_dir_with_mirror(dir: &std::path::Path, mirror_dir: &std::path::Path) -> Self {
        let mut store = Self::open(dir.join(NEST_IDENTITY_PIN_FILE));
        store.mirror = Some(mirror_dir.to_path_buf());
        mirror_pin_file(dir, mirror_dir);
        store
    }

    /// Open (or start empty for) the pin file at `path`. Best-effort load: a
    /// missing/unreadable/garbled file yields an empty store rather than an error.
    pub fn open(path: PathBuf) -> Self {
        let cache = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, String>>(&s).ok())
            .map(|raw| {
                raw.into_iter()
                    .filter_map(|(host, value)| {
                        let (hex_id, seq) = fauna_client_core::nest_trust::split_pin_value(&value);
                        let id: [u8; 32] = fauna_core::hex32::decode(hex_id).ok()?;
                        Some((host, (id, seq)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            path,
            mirror: None,
            cache: Mutex::new(cache),
        }
    }

    /// Persist the current cache to disk atomically. Best-effort: an I/O error is
    /// logged, not propagated — a failed write must never break a live connection
    /// (the in-memory pin still protects this session).
    fn persist(&self, cache: &PinCache) {
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
            return;
        };
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
        if let (Some(mirror), Some(dir)) = (&self.mirror, self.path.parent()) {
            mirror_pin_file(dir, mirror);
        }
    }
}

impl NestIdentityPinStore for DiskPinStore {
    fn get(&self, host: &str) -> Option<[u8; 32]> {
        self.cache.lock().unwrap().get(host).map(|(id, _)| *id)
    }
    fn set(&self, host: &str, actor_id: [u8; 32]) {
        let mut cache = self.cache.lock().unwrap();
        cache.insert(host.to_string(), (actor_id, None));
        self.persist(&cache);
    }
    fn remove(&self, host: &str) {
        let mut cache = self.cache.lock().unwrap();
        if cache.remove(host).is_some() {
            self.persist(&cache);
        }
    }
    fn rotation_seq(&self, host: &str) -> Option<u64> {
        self.cache.lock().unwrap().get(host).and_then(|(_, s)| *s)
    }
    fn set_rotation_accepted(&self, host: &str, head: [u8; 32], seq: u64) {
        let mut cache = self.cache.lock().unwrap();
        cache.insert(host.to_string(), (head, Some(seq)));
        self.persist(&cache);
    }
    /// One lock acquisition covering the check AND the mint — see the trait
    /// doc's full rationale (`fauna_client_core::nest_trust::
    /// NestIdentityPinStore::pin_if_absent`). Without this override the
    /// default's separate `get()`-then-`set()` is a genuine race between two
    /// concurrent native graduations (tui's synchronous login-time bearer mint
    /// racing its own fire-and-forget background silent challenge, both
    /// TOFU-checking the same host) — caught intermittently by
    /// `test_nest_identity_pin_post_auth.py` under load.
    fn pin_if_absent(&self, host: &str, actor_id: [u8; 32]) -> Option<[u8; 32]> {
        let mut cache = self.cache.lock().unwrap();
        let result = pin_entry_if_absent(&mut cache, host, actor_id);
        if result.is_none() {
            self.persist(&cache);
        }
        result
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
/// copy the app keeps at `<group container>/trust/`. Rule 2 is untouched:
/// the app is still the only minter; the replica is its file, copied.
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
/// Always ABSOLUTE (missing/relative env falls back under `/tmp` /
/// `C:\ProgramData` rather than yielding a relative path — a background agent
/// under launchd runs with cwd `/`, where a relative path panics the first
/// thing that touches it; see the sync-agent's 2026-07-20 `.pkg` incident).
pub fn install_scoped_trust_home() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        macos_trust_home(std::env::var_os("HOME"))
    }
    #[cfg(windows)]
    {
        windows_trust_home(std::env::var_os("LOCALAPPDATA"))
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
) -> PathBuf {
    fauna_core::platform_ids::xdg_config_root(xdg_config_home, home).join("fauna")
}

/// See [`install_scoped_trust_home`]; must equal the parent of the sync
/// agent's `…/Fauna/sync` base plus `trust/` — the user-domain home
/// (`fauna_core::platform_ids::apple_user_domain_home`), NEVER the app-group
/// container: a launchd-spawned agent reading a pin file there is prompted on
/// every instance (`installers/macos.md` § Identifier domain, item 5). The
/// sandboxed File Provider extension cannot reach this dir, so the macOS app
/// keeps a read replica for it in the container's `trust/` — [`mirror_pin_file`].
pub fn macos_trust_home(home: Option<std::ffi::OsString>) -> PathBuf {
    fauna_core::platform_ids::apple_user_domain_home(absolute_or_none(home))
        .join("Fauna")
        .join("trust")
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
    fn unix_trust_home_prefers_absolute_xdg_then_home_then_tmp() {
        assert_eq!(
            unix_trust_home(Some("/xdg".into()), Some("/home/u".into())),
            PathBuf::from("/xdg/fauna")
        );
        // Relative XDG is ignored (a launchd/systemd child may run with cwd /).
        assert_eq!(
            unix_trust_home(Some("rel".into()), Some("/home/u".into())),
            PathBuf::from("/home/u/.config/fauna")
        );
        assert_eq!(
            unix_trust_home(None, None),
            PathBuf::from("/tmp/.config/fauna")
        );
    }

    #[test]
    fn macos_trust_home_is_the_user_domain_trust_dir() {
        // Must equal the macOS app's writer dir (`NestTrust.installPinStore()`
        // takes it from `install_scoped_trust_home` over the FFI) and the sync
        // agent's `…/Fauna/sync`-parent + `trust/` — the agent + tui read here.
        assert_eq!(
            macos_trust_home(Some("/Users/u".into())),
            PathBuf::from("/Users/u/Library/Application Support/Fauna/trust")
        );
        assert_eq!(
            macos_trust_home(None),
            PathBuf::from("/tmp/Library/Application Support/Fauna/trust")
        );
        // Never the TCC-protected container — the agent would be prompted on
        // every instance there (installers/macos.md § Identifier domain, item 5).
        assert!(
            !macos_trust_home(Some("/Users/u".into()))
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
}
