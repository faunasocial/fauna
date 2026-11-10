"""The Windows app as the MICROSOFT STORE delivered it — activated, then driven.

Subject: the package the Store installed for this Windows user — signed by the
Store, under the identity Partner Center reserved (``FaunaSocial.FaunaSocial``),
in the ``store-no-payments`` flavour the Store ships (``installers/windows.md``
§ Store distribution) — not the debug build every other windows suite drives, not
the MSI, and not the same layout registered with ``Add-AppxPackage -Register``.
The ruling, its reasons and what is still open: ``docs/goal/architecture/
packaging-witnesses.md`` § Implementation status today, the bullet on how the
Windows Store install is driven.

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
for the witness. Only the Store-subject nodes are cited —
``test_the_store_installed_app_signs_in_to_the_feed`` on ``get-the-app`` and the
seven ``test_the_store_install_*`` nodes on ``store-install`` — and each skips as
environment unless the Store delivered what is installed.
The layout is this box's own architecture's unless the run passes
``--store-layout-arch``: ``x64`` on an Arm box runs the same nodes on the x64
layout under emulation (``layout_arch``).

**The store flavour's outcomes** (``docs/features/store-install.md``): each
``store-install`` node, and its registered-layout twin, relaunches the activated
app, signs in through its UI only when it opens on onboarding, reaches by clicks
the screen a full build would put a money control on, proves it is on that screen
by an id the flavour keeps, and asserts the excised ids out of the tree; outcome 5
opens the in-person share instead, which this flavour keeps. The buyer halves read
another author's priced tier and sold post, which ``store_seller`` seeds on the
loopback nest — the store build cannot make either.

**The sign-in leg runs in one approved venue.** A Store build names no credential
namespace of its own (the override is compiled out), so signing in writes this
Windows user's production ``fauna-windows`` and ``fauna-sync-agent`` rows in
Credential Manager. Convention 12's Windows arm refuses that for every other launch
and names this module's sign-in as its one exception (``e2e-conventions.md``, the
Windows arm, the Store-witness exception, owner-approved 2026-11-06): a virtual
machine's own Windows user, approved as disposable for Store witnesses.
``sign_in_venue`` holds the run to it: it refuses a machine that does not report
itself a virtual machine, refuses a user who holds any row under a Release build's
namespaces, and deletes every row the run added there afterwards
(``windows_credman.give_back``).

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
def layout_arch(request) -> str:
    """The architecture of the layout the development-mode nodes register: the
    run's `--store-layout-arch`, else this box's own. `x64` on an Arm box is the
    x64 leg, run under emulation."""
    return store.selected_layout_arch(request.config.getoption("--store-layout-arch"))


@pytest.fixture(scope="module")
def registered_layout(desktop_session, layout_arch) -> store.PackageFacts:
    """The store-no-payments layout `windows-store-package` staged under the
    reserved identity for `layout_arch`, registered for this user, removed
    afterwards.

    Read, never built here (convention 9's prebuild bullet): the build is the
    recipe's, and it is a release build of the whole payload."""
    repo = Path(get_repo_root())
    manifest = store.staged_layout_manifest(repo, layout_arch)
    recipe = store.layout_recipe(layout_arch)
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
    existing = store.package_facts(arch=layout_arch)
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
    facts = store.register_layout(manifest, layout_arch)
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
    12's Windows arm lets every other launch reach Credential Manager only under a
    namespace unique to it. Its Store-witness exception (owner-approved
    2026-11-06) names the venue for a Store build's rows: a virtual machine's own
    Windows user, approved as disposable, the way the macOS arm's virtual-machine
    photo library is.
    A machine that does not report itself a virtual machine is refused here
    before anything starts, since a person's own PC is not that venue.

    The venue starts with no row under a release namespace — one that holds any is
    refused, so a run never owes a row back — and after the module every row the
    run added there is deleted (`windows_credman.give_back`); nothing is written
    or restored."""
    virtual, reading = store.this_machine_is_virtual()
    if not virtual:
        skip_environment(
            f"the sign-in leg writes this Windows user's production Credential Manager "
            f"namespace {_PRODUCTION_NAMESPACE!r} (a Store build cannot be given another), "
            f"and convention 12's Windows arm approves that only on a virtual machine's "
            f"own user; this machine reads {reading}, which names no hypervisor"
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


#: The author the buyer halves read, and what they sell.
_SELLER_HANDLE = "storeseller"
_PRICED_TIER = "patrons"
#: Any asking price: a full build paints it as the offer's price, 21 sats in msat.
_ASKING_PRICE = {"value": 21_000, "unit": "msat"}


@pytest.fixture(scope="module")
def store_seller(store_nest) -> dict:
    """Another author on `store_nest` who sells: an ordinary tier carrying every
    money field a full build's tier editor writes (a price hint, an asking price
    and a payment link), and one sold post — its teaser public, its body sealed to
    the post's own unlock tier, named the way the composer names it
    (`fauna_client_subscriptions::orchestration::mint_unlock_tier_name`).

    Seeded through the author's own wire doors in the order a full build's
    composer drives them: the unlock tier designating the post before the post
    exists (`post_id = blake3(body)`), the shared gated-post seal, the sealed
    half's upload, `posts.create` — the sold-post fixture of
    `tests/api/test_feature_gate_refusals.py`. The store build under test cannot
    make either and is only ever their reader. An ordinary post rides along, so
    the profile's Posts tab holds a plain card as well as the gated one.

    Returns the admitted actor plus the sold post's `teaser` and `post_id`."""
    import secrets

    import blake3

    import fauna_ffi
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.admin_wire import admit_adult
    from helpers.blob_upload import upload_blob
    from tests.api import ws_api

    seller = admit_adult(store_nest, _SELLER_HANDLE, domain=store_nest["authority"], token=True)
    secret = bytes(seller["signing_key"])
    unlock_tier = f"post-unlock-{secrets.token_hex(8)}"
    teaser = f"a post for sale {secrets.token_hex(4)}"
    period_key = secrets.token_bytes(32)
    body, sealed = fauna_ffi.build_gated_post(
        secret, teaser, f"{teaser}\n\nthe part you pay for", unlock_tier, 1,
        secrets.token_bytes(32), period_key,
    )
    post_id = blake3.blake3(body).hexdigest()
    with WsRpcAdminClient(
        store_nest["url"], actor_id=bytes(seller["actor_id_bytes"]), signing_key=secret,
    ) as ws:
        priced = ws.call("fauna.subscriptions.tiers.create", {
            "name": _PRICED_TIER, "rank": 1, "auto_approve": False,
            "description": "the supporters' tier",
            "price_hint": "$5 a month",
            "payment_url": "https://example.com/pay/patrons",
            "asking_price": _ASKING_PRICE,
            "encrypted_upload": fauna_ffi.build_tier_birth_upload(
                secret, _PRICED_TIER, secrets.token_bytes(32)),
        })
        assert priced.get("created") is True, f"the seller's priced tier: {priced!r}"
        sold = ws.call("fauna.subscriptions.tiers.create", {
            "name": unlock_tier, "rank": 1, "auto_approve": False,
            "price_hint": "$3",
            "asking_price": _ASKING_PRICE,
            "unlocks_post": post_id,
            "encrypted_upload": fauna_ffi.build_tier_birth_upload(secret, unlock_tier, period_key),
        })
        assert sold.get("created") is True, f"the sold post's unlock tier: {sold!r}"
    upload_blob(store_nest["port"], seller["token"], sealed, audience_class="PeriodRestrictedPost")
    assert ws_api.create_post(store_nest["port"], seller, body) == post_id
    ws_api.create_post(
        store_nest["port"], seller,
        fauna_ffi.build_post(secret, f"an ordinary post {secrets.token_hex(4)}"),
    )
    return {**seller, "teaser": teaser, "post_id": post_id}


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
    _await_the_feed(driver, f"signing in as {actor['handle']!r}")


