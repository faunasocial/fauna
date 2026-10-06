"""tier_3 e2e: the ADMIN drives the deployment-seed rotation through the app UI.

``docs/goal/architecture/nest/box-recovery.md`` § Deployment-seed rotation — the
ceremony's *admin journey*, end to end over a real nest binary and real TLS:
navigate ``admin-nest`` → arm the confirm → the roster listing resolves → confirm
→ the box adopts the successor in-process → the status line renders the shared
verdict → the admin's own live session is silently re-pinned to the successor.

What this adds over the siblings (each pins one layer; this pins the wiring):

* ``api/test_nest_rotation_chain.py`` drives the rotate **kind** directly — no
  UI, no client acceptance.
* ``test_nest_rotation_repin.py`` proves the **relaunch** arm of acceptance
  (pin → rotate via python admin RPC → relaunch → no warning). This module's
  rotation is dispatched by the app itself, so the ceremony's client half
  (``rotate_deployment_seed_on_plane``: mint → custody → dispatch → mark)
  finally has an end-to-end witness — and its *mark* step must land over the
  **live-session reconnect** the serving-generation restart forces. The
  ``rotate_seed_done`` verdict (not the ``done_unmarked`` caveat) is therefore
  the assert: it renders only when the bookkeeping write landed over that
  reconnect.

  The live reconnect also moves the PIN — asserted below, without a relaunch
  (the ruling, 2026-08-13: `box-recovery.md` § Client acceptance →
  *Live-session convergence*). This test's first run measured the opposite:
  the nest's bearer token store deliberately survives the serving-generation
  restart (an unrelated restart — the serving-port change — must not sign
  everyone out), so the reconnect re-authenticated with the held bearer, ran
  no graduation, and the pin stayed on the predecessor until the next launch
  — an unbounded eviction window for a long-running app. The ruled mechanism
  is bearer eviction riding the rotation decision: the rotate handler clears
  the token store, so the teardown's forced reconnect takes 401 → re-mint —
  and the mint is where graduation runs the rotation bridge. The mechanism's
  own nest-side pin is ``api/test_nest_rotation_chain.py``'s
  ``test_a_rotation_evicts_every_bearer_minted_before_it``; here the whole
  wiring is witnessed: the DONE verdict lands over the re-minted connection
  AND the pin names the successor while the original process is still the
  only launch this app ever had.
* The **disarm-before-dispatch** guard gets its journey witness: the first
  confirm click consumes the armed surface in the same frame (nothing left to
  double-click), and the chain read at the end carries exactly ONE hop — one
  ceremony dispatched, ever.
* The **ordering rule** (`box-recovery.md` § Ordering rule: the confirm's job is
  to name the set that will inherit, before dispatch) is asserted as rendered
  state: roster rows painted, no withhold reason, confirm enabled — only then is
  it clicked.

Deliberately NOT asserted here: the custody map's contents after the
rotation. The drive runs on the account plane (``box-recovery.md`` § The
plane-era recovery floor, (c) The writes): it merges the successor's
``fauna.state.deployment-seeds`` row, waits until that row is published to the
bound nest, and only then dispatches — there is no seed-map fan-out any more.
That custody-before-dispatch ordering and the supersession mark are pinned
below the UI (``fauna-client-config``'s ``custody_leg`` tests of
``rotate_deployment_seed_on_plane``); this module is the admin surface's witness, not the custody map's.
Stated so a later reader doesn't mistake it for that witness.

Client arm: **all 7 apps**. tui leads the admin rotate
surface (``admin-nest-seed-rotate-*``, `ui.yaml` § admin_nest); linux joined
2026-08-16, android 2026-08-17, windows joined
2026-09-06, macOS/iOS joined
2026-09-06 — a genuine apple-side product bug, not
mere fixture wiring: ``AdminNestView.swift``'s roster rows registered under
the SHARED base id ``admin-nest-seed-rotate-roster-item`` via
``.automationScope(id, index:)``, which resolves only when a caller passes an
explicit ``scope=`` query — this journey (like windows'
``AutomationProperties.SetAutomationId(row, $"…roster-item-{i}")`` and
linux's ``set_test_id(&label, &format!("…roster-item-{i}"))``) addresses each
row by its own literal ``-{n}``-suffixed id with no scope at all, so the
roster never resolved and the ceremony hung indefinitely at the arm step.
Fixed by baking the index directly into the registered id, matching
windows/linux and `ui.yaml`'s own documented ``-{n}`` shape. iOS mirrors
``test_nest_identity_pin.py``'s ``ios_setup``/``udid`` branch, since
``make_launch_harness``'s direct-launch iOS leg needs both together. **Web got its own
arm, not a bare list-append**,
wired through the ``journey_env`` fixture below (mirrors
``test_nest_identity_pin.py``'s ``pin_env`` branch): its own dedicated
plain-HTTP ``rotatable_nest`` (already existed for
``api/test_nest_rotation_chain.py``) behind a dedicated ``rotatable_spa_url``
proxy, rather than a fourth ``{journey_client}_app_path`` entry.
``journey_env`` carries the driver-facing ``node_url`` (what the browser/app
navigates to — the proxy for web, the nest itself for native) SEPARATELY
from ``nest_url`` (the real nest, for this module's anonymous
``WsRpcAnonClient`` calls — ``_nest_id_hex``/``_chain``, which must bypass
the CORS proxy entirely since they are raw python sockets, not a browser
subject to CORS).

**Web's TOFU-pin assertions are structurally out of reach, not merely
harder** — this was the open question this row's original text left as "web
needs its own arm" without pinning down why, and the answer turned out to be
a real blocker, not a fixture-wiring detail. ``check_web_nest_identity``
(``fauna_client_core::nest_trust``) only mints a pin when the connect's
``seen`` identity is ``Some`` — i.e. the nest served a usable
``cert_binding`` — and a **plain-HTTP nest never serves one** (there is no
cert to bind against, full stop, independent of any nonce-signing on top of
it). Web therefore always reads ``Unprovable`` against ``rotatable_nest``,
never mints a real pin, and has nothing for the live-reconnect re-pin to
move. (`test_nest_identity_pin.py`'s own WITHDRAWN arm never contradicts
this — it SEEDS its pin through the test-only bridge to fake a stale one,
never by relying on an organic auto-pin, so it never actually exercises the
auto-pin path either.) ``env.checks_pin`` (``False`` for web, ``True``
otherwise) guards every pin-specific assertion below; web's real coverage is
the admin ceremony UI (nav → arm → roster → confirm) and the nest-side
rotation state (successor adoption, the one-hop chain) — genuine, valuable
witnesses on their own, just not the pin-acceptance ones native provides.

Latency-independent (convention 14): every wait is a named generous budget +
deadline poll on rendered/served state; a green run pays only the real delays.
"""

