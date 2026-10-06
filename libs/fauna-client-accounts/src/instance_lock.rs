//! The (OS login, account) instance lock.
//!
//! Whether a second same-account instance may run is **per app** — the
//! app's [`ServingMode`], re-ratified 2026-08-15 with W5.6 (account-data-plane.md § Workstreams)
//! (`docs/goal/architecture/apps/account-scoping.md` § Concurrent
//! instances): a *retired* app's servers take a **shared** lock and
//! coexist (the genuinely exclusive work is carried by the three critical
//! sections' own locks), while an *un-retired* app keeps the
//! exclusive-or-die law — two instances of the same account are refused.
//! Each platform's leg re-keys its per-OS-login guard to
//! per-(OS login, account) — and apple, which has no app-level guard of its
//! own, *adds* one. This module is the shared mechanism all seven legs
//! consume, so "is this account already served?" has exactly one answer
//! everywhere.
//!
//! **Never blocking.** Unlike [`MutationLock`] — a short critical section
//! every mutator waits through — a same-account collision must resolve
//! *now*: an exclusive acquire try-locks and reports
//! [`Refused`](InstanceLockOutcome::Refused) instead of waiting (there is
//! nothing coherent for the loser to do while the holder lives — the same
//! shape as the sync agent's `fauna_ipc::unix_transport::InstanceLock`),
//! and a shared acquire's bounded millisecond retry exists only to absorb
//! the probe's two-syscall window, never to queue behind a real holder.
//!
//! **Crash-safe by construction.** The kernel releases the lock when its
//! holder dies (`flock` on unix, `LockFileEx` on windows, via std's
//! `File::try_lock`), so a crashed instance leaves no stale lock to
//! reconcile at next launch. The lock **file** is deliberately never
//! deleted: unlinking a lock file re-opens the race it closes (a new
//! acquirer can lock the orphaned inode while another creates a fresh file
//! at the same path). It is a zero-byte `0600` file whose name — not its
//! content — is the key.
//!
//! **Degrades open.** A lock-file I/O failure yields
//! [`Degraded`](InstanceLockOutcome::Degraded): the caller proceeds
//! unguarded — exactly the pre-guard behavior — rather than turning a
//! filesystem hiccup into a client that refuses to launch at all (the
//! works-out-of-the-box invariant; same stance as [`MutationLock`]'s
//! degrade). The lock narrows a race; it must not widen a failure.
//!
//! **The key is (state base, actor id).** The lock file lives in the
//! install-scoped state base — the directory the per-account state dirs hang
//! off (apple's `AccountStateDir.base`, the same base the mutation lock
//! uses) — as a *sibling* of the actor dirs, named `instance-<hex>.lock`.
//! Sibling, not member, because account-erasure sweeps delete the actor
//! *dirs*; a held lock file must never be unlinked (above).
//!
//! [`MutationLock`]: crate::MutationLock

#![cfg(not(target_arch = "wasm32"))]

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// The normalized per-account token — the one derivation every per-account
/// *name* in the concurrent-instances design keys off.
///
/// `None` when `actor_id_hex` is not one of ours after normalization (trim +
/// ASCII-lowercase, the same rule as the `FAUNA_BOUND_ACCOUNT` read), so
/// differently-cased spellings of one account always produce one token.
///
/// Shared rather than inlined because two *different* mechanisms key off it
/// and they must never key differently (`account-scoping.md` § Concurrent
/// instances → *The per-(OS login, account) raise channel*: "the account
/// token in the endpoint name is the same normalized actor token the
/// per-account lock file uses — one shared derivation in
/// `fauna_client_accounts`"):
///
/// - the per-account **lock file** `instance-<token>.lock` (this module), and
/// - the per-account **activation endpoint** each desktop platform claims
///   while it serves the account — linux's D-Bus well-known name
///   `social.fauna.fauna.a<token>`, windows' `Local\FaunaApp-Activate-<token>`
///   named event.
///
/// A raiser derives the endpoint name from the account it wants to reach and
/// the server derives it from the account it serves; if those two derivations
/// could drift, focus-existing would silently address a name nobody owns —
/// indistinguishable from "the instance died", and therefore a bug that
/// degrades into a plausible-looking wrong answer rather than an error.
pub fn account_instance_token(actor_id_hex: &str) -> Option<String> {
    let actor = actor_id_hex.trim().to_ascii_lowercase();
    is_actor_hex(&actor).then_some(actor)
}

/// How this app serves an account — which lock a server takes
/// (`account-scoping.md` § Concurrent instances, re-ratified with W5.6).
///
/// Every app's servers take a *shared* lock: any number of same-account
/// instances coexist, and the three genuinely exclusive critical sections —
/// schema migration, the engine-singleton role, the conversations-engine
/// role — carry their own locks. The pre-retirement exclusive serving mode
/// is gone (no installation predates the retirement, so there is no
/// old-binary skew to interlock with); the exclusive lock survives only as
/// the *probe* ([`AccountInstanceLock::acquire`], [`AccountInstanceLock::is_served`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServingMode {
    /// Same-account instances coexist; serving takes a shared lock, which
    /// keeps [`AccountInstanceLock::is_served`]'s exclusive probe truthful
    /// for the raise channel and the chooser.
    Concurrent,
}

/// Result of [`AccountInstanceLock::acquire`].
pub enum InstanceLockOutcome {
    /// This process is now the account's single instance; the lock releases
    /// when the value drops (or the process dies).
    Held(AccountInstanceLock),
    /// Another live process already serves this account. The caller must
    /// refuse to run as it — terminally, never by falling back onto a
    /// different account (`account-scoping.md` § Concurrent instances).
    ///
    /// ⚠ One narrow way this can be transiently wrong, worth knowing because
    /// it is invisible from here: `flock` lives on the *open file
    /// description*, and `fork` duplicates every descriptor into the child
    /// until its `exec` closes our `O_CLOEXEC` ones. A lock released while
    /// some thread of the releasing process is mid-`Command::spawn` therefore
    /// stays held by the child's copy for that window, so an acquirer racing
    /// the release can see `Refused` microseconds after the account really
    /// became free. Harmless in a client (a relaunch takes it; nothing is
    /// stranded), and not worth a retry on an arbitration path — but it is
    /// exactly why this module's "the release freed it" tests poll rather than
    /// read once (`reacquire_released` in the test module).
    Refused,
    /// The lock could not be taken for a reason other than a live holder
    /// (I/O failure, or an actor id that is not a well-formed key). Proceed
    /// unguarded — pre-guard behavior — after logging; see module docs.
    Degraded,
}

/// Held (OS login, account) instance lock; RAII — dropping releases.
pub struct AccountInstanceLock {
    /// Held (and thereby locked) until drop; never read.
    _file: File,
}

impl AccountInstanceLock {
    /// Try to become the single instance for `actor_id_hex` under
    /// `state_base` (the install-scoped directory the per-account state dirs
    /// hang off). Never blocks.
    ///
    /// The actor id is normalized by the shared
    /// [`account_instance_token`] (trim + ASCII-lowercase — the same rule as
    /// the `FAUNA_BOUND_ACCOUNT` read) so differently-cased spellings of one
    /// account contend on one file. A value that is not 64 hex chars after
    /// normalization is not one of ours and yields `Degraded` rather than
    /// minting a stray lock file (callers pass registry-resolved ids, so
    /// this is defensive, not a validation gate — `bind_account` stays the
    /// single gate for bindings).
    pub fn acquire(state_base: &Path, actor_id_hex: &str) -> InstanceLockOutcome {
        let Some(actor) = account_instance_token(actor_id_hex) else {
            return InstanceLockOutcome::Degraded;
        };
        let lock_path = state_base.join(format!("instance-{actor}.lock"));
        match open_and_try_lock(&lock_path) {
            Ok(Some(file)) => InstanceLockOutcome::Held(Self { _file: file }),
            Ok(None) => InstanceLockOutcome::Refused,
            Err(_) => InstanceLockOutcome::Degraded,
        }
    }

