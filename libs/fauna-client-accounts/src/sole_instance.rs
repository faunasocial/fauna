//! **Is anybody else still serving this account?** — the question a sign-out
//! or a remove-account must ask before it erases anything
//! (`account-scoping.md` § Concurrent instances → *An erase refuses while a
//! sibling serves the account*).
//!
//! # Why an erase has to ask at all
//!
//! A retired app serves [`ServingMode::Concurrent`], so two instances of one
//! account coexist over the multi-process-safe account store. Sign-out erases
//! every account-scoped store, and remove-account erases one actor's — and
//! until this module existed, neither asked whether a second instance was
//! still running. On POSIX the sibling then kept its session, its bearer and
//! its keys in memory for an account the user had signed out of, while the
//! directory under it was unlinked: its conversations-engine role lock became
//! a lock on an unlinked inode, so the next sign-in created a fresh lock file
//! at the same path and took the same role beside it — the very race
//! [`crate::lock_file`] exists to close ("never delete a lock file"). On
//! windows the sibling's open handles failed the erase instead, leaving the
//! signed-out account's data on disk.
//!
//! # Why the answer needs this process to put its own lock down
//!
//! Under the concurrent law the erasing instance holds a **shared** lock on
//! its own account, so an exclusive probe beside it always answers "served" —
//! it cannot tell a sibling from its own reflection. So for the session's own
//! account the probe runs inside
//! [`SessionInstanceHolder::without_own_lock`], which puts this process's lock
//! down for the length of the probe and takes it again afterwards. For every
//! *other* account in the registry — the rest of a sign-out's sweep — this
//! process holds nothing, so the exclusive probe answers directly.
//!
//! # Degrade open, and why
//!
//! A probe that cannot reach the lock file at all reports the account as free.
//! The alternative is an erase that refuses because of a filesystem hiccup,
//! which would leave a user unable to sign out of their own device — and this
//! module's whole purpose is to keep a signed-out account's data from
//! outliving the sign-out. That is the same degrade-open posture every other
//! reader of these locks takes ([`AccountInstanceLock::is_served`]).

use std::path::Path;

use crate::instance_lock::{AccountInstanceLock, InstanceLockOutcome, ServingBases};

/// Which of `actor_ids` **another live instance is still serving**, in the
/// order given — the accounts an erase must leave alone.
///
/// Empty is the ordinary answer: one window, signing out. A non-empty answer
/// is a refusal the caller must carry all the way out to the user, *before*
/// erasing anything and before wiping credentials — a sign-out that erased
/// nothing but still discarded the credential namespace would strand the
/// account's stores under a writer key nobody holds.
///
/// `bases` names the two places an instance declares itself
/// ([`ServingBases`]): this app's install-scoped base, where a sibling *window
/// of this app* holds its instance lock, and the shared account-store root,
/// where an instance of *any* app holds the serving lock. Either half that
/// cannot be resolved reports nothing served — the degrade-open posture the
/// module docs give — and the two degrade independently.
///
/// ⚠ **Scope: every app that declares itself at the shared root.** Until
/// 2026-09-20 this looked under the install base alone, so a tui sign-out could
/// not see a live linux instance and erased the shared store out from under it.
/// A seat that passes no `store_root` when it *becomes* a session instance is
/// still invisible to its sibling apps (`account-scoping.md` names which).
pub fn actors_served_by_another_instance<'a, S: AsRef<str>>(
    bases: impl Into<ServingBases<'a>>,
    actor_ids: &[S],
) -> Vec<String> {
    let bases = bases.into();
    actor_ids
        .iter()
        .map(|a| a.as_ref())
        .filter(|actor| served_by_another(bases, actor))
        .map(|actor| actor.to_string())
        .collect()
}

/// The one-account probe: is anybody **other than this process** serving it?
///
/// The exclusive acquire is the arbiter, exactly as it is at launch — taking
/// it proves nobody else holds the file, and dropping it immediately restores
/// the state the caller found. A `Refused` is the honest "a sibling serves
/// this account"; a `Degraded` reports free (see the module docs).
fn served_by_another(bases: ServingBases<'_>, actor_id_hex: &str) -> bool {
    crate::instance_lock::without_process_lock(actor_id_hex, || {
        served_at_either_base(bases, actor_id_hex)
    })
}

