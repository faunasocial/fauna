//! UniFFI façade for **account-scoped client state placement** — where a client
//! puts the files that belong to one identity, and how they are erased
//! (`docs/goal/architecture/apps/account-scoping.md` § The scoping taxonomy,
//! class 1 + § Erasure follows scope).
//!
//! The registry next door ([`crate::accounts_registry`]) answers *which*
//! identities this install holds; this module answers *where each one's data
//! lives*, for the class-1 stores that are not the credential namespace: MLS
//! state, content caches, backup-coordinator state. Every shim delegates to the
//! ONE shared derivation in `fauna_sync_engine::db` — the same rule the sync
//! engine, the Apple File Provider extension and the macOS sync agent already
//! resolve — so no process can invent a second layout.
//!
//! Deliberately NOT gated on any sync/file-provider feature: every app has
//! account-scoped stores, only some have sync hosts. It rides `accounts-registry`
//! (default-on, dropped only by the Go mail-bridge build, which holds no user
//! identity's local stores).

use std::path::{Path, PathBuf};

use crate::FfiError;

/// The one shared per-actor state-dir derivation: `<base>/<actor-id-hex>/`
/// (`account-scoping.md` § The scoping taxonomy — the placement every class-1
/// datum takes). `base` is the platform's state root for the store in question
/// (apple's `~/Library/Application Support/Fauna`, the app-group `sync` dir,
/// windows' `%LocalAppData%\Fauna`, …); the hex is normalized to lowercase and
/// must be exactly 64 hex chars, so a malformed id is refused rather than
/// silently producing a stray directory next to the real ones.
///
/// Pure — derives a path, creates nothing.
#[uniffi::export]
pub fn account_state_dir(base_dir: String, actor_id_hex: String) -> Result<String, FfiError> {
    fauna_sync_engine::db::actor_state_dir(&PathBuf::from(base_dir), &actor_id_hex)
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| FfiError::General {
            msg: format!("account state dir derivation failed: {e:#}"),
        })
}

/// Erase ONE account's scoped stores under `base_dir` **and its W3 (account-data-plane.md § Workstreams) account
/// store under the W6 unified root** — the "remove this account from this
/// install" affordance. Touches nothing outside that actor's scoped dirs; an
/// account that never built either is a no-op, not an error.
///
/// ⚠ **The second root is not an extra, it is the bug this signature exists to
/// prevent** — see [`account_state_erase_all_scopes`] for the measured failure.
#[uniffi::export]
pub fn account_state_erase_scope(
    base_dir: String,
    actor_id_hex: String,
    store_container_dir: Option<String>,
) -> Result<(), FfiError> {
    erase_scope_under(
        &PathBuf::from(base_dir),
        store_root_for(store_container_dir.map(PathBuf::from)).as_path(),
        &actor_id_hex,
    )
}

/// The pure half of [`account_state_erase_scope`], both roots passed in — the
/// env-mutation-free testing shape tui's `account_scope::erase_under` takes,
/// and for the same reason: the production root is the developer's own
/// `~/.config/fauna/sync`, so a test that resolved it would erase real data.
fn erase_scope_under(
    app_base: &Path,
    store_root: &Path,
    actor_id_hex: &str,
) -> Result<(), FfiError> {
    fauna_sync_engine::db::erase_account_scope(app_base, actor_id_hex).map_err(|e| {
        FfiError::General {
            msg: format!("account state scope erase failed: {e:#}"),
        }
    })?;
    fauna_sync_engine::db::erase_account_scope(store_root, actor_id_hex).map_err(|e| {
        FfiError::General {
            msg: format!("account store scope erase failed: {e:#}"),
        }
    })
}

