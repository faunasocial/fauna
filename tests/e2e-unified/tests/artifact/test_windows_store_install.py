"""The Windows app as the MICROSOFT STORE delivered it — activated, then driven.

Subject: the package the Store installed for this Windows user — signed by the
Store, under the identity Partner Center reserved (``FaunaSocial.FaunaSocial``),
in the ``store-no-payments`` flavour the Store ships (``installers/windows.md``
§ Store distribution) — not the debug build every other windows suite drives, not
the MSI, and not the same layout registered with ``Add-AppxPackage -Register``.
The ruling, its reasons and what is still open: ``docs/goal/architecture/
feature-catalog.md`` § Implementation status today, the bullet on how the Windows
Store install is driven.

**Why no TestAgent.** A Store build is a Release build, and convention 15 compiles
the in-app automation surface out of it: no ``/app/state``, no commands, no seed,
no ``FAUNA_E2E_*`` variable read. What a Release build keeps is its UI Automation
tree, whose AutomationIds are the ui.yaml ids, and the FlaUI bridge reads and acts
on that tree from outside the process. So the app is started the way its Start-menu
entry starts it — activation by AUMID, the one launch that gives the process the
package's identity (``WindowsBridgeDriver.launch_activated``) — and everything after
is the bridge's ``/element/*``.

**The subject is asserted from the OS, on three facts** (``helpers/
windows_store_install.py``): ``SignatureKind`` is ``Store``, ``IsDevelopmentMode``
is false, and the running process carries the installed package's full name. The
first two are what a registered layout lacks — it shares the identity, the
publisher and the family name — and the third is what an exe started from the
package's folder lacks.

**The development-mode nodes.** Before the Store serves, the same mechanism runs
against the store-no-payments layout ``just windows-store-package`` staged under
the reserved identity, registered for this user. They carry no ``feature`` mark,
so no run of them can colour ``get-the-app``; one of them asserts that the
Store-subject check REFUSES that layout, so a registered layout can never stand in
for the witness. Only ``test_the_store_installed_app_signs_in_to_the_feed`` is
cited, and it skips as environment unless the Store delivered what is installed.

**The sign-in leg waits on a venue.** A Store build names no credential namespace
of its own (the override is compiled out), so signing in writes this Windows
user's production ``fauna-windows`` rows in Credential Manager — which convention
12's Windows arm refuses (``e2e-conventions.md``, the Windows arm: the namespace is
all that separates a launch from the installed product's rows). Until the venue
that may hold those rows is ruled, both sign-in nodes skip as environment, naming
it. What the ruling arms is
built and serves either answer: the venue must start with no row under a Release
build's namespaces, and every row the run added there is deleted after it
(``sign_in_venue``, ``windows_credman.give_back``).

**This user's catalogue is one set.** Registering, activating and removing all act
on it, so the module takes the box's ``real-desktop-session`` lock, the key the
real-session category serializes the package catalogue and Credential Manager on
(``tests/real_session/conftest.py``, the Windows arm) — which, being ``fcntl``'s,
excludes nothing on Windows today, exactly as for that category — and gives back
what it registered whether it passed or not.
"""

from __future__ import annotations

import contextlib
import time
from pathlib import Path

import pytest

from common import get_repo_root
from drivers import create_driver
from helpers import live_box_lock, windows_credman
from helpers import windows_store_install as store
from helpers.app_surface import skip_environment

# tier_4: the subject is a shipped client artifact as its channel delivered it
# (`testing.md` § The four-tier taxonomy, point 4's shipped-client-artifact arm).
pytestmark = [pytest.mark.tier_4, pytest.mark.windows]

#: The real-session category's key for this user's package catalogue and
#: Credential Manager (`tests/real_session/conftest.py`, `REAL_SESSION_LOCK_KEY`
#: on windows): one owner at a time, machine-wide.
_DESKTOP_SESSION_LOCK_KEY = "real-desktop-session"

#: The Store build's own credential namespace (`CredentialStore.DefaultNamespace`):
#: a Release build cannot be given another.
_PRODUCTION_NAMESPACE = "fauna-windows"

#: Every Credential Manager namespace a Release build or its packaged sync agent
#: writes on this user: the app's own and the agent's process-global one
#: (`libs/fauna-credential-store/src/lib.rs`, `account_scoped_aux_stores`).
_RELEASE_NAMESPACES = (_PRODUCTION_NAMESPACE, "fauna-sync-agent")

#: Those namespaces as Credential Manager targets are walked: one prefix each.
_RELEASE_PREFIXES = tuple(f"{namespace}/" for namespace in _RELEASE_NAMESPACES)

#: Whether the owner has ruled the venue a Store build's sign-in may write its
#: production rows on: this box's user approved as disposable, or a Windows user
#: of its own. The session recording the ruling flips it; `sign_in_venue`'s
#: give-back is the same under either answer.
_SIGN_IN_VENUE_RULED = False

