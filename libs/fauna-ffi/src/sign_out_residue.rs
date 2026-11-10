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
//!   re-sweep, which paints only if something is still left. Retired into
//!   [`sign_out_finish`] on every seat that has wired the sign-out record
//!   (android, apple); kept, unchanged, for windows until its row wires it.
//!
//! **The sign-out record** (`account-scoping.md` § Erasure follows scope → *The
//! sign-out record is on all 7 apps*) joins this face: the gesture writes it
//! with [`sign_out_record_begin`] before its first await, and its tail, the
//! launch and Remove Again all run [`sign_out_finish`] — the shared
//! `fauna_client_accounts::finish_sign_out_with` over the seat's bases and its
//! own acts ([`FfiSignOutHost`]). A remove-account joins the record inside
//! `remove_account_gesture`.
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

use crate::account_state::{FfiCredentialSweep, FfiEraseSweep, store_root_or_logged};
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
            store_root_or_logged(store_container_dir.map(PathBuf::from)).as_deref(),
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
        store_root_or_logged(store_container_dir.map(PathBuf::from)).as_deref(),
        own_lock.as_deref(),
        eraser.as_ref(),
    )
}

/// The seat's own acts inside the sign-out record's finish
/// ([`sign_out_finish`]) — what only a seat can do to its own credential store.
///
/// - [`Self::wipe_own_store`] — the platform store's wholesale reset of the
///   app's OWN credential store, run after the registry's own wipe
///   (`AccountRegistry::wipe_own`): android's `SecureStorage.clear()`. It must
///   not reach an auxiliary account-scoped namespace — each account's slots
///   there ride with its scopes (`account-scoping.md` § Erasure follows scope
///   → *The sign-out record is on all 7 apps*, decision 2).
/// - [`Self::erase_credentials`] — the residue retry's credential erase, as
///   [`FfiResidueCredentialEraser::erase_credentials`].
#[uniffi::export(with_foreign)]
pub trait FfiSignOutHost: Send + Sync {
    /// Reset the app's own credential store wholesale. `false` when the reset
    /// failed — the finish then reports the wipe as failed.
    fn wipe_own_store(&self) -> bool;

    /// Run the residue retry's credential erase and report what is still
    /// readable.
    fn erase_credentials(&self) -> FfiCredentialSweep;
}

/// What a [`sign_out_finish`] run answers — the seat's paint.
#[derive(uniffi::Record, Debug)]
pub struct FfiSignOutFinish {
    /// The launch must not route over the registry: another instance serves an
    /// account the run would reach and the sign-out record still owes its
    /// wipe. Land on `identity_choice` painting [`Self::residue`] (the retry's
    /// refusal line), whose Remove Again re-runs [`sign_out_finish`].
    pub fails_closed: bool,
    /// The run was refused — nothing wiped or erased — whether or not it fails
    /// closed.
    pub refused: bool,
    /// The `sign-out-residue` view: the retry's refusal for a refused run, the
    /// residue line for a run that left something, `None` for clean.
    pub residue: Option<Arc<FfiSignOutResidue>>,
    /// A credential wipe (or re-erase) ran.
    pub credentials_wiped: bool,
    /// A sign-out record was found (a refused run reports whether the record
    /// still owes its wipe).
    pub found_record: bool,
}