/// The W6 unified per-user account-store root — a **sibling** of an app's own
/// base, never a child of it, which is the whole reason the erases above take
/// it separately (`account-data-plane.md` § The account store).
///
/// Resolved here rather than taken as a parameter: it is a per-OS constant no
/// human chooses (`principles.md`'s bucket 1), and the one shared resolver is
/// what keeps every process on this machine opening the SAME root under the
/// machine-shared writer key.
///
/// ⚠ **The sandboxed-shell case arrived with android's host (2026-08-22)**, as
/// this comment predicted it would. A shell that hosts its runtime from a
/// container passes the same container here, and it becomes the root verbatim;
/// everyone else passes `None` and resolves `platform()` exactly as before.
///
/// The two resolutions are deliberately identical to
/// `fauna_client_account_runtime`'s (`Some(dir) => StoreRoot::at(dir)`,
/// `None => StoreRoot::platform()`), because an erase that resolved a different
/// root than the runtime opened is the stranding bug in
/// [`account_state_erase_all_scopes`] by another route: it would sweep a
/// directory the store was never in, leave the real store behind, and destroy
/// the writer key alongside. `store_root_for` is the one place that mapping
/// lives, and the tests below pin both arms.
pub(crate) fn store_root_for(store_container_dir: Option<PathBuf>) -> PathBuf {
    match store_container_dir {
        Some(dir) => fauna_sync_engine::root::StoreRoot::at(dir),
        None => fauna_sync_engine::root::StoreRoot::platform(),
    }
    .base()
    .to_path_buf()
}

/// The per-user, per-OS **platform state base** — `StoreRoot::platform()`'s
/// root, as shared Rust resolves it for the calling process
/// (`fauna_account_store::root::platform_state_base`;
/// `account-data-plane.md` § The account store → Placement): windows
/// `%LOCALAPPDATA%\Fauna\sync`, linux `<config root>/fauna/sync`, **macOS
/// `~/Library/Application Support/Fauna/sync`** — the user-domain root the
/// launchd `fauna-sync-agent` and fauna-tui already resolve, and therefore the
/// ONE derivation the macOS app's own sync-state home takes (`SyncStateDir`):
/// the app reads the agent-hosted sets' `file_states` out of this root, and a
/// second Swift spelling of it is exactly how the two could drift apart
/// (`on-demand-files.md` § Apple File Provider binding, *state unification* —
/// the user domain is one of macOS's two consent domains; the other is the
/// sandboxed File Provider extension's app-group container, which no Rust
/// process resolves state under any more).
///
/// Meant for unsandboxed desktop shells. A sandboxed shell (iOS) keeps its
/// container: there this would resolve inside the sandbox, away from the
/// extension that must share the root. Pure resolution — creates nothing;
/// always absolute (the Rust owner's launchd-cwd guarantee).
#[uniffi::export]
pub fn platform_state_base() -> String {
    fauna_sync_engine::root::platform_state_base()
        .to_string_lossy()
        .into_owned()
}

/// What a sign-out's erase removed, what it could **not**, and the one view a
/// seat may paint from — the FFI face of `fauna_sync_engine::db::EraseSweep`
/// beside `fauna_client_accounts::EraseResidueView`.
///
/// **Why the erase hands back a record rather than throwing.** Until 2026-09-09
/// [`account_state_erase_all_scopes`] returned `Result<u32, _>` with the
/// survivors encoded into the error message, and all three FFI seats did the
/// same thing with it: catch, log at warn, and paint a clean "Signed out" over a
/// device that still held the user's readable data. The outcome landed in a
/// `catch` precisely because it arrived as an exception; handing back the sweep
/// puts it on the success path, which is where the paint is.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiEraseSweep {
    /// How many account scopes were removed **across both roots** — an account
    /// that had built state in each counts twice, so this is a progress signal,
    /// never a head-count of accounts.
    pub erased: u32,
    /// The paths that would not go, for the host's **log**. ⚠ Never the user's
    /// line: `%LOCALAPPDATA%\Fauna\<64-hex>\mls.db` is neither the fact nor the
    /// remedy a user is owed (`account-scoping.md` § Erasure follows scope →
    /// *the count goes to the user; the paths go to the log*).
    pub survivors: Vec<String>,
    /// The same outcome reduced to what may reach the user. Carried here rather
    /// than left for a seat to build, so that no seat has to decide what
    /// "clean" means or can assemble the view out of the wrong number.
    pub residue: FfiEraseResidueView,
}

