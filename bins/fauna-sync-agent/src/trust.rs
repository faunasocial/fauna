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
/// instead (`<root>/trust`, the spawner's `agent_trust_dir`): a test launch
/// must never read or write the box's real install-scoped state (convention
/// 10). The app that spawned it writes its own data dir, so it keeps a read
/// replica of its pin file there (`agent_spawner::spawned_agent_pin_replica_dir`,
/// `transport-trust.md` § Pin custody across processes, *an isolated agent
/// reads its app's replica*).
///
/// `Err` (the shared no-home refusal) when the production layout has no
/// absolute home — the same refusal [`SyncPaths::new`] carries.
pub fn trust_dir(data_dir: Option<&Path>) -> anyhow::Result<PathBuf> {
    Ok(SyncPaths::new(data_dir.map(PathBuf::from))?.trust_dir())
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
///
/// `dir` is [`SyncPaths::trust_dir`] (see [`trust_dir`]), resolved once at
/// startup with the rest of the agent's paths.
pub fn install_consumer_pin_store(dir: &Path) {
    tracing::info!(
        trust_dir = %dir.display(),
        "installing read-only nest-identity pin store (the app is the sole minter)"
    );
    fauna_client::trust::install_pin_store(Arc::new(
        fauna_client::cert_binding::ReadOnlyDiskPinStore::open_in_dir(dir),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_data_dir_keeps_the_store_inside_that_root() {
        assert_eq!(
            trust_dir(Some(Path::new("/tmp/run-42"))).unwrap(),
            PathBuf::from("/tmp/run-42/trust")
        );
    }

    /// The e2e-isolated agent reads the pins its app mints (`transport-trust.md`
    /// § Pin custody across processes, *an isolated agent reads its app's
    /// replica*). Under `--data-dir` this agent reads `<root>/trust`, never the
    /// box's trust home, while the app writes its own data dir — so the app's
    /// writer store keeps a read replica where this agent reads. The replica
    /// dir is the spawner's derivation (`agent_pin_replica_dir_for`, the one
    /// the windows app and tui consult) and the read dir is this crate's
    /// [`trust_dir`]: a pin the writer mints must be the pin the consumer
    /// returns, or the agent waits on `PinRequired` for good against every
    /// self-signed nest its app trusted.
    #[test]
    fn a_pin_the_app_mints_reaches_an_agent_on_a_data_dir() {
        use fauna_client::cert_binding::{
            DiskPinStore, NestIdentityPinStore, ReadOnlyDiskPinStore,
        };

        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("app-data");
        let agent_root = tmp.path().join("agent-data");
        std::fs::create_dir_all(&app_dir).unwrap();

        let replica = fauna_client_sync::agent_spawner::agent_pin_replica_dir_for(Some(
            agent_root.as_os_str(),
        ));
        let writer = DiskPinStore::open_in_dir_mirrored_to(&app_dir, replica.as_deref());
        let consumer = ReadOnlyDiskPinStore::open_in_dir(&trust_dir(Some(&agent_root)).unwrap());

        assert_eq!(consumer.get("nest.example:443"), None, "nothing pinned yet");
        writer.set("nest.example:443", [7; 32]);
        assert_eq!(consumer.get("nest.example:443"), Some([7; 32]));
        writer.remove("nest.example:443");
        assert_eq!(
            consumer.get("nest.example:443"),
            None,
            "the user's re-trust (a removal) reaches the agent too"
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
        let paths = SyncPaths::new(None).expect("this box has an absolute home");
        let trust = paths.trust_dir();
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
        let paths = SyncPaths::at(PathBuf::from("/tmp/run-7"));
        let before = paths.trust_dir();

        paths.set_actor_scope(Some("a1b2c3".into()));
        assert_ne!(
            paths.base_dir(),
            paths.flat_base_dir(),
            "scope moved the base"
        );

        let after = paths.trust_dir();
        assert_eq!(before, after);
    }
}