/// Both halves of the question, with this process's own locks already down.
///
/// The install base is still asked, and not only the shared root, because the
/// root sees only instances new enough to declare themselves there: a sibling
/// window of this app from before the serving lock existed holds the
/// install-base lock alone, and it is every bit as live.
fn served_at_either_base(bases: ServingBases<'_>, actor_id_hex: &str) -> bool {
    served_by_this_app(bases.state_base, actor_id_hex)
        || bases.store_root.is_some_and(|root| {
            fauna_account_store::locks::ServingLock::is_served(root, actor_id_hex)
        })
}

/// The install-base half: a sibling **window of this app**.
fn served_by_this_app(state_base: Option<&Path>, actor_id_hex: &str) -> bool {
    let Some(base) = state_base else {
        return false;
    };
    match AccountInstanceLock::acquire(base, actor_id_hex) {
        InstanceLockOutcome::Held(_) => false,
        InstanceLockOutcome::Refused => true,
        InstanceLockOutcome::Degraded => false,
    }
}

/// The all-accounts erase's question — sign-out, and the unreadable-index
/// floor that runs the same erase: **may this device erase them all?**
///
/// `Some` means no — another instance, of this app or a sibling app, still
/// serves the named accounts, so the caller must refuse the gesture
/// *entirely*: erase nothing, and do not wipe the credential namespace either.
/// Wiping credentials after a refused erase would strand every account's
/// stores under a writer key nobody holds, which is the failure
/// `account-scoping.md` § Erasure follows scope warns about, arrived at from
/// the other side.
///
/// ⚠ **Asked about everything the erase reaches, which is more than the
/// registry.** `swept_bases` are the directories the caller's erase sweeps, and
/// every actor scope under them is asked about beside the registry's accounts
/// ([`fauna_account_store::db::account_scopes_under`]): the sweep removes each
/// one whether or not the registry names it, a malformed index names nothing
/// at all — the floor's whole situation — and the shared store root holds
/// accounts only a *sibling app's* registry lists. Fed by the registry alone,
/// this gate asked about nothing on the floor and could not see an account
/// only linux had signed in to from a tui sign-out.
///
/// Shared rather than per-seat because the answer is one rule and the seats
/// only differ in where they paint it (the lines are
/// [`crate::erase_residue::sign_out_blocked_copy`] and
/// [`crate::erase_residue::start_over_blocked_copy`]).
pub fn sign_out_blocked<'a>(
    registry: &crate::AccountRegistry,
    bases: impl Into<ServingBases<'a>>,
    swept_bases: &[&Path],
) -> Option<EraseBlocked> {
    let mut actors: Vec<String> = registry
        .list()
        .into_iter()
        .map(|entry| entry.actor_id)
        .collect();
    for base in swept_bases {
        let mut on_disk = fauna_account_store::db::account_scopes_under(base);
        // `read_dir` order is the filesystem's; sorted, the refusal's log line
        // names the same accounts in the same order every time.
        on_disk.sort();
        for actor in on_disk {
            if !actors.contains(&actor) {
                actors.push(actor);
            }
        }
    }
    blocked(bases, &actors)
}

/// Remove-account's question: may this device erase **one** account's scopes?
///
/// Two ways the answer is no, asked in this order:
///
/// 1. **This process serves it** ([`RemoveAccountBlocked::ServedHere`]). A
///    bound secondary serves an account that is *not* the registry's active
///    one, so a switcher keyed on the registry offered it for removal — and the
///    sibling probe below cannot catch that, because it puts this process's own
///    lock down by design (a sign-out must not meet its own reflection, and a
///    sign-out tears this process down afterwards; a remove does not). So the
///    process's own account is refused before anything else is asked.
/// 2. **A sibling serves it** ([`RemoveAccountBlocked::ServedElsewhere`]) —
///    the same probe as [`sign_out_blocked`], asked about the one actor the
///    gesture erases; a sibling serving some *other* account is none of this
///    gesture's business.
///
/// The caller asks **before** it touches the registry: the registry removal
/// drops the account's secret slots, so a refusal after it would leave the
/// account's stores on disk with nothing left to sign in to them.
pub fn remove_account_blocked<'a>(
    bases: impl Into<ServingBases<'a>>,
    actor_id_hex: &str,
) -> Option<RemoveAccountBlocked> {
    let serving_here = crate::instance_lock::process_session_account();
    remove_account_blocked_as(serving_here.as_deref(), bases, actor_id_hex)
}