import contextlib
import json
import time

import pytest

from common.launch_harness import make_launch_harness, reached_authenticated_app
from conftest import _trust_seeder, get_available_apps

pytestmark = [
    pytest.mark.tier_3, pytest.mark.tui, pytest.mark.linux, pytest.mark.android,
    pytest.mark.web, pytest.mark.windows, pytest.mark.macos, pytest.mark.ios,
]

_SUPPORTED_APPS = ("tui", "linux", "android", "web", "windows", "macos", "ios")

# Element IDs (tests/e2e-unified/ui.yaml § admin_nest + § launch_identity_changed).
ARM_BUTTON = "admin-nest-seed-rotate-button"
ROSTER_ITEM_0 = "admin-nest-seed-rotate-roster-item-0"
ROSTER_ITEM_1 = "admin-nest-seed-rotate-roster-item-1"
ROSTER_REASON = "admin-nest-seed-rotate-roster-reason"
CONFIRM_BUTTON = "admin-nest-seed-rotate-confirm-button"
STATUS = "admin-nest-seed-rotate-status"
IDENTITY_WARNING = "nest-identity-changed-warning"

# The surface's own wording (i18n `admin.nest_page.rotate_seed_*`, en.yaml) — the
# shared verdict every app renders (`fauna_client_config::seed_rotation_verdict`).
# `DONE`, not `done_unmarked`: the caveat wording would mean the ceremony's mark
# step never landed, i.e. the live-reconnect re-pin this journey exists to prove
# did not happen.
WORKING = "Rotating the deployment identity…"
DONE = "Deployment identity rotated. Apps re-trust this nest automatically."

