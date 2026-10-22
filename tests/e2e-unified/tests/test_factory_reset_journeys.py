"""tier_3 e2e: what the app does around a REAL factory reset — the page-driven
reset and re-claim, the identity that survives it, the refusal when the claim
code cannot be kept, a reopen while the box is down, two accounts' resets on
one device, and a member whose nest forgot them.

``docs/goal/architecture/nest/common.md`` § Factory reset and § Client-state
recoverability own every behaviour asserted here; the outcomes are the
``factory-reset`` feature page's (``docs/features/factory-reset.md``) 1, 5, 8,
9, 11 and 12.

**Why a module of its own.** Each journey needs a box it may wipe, so each runs
on a DEDICATED nest started by the run's mode provider (never the session
``nest_instance`` — a reset would wipe it under every later test), and each
drives its own app launch through ``common.launch_harness`` (or a seeded
driver, for the two-account case): a reset tears the session down, and the
journeys then reopen the app the way a person does — a fresh process reading
the same client store back (``preserve_state_across_relaunch()``).

**What is the app's act and what is arrangement.** Every reset, claim and
switch under test goes through the app UI (``testing.md`` § point 8). Two
things are the harness's, as everywhere a nest restarts: it plays the
supervisor that brings a self-exited nest back into the wipe
(``common.nest.resume_after_self_exit``), and it takes a nest down and up again
to make it unreachable (``stop_nest`` / ``start_nest_in_place``). The member
case's admin is a second seat — a second app process driven through its own UI
— not an API call standing in for it.

**The trust seed is read once.** ``NativeLaunchHarness.relaunch()`` re-reads the
nest's identity into the escrow-trust seed on every launch; the relaunches here
use ``relaunch_as_launched()`` instead, so the reconnect after a reset is made
with what the app knew BEFORE it, and a nest that is down needs no read at all.

tui leads (the lead app); another app joins by adding itself to ``_CLIENTS``
(and its platform mark to ``pytestmark``, which ``--app`` deselection reads)
once its run is green.

**One harness shape for every app** (``_seat``): a native app launches over the
file-backed client store — the pending-factory-reset slot lives on the shared
``AccountRegistry``, which honours ``FAUNA_E2E_CREDENTIAL_DIR`` independently
of an unlocked keyring, so ``_slot_code`` reads it at rest on tui and linux
alike; web launches through ``WebLaunchHarness`` behind the nest's own SPA
proxy (a browser reaches a nest only through one), and its store is the
origin's ``localStorage``, which ``helpers.registry_store`` reads the same way.
"""

from __future__ import annotations

import contextlib
import json
import secrets

import pytest

from actions import ActionLayer
from common.launch_harness import (
    in_authenticated_app,
    make_launch_harness,
    reached_authenticated_app,
)
from common.nest import (
    _wait_nest_fresh,
    resume_after_self_exit,
    start_nest_in_place,
    stop_nest,
)
from conftest import (
    _seeded_environment,
    _serve_spa_proxy,
    _start_dedicated_nest,
    _trust_seeder,
    get_available_apps,
)
from helpers.app_launch import app_launch_config
from helpers.crash_recovery import launch_surface_dump, setup_status
from helpers.registry_store import describe_registry, read_store_slot
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3, pytest.mark.tui, pytest.mark.linux, pytest.mark.web,
    pytest.mark.destructive,
]

_CLIENTS = ("tui", "linux", "web")

# tests/e2e-unified/ui.yaml — the claim page, the launch surfaces, the shell.
CLAIM_CODE_INPUT = "claim-code-input"
CLAIM_SUBMIT = "claim-code-submit-button"
IDENTITY_WARNING = "nest-identity-changed-warning"
IDENTITY_TRUST_BUTTON = "nest-identity-changed-trust-button"
SIGN_IN_REFUSED_NOTICE = "launch-sign-in-refused-notice"
LAUNCH_RETRY = "launch-retry-button"
LAUNCH_FALLTHROUGH = "launch-fallthrough-button"
HANDLE_INPUT = "handle-input"
RESET_BUTTON = "admin-factory-reset-button"
RESET_CONFIRM = "admin-factory-reset-confirm-button"
ACCOUNT_SWITCHER_LIST = "account-switcher-list"
ACCOUNT_SWITCHER_ITEM = "account-switcher-item"
REAUTH_PROMPT = "account-activate-reauth-prompt"
REAUTH_CONFIRM = "account-activate-reauth-confirm-button"

