//! UniFFI face of **the residue surface** — the sign-out's install-scoped
//! residue record and the one re-sweep over it
//! (`account-scoping.md` § Erasure follows scope → *the residue surface*).
//!
//! The record and the four re-sweep rules are shared Rust
//! (`fauna_client_accounts::SignOutResidue` / `retry_sign_out_residue`); tui and
//! linux call them directly. This module is the door the FFI seats (android,
//! apple, windows) take to the same code, so none of them re-decides anything:
//!
//! - [`sign_out_residue_record`] — the sign-out's half: record what the erase
//!   left, persist it under the install base, and hand back the paint.
//! - [`FfiSignOutResidue::retry`] — `sign-out-residue-retry-button`.
//! - [`sign_out_residue_recheck_at_launch`] — the signed-out launch's silent
//!   re-sweep, which paints only if something is still left.
//!
//! The one thing only a seat knows is **its own credential erase** — android's
//! `clear_all` then `SecureStorage.clear()` then `reverify_erase`, apple's and
//! windows' their own — so the retry calls back into it through
//! [`FfiResidueCredentialEraser`] rather than guessing at a platform reset. The
//! read-back of the RECORDED keys is folded in here, because the registry is
//! empty by the time a retry runs and its own read-back would find nothing to
//! ask about (the FFI twin of `fauna_credential_store::re_erase_credentials`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fauna_client_accounts::{CredentialSweep, ResidueRetry, SignOutResidue};
use fauna_core::localized::LocalizedText;

use crate::account_state::{FfiCredentialSweep, FfiEraseSweep, store_root_for};
use crate::erase_guard::{serving_bases, with_own_lock_down};
use crate::{FfiAccountInstanceLock, FfiAccountRegistry};

/// The seat's own credential erase — the one its sign-out ran — re-run by a
/// residue retry whose recorded credential half is not clean.
///
/// Return what the erase left, read back the way the sign-out reads it back
/// (android: `FfiAccountRegistry::clear_all`, then the platform store's
/// wholesale reset, then `FfiAccountRegistry::reverify_erase`). The recorded
/// keys are re-read on top of it by the caller, never by the seat.
#[uniffi::export(with_foreign)]
pub trait FfiResidueCredentialEraser: Send + Sync {
    /// Run the credential erase and report what is still readable.
    fn erase_credentials(&self) -> FfiCredentialSweep;
}

/// A residue that still owes work — the `sign-out-residue` view's whole state.
/// A seat holds one while the view is up and drops it when a retry hands back
/// nothing; it never inspects the record inside.
#[derive(uniffi::Object, Debug)]
pub struct FfiSignOutResidue {
    record: SignOutResidue,
    line: LocalizedText,
}

#[uniffi::export]
impl FfiSignOutResidue {
    /// `sign-out-residue-message` — the shared `Rendered` copy naming Remove
    /// Again, or the retry's own refusal while another instance serves one of
    /// the residue's accounts. Resolve it through the app's i18n pipeline.
    pub fn line(&self) -> LocalizedText {
        self.line.clone()
    }

    /// Remove Again: re-sweep what this residue recorded and return what is
    /// left — `None` when the device is now clean, which closes the view.
    ///
    /// `base_dir` is the install base the sign-out recorded under
    /// ([`sign_out_residue_record`]); `store_container_dir` and `own_lock` are
    /// what this seat passes to `sign_out_blocked`, so the retry asks the
    /// sign-out's own serving-lock question.
    pub fn retry(
        &self,
        registry: Arc<FfiAccountRegistry>,
        base_dir: String,
        store_container_dir: Option<String>,
        own_lock: Option<Arc<FfiAccountInstanceLock>>,
        eraser: Arc<dyn FfiResidueCredentialEraser>,
    ) -> Option<Arc<FfiSignOutResidue>> {
        run_retry(
            &self.record,
            &registry,
            Path::new(&base_dir),
            &store_root_for(store_container_dir.map(PathBuf::from)),
            own_lock.as_deref(),
            eraser.as_ref(),
        )
    }
}

/// The sign-out's half: record what its erase left — the filesystem sweep's
/// survivors and the credential sweep, both already read back — under
/// `base_dir` (the app's install base), and return the view to paint, or `None`
/// when the erase was clean (and no record is kept).
///
/// The line it carries names the Remove Again control every seat paints beside
/// it.
#[uniffi::export]
pub fn sign_out_residue_record(
    base_dir: String,
    sweep: FfiEraseSweep,
    credentials: FfiCredentialSweep,
) -> Option<Arc<FfiSignOutResidue>> {
    let survivors: Vec<PathBuf> = sweep.survivors.iter().map(PathBuf::from).collect();
    let record = SignOutResidue::record(&survivors, &credentials.into());
    persist(&record, Path::new(&base_dir));
    painted(record)
}

