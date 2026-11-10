//! The native **finish** of a sign-out — the one routine a native seat's
//! gesture tail, its launch and its `sign-out-residue-retry-button` all run
//! (`apps/account-scoping.md` § Erasure follows scope → *The sign-out record is
//! on all 7 apps*, decisions 2 to 7). `fauna-wasm`'s `account_scope::finish_recorded`
//! is its web twin.
//!
//! The gesture writes the [`SignOutRecord`] (into its [`SignOutRecordFile`])
//! after the serving question passes and before its first await; whoever runs
//! [`finish_sign_out`] next does what the record owes, in one order:
//!
//! 1. **The serving question** (decision 4), asked about everything the run
//!    would reach — the recorded accounts, every actor scope under the swept
//!    bases when a wipe is owed, and the residue record's accounts — through
//!    the same probe a sign-out asks ([`crate::actors_served_by_another_instance`]).
//!    Any one served → [`SignOutFinish::Refused`]: nothing wiped, nothing
//!    erased, the record stands whole.
//! 2. **The sync agent's dismissal** (decision 5) for each recorded account,
//!    before anything is erased — only for a sign-out's record (a wipe owed):
//!    a remove-account does not dismiss the agent.
//! 3. **The wipe** if owed (decision 2): [`SignOutRecord::before_wipe`] names
//!    every account the registry lists, the seat wipes its own namespace,
//!    [`SignOutRecord::wipe_done`].
//! 4. **Each recorded account's erase**: its scopes under the swept bases and
//!    its slots in the auxiliary namespaces; then, for a sign-out, the
//!    residual sweep of every actor scope under the bases.
//! 5. **The residue record's re-sweep** — only while the registry names
//!    nobody, by the residue record's own four rules
//!    ([`crate::retry_sign_out_residue`]).
//! 6. **The handover** (decision 6): what the erase left is written into the
//!    residue record, and only then is each erased account settled out of
//!    the sign-out record. A crash between the two leaves both standing, and
//!    each step is idempotent.
//!
//! Pure over its inputs: the install base, the swept bases, the serving bases
//! and the registry come in as [`FinishSeat`]; the seat's own credential acts
//! and the agent exchange come in as a [`FinishHost`].

use std::path::{Path, PathBuf};

use crate::{
    AccountRegistry, CredentialSweep, EraseBlocked, ResidueRetry, ResidueSurface, ServingBases,
    SignOutRecord, SignOutRecordFile, SignOutResidue,
};

/// Where a finish runs: everything it reads and the bases it erases under.
#[derive(Clone, Copy)]
pub struct FinishSeat<'a> {
    /// The app's install base — home of both records.
    pub install_base: &'a Path,
    /// Every base the seat's erase sweeps (tui's `erase_bases`).
    pub swept_bases: &'a [PathBuf],
    /// Where an instance declares itself, for the serving question.
    pub serving: ServingBases<'a>,
    /// The seat's erase-complete registry.
    pub registry: &'a AccountRegistry,
}

/// The seat's own acts, which differ per seat.
pub trait FinishHost {
    /// The **wipe**: the app's own credential namespace — the index and the
    /// app's own per-actor slots — and nothing in an auxiliary namespace.
    fn wipe(&mut self) -> CredentialSweep;

    /// One account's slots in the auxiliary account-scoped namespaces
    /// (`AccountRegistry::erase_account_aux_slots`).
    fn erase_account_slots(&mut self, actor_id_hex: &str) -> CredentialSweep;

    /// Dismiss the sync agent from `actor_id_hex` before its erase — the
    /// actor-matched exchange (`signed_out_onboarding_reconcile_with_marker`),
    /// awaited. A gesture whose own stop already dismissed it does nothing.
    fn dismiss_agent(&mut self, actor_id_hex: &str);

    /// The credential half of the residue re-sweep — the seat's
    /// `re_erase_credentials`, handed the recorded credential sweep.
    fn re_erase_credentials(&mut self, recorded: CredentialSweep) -> CredentialSweep;
}