/// [`remove_account_blocked`] with the account this process serves passed in
/// rather than read from the process-global holder — the pure half a seat's
/// own tests drive, so they need not seed process state.
pub fn remove_account_blocked_as<'a>(
    serving_here: Option<&str>,
    bases: impl Into<ServingBases<'a>>,
    actor_id_hex: &str,
) -> Option<RemoveAccountBlocked> {
    if serving_here.is_some_and(|here| here.trim().eq_ignore_ascii_case(actor_id_hex.trim())) {
        return Some(RemoveAccountBlocked::ServedHere);
    }
    blocked(bases, &[actor_id_hex]).map(RemoveAccountBlocked::ServedElsewhere)
}

/// Why a remove-account was refused — see [`remove_account_blocked`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveAccountBlocked {
    /// This process serves the account: removing it would unlink the stores
    /// this window is running from.
    ServedHere,
    /// Another live instance serves it.
    ServedElsewhere(EraseBlocked),
}

impl RemoveAccountBlocked {
    /// The line the seat paints on the Settings page's `error-message`. Each
    /// names its own remedy: closing *another* window does nothing for an
    /// account *this* window serves.
    pub fn copy(&self) -> fauna_core::localized::LocalizedText {
        match self {
            Self::ServedHere => crate::erase_residue::remove_account_served_here_copy(),
            Self::ServedElsewhere(_) => crate::erase_residue::remove_account_blocked_copy(),
        }
    }
}

fn blocked<'a, S: AsRef<str>>(
    bases: impl Into<ServingBases<'a>>,
    actor_ids: &[S],
) -> Option<EraseBlocked> {
    let served = actors_served_by_another_instance(bases, actor_ids);
    (!served.is_empty()).then_some(EraseBlocked { accounts: served })
}

