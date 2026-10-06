"""tier_3 e2e: web's background silent sign-in survives a transient first failure.

``docs/goal/behavior/identity-succession.md`` § The RecoveryKey → *At
succession* makes the successor's first authenticated session show the kit
unbidden, and web spells that session's ``attach_session`` gate as
``identity.registered`` — which only a COMPLETED background silent sign-in sets
(``$lib/store`` ``refreshFromServer``). ``security.md`` § Transport trust →
§ Post-auth surfacing keeps every non-terminal failure of that refresh silent;
silent must not mean abandoned. Before the retry, one transient failure (a
dropped anonymous connect, a timeout) was logged and swallowed, the single-flight
guard stayed sticky for the identity, and ``registered`` stayed false for the
whole document: a successor tab showed no kit and no error until a reload.

**The fault is a real one, not a stub.** The test-only nest dial override
(``$lib/api`` ``nestDialOverride`` — a ``sessionStorage`` slot ``nodeUrl()``
re-reads on every call, compiled out of release builds) is aimed at a loopback
port nothing listens on for the boot, so the production refresh meets a genuine
connection refusal, which the wasm classifies as ``transient:``. Clearing the
slot is the nest "coming back": the next attempt dials the real nest.

**What makes it red without the fix.** Nothing but the refresh's own retry can
set ``registered`` on this document: the test never reloads, never navigates and
never fires the ``silent_sign_in`` e2e trigger (which force-re-runs the refresh
and would go green on the unfixed tree). The wait on ``session.authenticated``
is a positive, latency-independent deadline poll under a named budget sized far
above the retry schedule's cap (convention 14), never a settle-sleep.

tier_3: needs a real ``fauna-nest`` binary; web only — the native apps reach
the same session through their launch machine, whose transient state carries a
visible Retry CTA and re-offers the kit on the next launch.
"""
from __future__ import annotations

import json
import secrets

import pytest

from actions import ActionLayer
from common import build_registry_seed, create_actor_and_register
from drivers import create_driver
from drivers.port_util import find_free_port
from helpers.succession_ceremony import closing_act_console
from helpers.waiting import wait_until


pytestmark = [pytest.mark.tier_3, pytest.mark.web]

#: `$lib/api`'s test-only nest dial override slot.
DIAL_OVERRIDE_KEY = "fauna_e2e_nest_dial_override"

#: The refresh's own failure line (`$lib/store`) — the evidence the fault fired.
REFRESH_FAILED = "[identity] silentSignIn refresh failed"

#: A page boot plus one refused connect. Generous: a green run pays nothing.
FAULT_S = 60.0

#: The retry schedule's cap (`$lib/silent-sign-in-retry`) is 30 s, so the next
#: attempt after the nest comes back lands within that — plus one challenge
#: round trip. Sized far above both (convention 14).
RECOVER_S = 120.0


def _session(driver) -> dict:
    """The tab's published session, or {} while the page is mid-navigation."""
    try:
        return (driver.get_state() or {}).get("session", {}) or {}
    except RuntimeError:
        return {}


def _failures(driver) -> list[str]:
    return [ln for ln in driver.console_log() if REFRESH_FAILED in ln]


def test_web_a_transient_first_silent_sign_in_recovers_without_a_reload(nest_instance, spa_url):
    """A signed-in tab whose first background silent sign-in is refused by a
    transient fault reaches a working session on its own once the nest is
    reachable — no reload, no navigation, no trigger."""
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    actor = user["actor_id_hex"]
    seed = build_registry_seed(
        [{
            "actor_id": actor,
            "secret_hex": bytes(user["signing_key"]).hex(),
            "nest_url": spa_url,
            "device_id": secrets.token_hex(32),
        }],
    )
    dead = f"http://127.0.0.1:{find_free_port()}"

    tab = create_driver("web")
    tab.launch({"url": spa_url + "/app/"})
    try:
        # The profile (one account, booted from the store) and the fault, both
        # in place before the one boot under test.
        tab.eval_js(
            ";".join(
                f"localStorage.setItem({json.dumps(k)},{json.dumps(v)})"
                for k, v in seed.items()
            )
            + f";sessionStorage.setItem({DIAL_OVERRIDE_KEY!r},{json.dumps(dead)})"
        )
        tab.hard_reload()

        def _diagnose() -> str:
            return (
                f"session={_session(tab)!r} "
                f"dial={tab.eval_js(f'sessionStorage.getItem({DIAL_OVERRIDE_KEY!r})')!r}"
                f"{closing_act_console(ActionLayer(tab))}"
            )

        # (1) The fault fired: the boot's refresh met the dead dial, and the tab
        # resolved its identity without reaching a working session.
        wait_until(
            lambda: _failures(tab) and _session(tab).get("actor_id") == actor,
            FAULT_S,
            diagnose=lambda: f"the boot's silent sign-in never met the fault; {_diagnose()}",
        )
        assert not _session(tab).get("authenticated"), (
            "precondition: with the nest unreachable the session must not be a "
            f"working one, or this test proves nothing; {_diagnose()}"
        )
        assert any("transient" in ln for ln in _failures(tab)), (
            "precondition: the fault must be classified TRANSIENT — only that "
            f"class is retried; {_diagnose()}"
        )

        # (2) The nest comes back — same document, nothing else touched.
        tab.eval_js(f"sessionStorage.removeItem({DIAL_OVERRIDE_KEY!r})")

        # (3) The refresh's own retry reaches it.
        wait_until(
            lambda: _session(tab).get("authenticated") is True,
            RECOVER_S,
            diagnose=lambda: (
                "the tab never reached a working session after the nest came back "
                "— the background silent sign-in did not retry its transient "
                f"failure (identity-succession.md § At succession); {_diagnose()}"
            ),
        )
        assert _session(tab).get("actor_id") == actor
    finally:
        tab.teardown()