/// What a [`finish_sign_out`] run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignOutFinish {
    /// Another instance serves an account the run would reach: nothing was
    /// wiped or erased, and both records stand as they were. A launch fails
    /// closed on this — it does not route over the registry.
    Refused {
        blocked: EraseBlocked,
        /// The residue record as it stands, if any.
        residue: Option<SignOutResidue>,
        /// Whether the sign-out record still owes its wipe.
        wipe_owed: bool,
    },
    /// The run did what was owed (possibly nothing).
    Finished {
        /// What is left on the device, as the residue record holds it;
        /// `None` when clean or when nothing may be reported yet (the
        /// registry names somebody).
        residue: Option<SignOutResidue>,
        /// A credential wipe ran — on a sealed arm it relocked the store.
        credentials_wiped: bool,
        /// A sign-out record was found.
        found_record: bool,
    },
}

impl SignOutFinish {
    /// Nothing was owed and nothing is left.
    pub const NOTHING: Self = Self::Finished {
        residue: None,
        credentials_wiped: false,
        found_record: false,
    };

    /// The `sign-out-residue` paint: the retry's refusal line for a refused
    /// run, the residue line for one that left something, `None` for clean.
    pub fn surface<F, S>(&self, lookup: F) -> Option<ResidueSurface>
    where
        F: Fn(&str) -> Option<S>,
        S: AsRef<str>,
    {
        match self {
            Self::Refused { residue, .. } => Some(ResidueSurface {
                record: residue.clone().unwrap_or_default(),
                line: crate::sign_out_residue_retry_blocked_copy().resolve(lookup),
            }),
            Self::Finished { residue, .. } => residue
                .clone()
                .and_then(|record| ResidueSurface::for_record(record, lookup)),
        }
    }

    /// The launch's gate: a refused run with its wipe still owed — the launch
    /// must not route over the registry.
    pub fn fails_closed(&self) -> bool {
        matches!(
            self,
            Self::Refused {
                wipe_owed: true,
                ..
            }
        )
    }
}

/// Do what the sign-out record and the residue record owe — see the module
/// docs for the order.
pub fn finish_sign_out(seat: FinishSeat<'_>, host: &mut dyn FinishHost) -> SignOutFinish {
    finish_sign_out_with(seat, host, None)
}

/// [`finish_sign_out`] for the gesture's tail, handed the record the gesture
/// wrote. The slot is read first; `written` stands in only when the slot holds
/// nothing — a record file the install base refused — so a sign-out whose
/// record could not be persisted still wipes and erases in this process,
/// exactly as before the record existed.
pub fn finish_sign_out_with(
    seat: FinishSeat<'_>,
    host: &mut dyn FinishHost,
    written: Option<SignOutRecord>,
) -> SignOutFinish {
    let slot = SignOutRecordFile::under(seat.install_base);
    let record = SignOutRecord::load(&slot).or_else(|| {
        written.inspect(|_| {
            tracing::warn!(
                "[sign-out record] {} holds no record — finishing from the one this \
                 gesture wrote in memory",
                slot.path().display()
            );
        })
    });
    let residue = SignOutResidue::load(seat.install_base);
    let Some(record) = record else {
        return finish_residue_only(seat, host, residue);
    };

    let registry_accounts: Vec<String> = seat
        .registry
        .list()
        .into_iter()
        .map(|entry| entry.actor_id)
        .collect();
    let plan = record.plan(&registry_accounts);

    // 1. The serving question, over everything this run reaches.
    let mut reach = plan.erase.clone();
    let mut push = |actor: String| {
        if !reach.iter().any(|a| a.eq_ignore_ascii_case(&actor)) {
            reach.push(actor);
        }
    };
    if plan.wipe {
        for base in seat.swept_bases {
            let mut on_disk = fauna_account_store::db::account_scopes_under(base);
            on_disk.sort();
            on_disk.into_iter().for_each(&mut push);
        }
    }
    if let Some(residue) = &residue {
        residue.accounts().into_iter().for_each(&mut push);
    }
    let served = crate::actors_served_by_another_instance(seat.serving, &reach);
    if !served.is_empty() {
        tracing::warn!(
            "[sign-out record] finish REFUSED — another Fauna instance still serves {}; \
             nothing wiped or erased, the record stands",
            served.join(", ")
        );
        return SignOutFinish::Refused {
            blocked: EraseBlocked { accounts: served },
            residue,
            wipe_owed: plan.wipe,
        };
    }

    // 2. The agent, before anything is erased — a sign-out's record only.
    if plan.wipe {
        for actor in &plan.erase {
            host.dismiss_agent(actor);
        }
    }

    // 3. The wipe.
    let mut credentials = CredentialSweep::default();
    if plan.wipe {
        SignOutRecord::before_wipe(&slot, plan.erase.iter().cloned());
        credentials = host.wipe();
        SignOutRecord::wipe_done(&slot);
        tracing::info!(
            "[sign-out record] credentials wiped; {} account(s) to erase",
            plan.erase.len()
        );
    }
    for actor in &plan.keep {
        tracing::info!(
            "[sign-out record] {actor} is signed in here again — kept, and it leaves the record"
        );
        SignOutRecord::settle(&slot, actor);
    }

    // 4. Each recorded account's erase, then a sign-out's residual sweep.
    let mut sweep = fauna_account_store::db::EraseSweep::default();
    for actor in &plan.erase {
        sweep.absorb(fauna_account_store::db::EraseSweep {
            erased: 0,
            survivors: fauna_account_store::db::erase_actor_state(
                seat.swept_bases.iter().map(PathBuf::as_path),
                actor,
            ),
        });
        credentials = union(credentials, host.erase_account_slots(actor));
    }
    if plan.wipe {
        for base in seat.swept_bases {
            sweep.absorb(fauna_account_store::db::erase_all_account_scopes(base));
        }
    }

    // 5. The residue record's re-sweep, while the registry names nobody.
    let (left, re_erased) = resweep_residue(seat, host, residue);

    // 6. The handover: the residue record first, then the sign-out record.
    crate::log_residue(&sweep.survivors, &credentials);
    let merged = merge(left, SignOutResidue::record(&sweep.survivors, &credentials));
    crate::persist_residue(&merged, Some(seat.install_base));
    for actor in &plan.erase {
        SignOutRecord::settle(&slot, actor);
    }
    SignOutFinish::Finished {
        residue: merged.owes_work().then_some(merged),
        credentials_wiped: plan.wipe || re_erased,
        found_record: true,
    }
}