def _await_the_feed(driver, after: str) -> None:
    """Wait for the main window's ``feed-tab`` after a sign-in or a relaunch.

    The welcome-back path may show a terminal wizard step before the app mounts
    (the network choice, a launch retry); pressing it is the user's own next act.

    A read the app's UI thread does not serve within the bridge's per-call budget
    comes back as a bridge 500 naming a UIA timeout. That is a busy app, not an
    answer: the app signs in on that thread, and on a box whose host is
    oversubscribed every step runs several times slower (measured 2026-11-07 on
    an Arm virtual machine the host ran on about a third of the cores it billed). The
    wait is for arrival and its deadline decides, so such a read is kept for
    the failure message and the poll goes on."""
    deadline = time.monotonic() + 180
    last_read_error = ""
    while time.monotonic() < deadline:  # deadline-ok: the exit condition is arrival
        try:
            if driver.is_visible("feed-tab"):
                return
        except RuntimeError as exc:
            last_read_error = str(exc).splitlines()[0]
        for step in ("nat-mode-confirm-button", "launch-retry-button"):
            with contextlib.suppress(Exception):
                if driver.is_visible(step):
                    driver.click(step)
        time.sleep(1.0)  # sleep-ok: poll interval of an arrival read
    error = ""
    with contextlib.suppress(Exception):
        error = driver.get_text("error-message")
    raise AssertionError(
        f"the app never reached the feed after {after}; "
        f"error-message: {error!r}; last unserved read: {last_read_error!r}; "
        f"{driver.diagnose('feed-tab')}"
    )