# Named budgets (generous ceilings; deadline polls pay only the real delay).
_PAGE_BUDGET_S = 30.0        # admin-nest page render after the nav patch
_ROSTER_BUDGET_S = 30.0      # roster read (admins_list + per-member users.get)
_ADOPTION_BUDGET_S = 60.0    # in-process generation restart (as the siblings)
_CEREMONY_BUDGET_S = 180.0   # whole drive incl. the post-rotation reconnect+mark
_REPIN_BUDGET_S = 30.0       # live re-pin store write → bridge read (post-verdict)


def _journey_clients():
    available = get_available_apps()
    return [c for c in _SUPPORTED_APPS if c in available]


@pytest.fixture(params=_journey_clients())
def journey_client(request):
    return request.param


class _JourneyEnv:
    """The launch harness for one app plus the two URLs the journey needs.

    ``node_url`` is what the driver/browser navigates to (the CORS proxy for
    web, the nest itself for native); ``nest_url`` is the real nest, for this
    module's own anonymous ``WsRpcAnonClient`` calls (``_nest_id_hex``,
    ``_chain``, ``_serving_successor``), which must bypass the proxy entirely
    — they are raw python sockets, never subject to CORS. Native rides both
    off the same URL; only web needs the split (mirrors ``pin_env`` in
    ``test_nest_identity_pin.py``, one fixture per app-family branch rather
    than a bare ``_SUPPORTED_APPS`` list-append)."""

    def __init__(self, *, harness, node_url, nest, secret_hex, checks_pin):
        self.harness = harness
        self.node_url = node_url
        #: The nest handle: its url for the anonymous calls, and the harness
        #: launch's `trust`.
        self.nest = nest
        self.nest_url = nest["url"]
        self.secret_hex = secret_hex
        #: Whether this app-family can produce a real TOFU pin to move.
        #: False for web — see ``journey_env``'s docstring; the ceremony UI
        #: and the nest-side rotation state are still asserted either way.
        self.checks_pin = checks_pin


@pytest.fixture
def journey_env(request, journey_client, tmp_path):
    """Per-app launch harness + the rotatable nest it drives.

    Native (tui/linux/android) rides ``rotatable_tls_nest`` (real TLS). Web
    rides the plain-HTTP ``rotatable_nest`` behind its own
    ``rotatable_spa_url`` proxy — and, unlike native, **never produces a real
    TOFU pin to move**: ``check_web_nest_identity``
    (``fauna_client_core::nest_trust``) only mints a pin when the connect's
    ``seen`` identity is ``Some`` — i.e. the nest served a usable
    ``cert_binding`` — and a plain-HTTP nest never serves one (there is no
    cert to bind). Web permanently reads ``Unprovable`` against a plain-HTTP
    nest, same root fact as ``test_nest_identity_pin.py``'s WITHDRAWN arm
    (whose pin exists only because that module SEEDS it through the
    test-only bridge, never by relying on an organic auto-pin — this module
    first assumed web could organically auto-pin the way native does, and
    that assumption was wrong; ``env.checks_pin`` is the fix). Both nest
    fixtures are function-scoped and dedicated: rotating the shared session
    nest would strand every later test on the identity-changed path."""
    if journey_client == "web":
        spa_url = request.getfixturevalue("rotatable_spa_url")
        nest = request.getfixturevalue("rotatable_nest")
        harness = make_launch_harness("web", spa_url=spa_url)
        env = _JourneyEnv(
            harness=harness,
            node_url=spa_url,
            nest=nest,
            secret_hex=bytes(nest["admin"]["signing_key"]).hex(),
            checks_pin=False,
        )
    else:
        nest = request.getfixturevalue("rotatable_tls_nest")
        if journey_client == "ios":
            # No bare `ios_app_path` fixture exists — iOS's direct-launch
            # fixture (`ios_setup`) returns `{"udid", "app_path"}` together,
            # because `drivers/ios.py`'s `launch()` requires both. Same
            # branch as `test_nest_identity_pin.py`'s `pin_env`.
            ios_setup = request.getfixturevalue("ios_setup")
            harness = make_launch_harness(
                "ios", tmp_path=tmp_path, app_path=ios_setup["app_path"],
                udid=ios_setup["udid"], seed_trust=_trust_seeder(request),
            )
        else:
            app_path = request.getfixturevalue(f"{journey_client}_app_path")
            harness = make_launch_harness(
                journey_client, tmp_path=tmp_path, app_path=app_path,
                seed_trust=_trust_seeder(request),
            )
        env = _JourneyEnv(
            harness=harness,
            node_url=nest["url"],
            nest=nest,
            secret_hex=bytes(nest["admin"]["signing_key"]).hex(),
            checks_pin=True,
        )
    try:
        yield env
    finally:
        with contextlib.suppress(Exception):
            harness.teardown()


