//! The background agent installs a **read-only** nest-identity pin store over
//! the *install-scoped* trust dir — `docs/goal/architecture/security.md`
//! § Pin custody across processes, rule 1 (one install-scoped store, shared by
//! every process of the install) and rule 2 (only the interactive app mints;
//! every other process is a read-only consumer).
//!
//! Before this, the agent installed no store at all, so it ran on the
//! process-global `MemoryPinStore` default: it re-TOFU'd from scratch on every
//! restart *and* would mint a pin for whatever it reached, with no user in the
//! loop — the exact shape rule 2 forbids.
//!
//! Lives in its own integration-test binary because `trust::install_pin_store`
//! mutates a process-global `OnceLock`, the same reason
//! `libs/fauna-anon-client/tests/read_only_pin_store_consumer.rs` is its own
//! binary. Keep this file to the single `#[test]` below.

use fauna_client::cert_binding::{DiskPinStore, NestIdentityPinStore};

/// The whole custody contract from the agent's side, in one process: it reads
/// what the app minted, sees later mints without a relaunch, and can neither
/// mint nor remove a pin itself.
#[test]
fn the_agent_consumes_the_apps_pins_and_can_never_mint_or_drop_one() {
    let root = tempfile::tempdir().expect("temp data dir");
    let data_dir = root.path();

    // An explicit `--data-dir` keeps the trust store *inside* that root: a test
    // (or e2e) launch must never read or write the box's real install-scoped
    // store — testing.md § conventions point 10, the same branch the apple
    // side takes in `NestTrust.installPinStore()`.
    let trust = fauna_sync_agent::trust::trust_dir(Some(data_dir));
    assert_eq!(trust, data_dir.join("trust"));

    // The interactive app — the sole minter — pins a TOFU-rooted nest.
    std::fs::create_dir_all(&trust).expect("create trust dir");
    let app = DiskPinStore::open_in_dir(&trust);
    let pi = [0x7eu8; 32];
    app.set("pi.local", pi);

    // Agent startup.
    fauna_sync_agent::trust::install_consumer_pin_store(Some(data_dir));

    // Rule 1 — the agent reads the pin the app minted, so a TOFU-rooted nest is
    // reachable from an agent process at all (it was not, before this).
    assert_eq!(
        fauna_client::trust::pinned_identity("pi.local"),
        Some(pi),
        "the agent must read the install's pin store, not an empty per-process one"
    );

    // Rule 2, closing clause — the consumer store is uncached, so a pin the app
    // mints *after* the agent launched is honored on the agent's next connect
    // retry, with no process relaunch.
    let lan = [0x11u8; 32];
    app.set("nest.lan", lan);
    assert_eq!(
        fauna_client::trust::pinned_identity("nest.lan"),
        Some(lan),
        "a pin minted after agent launch must be visible without a relaunch"
    );

    // Rule 2 — the agent can never *remove* a pin. Re-trust is a user decision
    // made where a user is present; a background process dropping the pin would
    // silently re-TOFU the next thing it reached.
    fauna_client::trust::forget_identity_pin("pi.local");
    assert_eq!(
        fauna_client::trust::pinned_identity("pi.local"),
        Some(pi),
        "a consumer process must not be able to drop the user's pin"
    );

    // …and an unpinned host stays unpinned: the dial fails closed with
    // `PinRequired` and retries until the app pins, rather than trusting
    // whatever answered.
    assert_eq!(
        fauna_client::trust::pinned_identity("unpinned.example"),
        None,
        "the agent must not mint a pin for a host the app never trusted"
    );
}