# ---------------------------------------------------------------------------
# store-install: the outcomes, driven by clicks alone
# ---------------------------------------------------------------------------
#
# A Release build has no state protocol, so every screen is reached the way a
# person reaches it: a nav tab, a profile tab, a Settings rail row, Find User. The
# ids each outcome asserts absent or reads as kept are the `STORE_ABSENT_*` /
# `STORE_KEPT_IN_PERSON_SHARE` tuples of `helpers/windows_store_install.py`, pinned
# by `test_windows_store_install_pins.py` to the `Views\Payments` / `Views\P2pShare`
# XAML that spells each. Where a full build paints a control only on some data (a
# tipped post, a linked Nostr account) the reading is a lower bound and is printed
# as a `store-install reading:` line; the strict proof of those is the build's own
# scan (`dynamic-features.md` § What "completely compiled away" means).

#: Where an activated install opens: onboarding when no account has signed in on
#: it, else the main window (or a wizard step on the way to it).
_LAUNCH_READY_IDS = (_IDENTITY_CHOICE_ID, "feed-tab", "nat-mode-confirm-button", "launch-retry-button")

#: How long a screen reached by a click may take to show what it was asked for.
#: Nothing in a Release build says "ready", and on an oversubscribed host every
#: read runs several times slower (the 2026-11-07 measurement in
#: `_await_the_feed`), so the wait is for arrival and its deadline decides.
_SCREEN_BUDGET_S = 120


def _open_signed_in(facts: store.PackageFacts, nest: dict, actor: dict):
    """Activate the installed app and reach the feed, signing in through its UI
    when it opens on onboarding — so each outcome node stands alone, in whatever
    order a run takes them. An install that opens signed in holds this run's
    `actor`: `sign_in_venue` admits a run only on a user with no row under a
    Release build's namespaces, so every row there is one this module's sign-in
    wrote."""
    driver = create_driver("windows")
    try:
        driver.launch_activated(
            facts.aumid, app_path=str(facts.executable), ready_id=_LAUNCH_READY_IDS
        )
        _assert_runs_as(driver, facts)
        if driver.is_visible(_IDENTITY_CHOICE_ID):
            _sign_in_through_the_app(driver, nest, actor)
        else:
            _await_the_feed(driver, f"relaunching the install signed in as {actor['handle']!r}")
    except BaseException:
        with contextlib.suppress(Exception):
            driver.teardown()
        raise
    return driver


def _error_text(driver) -> str:
    with contextlib.suppress(Exception):
        return driver.get_text("error-message")
    return ""