    /// Serve `actor_id_hex` **concurrently** — the
    /// [`ServingMode::Concurrent`] acquire: a *shared* lock on the same
    /// per-account file, so any number of retired-app instances coexist
    /// while [`Self::is_served`]'s exclusive probe (the sign-out probe) still
    /// sees the instance as served.
    ///
    /// **Bounded retry instead of a one-shot try.** A shared acquire can see
    /// `WouldBlock` from two very different holders: [`Self::is_served`]'s
    /// two-syscall exclusive probe window (microseconds — under the
    /// exclusive law that window was documented as a harmless transient
    /// false `Refused`; under the concurrent law it would wrongly refuse a
    /// *legitimate* co-server, so it must be absorbed), or a genuine
    /// exclusive holder — a pre-retirement binary of this app serving the
    /// account. A few millisecond-spaced retries tell them apart: the probe
    /// window clears within the first retry, while a real exclusive holder
    /// outlasts the budget and yields the honest
    /// [`Refused`](InstanceLockOutcome::Refused).
    pub fn acquire_shared(state_base: &Path, actor_id_hex: &str) -> InstanceLockOutcome {
        let Some(actor) = account_instance_token(actor_id_hex) else {
            return InstanceLockOutcome::Degraded;
        };
        let lock_path = state_base.join(format!("instance-{actor}.lock"));
        let file = match crate::lock_file::open_lock_file(&lock_path) {
            Ok(f) => f,
            Err(_) => return InstanceLockOutcome::Degraded,
        };
        // 10 × 2 ms — orders of magnitude above the probe's two-syscall
        // window, three orders below anything a human perceives at launch.
        const ATTEMPTS: u32 = 10;
        const RETRY_SPACING: std::time::Duration = std::time::Duration::from_millis(2);
        for attempt in 0..ATTEMPTS {
            match file.try_lock_shared() {
                Ok(()) => return InstanceLockOutcome::Held(Self { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if attempt + 1 < ATTEMPTS {
                        std::thread::sleep(RETRY_SPACING);
                    }
                }
                Err(std::fs::TryLockError::Error(_)) => return InstanceLockOutcome::Degraded,
            }
        }
        InstanceLockOutcome::Refused
    }

    /// **Display-only** probe: is `actor_id_hex` currently served by a live
    /// instance under `state_base`?
    ///
    /// This answers the launch-collision chooser's one question — "which
    /// accounts can I offer?" (`account-scoping.md` § Concurrent instances:
    /// the chooser lists "the registry's not-currently-served accounts (a
    /// display-only probe of the per-account lock files; **arbitration stays
    /// at acquire**)"). It is advisory by construction: an account free at
    /// probe time can be taken before the human clicks, so the pick still
    /// goes through [`Self::acquire`], which is the only gate.
    ///
    /// Two deliberate differences from `acquire`:
    ///
    /// - **It never creates the lock file.** `acquire` creates because it is
    ///   claiming; a probe that created would mint a file per never-launched
    ///   account just by rendering a list. An absent file therefore reads as
    ///   "not served", which is also the truthful answer — no instance can
    ///   hold a lock on a file that does not exist.
    /// - **It answers `false` on every failure** (missing file, I/O error,
    ///   malformed key), the same degrade-open posture as the rest of this
    ///   module: a filesystem hiccup must narrow the chooser's list, never
    ///   turn it into a dead end. A wrongly-offered account is refused a
    ///   moment later at acquire; a wrongly-withheld one leaves the user with
    ///   no way in.
    ///
    /// ⚠ **The probe takes the lock for the length of two syscalls.** `flock`
    /// has no non-destructive "is this locked?" test, so the only way to
    /// observe a free lock is to take it and release it immediately. A
    /// concurrent `acquire` from another process that lands inside that
    /// microsecond window sees `Refused` and refuses terminally. That is
    /// acceptable *here* and only here: the probe runs in a process that has
    /// already collided (chooser render), over accounts it is about to offer
    /// a human, not on any hot path. Do **not** reach for this as a cheap
    /// pre-check before `acquire` — `acquire` already reports `Refused`, and
    /// probing first would double the window for no gain.
    pub fn is_served(state_base: &Path, actor_id_hex: &str) -> bool {
        let Some(actor) = account_instance_token(actor_id_hex) else {
            return false;
        };
        let lock_path = state_base.join(format!("instance-{actor}.lock"));
        // `read(true)` without `create` — see above. A shared lock would be
        // enough to observe an exclusive holder, but std exposes only the
        // exclusive try-lock, and the window is identical either way.
        let Ok(file) = OpenOptions::new().read(true).write(true).open(&lock_path) else {
            return false;
        };
        match file.try_lock() {
            // We took it, so nobody held it. Release immediately (dropping
            // `file` unlocks) and report free.
            Ok(()) => false,
            Err(std::fs::TryLockError::WouldBlock) => true,
            Err(std::fs::TryLockError::Error(_)) => false,
        }
    }

    /// The chooser's list, in one call: those of `actor_ids` **not**
    /// currently served, order preserved.
    ///
    /// Shared rather than left to each app because all three chooser
    /// platforms (windows, linux, tui — ui.yaml `launch_instance_chooser`)
    /// need exactly this filter, and "which accounts may I offer?" must have
    /// one answer everywhere (priority #1). Same advisory contract as
    /// [`Self::is_served`].
    pub fn not_currently_served<S: AsRef<str>>(state_base: &Path, actor_ids: &[S]) -> Vec<String> {
        actor_ids
            .iter()
            .map(|a| a.as_ref())
            .filter(|a| !Self::is_served(state_base, a))
            .map(str::to_string)
            .collect()
    }
}

/// The launch-collision chooser's list: `entries` filtered down to the
/// accounts no live instance currently serves, as `(actor_id, display
/// label)` pairs in registry order. The account that collided is excluded by
/// the probe itself (a live holder is why the chooser rendered at all).
///
/// Shared rather than left to each app because every chooser platform
/// (windows, linux, tui — ui.yaml `launch_instance_chooser`) must answer
/// "which accounts may I offer?" identically (priority #1): the filter is
/// [`AccountInstanceLock::not_currently_served`] and the label is
/// [`fauna_core::format::account_display_label`].
///
/// Base and registry rows are passed in rather than read from the
/// environment, so the filter is testable without a real credential store. A
/// caller with no resolvable base should skip this entirely and offer
/// nothing — there is no way to tell free from served without one.
pub fn choosable_accounts(
    base: &Path,
    entries: &[(String, Option<String>)],
) -> Vec<(String, String)> {
    let ids: Vec<&str> = entries.iter().map(|(id, _)| id.as_str()).collect();
    let free = AccountInstanceLock::not_currently_served(base, &ids);
    entries
        .iter()
        .filter(|(id, _)| free.iter().any(|f| f == id))
        .map(|(id, handle)| {
            (
                id.clone(),
                fauna_core::format::account_display_label(handle.as_deref(), id),
            )
        })
        .collect()
}

/// What the launch-collision chooser's focus-existing button resolves to
/// (`account-scoping.md` § Concurrent instances → the raise channel, "the
/// ratified degrade").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusExistingOutcome {
    /// The serving instance was raised; this process exits.
    Raised,
    /// Nothing serves the account any more — the sibling went away between
    /// the probe that rendered the chooser and the click. There is no
    /// collision left, so this process continues as the plain launch it was
    /// always trying to be.
    NoLongerServed,
    /// Still served, but there was no channel to raise it over (a client
    /// that claims no endpoint, or one this process cannot reach). The seat
    /// says so; it must never exit silently and leave the user with nothing.
    StillServedNoChannel,
}

/// The focus-existing decision every chooser seat makes, in one place: try
/// the raise; if it did not land, **re-probe the lock** rather than guess —
/// an unanswered raise looks the same whether the sibling is gone or merely
/// unreachable, and only the lock tells them apart.
///
/// `try_raise` is the seat's own channel (linux's per-account bus name,
/// windows' named event); a seat with no channel (tui) passes one that
/// returns `false`. `is_served` is normally [`AccountInstanceLock::is_served`]
/// over the install's state base; both are passed in so the decision is
/// testable without lock files or a bus. An empty `served_actor_id` has
/// nothing to address or re-probe, so it reports still-served: this process
/// has no evidence the account is free. (The windows twin is
/// `LaunchCollisionGate.ResolveFocusExisting`, which this mirrors arm for arm.)
pub fn resolve_focus_existing(
    served_actor_id: &str,
    try_raise: impl FnOnce(&str) -> bool,
    is_served: impl FnOnce(&str) -> bool,
) -> FocusExistingOutcome {
    if served_actor_id.trim().is_empty() {
        return FocusExistingOutcome::StillServedNoChannel;
    }
    if try_raise(served_actor_id) {
        return FocusExistingOutcome::Raised;
    }
    if !is_served(served_actor_id) {
        return FocusExistingOutcome::NoLongerServed;
    }
    FocusExistingOutcome::StillServedNoChannel
}

/// `Ok(Some(file))` = locked; `Ok(None)` = a live holder exists; `Err` = I/O.
fn open_and_try_lock(lock_path: &Path) -> std::io::Result<Option<File>> {
    let file = crate::lock_file::open_lock_file(lock_path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// A 64-char lowercase-hex actor id — the only key shape this lock mints
/// files for (mirrors the state-dir derivation's rule).
fn is_actor_hex(name: &str) -> bool {
    fauna_core::hex32::is_lowercase_hex64(name)
}

/// Why a launch may proceed *unguarded* — the degrade-open cases
/// ([`AccountInstanceLock`] module docs). The platform logs it: the goal doc
/// assigns the log to the client ("the platform logs the unguarded proceed",
/// `account-scoping.md` § Concurrent instances), which is also why this crate
/// takes no logging dependency and reports the degrade upward instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceDegrade {
    /// No resolvable install-scoped state base to key the lock file off.
    NoStateBase,
    /// The lock file could not be opened or locked (I/O failure, or an actor
    /// id that is not a well-formed key).
    LockUnavailable,
}

/// Why this process may not run (or keep running) as an account. Terminal by
/// contract (`account-scoping.md` § Concurrent instances): the caller refuses
/// — a refused instance never falls back onto a different account. Platforms
/// whose OS starts a second process on icon re-click may render the
/// launch-collision chooser for [`AlreadyServed`](Self::AlreadyServed) on a
/// *plain* launch instead of exiting; a bound launch stays terminally refused
/// either way (the chooser is strictly a human affordance).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceRefusal {
    /// Another live process already serves this account (it holds the
    /// account's [`AccountInstanceLock`]).
    AlreadyServed,
    /// The process was launched bound (`FAUNA_BOUND_ACCOUNT`) to one account
    /// but the session resolved another — "launch bound or refuse", never a
    /// plain-launch fallback onto the resolved account.
    BoundMismatch {
        /// The account named by the binding.
        bound: String,
    },
}

impl InstanceRefusal {
    /// A platform-neutral one-line reason, for the client's refusal log.
    pub fn reason(&self) -> String {
        match self {
            InstanceRefusal::AlreadyServed => {
                "another instance already serves this account".to_string()
            }
            InstanceRefusal::BoundMismatch { bound } => {
                format!("launched bound to {bound} but the session resolved a different account")
            }
        }
    }

