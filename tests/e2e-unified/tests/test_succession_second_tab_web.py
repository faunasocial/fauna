"""tier_3 e2e: web's second-window legs of "take your account back" — a second
TAB of the same account runs the ceremony, and a tab pinned to the retired id
comes back up as the successor.

``docs/goal/architecture/apps/account-scoping.md`` § Concurrent instances →
*The binding follows the account* (the rule lives in the shared account
registry the wasm ceremony goes through too) and → *Web*, whose tab pin is
web's twin of a native bound launch: a tab resolves its identity from its own
``sessionStorage`` pin, and rider 2 — a binding to a retired id follows the
chain to the successor — applies to that pin.

**The world.** Tab A is the signed-in app, on Conversations, holding the
account's conversations-engine role (Web Locks, per account). Tab B is a second
tab of the same browser profile — one shared origin store, so the same
identity — which is refused the engine and renders the served-elsewhere
refusal. That is the web shape of the native "bound second instance whose
engine the first holds", and it is what makes tab B's sweep the ``no_engine``
arm without any fault injection.

This file witnesses outcomes **9** and **10** of
``docs/features/take-your-account-back.md``:

* **9** — tab B runs the ceremony and survives it: it shows the successor's
  fresh kit, comes back as the successor, and its sweep view (the retry's
  render gate) survives the switch. On web "stays up" is the same tab coming
  back through a reload, since the ceremony re-pins and reloads the tab that
  switched; the kit and the sweep view are parked across it.
* **10** — a tab whose pin still names the RETIRED id, booted after the
  recovery, comes up signed in as the successor with its own pin re-pointed:
  rider 2 at web's boot (``$lib/accounts`` ``accountsBoot()``, over the shared
  ``terminal_successor_of``). Measured to discriminate: with that one hop
  removed the tab boots as the retired identity, the nest refuses it
  (``fauna.auth.superseded``) and the test goes red.

**Outcome 10 runs in a browser of its own, not on the module's app.** Its
subject is the pinned tab's boot, not a second window's ceremony, so its world
is one tab that runs the ceremony and one that boots pinned to the retired id
afterwards — the linux twin's shape (fresh processes per test). A witness must
not depend on what the module's one seat carries from the test before it.

**The "intermittent second ceremony" this file was once blocked on did not
survive measurement** (2026-09-21). The refused-second-tab
ceremony completed in all six of its runs, across two pytest runs of this file
and a probe module: four as the first ceremony of a module, two as the second.
Each took about 9 s against a 120 s ceiling. The failures recorded against it on
2026-09-20 were measured on trees that still carried web's two boot bugs (the
removed outcome-10 attempt's ledger record predates both fixes). Their
"token fetches against an already-revoked nest" signature is also the signature
of a seat still holding a previous ceremony's RETIRED identity, the only one a
ceremony revokes, and a fresh actor per test cannot produce that. The failure
diagnostics now quote each tab's console chain lines
(``helpers/succession_ceremony.py::closing_act_console``), so a recurrence names
its own broken link. The one mechanism in today's tree with the exact
"no kit, empty error surface" shape is web's one-shot background silent sign-in
(``$lib/store`` ``refreshFromServer``: a transient failure left
``identity.registered`` false for the page, and the closing act gates on it).
That refresh now retries a transient failure, witnessed deterministically by
``test_silent_sign_in_retry_web.py``.

tier_3: needs a real ``fauna-nest`` binary; web only (the native legs live in
``test_account_instance_lock_{linux,windows,tui}.py`` and
``test_account_switcher_apple.py``). Outcome 9 uses ``ungranted_app`` — a
dedicated fresh actor — because the ceremony revokes every session of the
account; outcome 10 registers its own.
"""
from __future__ import annotations

import json
import secrets

import pytest

from actions import ActionLayer
from common import build_registry_seed, create_actor_and_register
from drivers import create_driver
from helpers.succession_ceremony import (
    SECRET_HEX_LEN,
    SUCCESSION_AND_RELAUNCH_S,
    wait_for_successor_actor,
    closing_act_console,
    kit_on_screen,
    settled_actor_id,
    succeed_identity_from,
)
from helpers.succession_retry import (
    assert_the_retry_affordance_matches_the_sweep,
    sweep_owes_work,
)
from helpers.waiting import wait_until
from helpers.web_tabs import open_same_account_tab

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

CONVERSATIONS_NAV = {"nav": {"stack": [{"view": "conversations"}]}}
NEW_CONVERSATION_BUTTON = "new-conversation-button"
ERROR_MESSAGE = "error-message"
#: This tab's account pin (`$lib/tabPin`) — `sessionStorage`, per-tab, and web's
#: twin of a native bound launch's `FAUNA_BOUND_ACCOUNT`.
PIN_KEY = "fauna_tab_account"