/// **The sign-out's decision**, written by the seat's gesture before anything
/// else — after the serving question passed, before the runtime's stop is
/// awaited (`account-scoping.md` § Erasure follows scope → *The sign-out record
/// is on all 7 apps*, decision 1). The native record (`sign-out-record.json`
/// under `base_dir`, the app's install base) names every account the registry
/// lists plus `served_actor` — the account this process serves, which an
/// unreadable index would not name — and owes the credential wipe. A process
/// killed anywhere after this call is finished by the next launch's
/// [`sign_out_finish`].
///
/// The unreadable-index floor's start-over calls it with the registry naming
/// nobody and no served actor: a wipe owed with no accounts, whose finish
/// sweeps every scope under the bases.
///
/// Returns the accounts recorded — the gesture hands them to its own
/// [`sign_out_finish`] as `written`, so a record the install base refused still
/// finishes in this process.
#[uniffi::export]
pub fn sign_out_record_begin(
    registry: Arc<FfiAccountRegistry>,
    base_dir: String,
    served_actor: Option<String>,
) -> Vec<String> {
    let reach = registry
        .registry()
        .list()
        .into_iter()
        .map(|entry| entry.actor_id)
        .chain(served_actor);
    let record = fauna_client_accounts::SignOutRecord::record_sign_out(
        &fauna_client_accounts::SignOutRecordFile::under(&base_dir),
        reach,
    );
    tracing::info!(
        "[sign-out] recorded for {} account(s)",
        record.accounts.len()
    );
    record.accounts
}

/// Whether a sign-out record under `base_dir` still owes anything — so a
/// launch closes its open account-scoped handles before a finish that will
/// erase under them, and leaves them alone when there is nothing to finish.
#[uniffi::export]
pub fn sign_out_record_pending(base_dir: String) -> bool {
    fauna_client_accounts::SignOutRecord::load(&fauna_client_accounts::SignOutRecordFile::under(
        &base_dir,
    ))
    .is_some()
}

/// **The one routine** a seat's sign-out tail, its launch and Remove Again
/// (`sign-out-residue-retry-button`) run — the shared
/// [`fauna_client_accounts::finish_sign_out_with`] over the seat's bases, its
/// registry and its own acts ([`FfiSignOutHost`]).
///
/// - `base_dirs` — the app's own state bases the erase sweeps; the FIRST is the
///   install base, home of both records and where an instance of this app
///   declares itself (android passes its two roots).
/// - `store_container_dir` — exactly what the seat passes to the account
///   runtime (`None` on desktops); its root is swept and asked about too.
/// - `own_lock` — a lock this process still holds, put down around the run;
///   `None` at the launch, which holds none yet.
/// - `dismiss_agent` — dismiss the sync agent from each recorded account
///   before the erase (decision 5): `true` for the launch and Remove Again,
///   `false` for the gesture, whose own stop already dismissed it.
/// - `written` — the accounts [`sign_out_record_begin`] returned, from the
///   gesture only: stands in for a record the install base refused.
///
/// Blocking file I/O and, with `dismiss_agent`, up to the agent budget per
/// recorded account: call it off the main thread.
#[uniffi::export]
pub fn sign_out_finish(
    registry: Arc<FfiAccountRegistry>,
    base_dirs: Vec<String>,
    store_container_dir: Option<String>,
    own_lock: Option<Arc<FfiAccountInstanceLock>>,
    host: Arc<dyn FfiSignOutHost>,
    dismiss_agent: bool,
    written: Option<Vec<String>>,
) -> FfiSignOutFinish {
    let store_root = store_root_or_logged(store_container_dir.map(PathBuf::from));
    let outcome = with_own_lock_down(own_lock.as_deref(), || {
        finish_under(
            &registry,
            &base_dirs,
            store_root.as_deref(),
            host.as_ref(),
            dismiss_agent,
            written,
        )
    });
    painted_finish(outcome)
}

/// The pure half of [`sign_out_finish`], every base passed in.
fn finish_under(
    registry: &FfiAccountRegistry,
    base_dirs: &[String],
    store_root: Option<&Path>,
    host: &dyn FfiSignOutHost,
    dismiss_agent: bool,
    written: Option<Vec<String>>,
) -> fauna_client_accounts::SignOutFinish {
    let Some(install_base) = base_dirs.first().map(PathBuf::from) else {
        tracing::error!("[sign-out] finish called with no base — nothing to finish");
        return fauna_client_accounts::SignOutFinish::NOTHING;
    };
    let swept: Vec<PathBuf> = base_dirs
        .iter()
        .map(PathBuf::from)
        .chain(store_root.map(Path::to_path_buf))
        .collect();
    let mut seat_host = FfiFinishHost {
        registry,
        host,
        dismiss_agent,
    };
    fauna_client_accounts::finish_sign_out_with(
        fauna_client_accounts::FinishSeat {
            install_base: &install_base,
            swept_bases: &swept,
            serving: serving_bases(&install_base, store_root),
            registry: registry.registry(),
        },
        &mut seat_host,
        written.map(|accounts| fauna_client_accounts::SignOutRecord {
            wipe_owed: true,
            accounts,
        }),
    )
}