    /// The standard terminal response to a refusal: log to both `eprintln!`
    /// (`exit` skips the log appender's flush) and `tracing::error!`, then
    /// exit the process. Every native leg's refusal handling reached this
    /// exact shape independently (tui inline, linux at its one call site);
    /// one owner keeps the `[launch-refused]` log line byte-identical across
    /// platforms rather than three call sites drifting on a message tweak.
    pub fn exit(&self, actor_id_hex: &str) -> ! {
        let reason = self.reason();
        eprintln!("[launch-refused] for {actor_id_hex}: {reason} — exiting");
        tracing::error!("[launch-refused] for {actor_id_hex}: {reason} — exiting");
        std::process::exit(1);
    }
}

/// The outcome of [`SessionInstanceHolder::become_session_instance`].
///
/// Three of the four variants mean **proceed**; only [`Refused`](Self::Refused)
/// is terminal. Callers that only care about that split can match on
/// [`refusal`](Self::refusal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionInstanceOutcome {
    /// A fresh acquire — this process is now the account's single instance.
    Acquired,
    /// A same-account session rebuild reused the already-held lock. Reuse is
    /// mandatory, not an optimization: `flock` treats a second open of the
    /// same file as a *competing* owner, so re-acquiring would refuse us.
    Reused,
    /// Proceeding unguarded — pre-guard behavior. The platform logs this.
    Degraded(InstanceDegrade),
    /// Terminal: the caller must not run as this account.
    Refused(InstanceRefusal),
}

impl SessionInstanceOutcome {
    /// The terminal refusal, if this outcome is one.
    pub fn refusal(&self) -> Option<&InstanceRefusal> {
        match self {
            SessionInstanceOutcome::Refused(r) => Some(r),
            _ => None,
        }
    }
}

/// The two directories a serving instance is keyed under — and they answer
/// different questions, which is why there are two
/// (`account-scoping.md` § Concurrent instances → *An erase refuses while a
/// sibling serves the account*).
///
/// * `state_base` — **this app's** install-scoped base. The
///   [`AccountInstanceLock`] there is the launch law: is *this app* already
///   showing the account (raise it, offer the chooser, or — un-retired — refuse).
/// * `store_root` — the per-user account-store root **every app on this OS login
///   shares** (`fauna_account_store::root::StoreRoot`). The
///   `fauna_account_store::locks::ServingLock` there is presence only: it never
///   refuses a launch, and exists so an erase in one app can see an instance of
///   another.
///
/// `From<Option<&Path>>` is the launch base alone — what a caller with no
/// shared store to declare passes, and what every pre-existing holder test
/// means. Production seats name both.
#[derive(Debug, Clone, Copy, Default)]
pub struct ServingBases<'a> {
    /// This app's install-scoped base; `None` = unresolvable, a degrade.
    pub state_base: Option<&'a Path>,
    /// The shared account-store root; `None` = this seat declares no presence
    /// there, so a sibling app's erase cannot see it.
    pub store_root: Option<&'a Path>,
}

impl<'a> From<Option<&'a Path>> for ServingBases<'a> {
    fn from(state_base: Option<&'a Path>) -> Self {
        Self {
            state_base,
            store_root: None,
        }
    }
}

/// Process-lifetime holder of the session's [`AccountInstanceLock`] and, beside
/// it, the account's cross-app `ServingLock`.
///
/// Every native leg needs the same four behaviours around the raw lock —
/// bound-or-refuse, reuse on a same-account rebuild, swap-by-replacement on a
/// cross-account switch, and degrade open — so they live here once rather
/// than in each app's `account_scope.rs` (priority #2; the goal doc's
/// "one implementation, not seven" applies to the holder for exactly the
/// reason it applies to the lock). Clients supply only their own
/// install-scoped state base.
///
/// Most clients want the process-global [`become_process_session_instance`];
/// this type is public for tests and for a client that needs its own holder.
#[derive(Default)]
pub struct SessionInstanceHolder {
    held: Option<Held>,
}

struct Held {
    actor_id: String,
    /// `None` = the acquire degraded — proceeding unguarded. The actor is
    /// still recorded so a same-account rebuild stays a quiet no-op rather
    /// than re-logging and re-trying.
    lock: Option<AccountInstanceLock>,
    /// What a re-acquire needs: this process cannot restore a lock it put
    /// down without the base it was taken under and the law it was taken by
    /// ([`SessionInstanceHolder::without_own_lock`]).
    state_base: Option<PathBuf>,
    mode: ServingMode,
    /// The cross-app presence lock at the shared store root. Independent of
    /// `lock`: a seat whose install base will not resolve still serves out of
    /// the shared store, and is still what a sibling app's erase must see.
    serving: Option<fauna_account_store::locks::ServingLock>,
    /// Where `serving` was taken, for the same restore
    /// [`SessionInstanceHolder::without_own_lock`] owes `lock`.
    store_root: Option<PathBuf>,
}

/// Take the cross-app presence lock, degrading open: a serving lock that will
/// not come is logged and the instance serves unseen, never refused.
fn serve_store_root(
    store_root: Option<&Path>,
    actor_id_hex: &str,
) -> Option<fauna_account_store::locks::ServingLock> {
    use fauna_account_store::locks::{ServingLock, ServingLockOutcome};
    match ServingLock::acquire(store_root?, actor_id_hex) {
        ServingLockOutcome::Held(lock) => Some(lock),
        ServingLockOutcome::Degraded(e) => {
            tracing::warn!(
                "account serving lock unavailable ({e}) — serving unseen by a sibling app's erase"
            );
            None
        }
    }
}

impl SessionInstanceHolder {
    /// An empty holder. `const` so a client can hold one in a `static`.
    pub const fn new() -> Self {
        Self { held: None }
    }

    /// Whether a lock is genuinely held (as opposed to empty or degraded) —
    /// for tests and diagnostics.
    pub fn holds_lock(&self) -> bool {
        self.held.as_ref().is_some_and(|h| h.lock.is_some())
    }

    /// The account this holder serves — recorded on a degraded acquire too,
    /// because a degraded instance still serves out of that account's stores.
    /// `None` until [`Self::become_session_instance`] first admits one.
    pub fn serving_actor(&self) -> Option<&str> {
        self.held.as_ref().map(|h| h.actor_id.as_str())
    }

    /// Become (or remain) `actor_id_hex`'s serving instance under
    /// `state_base`, with `bound` the launch binding
    /// (`fauna_client_accounts::requested_bound_account`) and `mode` the
    /// app's [`ServingMode`] ([`Concurrent`](ServingMode::Concurrent)).
    ///
    /// Call it at the point the session account resolves, **before opening any
    /// of that account's scoped state** (`account-scoping.md` § Concurrent
    /// instances). `state_base` is `None` when the client cannot resolve its
    /// install-scoped base — a degrade, not a refusal. Bound-or-refuse is
    /// mode-independent: a bound launch that resolved a different account is
    /// wrong under either law.
    pub fn become_session_instance<'a>(
        &mut self,
        bases: impl Into<ServingBases<'a>>,
        actor_id_hex: &str,
        bound: Option<&str>,
        mode: ServingMode,
    ) -> SessionInstanceOutcome {
        let ServingBases {
            state_base,
            store_root,
        } = bases.into();
        // Bound-or-refuse first: a bound launch that resolved a different
        // account is wrong even when the lock would be free.
        if let Some(bound) = bound
            && !bound.eq_ignore_ascii_case(actor_id_hex.trim())
        {
            return SessionInstanceOutcome::Refused(InstanceRefusal::BoundMismatch {
                bound: bound.to_string(),
            });
        }
        if let Some(h) = self.held.as_ref()
            && h.actor_id.eq_ignore_ascii_case(actor_id_hex)
        {
            return SessionInstanceOutcome::Reused;
        }
        let Some(base) = state_base else {
            self.record_degraded(actor_id_hex, store_root);
            return SessionInstanceOutcome::Degraded(InstanceDegrade::NoStateBase);
        };
        let acquired = match mode {
            ServingMode::Concurrent => AccountInstanceLock::acquire_shared(base, actor_id_hex),
        };
        match acquired {
            InstanceLockOutcome::Held(lock) => {
                // The assignment drops any previously held (different-account)
                // lock — the acquire-new-then-release-old swap the goal doc
                // prescribes for a cross-account switch.
                // The launch law has admitted this instance, so it is about
                // to serve: declare it at the shared root too. Taken AFTER the
                // launch lock so a refused launch never flickers as a served
                // account to a sibling app's erase.
                self.held = Some(Held {
                    actor_id: actor_id_hex.to_string(),
                    lock: Some(lock),
                    state_base: Some(base.to_path_buf()),
                    mode,
                    serving: serve_store_root(store_root, actor_id_hex),
                    store_root: store_root.map(Path::to_path_buf),
                });
                SessionInstanceOutcome::Acquired
            }
            InstanceLockOutcome::Refused => {
                SessionInstanceOutcome::Refused(InstanceRefusal::AlreadyServed)
            }
            InstanceLockOutcome::Degraded => {
                self.record_degraded(actor_id_hex, store_root);
                SessionInstanceOutcome::Degraded(InstanceDegrade::LockUnavailable)
            }
        }
    }

    /// A degraded *launch* lock still serves — so it still declares itself at
    /// the shared root, which has nothing to do with the install base that
    /// failed.
    fn record_degraded(&mut self, actor_id_hex: &str, store_root: Option<&Path>) {
        self.held = Some(Held {
            actor_id: actor_id_hex.to_string(),
            lock: None,
            state_base: None,
            mode: ServingMode::Concurrent,
            serving: serve_store_root(store_root, actor_id_hex),
            store_root: store_root.map(Path::to_path_buf),
        });
    }

    /// Run `probe` with **this process's own lock on `actor_id_hex` put
    /// down**, then take it again — the one way a live instance can ask
    /// whether anybody *else* is serving that account.
    ///
    /// Under [`ServingMode::Concurrent`] the serving instance holds a shared
    /// lock itself, so an exclusive probe run beside it always answers
    /// "served" and can never tell a sibling from its own reflection. Putting
    /// the lock down for the length of the probe is what makes the question
    /// answerable; it is deliberately the *narrowest* window that can answer
    /// it, and nothing else in the process may take the lock meanwhile,
    /// because this holder is the process's only door to it.
    ///
    /// **The lock is restored whatever `probe` returns**, on the erase path
    /// and the refusal path alike. A restore that fails leaves the holder
    /// degraded — recorded, never silent — which is the same posture as an
    /// acquire that could not reach the file: the instance keeps serving, it
    /// just stops being the one the next probe can see.
    ///
    /// A holder that holds no lock for this actor (a different account, a
    /// degraded acquire, no lock at all) runs `probe` untouched: there is
    /// nothing of ours in the way of the answer.
    pub fn without_own_lock<T>(&mut self, actor_id_hex: &str, probe: impl FnOnce() -> T) -> T {
        let ours = self
            .held
            .as_ref()
            .is_some_and(|h| h.actor_id.eq_ignore_ascii_case(actor_id_hex.trim()));
        if !ours {
            return probe();
        }
        let Some(held) = self.held.as_mut() else {
            return probe();
        };
        let restore = held.lock.take().map(|lock| {
            drop(lock);
            (held.state_base.clone(), held.mode)
        });
        // Both of this process's reflections go down together: the probe asks
        // at the install base AND at the shared root, and either of ours left
        // up would answer "served" for a lone instance.
        let restore_serving = held.serving.take().map(|lock| {
            drop(lock);
            held.store_root.clone()
        });
        let answer = probe();
        if let Some(root) = restore_serving
            && let Some(held) = self.held.as_mut()
        {
            held.serving = serve_store_root(root.as_deref(), actor_id_hex);
        }
        if let Some((base, mode)) = restore {
            let reacquired = match (base.as_deref(), mode) {
                (Some(base), ServingMode::Concurrent) => {
                    AccountInstanceLock::acquire_shared(base, actor_id_hex)
                }
                (None, _) => InstanceLockOutcome::Degraded,
            };
            if let Some(held) = self.held.as_mut() {
                held.lock = match reacquired {
                    InstanceLockOutcome::Held(lock) => Some(lock),
                    // Refused is reachable only if a sibling took an exclusive
                    // lock inside the window — which is precisely the answer
                    // the probe just reported, so there is nothing to log twice.
                    InstanceLockOutcome::Refused | InstanceLockOutcome::Degraded => None,
                };
            }
        }
        answer
    }
}