# The two storage reads a boot-resolution failure has to be diagnosed from:
# this tab's pin and the wasm registry's index (whose `succeeded_by` link is
# what the chain hop walks).
_PIN_JS = f"sessionStorage.getItem('{PIN_KEY}')"
_INDEX_JS = "localStorage.getItem('fauna/index')"

# A named, generous ceiling on "the tab booted and asked for the engine role" —
# one Web Locks round trip plus a page boot (convention 14).
ROLE_S = 60.0


def _session(driver) -> dict:
    """The tab's published session, or {} while it is mid-navigation (the
    bridge errors when the execution context is being replaced)."""
    try:
        return (driver.get_state() or {}).get("session", {}) or {}
    except RuntimeError:
        return {}


def _signed_in_as(driver, actor: str) -> bool:
    """Whether this tab serves ``actor``.

    Deliberately NOT also asserting ``session.authenticated``: that field is
    ``identity.registered``, which only the silent challenge's completion sets
    (`$lib/store`), so a seat arranged through the session patch publishes no
    such key at all. The identity the tab resolved is the observable here; that
    it is a WORKING session is what the Status-page reads assert.
    """
    return _session(driver).get("actor_id") == actor


def _tab_a_holds_the_engine(app) -> None:
    """Put tab A on Conversations and wait until it holds the engine role —
    the compose button is bound to ``disabled={!manager}``, so an ENABLED
    button is positive evidence (the same observable
    ``test_engine_role_election_web.py`` reads)."""
    app.driver.set_state(CONVERSATIONS_NAV)
    app.driver.wait_for(NEW_CONVERSATION_BUTTON, timeout=ROLE_S)
    wait_until(
        lambda: app.driver.is_enabled(NEW_CONVERSATION_BUTTON),
        ROLE_S,
        diagnose=lambda: (
            "tab A never built its conversations manager (compose stayed "
            f"disabled), so tab B cannot be the engine-less seat; error={app.error_text()!r}"
        ),
    )


def _open_refused_second_tab(app):
    """Open tab B on the same account and wait for its served-elsewhere
    refusal — the proof it is the engine-less second window."""
    # `open_same_account_tab` does the arranging: web's session patch writes the
    # page's in-memory identity store and not `localStorage`, so an unseeded new
    # tab boots as nobody, and a seeded-but-unpinned one can boot with no
    # resolved identity at all (see `helpers/web_tabs.py`).
    tab_b = open_same_account_tab(app.driver)
    tab_b.set_state(CONVERSATIONS_NAV)

    def _refused():
        try:
            return "another instance" in (tab_b.get_text(ERROR_MESSAGE) or "").lower()
        except Exception:  # noqa: BLE001 — mid-boot the element may not exist yet
            return False

    def _diagnose():
        try:
            text = tab_b.get_text(ERROR_MESSAGE)
        except Exception as e:  # noqa: BLE001
            text = f"<unreadable: {e}>"
        state = tab_b.get_state() or {}
        return (
            "tab B rendered no served-elsewhere refusal — it is not the "
            f"engine-less second window. error-message={text!r}, "
            f"session={state.get('session')!r}, nav={state.get('nav')!r}"
        )

    wait_until(_refused, ROLE_S, diagnose=_diagnose)
    return tab_b


def _run_ceremony_in(tab_b, *, other_tab=None) -> tuple[str, str, str, ActionLayer]:
    """Run the ceremony from tab B and wait for the closing act and the switch.
    Returns ``(old_actor, held_kit, new_actor, app_b)``.

    ``other_tab`` is the live sibling tab, when the world has one: its console
    joins the failure message, because a sibling of the same origin shares the
    store the ceremony writes, and a break in the chain may be its doing."""
    app_b = ActionLayer(tab_b)
    old_actor, held = succeed_identity_from(app_b)

    def _consoles() -> str:
        text = closing_act_console(app_b, label="the ceremony's tab")
        if other_tab is not None:
            text += closing_act_console(ActionLayer(other_tab), label="the sibling tab")
        return text

    # The closing act, read FIRST and with no navigation of our own — entering
    # Account clears a kit on screen (the shown-once custody rule).
    wait_until(
        lambda: len(kit_on_screen(app_b)) == SECRET_HEX_LEN,
        SUCCESSION_AND_RELAUNCH_S,
        diagnose=lambda: (
            f"no kit on screen for the successor in tab B (reads {kit_on_screen(app_b)!r}), "
            f"error={app_b.error_text()!r}, session={_session(tab_b)!r}{_consoles()}"
        ),
    )
    assert kit_on_screen(app_b) != held, (
        "the successor must mint a FRESH RecoveryKey — the old one retired with "
        "the old identity"
    )

    # The switch: the Status page settles on a different actor id, and tab B
    # holds a live session as it.
    new_actor = wait_for_successor_actor(app_b, old_actor, diagnose=_consoles)
    assert len(new_actor) == SECRET_HEX_LEN and new_actor != old_actor, (
        f"the successor id is a fresh 64-hex actor id, got {new_actor!r}"
    )
    return old_actor, held, new_actor, app_b