/// The erase's outcome as a surface may see it — the shared
/// `fauna_client_accounts::EraseResidueView`, with its render gate already
/// answered.
///
/// **It carries no paths, and that is the boundary rather than an omission**: a
/// type that cannot express a path cannot leak one onto a user's screen. An app
/// hands the sweep it rides in to [`crate::sign_out_residue_record`] and paints
/// what comes back — it does not phrase, classify, or threshold anything.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FfiEraseResidueView {
    /// How many paths the sweep tried to remove and could not.
    pub survivors: u32,
    /// Whether the credential erase left the user's sign-in credentials on
    /// this device — never set by a seat. `false` on a view straight out of the
    /// filesystem sweep, which cannot know; the credential half is folded in by
    /// [`crate::sign_out_residue_record`].
    pub credentials_survived: bool,
    /// Whether the erase left the user's data on this device — the shared
    /// `EraseResidueView::owes_work`, and the render gate for any retry control.
    /// Derived here so that no seat re-derives `> 0` in a fourth language.
    pub owes_work: bool,
}

impl From<fauna_client_accounts::EraseResidueView> for FfiEraseResidueView {
    fn from(view: fauna_client_accounts::EraseResidueView) -> Self {
        Self {
            survivors: view.survivors,
            credentials_survived: view.credentials_survived,
            owes_work: view.owes_work(),
        }
    }
}

impl From<FfiEraseResidueView> for fauna_client_accounts::EraseResidueView {
    /// `owes_work` is dropped, not trusted: it is re-derived from the two
    /// fields on the way back out, so a view a seat built by hand cannot carry
    /// a render gate that disagrees with its own facts.
    fn from(view: FfiEraseResidueView) -> Self {
        Self {
            survivors: view.survivors,
            credentials_survived: view.credentials_survived,
        }
    }
}

/// What the credential half of a sign-out could not remove — the FFI face of
/// `fauna_client_accounts::CredentialSweep`, handed back by
/// `FfiAccountRegistry::clear_all`.
///
/// **A seat never reads it to decide anything.** It hands it to
/// [`crate::sign_out_residue_record`] beside the filesystem sweep, and logs
/// `survivors`; the shared record owns what "clean" means.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq, Eq)]
pub struct FfiCredentialSweep {
    /// Credential keys (`fauna/<actor>/secret`, `fauna/index`, …) still readable
    /// after the erase — for the host's **log**, never the user's line.
    pub survivors: Vec<String>,
    /// A wholesale namespace wipe reported failure. The FFI seats run none (the
    /// foreign store cannot enumerate), so this is `false` on every sweep they
    /// receive today; it is carried so the conversion is lossless.
    pub wipe_failed: bool,
}

impl From<fauna_client_accounts::CredentialSweep> for FfiCredentialSweep {
    fn from(sweep: fauna_client_accounts::CredentialSweep) -> Self {
        Self {
            survivors: sweep.survivors,
            wipe_failed: sweep.wipe_failed,
        }
    }
}

impl From<FfiCredentialSweep> for fauna_client_accounts::CredentialSweep {
    fn from(sweep: FfiCredentialSweep) -> Self {
        Self {
            survivors: sweep.survivors,
            wipe_failed: sweep.wipe_failed,
        }
    }
}

/// Fold several sweeps into the ONE answer a user is owed about their device —
/// the FFI face of `EraseSweep::absorb`.
///
/// For a host whose app base is not a single directory: android erases under
/// both `filesDir` and the Room database dir, so it calls [`account_state_erase_all_scopes`] twice and would
/// otherwise have to add up the survivors itself. Adding up counts is where a
/// seat re-derives `owes_work` in a fourth language, so the fold lives here and
/// the residue view comes back rebuilt from the folded total.
// Provenance.
#[uniffi::export]
pub fn erase_sweep_fold(sweeps: Vec<FfiEraseSweep>) -> FfiEraseSweep {
    let mut folded = fauna_sync_engine::db::EraseSweep::default();
    for sweep in sweeps {
        folded.absorb(fauna_sync_engine::db::EraseSweep {
            erased: sweep.erased as usize,
            survivors: sweep.survivors.iter().map(PathBuf::from).collect(),
        });
    }
    folded.into()
}