/// The signed-out launch's silent re-check: a record a previous sign-out left
/// under `base_dir` is re-swept FIRST, and the view comes back only if
/// something is still left.
///
/// Only on a launch whose registry holds no account — the launch a sign-out
/// hands back. A signed-in launch is not the user the residue was reported to;
/// its record waits, untouched, for the next signed-out launch. Arguments as
/// [`FfiSignOutResidue::retry`].
#[uniffi::export]
pub fn sign_out_residue_recheck_at_launch(
    registry: Arc<FfiAccountRegistry>,
    base_dir: String,
    store_container_dir: Option<String>,
    own_lock: Option<Arc<FfiAccountInstanceLock>>,
    eraser: Arc<dyn FfiResidueCredentialEraser>,
) -> Option<Arc<FfiSignOutResidue>> {
    let base = PathBuf::from(&base_dir);
    let record = SignOutResidue::load(&base)?;
    if !registry.registry().list().is_empty() {
        return None;
    }
    run_retry(
        &record,
        &registry,
        &base,
        &store_root_for(store_container_dir.map(PathBuf::from)),
        own_lock.as_deref(),
        eraser.as_ref(),
    )
}

/// The one re-sweep both gestures run, behind the sign-out's serving-lock
/// question asked about exactly the two bases its erase reaches.
fn run_retry(
    record: &SignOutResidue,
    registry: &FfiAccountRegistry,
    base: &Path,
    store_root: &Path,
    own_lock: Option<&FfiAccountInstanceLock>,
    eraser: &dyn FfiResidueCredentialEraser,
) -> Option<Arc<FfiSignOutResidue>> {
    let outcome = with_own_lock_down(own_lock, || {
        fauna_client_accounts::retry_sign_out_residue(
            record,
            serving_bases(base, store_root),
            |recorded| re_erase_credentials(registry, eraser, recorded),
        )
    });
    match outcome {
        ResidueRetry::Blocked(_) => Some(Arc::new(FfiSignOutResidue {
            record: record.clone(),
            line: fauna_client_accounts::sign_out_residue_retry_blocked_copy(),
        })),
        ResidueRetry::Swept(left) => {
            persist(&left, base);
            painted(left)
        }
    }
}

/// The seat's erase, plus a read-back of the keys the sign-out recorded — the
/// registry is empty by now, so its own read-back cannot name them.
fn re_erase_credentials(
    registry: &FfiAccountRegistry,
    eraser: &dyn FfiResidueCredentialEraser,
    recorded: CredentialSweep,
) -> CredentialSweep {
    let fresh: CredentialSweep = eraser.erase_credentials().into();
    let read_back = registry.registry().reverify(CredentialSweep {
        survivors: recorded.survivors,
        wipe_failed: false,
    });
    let mut survivors = fresh.survivors;
    for key in read_back.survivors {
        if !survivors.contains(&key) {
            survivors.push(key);
        }
    }
    CredentialSweep {
        survivors,
        wipe_failed: fresh.wipe_failed,
    }
}

/// The paint for an owing record; `None` for a clean one.
fn painted(record: SignOutResidue) -> Option<Arc<FfiSignOutResidue>> {
    let line = record.view().copy().warning?;
    Some(Arc::new(FfiSignOutResidue { record, line }))
}