def _poll(check, budget_s, tag, detail=None):
    """Deadline-poll ``check`` until truthy; the failure names the budget and,
    when given, what ``detail()`` reads off the app at that moment (convention
    6 — the failure diagnoses itself)."""
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.25)
    seen = ""
    if detail is not None:
        try:
            seen = f" — app shows: {detail()!r}"
        except Exception as e:  # noqa: BLE001 — diagnosis must not mask the failure
            seen = f" — (could not read the app: {e})"
    raise AssertionError(f"{tag}: not reached within {budget_s}s{seen}")


def _nest_id_hex(nest_url):
    """The identity the box serves right now (hex), over an anonymous socket
    (mirrors ``test_nest_rotation_repin.py``)."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(nest_url) as anon:
        info = anon.call("fauna.nest.info", {})
    return info["nest_id"]


def _chain(nest_url):
    """The box's rotation chain, oldest hop first, over an anonymous socket
    (mirrors ``api/test_nest_rotation_chain.py``)."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(nest_url) as anon:
        reply = anon.call("fauna.auth.rotation_chain", {})
    assert isinstance(reply, dict), f"rotation_chain reply not a map: {reply!r}"
    return list(reply.get("chain", []))


def _serving_successor(nest_url, before_hex):
    """The successor id (hex) once the box serves one, else None — tolerates the
    re-enter window's refused connections."""
    try:
        served = _nest_id_hex(nest_url)
    except Exception:  # noqa: BLE001 — the window's error shape varies
        return None
    return served if served != before_hex else None


def _read_pin(driver, nest_url):
    """The pin the installed store holds for ``nest_url`` (hex, or None) — the
    same bridge read the pin/repin siblings use."""
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