/// The process-global [`SessionInstanceHolder`] — one per process, because
/// one process serves one account (`account-scoping.md` § Concurrent
/// instances). Poisoning is impossible: nothing below panics while holding it.
static PROCESS_SESSION_INSTANCE: std::sync::Mutex<SessionInstanceHolder> =
    std::sync::Mutex::new(SessionInstanceHolder::new());

/// [`SessionInstanceHolder::without_own_lock`] over the process-global holder
/// — the form the erase guard uses, since the lock it must put down is this
/// process's own session lock.
pub fn without_process_lock<T>(actor_id_hex: &str, probe: impl FnOnce() -> T) -> T {
    PROCESS_SESSION_INSTANCE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .without_own_lock(actor_id_hex, probe)
}

/// The account **this process serves** — the process-global holder's actor
/// (`account-scoping.md` § Concurrent instances → *Session identity resolves
/// through the session's account*: "the bound actor for a secondary instance,
/// the store-active account for a primary", held in process state).
///
/// Read this, never `AccountRegistry::active`, wherever the question is "which
/// account is this window using": on a bound secondary the two differ by
/// design, because a secondary never moves the registry's active pointer.
/// `None` before the launch has admitted an account; [`session_account`] is
/// the form with the primary's fallback.
pub fn process_session_account() -> Option<String> {
    PROCESS_SESSION_INSTANCE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .serving_actor()
        .map(str::to_string)
}

/// The account this window is using: [`process_session_account`], or — before
/// any account has been admitted, which only a primary can be — the registry's
/// active account, which is what a primary resolves to.
///
/// The key both account switchers mark "this is the account you are using"
/// by, so a bound instance never offers to remove the account it serves.
pub fn session_account(registry: &crate::AccountRegistry) -> Option<String> {
    process_session_account().or_else(|| registry.active())
}

/// This process's **launch binding** — the account it runs as when it is not
/// the plain/primary instance (`account-scoping.md` § Concurrent instances).
///
/// Outer `None` = not yet seeded from the environment; inner `None` = an
/// ordinary primary launch.
static SESSION_LAUNCH_BINDING: std::sync::Mutex<Option<Option<String>>> =
    std::sync::Mutex::new(None);

/// The account this process is bound to, or `None` for a plain launch.
///
/// Seeded lazily from `FAUNA_BOUND_ACCOUNT` ([`crate::requested_bound_account`])
/// and then held in process state, because **a binding can also arise — or
/// move — after launch**: the launch-collision chooser makes the colliding
/// process the chosen account's bound instance — "`bind_account` + the bound
/// launch seam, no third process" — so by then there is no environment left
/// to re-read; and a succession re-points the account a binding names, so
/// the binding follows it ([`rebind_session_launch_after_succession`]). One
/// cell, every writer, so every consumer asks the same question once and
/// cannot disagree about what this process is.
///
/// Shared rather than per-app because all three chooser platforms
/// (windows, linux, tui — ui.yaml `launch_instance_chooser`) hit exactly this
/// problem; a client that read the environment directly would silently ignore
/// its own chooser's pick.
pub fn session_launch_binding() -> Option<String> {
    SESSION_LAUNCH_BINDING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(crate::requested_bound_account)
        .clone()
}

/// A bound launch (`FAUNA_BOUND_ACCOUNT`) that routes to `routed_to`
/// (typically onboarding) refuses terminally: onboarding reads and writes
/// the shared onboarding scratchpad, which belongs to the primary instance,
/// and falling back to a plain wizard would silently drop the binding
/// (`account-scoping.md` § Concurrent instances — "launch bound or refuse").
/// A no-op when this process is not bound. Same `[launch-refused]` marker
/// and exit code as the ordinary bound-or-refuse check each app runs at its
/// launch seam over [`become_process_session_instance`]'s
/// [`SessionInstanceOutcome::Refused`] — this covers the case that check
/// cannot: a bound launch whose account resolves to a wizard state (no
/// identity registered, or its material can't be read) never reaches it.
///
/// Reads the process's binding via [`session_launch_binding`], not the
/// environment: the launch-collision chooser binds a process *after*
/// launch, and its pick must land under the same rule as an env-carried
/// binding — a chosen account whose slots route to onboarding has no wizard
/// to run either. The read-only blocking surfaces (identity-changed,
/// offline retry, awaiting-nothing) are deliberately NOT routed through
/// this — they render normally when bound, per the same section.
///
/// Shared rather than per-app (linux, tui — windows carries its own
/// `[launch-refused]` convention at the C# FFI boundary) for the same
/// reason as [`session_launch_binding`] itself: every consumer must agree
/// on the marker text and exit code, and a hand-copy is exactly how they'd
/// drift.
pub fn refuse_if_bound_from_onboarding(routed_to: &str) {
    if let Some(bound) = session_launch_binding() {
        eprintln!(
            "[launch-refused] for {bound}: launch routed to {routed_to} — \
             onboarding belongs to the primary instance — exiting"
        );
        tracing::error!(
            "[launch-refused] for {bound}: launch routed to {routed_to} — \
             onboarding belongs to the primary instance — exiting"
        );
        std::process::exit(1);
    }
}

/// Bind this process to `actor_id_hex` — the launch-collision chooser's pick.
///
/// The chooser is the one writer that **creates** a binding after launch, and
/// it runs only on a **plain** launch: a `FAUNA_BOUND_ACCOUNT` launch that
/// collides is terminally refused and never renders the chooser, so this can
/// never contradict an environment-carried binding. (The cell's other
/// post-launch writer, [`rebind_session_launch_after_succession`], *moves* an
/// existing binding — environment-carried or chooser-made alike — and never
/// creates one.) [`AccountRegistry::bind_account`] remains the gate that
/// decides whether the pick is *allowed*; this records the answer for the
/// rest of the process. Normalized to the same rule the environment read and
/// the lock key use, so the binding, the lock file, and the bound-or-refuse
/// comparison cannot disagree on spelling.
pub fn bind_session_launch_to(actor_id_hex: &str) {
    *SESSION_LAUNCH_BINDING
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(Some(actor_id_hex.trim().to_ascii_lowercase()));
}

