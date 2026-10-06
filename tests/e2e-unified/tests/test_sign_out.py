"""Sign-out returns the app to onboarding AND erases the credential namespace.

Drives ``sign-out-button`` → inline ``sign-out-confirm-button`` on the Settings
Account sub-page. Two independent properties, one per test, because they fail
differently and a single test asserting only the first read for months as though
it covered both:

1. **The app re-roots onboarding at identity_choice** (the create/import buttons)
   — what the user sees.
2. **The client's whole credential namespace is gone** — ``long-term-store.md``
   § Cleanup contract, which is *not* implied by (1): an app that navigates to
   onboarding while leaving ``fauna/{actor}/secret`` on disk has signed the user
   out of the UI and nowhere else, and its next launch boots that account
   from the surviving index and silently signs them back in. This module's docstring
   claimed (2) from the day it was written and asserted only (1); the claim is
   now the second test rather than a sentence.

Windows **led** the uniform ``sign-out-confirm-button`` shape (mirroring the
established ``admin-factory-reset-button`` → ``admin-factory-reset-confirm-button``
pattern, drivable on every app). linux + web have now joined: each exposes a
drivable ``sign-out-confirm-button`` using its own factory-reset idiom — linux
stamps the ``adw::MessageDialog`` confirm response button via
``testid::tag_response_button`` (uniform with ``admin-factory-reset-confirm-button``
on linux), web reveals an inline confirm box (uniform with the web admin
factory-reset confirm). android also carries the inline-confirm
(an AlertDialog whose confirm button is tagged ``sign-out-confirm-button``,
uniform with the android factory-reset/delete-account dialogs) — compile-verified
on Linux; the ``--client android`` run is the standing gate once the ``host``
emulator lands (android e2e is host-blocked fleet-wide). macOS + iOS landed the
identical inline confirm (shared FaunaKit ``SignOutSection``,
``sign-out-confirm-button``); their e2e markers are entrusted to the serialized
apple-bridge runner — iOS adds ``pytest.mark.ios`` after a green ``--client ios``
run; macOS is deferred to the Settings sidebar-swap
shell, since the Account pane is off-screen-
unhittable in the single-scroll ``PreferencesView`` until the shell lands.
"""
import contextlib
import os
import stat
import time

import pytest

from common.cred_store import attach_account_store, attach_cred_store
from common.scope_store import attach_scope_store
from helpers import app_surface
from helpers.waiting import await_account_runtime_assembled
from i18n.strings import S


@contextlib.contextmanager
def _held_open_no_share(path):
    """Hold `path` open the way a live SQLite connection actually does —
    reproducing the ACTUAL holder, not a simulated one.

    ⚠ **A raw `CreateFileW(..., dwShareMode=0, ...)` handle does NOT
    reproduce this fault, measured.** Modern Windows deletes through an
    ordinary read/write handle via POSIX delete semantics (rename-based
    unlink, introduced for WSL/Docker compatibility) regardless of its share
    mode — that is exactly what
    `libs/fauna-ffi/src/account_state.rs`'s
    `an_undeletable_app_scope_does_not_spare_the_store_root` doc-comment
    means by "a plain open makes this test pass for the wrong reason": only
    a connection that also takes an active BYTE-RANGE LOCK (SQLite's Windows
    VFS locking protocol) blocks it. So this opens a real
    `sqlite3` connection against the SQLite file itself and holds an
    EXCLUSIVE transaction — the same lock a live MLS store connection holds
    — rather than trying to simulate one via a raw Win32 handle.
    """
    import sqlite3

    conn = sqlite3.connect(str(path))
    conn.execute("BEGIN EXCLUSIVE")
    try:
        yield
    finally:
        conn.rollback()
        conn.close()

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.windows,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
    pytest.mark.ios,
    pytest.mark.macos,
    pytest.mark.tui,
]


# How long the residue line may take to reach `sign-out-residue-message` after
# `sign_out()` returns. Its own wait targets `create-identity-button`, which some
# seats attach before the residue view lands (windows measured sub-100 ms), so
# the line is deadline-polled against a budget far above any non-pathological
# delay (e2e-conventions.md convention 14) — never read once, never slept on.
# A one-shot read went red on linux in a whole-suite sweep (2026-09-22) with the
# main loop 88% busy across the sign-out: the notice painted after the read.
_RESIDUE_PAINT_BUDGET_S = 15.0


def _await_residue_line(app, expected: str, read=None) -> str:
    """The residue surface's text once it carries `expected`, or the last text
    read when the budget runs out — for the caller's assertion to report.
    `read` is the surface reader (`_residue_surface`);
    `sign-out-residue-message` when omitted."""
    read = read or app.onboarding.sign_out_residue_text
    deadline = time.monotonic() + _RESIDUE_PAINT_BUDGET_S
    actual = read()
    while expected not in (actual or "") and time.monotonic() < deadline:
        actual = read()
    return actual


