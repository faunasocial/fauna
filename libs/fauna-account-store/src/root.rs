//! The per-user account-store root — W6 (account-data-plane.md § Workstreams)'s "per-platform adoption + path
//! unification" (`account-data-plane.md` § The account store).
//!
//! One rule, stated once: the account store lives at
//! `<platform state base>/<actor-id-hex>/`[`STORE_SUBDIR`], where the
//! **platform state base** is the per-user, per-OS root the sync stack
//! already ships (the apple state-unification law of
//! `on-demand-files.md`, extended to every desktop platform):
//!
//! * **Windows** — `%LOCALAPPDATA%\Fauna\sync`
//! * **macOS** — `~/Library/Application Support/Fauna/sync`, the
//!   **user-domain** root (moved OUT of the app-group container 2026-08-25:
//!   `~/Library/Group Containers` is TCC-protected on macOS 15+, and a
//!   launchd-spawned agent is prompted on every instance with no user
//!   decision ever binding the next one — `installers/macos.md`
//!   § Identifier domain, record item 5. The container is now the
//!   sandboxed File Provider extension's root ONLY — it is not a store
//!   consumer — `on-demand-files-apple.md` § Apple File Provider binding,
//!   *state unification*)
//! * **linux (+ other unix)** — `$XDG_CONFIG_HOME/fauna/sync`
//! * **web** — no filesystem: a per-origin store NAME,
//!   `fauna-account-store/<actor-id-hex>`, naming the IndexedDB database and
//!   the OPFS segment directory — the web leg of this module (`root_web.rs`,
//!   mounted as `crate::root` on wasm32; same `StoreRoot` name, same
//!   actor-scope floor)
//!   (fallback `~/.config/fauna/sync`)
//!
//! No human ever chooses this path (`principles.md`'s two-bucket
//! configuration rule): it is a hard-coded per-OS constant, and on
//! sandboxed mobile platforms it is the app container the shell supplies
//! (artifact wiring). The point of resolving it HERE, once, is the
//! journal-equivocation trap the 2026-08-14 ⚠ Gap entry records: the T10
//! writer-key slot is machine-shared, so every process on the machine is
//! the SAME store writer — two processes keeping two store dirs under one
//! `WriterId` diverge silently and surface only as a remote
//! journal-equivocation refusal. One root per user per machine is what
//! makes the shared slot correct.
//!
//! [`StoreRoot::platform`] is the production constructor for every
//! desktop surface (apps and the sync agent — `SyncPaths` delegates its
//! per-OS base resolution here, so the two can never diverge);
//! [`StoreRoot::at`] exists for sandboxed mobile shells passing their
//! container dir, and for tests.

use std::path::{Path, PathBuf};

use anyhow::Result;

/// The account store's directory name under the per-actor state dir
/// (`<base>/<actor-id-hex>/account-store/`). Moved here from
/// `fauna_sync_engine::account_runtime` at W6 — this crate owns store
/// placement; the runtime re-exports it.
pub const STORE_SUBDIR: &str = "account-store";

/// The resolved per-user account-store root: the directory the per-actor
/// state dirs (`<root>/<actor-id-hex>/`) live directly under.
///
/// Constructed via [`Self::platform`] (desktop production — resolution is
/// internal, so an app cannot re-introduce an app-namespaced root by
/// passing the wrong dir) or [`Self::at`] (sandboxed mobile shells and
/// tests).
#[derive(Debug, Clone)]
pub struct StoreRoot(PathBuf);

impl StoreRoot {
    /// The production desktop root — [`platform_state_base`]. `Err` (with
    /// [`NO_HOME_REFUSAL`]) when no absolute home exists; every surface fails
    /// closed on it with a logged reason, never a panic.
    pub fn platform() -> Result<Self> {
        platform_state_base().map(Self)
    }

    /// An explicit root: sandboxed mobile shells passing their app
    /// container (the per-app dir IS the per-user root there — sandboxing
    /// keeps sharing per-app by construction), and tests.
    pub fn at(base: impl Into<PathBuf>) -> Self {
        Self(base.into())
    }

    /// The root directory itself.
    pub fn base(&self) -> &Path {
        &self.0
    }

    /// The store dir for one actor:
    /// `<root>/<actor-id-hex>/`[`STORE_SUBDIR`], through the shared
    /// [`actor_state_dir`](crate::db::actor_state_dir) floor (refuses
    /// junk hex rather than minting a stray directory).
    pub fn store_dir(&self, actor_id_hex: &str) -> Result<PathBuf> {
        Ok(crate::db::actor_state_dir(&self.0, actor_id_hex)?.join(STORE_SUBDIR))
    }
}

/// The per-user, per-OS platform state base (module docs for the table).
///
/// **The result is always ABSOLUTE** — an agent under launchd/systemd can
/// start with a cwd it cannot write, where a relative base panics the
/// first path touch before there is any log to diagnose it from.
///
/// **A missing/empty/relative home is REFUSED, never mapped to `/tmp`**
/// (closing the 2026-07-20 fallback): `/tmp` is
/// world-writable, so another local account could pre-create the fallback
/// path and read, or plant a `config.toml` in, the victim's account store and
/// sync state. The refusal is an `Err` (with [`NO_HOME_REFUSAL`]) on a unix
/// target with no absolute home, for every caller to log and fail closed on —
/// a process entry point exits, an app surface shows the error; never a
/// panic. Windows reads `%LOCALAPPDATA%`, which the OS always supplies, and
/// never refuses.
pub fn platform_state_base() -> Result<PathBuf> {
    production_base().ok_or_else(|| anyhow::anyhow!(NO_HOME_REFUSAL))
}