/// Re-point this process's launch binding from `old_actor_hex` to
/// `new_actor_hex` — **the binding follows the account across a succession**
/// (`account-scoping.md` § Concurrent instances → *The binding follows the
/// account*). Returns whether it moved.
///
/// A binding names an **account** by the actor id that identified it at
/// launch, and the bound-or-refuse comparison in
/// [`SessionInstanceHolder::become_session_instance`] holds every later
/// session build to that id. A succession re-points the account to a new id
/// and the app switches to it — the account this process is bound to did not
/// change, only the identity it resolves to — so a binding left on the
/// retired id would refuse the very switch the ceremony demands, and the
/// bound instance would exit mid-ceremony: not data loss, since the successor seed is persisted before the
/// sweep runs, but the closing act's shown-once kit render and the in-memory
/// sweep view the retry affordance reads died with the process.
///
/// Only a binding that names `old_actor_hex` moves: a plain launch stays
/// plain, and a binding on an unrelated account is untouched — this is not a
/// second [`bind_session_launch_to`]. Called from
/// [`AccountRegistry::record_succession`], the one seam every adopter of a
/// successor crosses *before* it switches, so no app carries its own copy of
/// the rule and none can forget it.
pub fn rebind_session_launch_after_succession(old_actor_hex: &str, new_actor_hex: &str) -> bool {
    let old = old_actor_hex.trim().to_ascii_lowercase();
    let new = new_actor_hex.trim().to_ascii_lowercase();
    let mut cell = SESSION_LAUNCH_BINDING
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let bound = cell.get_or_insert_with(crate::requested_bound_account);
    if old != new && bound.as_deref() == Some(old.as_str()) {
        *bound = Some(new);
        return true;
    }
    false
}

/// Serializes every test that touches the process-global binding cell — in
/// this module and in `lib.rs`'s registry tests — so they cannot race each
/// other's expectations. Each such test holds this **and** resets the cell to
/// the state it assumes ([`set_session_launch_binding_for_tests`]).
#[cfg(test)]
pub(crate) static SESSION_LAUNCH_BINDING_TEST_LOCK: std::sync::Mutex<()> =
    std::sync::Mutex::new(());

/// Put the binding cell into a known state — outer `None` = unseeded, as at
/// process start. Test-only, and only under [`SESSION_LAUNCH_BINDING_TEST_LOCK`].
#[cfg(test)]
pub(crate) fn set_session_launch_binding_for_tests(binding: Option<Option<String>>) {
    *SESSION_LAUNCH_BINDING
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = binding;
}