/// The FFI seats' acts inside the shared finish: the wipe is the registry's
/// own store (`wipe_own`) then the seat's wholesale reset of it, read back over
/// that store alone; each account's slots in the auxiliary namespaces ride with
/// its scopes; the residue retry's credential half is the retry's own.
struct FfiFinishHost<'a> {
    registry: &'a FfiAccountRegistry,
    host: &'a dyn FfiSignOutHost,
    dismiss_agent: bool,
}

impl fauna_client_accounts::FinishHost for FfiFinishHost<'_> {
    fn wipe(&mut self) -> CredentialSweep {
        let mut sweep = self.registry.registry().wipe_own();
        if !self.host.wipe_own_store() {
            tracing::warn!("[sign-out] the seat's own credential store reset failed");
            sweep.record_wipe_failure();
        }
        self.registry.registry().reverify_own(sweep)
    }

    fn erase_account_slots(&mut self, actor_id_hex: &str) -> CredentialSweep {
        self.registry
            .registry()
            .erase_account_aux_slots(actor_id_hex)
    }

    fn dismiss_agent(&mut self, actor_id_hex: &str) {
        if self.dismiss_agent {
            dismiss_agent_from(actor_id_hex);
        }
    }

    fn re_erase_credentials(&mut self, recorded: CredentialSweep) -> CredentialSweep {
        let fresh: CredentialSweep = self.host.erase_credentials().into();
        merge_read_back(self.registry, fresh, recorded)
    }
}

/// How long the finish waits on the sync agent's dismissal of one recorded
/// account (decision 5) — tui's `AGENT_DISMISS_BUDGET`. Two IPC round trips;
/// an agent that answers neither in this long meets the revoke at the nest.
#[cfg(all(any(unix, windows), feature = "sync-agent-provisioning"))]
const AGENT_DISMISS_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// Dismiss the sync agent from a recorded account, awaited — the actor-matched
/// exchange (`signed_out_onboarding_reconcile_with_marker`) over a marker built
/// from the record, since a provisioner killed inside its stop never wrote one.
/// Its own thread and runtime, so a synchronous export can wait on it; an
/// unresolvable endpoint (no agent on this platform) answers at once.
#[cfg(all(any(unix, windows), feature = "sync-agent-provisioning"))]
fn dismiss_agent_from(actor_id_hex: &str) {
    let Ok(actor) = fauna_core::hex32::decode(actor_id_hex) else {
        return;
    };
    let Ok(endpoint) = fauna_ipc::endpoint::AgentEndpoint::default_for_user() else {
        return;
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let marker = fauna_ipc::sync::SignedOutMarker::new(actor.to_vec(), now_ms);
    let label = actor_id_hex.to_string();
    let spawned = std::thread::Builder::new()
        .name("sign-out-agent".into())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async {
                let exchange =
                    fauna_client_sync::agent::signed_out_onboarding_reconcile_with_marker(
                        &endpoint, &marker,
                    );
                if tokio::time::timeout(AGENT_DISMISS_BUDGET, exchange)
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        "[sign-out] the sync agent did not answer its dismissal from {label} \
                         within {AGENT_DISMISS_BUDGET:?} — erasing all the same"
                    );
                }
            });
        });
    if let Ok(handle) = spawned {
        let _ = handle.join();
    }
}

/// No local agent seam in this build: nothing to dismiss.
#[cfg(not(all(any(unix, windows), feature = "sync-agent-provisioning")))]
fn dismiss_agent_from(_actor_id_hex: &str) {}