/// Save the record — or remove it, when clean. A failed write is logged and not
/// fatal: the view still paints for this process, and the paths are in the log
/// the erase wrote.
fn persist(record: &SignOutResidue, base: &Path) {
    if let Err(e) = record.save(base) {
        tracing::warn!(
            "[sign-out residue] the record under {} could not be written: {e}",
            base.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FfiSecretStore, acquire_account_instance_lock_shared};
    use fauna_client_accounts::SIGN_OUT_RESIDUE_FILE;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ACTOR: &str = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
    const KEY: &str = "fauna/aa11/secret";

    #[derive(Default)]
    struct MapStore(Mutex<HashMap<String, String>>);

    impl FfiSecretStore for MapStore {
        fn get(&self, key: String) -> Option<String> {
            self.0.lock().unwrap().get(&key).cloned()
        }
        fn set(&self, key: String, value: String) {
            self.0.lock().unwrap().insert(key, value);
        }
        fn delete(&self, key: String) {
            self.0.lock().unwrap().remove(&key);
        }
    }

    /// A seat's credential erase: wipes the store when `works`, and counts calls.
    struct Eraser {
        store: Arc<MapStore>,
        works: bool,
        calls: AtomicUsize,
    }

    impl FfiResidueCredentialEraser for Eraser {
        fn erase_credentials(&self) -> FfiCredentialSweep {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.works {
                self.store.0.lock().unwrap().clear();
            }
            FfiCredentialSweep::default()
        }
    }

    /// One signed-out device: an install base, a store container, an empty
    /// registry over `store`.
    struct Device {
        _tmp: tempfile::TempDir,
        base: PathBuf,
        container: PathBuf,
        store: Arc<MapStore>,
        registry: Arc<FfiAccountRegistry>,
    }

    impl Device {
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("temp dir");
            let base = tmp.path().join("app");
            let container = tmp.path().join("store");
            std::fs::create_dir_all(&base).expect("app base");
            std::fs::create_dir_all(&container).expect("container");
            let store = Arc::new(MapStore::default());
            let registry = FfiAccountRegistry::new(store.clone());
            Self {
                _tmp: tmp,
                base,
                container,
                store,
                registry,
            }
        }

        fn base(&self) -> String {
            self.base.to_string_lossy().into_owned()
        }

        fn container(&self) -> Option<String> {
            Some(self.container.to_string_lossy().into_owned())
        }

        /// An actor scope under the install base holding one file.
        fn scope(&self) -> PathBuf {
            let dir = self.base.join(ACTOR);
            std::fs::create_dir_all(&dir).expect("scope");
            std::fs::write(dir.join("mls.db"), b"the user's data").expect("file");
            dir
        }

        fn sweep(survivors: &[&Path]) -> FfiEraseSweep {
            FfiEraseSweep {
                erased: 0,
                survivors: survivors.iter().map(|p| p.display().to_string()).collect(),
                residue: fauna_client_accounts::EraseResidueView::from_survivor_count(
                    survivors.len(),
                )
                .into(),
            }
        }

        fn eraser(&self, works: bool) -> Arc<Eraser> {
            Arc::new(Eraser {
                store: self.store.clone(),
                works,
                calls: AtomicUsize::new(0),
            })
        }

        fn record_file(&self) -> PathBuf {
            self.base.join(SIGN_OUT_RESIDUE_FILE)
        }

        fn retry(
            &self,
            residue: &FfiSignOutResidue,
            eraser: Arc<Eraser>,
        ) -> Option<Arc<FfiSignOutResidue>> {
            residue.retry(
                self.registry.clone(),
                self.base(),
                self.container(),
                None,
                eraser,
            )
        }

        fn recheck(&self, eraser: Arc<Eraser>) -> Option<Arc<FfiSignOutResidue>> {
            sign_out_residue_recheck_at_launch(
                self.registry.clone(),
                self.base(),
                self.container(),
                None,
                eraser,
            )
        }
    }

    fn rendered_line(survivors: usize, credentials: bool) -> LocalizedText {
        fauna_client_accounts::EraseResidueView::from_survivor_count(survivors)
            .with_credentials(&CredentialSweep {
                survivors: if credentials {
                    vec![KEY.into()]
                } else {
                    vec![]
                },
                wipe_failed: false,
            })
            .copy()
            .warning
            .expect("an owing view has a line")
    }

    #[test]
    fn a_clean_sign_out_paints_nothing_and_keeps_no_record() {
        let device = Device::new();
        let painted = sign_out_residue_record(
            device.base(),
            Device::sweep(&[]),
            FfiCredentialSweep::default(),
        );
        assert!(painted.is_none(), "a clean erase says nothing");
        assert!(!device.record_file().exists());
    }

    /// The sign-out's half: the line is the `Rendered` copy (it names the
    /// button the seat now paints), and the record is on disk for the relaunch.
    #[test]
    fn a_survivor_paints_the_rendered_line_and_is_recorded_on_disk() {
        let device = Device::new();
        let dir = device.scope();
        let painted = sign_out_residue_record(
            device.base(),
            Device::sweep(&[&dir]),
            FfiCredentialSweep::default(),
        )
        .expect("a survivor owes the user a line");
        assert_eq!(painted.line(), rendered_line(1, false));
        let on_disk = SignOutResidue::load(&device.base).expect("the record outlives the process");
        assert_eq!(on_disk.paths.len(), 1);
        assert_eq!(on_disk.paths[0].path, dir);
    }

    #[test]
    fn remove_again_finishes_the_erase_and_closes_the_view() {
        let device = Device::new();
        let dir = device.scope();
        let painted = sign_out_residue_record(
            device.base(),
            Device::sweep(&[&dir]),
            FfiCredentialSweep::default(),
        )
        .expect("owes work");
        let eraser = device.eraser(true);
        let after = device.retry(&painted, eraser.clone());
        assert!(after.is_none(), "a clean re-sweep closes the view");
        assert!(!dir.exists(), "the recorded scope is gone");
        assert!(!device.record_file().exists(), "and so is the record");
        assert_eq!(
            eraser.calls.load(Ordering::SeqCst),
            0,
            "a clean credential half is never re-erased"
        );
    }

    /// The sign-out's own refusal, asked first: another instance serving the
    /// account erases nothing and paints the refusal's own line.
    #[test]
    fn remove_again_beside_a_serving_instance_erases_nothing_and_says_why() {
        let device = Device::new();
        let dir = device.scope();
        let painted = sign_out_residue_record(
            device.base(),
            Device::sweep(&[&dir]),
            FfiCredentialSweep::default(),
        )
        .expect("owes work");
        let _sibling = acquire_account_instance_lock_shared(
            device.base(),
            ACTOR.to_string(),
            device.container(),
        )
        .expect("the sibling's acquire");

        let after = device
            .retry(&painted, device.eraser(true))
            .expect("a refused retry keeps the view up");
        assert_eq!(
            after.line(),
            fauna_client_accounts::sign_out_residue_retry_blocked_copy()
        );
        assert!(
            dir.join("mls.db").exists(),
            "a served store is never touched"
        );
        assert!(device.record_file().exists(), "the record stands");
    }

    /// The credential half: the seat's own erase runs, and the RECORDED keys
    /// are read back on top of it — a key the erase could not take keeps the
    /// view up even though the (empty) registry would never name it.
    #[test]
    fn surviving_credentials_rerun_the_seats_erase_and_read_back_the_recorded_keys() {
        let device = Device::new();
        device.store.set(KEY.into(), "seed".into());
        let credentials = FfiCredentialSweep {
            survivors: vec![KEY.into()],
            wipe_failed: false,
        };
        let painted =
            sign_out_residue_record(device.base(), Device::sweep(&[]), credentials.clone())
                .expect("surviving credentials owe a line");
        assert_eq!(painted.line(), rendered_line(0, true));

        let stuck = device.eraser(false);
        let still = device
            .retry(&painted, stuck.clone())
            .expect("the key is still readable");
        assert_eq!(
            stuck.calls.load(Ordering::SeqCst),
            1,
            "the seat's erase ran"
        );
        assert_eq!(still.line(), rendered_line(0, true));

        let works = device.eraser(true);
        assert!(device.retry(&still, works.clone()).is_none());
        assert_eq!(works.calls.load(Ordering::SeqCst), 1);
        assert!(!device.record_file().exists());
    }

    #[test]
    fn a_signed_out_launch_resweeps_silently_and_paints_only_what_is_left() {
        let device = Device::new();
        assert!(
            device.recheck(device.eraser(true)).is_none(),
            "no record, nothing to do"
        );

        let dir = device.scope();
        let _ = sign_out_residue_record(
            device.base(),
            Device::sweep(&[&dir]),
            FfiCredentialSweep::default(),
        );
        assert!(
            device.recheck(device.eraser(true)).is_none(),
            "a residue that now goes is finished without a word"
        );
        assert!(!dir.exists());
        assert!(!device.record_file().exists());
    }

    /// A launch with an account in the registry is not the user the residue
    /// was reported to: the record waits, untouched.
    #[test]
    fn a_signed_in_launch_leaves_the_record_alone() {
        let device = Device::new();
        let dir = device.scope();
        let _ = sign_out_residue_record(
            device.base(),
            Device::sweep(&[&dir]),
            FfiCredentialSweep::default(),
        );
        device
            .registry
            .add_account("1".repeat(64), Some("https://a.example".into()), None)
            .expect("sign in");

        assert!(device.recheck(device.eraser(true)).is_none());
        assert!(dir.join("mls.db").exists(), "nothing was swept");
        assert!(device.record_file().exists(), "the record waits");
    }
}