# Every seat paints the approved `sign-out-residue` package (IDs user-approved
# 2026-09-25; `account-scoping.md` § Erasure follows scope → the residue
# surface): the line rides `sign-out-residue-message`, names the Remove Again
# control beside it, and outlives the process. macOS / iOS was the last seat; the
# `error-message` / `_no_retry` arm retired with it.
#
# Two seats still cannot run every journey here, for where the fault can be
# injected rather than for what they paint: web has no on-disk scope directory
# (its package is driven by `test_sign_out_web.py`, through a held IndexedDB
# connection — `_require_residue_surface` declares the absence), and android's
# scope directories are on-device, out of the host's reach
# (`_skip_android_host_fault_injection`; its paint gate, record, retry and launch
# re-check are pinned by the Robolectric tests `AccountStoresEraseTest` and
# `SignOutResidueViewTest`).


def _residue_surface(app, expected: str):
    """`(expected line, reader, element id)` for the residue surface — the same
    package on every seat."""
    return expected, app.onboarding.sign_out_residue_text, "sign-out-residue-message"


def _require_residue_surface(app, client: str) -> None:
    """Gate a journey that drives the package itself (retry, persistence) to
    the seats whose fault can be injected from here — declared, never a bare
    skip (convention 7). Web paints the package and still cannot run these
    journeys: their fault is a read-only scope directory, and web's residue is an
    account store in IndexedDB (the web paragraph's decision 4). Its twins are
    `test_sign_out_web.py`'s retry journey and later-load journey."""
    if client == "web":
        app_surface.declared_absence(
            app.driver,
            capability="on-disk account-scoped state directory",
            doc="docs/goal/architecture/long-term-store.md § Implementation status today",
        )


def _skip_android_host_fault_injection(app) -> None:
    """android paints the residue package (`AccountStores.recordResidue` →
    `AppState.signOutResidue` → `identity_choice`'s `sign-out-residue` view),
    but every residue journey injects its fault from the host, and android's
    scoped directories are on-device — declared, not a bare skip."""
    app_surface.skip_unbuilt(
        app.driver,
        surface="host-side account-scope directory read-back",
        detail=(
            "android's residue surface LANDED (pinned by AccountStoresEraseTest and "
            "SignOutResidueViewTest), but its scoped directories are on-device in the "
            "app's own filesDir, so the host cannot chmod one to inject the fault — "
            "the same gap attach_cred_store documents for credentials"
        ),
        tracked="docs/goal/architecture/long-term-store.md § Implementation status today",
    )


def _seeded_scope(app, client: str, test_user):
    """The account-scoped directory the post-auth hook always populates — see
    `test_a_sign_out_that_cannot_erase_everything_says_so` for why exactly this
    one is guaranteed non-empty after sign-in."""
    if client == "android":
        _skip_android_host_fault_injection(app)
    store = attach_scope_store(client, app.driver)
    actor = test_user["actor_id_hex"]
    seeded = [d for d in store.scope_dirs(actor) if d.is_dir() and any(d.iterdir())]
    assert seeded, (
        "precondition: at least one account-scoped directory must exist and hold "
        "something after sign-in — an empty scope has nothing for the erase to "
        "fail on, so this test would assert nothing at all "
        f"(client={client}, actor={actor}, "
        f"checked={[str(d) for d in store.scope_dirs(actor)]}, "
        f"error={app.error_text()!r})"
    )
    return seeded[0]


def _sqlite_file_to_hold(scope):
    """The SQLite file inside `scope` the windows fault holds open, or None.

    A real SQLite file specifically — `_held_open_no_share` needs one to connect
    to (mirroring "a live SQLite connection", the actual holder the fault
    reproduces), not just any regular file."""
    return next(
        (p for p in sorted(scope.rglob("*")) if p.is_file() and p.suffix in (".db", ".sqlite")),
        None,
    )


@contextlib.contextmanager
def _undeletable_scope(scope, client: str):
    """Make `scope` survive an erase while it stays readable, in this seat's
    platform shape (the docstring of
    `test_a_sign_out_that_cannot_erase_everything_says_so` says why each shape
    is the only one that proves anything on its platform). The body after the
    `with` is the fault LIFTED.

    * **windows: a held SQLite lock on one file inside the scope**
      (`_held_open_no_share`). Lifting it must leave the scope directory exactly
      as the sign-out recorded it, because the re-sweep leaves alone any path
      whose modification time moved since (`fauna_client_accounts::
      sign_out_residue`, rule 3 — a later sign-in re-creating the store) and
      would then drop the scope from the record untouched. It does: the store is
      on the rollback journal, and an `BEGIN EXCLUSIVE` that never writes never
      creates the journal file, so neither taking nor releasing the lock adds or
      removes a directory entry.
    * **POSIX: a read-only scope DIRECTORY.** Probes the injection
      non-destructively and skips as an ENVIRONMENT fact when this process
      writes through mode 0o555 (root), because a test that silently proves
      nothing is worse than none. Restores the mode on exit (a mode change moves
      the directory's ctime, never its mtime).
    """
    if client == "windows":
        held = _sqlite_file_to_hold(scope)
        assert held is not None, (
            f"precondition: {scope} must contain at least one SQLite file to hold "
            "open — an empty scope has nothing for the held-lock fault to attach "
            f"to (contents={[p.name for p in scope.rglob('*') if p.is_file()]})"
        )
        with _held_open_no_share(held):
            yield
        return

    original_mode = stat.S_IMODE(scope.stat().st_mode)
    os.chmod(scope, 0o555)
    try:
        probe = scope / ".fault-injection-probe"
        try:
            probe.touch()
        except PermissionError:
            pass
        else:
            probe.unlink(missing_ok=True)
            app_surface.skip_environment(
                "this process writes through mode 0o555 (root in a container?), so "
                "denying the write bit cannot make the erase fail — the fault is "
                "never injected and the test would pass for the wrong reason"
            )
        yield
    finally:
        # Restore before the harness's own teardown tries to remove the tree.
        with contextlib.suppress(FileNotFoundError):
            os.chmod(scope, original_mode)