@pytest.mark.feature("take-your-account-back")
def test_web_a_second_tab_runs_the_ceremony_and_comes_back_as_the_successor(ungranted_app):
    """A second tab of the same account runs "my identity was stolen" to
    completion: the successor's kit is shown, the tab comes back signed in as
    the successor, and its sweep view — the retry offer's render gate —
    survives the switch as the ``no_engine`` arm."""
    app = ungranted_app
    tab_b = None
    try:
        _tab_a_holds_the_engine(app)
        tab_b = _open_refused_second_tab(app)

        _old_actor, _held, new_actor, app_b = _run_ceremony_in(tab_b, other_tab=app.driver)

        wait_until(
            lambda: _signed_in_as(tab_b, new_actor),
            SUCCESSION_AND_RELAUNCH_S,
            diagnose=lambda: (
                "tab B must come back up serving the successor — the binding "
                f"followed the account; session={_session(tab_b)!r}"
            ),
        )

        sweep = tab_b.get_state("data.succession_sweep")
        assert sweep is not None, (
            "the sweep view died with the switch — the retry's render gate reads "
            "it, and web parks it across the ceremony's reload"
        )
        assert sweep.get("status") == "no_engine", (
            "tab B's engine was refused by tab A's role lock, so its ceremony "
            f"swept with NO engine — the arm this world exists to produce; got {sweep!r}"
        )
        assert sweep_owes_work(sweep), f"a no_engine sweep owes work; {sweep!r}"
        assert_the_retry_affordance_matches_the_sweep(app_b, sweep)

        # Tab A is still an open, answering page — the second tab's ceremony did
        # not take the first one down. What its next connect does about the
        # superseded identity is the own-device leg, not this outcome.
        assert isinstance(app.driver.get_state(), dict), (
            "tab A stopped answering after tab B's ceremony"
        )
    finally:
        if tab_b is not None:
            tab_b.teardown()


def _index(driver) -> dict | None:
    """The wasm registry's ``fauna/index`` (``AccountIndex``), or None — read
    through a navigation, since ``localStorage`` holds the same value on both
    sides of a reload and only the execution context goes away."""
    try:
        raw = driver.eval_js(_INDEX_JS)
    except RuntimeError:
        return None
    return json.loads(raw) if raw else None


def _pin_of(driver) -> str | None:
    try:
        return driver.eval_js(_PIN_JS)
    except RuntimeError:
        return None


def _recorded_the_chain(index: dict | None, old_actor: str, new_actor: str) -> bool:
    """Whether the registry holds the ``old → new`` hop the pin walk follows —
    on the retired row (``succeeded_by``) or on the successor's own
    (``succeeded_from``), the two sources ``terminal_successor_of`` reads."""
    rows = {a.get("actor_id"): a for a in (index or {}).get("accounts", [])}
    return (
        rows.get(old_actor, {}).get("succeeded_by") == new_actor
        or old_actor in (rows.get(new_actor, {}).get("succeeded_from") or [])
    )