#: A launch into the shell, cold (an account runtime assembling).
_APP_S = 90.0
#: A nest restarting into its wipe and answering fresh.
_NEST_FRESH_S = 120.0
#: A claim landing on the box.
_CLAIM_S = 60.0
#: A launch routing onto a wizard page or a launch surface.
_ROUTE_S = 60.0
#: A running app's reconnect meeting the wiped box: the WS drop, the
#: supervisor's backoff (1 s doubling, 60 s cap), the refused re-mint.
_RECONNECT_REFUSED_S = 150.0
#: The TOFU pin write after the shell renders (a separate async step).
_PIN_S = 30.0


def _clients():
    available = get_available_apps()
    return [c for c in _CLIENTS if c in available]


@pytest.fixture(params=_clients())
def reset_client(request):
    """The app under test; its id lands in the test name, which conftest's
    ``--app`` filter reads."""
    return request.param


@contextlib.contextmanager
def _seat(request, client, tmp_path, nest):
    """A launch harness for ``client`` on ``nest``, and the url that app dials
    (and pins the nest's identity by): the nest's own on native, the SPA
    proxy's on web — a browser reaches a nest only through one
    (``conftest._serve_spa_proxy``). Native launches are file-backed, so the
    pending-factory-reset slot is readable at rest (module docstring)."""
    server = None
    if client == "web":
        node_url, server = _serve_spa_proxy(request.getfixturevalue("static_dir"), nest["url"])
        harness = make_launch_harness("web", spa_url=node_url)
    else:
        harness = make_launch_harness(
            client, tmp_path=tmp_path, file_backed=True,
            **app_launch_config(client, request), seed_trust=_trust_seeder(request),
        )
        node_url = nest["url"]
    try:
        yield harness, node_url
    finally:
        harness.teardown()
        if server is not None:
            server.shutdown()


@pytest.fixture
def reset_nest(request, nest_mode, tmp_path_factory):
    """A claimed nest this test may wipe (plain HTTP — the journeys that need
    the TOFU pin take ``reset_tls_nest``)."""
    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "reset-journey-nest")
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture
def reset_tls_nest(request, nest_mode, tmp_path_factory):
    """A claimed nest this test may wipe, serving its self-signed cert over
    real HTTPS: a native app TOFU-pins a nest's identity only over TLS
    (``security.md`` § Transport trust), so only this nest can show whether a
    reset kept the identity a pinned app trusts."""
    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "reset-journey-tls", serve_tls=True)
    assert nest["url"].startswith("https://"), nest["url"]
    try:
        yield nest
    finally:
        cleanup()


def _admin_hex(nest) -> tuple[str, str]:
    """(actor id, secret) of the nest's claiming admin, both hex."""
    sk = nest["admin"]["signing_key"]
    return bytes(sk.verify_key).hex(), bytes(sk).hex()


def _slot_key(actor_hex: str) -> str:
    return f"fauna/{actor_hex}/pending_factory_reset"


def _slot_code(driver, actor_hex: str) -> str | None:
    """The claim code in ``actor_hex``'s pending-factory-reset slot, read from
    the client store at rest (None when there is no slot)."""
    raw = read_store_slot(driver, _slot_key(actor_hex))
    if raw is None:
        return None
    return json.loads(raw).get("claim_code")


def _read_pin(driver, nest_url):
    """The TOFU pin the app's installed store holds for ``nest_url``."""
    raw = driver.call_machine_method(
        "nest_identity_pin_for_test", json.dumps({"nest_url": nest_url})
    )
    if raw in (None, "", "null"):
        return None
    if not isinstance(raw, str):
        return raw
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return raw


def _auth_log_lines(driver, limit: int = 25) -> list[str]:
    """The app's own log lines about its connection and sign-in — what a
    reconnect decided reaches no element, so a failure quotes them."""
    keep = ("supervisor", "refresh", "auth", "sign", "reconnect", "not_registered")
    lines = [
        line for line in (driver.app_stderr_text() or "").splitlines()
        if any(k in line.lower() for k in keep)
    ]
    return lines[-limit:]