/// No sign-out record: only the residue record's re-sweep is owed — today's
/// launch re-check and Remove Again.
fn finish_residue_only(
    seat: FinishSeat<'_>,
    host: &mut dyn FinishHost,
    residue: Option<SignOutResidue>,
) -> SignOutFinish {
    let Some(record) = residue else {
        return SignOutFinish::NOTHING;
    };
    if !seat.registry.list().is_empty() {
        // A signed-in user is not the one this residue was reported to.
        tracing::info!(
            "[sign-out residue] not re-swept and not painted — the registry names {} \
             account(s); the record waits for a signed-out run",
            seat.registry.list().len()
        );
        return SignOutFinish::NOTHING;
    }
    let mut re_erased = false;
    let outcome = crate::retry_sign_out_residue(&record, seat.serving, |recorded| {
        re_erased = true;
        host.re_erase_credentials(recorded)
    });
    match outcome {
        ResidueRetry::Blocked(blocked) => SignOutFinish::Refused {
            blocked,
            residue: Some(record),
            wipe_owed: false,
        },
        ResidueRetry::Swept(left) => {
            let paths: Vec<PathBuf> = left.paths.iter().map(|p| p.path.clone()).collect();
            crate::log_residue(&paths, &left.credentials());
            crate::persist_residue(&left, Some(seat.install_base));
            SignOutFinish::Finished {
                residue: left.owes_work().then_some(left),
                credentials_wiped: re_erased,
                found_record: false,
            }
        }
    }
}

/// Re-sweep an earlier residue record when the registry names nobody;
/// otherwise keep it as it is. Answers what is left and whether the
/// credential re-erase ran.
fn resweep_residue(
    seat: FinishSeat<'_>,
    host: &mut dyn FinishHost,
    residue: Option<SignOutResidue>,
) -> (SignOutResidue, bool) {
    let Some(record) = residue else {
        return (SignOutResidue::default(), false);
    };
    if !seat.registry.list().is_empty() {
        return (record, false);
    }
    let mut re_erased = false;
    let outcome = crate::retry_sign_out_residue(&record, seat.serving, |recorded| {
        re_erased = true;
        host.re_erase_credentials(recorded)
    });
    match outcome {
        ResidueRetry::Swept(left) => (left, re_erased),
        ResidueRetry::Blocked(_) => (record, false),
    }
}