impl From<fauna_sync_engine::db::EraseSweep> for FfiEraseSweep {
    fn from(sweep: fauna_sync_engine::db::EraseSweep) -> Self {
        // Saturating rather than truncating, for the same reason
        // `EraseResidueView::from_survivor_count` saturates: a wrapped count is
        // a *smaller* number, the one direction that under-reports what the
        // sweep did.
        let view =
            fauna_client_accounts::EraseResidueView::from_survivor_count(sweep.survivors.len());
        Self {
            erased: u32::try_from(sweep.erased).unwrap_or(u32::MAX),
            survivors: sweep
                .survivors
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            residue: view.into(),
        }
    }
}

/// Erase **every** account's scoped stores under `base_dir` *and* every W3 account store under the W6 unified root — the
/// all-accounts sign-out erase (`account-scoping.md` § Erasure follows scope,
/// which extends `long-term-store.md`'s cleanup contract from the credential
/// namespace to the content stores).
///
/// ⚠ **"Every" includes the store that does NOT live under `base_dir`, and
/// missing it is worse than stale bytes.** After W6 the account store sits at
/// the shared per-user root — a *sibling* of the app's base, not a child — so
/// an erase iterating `base_dir` alone leaves it behind. The credential
/// namespace this erase accompanies is exactly where that store's Ed25519
/// writer key lives, so the stranded store then **refuses every later sign-in**
/// ("account store belongs to a different writer") and the app runs with no
/// account runtime at all, silently, for good: every store-backed surface
/// fails and the account's own scopes go unwalked, with nothing on screen
/// saying why. Measured on tui 2026-08-18 (78 ms from *minted this machine's
/// store writer key* to *assembly failed*) and fixed there; this is the same
/// fix at the seat that inherits the duty.
///
/// Entries that are not an actor scope survive, which is exactly how
/// install-scoped state (log files, host-keyed TOFU pin stores) is preserved
/// through a sign-out.
///
/// ⚠ **A non-empty [`FfiEraseSweep::survivors`] means data of the user's is
/// STILL ON THIS DEVICE**, and it is the caller's job to say so. A seat that
/// turns that into a log line and then paints a clean "Signed out" is telling
/// the user something untrue — `principles.md` § The user always controls their
/// data puts the delete affordance in the app, and a log line is not an
/// affordance. The paths go to the log; the sweep goes to
/// [`crate::sign_out_residue_record`] and thence to the `sign-out-residue` view.
///
/// ⚠ **This does not fail, and that is deliberate.** It returned
/// `Result<u32, FfiError>` until 2026-09-09, with the survivors encoded into the
/// error message — which is exactly why every seat handled the outcome in a
/// `catch` and painted success on the path that fell through. The sweep itself
/// has no failure mode: it attempts both roots, reports what would not go, and
/// an `Err` arm that cannot occur would be a lie about which outcome is
/// exceptional. Empty `survivors` is the only outcome that means the device is
/// clean.
// ⚠ The provenance span below sits on a `//` line and NOT in the `///` run
// above, deliberately: the UniFFI API checksum covers docstrings, so a span the
// publish transform excises from a doc comment changes the checksum in the
// public tree and panics the tracked Go binding at runtime init. A `//` line is
// excised too, and harmlessly (the `publish-hygiene` gate catches the other).
// Provenance.
#[uniffi::export]
pub fn account_state_erase_all_scopes(
    base_dir: String,
    store_container_dir: Option<String>,
) -> FfiEraseSweep {
    erase_all_scopes_under(
        &PathBuf::from(base_dir),
        store_root_for(store_container_dir.map(PathBuf::from)).as_path(),
    )
    .into()
}