def _nest_id(nest_url) -> str:
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(nest_url) as anon:
        raw = anon.call("fauna.nest.info", {})["nest_id"]
    return bytes(raw).hex() if isinstance(raw, (bytes, bytearray, list)) else raw


def _wait_claimed(nest, app, what: str) -> None:
    wait_until(
        lambda: (lambda st: st.get("claimed") and st.get("admin_exists"))(
            setup_status(nest["url"])),
        _CLAIM_S,
        diagnose=lambda: f"{what}: box reads {setup_status(nest['url'])!r}; "
        f"app error={app.error_text()!r}",
    )


def _await_prefilled_claim(app, want_code: str, what: str) -> None:
    """The launch routed onto the claim page with ``want_code`` already in it."""
    try:
        app.driver.wait_for(CLAIM_CODE_INPUT, timeout=_ROUTE_S)
    except Exception as exc:
        raise AssertionError(
            f"{what}: the app never opened the claim page; "
            f"error={app.error_text()!r} launch surface: {launch_surface_dump(app)}"
        ) from exc
    shown = app.driver.get_text(CLAIM_CODE_INPUT)
    assert shown == want_code, (
        f"{what}: the claim page must carry the code minted before the reset — "
        f"shows {shown!r}, minted {want_code!r}; error={app.error_text()!r}"
    )


def _claim_through_the_wizard(app, nest, what: str) -> None:
    """Submit the pre-filled code and finish the wizard into the app — the claim
    itself, then the steps a claim lands on (the NAT-mode choice and the trust
    offer, both answered with their defaults), up to the signed-in terminal. The
    terminal is what spends the pending-reset slot (`nest/common.md` § Client-
    state recoverability: "cleared at the claim terminal"), so a journey that
    stops at the claim leaves it behind."""
    app.driver.click(CLAIM_SUBMIT)
    _wait_claimed(nest, app, what)

    def signed_in() -> bool:
        if in_authenticated_app(app.driver):
            return True
        with contextlib.suppress(Exception):
            app.onboarding.finish_nat_mode()
        with contextlib.suppress(Exception):
            app.onboarding.finish_trust_prompt()
        return False

    wait_until(
        signed_in,
        _APP_S,
        diagnose=lambda: f"{what}: the wizard never reached the app; "
        f"error={app.error_text()!r} launch surface: {launch_surface_dump(app)}",
    )


# ---------------------------------------------------------------------------
# Outcomes 1 and 5 — reset from the page, re-claim, and the pinned app comes
# back with no identity-changed warning
# ---------------------------------------------------------------------------