@pytest.mark.feature("take-your-account-back")
def test_web_a_tab_pinned_to_the_retired_id_comes_up_as_the_successor(nest_instance, spa_url):
    """**A tab pinned to the retired id follows the chain** (`account-scoping.md`
    § Concurrent instances → *Web*: "Rider 2 holds unchanged — a pin names an
    account, so it is walked through `terminal_successor_of` at boot and
    re-pointed at the terminal successor"). Web's twin of
    ``test_linux_a_launch_bound_to_a_retired_id_comes_up_as_the_successor``: a
    tab's ``sessionStorage`` pin is its launch binding, so a tab whose pin still
    names the account's OLD id — minted before the recovery, booting after it —
    must come up signed in as the successor.

    **Its own browser, not the module's app** — the shape of every two-tab test
    in ``test_account_tab_pin_web.py``, and the linux twin's (fresh processes
    per test). What that buys, and why it is load-bearing rather than tidy, is in
    the module docstring: the module app is one seat for every test in the file.

    The first tab signs in from the origin store alone (a seeded registry and a
    reload, no session patch), so the account's row, the pin and the ``active``
    pointer are all the product's own boot's — the state a real profile holds.
    """
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    old_actor = user["actor_id_hex"]
    seed = build_registry_seed(
        [{
            "actor_id": old_actor,
            "secret_hex": bytes(user["signing_key"]).hex(),
            "nest_url": spa_url,
            # A device leaf of this profile's own: never the fixture-wide
            # `_E2E_LOGIN_DEVICE_ID` every patched seat on the session nest shares.
            "device_id": secrets.token_hex(32),
        }],
    )

    tab_a = create_driver("web")
    tab_a.launch({"url": spa_url + "/app/"})
    tab_c = None
    try:
        # ── the profile: one account, booted from the store ─────────────────
        tab_a.eval_js(
            ";".join(
                f"localStorage.setItem({json.dumps(k)},{json.dumps(v)})"
                for k, v in seed.items()
            )
        )
        tab_a.hard_reload()
        wait_until(
            lambda: _signed_in_as(tab_a, old_actor)
            and _session(tab_a).get("authenticated"),
            ROLE_S,
            diagnose=lambda: (
                "the seeded profile never booted into a WORKING session as its "
                f"account; session={_session(tab_a)!r} pin={_pin_of(tab_a)!r}"
                f"{closing_act_console(ActionLayer(tab_a))}"
            ),
        )

        # ── the recovery, run from this tab ─────────────────────────────────
        ceremony_old, _held, new_actor, _app_a = _run_ceremony_in(tab_a)
        assert ceremony_old == old_actor
        index = _index(tab_a)
        assert _recorded_the_chain(index, old_actor, new_actor), (
            "precondition: the ceremony must record the old → new hop in the "
            "origin's registry — without it there is no chain for a pin to walk, "
            f"and this test would prove nothing; fauna/index reads {index!r}"
        )

        # ── the shortcut: a tab whose pin names the RETIRED id ──────────────
        # A new tab starts with an empty `sessionStorage`, so its first boot is
        # the primary's (it pins itself to `active`, the successor). The pin is
        # then set to the retired id — what a tab pinned before the recovery
        # still carries — and the tab is booted again over it: that boot is the
        # one under test.
        tab_c = tab_a.open_same_context_tab()
        tab_c.eval_js(f"sessionStorage.setItem({PIN_KEY!r},{json.dumps(old_actor)})")
        assert _pin_of(tab_c) == old_actor, "precondition: the tab carries the retired id"
        tab_c.hard_reload()

        def _diagnose() -> str:
            return (
                f"a tab pinned to the RETIRED id {old_actor!r} did not come up as the "
                f"successor {new_actor!r} (account-scoping.md § Concurrent instances → "
                f"Web, rider 2). session={_session(tab_c)!r} pin={_pin_of(tab_c)!r} "
                f"index={_index(tab_c)!r}"
                f"{closing_act_console(ActionLayer(tab_c), label='the pinned tab')}"
            )

        # Not also `session.authenticated`: tab A is still open and holding
        # the account's engine role (`test_web_a_second_tab_runs_the_ceremony_
        # and_comes_back_as_the_successor`'s `_signed_in_as`-only wait is the
        # same shape for the same reason), and the identity here also arrived
        # via the succeeded-identity relaunch escape hatch rather than the
        # normal `refreshFromServer` boot, which is what actually sets that
        # field. The live-session proof is the later Status-page wait below.
        wait_until(
            lambda: _signed_in_as(tab_c, new_actor),
            SUCCESSION_AND_RELAUNCH_S,
            diagnose=_diagnose,
        )
        assert _pin_of(tab_c) == new_actor, (
            "the walk must RE-POINT the tab's own pin at the successor, as the "
            "native `resolve_launch_binding` re-points a process binding — "
            f"otherwise every later boot walks the chain again; {_diagnose()}"
        )
        # The session is a live one as the successor: the Status page renders
        # the actor id the nest authenticated, not merely the one the tab chose.
        app_c = ActionLayer(tab_c)
        wait_until(
            lambda: settled_actor_id(app_c) == new_actor,
            SUCCESSION_AND_RELAUNCH_S,
            diagnose=lambda: f"Status reads {settled_actor_id(app_c)!r}; {_diagnose()}",
        )
    finally:
        if tab_c is not None:
            tab_c.teardown()
        tab_a.teardown()