/// The reason [`platform_state_base`] refuses — one text, owned by
/// `fauna-core`.
pub use fauna_core::platform_ids::NO_HOME_REFUSAL;

#[cfg(windows)]
fn production_base() -> Option<PathBuf> {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| r"C:\ProgramData".into());
    Some(PathBuf::from(local).join("Fauna").join("sync"))
}

#[cfg(target_os = "macos")]
fn production_base() -> Option<PathBuf> {
    // `dirs::home_dir()` (not `env::var("HOME")`) so an unset HOME falls
    // back to the passwd database.
    macos_user_domain_base(dirs::home_dir())
}

/// Pure user-domain resolution (home passed in, so tests never mutate
/// process env): `<home>/Library/Application Support/Fauna/sync` — the
/// macOS twin of linux's `<config root>/fauna/sync` and windows'
/// `%LOCALAPPDATA%\Fauna\sync`, so the macOS arm is no longer the odd one
/// out. **Never the app-group container**: `~/Library/Group Containers` is
/// `kTCCServiceSystemPolicyAppData`-protected on macOS 15+, and the measured
/// 2026-08-25 record (`installers/macos.md` § Identifier domain, item 5) is
/// that a launchd-spawned background process is prompted on EVERY instance
/// there, with neither Allow nor Deny ever binding the next one — so a data
/// root under it is unreachable for the agent by construction, not merely
/// prompt-y. `~/Library/Application Support` carries no such gate (bisected
/// out as a confound in the same record). The container stays the sandboxed
/// File Provider extension's root, which is not a store consumer
/// (`on-demand-files-apple.md` § Apple File Provider binding, *state unification*).
///
/// Always absolute, `None` without an absolute home — see
/// [`platform_state_base`]; the incident behind the rule is 2026-07-20's
/// `.pkg` install failure, where an unset HOME yielded a RELATIVE base that
/// crash-looped the LaunchAgent (cwd `/`, `fauna_log::init` panicking before
/// any log existed to explain why). The refusal is an error the entry point
/// logs, not a `/tmp` fallback.
#[cfg(target_os = "macos")]
pub(crate) fn macos_user_domain_base(home: Option<PathBuf>) -> Option<PathBuf> {
    Some(
        fauna_core::platform_ids::apple_user_domain_home(home)?
            .join("Fauna")
            .join("sync"),
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn production_base() -> Option<PathBuf> {
    unix_config_base(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// Pure XDG resolution (env values passed in, so tests never mutate
/// process env): `$XDG_CONFIG_HOME/fauna/sync`, else
/// `$HOME/.config/fauna/sync`. An empty value counts as unset per the
/// basedir spec, and a relative value falls back too — always absolute,
/// `None` when neither yields an absolute root; see [`platform_state_base`].
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn unix_config_base(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    Some(
        fauna_core::platform_ids::xdg_config_root(xdg_config_home, home)?
            .join("fauna")
            .join("sync"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTOR: &str = "aa11223344556677889900aabbccddeeff00112233445566778899aabbccddee";

    #[test]
    fn store_root_derives_the_scoped_store_dir_and_refuses_junk_hex() {
        let root = StoreRoot::at("/base");
        assert_eq!(
            root.store_dir(ACTOR).unwrap(),
            Path::new("/base").join(ACTOR).join(STORE_SUBDIR)
        );
        assert!(root.store_dir("not-hex").is_err());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn unix_base_prefers_xdg_then_home_and_is_always_absolute() {
        // Value pin: the SAME `~/.config/fauna/sync` the in-app GTK
        // `SyncDriver` (`apps/fauna-linux/src/sync.rs::sync_state_dir`) and
        // the sync agent resolve — one root per user per machine.
        assert_eq!(
            unix_config_base(Some("/run/user/1000/xdg".into()), None).unwrap(),
            Path::new("/run/user/1000/xdg/fauna/sync")
        );
        assert_eq!(
            unix_config_base(None, Some("/home/alice".into())).unwrap(),
            Path::new("/home/alice/.config/fauna/sync")
        );
        // Empty and relative values count as unset.
        assert_eq!(
            unix_config_base(Some("".into()), Some("/home/alice".into())).unwrap(),
            Path::new("/home/alice/.config/fauna/sync")
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn unix_base_refuses_a_missing_empty_or_relative_home_instead_of_tmp() {
        // The old `/tmp/.config/fauna/sync` fallback is a
        // world-writable base another local account can pre-create.
        for home in [None, Some("".into()), Some("rel".into())] {
            assert_eq!(unix_config_base(None, home.clone()), None);
            assert_eq!(unix_config_base(Some("rel".into()), home), None);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_base_is_the_user_domain_sync_dir_and_always_absolute() {
        assert_eq!(
            macos_user_domain_base(Some("/Users/alice".into())).unwrap(),
            Path::new("/Users/alice/Library/Application Support/Fauna/sync")
        );
        // The one place the base must NEVER resolve: the TCC-protected
        // app-group container (a launchd agent is prompted per instance there,
        // and no user decision binds the next one — the 2026-08-25 record).
        assert!(
            !macos_user_domain_base(Some("/Users/alice".into()))
                .unwrap()
                .to_string_lossy()
                .contains("Group Containers")
        );
        // A missing/empty/relative home is refused — no `/tmp` base.
        for home in [None, Some("".into()), Some("rel".into())] {
            assert_eq!(macos_user_domain_base(home), None);
        }
    }

    #[test]
    fn platform_state_base_is_absolute() {
        assert!(platform_state_base().unwrap().is_absolute());
    }

    #[test]
    fn the_refusal_names_the_cause() {
        assert!(NO_HOME_REFUSAL.contains("refusing"));
    }
}
