//! Install-scoped TLS trust for the background agent (`security.md` § Pin
//! custody across processes).
//!
//! A client installation puts several processes on the wire to the same nest:
//! the interactive app, the apple File Provider extension, and this agent. They
//! share **one** identity pin store — the `known_hosts` of "the nest this
//! installation trusts" — and exactly one of them, the interactive app, may
//! mint or remove a pin. This agent is a **read-only consumer**: it reads what
//! the user's app trusted, and with no pin for a TOFU-rooted (self-signed /
//! LAN) nest its connect fails `PinRequired` and retries until the app has
//! pinned it, rather than silently trusting whatever answered.
//!
//! Until this module existed the agent installed no store at all, so it ran on
//! the process-global `MemoryPinStore` default — which is both empty at every
//! start (a TOFU-rooted nest was simply unreachable from an agent process) and
//! *writable* (it would mint a pin for whatever it reached, with no user in the
//! loop — the shape rule 2 exists to forbid).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::SyncPaths;

/// The install-scoped trust home — where the **interactive app** keeps the pin
/// file this agent reads. Deliberately *not* under the agent's own data dir: a
/// per-process store is a bug twice over (starts empty, then drifts from the
/// app's), which is rule 1 of `security.md` § Pin custody across processes.
///
/// The derivation is the shared
/// [`fauna_client::cert_binding::install_scoped_trust_home`] — ONE function
/// for writer (linux app, tui) and consumer (this agent), which is what makes
/// the alignment structural rather than a table two crates each transcribe
/// (tui once drifted to a per-app `fauna-tui/` store nothing consumed; see the
/// shared fn's doc for the per-platform table).
///
/// An explicit `--data-dir` (tests, e2e) keeps the store **inside** that root
/// instead: a test launch must never read or write the box's real
/// install-scoped state (testing.md § conventions point 10 — the same branch
/// `NestTrust.installPinStore()` takes for `FaunaE2E.isActive`).
pub fn trust_dir(data_dir: Option<&Path>) -> PathBuf {
    trust_dir_for_paths(&SyncPaths::new(data_dir.map(PathBuf::from)))
}

/// Scoped to the *install*, so it must not move when the agent adopts a
/// per-actor scope (which moves `base_dir`, never `flat_base_dir`).
fn trust_dir_for_paths(paths: &SyncPaths) -> PathBuf {
    if paths.is_production() {
        fauna_client::cert_binding::install_scoped_trust_home()
    } else {
        paths.flat_base_dir().join("trust")
    }
}

/// Install the read-only pin store. Call **once**, at agent startup, before the
/// first nest connect — it replaces the process-global in-memory default.
///
/// The directory is deliberately **not** created: the interactive app owns it,
/// and a missing file simply reads back as "nothing pinned", which is the
/// correct fail-closed answer for a consumer (the writer-side
/// `install_nest_identity_pin_store` creates it because a minter needs
/// somewhere to write). The store is uncached, so a pin the app mints after
/// this agent launched is picked up on the very next connect retry — no
/// relaunch, and nothing to re-install here.
pub fn install_consumer_pin_store(data_dir: Option<&Path>) {
    let dir = trust_dir(data_dir);
    tracing::info!(
        trust_dir = %dir.display(),
        "installing read-only nest-identity pin store (the app is the sole minter)"
    );
    fauna_client::trust::install_pin_store(Arc::new(
        fauna_client::cert_binding::ReadOnlyDiskPinStore::open_in_dir(&dir),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_data_dir_keeps_the_store_inside_that_root() {
        assert_eq!(
            trust_dir(Some(Path::new("/tmp/run-42"))),
            PathBuf::from("/tmp/run-42/trust")
        );
    }

    // The per-platform *production* paths — including the cross-language
    // contracts with `NestTrust.sharedTrustDir()` (mac) and `BackupPaths
    // .DataDir` (windows) — are pinned where the one shared derivation lives:
    // `fauna_anon_client::cert_binding`'s `*_trust_home` tests. This module
    // pins only what is agent-local: the e2e override arm (above) and the two
    // structural invariants below.

    /// The failure this whole module exists to prevent: a store rooted in the
    /// agent's own dir starts empty and drifts from the app's (rule 1).
    #[test]
    fn the_production_store_is_never_inside_the_agents_own_root() {
        let paths = SyncPaths::new(None);
        let trust = trust_dir_for_paths(&paths);
        let base = paths.flat_base_dir();
        assert!(
            !trust.starts_with(&base),
            "trust dir {} must not live under the agent's own root {}",
            trust.display(),
            base.display()
        );
    }

    /// The pin store is scoped to the install, so adopting a per-actor scope —
    /// which moves every *other* agent path to `<base>/<actor-hex>` — must not
    /// move it.
    #[test]
    fn a_per_actor_scope_does_not_move_the_trust_dir() {
        let paths = SyncPaths::new(Some(PathBuf::from("/tmp/run-7")));
        let before = trust_dir_for_paths(&paths);

        paths.set_actor_scope(Some("a1b2c3".into()));
        assert_ne!(
            paths.base_dir(),
            paths.flat_base_dir(),
            "scope moved the base"
        );

        let after = trust_dir_for_paths(&paths);
        assert_eq!(before, after);
    }
}