/// Two credential sweeps as one answer.
fn union(mut a: CredentialSweep, b: CredentialSweep) -> CredentialSweep {
    for key in b.survivors {
        if !a.survivors.contains(&key) {
            a.survivors.push(key);
        }
    }
    a.wipe_failed |= b.wipe_failed;
    a
}

/// An earlier residue record and a fresh erase's survivors as one record — a
/// path once, the earlier entry (and its fingerprint) kept.
fn merge(mut earlier: SignOutResidue, fresh: SignOutResidue) -> SignOutResidue {
    for path in fresh.paths {
        if !earlier.paths.iter().any(|p| p.path == path.path) {
            earlier.paths.push(path);
        }
    }
    for key in fresh.credential_keys {
        if !earlier.credential_keys.contains(&key) {
            earlier.credential_keys.push(key);
        }
    }
    earlier.credential_wipe_failed |= fresh.credential_wipe_failed;
    earlier
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::instance_lock::{ServingMode, SessionInstanceHolder};
    use crate::{InMemorySecretStore, SecretStore, SignOutRecordSlot};

    fn actor(pair: &str) -> String {
        pair.repeat(32)
    }

    /// A seat over temp dirs: an install base, two swept bases (the flat base
    /// and a store root), an app store and one auxiliary namespace.
    struct World {
        install: tempfile::TempDir,
        store_root: tempfile::TempDir,
        swept: Vec<PathBuf>,
        app: Arc<InMemorySecretStore>,
        aux: Arc<InMemorySecretStore>,
        registry: AccountRegistry,
    }

    impl World {
        fn new() -> Self {
            let install = tempfile::tempdir().expect("install base");
            let store_root = tempfile::tempdir().expect("store root");
            let swept = vec![
                install.path().to_path_buf(),
                store_root.path().to_path_buf(),
            ];
            let app = Arc::new(InMemorySecretStore::new());
            let aux = Arc::new(InMemorySecretStore::new());
            let registry = AccountRegistry::new(Arc::clone(&app) as Arc<dyn SecretStore>)
                .also_erasing(vec![Arc::clone(&aux) as Arc<dyn SecretStore>]);
            Self {
                install,
                store_root,
                swept,
                app,
                aux,
                registry,
            }
        }

        fn seat(&self) -> FinishSeat<'_> {
            FinishSeat {
                install_base: self.install.path(),
                swept_bases: &self.swept,
                serving: ServingBases {
                    state_base: Some(self.install.path()),
                    store_root: Some(self.store_root.path()),
                },
                registry: &self.registry,
            }
        }

        fn slot(&self) -> SignOutRecordFile {
            SignOutRecordFile::under(self.install.path())
        }

        /// A signed-in account: registry entry, an aux slot, a scope under
        /// each swept base.
        fn sign_in(&self, seed: &str) -> String {
            let actor = self
                .registry
                .add_account(&seed.repeat(64), Some("https://n.example"), None)
                .expect("registers");
            self.aux.set(&actor, "writer-key");
            for base in &self.swept {
                let scope = base.join(&actor);
                std::fs::create_dir_all(&scope).expect("scope");
                std::fs::write(scope.join("store.db"), b"data").expect("data");
            }
            actor
        }

        fn scopes(&self) -> Vec<String> {
            let mut all = Vec::new();
            for base in &self.swept {
                all.extend(fauna_account_store::db::account_scopes_under(base));
            }
            all
        }

        fn record(&self) -> Option<SignOutRecord> {
            SignOutRecord::load(&self.slot())
        }

        fn finish(&self, host: &mut Host<'_>) -> SignOutFinish {
            finish_sign_out(self.seat(), host)
        }
    }

    /// The seat's acts over the world's registry, logging what ran.
    struct Host<'w> {
        world: &'w World,
        log: Vec<String>,
    }

    impl<'w> Host<'w> {
        fn new(world: &'w World) -> Self {
            Self {
                world,
                log: Vec::new(),
            }
        }
    }

    impl FinishHost for Host<'_> {
        fn wipe(&mut self) -> CredentialSweep {
            self.log.push("wipe".into());
            self.world.registry.wipe_own()
        }
        fn erase_account_slots(&mut self, actor: &str) -> CredentialSweep {
            self.log.push(format!("slots {actor}"));
            self.world.registry.erase_account_aux_slots(actor)
        }
        fn dismiss_agent(&mut self, actor: &str) {
            self.log.push(format!("agent {actor}"));
        }
        fn re_erase_credentials(&mut self, recorded: CredentialSweep) -> CredentialSweep {
            self.log.push("re-erase".into());
            self.world.registry.reverify(recorded)
        }
    }

    /// Nothing recorded, nothing left: the run does nothing at all.
    #[test]
    fn nothing_owed_does_nothing() {
        let world = World::new();
        let a = world.sign_in("a");
        let mut host = Host::new(&world);
        assert_eq!(world.finish(&mut host), SignOutFinish::NOTHING);
        assert!(host.log.is_empty());
        assert_eq!(world.registry.list().len(), 1, "the account is untouched");
        assert!(world.scopes().contains(&a));
    }

    /// Killed after the record (or inside the stop): the launch finds the wipe
    /// owed, dismisses the agent, wipes, erases, and lands signed out — no
    /// registry entry, no credential, no scope, no record, no residue.
    #[test]
    fn a_record_with_its_wipe_owed_finishes_the_whole_sign_out() {
        let world = World::new();
        let a = world.sign_in("a");
        SignOutRecord::record_sign_out(&world.slot(), [a.clone()]);

        let mut host = Host::new(&world);
        let outcome = world.finish(&mut host);

        assert_eq!(
            outcome,
            SignOutFinish::Finished {
                residue: None,
                credentials_wiped: true,
                found_record: true,
            }
        );
        assert_eq!(
            host.log,
            vec![format!("agent {a}"), "wipe".into(), format!("slots {a}")],
            "the agent first, then the wipe, then the account's slots"
        );
        assert!(world.registry.list().is_empty());
        assert_eq!(world.app.get(&crate::secret_key(&a)), None);
        assert_eq!(world.aux.get(&a), None, "the aux writer key went with it");
        assert!(world.scopes().is_empty());
        assert_eq!(world.record(), None);
        assert!(SignOutResidue::load(world.install.path()).is_none());
    }

    /// Killed after the wipe, before `wipe_done`: the registry is empty, the
    /// wipe is a no-op, and the record alone names whose scopes to erase.
    #[test]
    fn a_wipe_already_run_still_erases_the_recorded_accounts() {
        let world = World::new();
        let a = world.sign_in("a");
        SignOutRecord::record_sign_out(&world.slot(), [a.clone()]);
        let _ = world.registry.wipe_own();
        assert!(world.registry.list().is_empty());

        let mut host = Host::new(&world);
        world.finish(&mut host);
        assert!(world.scopes().is_empty());
        assert_eq!(world.aux.get(&a), None);
        assert_eq!(world.record(), None);
    }

    /// Wipe done, accounts left (killed inside the erase): no second wipe, no
    /// agent exchange left to run for a wipe already done, each recorded
    /// account erased again.
    #[test]
    fn wipe_done_with_accounts_left_erases_them_without_wiping_again() {
        let world = World::new();
        let a = world.sign_in("a");
        SignOutRecord::record_sign_out(&world.slot(), [a.clone()]);
        let _ = world.registry.wipe_own();
        SignOutRecord::wipe_done(&world.slot());

        let mut host = Host::new(&world);
        let outcome = world.finish(&mut host);
        assert_eq!(host.log, vec![format!("slots {a}")]);
        assert!(matches!(
            outcome,
            SignOutFinish::Finished {
                credentials_wiped: false,
                ..
            }
        ));
        assert!(world.scopes().is_empty());
        assert_eq!(world.record(), None);
    }

    /// A record the install base refused to hold: the gesture's tail finishes
    /// from the record it wrote in memory, so the sign-out still lands.
    #[test]
    fn a_record_the_slot_refused_still_finishes_from_memory() {
        let world = World::new();
        let a = world.sign_in("a");
        let written = SignOutRecord {
            wipe_owed: true,
            accounts: vec![a.clone()],
        };
        assert_eq!(world.record(), None, "nothing on disk");
        let mut host = Host::new(&world);
        finish_sign_out_with(world.seat(), &mut host, Some(written));
        assert!(world.registry.list().is_empty());
        assert!(world.scopes().is_empty());
        assert_eq!(world.aux.get(&a), None);
    }

    /// A served account refuses everything: nothing wiped, nothing erased,
    /// the record stands whole, and the launch fails closed.
    #[test]
    fn a_served_account_refuses_everything_and_keeps_the_record() {
        let world = World::new();
        let a = world.sign_in("a");
        let b = world.sign_in("b");
        let before = SignOutRecord::record_sign_out(&world.slot(), [a.clone(), b.clone()]);

        let mut sibling = SessionInstanceHolder::new();
        sibling.become_session_instance(
            ServingBases {
                state_base: Some(world.install.path()),
                store_root: Some(world.store_root.path()),
            },
            &b,
            None,
            ServingMode::Concurrent,
        );

        let mut host = Host::new(&world);
        let outcome = world.finish(&mut host);
        assert!(outcome.fails_closed(), "{outcome:?}");
        assert!(
            matches!(&outcome, SignOutFinish::Refused { blocked, .. } if blocked.accounts == vec![b.clone()])
        );
        assert!(host.log.is_empty(), "no agent exchange, no wipe, no erase");
        assert_eq!(world.registry.list().len(), 2);
        assert_eq!(world.aux.get(&a).as_deref(), Some("writer-key"));
        assert!(world.scopes().contains(&a) && world.scopes().contains(&b));
        assert_eq!(world.record(), Some(before));
        let surface = outcome
            .surface(fauna_i18n::strings::lookup)
            .expect("a refusal paints");
        assert_eq!(
            surface.line,
            crate::sign_out_residue_retry_blocked_copy().resolve(fauna_i18n::strings::lookup)
        );

        drop(sibling);
        world.finish(&mut Host::new(&world));
        assert!(world.registry.list().is_empty(), "Remove Again finishes it");
        assert_eq!(world.record(), None);
    }

    /// A scope only the disk names (a sibling app's account under the shared
    /// root) is asked about too when a wipe is owed: its sweep would take it.
    #[test]
    fn the_serving_question_reaches_every_scope_the_sweep_would() {
        let world = World::new();
        let a = world.sign_in("a");
        let other = actor("cd");
        std::fs::create_dir_all(world.store_root.path().join(&other)).expect("scope");
        SignOutRecord::record_sign_out(&world.slot(), [a]);

        // A sibling APP: its own install base, the shared store root.
        let sibling_base = tempfile::tempdir().expect("sibling base");
        let mut sibling = SessionInstanceHolder::new();
        sibling.become_session_instance(
            ServingBases {
                state_base: Some(sibling_base.path()),
                store_root: Some(world.store_root.path()),
            },
            &other,
            None,
            ServingMode::Concurrent,
        );
        let outcome = world.finish(&mut Host::new(&world));
        assert!(outcome.fails_closed(), "{outcome:?}");
        assert!(world.store_root.path().join(&other).is_dir());
    }

    /// The residue record is written before the sign-out record is cleared:
    /// a survivor lands in the residue record, and the account leaves the
    /// sign-out record only then.
    #[cfg(unix)]
    #[test]
    fn survivors_go_to_the_residue_record_before_the_account_is_settled() {
        use std::os::unix::fs::PermissionsExt;
        let world = World::new();
        let a = world.sign_in("a");
        SignOutRecord::record_sign_out(&world.slot(), [a.clone()]);
        // The store root will not let its scope go.
        let root = world.store_root.path();
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o555)).expect("chmod");

        let outcome = world.finish(&mut Host::new(&world));

        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        if root.join(&a).is_dir() {
            let SignOutFinish::Finished {
                residue: Some(residue),
                ..
            } = &outcome
            else {
                panic!("a survivor is reported: {outcome:?}");
            };
            assert_eq!(residue.paths.len(), 1);
            assert_eq!(
                SignOutResidue::load(world.install.path()).as_ref(),
                Some(residue),
                "the residue record holds it"
            );
            assert_eq!(
                world.record(),
                None,
                "and the account left the sign-out record"
            );
        }
        // Run as root, the chmod refuses nothing and the erase is clean — the
        // ordering is then unobservable here and the run says nothing.
    }

    /// A removal joins the record with no wipe owed: only its account is
    /// erased — no agent exchange, no wipe, no residual sweep of others.
    #[test]
    fn a_removal_record_erases_its_one_account() {
        let world = World::new();
        let a = world.sign_in("a");
        let b = world.sign_in("b");
        SignOutRecord::record_removal(&world.slot(), &b);
        world.registry.remove(&b).expect("removed");
        // A scope nobody's registry names, which a sign-out's residual sweep
        // would take and a removal must not.
        let stray = actor("ef");
        std::fs::create_dir_all(world.store_root.path().join(&stray)).expect("scope");

        let mut host = Host::new(&world);
        world.finish(&mut host);
        assert_eq!(host.log, vec![format!("slots {b}")]);
        assert_eq!(
            world
                .registry
                .list()
                .into_iter()
                .map(|e| e.actor_id)
                .collect::<Vec<_>>(),
            vec![a.clone()]
        );
        let scopes = world.scopes();
        assert!(!scopes.contains(&b));
        assert!(scopes.contains(&a) && scopes.contains(&stray));
        assert_eq!(world.record(), None);
    }

    /// A removal the process never got to (killed before the registry step):
    /// the registry still names the account, so it is kept.
    #[test]
    fn a_removal_that_never_happened_keeps_the_account() {
        let world = World::new();
        let a = world.sign_in("a");
        SignOutRecord::record_removal(&world.slot(), &a);
        let mut host = Host::new(&world);
        world.finish(&mut host);
        assert!(host.log.is_empty());
        assert_eq!(world.registry.list().len(), 1);
        assert!(world.scopes().contains(&a));
        assert_eq!(world.record(), None);
    }

    /// The unreadable-index floor's start-over: a record with no accounts and
    /// the wipe owed sweeps every scope under the bases.
    #[test]
    fn a_record_naming_nobody_sweeps_every_scope() {
        let world = World::new();
        let _a = world.sign_in("a");
        world.app.set(crate::INDEX_KEY, "not json");
        SignOutRecord::record_sign_out(&world.slot(), std::iter::empty());
        world.finish(&mut Host::new(&world));
        assert!(world.scopes().is_empty());
        assert_eq!(world.app.get(crate::INDEX_KEY), None);
        assert_eq!(world.record(), None);
    }

    /// A malformed record file reads as a wipe owed: the launch finishes the
    /// sign-out over the registry rather than leave the user signed in.
    #[test]
    fn a_malformed_record_finishes_over_the_registry() {
        let world = World::new();
        let a = world.sign_in("a");
        std::fs::write(world.slot().path(), b"{not").expect("garbage");
        world.finish(&mut Host::new(&world));
        assert!(world.registry.list().is_empty());
        assert!(!world.scopes().contains(&a));
        assert_eq!(world.slot().read(), None);
    }

    /// No sign-out record, a residue record and nobody signed in: the
    /// re-sweep runs, as the launch's silent re-check always has; with
    /// somebody signed in it waits.
    #[test]
    fn a_residue_alone_is_re_swept_only_while_nobody_is_signed_in() {
        let world = World::new();
        let gone = world.store_root.path().join(actor("ab"));
        std::fs::create_dir_all(&gone).expect("scope");
        let record =
            SignOutResidue::record(std::slice::from_ref(&gone), &CredentialSweep::default());
        record.save(world.install.path()).expect("saved");

        let _a = world.sign_in("a");
        assert_eq!(world.finish(&mut Host::new(&world)), SignOutFinish::NOTHING);
        assert!(gone.is_dir(), "a signed-in launch leaves the residue alone");

        let _ = world.registry.wipe_own();
        let outcome = world.finish(&mut Host::new(&world));
        assert_eq!(
            outcome,
            SignOutFinish::Finished {
                residue: None,
                credentials_wiped: false,
                found_record: false,
            }
        );
        assert!(!gone.exists());
        assert!(SignOutResidue::load(world.install.path()).is_none());
    }
}