def _await_residue_gone(app, scope) -> tuple[bool, bool]:
    """Poll until the residue view is gone AND `scope` is removed, or the budget
    runs out. Returns `(view_gone, scope_gone)` for the caller to assert on —
    the two halves fail differently (a re-sweep that erased but kept talking,
    or one that went quiet with the data still there)."""
    deadline = time.monotonic() + _RESIDUE_PAINT_BUDGET_S
    while True:
        view_gone = not app.onboarding.sign_out_residue_showing()
        scope_gone = not scope.exists()
        if (view_gone and scope_gone) or time.monotonic() >= deadline:
            return view_gone, scope_gone


@pytest.mark.feature("account")
def test_sign_out_returns_to_onboarding(logged_in_app):
    app = logged_in_app

    app.settings.sign_out()

    # Back at onboarding identity_choice — the stored secret was wiped, so the
    # user must re-create or import an identity to sign back in.
    assert app.is_visible("create-identity-button"), (
        "after sign-out the app should re-root onboarding at identity_choice; "
        f"create-identity-button not visible. error={app.error_text()!r}"
    )
    assert app.is_visible("import-identity-button"), (
        "identity_choice should also offer import-identity-button after sign-out"
    )

    # A CLEAN sweep says nothing. The residue line
    # (`test_a_sign_out_that_cannot_erase_everything_says_so`) rides
    # `error-message` on the surface a sign-out hands back, so the ordinary
    # sign-out landing here must leave that surface silent — *"0 items were left
    # behind"* is reassurance by vacuity, and a residue line that appeared on
    # every sign-out would be worse than none at all (`account-scoping.md`
    # § Erasure follows scope → *A clean sweep says nothing*). This is also what
    # keeps the residue assertion below honest: without it, a seat that painted
    # the line unconditionally would pass that test.
    assert not app.has_error(), (
        "a sign-out whose erase left nothing behind must paint no error at all, "
        f"but error-message reads {app.error_text()!r}"
    )