@pytest.mark.feature("factory-reset")
def test_a_reset_from_the_page_reclaims_and_the_pinned_app_reconnects_unwarned(
    request, reset_client, reset_tls_nest, tmp_path
):
    """Outcome 1: the admin resets the nest from the app; the app opens the
    claim step with the nest's new claim code filled in, and submitting it
    claims the nest again. Outcome 5: the nest kept its identity through the
    reset, so the app — which TOFU-pinned that identity before it — opens
    straight into the shell on its next launch, with no identity-changed
    warning, and its pin still names the same identity.

    Non-vacuous on three counts: the pin is read back as the nest's real
    identity BEFORE the reset (a pin-less app would accept any identity); the
    nest's served identity is read again AFTER the reset and must be the same
    one; and the relaunch does not re-seed trust from the nest
    (``relaunch_as_launched``), so nothing the harness does can paper over a
    rotated identity. ``test_nest_identity_pin.py`` proves the same launch path
    DOES warn when the pin disagrees."""
    nest = reset_tls_nest
    url = nest["url"]
    _actor, secret_hex = _admin_hex(nest)
    with _seat(request, reset_client, tmp_path, nest) as (harness, node_url):
        driver = harness.launch_and_route(secret_hex=secret_hex, node_url=node_url, trust=nest)
        reached_authenticated_app(driver, timeout=_APP_S)
        assert driver.preserve_state_across_relaunch(), (
            "the client store must survive the relaunch for the pin to be read back"
        )
        app = ActionLayer(driver)
        before = _nest_id(url)
        wait_until(
            lambda: _read_pin(driver, node_url) == before,
            _PIN_S,
            diagnose=lambda: f"pin={_read_pin(driver, node_url)!r}, nest serves {before!r}",
        )

        # Outcome 1 — the page's reset lands the claim step with the code in it.
        code = app.admin.factory_reset_via_ui()
        resume_after_self_exit(nest)
        _wait_nest_fresh(url, timeout=_NEST_FRESH_S)
        with open(f"{nest['tmp_dir']}/claim-code") as f:
            booted = f.read().strip()
        assert code == booted, (
            f"the pre-filled code must be the one the wiped box booted with: "
            f"prefilled {code!r}, booted {booted!r}"
        )
        _claim_through_the_wizard(
            app, nest, "submitting the pre-filled code re-claims the box")

        after = _nest_id(url)
        assert after == before, (
            f"a reset must keep the nest's identity: served {before!r} before, "
            f"{after!r} after"
        )

        # Outcome 5 — reopen the app; the pin it held is consulted, and agrees.
        harness.relaunch_as_launched()
        app = ActionLayer(harness.driver)
        try:
            reached_authenticated_app(harness.driver, timeout=_APP_S)
        except AssertionError as exc:
            raise AssertionError(
                "the reopened app never reached the shell after the re-claim; "
                f"identity warning shown={harness.driver.count(IDENTITY_WARNING) > 0}, "
                f"error={app.error_text()!r}, launch surface: {launch_surface_dump(app)}"
            ) from exc
        assert harness.driver.is_absent(IDENTITY_WARNING), (
            "an app that trusted the nest before its reset must reconnect without "
            "the identity-changed warning"
        )
        assert harness.driver.is_absent(IDENTITY_TRUST_BUTTON)
        assert _read_pin(harness.driver, node_url) == before, (
            "the pin must still name the identity it held before the reset — "
            f"reads {_read_pin(harness.driver, node_url)!r}"
        )


# ---------------------------------------------------------------------------
# Outcome 9 — the claim code cannot be kept, so the app refuses to reset
# ---------------------------------------------------------------------------

@pytest.mark.feature("factory-reset")
def test_a_reset_whose_claim_code_cannot_be_kept_is_refused(
    request, reset_client, reset_nest, tmp_path
):
    """Outcome 9: when the app cannot save the new claim code first, it refuses
    to reset and says so — the box stays claimed and the admin stays in the
    app (``nest/common.md`` § Client-state recoverability, *A store write that
    silently fails is treated as a failure to reset*).

    The fault is the registry bridge's ``refuse_secret_writes_for_test``: the
    app's own store takes the pending-factory-reset write and keeps nothing,
    the locked-keyring shape no user gesture produces
    (``onboarding.md`` § E2E bridge contract). Everything else is the admin's
    own two clicks."""
    nest = reset_nest
    actor, secret_hex = _admin_hex(nest)
    with _seat(request, reset_client, tmp_path, nest) as (harness, node_url):
        driver = harness.launch_and_route(secret_hex=secret_hex, node_url=node_url, trust=nest)
        reached_authenticated_app(driver, timeout=_APP_S)
        app = ActionLayer(driver)

        app.admin.navigate_nest()
        driver.wait_until_enabled(RESET_BUTTON, timeout=15.0)
        driver.call_machine_method(
            "refuse_secret_writes_for_test", json.dumps({"refuse": True}))
        try:
            driver.click(RESET_BUTTON)
            driver.wait_for(RESET_CONFIRM, timeout=10.0)
            driver.click(RESET_CONFIRM)
            refusal = S.admin.settings_page.factory_reset_persist_failed
            wait_until(
                lambda: refusal in (app.error_text() or ""),
                _ROUTE_S,
                diagnose=lambda: f"error surface reads {app.error_text()!r}; "
                f"claim page shown={driver.count(CLAIM_CODE_INPUT) > 0}",
            )
        finally:
            driver.call_machine_method(
                "refuse_secret_writes_for_test", json.dumps({"refuse": False}))

        st = setup_status(nest["url"])
        assert st.get("claimed") and st.get("admin_exists"), (
            f"a refused reset must never reach the box — it reads {st!r}"
        )
        assert driver.count(CLAIM_CODE_INPUT) == 0, (
            "a refused reset must leave the admin in the app, not on a claim page"
        )
        assert _slot_code(driver, actor) is None, (
            "a refused reset must leave no pending-reset slot behind: "
            f"{describe_registry(driver)}"
        )