/// A finish's outcome as the seat paints it.
fn painted_finish(outcome: fauna_client_accounts::SignOutFinish) -> FfiSignOutFinish {
    let fails_closed = outcome.fails_closed();
    match outcome {
        fauna_client_accounts::SignOutFinish::Refused {
            residue, wipe_owed, ..
        } => FfiSignOutFinish {
            fails_closed,
            refused: true,
            residue: Some(Arc::new(FfiSignOutResidue {
                record: residue.unwrap_or_default(),
                line: fauna_client_accounts::sign_out_residue_retry_blocked_copy(),
            })),
            credentials_wiped: false,
            found_record: wipe_owed,
        },
        fauna_client_accounts::SignOutFinish::Finished {
            residue,
            credentials_wiped,
            found_record,
        } => FfiSignOutFinish {
            fails_closed,
            refused: false,
            residue: residue.and_then(painted),
            credentials_wiped,
            found_record,
        },
    }
}

/// The one re-sweep both gestures run, behind the sign-out's serving-lock
/// question asked about exactly the two bases its erase reaches.
fn run_retry(
    record: &SignOutResidue,
    registry: &FfiAccountRegistry,
    base: &Path,
    store_root: Option<&Path>,
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
    merge_read_back(registry, fresh, recorded)
}

/// A fresh credential erase plus the read-back of the keys a sign-out recorded
/// as surviving, as one answer.
fn merge_read_back(
    registry: &FfiAccountRegistry,
    fresh: CredentialSweep,
    recorded: CredentialSweep,
) -> CredentialSweep {
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

    // ── The sign-out record (`account-scoping.md` § Erasure follows scope →
    //    *The sign-out record is on all 7 apps*) ──────────────────────────────

    /// The seat's own acts: its wholesale reset clears the app store, and the
    /// residue retry's erase does the same; both counted.
    struct Host {
        store: Arc<MapStore>,
        wipes: AtomicUsize,
    }

    impl FfiSignOutHost for Host {
        fn wipe_own_store(&self) -> bool {
            self.wipes.fetch_add(1, Ordering::SeqCst);
            self.store.0.lock().unwrap().clear();
            true
        }
        fn erase_credentials(&self) -> FfiCredentialSweep {
            self.store.0.lock().unwrap().clear();
            FfiCredentialSweep::default()
        }
    }

    impl Device {
        fn host(&self) -> Arc<Host> {
            Arc::new(Host {
                store: self.store.clone(),
                wipes: AtomicUsize::new(0),
            })
        }

        /// A signed-in account; its actor id.
        fn sign_in(&self, secret_fill: char) -> String {
            self.registry
                .add_account(
                    secret_fill.to_string().repeat(64),
                    Some("https://a.example".into()),
                    None,
                )
                .expect("sign in")
        }

        /// `actor`'s scope under the install base, holding one file.
        fn scope_of(&self, actor: &str) -> PathBuf {
            let dir = self.base.join(actor);
            std::fs::create_dir_all(&dir).expect("scope");
            std::fs::write(dir.join("mls.db"), b"the user's data").expect("file");
            dir
        }

        fn sign_out_record_file(&self) -> PathBuf {
            self.base.join(fauna_client_accounts::SIGN_OUT_RECORD_FILE)
        }

        /// The launch's call: no lock held, the agent dismissed, no record in
        /// hand.
        fn finish(&self, host: Arc<Host>) -> FfiSignOutFinish {
            sign_out_finish(
                self.registry.clone(),
                vec![self.base()],
                self.container(),
                None,
                host,
                true,
                None,
            )
        }
    }

    /// The gesture's decision is on disk before anything awaits: every account
    /// the registry lists plus the one this process serves, a wipe owed.
    #[test]
    fn the_gesture_records_every_listed_account_and_the_served_one_on_disk() {
        let device = Device::new();
        let listed = device.sign_in('1');
        let served = ACTOR.to_string();

        let recorded =
            sign_out_record_begin(device.registry.clone(), device.base(), Some(served.clone()));

        assert!(
            device.sign_out_record_file().exists(),
            "the record is on disk"
        );
        let on_disk = fauna_client_accounts::SignOutRecord::load(
            &fauna_client_accounts::SignOutRecordFile::under(&device.base),
        )
        .expect("a record");
        assert!(on_disk.wipe_owed);
        assert_eq!(on_disk.accounts, vec![listed.clone(), served.clone()]);
        assert_eq!(recorded, on_disk.accounts);
    }

    /// A launch over a sign-out killed after its record: the wipe, every
    /// recorded account's scope, and the record itself — the launch lands
    /// signed out and clean.
    #[test]
    fn a_launch_over_a_seeded_record_wipes_erases_and_clears() {
        let device = Device::new();
        let actor = device.sign_in('1');
        let dir = device.scope_of(&actor);
        sign_out_record_begin(device.registry.clone(), device.base(), None);

        let host = device.host();
        let outcome = device.finish(host.clone());

        assert!(!outcome.fails_closed && !outcome.refused);
        assert!(outcome.found_record && outcome.credentials_wiped);
        assert!(outcome.residue.is_none(), "a clean finish paints nothing");
        assert_eq!(host.wipes.load(Ordering::SeqCst), 1, "the seat's reset ran");
        assert!(device.registry.list().is_empty(), "the registry is wiped");
        assert!(device.store.0.lock().unwrap().is_empty(), "so is the store");
        assert!(!dir.exists(), "the recorded scope is erased");
        assert!(
            !device.sign_out_record_file().exists(),
            "and the record cleared"
        );

        let again = device.finish(device.host());
        assert!(
            !again.found_record && again.residue.is_none(),
            "nothing is owed"
        );
    }

    /// The serving question first: an account another instance serves refuses
    /// the whole run — nothing wiped, nothing erased, the record standing — and
    /// the launch fails closed onto the retry's refusal line.
    #[test]
    fn a_served_account_refuses_everything_and_the_launch_fails_closed() {
        let device = Device::new();
        let actor = device.sign_in('1');
        let dir = device.scope_of(&actor);
        sign_out_record_begin(device.registry.clone(), device.base(), None);
        let _sibling =
            acquire_account_instance_lock_shared(device.base(), actor.clone(), device.container())
                .expect("the sibling's acquire");

        let host = device.host();
        let outcome = device.finish(host.clone());

        assert!(outcome.fails_closed && outcome.refused);
        assert_eq!(
            outcome.residue.expect("the refusal paints").line(),
            fauna_client_accounts::sign_out_residue_retry_blocked_copy()
        );
        assert_eq!(host.wipes.load(Ordering::SeqCst), 0, "nothing wiped");
        assert_eq!(device.registry.list().len(), 1, "the registry stands");
        assert!(
            dir.join("mls.db").exists(),
            "a served store is never touched"
        );
        assert!(device.sign_out_record_file().exists(), "the record stands");
    }

    /// A remove-account killed after its registry step: the removal joined the
    /// record, so the next launch erases that account's scope alone — no wipe,
    /// the other account kept — and clears the record.
    #[test]
    fn a_removal_joins_the_record_and_the_launch_erases_only_that_account() {
        let device = Device::new();
        let removed = device.sign_in('1');
        let kept = device.sign_in('2');
        let removed_dir = device.scope_of(&removed);
        let kept_dir = device.scope_of(&kept);
        fauna_client_accounts::SignOutRecord::record_removal(
            &fauna_client_accounts::SignOutRecordFile::under(&device.base),
            &removed,
        );
        device
            .registry
            .remove(removed.clone())
            .expect("registry step");

        let host = device.host();
        let outcome = device.finish(host.clone());

        assert!(!outcome.fails_closed && outcome.found_record);
        assert!(!outcome.credentials_wiped, "a removal owes no wipe");
        assert_eq!(host.wipes.load(Ordering::SeqCst), 0);
        assert!(
            !removed_dir.exists(),
            "the removed account's scope is erased"
        );
        assert!(
            kept_dir.join("mls.db").exists(),
            "the other account is kept"
        );
        assert_eq!(device.registry.list().len(), 1);
        assert!(!device.sign_out_record_file().exists());
    }
}