@pytest.mark.feature("account")
def test_sign_out_erases_the_credential_namespace(logged_in_app):
    """Sign out ⇒ the client's credential namespace reads empty — AND the
    shared account-store namespace beside it does too.

    ``long-term-store.md`` § Cleanup contract: the erase covers *every* account's
    ``fauna/{actor_id}/*`` slots, the ``fauna/index`` blob — not merely
    the active account. Two of
    its three properties hang directly on this assertion: a surviving
    ``fauna/{actor}/secret`` leaves the user's Ed25519 key recoverable on a shared
    machine, and a surviving ``fauna/index`` boots the signed-out account on
    the next launch, signing them back in as the identity they signed out of.

    A THIRD property (hole 3, closed 2026-09-01) hangs on the second half of this
    test: the erase must *also* reach the shared ``fauna-account-store`` namespace
    — this machine's writer key and each account's ``principal_bundle`` slots,
    written through a namespace distinct from any app's own. Before the 2026-09-01
    fix `AccountRegistry` only ever wiped the app's own namespace, so that second
    store survived every sign-out in production while staying invisible to every
    e2e: every driver's ``FAUNA_KEYRING_APP`` collapsed the two namespaces onto one
    physical store under the harness. That collapse is now fixed at the source
    (``fauna_credential_store::apply_namespace_override`` derives the
    account-store's override instead of reusing the app's), so this second half
    can finally assert what production actually does.

    The stores are read through the *live* namespaces this driver launched with
    (``attach_cred_store`` / ``attach_account_store``), so they are the app's own
    and the account-store's real file/keyring/foreign-seam rows the assertions
    inspect — not ones the test arranged and could therefore have swept itself.

    ⚠ **Both precondition asserts are load-bearing, not ceremony.** Every adapter
    reports a missing/unreadable store as an empty set, so an attach that resolved
    the wrong path would make the post-condition pass on emptiness that was never
    an erase. Asserting each namespace is non-empty *first* is what distinguishes
    "sign-out erased it" from "there was nothing there" — the exact vacuity this
    module carried while its docstring claimed the coverage, for the app's own
    namespace and (until this test's own hole-3 widening) for the account-store's
    too.
    """
    app = logged_in_app
    client = app_surface.app_name(app.driver)
    store = attach_cred_store(client, app.driver)

    before = store.stored_accounts()
    assert before, (
        "precondition: the logged-in app must have persisted credentials — that is "
        "what sign-out has to erase. An empty namespace here means this test would "
        "assert nothing at all, so it fails rather than passing vacuously "
        f"(client={client}, error={app.error_text()!r})"
    )

    # web has no account runtime (`succession::account_registry` is its own,
    # browser-only store) and therefore no second namespace to attach to.
    account_store = None if client == "web" else attach_account_store(client, app.driver)
    if account_store is not None:
        # The mint this precondition reads for (the writer key + the
        # principal-bundle slots) is a task spawned OFF the login path
        # (`apps/fauna-linux/src/account_runtime.rs`'s `install` module docs)
        # — `wait_until_online` above covers transport only, not this. Without
        # this barrier the read races that background task: reliably fine in
        # isolation (assembly usually wins the race against however long it
        # takes pytest to reach this line), reliably losing when this app's
        # login lands hot on another app's in the same session and the box is
        # that much more loaded.
        await_account_runtime_assembled(app.driver)
    account_before = account_store.stored_accounts() if account_store else None
    if account_store is not None:
        assert account_before, (
            "precondition: sign-in must have minted the shared fauna-account-store "
            "namespace's writer key / principal-bundle slots too — an empty "
            "account-store namespace here means this half of the test would assert "
            f"nothing at all (client={client}, error={app.error_text()!r})"
        )

    app.settings.sign_out()

    # `signed-out/v1` is the sync agent's OWN cross-boot safety marker
    # (`fauna_ipc::sync::SIGNED_OUT_KEY`), never deleted by design — it must
    # outlive this sign-out so a later boot can refuse to resume a capability
    # whose un-provision message got lost (`fauna-credential-store/src/lib.rs`
    # § `account_scoped_aux_stores`, `bins/fauna-sync-agent/src/credentials.rs`
    # `restore_authorized_capability`). Under e2e, `FAUNA_KEYRING_APP`
    # deliberately collapses the agent's own namespace onto this app's
    # (`apply_namespace_override`'s own test asserts exactly that collapse),
    # so a co-located isolated agent's marker becomes visible here — a
    # production keyring never mixes the two. Not a leaked secret: the marker
    # carries no key material, only "this actor signed out at this time".
    survivors = store.stored_accounts() - {"signed-out/v1"}
    assert survivors == set(), (
        "sign-out must erase the client's WHOLE credential namespace "
        "(long-term-store.md § Cleanup contract), but these slots survived it: "
        f"{sorted(survivors)}. A surviving per-actor secret leaves the user's key "
        "recoverable on a shared machine; a surviving index boots the "
        f"signed-out account on the next launch. (before={sorted(before)})"
    )

    if account_store is not None:
        account_survivors = account_store.stored_accounts()
        assert account_survivors == set(), (
            "sign-out must ALSO erase the shared fauna-account-store namespace "
            "(long-term-store.md § Cleanup contract, hole 3), but these slots "
            f"survived it: {sorted(account_survivors)}. A surviving writer key or "
            "principal-bundle slot leaves this machine's store-device identity for "
            "the signed-out account recoverable. "
            f"(before={sorted(account_before)})"
        )


@pytest.mark.feature("account")
def test_sign_out_erases_the_account_scoped_state_directory(logged_in_app, test_user):
    """Sign out ⇒ every account-scoped state DIRECTORY this launch owns is
    gone from disk — the on-disk half `test_sign_out_erases_the_credential_namespace`
    does NOT cover.

    `account-scoping.md` § Erasure follows scope: sign-out "erases *every*
    account-scoped store, not only the credential namespace." The credential
    test above reads specific KNOWN files in the credential/account-store
    namespaces; this test reads the app's own account-scoped state tree
    instead — the directory `mls_state.db` and friends live under
    (`<flat-base>/<actor-id-hex>/`) — plus the sibling unified
    `StoreRoot::platform()` root the same actor scopes under. The doc's own
    ⚠ names the exact failure this closes: that root is "a sibling of an
    app's flat base, not a child of it", so an erase (or a TEST) that
    iterates only the app's own base silently misses it, and the miss is a
    signed-OUT user's content surviving in a directory the next sign-in
    silently re-adopts (refinement 10's self-heal is what makes the survivor
    invisible rather than fatal).

    Only "at least one scope directory is non-empty" is the load-bearing
    precondition, not "every one is" — every driver's post-auth hook opens
    the MLS engine synchronously against the flat base
    (`apps/fauna-tui/src/app.rs`'s `AuthSuccess` arm and its linux twin both
    call `account_scope::account_state_dir` there), so THAT directory always
    exists the moment `logged_in_app` returns; the unified sync root and the
    backup-audit dir may legitimately be empty pre-sign-out (nothing synced
    or backed up yet) — checking each one for survival only if it existed
    pre-sign-out avoids a false failure on an app that never populated it.
    """
    app = logged_in_app
    client = app_surface.app_name(app.driver)

    if client == "android":
        app_surface.skip_unbuilt(
            app.driver,
            surface="host-side account-scope directory read-back",
            detail=(
                "android's account-scoped directories are on-device, in the app's "
                "own filesDir; no host path exists to read them back — the same "
                "gap attach_cred_store documents for the credential namespace"
            ),
            tracked="docs/goal/architecture/long-term-store.md § Implementation status today",
        )
    if client == "web":
        app_surface.declared_absence(
            app.driver,
            capability="on-disk account-scoped state directory",
            doc="docs/goal/architecture/long-term-store.md § Implementation status today",
        )

    store = attach_scope_store(client, app.driver)
    actor = test_user["actor_id_hex"]
    dirs = store.scope_dirs(actor)

    existed_before = [d for d in dirs if d.exists()]
    assert existed_before, (
        "precondition: at least one account-scoped directory must exist after "
        "sign-in — that is what sign-out has to erase. An empty set here means "
        "this test would assert nothing at all, so it fails rather than passing "
        f"vacuously (client={client}, actor={actor}, "
        f"checked={[str(d) for d in dirs]}, error={app.error_text()!r})"
    )

    app.settings.sign_out()

    survivors = [d for d in existed_before if d.exists()]
    assert survivors == [], (
        "sign-out must erase EVERY account-scoped directory this launch owns "
        "(account-scoping.md § Erasure follows scope), but these survived it: "
        f"{_describe_survivors(survivors)}. A surviving directory is a signed-out "
        "user's content left on disk for the next sign-in to silently re-adopt "
        f"(client={client}, actor={actor})"
    )