# ---------------------------------------------------------------------------
# Outcome 11 — reopening while the box is unreachable keeps the re-claim
# ---------------------------------------------------------------------------

@pytest.mark.feature("factory-reset")
def test_reopening_while_the_reset_nest_is_down_keeps_the_reclaim(
    request, reset_client, reset_nest, tmp_path
):
    """Outcome 11: the app reopens mid-reset while the nest cannot be reached.
    An unreachable box says nothing about whether the reset landed, so the
    pending re-claim is kept — the claim page opens with the code in it, the
    slot is still in the store — and the NEXT launch, with the box back,
    resumes the claim from it and completes (the reconcile's ``Unreachable``
    arm, ``nest/common.md`` § Client-state recoverability; unit-pinned by
    ``pending_factory_reset_reconcile.rs``)."""
    nest = reset_nest
    actor, secret_hex = _admin_hex(nest)
    with _seat(request, reset_client, tmp_path, nest) as (harness, node_url):
        driver = harness.launch_and_route(secret_hex=secret_hex, node_url=node_url, trust=nest)
        reached_authenticated_app(driver, timeout=_APP_S)
        assert driver.preserve_state_across_relaunch()
        code = ActionLayer(driver).admin.factory_reset_via_ui()
        resume_after_self_exit(nest)
        _wait_nest_fresh(nest["url"], timeout=_NEST_FRESH_S)
        stop_nest(nest)

        # Reopen with the box down: the slot is kept and the claim resumes.
        harness.relaunch_as_launched()
        app = ActionLayer(harness.driver)
        _await_prefilled_claim(app, code, "reopening with the nest down")
        assert _slot_code(harness.driver, actor) == code, (
            "an unreachable nest must never clear the pending re-claim: "
            f"{describe_registry(harness.driver)}"
        )

        # The box returns; the next launch resumes from the kept slot.
        start_nest_in_place(nest)
        harness.relaunch_as_launched()
        app = ActionLayer(harness.driver)
        _await_prefilled_claim(app, code, "the next launch with the nest back")
        _claim_through_the_wizard(app, nest, "the kept code re-claims the box")
        wait_until(
            lambda: _slot_code(harness.driver, actor) is None,
            _CLAIM_S,
            diagnose=lambda: f"slot never cleared: {describe_registry(harness.driver)}",
        )


# ---------------------------------------------------------------------------
# Outcome 12 — two accounts on one device, each with its own pending reset
# ---------------------------------------------------------------------------

#: web's per-tab account pin (``apps/fauna-web/src/lib/tabPin.ts``) — the
#: browser twin of ``FAUNA_BOUND_ACCOUNT`` (``account-scoping.md`` § Concurrent
#: instances → *Web*).
_TAB_PIN_KEY = "fauna_tab_account"


class _NativeAccountSeats:
    """The native shape of "one device, two accounts": one app process, reopened
    bound to an account — ``FAUNA_BOUND_ACCOUNT`` on the launch, the command
    Settings copies for "open as new instance" — or as the primary instance.
    Each account dials its own nest directly."""

    def __init__(self, request, client, nest_a, nest_b):
        from drivers import create_driver

        self.urls = (nest_a["url"], nest_b["url"])
        self._environment = _seeded_environment(request, nest_a, nest_b)
        self._config = {
            "app_path": app_launch_config(client, request)["app_path"],
            "url": nest_a["url"],
            "environment": self._environment,
        }
        self.driver = create_driver(client)

    # Not `start`: `helpers/kind_reach.py` merges public names across the tree,
    # so a `start` here would hand this relaunch's reach to every `.start()`.
    def launch_with_seed(self, seed: dict[str, str]):
        self._config["seed_credentials"] = seed
        self.driver.launch(self._config)
        assert self.driver.preserve_state_across_relaunch()
        # The seed was the install's starting state; every later launch reads
        # back what the app itself wrote since.
        self._config.pop("seed_credentials")
        return self.driver

    def _reopen(self, actor: str | None):
        self.driver.teardown()
        if actor is None:
            self._environment.pop("FAUNA_BOUND_ACCOUNT", None)
        else:
            self._environment["FAUNA_BOUND_ACCOUNT"] = actor
        self.driver.launch(self._config)
        return self.driver

    def open_bound(self, actor: str):
        return self._reopen(actor)

    def describe(self, driver) -> str:
        return "app log:\n" + "\n".join(_auth_log_lines(driver))

    def open_primary(self):
        return self._reopen(None)

    def teardown(self) -> None:
        with contextlib.suppress(Exception):
            self.driver.teardown()