def _await_present(driver, element_ids: tuple[str, ...], what: str) -> str:
    """Wait until any of ``element_ids`` is in the window's tree and return it.

    Tree membership (``count``), not the viewport: an anchor below the fold is on
    its screen all the same. A read the UI thread did not serve is kept for the
    failure message and the poll goes on (`_await_the_feed`)."""
    deadline = time.monotonic() + _SCREEN_BUDGET_S
    last_read_error = ""
    while time.monotonic() < deadline:  # deadline-ok: the exit condition is arrival
        for element_id in element_ids:
            try:
                if driver.count(element_id) >= 1:
                    return element_id
            except RuntimeError as exc:
                last_read_error = str(exc).splitlines()[0]
        time.sleep(0.5)  # sleep-ok: poll interval of an arrival read
    raise AssertionError(
        f"{what}: none of {list(element_ids)} appeared within {_SCREEN_BUDGET_S} s; "
        f"error-message: {_error_text(driver)!r}; last unserved read: {last_read_error!r}; "
        f"{driver.diagnose(element_ids[0])}"
    )


def _served_count(driver, element_id: str) -> int:
    """``count(element_id)``, waiting out reads the UI thread did not serve: an
    absence is only ever asserted from an answer."""
    deadline = time.monotonic() + _SCREEN_BUDGET_S
    last_read_error = ""
    while time.monotonic() < deadline:  # deadline-ok: the exit condition is a served read
        try:
            return driver.count(element_id)
        except RuntimeError as exc:
            last_read_error = str(exc).splitlines()[0]
        time.sleep(0.5)  # sleep-ok: poll interval of a state read
    raise AssertionError(
        f"the app never answered a read of {element_id!r} within {_SCREEN_BUDGET_S} s: "
        f"{last_read_error}"
    )


def _assert_absent(driver, element_ids: tuple[str, ...], on: str) -> None:
    """None of ``element_ids`` is in the tree — read only after the screen's own
    anchor, so an empty read is the screen's answer, not a page still painting."""
    present = [eid for eid in element_ids if _served_count(driver, eid)]
    assert not present, (
        f"the store install paints {present} on {on} — the store-no-payments excision "
        f"did not hold; {driver.diagnose(present[0])}"
    )
    print(f"store-install absent on {on}: {', '.join(element_ids)}")


def _open_tab(driver, tab_id: str, anchors: tuple[str, ...], what: str) -> None:
    """A main-window nav tab, by its ``{page}-tab`` (convention 3)."""
    _await_present(driver, (tab_id,), f"the {tab_id} nav item")
    driver.click(tab_id)
    _await_present(driver, anchors, what)


def _open_settings_page(driver, key: str, anchors: tuple[str, ...]) -> None:
    """A Settings sub-page by its rail row, from the main window: ``settings-tab``
    through the shared click door (`helpers/settings_tab_click.py`), then the row.
    Each wait is on the next thing clicked or read, never on the shell's
    ``settings-nav-back``, which sits in the rail's PaneHeader — a slot WinUI does
    not reliably put in the UIA tree."""
    from helpers.settings_tab_click import click_settings_tab

    row = f"settings-nav-row[{key}]"
    _await_present(driver, ("settings-tab",), "the settings-tab nav item")
    click_settings_tab(driver, timeout=_SCREEN_BUDGET_S)
    _await_present(driver, (row,), f"the Settings rail's {key} row")
    driver.click(row)
    _await_present(driver, anchors, f"Settings → {key}")


def _open_own_tiers(driver) -> None:
    """The own profile's Tiers tab, its §1 list and create button on screen."""
    _open_tab(driver, "profile-tab", ("profile-tiers-tab",), "the own profile")
    driver.click("profile-tiers-tab")
    _await_present(driver, ("subscription-tiers-section",), "the own profile's Tiers tab")
    _await_present(driver, ("subscription-tier-create-button",), "the own profile's Tiers tab")