@pytest.mark.feature("account")
def test_a_sign_out_that_cannot_erase_everything_says_so(logged_in_app, test_user):
    """Sign out with the erase made to FAIL ⇒ the app must not report a clean
    sign-out.

    ⚠ **The defect this closes is not that sign-out proceeds — it is that
    proceeding was indistinguishable from succeeding.** The erase is best-effort
    by design and must stay that way (`account-scoping.md` § Erasure follows
    scope: sign-out completes even on an I/O error). But every seat turned the
    failure into a log line no user reads and painted a clean "Signed out" over a
    device that still held the user's readable MLS and account stores. A log is
    not an affordance — `principles.md` § The user always controls their data
    puts the delete affordance in the app.

    Not theoretical: Windows cannot delete an open file, so an antivirus scanner,
    a search indexer or a backup agent holding a handle produces this outcome with
    no defect on our side at all. Every instance so far was caught by an e2e test;
    in production each would have painted a clean "Signed out".

    **The fault injection is PLATFORM-SPECIFIC, and the choice on each is
    load-bearing — neither shape proves anything on the other platform:**

    * **POSIX (tui/linux/macOS/iOS): a read-only SCOPE DIRECTORY.** A held
      handle is the WRONG shape here — POSIX `unlink` removes an open file
      happily, which is exactly why this defect stayed invisible outside
      Windows for as long as it existed. A read-only PARENT of the scope
      would be a third, also-wrong shape: `remove_dir_all` unlinks the files
      inside the scope first (which needs write on the SCOPE, still granted)
      and only then fails to `rmdir` the scope itself. The user's data is
      therefore **gone** and an empty directory survives: the sweep reports a
      survivor, but nothing a user would care about was left behind, and any
      precondition written as "the seeded file still exists" silently skips
      instead. That cost one round of a green-under-mutation test before it
      was caught. Denying the write bit on
      the scope's own directory is what makes real, readable user data
      survive.
    * **Windows: a HELD HANDLE on one file inside the scope**, opened with
      `dwShareMode=0` — the restrictive kind a live SQLite connection
      actually holds, established by
      `libs/fauna-ffi/src/account_state.rs`'s
      `an_undeletable_app_scope_does_not_spare_the_store_root` unit test. A
      read-only directory is the WRONG shape here: Windows' `remove_dir_all`
      still deletes every file it CAN inside a read-only-by-owner directory
      (the ACL check that matters is on each file, not the listing), so the
      failure this test reproduces — one specific undeletable FILE — needs a
      held handle, not a permission bit. Either way `erase_all_account_scopes`
      records exactly one survivor: the whole actor-scope directory the failed
      removal was attempted on, never the nested file
      (`libs/fauna-account-store/src/db.rs::EraseSweep::remove_dir`) — which is
      why `count="1"` is the right expectation on both platforms.
    """
    app = logged_in_app
    client = app_surface.app_name(app.driver)

    # Web's twin of this journey holds the account store's IndexedDB database
    # open instead of a file under a directory web does not have:
    # `test_sign_out_web.py::test_web_sign_out_that_cannot_erase_a_store_says_so_and_a_later_load_finishes`.
    if client == "web":
        app_surface.declared_absence(
            app.driver,
            capability="on-disk account-scoped state directory",
            doc="docs/goal/architecture/long-term-store.md § Implementation status today",
        )
    if client == "android":
        _skip_android_host_fault_injection(app)
    store = attach_scope_store(client, app.driver)
    actor = test_user["actor_id_hex"]

    # The scope the post-auth hook always populates: every driver opens the MLS
    # engine synchronously against the flat base before `logged_in_app` returns,
    # so exactly this directory is guaranteed non-empty here — the same fact
    # `test_sign_out_erases_the_account_scoped_state_directory` relies on.
    seeded = [d for d in store.scope_dirs(actor) if d.is_dir() and any(d.iterdir())]
    assert seeded, (
        "precondition: at least one account-scoped directory must exist and hold "
        "something after sign-in — an empty scope has nothing for the erase to "
        "fail on, so this test would assert nothing at all "
        f"(client={client}, actor={actor}, "
        f"checked={[str(d) for d in store.scope_dirs(actor)]}, "
        f"error={app.error_text()!r})"
    )
    scope = seeded[0]

    if client == "windows":
        pinned = _sqlite_file_to_hold(scope)
        assert pinned is not None, (
            f"precondition: {scope} must contain at least one SQLite file to pin "
            f"open — an empty scope has nothing for the held-handle fault to "
            f"attach to (client={client}, actor={actor}, "
            f"contents={[p.name for p in scope.rglob('*') if p.is_file()]})"
        )
        with _held_open_no_share(pinned):
            app.settings.sign_out()

            assert pinned.exists(), (
                "precondition: the held-open file is what makes the user's data "
                f"survive the erase, but {pinned} is gone — the fault was not "
                "injected, so the assertion below would be meaningless"
            )

            # sign_out()'s own wait targets create-identity-button, which
            # OnboardingPage attaches at CONSTRUCTION time — before
            # OnNavigatedTo hands the residue to the view a few ms later.
            expected, read, where = _residue_surface(
                app, S.settings.sign_out_residue(count="1")
            )
            actual = _await_residue_line(app, expected, read)
            assert actual and expected in actual, (
                "a sign-out that could not remove the user's data must SAY so, on "
                "the surface it hands the user — proceeding is not the defect, "
                "proceeding while looking identical to succeeding is "
                f"(account-scoping.md § Erasure follows scope). {where} reads "
                f"{actual!r}, expected to contain {expected!r}. {pinned} is still "
                "held open, so the residue is real; if this app logged a warning "
                "instead, that is precisely the finding: a log is not an "
                "affordance."
            )
        return

    with _undeletable_scope(scope, client):
        app.settings.sign_out()

        assert scope.is_dir() and any(scope.iterdir()), (
            "precondition: the read-only scope is what makes the user's data "
            f"survive the erase, but {scope} is gone or empty — the fault was not "
            "injected, so the assertion below would be meaningless"
        )

        expected, read, where = _residue_surface(app, S.settings.sign_out_residue(count="1"))
        actual = _await_residue_line(app, expected, read)
        assert actual and expected in actual, (
            "a sign-out that could not remove the user's data must SAY so, on the "
            "surface it hands the user — proceeding is not the defect, proceeding "
            "while looking identical to succeeding is (account-scoping.md § Erasure "
            f"follows scope). {where} reads {actual!r}, expected to contain "
            f"{expected!r}. The scope {scope} still holds "
            f"{sorted(p.name for p in scope.iterdir())}, so the residue is real; if "
            "this app logged a warning instead, that is precisely the finding: a log "
            "is not an affordance."
        )