/// An erase refused because another instance is still serving one of the
/// accounts it would reach — see [`sign_out_blocked`] and
/// [`remove_account_blocked`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EraseBlocked {
    /// The actor ids another instance is serving: the registry's order first,
    /// then what only the disk named. Reported for the log; the user-facing
    /// line names no account, because the remedy (close the other window) is
    /// the same whichever one it is.
    pub accounts: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance_lock::{ServingMode, SessionInstanceHolder};

    /// A 64-hex-char actor id — what `account_instance_token` accepts, and
    /// what every caller passes (registry-resolved ids).
    fn actor(pair: &str) -> String {
        pair.repeat(32)
    }

    /// The ordinary sign-out: nothing else holds the account, so nothing is
    /// withheld from the erase.
    #[test]
    fn an_unserved_account_is_not_withheld() {
        let base = tempfile::tempdir().expect("temp base");
        assert!(
            actors_served_by_another_instance(Some(base.path()), &[actor("a1")]).is_empty(),
            "no instance holds this account, so the erase must proceed"
        );
    }

    /// ⚠ **The defect this module exists for**: a second instance of the same
    /// account is live, and the erase must leave that account alone rather
    /// than unlink the directory its engine is running out of.
    #[test]
    fn an_account_a_sibling_serves_is_withheld() {
        let base = tempfile::tempdir().expect("temp base");
        let account = actor("b2");

        let mut sibling = SessionInstanceHolder::new();
        sibling.become_session_instance(Some(base.path()), &account, None, ServingMode::Concurrent);
        assert!(sibling.holds_lock(), "the sibling is genuinely serving it");

        assert_eq!(
            actors_served_by_another_instance(Some(base.path()), std::slice::from_ref(&account)),
            vec![account.clone()],
            "an erase must not unlink the directory a live instance is serving from"
        );

        drop(sibling);
        assert!(
            actors_served_by_another_instance(Some(base.path()), &[account]).is_empty(),
            "and once that instance is gone the erase proceeds"
        );
    }

    /// Only the served accounts are withheld — a sign-out sweeps the whole
    /// registry, and one live sibling must not hold back every other
    /// account's erase.
    #[test]
    fn only_the_served_account_is_withheld() {
        let base = tempfile::tempdir().expect("temp base");
        let served = actor("c3");
        let free = actor("d4");

        let mut sibling = SessionInstanceHolder::new();
        sibling.become_session_instance(Some(base.path()), &served, None, ServingMode::Concurrent);

        assert_eq!(
            actors_served_by_another_instance(Some(base.path()), &[served.clone(), free]),
            vec![served],
        );
    }

    /// Two apps on one OS login: each has its **own** install base, and the
    /// account store they both run out of is shared.
    struct TwoApps {
        tui_base: tempfile::TempDir,
        linux_base: tempfile::TempDir,
        store_root: tempfile::TempDir,
    }

    impl TwoApps {
        fn new() -> Self {
            Self {
                tui_base: tempfile::tempdir().expect("tui base"),
                linux_base: tempfile::tempdir().expect("linux base"),
                store_root: tempfile::tempdir().expect("store root"),
            }
        }
        fn tui(&self) -> ServingBases<'_> {
            ServingBases {
                state_base: Some(self.tui_base.path()),
                store_root: Some(self.store_root.path()),
            }
        }
        fn linux(&self) -> ServingBases<'_> {
            ServingBases {
                state_base: Some(self.linux_base.path()),
                store_root: Some(self.store_root.path()),
            }
        }
    }

    /// ⚠ **The cross-app residual this guard was widened for**: linux is live on
    /// the account, and a tui sign-out — which looks under tui's install base and
    /// finds no lock there at all — must still be told the account is served,
    /// because the store it is about to unlink is the one linux is running out of.
    #[test]
    fn an_account_a_sibling_app_serves_is_withheld() {
        let apps = TwoApps::new();
        let account = actor("f6");

        let mut linux = SessionInstanceHolder::new();
        linux.become_session_instance(apps.linux(), &account, None, ServingMode::Concurrent);

        assert_eq!(
            actors_served_by_another_instance(apps.tui(), std::slice::from_ref(&account)),
            vec![account.clone()],
            "tui cannot see linux's install-base lock; the shared root is where it must look"
        );

        drop(linux);
        assert!(
            actors_served_by_another_instance(apps.tui(), &[account]).is_empty(),
            "and once linux is gone tui signs out"
        );
    }

    /// …and in the other direction, so the guard is not an accident of which app
    /// happens to live nearer the store root.
    #[test]
    fn the_sibling_app_refusal_holds_in_the_other_direction() {
        let apps = TwoApps::new();
        let account = actor("a7");

        let mut tui = SessionInstanceHolder::new();
        tui.become_session_instance(apps.tui(), &account, None, ServingMode::Concurrent);

        assert_eq!(
            actors_served_by_another_instance(apps.linux(), std::slice::from_ref(&account)),
            vec![account],
        );
    }

    /// The single-app case still signs out: a lone instance holds BOTH locks
    /// itself, and neither of its own reflections may read as a sibling.
    #[test]
    fn a_lone_instance_is_not_its_own_sibling_at_the_shared_root() {
        let apps = TwoApps::new();
        let account = actor("b8");

        let mut ours = SessionInstanceHolder::new();
        ours.become_session_instance(apps.tui(), &account, None, ServingMode::Concurrent);

        let served =
            ours.without_own_lock(&account, || served_at_either_base(apps.tui(), &account));
        assert!(!served, "a lone instance refused its own sign-out");
        assert!(
            fauna_account_store::locks::ServingLock::is_served(apps.store_root.path(), &account),
            "and the presence lock must be back up afterwards, or the NEXT sibling's \
             erase cannot see this instance"
        );
    }

    /// A seat that cannot resolve its install base still asks at the shared
    /// root — the two halves degrade independently.
    #[test]
    fn a_missing_install_base_still_consults_the_shared_root() {
        let apps = TwoApps::new();
        let account = actor("c9");

        let mut linux = SessionInstanceHolder::new();
        linux.become_session_instance(apps.linux(), &account, None, ServingMode::Concurrent);

        let baseless = ServingBases {
            state_base: None,
            store_root: Some(apps.store_root.path()),
        };
        assert_eq!(
            actors_served_by_another_instance(baseless, std::slice::from_ref(&account)),
            vec![account],
        );
    }

    /// A client with no resolvable base reports nothing served — the erase
    /// still runs. A filesystem this process cannot read must not become a
    /// device the user cannot sign out of.
    #[test]
    fn no_state_base_degrades_open() {
        assert!(actors_served_by_another_instance(None, &[actor("e5")]).is_empty());
    }

    /// A registry over nothing but memory, holding `accounts` — what a seat's
    /// credential store looks like to the gate.
    fn registry_with(accounts: &[&str]) -> crate::AccountRegistry {
        let registry =
            crate::AccountRegistry::new(std::sync::Arc::new(crate::InMemorySecretStore::new()));
        for secret in accounts {
            registry
                .add_account(secret, None, None)
                .expect("add a test account");
        }
        registry
    }

    /// An actor scope on disk under `base`, as a signed-in instance leaves one.
    fn scope_on_disk(base: &std::path::Path, actor_id: &str) {
        std::fs::create_dir_all(base.join(actor_id)).expect("actor scope dir");
    }

    /// ⚠ **The erase reaches further than the registry, so the question must
    /// too.** Every all-accounts erase sweeps each `<base>/<64-hex>/` scope on
    /// disk, registry or not (`fauna_account_store::db::erase_all_account_scopes`)
    /// — and the store root is shared between apps, so it holds accounts only a
    /// *sibling app's* registry names. Asked about this app's registry alone,
    /// the gate waved through the erase of a store linux was serving.
    #[test]
    fn a_sign_out_asks_about_every_scope_its_erase_reaches_not_only_the_registry() {
        let apps = TwoApps::new();
        let linux_only = actor("d1");
        scope_on_disk(apps.store_root.path(), &linux_only);

        let mut linux = SessionInstanceHolder::new();
        linux.become_session_instance(apps.linux(), &linux_only, None, ServingMode::Concurrent);

        let tui_registry = registry_with(&[]);
        let swept = [apps.tui_base.path(), apps.store_root.path()];
        assert_eq!(
            sign_out_blocked(&tui_registry, apps.tui(), &swept),
            Some(EraseBlocked {
                accounts: vec![linux_only.clone()]
            }),
            "tui's sign-out sweeps the shared root, so it must ask about what is there"
        );

        drop(linux);
        assert_eq!(sign_out_blocked(&tui_registry, apps.tui(), &swept), None);
    }

    /// ⚠ **The index-reset floor runs the same erase over an index it cannot
    /// read**, and a malformed index lists no accounts at all — so a gate fed
    /// by the registry alone asks about nothing and refuses nothing. The disk
    /// is the list the floor can trust (`account-scoping.md` § Concurrent
    /// instances → *An erase refuses while a sibling serves the account*).
    #[test]
    fn a_malformed_index_still_asks_about_what_is_on_disk() {
        let apps = TwoApps::new();
        let served = actor("e2");
        scope_on_disk(apps.tui_base.path(), &served);

        let mut sibling = SessionInstanceHolder::new();
        sibling.become_session_instance(apps.tui(), &served, None, ServingMode::Concurrent);

        let store = std::sync::Arc::new(crate::InMemorySecretStore::new());
        store.seed(crate::INDEX_KEY, "{ this is not an index");
        let malformed = crate::AccountRegistry::new(store);
        assert_eq!(
            malformed.index_refusal(),
            Some(fauna_launch_machine::AccountIndexRefusal::Malformed),
            "the precondition: this is the floor's index"
        );
        assert!(malformed.list().is_empty(), "and it names nobody");

        let swept = [apps.tui_base.path(), apps.store_root.path()];
        assert_eq!(
            sign_out_blocked(&malformed, apps.tui(), &swept),
            Some(EraseBlocked {
                accounts: vec![served]
            }),
        );
    }

    /// The registry still counts: an account it names is asked about even when
    /// no scope for it exists on disk yet, and one named twice is asked once.
    #[test]
    fn registry_accounts_are_asked_about_once_whether_or_not_they_are_on_disk() {
        const SECRET: &str = "0101010101010101010101010101010101010101010101010101010101010101";
        let registry = registry_with(&[SECRET]);
        let registered = registry.list()[0].actor_id.clone();

        let apps = TwoApps::new();
        let mut sibling = SessionInstanceHolder::new();
        sibling.become_session_instance(apps.tui(), &registered, None, ServingMode::Concurrent);

        assert_eq!(
            sign_out_blocked(&registry, apps.tui(), &[apps.tui_base.path()]),
            Some(EraseBlocked {
                accounts: vec![registered.clone()]
            }),
            "no scope on disk, but the credential wipe still reaches it"
        );

        scope_on_disk(apps.tui_base.path(), &registered);
        assert_eq!(
            sign_out_blocked(&registry, apps.tui(), &[apps.tui_base.path()])
                .map(|b| b.accounts.len()),
            Some(1),
            "the registry and the disk naming one account is one account"
        );
    }

    /// Remove-account erases ONE actor's scopes, so it asks about that actor
    /// alone: a sibling serving some other account must not stop it.
    #[test]
    fn remove_account_asks_about_the_one_account_it_erases() {
        let apps = TwoApps::new();
        let removed = actor("f3");
        let other = actor("a4");

        let mut sibling = SessionInstanceHolder::new();
        sibling.become_session_instance(apps.linux(), &other, None, ServingMode::Concurrent);
        assert_eq!(remove_account_blocked_as(None, apps.tui(), &removed), None);

        let mut on_removed = SessionInstanceHolder::new();
        on_removed.become_session_instance(apps.linux(), &removed, None, ServingMode::Concurrent);
        assert_eq!(
            remove_account_blocked_as(None, apps.tui(), &removed),
            Some(RemoveAccountBlocked::ServedElsewhere(EraseBlocked {
                accounts: vec![removed]
            })),
        );
    }

    /// ⚠ **The self-inflicted erase.** A bound secondary serves B while the
    /// registry's active account is A, so a switcher keyed on the registry
    /// offered B for removal — and the sibling probe, which puts this process's
    /// own locks down by design, found nobody else on B and waved the erase of
    /// this very process's stores through. With no sibling anywhere, the
    /// process's own account must still be refused.
    #[test]
    fn remove_account_refuses_the_account_this_process_serves() {
        let apps = TwoApps::new();
        let served_here = actor("b5");

        let mut ours = SessionInstanceHolder::new();
        ours.become_session_instance(apps.tui(), &served_here, None, ServingMode::Concurrent);
        let here = ours.serving_actor().map(str::to_string);
        assert_eq!(here.as_deref(), Some(served_here.as_str()));

        assert_eq!(
            remove_account_blocked_as(here.as_deref(), apps.tui(), &served_here),
            Some(RemoveAccountBlocked::ServedHere),
            "the account this process serves is never erased from under it"
        );
        assert_eq!(
            remove_account_blocked_as(
                here.as_deref(),
                apps.tui(),
                &served_here.to_ascii_uppercase()
            ),
            Some(RemoveAccountBlocked::ServedHere),
            "actor ids compare as hex, not as bytes"
        );
        assert_eq!(
            remove_account_blocked_as(here.as_deref(), apps.tui(), &actor("c6")),
            None,
            "and another account nobody serves is still removable from here"
        );
    }

    /// The two refusals paint different lines, because the remedies differ:
    /// closing another window does nothing for an account this one serves.
    #[test]
    fn each_remove_refusal_names_its_own_remedy() {
        assert_ne!(
            RemoveAccountBlocked::ServedHere.copy(),
            RemoveAccountBlocked::ServedElsewhere(EraseBlocked { accounts: vec![] }).copy(),
        );
    }
}