#: Where a fresh install opens: onboarding's identity choice.
_IDENTITY_CHOICE_ID = "create-identity-button"

#: ui.yaml ids of that screen, read off the Release build's UIA tree to show the
#: tree carries the ids with no in-app seam.
_IDENTITY_CHOICE_IDS = ("create-identity-button", "import-identity-button")


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------
@pytest.fixture(scope="module")
def desktop_session():
    """Hold the box's `real-desktop-session` lock for the module (a no-op on
    Windows while `live_box_lock` is `fcntl`-based)."""
    fd = live_box_lock.acquire(_DESKTOP_SESSION_LOCK_KEY)
    try:
        yield
    finally:
        live_box_lock.release(fd)


@pytest.fixture(scope="module")
def store_install(desktop_session) -> store.PackageFacts:
    """The Store delivery of the app for this user, or an environment skip
    naming what is installed instead.

    Never fails on a non-Store package: the development-mode fixture below
    registers one under the same identity, and a sideload is equally not this
    witness's subject — either way the witness has nothing to drive."""
    facts = store.package_facts()
    if facts is None:
        skip_environment(
            f"no {store.IDENTITY_NAME} is installed for this Windows user; the witness "
            f"needs Fauna obtained from the Microsoft Store by an account in the app's "
            f"private audience"
        )
    problems = store.store_subject_problems(facts)
    if problems:
        skip_environment(
            f"the installed {store.IDENTITY_NAME} is not a Store delivery "
            f"({'; '.join(problems)}) — a registered layout or a sideload is never this "
            f"witness's subject"
        )
    return facts


@pytest.fixture(scope="module")
def registered_layout(desktop_session) -> store.PackageFacts:
    """The store-no-payments layout `windows-store-package` staged under the
    reserved identity, registered for this user, removed afterwards.

    Read, never built here (convention 9's prebuild bullet): the build is the
    recipe's, and it is a release build of the whole payload."""
    repo = Path(get_repo_root())
    manifest = store.staged_layout_manifest(repo)
    recipe = store.STORE_LAYOUT_RECIPE.format(arch=store.host_arch())
    if not manifest.is_file():
        pytest.fail(
            f"no staged Store layout at {manifest} — run `{recipe}` first", pytrace=False
        )
    name, publisher = store.layout_identity(manifest)
    if (name, publisher) != (store.IDENTITY_NAME, store.STORE_PUBLISHER):
        pytest.fail(
            f"the staged layout at {manifest} declares {name} by {publisher}, not the "
            f"reserved identity — re-run `{recipe}`", pytrace=False
        )
    existing = store.package_facts()
    if existing is not None:
        if not store.store_subject_problems(existing):
            skip_environment(
                "the Microsoft Store install is present, and registering the layout "
                "would replace the witness's own subject; the development-mode nodes run "
                "where the Store install is absent"
            )
        if not store.registered_from(existing, manifest):
            pytest.fail(
                f"{store.IDENTITY_NAME} is already installed from "
                f"{existing.install_location} (SignatureKind {existing.signature_kind}, "
                f"development mode {existing.is_development_mode}) — not this module's "
                f"layout, so it is not this module's to replace. Remove it "
                f"(`Get-AppxPackage {store.IDENTITY_NAME} | Remove-AppxPackage`) and run "
                f"again", pytrace=False
            )
    facts = store.register_layout(manifest)
    try:
        yield facts
    finally:
        store.remove_package()


@pytest.fixture
def no_fauna_account_on_this_user():
    """Refuse to start a Store build on a Windows user who holds Fauna accounts.

    The Store build reads this user's real Credential Manager under its production
    namespace and would route straight into the active account — signing in as
    someone's real identity and refreshing its rows. Only a user with no row there
    is one the build opens on onboarding for."""
    rows = windows_credman.rows_under(f"{_PRODUCTION_NAMESPACE}/")
    if rows:
        skip_environment(
            f"this Windows user's Credential Manager holds {len(rows)} row(s) under the "
            f"Store build's production namespace {_PRODUCTION_NAMESPACE!r}; a Store build "
            f"would open signed in as that account, so the witness never starts one here"
        )