@contextlib.contextmanager
def _credential_erase_refused(cred_dir: str):
    """Make the app's credential store refuse the erase while it can still be
    READ — the file backend's shape of a locked or failing keyring.

    The shape per platform is load-bearing, for the same reasons as the scope
    fault above:

    * **POSIX: a read-only credential DIRECTORY.** Every rewrite of a namespace
      file is a temp-file-plus-rename in that directory, and the wholesale wipe
      is an unlink in it, so all of them fail while the file itself stays
      readable. That is exactly the outcome a read-back must catch: the delete
      reported nothing, the key is still there.
    * **Windows: the read-only ATTRIBUTE on each file.** Windows mode bits on a
      directory do not deny entry creation, but a read-only file can be neither
      deleted nor replaced by a rename.

    Probes the injection non-destructively on POSIX and skips as an ENVIRONMENT
    fact when this process writes through a read-only directory (root), because
    a test that silently proves nothing is worse than none.
    """
    if os.name == "nt":
        files = [
            path
            for path in (os.path.join(cred_dir, name) for name in os.listdir(cred_dir))
            if os.path.isfile(path)
        ]
        try:
            for path in files:
                os.chmod(path, stat.S_IREAD)
            yield
        finally:
            for path in files:
                with contextlib.suppress(OSError):
                    os.chmod(path, stat.S_IREAD | stat.S_IWRITE)
        return

    original_mode = stat.S_IMODE(os.stat(cred_dir).st_mode)
    os.chmod(cred_dir, 0o555)
    try:
        probe = os.path.join(cred_dir, ".fault-injection-probe")
        try:
            with open(probe, "w"):
                pass
        except PermissionError:
            pass
        else:
            os.unlink(probe)
            app_surface.skip_environment(
                "this process writes through mode 0o555 (root in a container?), so "
                "denying the write bit cannot make the credential erase fail — the "
                "fault is never injected and the test would pass for the wrong reason"
            )
        yield
    finally:
        # Restore before anything else touches the store: the next test's
        # `reset()` erases these very credentials, and must be able to.
        os.chmod(cred_dir, original_mode)