/// Become (or remain) this process's session account, reading the launch
/// binding from [`session_launch_binding`] — the environment *or* the
/// chooser's pick, whichever this process has.
///
/// The entry point every native leg calls, each passing its app's
/// [`ServingMode`] (`account-scoping.md` § Concurrent instances); see
/// [`SessionInstanceHolder::become_session_instance`] for the contract.
///
/// `bases` is a [`ServingBases`] and **not** a bare path on purpose: a seat
/// that names only its install base is invisible to a sibling app's erase, and
/// the type is what makes leaving the store root out a written decision rather
/// than a forgotten argument.
pub fn become_process_session_instance(
    bases: ServingBases<'_>,
    actor_id_hex: &str,
    mode: ServingMode,
) -> SessionInstanceOutcome {
    let bound = session_launch_binding();
    PROCESS_SESSION_INSTANCE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .become_session_instance(bases, actor_id_hex, bound.as_deref(), mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(fill: u8) -> String {
        String::from_utf8(vec![fill; 64]).unwrap()
    }

    /// How long a release may take to become observable to the next acquirer.
    /// Generous on purpose: a green run never spends any of it (see
    /// [`reacquire_released`]), and only a genuine leak burns the budget.
    const RELEASE_OBSERVABLE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

    /// Re-acquire `actor` under `base` after something released it, asserting
    /// the release is real — the shape every "the release freed it" test in
    /// this module needs. `claim` names what would be broken if it never frees.
    ///
    /// **Why a poll, when the release is synchronous?** Because the *observer*
    /// is not. `flock` lives on the open file description, and `fork` — inside
    /// `Command::spawn`, before the child's `exec` closes our `O_CLOEXEC`
    /// descriptors — duplicates every descriptor of this process into the
    /// child. A lock dropped while any thread is inside that window stays held
    /// by the child's copy until the child execs, so a re-acquire can see
    /// `Refused` for a lock that genuinely was released
    /// ([`InstanceLockOutcome::Refused`] carries the same caveat). This binary
    /// spawns exactly such a child in `a_killed_holders_lock_is_released`, and
    /// libtest runs it concurrently with everything else: on 2026-08-13 that
    /// reddened the merge gate through
    /// `cross_account_switch_swaps_the_lock` (~3% of full runs; ~17% with just
    /// those two tests selected, and 60/60 green with the spawning test
    /// excluded — which is what identified the window).
    ///
    /// The poll keeps the assertion at full strength rather than softening it:
    /// the fork window clears in microseconds, while a real swap/release
    /// regression holds the lock for the entire budget and still fails (e2e
    /// convention 14's deadline-poll shape, as used by
    /// `a_killed_holders_lock_is_released` for the kernel's own rundown).
    ///
    /// `Degraded` is never polled through: it means the acquire never got to
    /// ask the question (an I/O error), so the run proves nothing in either
    /// direction and has to say so rather than report a regression it did not
    /// observe — the distinction a bare `matches!(…, Held(_))` loses, which
    /// cost a session on 2026-08-13.
    #[track_caller]
    fn reacquire_released(base: &Path, actor: &str, claim: &str) -> AccountInstanceLock {
        let deadline = std::time::Instant::now() + RELEASE_OBSERVABLE_BUDGET;
        loop {
            match AccountInstanceLock::acquire(base, actor) {
                InstanceLockOutcome::Held(lock) => return lock,
                InstanceLockOutcome::Degraded => panic!(
                    "{claim}: acquire() degraded (an I/O error opening or locking the \
                     file), so this run proves nothing either way — inconclusive, NOT \
                     evidence of a regression"
                ),
                InstanceLockOutcome::Refused => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "{claim}: the lock was still held {RELEASE_OBSERVABLE_BUDGET:?} \
                         after the release — a real regression, not the fork window \
                         (which clears in microseconds)"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
    }

    /// The probe-side twin of [`reacquire_released`]: does `actor` read free
    /// within the same budget, for the same reason? Returns the verdict so the
    /// caller can phrase its own claim.
    fn reads_free(base: &Path, actor: &str) -> bool {
        let deadline = std::time::Instant::now() + RELEASE_OBSERVABLE_BUDGET;
        loop {
            if !AccountInstanceLock::is_served(base, actor) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// A second acquire for the same (base, actor) is refused while the
    /// first lock lives — including from another handle in this process
    /// (flock treats each open file description as a distinct owner, which
    /// is also why a same-account session rebuild must reuse its held lock
    /// rather than re-acquire).
    #[test]
    fn a_second_acquire_for_the_same_account_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let held = match AccountInstanceLock::acquire(dir.path(), &actor(b'a')) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("first acquire must hold"),
        };
        assert!(matches!(
            AccountInstanceLock::acquire(dir.path(), &actor(b'a')),
            InstanceLockOutcome::Refused
        ));
        drop(held);
    }

    /// Different accounts under one base coexist — the key is the account,
    /// not the install.
    #[test]
    fn different_accounts_coexist() {
        let dir = tempfile::tempdir().unwrap();
        let _a = match AccountInstanceLock::acquire(dir.path(), &actor(b'a')) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("first account must hold"),
        };
        assert!(matches!(
            AccountInstanceLock::acquire(dir.path(), &actor(b'b')),
            InstanceLockOutcome::Held(_)
        ));
    }

    /// Dropping the lock frees the account for the next acquirer (the
    /// switch-away / clean-exit path), and the lock file survives the
    /// release (never unlinked — module docs).
    #[test]
    fn release_frees_the_account_and_keeps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let held = match AccountInstanceLock::acquire(dir.path(), &actor(b'c')) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("must hold"),
        };
        drop(held);
        drop(reacquire_released(
            dir.path(),
            &actor(b'c'),
            "dropping the lock did not free the account for the next acquirer",
        ));
        assert!(
            dir.path()
                .join(format!("instance-{}.lock", actor(b'c')))
                .exists(),
            "the lock file must survive release"
        );
    }

    /// Case-variant spellings of one actor contend on one lock file.
    #[test]
    fn normalization_makes_case_variants_contend() {
        let dir = tempfile::tempdir().unwrap();
        let _held = match AccountInstanceLock::acquire(dir.path(), &actor(b'd')) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("must hold"),
        };
        let upper = actor(b'd').to_ascii_uppercase();
        assert!(matches!(
            AccountInstanceLock::acquire(dir.path(), &upper),
            InstanceLockOutcome::Refused
        ));
    }

    /// The shared token is what keeps the lock file and the per-account
    /// activation endpoint keyed identically (`account-scoping.md` § the
    /// per-(OS login, account) raise channel). Two properties matter and both
    /// are load-bearing for the raise channel, not just tidiness: case and
    /// whitespace variants of one account collapse to ONE token (else a
    /// raiser addresses an endpoint name its server never claimed, which
    /// looks exactly like "the instance died"), and a value that is not one
    /// of ours yields no token at all (so no stray name is ever claimed).
    #[test]
    fn the_shared_token_normalizes_and_rejects_non_actors() {
        let canonical = actor(b'a');
        assert_eq!(
            account_instance_token(&canonical).as_deref(),
            Some(canonical.as_str())
        );
        assert_eq!(
            account_instance_token(&format!("  {}  ", canonical.to_ascii_uppercase())).as_deref(),
            Some(canonical.as_str()),
            "trim + lowercase must collapse spellings onto one token"
        );
        for not_ours in [
            "",
            "not-hex",
            &"a".repeat(63),
            &"a".repeat(65),
            &"g".repeat(64),
        ] {
            assert!(
                account_instance_token(not_ours).is_none(),
                "{not_ours:?} is not an actor id and must mint no token"
            );
        }
    }

    /// The token IS the lock file's key — pinned as one fact rather than two
    /// so a future change to either derivation cannot silently split them.
    #[test]
    fn the_lock_file_is_named_after_the_shared_token() {
        let dir = tempfile::tempdir().unwrap();
        let spelled = format!("  {}  ", actor(b'e').to_ascii_uppercase());
        let _held = match AccountInstanceLock::acquire(dir.path(), &spelled) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("must hold"),
        };
        let token = account_instance_token(&spelled).expect("a well-formed actor");
        assert!(
            dir.path().join(format!("instance-{token}.lock")).exists(),
            "the lock file must be named from the shared token"
        );
    }

    /// The chooser's probe sees a held account as served and a released one
    /// as free — the display-only read of the same lock `acquire` arbitrates.
    #[test]
    fn is_served_tracks_the_live_holder() {
        let dir = tempfile::tempdir().unwrap();
        let held = match AccountInstanceLock::acquire(dir.path(), &actor(b'a')) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("must hold"),
        };
        assert!(
            AccountInstanceLock::is_served(dir.path(), &actor(b'a')),
            "a live holder must read as served"
        );
        drop(held);
        assert!(
            reads_free(dir.path(), &actor(b'a')),
            "a released account must read as free"
        );
    }

    /// The focus-existing decision, arm by arm: a landed raise wins; an
    /// unanswered one re-probes the lock — gone → plain launch, still held →
    /// say so. Driven over a REAL lock for the two probe arms, so the re-probe
    /// is the same read `acquire` arbitrates, not a stub of it.
    #[test]
    fn focus_existing_raises_else_re_probes_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let served = actor(b'a');
        let probe = |id: &str| AccountInstanceLock::is_served(dir.path(), id);

        let held = match AccountInstanceLock::acquire(dir.path(), &served) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("must hold"),
        };
        assert_eq!(
            resolve_focus_existing(&served, |_| true, probe),
            FocusExistingOutcome::Raised
        );
        assert_eq!(
            resolve_focus_existing(&served, |_| false, probe),
            FocusExistingOutcome::StillServedNoChannel,
            "a live holder with no channel to raise it is still served"
        );
        drop(held);
        assert_eq!(
            resolve_focus_existing(&served, |_| false, probe),
            FocusExistingOutcome::NoLongerServed,
            "the sibling went away: no collision is left, so a plain launch"
        );
        assert_eq!(
            resolve_focus_existing("  ", |_| true, |_| false),
            FocusExistingOutcome::StillServedNoChannel,
            "no account to address is no evidence the account is free"
        );
    }

    /// The child half of [`a_killed_holders_lock_is_released`]: acquire the
    /// lock, announce it on disk, then park. `#[ignore]`d so the ordinary
    /// suite never runs it — the parent re-invokes this very test binary with
    /// `--exact … --ignored` and the three env vars below, which is how we get
    /// a *real* second process holding a *real* kernel lock without shipping a
    /// helper binary just for one test.
    #[test]
    #[ignore = "spawned as a child by a_killed_holders_lock_is_released; not a standalone test"]
    fn instance_lock_holder_child() {
        let (Ok(base), Ok(actor), Ok(ready)) = (
            std::env::var("FAUNA_TEST_LOCK_BASE"),
            std::env::var("FAUNA_TEST_LOCK_ACTOR"),
            std::env::var("FAUNA_TEST_LOCK_READY"),
        ) else {
            // Run directly (e.g. `--ignored` over the whole suite) rather than
            // spawned: nothing to hold, and taking a lock over an unknown base
            // would be worse than doing nothing.
            return;
        };
        let _held = match AccountInstanceLock::acquire(Path::new(&base), &actor) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("the child must be able to acquire a fresh lock"),
        };
        std::fs::write(&ready, b"held").expect("announce the hold");
        // Park while the parent kills us — that kill IS the test. Bounded so a
        // parent that died before killing leaves no immortal child behind.
        std::thread::sleep(std::time::Duration::from_secs(60));
    }

    /// **The crash-safety claim, tested against a real killed process.**
    ///
    /// `account-scoping.md` § Concurrent instances states the lock is
    /// "crash-safe by construction (kernel file lock, released when the holder
    /// dies — no stale-lock reconciliation at boot)". Every other test in this
    /// module releases by *dropping* the value, which proves only that RAII
    /// works; none of them proves the OS half, which is the half every launch
    /// after a crash, a force-quit, or a `TerminateProcess` depends on.
    ///
    /// That gap is not hypothetical. The windows e2e harness relaunches the app
    /// against the SAME data dir right after killing it, and a lock that
    /// outlived its killed holder would strand the new process in the
    /// launch-collision chooser — which `App.xaml.cs` promises can never happen
    /// ("fails toward the ordinary launch, never toward a chooser"). A user who
    /// force-quits from Task Manager and relaunches is the same path with a
    /// human at the keyboard, so this belongs in shared Rust, not in one
    /// platform's harness.
    ///
    /// Deliberately a *kill*, never a drop: `Child::kill` is `TerminateProcess`
    /// on windows and `SIGKILL` on unix, so the holder gets no chance to run
    /// any cleanup and only the kernel can free the lock.
    #[test]
    fn a_killed_holders_lock_is_released() {
        use std::process::Command;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let actor = actor(b'a');
        let ready = dir.path().join("held.ready");

        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "instance_lock::tests::instance_lock_holder_child",
                "--ignored",
                "--test-threads=1",
            ])
            .env("FAUNA_TEST_LOCK_BASE", dir.path())
            .env("FAUNA_TEST_LOCK_ACTOR", &actor)
            .env("FAUNA_TEST_LOCK_READY", &ready)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the holder child");

        // Deadline-poll for the child's hold rather than sleeping a guess:
        // a green run pays only the real process-start time.
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline && !ready.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            ready.exists(),
            "the child never announced its hold — it could not start or could not acquire"
        );
        assert!(
            AccountInstanceLock::is_served(dir.path(), &actor),
            "a live foreign holder must read as served (else this test proves nothing)"
        );

        child.kill().expect("kill the holder");
        let killed_at = Instant::now();
        child.wait().expect("reap the holder");

        // The claim under test: once the holder is gone the account is free.
        // Generous ceiling, deadline poll (e2e convention 14's shape): a
        // kernel that releases synchronously with process rundown pays ~0,
        // and only a genuine leak spends the budget.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && AccountInstanceLock::is_served(dir.path(), &actor) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let freed_after = killed_at.elapsed();
        assert!(
            !AccountInstanceLock::is_served(dir.path(), &actor),
            "the lock outlived its killed holder by more than {freed_after:?} — \
             account-scoping.md's crash-safety claim is false on this platform, and \
             a relaunch after a force-quit would be stranded in the chooser"
        );
        // And the account is genuinely re-acquirable, not merely un-probeable:
        // `is_served` degrades to false on I/O failure, so the probe alone
        // could report "free" for a reason that is not freedom.
        drop(reacquire_released(
            dir.path(),
            &actor,
            "the account must be re-acquirable after its holder was killed",
        ));
        println!("[instance-lock] released {freed_after:?} after the kill");
    }

    /// Probing must not leave the account looking served to the next
    /// acquirer — the probe releases what it took, so the pick that follows
    /// the chooser's render still succeeds.
    #[test]
    fn probing_does_not_claim_the_account() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!AccountInstanceLock::is_served(dir.path(), &actor(b'b')));
        assert!(!AccountInstanceLock::is_served(dir.path(), &actor(b'b')));
        assert!(
            matches!(
                AccountInstanceLock::acquire(dir.path(), &actor(b'b')),
                InstanceLockOutcome::Held(_)
            ),
            "acquire after probing must still hold"
        );
    }

    /// A never-launched account has no lock file — and the probe must not
    /// mint one just by asking (else rendering the chooser would litter the
    /// state base with a file per registered account).
    #[test]
    fn probing_an_unknown_account_creates_no_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!AccountInstanceLock::is_served(dir.path(), &actor(b'c')));
        assert!(
            !dir.path()
                .join(format!("instance-{}.lock", actor(b'c')))
                .exists(),
            "the probe must never create a lock file"
        );
    }

    /// The chooser's filter: served accounts drop out, order is preserved,
    /// and a case-variant spelling of the served account still drops out
    /// (the probe normalizes exactly as `acquire` does).
    #[test]
    fn not_currently_served_filters_the_live_ones() {
        let dir = tempfile::tempdir().unwrap();
        let _held = match AccountInstanceLock::acquire(dir.path(), &actor(b'd')) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("must hold"),
        };
        let all = [actor(b'e'), actor(b'd').to_ascii_uppercase(), actor(b'f')];
        assert_eq!(
            AccountInstanceLock::not_currently_served(dir.path(), &all),
            vec![actor(b'e'), actor(b'f')]
        );
    }

    /// The chooser offers the accounts no live instance serves, in registry
    /// order, labelled with the shared display formatter — and never offers
    /// the account that collided (a live holder is why the chooser rendered).
    #[test]
    fn choosable_accounts_exclude_the_served_ones() {
        let dir = tempfile::tempdir().unwrap();
        let _served = match AccountInstanceLock::acquire(dir.path(), &actor(b'a')) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("the collided account must be held"),
        };
        let entries = vec![
            (actor(b'a'), Some("ana".to_string())),
            (actor(b'b'), Some("bo".to_string())),
            (actor(b'c'), None),
        ];
        let offered = choosable_accounts(dir.path(), &entries);
        assert_eq!(
            offered
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec![actor(b'b').as_str(), actor(b'c').as_str()],
            "the served account must not be offered"
        );
        assert_eq!(offered[0].1, "bo", "handle labels the row when cached");
        assert_eq!(
            offered[1].1,
            fauna_core::format::account_display_label(None, &actor(b'c')),
            "a handle-less account falls back to the shared short-id label"
        );
    }

    /// Every account served somewhere → nothing to offer. The chooser renders
    /// its "all open" explanation rather than an empty list.
    #[test]
    fn choosable_accounts_can_be_empty() {
        let dir = tempfile::tempdir().unwrap();
        let _a = AccountInstanceLock::acquire(dir.path(), &actor(b'a'));
        let _b = AccountInstanceLock::acquire(dir.path(), &actor(b'b'));
        let entries = vec![(actor(b'a'), None), (actor(b'b'), None)];
        assert!(choosable_accounts(dir.path(), &entries).is_empty());
    }

    /// No live holders at all → every registered account is offered.
    #[test]
    fn choosable_accounts_offers_everyone_with_no_lock_holders() {
        let dir = tempfile::tempdir().unwrap();
        let entries = vec![(actor(b'a'), None), (actor(b'b'), None)];
        assert_eq!(
            choosable_accounts(dir.path(), &entries)
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec![actor(b'a').as_str(), actor(b'b').as_str()],
        );
    }

    /// The launch binding: no `FAUNA_BOUND_ACCOUNT` means a plain launch;
    /// the chooser's pick then binds this process **after** launch — the
    /// whole reason the cell exists rather than a bare environment read —
    /// and is normalized like every other actor key, so the bound-or-refuse
    /// comparison cannot reject the very account the user just chose over a
    /// spelling difference.
    ///
    /// The cell is process-global, so every test that reads or writes it
    /// holds [`SESSION_LAUNCH_BINDING_TEST_LOCK`] and starts from the state it
    /// assumes — here the unseeded one, as at process start.
    #[test]
    fn the_launch_binding_seeds_from_the_environment_then_takes_the_chooser_pick() {
        let _serial = SESSION_LAUNCH_BINDING_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        set_session_launch_binding_for_tests(None);
        assert_eq!(
            session_launch_binding(),
            None,
            "no FAUNA_BOUND_ACCOUNT in the test process — a plain launch"
        );
        // A succession of some account cannot bind a PLAIN process: the rule
        // moves a binding, it never creates one.
        assert!(!rebind_session_launch_after_succession(
            &actor(b'e'),
            &actor(b'd')
        ));
        assert_eq!(session_launch_binding(), None);
        bind_session_launch_to(&format!("  {}  ", actor(b'e').to_ascii_uppercase()));
        assert_eq!(session_launch_binding(), Some(actor(b'e')));
        // And the guard accepts the pick for that same account.
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            become_process_session_instance(
                Some(dir.path()).into(),
                &actor(b'e'),
                ServingMode::Concurrent
            ),
            SessionInstanceOutcome::Acquired
        ));
        // …while a session that resolved a different account is refused,
        // rather than quietly running as the wrong one.
        assert!(matches!(
            become_process_session_instance(
                Some(dir.path()).into(),
                &actor(b'f'),
                ServingMode::Concurrent
            ),
            SessionInstanceOutcome::Refused(InstanceRefusal::BoundMismatch { .. })
        ));
        // A succession of an UNRELATED account leaves this binding alone…
        assert!(!rebind_session_launch_after_succession(
            &actor(b'f'),
            &actor(b'd')
        ));
        assert_eq!(session_launch_binding(), Some(actor(b'e')));
        // …and the binding FOLLOWS ITS OWN ACCOUNT across one: e → d re-points
        // the account this process is bound to, so the successor's session
        // passes the gate the predecessor's did, and the retired id is the
        // mismatch now. The holder swaps by replacement underneath, exactly as
        // on any cross-account switch.
        assert!(rebind_session_launch_after_succession(
            &format!("  {}  ", actor(b'e').to_ascii_uppercase()),
            &actor(b'd').to_ascii_uppercase()
        ));
        assert_eq!(session_launch_binding(), Some(actor(b'd')));
        assert_eq!(
            become_process_session_instance(
                Some(dir.path()).into(),
                &actor(b'd'),
                ServingMode::Concurrent
            ),
            SessionInstanceOutcome::Acquired,
            "the successor's session must pass bound-or-refuse — a binding left \
             on the retired id exits the bound instance mid-ceremony"
        );
        assert!(matches!(
            become_process_session_instance(
                Some(dir.path()).into(),
                &actor(b'e'),
                ServingMode::Concurrent
            ),
            SessionInstanceOutcome::Refused(InstanceRefusal::BoundMismatch { .. })
        ));
        // Idempotent over a re-run of the same ceremony's persist.
        assert!(!rebind_session_launch_after_succession(
            &actor(b'e'),
            &actor(b'd')
        ));
        assert_eq!(session_launch_binding(), Some(actor(b'd')));
    }

    // The shared `account_instance_token` derivation, and the token↔lock-file
    // coupling it exists to guarantee, are pinned by
    // `the_shared_token_normalizes_and_rejects_non_actors` and
    // `the_lock_file_is_named_after_the_shared_token` (added with the fn on the
    // linux leg). windows' endpoint names off the same fn — no separate proof
    // needed here.

    /// Degrade-open, probe edition: an unreadable base and a malformed key
    /// both read as "not served" — a hiccup narrows the chooser's list, it
    /// never leaves the user with no way in.
    #[test]
    fn probe_failures_read_as_not_served() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain-file");
        std::fs::write(&plain, b"x").unwrap();
        assert!(!AccountInstanceLock::is_served(
            &plain.join("sub"),
            &actor(b'g')
        ));
        assert!(!AccountInstanceLock::is_served(
            dir.path(),
            "not-an-actor-id"
        ));
    }

    /// An impossible base (parent is a regular file) degrades instead of
    /// erroring the launch; so does a key that is not an actor id.
    #[test]
    fn io_failure_and_malformed_keys_degrade_open() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain-file");
        std::fs::write(&plain, b"x").unwrap();
        assert!(matches!(
            AccountInstanceLock::acquire(&plain.join("sub"), &actor(b'e')),
            InstanceLockOutcome::Degraded
        ));
        assert!(matches!(
            AccountInstanceLock::acquire(dir.path(), "not-an-actor-id"),
            InstanceLockOutcome::Degraded
        ));
    }

    // --- SessionInstanceHolder ------------------------------------------
    //
    // Lifted from `apps/fauna-linux/src/account_scope.rs` (2026-07-22) when
    // the holder moved here so tui/windows/android consume it rather than
    // re-implement it; linux's leg now calls this module and these tests
    // moved with the logic.

    const ACTOR_A: &str = "aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44";
    const ACTOR_B: &str = "bb11cc22dd33ee44ff55aa66bb11cc22dd33ee44ff55aa66bb11cc22dd33ee44";

    /// A same-account session rebuild must REUSE the held lock: flock treats
    /// a second open of the same file as a competing owner, so a re-acquire
    /// would refuse *ourselves*. The foreign probe proves we genuinely hold
    /// the lock throughout, and the second call proves reuse.
    #[test]
    fn same_account_rebuild_reuses_the_held_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let mut holder = SessionInstanceHolder::new();
        assert_eq!(
            holder.become_session_instance(
                Some(tmp.path()),
                ACTOR_A,
                None,
                ServingMode::Concurrent
            ),
            SessionInstanceOutcome::Acquired
        );
        // A "second process" (a distinct open file description) is refused —
        // we really hold the lock.
        assert!(matches!(
            AccountInstanceLock::acquire(tmp.path(), ACTOR_A),
            InstanceLockOutcome::Refused
        ));
        assert_eq!(
            holder.become_session_instance(
                Some(tmp.path()),
                ACTOR_A,
                None,
                ServingMode::Concurrent
            ),
            SessionInstanceOutcome::Reused,
            "a same-account rebuild must reuse, never re-acquire (which would self-refuse)"
        );
    }

    /// A cross-account switch swaps by replacement: the new account's lock is
    /// acquired and the old account's released, observable to a foreign
    /// acquirer on both sides.
    #[test]
    fn cross_account_switch_swaps_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let mut holder = SessionInstanceHolder::new();
        assert_eq!(
            holder.become_session_instance(
                Some(tmp.path()),
                ACTOR_A,
                None,
                ServingMode::Concurrent
            ),
            SessionInstanceOutcome::Acquired
        );
        assert_eq!(
            holder.become_session_instance(
                Some(tmp.path()),
                ACTOR_B,
                None,
                ServingMode::Concurrent
            ),
            SessionInstanceOutcome::Acquired
        );
        // Old account freed for the next acquirer — through
        // `reacquire_released`, which keeps the three ways this can end apart:
        // a real swap regression, an I/O hiccup that proves nothing, and the
        // sibling `Command::spawn`'s fork window, which is what actually
        // reddened the merge gate here. All three were paid for on 2026-08-13.
        drop(reacquire_released(
            tmp.path(),
            ACTOR_A,
            "the switch to ACTOR_B did not release ACTOR_A's lock",
        ));
        // … new account genuinely held.
        assert!(matches!(
            AccountInstanceLock::acquire(tmp.path(), ACTOR_B),
            InstanceLockOutcome::Refused
        ));
    }

    /// Another live holder refuses the launch (`AlreadyServed`), and the
    /// refusal leaves the holder empty — the refused process exits (or
    /// renders the chooser) without having taken anything.
    #[test]
    fn a_served_account_refuses_the_second_instance() {
        let tmp = tempfile::tempdir().unwrap();
        let foreign = match AccountInstanceLock::acquire(tmp.path(), ACTOR_A) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("foreign holder must acquire"),
        };
        let mut holder = SessionInstanceHolder::new();
        assert_eq!(
            holder.become_session_instance(
                Some(tmp.path()),
                ACTOR_A,
                None,
                ServingMode::Concurrent
            ),
            SessionInstanceOutcome::Refused(InstanceRefusal::AlreadyServed)
        );
        assert!(
            !holder.holds_lock(),
            "a refused become must not record a holder"
        );
        drop(foreign);
    }

    /// The bound-or-refuse rule: a binding for a different account refuses
    /// even when the lock is free; a binding matching the resolved account
    /// proceeds (≈ a plain launch of the active account).
    #[test]
    fn bound_mismatch_refuses_and_bound_match_proceeds() {
        let tmp = tempfile::tempdir().unwrap();
        let mut holder = SessionInstanceHolder::new();
        assert_eq!(
            holder.become_session_instance(
                Some(tmp.path()),
                ACTOR_A,
                Some(ACTOR_B),
                ServingMode::Concurrent
            ),
            SessionInstanceOutcome::Refused(InstanceRefusal::BoundMismatch {
                bound: ACTOR_B.to_string()
            })
        );
        assert!(!holder.holds_lock());
        assert_eq!(
            holder.become_session_instance(
                Some(tmp.path()),
                ACTOR_A,
                Some(ACTOR_A),
                ServingMode::Concurrent
            ),
            SessionInstanceOutcome::Acquired
        );
    }

    /// An unresolvable base and an I/O failure both degrade OPEN — the guard
    /// narrows a race, it must not widen a failure into a refused launch —
    /// and the degraded actor is recorded so a rebuild stays a quiet no-op.
    #[test]
    fn degraded_acquire_proceeds_unguarded() {
        let mut holder = SessionInstanceHolder::new();
        assert_eq!(
            holder.become_session_instance(None, ACTOR_A, None, ServingMode::Concurrent),
            SessionInstanceOutcome::Degraded(InstanceDegrade::NoStateBase)
        );
        assert!(!holder.holds_lock(), "a degrade holds no lock");
        // The degraded actor is recorded: the rebuild is a quiet reuse, not a
        // second degrade (which would re-log on every session rebuild).
        assert_eq!(
            holder.become_session_instance(None, ACTOR_A, None, ServingMode::Concurrent),
            SessionInstanceOutcome::Reused
        );

        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("plain-file");
        std::fs::write(&plain, b"x").unwrap();
        let mut holder_io = SessionInstanceHolder::new();
        assert_eq!(
            holder_io.become_session_instance(
                Some(plain.join("sub").as_path()),
                ACTOR_A,
                None,
                ServingMode::Concurrent
            ),
            SessionInstanceOutcome::Degraded(InstanceDegrade::LockUnavailable)
        );
        assert!(!holder_io.holds_lock());
    }

    /// A refusal is terminal, never a silent proceed — the property every
    /// platform leg's refusal handling keys off.
    #[test]
    fn refusal_reasons_are_reportable() {
        assert!(
            InstanceRefusal::AlreadyServed
                .reason()
                .contains("already serves")
        );
        assert!(
            InstanceRefusal::BoundMismatch {
                bound: ACTOR_B.to_string(),
            }
            .reason()
            .contains(ACTOR_B)
        );
    }

    /// The W5.6 successor law: two `Concurrent` servers of one account
    /// coexist — the same-account refusal is retired for a retired app.
    #[test]
    fn concurrent_mode_two_servers_of_one_account_coexist() {
        let dir = tempfile::tempdir().unwrap();
        let first = match AccountInstanceLock::acquire_shared(dir.path(), &actor(b'a')) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("first shared acquire must hold"),
        };
        let second = match AccountInstanceLock::acquire_shared(dir.path(), &actor(b'a')) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("a second same-account shared acquire must coexist, not refuse"),
        };
        drop((first, second));
    }

    /// A shared server still reads as *served* — the raise channel's and the
    /// chooser's question stays answerable because `is_served`'s probe is
    /// exclusive and a shared holder blocks it.
    #[test]
    fn a_shared_server_still_reads_as_served() {
        let dir = tempfile::tempdir().unwrap();
        let _server = match AccountInstanceLock::acquire_shared(dir.path(), &actor(b'a')) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("shared acquire must hold"),
        };
        assert!(
            AccountInstanceLock::is_served(dir.path(), &actor(b'a')),
            "a concurrent server must still be visible to the display-only probe"
        );
    }

    /// The version-skew interlock, both directions: an exclusive holder (a
    /// pre-retirement binary) refuses a shared acquire after its bounded
    /// retry, and a shared holder refuses an exclusive acquire — a mixed
    /// pair degenerates to the stricter law, never to an unguarded pair.
    #[test]
    fn exclusive_and_shared_holders_refuse_each_other() {
        let dir = tempfile::tempdir().unwrap();

        let exclusive = match AccountInstanceLock::acquire(dir.path(), &actor(b'a')) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("exclusive acquire must hold"),
        };
        assert!(
            matches!(
                AccountInstanceLock::acquire_shared(dir.path(), &actor(b'a')),
                InstanceLockOutcome::Refused
            ),
            "a live exclusive holder must refuse a shared acquire (bounded retry, then honest)"
        );
        drop(exclusive);

        let _shared = match AccountInstanceLock::acquire_shared(dir.path(), &actor(b'a')) {
            InstanceLockOutcome::Held(l) => l,
            _ => panic!("shared acquire must hold after the exclusive released"),
        };
        assert!(
            matches!(
                AccountInstanceLock::acquire(dir.path(), &actor(b'a')),
                InstanceLockOutcome::Refused
            ),
            "a live shared holder must refuse an exclusive acquire"
        );
    }

    /// Bound-or-refuse is mode-independent: `Concurrent` retires the
    /// same-account refusal, never the binding contract.
    #[test]
    fn concurrent_mode_still_refuses_a_bound_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let mut holder = SessionInstanceHolder::new();
        assert_eq!(
            holder.become_session_instance(
                Some(tmp.path()),
                ACTOR_A,
                Some(ACTOR_B),
                ServingMode::Concurrent
            ),
            SessionInstanceOutcome::Refused(InstanceRefusal::BoundMismatch {
                bound: ACTOR_B.to_string(),
            })
        );
        assert!(!holder.holds_lock());
    }

    /// The holder end-to-end under the successor: two holders (two would-be
    /// processes — each acquire opens its own file description) both proceed
    /// `Acquired` on one account in `Concurrent` mode.
    #[test]
    fn two_concurrent_holders_both_acquire_one_account() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = SessionInstanceHolder::new();
        let mut b = SessionInstanceHolder::new();
        assert_eq!(
            a.become_session_instance(Some(tmp.path()), ACTOR_A, None, ServingMode::Concurrent),
            SessionInstanceOutcome::Acquired
        );
        assert_eq!(
            b.become_session_instance(Some(tmp.path()), ACTOR_A, None, ServingMode::Concurrent),
            SessionInstanceOutcome::Acquired,
            "the second same-account server must coexist — the refusal is retired on tui"
        );
        assert!(a.holds_lock() && b.holds_lock());
    }

    // ── without_own_lock: the erase guard's window ───────────────────────

    /// ⚠ **A serving instance cannot see past its own lock**, which is what
    /// this window exists to fix.
    ///
    /// Under the concurrent law the erasing instance holds a shared lock on
    /// its own account, so an exclusive probe run beside it answers "served"
    /// whether or not anybody else is there — it would refuse every sign-out
    /// on a single-window device. Putting our own lock down for the length of
    /// the probe is the only way to ask the question.
    #[test]
    fn our_own_lock_goes_down_for_the_length_of_a_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ours = SessionInstanceHolder::new();
        ours.become_session_instance(Some(tmp.path()), ACTOR_A, None, ServingMode::Concurrent);
        assert!(ours.holds_lock());

        let free = ours.without_own_lock(ACTOR_A, || {
            matches!(
                AccountInstanceLock::acquire(tmp.path(), ACTOR_A),
                InstanceLockOutcome::Held(_)
            )
        });
        assert!(
            free,
            "with only this instance serving, the account must probe FREE — otherwise \
             a one-window device could never sign out"
        );
        assert!(
            ours.holds_lock(),
            "and the lock is taken again afterwards: the instance goes on serving"
        );
    }

    /// The other half: a genuine sibling outlasts our release, so the probe
    /// reports the account served and the erase is refused.
    #[test]
    fn a_siblings_lock_outlasts_our_release() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ours = SessionInstanceHolder::new();
        let mut sibling = SessionInstanceHolder::new();
        ours.become_session_instance(Some(tmp.path()), ACTOR_A, None, ServingMode::Concurrent);
        sibling.become_session_instance(Some(tmp.path()), ACTOR_A, None, ServingMode::Concurrent);

        let free = ours.without_own_lock(ACTOR_A, || {
            matches!(
                AccountInstanceLock::acquire(tmp.path(), ACTOR_A),
                InstanceLockOutcome::Held(_)
            )
        });
        assert!(
            !free,
            "a second instance is serving this account — the erase must be refused, \
             not run under its engine"
        );
        assert!(ours.holds_lock(), "our own lock is restored either way");
        assert!(sibling.holds_lock(), "and the sibling never lost its own");
    }

    /// Another account's lock is none of this window's business: the holder
    /// leaves it alone and the probe answers about the account it was asked
    /// about.
    #[test]
    fn the_window_only_puts_down_the_lock_it_was_asked_about() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ours = SessionInstanceHolder::new();
        ours.become_session_instance(Some(tmp.path()), ACTOR_A, None, ServingMode::Concurrent);

        let free_b = ours.without_own_lock(ACTOR_B, || {
            matches!(
                AccountInstanceLock::acquire(tmp.path(), ACTOR_B),
                InstanceLockOutcome::Held(_)
            )
        });
        assert!(free_b, "nobody serves B");
        assert!(
            ours.holds_lock(),
            "and A's lock was never in question, so it was never put down"
        );
    }
}