def _open_seller_profile(driver, seller: dict, nest: dict) -> None:
    """The seller's profile, opened as a reader opens anyone's: Contacts → Find
    User by handle → Open profile (`contacts.md` § Layout & flow, region 3)."""
    handle = f"{seller['handle']}@{nest['authority']}"
    _open_tab(driver, "contacts-tab", ("contact-actor-id-field",), "Contacts")
    driver.clear_and_type("contact-actor-id-field", handle)
    driver.click("contact-actor-id-lookup")
    _await_present(driver, ("contact-actor-id-result",), f"Find User resolving {handle}")
    driver.click("contact-actor-id-open-profile-button")
    _await_present(driver, ("profile-posts-tab",), f"{handle}'s profile")


def _outcome_no_provider_or_code(driver, **_) -> None:
    """Outcome 1: no payment provider to connect and no code to hand out (the
    own profile's Tiers tab), none to redeem (Settings → Subscriptions)."""
    _open_own_tiers(driver)
    _assert_absent(driver, store.STORE_ABSENT_PROVIDER_AND_MINT, "the own profile's Tiers tab")
    _open_settings_page(driver, "subscription-settings", ("subscription-mine-section",))
    _assert_absent(driver, store.STORE_ABSENT_REDEEM, "Settings → Subscriptions")


def _outcome_tier_editor_has_no_price(driver, **_) -> None:
    """Outcome 2, the author half: the tier editor has no price, price hint or
    payment link. The form is opened and cancelled; nothing is saved."""
    _open_own_tiers(driver)
    driver.click("subscription-tier-create-button")
    _await_present(driver, ("subscription-tier-form-save",), "the tier editor")
    _assert_absent(driver, store.STORE_ABSENT_TIER_MONEY, "the tier editor")
    driver.click("subscription-tier-form-cancel")


def _outcome_never_offers_a_sale(driver, **_) -> None:
    """Outcome 3, the author half: the compose audience picker offers no
    "Sell this post…" answer and no sale control is painted. The picker's options
    are read, never chosen; nothing is posted."""
    from i18n.strings import S

    _open_tab(driver, "feed-tab", ("compose-gate-tier-select",), "the feed's compose bar")
    _await_present(driver, ("compose-text-field",), "the feed's compose bar")
    _assert_absent(driver, store.STORE_ABSENT_SELL, "compose")
    options = driver.option_texts("compose-gate-tier-select")
    assert options is not None, (
        f"the bridge read no option list off compose-gate-tier-select; "
        f"{driver.diagnose('compose-gate-tier-select')}"
    )
    assert S.feed.post.gate_public in options, (
        f"the audience picker offers {options}, without its {S.feed.post.gate_public!r} answer — "
        f"the read is not of the picker a full build adds the sale to"
    )
    assert S.feed.post.gate_sell not in options, (
        f"the store install's audience picker offers {S.feed.post.gate_sell!r} ({options}) — "
        f"the sale is not excised"
    )
    print(f"store-install absent on the audience picker: {S.feed.post.gate_sell} (offered: {options})")


def _outcome_priced_tier_without_price(driver, *, seller: dict, nest: dict, **_) -> None:
    """Outcome 2, the buyer half: the seller's priced tier is listed on their Tiers
    tab as an ordinary tier, with no price or payment link. Nothing is subscribed
    to."""
    _open_seller_profile(driver, seller, nest)
    driver.click("profile-tiers-tab")
    _await_present(
        driver, ("subscription-offer-row",),
        f"{seller['handle']}'s Tiers tab listing the seeded priced tier",
    )
    _assert_absent(driver, store.STORE_ABSENT_OFFER_MONEY, f"{seller['handle']}'s Tiers tab")


def _outcome_sold_post_without_price(driver, *, seller: dict, nest: dict, **_) -> None:
    """Outcome 3, the buyer half: the seller's sold post is painted on their Posts
    tab as a gated post with its badge, and no price, payment link or buy button
    beside it. Nothing is bought."""
    _open_seller_profile(driver, seller, nest)
    driver.click("profile-posts-tab")
    _await_present(
        driver, ("gated-post-badge",), f"{seller['handle']}'s Posts tab painting the seeded sold post",
    )
    _assert_absent(driver, store.STORE_ABSENT_SOLD_POST_MONEY, f"{seller['handle']}'s Posts tab")