class _WebAccountSeats:
    """web's shape of "one device, two accounts": one browser profile, whose
    tabs share the origin's store. The primary instance is the first tab; a
    second instance bound to an account is a tab of the same profile pinned to
    it (``_TAB_PIN_KEY``). A browser reaches a nest only through an SPA proxy
    (``conftest._serve_spa_proxy``), so each nest gets its own: the page is
    served by A's, and B's account dials B's across origins, as a stored nest
    URL does."""

    def __init__(self, request, nest_a, nest_b):
        static_dir = request.getfixturevalue("static_dir")
        self._servers = []
        urls = []
        try:
            for nest in (nest_a, nest_b):
                url, server = _serve_spa_proxy(static_dir, nest["url"])
                self._servers.append(server)
                urls.append(url)
        except Exception:
            self._shutdown_servers()
            raise
        self.urls = tuple(urls)
        self._harness = make_launch_harness("web", spa_url=urls[0])
        self._bound = None

    def launch_with_seed(self, seed: dict[str, str]):
        # `launch()` seeds the store on the empty-store surface; the reload is
        # the first launch over it, as a native launch reads its seeded store.
        self._harness.launch(node_url=self.urls[0], trust=None, raw_fields=seed)
        self._harness.relaunch()
        return self._harness.driver

    @staticmethod
    def _pin(tab) -> str | None:
        """The tab's pin, or None while a navigation replaces its page."""
        try:
            return tab.eval_js(f"sessionStorage.getItem({_TAB_PIN_KEY!r})")
        except RuntimeError:
            return None

    def open_bound(self, actor: str):
        tab = self._harness.driver.open_same_context_tab()
        self._bound = tab
        # An unpinned tab pins itself to the store-active account at boot,
        # behind an await (`accounts.ts::accountsBoot`); a pin written before
        # that lands would be overwritten by it, so wait for it first.
        wait_until(
            lambda: self._pin(tab) is not None,
            _ROUTE_S,
            diagnose=lambda: "the new tab never pinned itself at boot; console: "
            + "\n".join(tab.console_log()[-20:]),
        )
        tab.eval_js(f"sessionStorage.setItem({_TAB_PIN_KEY!r}, {actor!r})")
        tab.hard_reload()
        wait_until(
            lambda: self._pin(tab) == actor,
            _ROUTE_S,
            diagnose=lambda: f"the tab's pin reads {self._pin(tab)!r}, wanted {actor!r}",
        )
        return tab

    def describe(self, tab) -> str:
        """The tab's page path, its pin and the tail of its console ring — a
        page has no stderr (convention 6)."""
        try:
            where = tab.eval_js("location.pathname")
        except RuntimeError as exc:
            where = f"unreadable ({exc})"
        console = "\n".join(tab.console_log()[-40:])
        return f"page {where!r}, pin {self._pin(tab)!r}; console:\n{console}"

    def open_primary(self):
        """Close the bound tab, as the native leg tears its instance down, and
        reopen the first tab — pinned since its first boot to A, the account it
        resolved then."""
        if self._bound is not None:
            with contextlib.suppress(Exception):
                self._bound.teardown()
            self._bound = None
        self._harness.relaunch_as_launched()
        return self._harness.driver

    def _shutdown_servers(self) -> None:
        for server in self._servers:
            with contextlib.suppress(Exception):
                server.shutdown()

    def teardown(self) -> None:
        if self._bound is not None:
            with contextlib.suppress(Exception):
                self._bound.teardown()
        self._harness.teardown()
        self._shutdown_servers()