/// The pure half of [`account_state_erase_all_scopes`], both roots passed in —
/// same reasoning as [`erase_scope_under`].
fn erase_all_scopes_under(app_base: &Path, store_root: &Path) -> fauna_sync_engine::db::EraseSweep {
    // BOTH roots are attempted, and neither one's survivors end the sweep. These
    // are two independent directory trees; a `?` here made one undeletable file
    // under the app base cost the store root its erase entirely — so a Windows
    // sign-out that could not remove `mls.db` also left the whole account store
    // behind, in a root nothing had even tried to touch. Erasing what CAN be erased is strictly better
    // for a user walking away from their data.
    //
    // Since 2026-09-09 the shared sweep does not stop early *within* a root
    // either, and it returns the paths that survived instead of one opaque
    // error — so the two roots simply fold into one report.
    let mut sweep = fauna_sync_engine::db::erase_all_account_scopes(app_base);
    sweep.absorb(fauna_sync_engine::db::erase_all_account_scopes(store_root));
    sweep
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTOR_A: &str = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
    const ACTOR_B: &str = "bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22";

    /// Seed one actor's scope under `base` with a file, so an erase that skips
    /// the root leaves something a test can see.
    fn seed(base: &Path, actor: &str, name: &str) -> PathBuf {
        let dir = base.join(actor);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(name);
        std::fs::write(&file, b"bytes").unwrap();
        file
    }

    /// Sign-out reaches the W6 store root, not only the app's own base.
    ///
    /// ⚠ This is the exact bug measured on tui 2026-08-18, red-verified here by
    /// reverting to the single-root call: the app base is wiped, the store
    /// survives, and because the credential namespace wiped alongside holds the
    /// store's writer key, the NEXT sign-in mints a fresh key and the surviving
    /// store refuses it — permanently, silently, with the app running without
    /// its account runtime. Nothing downstream is louder than this assertion.
    #[test]
    fn the_all_accounts_erase_reaches_the_w6_store_root() {
        let tmp = tempfile::tempdir().unwrap();
        let app_base = tmp.path().join("Fauna");
        let store_root = tmp.path().join("Fauna").join("sync");
        let app_a = seed(&app_base, ACTOR_A, "mls.db");
        let store_a = seed(&store_root, ACTOR_A, "account-store.sqlite");
        let store_b = seed(&store_root, ACTOR_B, "account-store.sqlite");

        let sweep = erase_all_scopes_under(&app_base, &store_root);
        assert!(
            sweep.is_clean(),
            "nothing should survive: {:?}",
            sweep.survivors
        );

        assert!(!app_a.exists(), "the app's own scope goes");
        assert!(
            !store_a.exists(),
            "the W3 account store goes too — leaving it strands a store whose writer \
             key the accompanying credential wipe just destroyed"
        );
        assert!(!store_b.exists(), "every account, not just one");
        // 1 under the app base + 2 under the store root.
        assert_eq!(sweep.erased, 3);
    }

    /// A root that CANNOT be erased must not cost the other root its erase.
    ///
    /// The app base used to be a `?`, so one undeletable file under it returned
    /// early and the W6 store root was never even attempted — two independent
    /// trees, one shared fate. On Windows that is not hypothetical: an open handle
    /// makes a file undeletable, so a sign-out that could not close `mls.db` left
    /// the account store behind as well. The error is
    /// still returned — the caller must not think the erase was clean — but it is
    /// returned *after* both roots have been tried.
    ///
    /// Windows-only because that is the only platform where the precondition is
    /// reachable: POSIX `unlink` removes an open file, so there is no portable way
    /// to make one of these roots fail.
    #[cfg(windows)]
    #[test]
    fn an_undeletable_app_scope_does_not_spare_the_store_root() {
        let tmp = tempfile::tempdir().unwrap();
        let app_base = tmp.path().join("Fauna");
        let store_root = tmp.path().join("Fauna").join("sync");
        let pinned = seed(&app_base, ACTOR_A, "mls.db");
        let store_a = seed(&store_root, ACTOR_A, "account-store.sqlite");

        // Hold the app-base file open with NO sharing — `share_mode(0)`, not a
        // plain `File::open`. Rust's `remove_dir_all` deletes through an ordinary
        // read handle on modern Windows (it opens with POSIX semantics), so a plain
        // open makes this test pass for the wrong reason; a live SQLite connection
        // is the restrictive kind, which is why production saw `os error 32`.
        use std::os::windows::fs::OpenOptionsExt;
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&pinned)
            .unwrap();

        let sweep = erase_all_scopes_under(&app_base, &store_root);
        assert!(
            !sweep.is_clean(),
            "an undeletable scope must still be reported"
        );

        assert!(
            pinned.exists(),
            "precondition: the open handle is what makes this scope survive"
        );
        assert!(
            !store_a.exists(),
            "the store root is a SEPARATE tree and must still be erased — leaving \
             it because a different root failed is how a signed-out user's account \
             store survived a sign-out entirely"
        );
        // Since 2026-09-09 the sweep names the surviving PATH rather than
        // handing back a message about "the app base" — which is what lets a
        // seat tell the user *what* is still on their device.
        assert!(
            sweep
                .survivors
                .iter()
                .any(|p| p == &pinned || pinned.starts_with(p)),
            "the survivor list must name the pinned scope: {:?}",
            sweep.survivors
        );
        drop(handle);
    }

    /// The single-account form owes the same second root.
    #[test]
    fn removing_one_account_reaches_its_store_and_leaves_the_others() {
        let tmp = tempfile::tempdir().unwrap();
        let app_base = tmp.path().join("Fauna");
        let store_root = tmp.path().join("Fauna").join("sync");
        let store_a = seed(&store_root, ACTOR_A, "account-store.sqlite");
        let store_b = seed(&store_root, ACTOR_B, "account-store.sqlite");
        seed(&app_base, ACTOR_A, "mls.db");

        erase_scope_under(&app_base, &store_root, ACTOR_A).unwrap();

        assert!(!store_a.exists(), "the removed account's store goes");
        assert!(
            store_b.exists(),
            "a sibling account on this install keeps its store — removing one account \
             is not a sign-out"
        );
    }

    /// A sandboxed host erases under the container it hosts from, not under
    /// `platform()`.
    ///
    /// ⚠ This is the same stranding bug as
    /// `the_all_accounts_erase_reaches_the_w6_store_root`, one level up: on
    /// android and iOS `platform()` resolves `$HOME/.config/fauna/sync`, which
    /// inside the sandbox is unwritable or plain wrong, so an erase that
    /// resolved it would sweep a directory the store was never in and leave the
    /// real one — writer key destroyed, every later sign-in refused.
    ///
    /// The resolution MUST agree with the one the runtime starts under
    /// (`fauna_client_account_runtime`: `Some(dir) => StoreRoot::at(dir)`,
    /// `None => StoreRoot::platform()`); the two drifting apart is the same
    /// stranding by another route, so this asserts the container arrives
    /// verbatim rather than re-deriving anything from it.
    #[test]
    fn a_sandboxed_host_erases_under_the_container_it_hosts_from() {
        let container = PathBuf::from("/data/user/0/social.fauna.fauna/files/sync");
        assert_eq!(
            store_root_for(Some(container.clone())),
            container,
            "the container the shell supplies IS the per-user root there"
        );
    }

    /// With no container the resolution is unchanged — the desktop path every
    /// existing caller takes.
    #[test]
    fn no_container_still_resolves_the_platform_root() {
        assert_eq!(
            store_root_for(None),
            fauna_sync_engine::root::StoreRoot::platform()
                .base()
                .to_path_buf(),
        );
    }

    /// A root that never existed is a no-op, not an error — the fresh-install
    /// and never-hosted-a-runtime cases both take this path on every sign-out.
    #[test]
    fn absent_roots_are_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        let sweep = erase_all_scopes_under(&tmp.path().join("nope"), &tmp.path().join("also-nope"));
        assert_eq!(sweep.erased, 0);
        assert!(
            sweep.is_clean(),
            "a root that never existed leaves no survivor: {:?}",
            sweep.survivors
        );
        erase_scope_under(
            &tmp.path().join("nope"),
            &tmp.path().join("also-nope"),
            ACTOR_A,
        )
        .unwrap();
    }

    // ------------------------------------------------------------------
    // The widened seam: what the three FFI seats are handed
    // ------------------------------------------------------------------

    /// The line the residue surface paints for `view` — the shared copy the
    /// `fauna-ffi` residue face hands every FFI seat.
    fn line_for(view: FfiEraseResidueView) -> Option<fauna_core::localized::LocalizedText> {
        fauna_client_accounts::EraseResidueView::from(view)
            .copy()
            .warning
    }

    /// A clean sweep says **nothing** — the rule that keeps *"0 items were left
    /// behind"* off a successful sign-out. `owes_work` is the render gate, and
    /// on this arm it must be false all the way through the copy.
    #[test]
    fn a_clean_sweep_owes_no_work_and_carries_no_line() {
        let tmp = tempfile::tempdir().unwrap();
        let app_base = tmp.path().join("Fauna");
        let store_root = tmp.path().join("Fauna").join("sync");
        seed(&app_base, ACTOR_A, "mls.db");
        seed(&store_root, ACTOR_B, "account-store.sqlite");

        let ffi = FfiEraseSweep::from(erase_all_scopes_under(&app_base, &store_root));
        assert_eq!(ffi.erased, 2, "both scopes went");
        assert!(ffi.survivors.is_empty(), "{:?}", ffi.survivors);
        assert!(!ffi.residue.owes_work);
        assert_eq!(ffi.residue.survivors, 0);
        assert_eq!(
            line_for(ffi.residue),
            None,
            "a clean sign-out must not reassure by vacuity"
        );
    }

    /// The defect the widening exists for: a scope that will not go must reach
    /// the seat as a **survivor it can paint**, not as an exception it will
    /// catch and log. The count goes to the user's line; the path does not.
    ///
    /// Fault injection is a **read-only scope dir**, not a held handle — POSIX
    /// `unlink` removes an open file, so the `share_mode(0)` shape that pins
    /// this on Windows above proves nothing here. Same shape as the shared
    /// sweep's own pin,
    /// `fauna_account_store::db::a_scope_that_will_not_go_is_reported_and_does_not_abandon_the_others`:
    /// deny the write bit on the scope's OWN directory, so the `mls.db` inside
    /// it cannot be unlinked and the user's readable data genuinely survives.
    #[cfg(unix)]
    #[test]
    fn a_scope_that_will_not_go_reaches_the_seat_as_a_paintable_survivor() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let app_base = tmp.path().join("Fauna");
        let store_root = tmp.path().join("Store").join("sync");
        let pinned = seed(&app_base, ACTOR_A, "mls.db");
        let scope = pinned.parent().unwrap().to_path_buf();
        let store_b = seed(&store_root, ACTOR_B, "account-store.sqlite");
        std::fs::set_permissions(&scope, std::fs::Permissions::from_mode(0o555)).unwrap();

        // Probe the injection with a real removal rather than reading a mode:
        // a process that writes through a read-only dir (root) would make every
        // assertion below pass for the wrong reason, and a test that silently
        // proves nothing is worse than no test.
        if std::fs::remove_file(&pinned).is_ok() {
            std::fs::set_permissions(&scope, std::fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skipping: this process can write through a read-only dir (root?)");
            return;
        }

        let ffi = FfiEraseSweep::from(erase_all_scopes_under(&app_base, &store_root));
        std::fs::set_permissions(&scope, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            pinned.exists(),
            "precondition: the read-only scope is what makes this data survive"
        );
        assert!(
            !store_b.exists(),
            "the other root is a separate tree and must still have been swept"
        );

        assert_eq!(ffi.survivors.len(), 1, "{:?}", ffi.survivors);
        assert!(
            ffi.survivors[0].contains(ACTOR_A),
            "the log gets the PATH: {:?}",
            ffi.survivors
        );
        assert!(ffi.residue.owes_work, "the user is owed a line");
        assert_eq!(
            ffi.residue.survivors as usize,
            ffi.survivors.len(),
            "the count the user is shown is the length of the list the log gets — \
             assembling the view out of the wrong number is exactly what carrying \
             it beside the paths is meant to prevent"
        );

        let line = line_for(ffi.residue).expect("a survivor owes the user a line");
        assert_eq!(
            line.args.get("count").map(String::as_str),
            Some("1"),
            "the count reaches the line"
        );
        assert!(
            !line.args.values().any(|v| v.contains(ACTOR_A)),
            "the survivor PATH belongs in the log, never on the user's line: {:?}",
            line.args
        );
    }

    /// A host with more than one app base gets ONE answer about its device, and
    /// the folded residue is rebuilt from the total rather than added up by the
    /// seat — the fold is where `owes_work` would otherwise be re-derived in a
    /// fourth language.
    #[test]
    fn folding_several_sweeps_rebuilds_the_residue_from_the_total() {
        let clean = FfiEraseSweep {
            erased: 2,
            survivors: vec![],
            residue: FfiEraseResidueView {
                survivors: 0,
                credentials_survived: false,
                owes_work: false,
            },
        };
        let dirty = FfiEraseSweep {
            erased: 1,
            survivors: vec!["/nowhere/aa11/mls.db".to_string()],
            residue: FfiEraseResidueView {
                survivors: 1,
                credentials_survived: false,
                owes_work: true,
            },
        };

        let folded = erase_sweep_fold(vec![clean.clone(), dirty.clone()]);
        assert_eq!(folded.erased, 3);
        assert_eq!(folded.survivors, dirty.survivors);
        assert_eq!(folded.residue.survivors, 1);
        assert!(
            folded.residue.owes_work,
            "one dirty root makes the whole device dirty — a clean sibling root \
             must not talk the answer back down"
        );

        // Two dirty roots are two items on the user's line, not two lines.
        let other_root = FfiEraseSweep {
            erased: 0,
            survivors: vec!["/elsewhere/aa11/mls.db".to_string()],
            residue: FfiEraseResidueView {
                survivors: 1,
                credentials_survived: false,
                owes_work: true,
            },
        };
        let both = erase_sweep_fold(vec![dirty.clone(), other_root]);
        assert_eq!(both.residue.survivors, 2);
        assert_eq!(
            line_for(both.residue)
                .expect("owing")
                .args
                .get("count")
                .map(String::as_str),
            Some("2")
        );

        // The SAME location reported by two sweeps is one fact about the
        // device, not two: `EraseSweep::absorb` dedups survivors by path, and
        // the count on the user's line is that deduped length (android folds
        // two roots, and a survivor two sweeps both saw is one place).
        let twice = erase_sweep_fold(vec![dirty.clone(), dirty]);
        assert_eq!(twice.survivors.len(), 1, "{:?}", twice.survivors);
        assert_eq!(twice.residue.survivors, 1);

        // Nothing to fold is clean, not an error — the fresh-install sign-out.
        assert!(!erase_sweep_fold(vec![]).residue.owes_work);
    }

    /// A seat-built view cannot smuggle in a render gate that disagrees with
    /// its facts: `owes_work` is re-derived on the way back into shared Rust.
    #[test]
    fn a_hand_built_render_gate_is_re_derived_not_trusted() {
        let lying = FfiEraseResidueView {
            survivors: 0,
            credentials_survived: true,
            owes_work: false,
        };
        assert!(line_for(lying).is_some());
        assert!(fauna_client_accounts::EraseResidueView::from(lying).owes_work());
    }
}