def _outcome_no_tips_or_zap_signer(driver, *, seller: dict, nest: dict, **_) -> None:
    """Outcome 4: a post card paints no tip surface, and Settings → Nostr offers no
    zap signer. The cards read are the seller's (a full build paints the tip
    surface only on a tipped post, and none is tipped), and a full build paints the
    zap-signer section only for a linked account: both readings are lower bounds."""
    _open_seller_profile(driver, seller, nest)
    _await_present(driver, ("post-card",), f"{seller['handle']}'s Posts tab")
    cards = _served_count(driver, "post-card")
    print(f"store-install reading: {cards} post card(s), none tipped")
    _assert_absent(driver, store.STORE_ABSENT_TIPS, f"{seller['handle']}'s post cards")
    _open_settings_page(driver, "nostr", ("nostr-link-mode", "nostr-unlink-button"))
    linked = _served_count(driver, "nostr-unlink-button") > 0
    print(f"store-install reading: nostr {'linked' if linked else 'unlinked'}")
    _assert_absent(driver, store.STORE_ABSENT_ZAP_SIGNER, "Settings → Nostr")


def _outcome_keeps_the_in_person_share(driver, *, actor: dict, **_) -> None:
    """Outcome 5, the Microsoft Store half: sharing a folder in person stays.
    Settings → Folders paints both entry buttons, and opening the share panel
    shows this device's compare code, leading with the account's key — the panel
    binds its ceremony listener as it does for anyone who opens it. Closed again;
    nothing is shared."""
    _open_settings_page(driver, "folders", ("folder-add-button",))
    for element_id in ("offline-share-button", "offline-receive-button"):
        _await_present(driver, (element_id,), "Settings → Folders' in-person share")
    driver.click("offline-share-button")
    _await_present(driver, ("offline-share-own-code",), "the in-person share panel")
    want = actor["actor_id_hex"]
    deadline = time.monotonic() + _SCREEN_BUDGET_S
    shown = ""
    while time.monotonic() < deadline:  # deadline-ok: waits for the bound seat's code
        with contextlib.suppress(RuntimeError):
            shown = driver.get_text("offline-share-own-code").strip()
        if shown == want or shown.startswith(want + "-"):
            break
        time.sleep(0.5)  # sleep-ok: poll interval of a state read
    assert shown == want or shown.startswith(want + "-"), (
        f"offline-share-own-code must lead with this account's key {want!r}, got {shown!r}; "
        f"error-message: {_error_text(driver)!r}"
    )
    print(f"store-install kept on Settings → Folders: {', '.join(store.STORE_KEPT_IN_PERSON_SHARE)}")
    driver.click("offline-share-cancel-button")


def _run_outcome(outcome, facts: store.PackageFacts, nest: dict, actor: dict, **extra) -> None:
    driver = _open_signed_in(facts, nest, actor)
    try:
        outcome(driver, actor=actor, nest=nest, **extra)
    finally:
        driver.teardown()


def _run_store_outcome(outcome, store_install, nest: dict, actor: dict, **extra) -> None:
    """Re-assert the Store subject from the OS, then run one outcome on it — a
    narrowed run must not colour a cell while driving something else (the linux
    module's lesson, ``test_linux_installed_product.py``)."""
    problems = store.store_subject_problems(store.package_facts() or store_install)
    assert not problems, "; ".join(problems)
    _run_outcome(outcome, store_install, nest, actor, **extra)


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


@pytest.mark.feature("store-install")
def test_the_store_install_offers_no_payment_provider_or_code(store_install, store_nest, store_actor):
    """``store-install`` outcome 1, the Microsoft Store arm: no payment provider
    to connect and no code to hand out (own profile → Tiers), none to redeem
    (Settings → Subscriptions)."""
    _run_store_outcome(_outcome_no_provider_or_code, store_install, store_nest, store_actor)


@pytest.mark.feature("store-install")
def test_the_store_install_tier_editor_has_no_price(store_install, store_nest, store_actor):
    """``store-install`` outcome 2, the author half: the tier editor has no
    price, price hint or payment link (opened and cancelled, nothing saved)."""
    _run_store_outcome(_outcome_tier_editor_has_no_price, store_install, store_nest, store_actor)