@pytest.mark.feature("account")
def test_a_sign_out_whose_credentials_cannot_be_erased_says_so(logged_in_app):
    """Sign out with the CREDENTIAL erase made to fail ⇒ the app must not report a
    clean sign-out, even though every account-scoped directory went.

    ⚠ **The defect this closes is the residue line asking only half the erase.**
    The line the scope test above pins was fed by the filesystem sweep alone; the
    credential wipe — the store holding the identity seed, the index and the
    writer key — was a separate call whose failure reached no one: linux built
    the line and then ran `let _ = client::delete_credentials()`, tui `warn!`ed,
    and the three FFI seats' store delete reports nothing at all. So a sign-out
    whose keyring refused the wipe painted a clean "Signed out" over a device
    that still held everything needed to sign straight back in
    (`account-scoping.md` § Erasure follows scope → *the credential half is a
    residue class too*; `long-term-store.md` § Cleanup contract, property 1).

    The erase is read back in shared Rust (`AccountRegistry::clear_all` returns a
    `CredentialSweep`), so the fault only has to make the store keep what it was
    told to delete — see `_credential_erase_refused` for the per-platform shape.

    The scope directories stay writable, so the filesystem half is clean and the
    expected line is the credentials-only one: a seat that still built the line
    before the credential wipe paints nothing here, and one that folded the two
    wrongly paints the wrong copy.
    """
    app = logged_in_app
    client = app_surface.app_name(app.driver)

    if client == "web":
        app_surface.declared_absence(
            app.driver,
            capability="credential half of the sign-out residue (localStorage removal cannot fail)",
            doc="docs/goal/architecture/apps/account-scoping.md § Erasure follows scope",
        )
    if client == "android":
        app_surface.skip_unbuilt(
            app.driver,
            surface="host-side credential fault injection",
            detail=(
                "android's credential file is written by the on-device bridge into the "
                "app's own filesDir, so the host can neither read it back nor deny it "
                "writes — the same gap attach_cred_store documents. The seat's wiring "
                "(registry.clearAll → secureStorage.clear → reverifyErase → "
                "AccountStores.reportResidue) is pinned by its unit tests"
            ),
            tracked="docs/goal/architecture/long-term-store.md § Implementation status today",
        )

    store = attach_cred_store(client, app.driver)
    before = store.stored_accounts()
    assert before, (
        "precondition: the logged-in app must have persisted credentials — that is "
        "what the refused erase has to keep. An empty namespace here means this test "
        "would assert nothing at all, so it fails rather than passing vacuously "
        f"(client={client}, error={app.error_text()!r})"
    )
    cred_dir = getattr(app.driver, "_resolved_credential_dir", None)
    assert cred_dir and os.path.isdir(cred_dir), (
        "precondition: this fault injection needs the file-backed credential store "
        f"every isolated launch uses, but the driver recorded {cred_dir!r} "
        f"(client={client})"
    )

    with _credential_erase_refused(cred_dir):
        app.settings.sign_out()

        survivors = store.stored_accounts()
        assert survivors, (
            "precondition: the refused erase is what keeps the credentials on this "
            f"device, but the namespace under {cred_dir} reads empty — the fault was "
            "not injected, so the assertion below would be meaningless "
            f"(before={sorted(before)})"
        )

        expected, read, where = _residue_surface(app, S.settings.sign_out_residue_credentials)
        actual = _await_residue_line(app, expected, read)
        assert actual and expected in actual, (
            "a sign-out whose credential erase was refused must SAY so, on the "
            "surface it hands the user — the scope directories all went, so a seat "
            "that asks only the filesystem sweep paints nothing here "
            f"(account-scoping.md § Erasure follows scope). {where} reads "
            f"{actual!r}, expected to contain {expected!r}. These credential slots "
            f"are still on disk, so the residue is real: {sorted(survivors)}."
        )


@pytest.mark.feature("account")
def test_the_sign_out_residue_retry_finishes_the_erase(logged_in_app, test_user):
    """Remove Again re-runs the erase over what the sign-out left — and only
    reports clean once it IS clean.

    The package's other half (`sign-out-residue-retry-button`,
    `account-scoping.md` § Erasure follows scope → the residue surface): the
    line on its own offers no way out but signing in and out again. Pressed
    while the fault still holds, the view must stay and keep its line (a retry
    that cleared the view over data still on disk is the original defect,
    re-created one button later). Pressed once the fault is lifted, the recorded
    scope must be gone AND the view with it — a clean sweep says nothing.
    """
    app = logged_in_app
    client = app_surface.app_name(app.driver)
    _require_residue_surface(app, client)
    scope = _seeded_scope(app, client, test_user)
    expected = S.settings.sign_out_residue(count="1")

    with _undeletable_scope(scope, client):
        app.settings.sign_out()
        actual = _await_residue_line(app, expected, app.onboarding.sign_out_residue_text)
        assert actual and expected in actual, (
            f"precondition: the sign-out's residue line must be up on "
            f"sign-out-residue-message before the retry is pressed; it reads "
            f"{actual!r}, expected {expected!r} (error={app.error_text()!r})"
        )

        app.onboarding.retry_sign_out_residue()
        assert scope.is_dir() and any(scope.iterdir()), (
            "precondition: the fault still holds, so the retry cannot have "
            f"removed the scope — but {scope} is gone or empty"
        )
        still = app.onboarding.sign_out_residue_text()
        assert expected in still, (
            "a retry that could not remove the data must keep saying so — the view "
            f"now reads {still!r} (expected {expected!r}) while {scope} still holds "
            f"{sorted(p.name for p in scope.iterdir())}"
        )

    # The fault is lifted: the same button now finishes the job.
    app.onboarding.retry_sign_out_residue()
    view_gone, scope_gone = _await_residue_gone(app, scope)
    assert scope_gone, (
        f"Remove Again must erase what the sign-out left once it can: {scope} "
        f"still holds {_describe_survivors([scope])} "
        f"(view={app.onboarding.sign_out_residue_text()!r}, error={app.error_text()!r})"
    )
    assert view_gone, (
        "a clean re-sweep says nothing — the residue view must disappear, but it "
        f"still reads {app.onboarding.sign_out_residue_text()!r}"
    )