@pytest.mark.feature("factory-reset")
def test_each_account_keeps_its_own_pending_reset(
    request, reset_client, nest_mode, tmp_path_factory
):
    """Outcome 12: one device holds two accounts, each the admin of its own
    nest. Account A resets its nest from the app; then account B resets its
    nest from the app. B's reset must not touch A's pending re-claim: A's slot
    is byte-for-byte what A's reset wrote, and when the app next opens on A it
    lands A's claim page with A's code — and so does B with B's
    (``nest/common.md`` § Client-state recoverability, CR-3: the slot is
    per-identity).

    B's turn opens the way the product opens a second instance on one account
    — natively ``FAUNA_BOUND_ACCOUNT`` on the launch, on web a tab pinned to B
    (``_NativeAccountSeats`` / ``_WebAccountSeats``) — because A's reset ended
    the session on the claim page, where no account switcher is offered. The
    re-claims run in the primary instance, which alone hosts onboarding: it
    opens on A, and once A is back in the app the switcher takes it to B."""
    from common.accounts import build_registry_seed

    nest_a, cleanup_a = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "reset-journey-a")
    nest_b, cleanup_b = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "reset-journey-b")
    seats = None
    try:
        if reset_client == "web":
            seats = _WebAccountSeats(request, nest_a, nest_b)
        else:
            seats = _NativeAccountSeats(request, reset_client, nest_a, nest_b)
        url_a, url_b = seats.urls
        actor_a, secret_a = _admin_hex(nest_a)
        actor_b, secret_b = _admin_hex(nest_b)
        seed = build_registry_seed([
            {"actor_id": actor_a, "secret_hex": secret_a, "nest_url": url_a,
             "device_id": "reset-a", "handle": "admin"},
            {"actor_id": actor_b, "secret_hex": secret_b, "nest_url": url_b,
             "device_id": "reset-b", "handle": "admin"},
        ], active=actor_a)

        driver = seats.launch_with_seed(seed)
        reached_authenticated_app(driver, timeout=_APP_S)

        code_a = ActionLayer(driver).admin.factory_reset_via_ui()
        resume_after_self_exit(nest_a)
        _wait_nest_fresh(nest_a["url"], timeout=_NEST_FRESH_S)
        slot_a = read_store_slot(driver, _slot_key(actor_a))
        assert slot_a and json.loads(slot_a).get("claim_code") == code_a, (
            f"A's reset must have written A's slot: {describe_registry(driver)}"
        )

        driver = seats.open_bound(actor_b)
        app = ActionLayer(driver)
        try:
            reached_authenticated_app(driver, timeout=_APP_S)
        except AssertionError as exc:
            raise AssertionError(
                "the instance bound to B never reached the app with A's reset "
                f"pending; claim page shown={driver.count(CLAIM_CODE_INPUT) > 0}, "
                f"error={app.error_text()!r}, launch surface: {launch_surface_dump(app)}"
                f"\n{seats.describe(driver)}"
            ) from exc
        code_b = app.admin.factory_reset_via_ui()
        resume_after_self_exit(nest_b)
        _wait_nest_fresh(nest_b["url"], timeout=_NEST_FRESH_S)
        assert code_b != code_a
        assert read_store_slot(driver, _slot_key(actor_a)) == slot_a, (
            "B's reset must leave A's pending re-claim exactly as A's reset wrote "
            f"it: {describe_registry(driver)}"
        )
        assert _slot_code(driver, actor_b) == code_b, (
            f"B's reset must write B's own slot: {describe_registry(driver)}"
        )

        driver = seats.open_primary()
        app = ActionLayer(driver)
        _await_prefilled_claim(app, code_a, "opening on A after B's reset")
        _claim_through_the_wizard(app, nest_a, "A's kept code re-claims A's nest")

        # A is back in the app; the switcher takes the device to B, whose launch
        # resumes B's own pending re-claim.
        driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
        })
        driver.wait_for(ACCOUNT_SWITCHER_LIST, timeout=_ROUTE_S)
        wait_until(lambda: driver.count(ACCOUNT_SWITCHER_ITEM) >= 2, _ROUTE_S)
        # The switcher lists accounts in the order they were added: A, then B.
        driver.click(ACCOUNT_SWITCHER_ITEM, index=1)
        # An admin account asks to confirm before it is switched to.
        wait_until(
            lambda: driver.count(REAUTH_PROMPT) > 0 or driver.count(CLAIM_CODE_INPUT) > 0,
            _ROUTE_S,
            diagnose=lambda: launch_surface_dump(app),
        )
        if driver.count(REAUTH_PROMPT) > 0:
            driver.click(REAUTH_CONFIRM)
        _await_prefilled_claim(app, code_b, "switching to B")
        _claim_through_the_wizard(app, nest_b, "B's code re-claims B's nest")
    finally:
        if seats is not None:
            seats.teardown()
        cleanup_b()
        cleanup_a()