@pytest.fixture(scope="module")
def sign_in_venue():
    """The venue whose production credential rows a sign-in may write, and the
    give-back of every row the run gained there.

    A Store build names no namespace of its own, so its sign-in writes this
    Windows user's `fauna-windows` rows (and its packaged agent's); convention
    12's Windows arm lets a launch reach Credential Manager only under a namespace
    unique to it. The venue that may hold a Store build's rows — a Windows user of
    its own, or this box's user approved as disposable — is the owner's ruling;
    until `_SIGN_IN_VENUE_RULED` records it, the sign-in nodes skip here before
    anything starts.

    Under either answer the venue starts with no row under a release namespace —
    one that holds any is refused, so a run never owes a row back — and after the
    module every row the run added there is deleted
    (`windows_credman.give_back`); nothing is written or restored."""
    if not _SIGN_IN_VENUE_RULED:
        skip_environment(
            f"the sign-in leg writes this Windows user's production Credential Manager "
            f"namespace {_PRODUCTION_NAMESPACE!r} (a Store build cannot be given another), "
            f"which convention 12's Windows arm refuses; the venue that may hold those "
            f"rows is the owner's ruling"
        )
    before = _release_rows()
    if before:
        skip_environment(
            f"this Windows user's Credential Manager holds {len(before)} row(s) under the "
            f"Store build's namespaces {list(_RELEASE_PREFIXES)}; the give-back deletes "
            f"only the rows a run added and restores none, so a venue starts with none"
        )
    try:
        yield
    finally:
        windows_credman.give_back(before, _RELEASE_PREFIXES)


@pytest.fixture(scope="module")
def store_nest(sign_in_venue, request, nest_mode, tmp_path_factory):
    """A nest a Release build signs in to with no seed: TLS on its loopback
    authority, trusted on first contact (`transport-trust.md`, the LAN-IP and
    loopback row), with a handle domain equal to that authority so a typed
    `<handle>@127.0.0.1:<port>` resolves to it — the shape
    `test_onboarding_launch_routing_smoke.py::smoke_handled_nest` drives a real
    onboarding completion against."""
    from common.auth import open_registration
    from common.nest import OWN_DIAL_AUTHORITY
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "store-install-nest",
        handle_domain_seed=OWN_DIAL_AUTHORITY, serve_tls=True,
    )
    open_registration(nest)
    nest["authority"] = nest["handle_domain_seed"]
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture(scope="module")
def store_actor(store_nest):
    """An already-registered non-admin actor on `store_nest`, whose key the
    journey types into the app's own import screen."""
    from helpers.admin_wire import admit_adult

    return admit_adult(store_nest, "storeinstall", domain=store_nest["authority"], token=True)


# ---------------------------------------------------------------------------
# The journey
# ---------------------------------------------------------------------------
def _activated(facts: store.PackageFacts):
    """A windows driver whose app the OS activated from the installed package."""
    driver = create_driver("windows")
    try:
        driver.launch_activated(
            facts.aumid, app_path=str(facts.executable), ready_id=_IDENTITY_CHOICE_ID
        )
    except BaseException:
        with contextlib.suppress(Exception):
            driver.teardown()
        raise
    return driver


def _release_rows() -> list[windows_credman.CredentialRow]:
    """This user's rows under every namespace a Release build writes."""
    return [row for prefix in _RELEASE_PREFIXES for row in windows_credman.rows_under(prefix)]


def _blobs_by_target(rows: list[windows_credman.CredentialRow]) -> dict[str, bytes]:
    return {row.target: row.blob for row in rows}


def _assert_runs_as(driver, facts: store.PackageFacts) -> None:
    problems = store.process_identity_problems(driver.app_package_full_name(), facts)
    assert not problems, "; ".join(problems)


def _sign_in_through_the_app(driver, nest: dict, actor: dict) -> None:
    """Sign in the way a person with an account does, through the app's own UI
    alone: import the key, give the handle, let the app check it, continue, and
    arrive at the feed. Nothing is seeded; the nest's identity is pinned on first
    contact by the app itself."""
    from actions import ActionLayer

    app = ActionLayer(driver)
    app.onboarding.import_key(bytes(actor["signing_key"]).hex())
    driver.wait_for("handle-input", timeout=60)
    app.onboarding.fill_handle(f"{actor['handle']}@{nest['authority']}")
    app.onboarding.run_handle_check(timeout=45)
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:  # deadline-ok: waits on the button's enabled state
        if driver.is_visible("handle-entry-continue-button") and driver.is_enabled(
                "handle-entry-continue-button"):
            break
        time.sleep(0.5)  # sleep-ok: poll interval of a state read
    else:
        raise AssertionError(
            f"Continue never enabled — the handle check did not pass. handle-message-area: "
            f"{driver.get_text('handle-message-area')!r}; "
            f"{driver.diagnose('handle-entry-continue-button')}"
        )
    app.onboarding.submit_handle()
    # The welcome-back path may show a terminal wizard step before the app mounts
    # (the network choice, a launch retry); pressing it is the user's own next act.
    deadline = time.monotonic() + 180
    while time.monotonic() < deadline:  # deadline-ok: the exit condition is arrival
        if driver.is_visible("feed-tab"):
            return
        for step in ("nat-mode-confirm-button", "launch-retry-button"):
            with contextlib.suppress(Exception):
                if driver.is_visible(step):
                    driver.click(step)
        time.sleep(1.0)  # sleep-ok: poll interval of an arrival read
    error = ""
    with contextlib.suppress(Exception):
        error = driver.get_text("error-message")
    raise AssertionError(
        f"the app never reached the feed after signing in as {actor['handle']!r}; "
        f"error-message: {error!r}; {driver.diagnose('feed-tab')}"
    )