@pytest.mark.feature("admin-nest")
def test_the_admin_rotates_the_deployment_seed_through_the_app(journey_env, journey_client):
    """nav → arm → roster → confirm → box serves the successor → verdict —
    with the admin's own live session silently re-pinned along the way."""
    env = journey_env
    harness = env.harness
    driver = harness.launch(
        secret_hex=env.secret_hex,
        node_url=env.node_url,
        trust=env.nest,
    )
    if journey_client == "web":
        # `WebLaunchHarness.launch()` only seeds localStorage and deliberately
        # stays on the reset launch surface (`common/launch_harness.py` — a
        # stray reload there would cache state that bypasses a pinned
        # challenge elsewhere); `relaunch()` is the FIRST real login attempt,
        # the web twin of native's boot-with-a-pre-seeded-store `launch()`.
        harness.relaunch()
    reached_authenticated_app(driver, timeout=90)
    before_hex = _nest_id_hex(env.nest_url)
    if env.checks_pin:
        # First contact TOFU-pins the box's real identity over the live TLS
        # binding — the online baseline the re-pin must later move. Native
        # only (`env.checks_pin` — see ``journey_env``'s docstring): a
        # plain-HTTP nest never serves a `cert_binding`, so web can never
        # produce a real pin to check here.
        #
        # The pin store keys by the ORIGIN the browser/app actually dials
        # (== `env.nest_url` for every app this arm covers).
        #
        # Polled, not a bare assert (convention 14): `reached_authenticated_app`
        # returns the instant the shell renders, but the TOFU pin write is a
        # separate async step (`test_nest_identity_pin.py`'s own comment on
        # the CHANGED arm names this exact race) — the budget covers the
        # write landing, never a fixed settle delay.
        _poll(
            lambda: _read_pin(driver, env.node_url) == before_hex,
            _REPIN_BUDGET_S,
            "first contact TOFU-pins the nest's genuine identity",
        )
    assert _chain(env.nest_url) == [], "fixture nest should start un-rotated"

    # Navigate to admin-nest — the same nav patch
    # `actions/admin.py::navigate_nest` sends; controls load async, so poll
    # the rendered section rather than trusting the patch's return.
    driver.set_state({
        "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "admin-nest"}]},
    })
    _poll(lambda: driver.count(ARM_BUTTON) > 0, _PAGE_BUDGET_S,
          "admin-nest renders the rotate section")

    # ARM. The agent awaits the roster op on this click (it is not an
    # outlives-click op), so the armed confirm exists when the click
    # replies; the roster may already be resolved. Poll to Ready.
    driver.click(ARM_BUTTON)
    assert driver.count(CONFIRM_BUTTON) > 0, (
        "arming must paint the confirm surface in the same frame "
        "(disabled while the roster is unknown, never absent)"
    )
    _poll(lambda: driver.count(ROSTER_ITEM_0) > 0, _ROSTER_BUDGET_S,
          "the roster listing resolves")

    # The ordering rule as rendered state (`box-recovery.md` § Ordering
    # rule): the set that will inherit is named — this fixture's whole
    # roster is the one claiming admin — before the confirm is live.
    assert driver.count(ROSTER_ITEM_1) == 0, (
        "a single-admin nest must list exactly one inheritor"
    )
    assert driver.get_text(ROSTER_ITEM_0).strip(), (
        "the inheritor row must carry a label (name or short id — the "
        "shared fold never drops a row it cannot name)"
    )
    assert driver.count(ROSTER_REASON) == 0, (
        "a resolved, non-empty roster must not render a withhold reason"
    )
    assert driver.is_enabled(CONFIRM_BUTTON), (
        "the confirm must be enabled once the inheritor set is named"
    )

    # The plane-era drive merges the successor's custody row through the
    # account store before it dispatches, and refuses plainly without one —
    # so the store's assembly (a task spawned off the login path, on web
    # behind the conversations manager's build) is this step's precondition.
    # Wait for it as state (convention 14), never race it: measured
    # 2026-10-01, web's assembly outlasted the drive's own bounded wait.
    from helpers.waiting import account_pump_role, await_account_runtime_assembled

    if account_pump_role(driver) is not None:
        await_account_runtime_assembled(driver)

    # CONFIRM — dispatches the ceremony (mint → custody merged and published
    # on the account plane → dispatch → mark). Disarm-before-dispatch: the
    # click consumes the armed surface synchronously, so by the time the
    # click replies there is nothing left to double-click.
    driver.click(CONFIRM_BUTTON)
    assert driver.count(CONFIRM_BUTTON) == 0, (
        "the first confirm click must disarm the surface — a second "
        "dispatch would chain a second rotation onto the first"
    )
    assert driver.count(STATUS) > 0, (
        "dispatching must paint the status line (working, then the verdict)"
    )

    # The box adopts the successor in-process (§ Adoption by the running
    # process) — no restart by this test.
    after = {}

    def _adopted():
        served = _serving_successor(env.nest_url, before_hex)
        if served is not None:
            after["hex"] = served
            return True
        return False

    # A drive that refused before dispatch (no store, custody refused, the
    # successor's row not published in time) paints its verdict on the status
    # line — read it into the failure rather than only timing out.
    _poll(_adopted, _ADOPTION_BUDGET_S, "the box serves the successor",
          detail=lambda: driver.get_text(STATUS) if driver.count(STATUS) > 0 else None)
    after_hex = after["hex"]

    # The verdict. `rotate_seed_done` exactly: the caveat wording
    # (`done_unmarked`) would mean the mark step never landed — i.e. the
    # post-rotation reconnect did not silently re-pin, which is the arm
    # this journey exists to witness. The budget covers the reconnect.
    _poll(
        lambda: driver.count(STATUS) > 0 and driver.get_text(STATUS) != WORKING,
        _CEREMONY_BUDGET_S,
        "the ceremony reports its verdict",
    )
    verdict = driver.get_text(STATUS)
    assert verdict == DONE, (
        f"the ceremony must end on the clean success verdict, got: "
        f"{verdict!r} (the unmarked caveat means the live-reconnect re-pin "
        f"never carried the bookkeeping write; a failure wording means the "
        f"ceremony itself broke)"
    )

    # The admin's live session crossed the rotation silently — no
    # identity-changed surface.
    assert driver.is_absent(IDENTITY_WARNING), (
        "a committed rotation must never surface the identity-changed "
        "warning on the driving admin's own session"
    )

    if env.checks_pin:
        # Live-session convergence, with NO relaunch (`box-recovery.md`
        # § Client acceptance → Live-session convergence, the
        # ruling): the rotation cleared the bearer store, so the reconnect
        # the DONE verdict just rode was a re-mint — and the mint's
        # graduation ran the rotation bridge, moving the pin to the
        # successor. The verdict already implies the re-pin happened
        # in-process; the small poll only covers the store write reaching
        # the bridge read. Native only — web never held a real pin to move
        # (`env.checks_pin`).
        _poll(
            lambda: _read_pin(driver, env.node_url) == after_hex,
            _REPIN_BUDGET_S,
            "the live session's pin names the successor without a relaunch",
        )

    # Exactly one ceremony dispatched (the disarm guard's other witness),
    # and the hop links the identity the admin was pinned to with the one
    # the box now serves.
    chain = _chain(env.nest_url)
    assert len(chain) == 1, (
        f"exactly one hop should exist after one confirmed ceremony: {chain!r}"
    )
    hop = chain[0]["statement"]
    assert bytes(hop["old_nest_actor_id"]).hex() == before_hex
    assert bytes(hop["new_nest_actor_id"]).hex() == after_hex

    if not env.checks_pin:
        # Web never held a real pin to move (`env.checks_pin` — see
        # `journey_env`'s docstring), so the persistence-across-relaunch leg
        # below has nothing to witness; the ceremony UI + nest-side state
        # above are web's coverage.
        return

    # Persistence: the pin that moved live must survive the next launch —
    # the relaunch graduation verifies the held (successor) pin against
    # the live binding and stays silent (`box-recovery.md` § Client
    # acceptance; the chain-walk arm itself is `test_nest_rotation_repin.py`'s
    # subject, which starts from a still-stale pin).
    if not driver.preserve_state_across_relaunch():
        pytest.skip(
            f"{journey_client} driver cannot pin its client store across a "
            "relaunch, so the convergence leg cannot be witnessed"
        )
    harness.relaunch()
    reached_authenticated_app(driver, timeout=90)
    assert driver.is_absent(IDENTITY_WARNING), (
        "the post-rotation relaunch must re-pin through the chain, never "
        "surface the identity-changed warning"
    )
    assert _read_pin(driver, env.node_url) == after_hex, (
        "after the relaunch the pin must name the successor the ceremony "
        "installed"
    )