# ---------------------------------------------------------------------------
# Outcome 8 — a member whose nest was reset is told, never signed in to an
# empty account
# ---------------------------------------------------------------------------

@pytest.mark.feature("factory-reset")
def test_a_member_whose_nest_was_reset_is_told_and_offered_a_fresh_start(
    request, reset_client, reset_nest, tmp_path
):
    """Outcome 8: two seats. A member is signed in on their app; the admin, on
    a second app, resets the nest and claims it again. The member's running
    app reconnects to a box that has forgotten them, and must say so — the
    sign-in-refused surface with its sentence, "Use a different nest" for a
    fresh start — never quietly sign in to an empty account (a wiped box no
    longer registers an actor it does not know, ``nest/common.md`` § Factory
    reset → *Persistent-client reconnect across a reset*). Retry asks the box
    again and is refused again; the fresh start opens handle entry."""
    from common.auth import make_keypair, register_user

    nest = reset_nest
    member_actor, member_secret = make_keypair()
    register_user(
        nest["port"], member_actor, base_url=nest["url"],
        admin_signing_key=nest["admin"]["signing_key"],
        handle=f"member{secrets.token_hex(3)}",
    )
    _admin_actor, admin_secret = _admin_hex(nest)
    for seat in ("member", "admin"):
        (tmp_path / seat).mkdir()
    with (
        _seat(request, reset_client, tmp_path / "member", nest) as (member_seat, member_url),
        _seat(request, reset_client, tmp_path / "admin", nest) as (admin_seat, admin_url),
    ):
        member = member_seat.launch_and_route(
            secret_hex=member_secret, node_url=member_url, trust=nest)
        reached_authenticated_app(member, timeout=_APP_S)
        member.wait_for_state(
            lambda s: (s.get("connection") or {}).get("online") is True, timeout=_APP_S)

        admin = admin_seat.launch_and_route(
            secret_hex=admin_secret, node_url=admin_url, trust=nest)
        reached_authenticated_app(admin, timeout=_APP_S)
        admin_app = ActionLayer(admin)
        admin_app.admin.factory_reset_via_ui()
        resume_after_self_exit(nest)
        _wait_nest_fresh(nest["url"], timeout=_NEST_FRESH_S)
        _claim_through_the_wizard(admin_app, nest, "the admin re-claims the reset nest")

        member_app = ActionLayer(member)
        refused = S.onboarding.launch.sign_in_refused

        def told() -> bool:
            return member.is_visible(SIGN_IN_REFUSED_NOTICE) and refused in (
                member.get_text(SIGN_IN_REFUSED_NOTICE) or "")

        wait_until(
            told,
            _RECONNECT_REFUSED_S,
            diagnose=lambda: f"member app: notice shown="
            f"{member.count(SIGN_IN_REFUSED_NOTICE) > 0}, "
            f"authenticated={member.get_state('session.authenticated')!r}, "
            f"connection={member.get_state('connection')!r}, "
            f"error={member_app.error_text()!r}, "
            f"launch surface: {launch_surface_dump(member_app)}; "
            f"member log, auth lines: {_auth_log_lines(member)}",
        )
        assert member.is_visible(LAUNCH_FALLTHROUGH), (
            "the member must be offered a fresh start ('Use a different nest')"
        )

        # Retry asks the box again: still refused, never an empty account.
        member.click(LAUNCH_RETRY)
        wait_until(told, _ROUTE_S, diagnose=lambda: launch_surface_dump(member_app))
        assert not member.get_state("session.authenticated"), (
            "a member the reset nest forgot must never be signed in to an empty account"
        )

        member.click(LAUNCH_FALLTHROUGH)
        member.wait_for(HANDLE_INPUT, timeout=_ROUTE_S)