# ---------------------------------------------------------------------------
# The witness: the Store delivery
# ---------------------------------------------------------------------------
def test_the_store_delivered_the_installed_package(store_install):
    """The subject on its own: what is installed is the Store's delivery of the
    reserved identity."""
    assert store_install.family_name == store.STORE_FAMILY_NAME
    assert not store.store_subject_problems(store_install)


@pytest.mark.feature("get-the-app")
def test_the_store_installed_app_signs_in_to_the_feed(
        store_install, no_fauna_account_on_this_user, store_nest, store_actor):
    """``get-the-app`` outcome 1, the Microsoft Store arm: installed from the
    store, the app opens and you are signed in to your account.

    It re-asserts its own subject before its outcome, the linux module's lesson
    (``test_linux_installed_product.py``): a narrowed run must not colour this
    cell while driving something else."""
    problems = store.store_subject_problems(store.package_facts() or store_install)
    assert not problems, "; ".join(problems)
    driver = _activated(store_install)
    try:
        _assert_runs_as(driver, store_install)
        _sign_in_through_the_app(driver, store_nest, store_actor)
    finally:
        driver.teardown()


# ---------------------------------------------------------------------------
# Development mode: the same mechanism on the registered layout, never cited
# ---------------------------------------------------------------------------
def test_the_store_subject_check_refuses_a_registered_layout(registered_layout):
    """The layout carries everything a Store delivery's identity carries — name,
    publisher, family name — and the subject check still refuses it, on both of
    the facts a registration cannot fake."""
    assert registered_layout.family_name == store.STORE_FAMILY_NAME, (
        f"the registered layout's family name is {registered_layout.family_name}, not the "
        f"Store's {store.STORE_FAMILY_NAME} — it was packed under another publisher"
    )
    problems = store.store_subject_problems(registered_layout)
    assert any("SignatureKind" in p for p in problems), (
        f"a registered layout passed the signature check: {registered_layout}"
    )
    assert any("IsDevelopmentMode" in p for p in problems), (
        f"a registered layout passed the development-mode check: {registered_layout}"
    )


def test_a_registered_layout_activates_under_its_identity_with_no_test_agent(
        registered_layout, no_fauna_account_on_this_user):
    """The launch shape, end to end on a Release build: the OS activates the
    package's application, the process carries the package's identity, the window's
    UIA tree carries the ui.yaml ids, and no e2e state is published — the
    automation surface is compiled out, exactly as convention 15 says.

    A launch that only reaches onboarding writes nothing to this user's real
    Credential Manager; the node holds itself to that, since it runs where no
    venue for a Store build's rows has been ruled (`sign_in_venue`)."""
    before = _blobs_by_target(_release_rows())
    driver = _activated(registered_layout)
    try:
        _assert_runs_as(driver, registered_layout)
        missing = [eid for eid in _IDENTITY_CHOICE_IDS if not driver.is_visible(eid)]
        assert not missing, (
            f"the Release build's UIA tree lacks the ui.yaml ids {missing}; "
            f"{driver.diagnose(missing[0])}"
        )
        # The bridge answers 204 and no body while no agent has pushed state.
        try:
            published = driver._get("/app/state")
        except Exception:  # noqa: BLE001 — an empty 204 is not JSON
            published = None
        assert driver.app_running(), "the activated app exited while it was being read"
        assert not published, (
            f"the Store build published e2e state {published!r} — an automation surface "
            f"reached a release artifact (convention 15)"
        )
    finally:
        driver.teardown()
    after = _blobs_by_target(_release_rows())
    assert after == before, (
        f"launching the Release build to onboarding changed this user's real Credential "
        f"Manager: gained {sorted(set(after) - set(before))}, lost "
        f"{sorted(set(before) - set(after))}, changed "
        f"{sorted(t for t in set(after) & set(before) if after[t] != before[t])}"
    )


def test_a_registered_layout_signs_in_to_the_feed(
        registered_layout, no_fauna_account_on_this_user, store_nest, store_actor):
    """The witness's journey in development mode: the same activation, identity
    check and sign-in through the UI, on the registered layout. Uncited — a pass
    here proves the mechanism, never the Store install."""
    driver = _activated(registered_layout)
    try:
        _assert_runs_as(driver, registered_layout)
        _sign_in_through_the_app(driver, store_nest, store_actor)
    finally:
        driver.teardown()