@pytest.mark.feature("store-install")
def test_the_store_install_never_offers_to_sell_a_post(store_install, store_nest, store_actor):
    """``store-install`` outcome 3, the author half: the compose audience picker
    has no "Sell this post…" answer and no sale control is painted (nothing
    posted)."""
    _run_store_outcome(_outcome_never_offers_a_sale, store_install, store_nest, store_actor)


@pytest.mark.feature("store-install")
def test_the_store_install_shows_a_priced_tier_without_its_price(
        store_install, store_nest, store_actor, store_seller):
    """``store-install`` outcome 2, the buyer half: another author's priced tier
    shows on their Tiers tab as an ordinary tier, with no price or payment link
    (nothing subscribed to)."""
    _run_store_outcome(_outcome_priced_tier_without_price, store_install, store_nest, store_actor,
                       seller=store_seller)


@pytest.mark.feature("store-install")
def test_the_store_install_shows_a_sold_post_without_its_price(
        store_install, store_nest, store_actor, store_seller):
    """``store-install`` outcome 3, the buyer half: another author's sold post
    shows on their Posts tab as a gated post with its badge, and no price, payment
    link or buy button (nothing bought)."""
    _run_store_outcome(_outcome_sold_post_without_price, store_install, store_nest, store_actor,
                       seller=store_seller)


@pytest.mark.feature("store-install")
def test_the_store_install_shows_no_tips_or_zap_signer(
        store_install, store_nest, store_actor, store_seller):
    """``store-install`` outcome 4: no tip surface on a post card, no zap signer on
    Settings → Nostr."""
    _run_store_outcome(_outcome_no_tips_or_zap_signer, store_install, store_nest, store_actor,
                       seller=store_seller)


@pytest.mark.feature("store-install")
def test_the_store_install_keeps_the_in_person_share(store_install, store_nest, store_actor):
    """``store-install`` outcome 5, the Microsoft Store half: Settings → Folders
    offers sharing a folder in person, and its panel opens on this device's
    compare code."""
    _run_store_outcome(_outcome_keeps_the_in_person_share, store_install, store_nest, store_actor)


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
    Credential Manager; the node holds itself to that, since it takes no venue
    (`sign_in_venue` is the sign-in nodes' alone)."""
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


# The store-install outcomes in development mode: each `test_the_store_install_*`
# node's journey on the registered store-no-payments layout, the same flavour the
# Store delivers. Uncited — a pass proves the mechanism and colours nothing.
def test_a_registered_layout_offers_no_payment_provider_or_code(
        registered_layout, store_nest, store_actor):
    _run_outcome(_outcome_no_provider_or_code, registered_layout, store_nest, store_actor)


def test_a_registered_layout_tier_editor_has_no_price(registered_layout, store_nest, store_actor):
    _run_outcome(_outcome_tier_editor_has_no_price, registered_layout, store_nest, store_actor)


def test_a_registered_layout_never_offers_to_sell_a_post(registered_layout, store_nest, store_actor):
    _run_outcome(_outcome_never_offers_a_sale, registered_layout, store_nest, store_actor)


def test_a_registered_layout_shows_a_priced_tier_without_its_price(
        registered_layout, store_nest, store_actor, store_seller):
    _run_outcome(_outcome_priced_tier_without_price, registered_layout, store_nest, store_actor,
                 seller=store_seller)


def test_a_registered_layout_shows_a_sold_post_without_its_price(
        registered_layout, store_nest, store_actor, store_seller):
    _run_outcome(_outcome_sold_post_without_price, registered_layout, store_nest, store_actor,
                 seller=store_seller)


def test_a_registered_layout_shows_no_tips_or_zap_signer(
        registered_layout, store_nest, store_actor, store_seller):
    _run_outcome(_outcome_no_tips_or_zap_signer, registered_layout, store_nest, store_actor,
                 seller=store_seller)


def test_a_registered_layout_keeps_the_in_person_share(registered_layout, store_nest, store_actor):
    _run_outcome(_outcome_keeps_the_in_person_share, registered_layout, store_nest, store_actor)