@pytest.mark.feature("account")
def test_the_sign_out_residue_outlives_the_app(logged_in_app, test_user):
    """The residue is on the disk, so what the user is told about it must not
    die with the window.

    A user who signs out, sees the line and closes the app used to be told
    nothing next time, with their data still on the device. The record the
    sign-out writes into install-scoped state closes that: a signed-out launch
    re-sweeps it silently FIRST and paints the view only if something is still
    left (`account-scoping.md` § Erasure follows scope → the residue surface).
    Both arms, one relaunch each: fault still held → the view comes back with
    its line; fault lifted → the launch's silent re-sweep erases the scope and
    the view never appears.
    """
    app = logged_in_app
    client = app_surface.app_name(app.driver)
    _require_residue_surface(app, client)
    if not app.driver.preserve_state_across_relaunch():
        app_surface.skip_unbuilt(
            app.driver,
            surface="a relaunch that keeps the client's install-scoped state",
            detail="without it the record the sign-out wrote is thrown away on relaunch",
            tracked="docs/goal/architecture/apps/account-scoping.md § Erasure follows scope",
        )
    scope = _seeded_scope(app, client, test_user)
    expected = S.settings.sign_out_residue(count="1")

    with _undeletable_scope(scope, client):
        app.settings.sign_out()
        actual = _await_residue_line(app, expected, app.onboarding.sign_out_residue_text)
        assert actual and expected in actual, (
            f"precondition: the sign-out's residue line must be up before the "
            f"relaunch; it reads {actual!r}, expected {expected!r}"
        )

        assert app.driver.recover(), "relaunch after the residue sign-out failed"
        app.driver.wait_for("create-identity-button", timeout=30.0)
        actual = _await_residue_line(app, expected, app.onboarding.sign_out_residue_text)
        assert actual and expected in actual, (
            "a residue still on the disk must still be on the screen after a "
            f"relaunch — sign-out-residue-message reads {actual!r}, expected "
            f"{expected!r}, while {scope} still holds "
            f"{sorted(p.name for p in scope.iterdir())}. A window-scoped line tells a "
            "user who closed the app nothing at all."
        )

    assert app.driver.recover(), "relaunch after lifting the fault failed"
    app.driver.wait_for("create-identity-button", timeout=30.0)
    view_gone, scope_gone = _await_residue_gone(app, scope)
    assert scope_gone, (
        "a signed-out launch must re-sweep the recorded residue silently, but "
        f"{scope} still holds {_describe_survivors([scope])} "
        f"(view={app.onboarding.sign_out_residue_text()!r})"
    )
    assert view_gone, (
        "the launch's re-sweep removed everything, so it must say nothing — the "
        f"residue view still reads {app.onboarding.sign_out_residue_text()!r}"
    )


def _describe_survivors(survivors: list) -> str:
    """Each surviving directory with what is still INSIDE it — the diagnosis, not
    just the symptom.

    The erase is `remove_dir_all`, and on windows that cannot delete an open file
    at all: the sweep aborts on the first locked child and leaves the rest behind.
    So "this directory survived" is one step short of the answer every time — the
    useful fact is WHICH file is still there, because that names the holder that
    was never released (`mls.db` → the conversations session/manager;
    `account-store/` → the account runtime). Reporting only the directory cost
    two full windows e2e cycles of guessing (e2e-conventions.md point 6: read the
    diagnostic INTO the failure message rather than debugging afterwards).

    Best-effort: an unreadable directory reports why, never raises — this runs
    inside an assertion message, where an exception would replace a real failure
    with a confusing one.
    """
    parts = []
    for d in survivors:
        try:
            inside = sorted(p.name + ("/" if p.is_dir() else "") for p in d.iterdir())
        except OSError as exc:  # pragma: no cover - diagnostic path
            inside = [f"<unreadable: {exc}>"]
        parts.append(f"{d} (still holds: {inside or 'nothing — the dir itself is locked'})")
    return "; ".join(parts)
