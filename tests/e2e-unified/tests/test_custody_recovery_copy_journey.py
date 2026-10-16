"""Tier_3 tui journey: the kit-only restore at a CUSTODIAN nest — every device
and the home nest gone, the copied recovery kit and a friend's nest address
bring the account back.

Goal docs: ``docs/goal/behavior/identity-succession.md`` § Seed escrow → *The
custodian copy* (ruling 4: no new screen — ``recovery_entry``'s account field
in its direct-address form, ``handle@<custodian address>``, with the kit's
``actor=`` naming the account; it lands on ``handle_entry`` as every restore
does) and ``docs/goal/behavior/onboarding.md`` § 1 → *The cold device*.

The nest half is pinned in Rust over two real nests
(``bins/fauna-nest/tests/conformance_custody_recovery_copy_client.rs``), but
that rig CONSTRUCTS the custody and calls the shared ``restore_seed``
directly. Only this journey proves the chain a user actually walks: a custody
minted through both apps' UIs reaches the custodian's pump as an
``Account``-form row; the pump's recovery leg dials the owner's TLS nest and
takes the seed escrow into the pair store; and a factory-reset app, typing
nothing but the copied kit and ``handle@<custodian>``, gets the seed back from
a nest that never held the account — with the home nest stopped.

Topology: ``two_nest_custody_rig`` — alice home on TLS nest A, bob on TLS nest
B, B's periodic pump held so every pass is this test's own ``run-now`` poke
(convention 14: state, never a cadence).

The two honest arms ruling 3 and ruling 2 ask for, both at B with A gone:

* **a wrong kit reads exactly like an unknown account** — every refusal for a
  non-resident actor is the one opaque ``not_registered``, so nobody but the
  kit holder learns whether B custodies the account;
* **a kit replaced while A lived stops opening the copy at B** after one pump
  pass, while the replacement opens it.
"""

from __future__ import annotations

import secrets
import time
from urllib.parse import parse_qsl, urlencode, urlsplit

import pytest
import requests

from helpers.budgets import MLS_HANDSHAKE_S, RECEIVE_CYCLE_S
from helpers.recovery_restore import (
    RESTORE_OUTCOME_S,
    copy_the_shown_kit,
    lose_every_device_and_open_restore,
    wait_for_restore_landing,
)
from helpers.waiting import (
    await_receive_cycle_after,
    conv_receive_cycles,
    poke_receive_cycle,
    wait_until,
)
from tests.api import conv_api

pytestmark = pytest.mark.tier_3

# A UI state that only needs local folds after a poked cycle.
_UI_LEG_S = 30.0
# The custody ceremony plus the custodian pump's first pulls, poked every try.
_CUSTODY_S = 240.0


def _receive_once(app) -> None:
    """One poked receive cycle, completion-anchored."""
    cycles = conv_receive_cycles(app.driver)
    started = cycles[0] if cycles else None
    poke_receive_cycle(app.driver)
    await_receive_cycle_after(
        app.driver, started, budget_s=RECEIVE_CYCLE_S, what="the ceremony payload"
    )


def _pump_once(custodian: dict) -> dict:
    """One custodian-nest custody-hosting pass; returns its report."""
    resp = requests.post(
        f"{custodian['url']}/api/v1/test/custody_hosting/run-now",
        timeout=120,
        verify=False,
    )
    assert resp.status_code == 200, (
        f"custody_hosting run-now returned {resp.status_code}: {resp.text}"
    )
    return resp.json()


def _custody_log_tail(app) -> str:
    """The custody/recovery lines from one seat's app stderr (convention 6)."""
    try:
        text = app.driver.app_stderr_text() or ""
    except Exception as e:  # noqa: BLE001 — diagnostics must not mask the assert
        return f"(app stderr unreadable: {e})"
    wanted = ("custody", "recovery", "escrow", "generation")
    lines = [ln for ln in text.splitlines() if any(w in ln for w in wanted)]
    return "\n".join(lines[-60:]) or "(no custody lines in the app log)"


def _open_carrier(alice_app, bob_app, bob: dict, custodian: dict) -> None:
    """The ceremony's carrier: a real 1:1 MLS conversation alice opens with
    bob ACROSS the nests (the mint's host list derives from 2-member conv
    channels), bound on bob's side before any ceremony payload rides it. Bob's
    key-package pool must settle first — a >=1 read races his login top-up."""
    quiet_s = 3.0
    deadline = time.time() + MLS_HANDSHAKE_S
    last, stable_since, settled = None, None, False
    while time.time() < deadline:
        count = conv_api.keypackage_count(custodian["port"], bob, bob["actor_id_hex"])
        now = time.time()
        if count != last:
            last, stable_since = count, now
        elif count >= 1 and now - stable_since >= quiet_s:
            settled = True
            break
        time.sleep(0.5)
    assert settled, f"bob's key-package pool never settled (last {last!r})"

    alice_app.conversations.real_resolve_send_new(
        f"{bob['handle']}@{custodian['authority']}", "custody carrier"
    )
    _receive_once(bob_app)


def _poll_devices(app, predicate, *, what: str, budget_s: float = _CUSTODY_S) -> None:
    """Deadline-poll a Devices-page predicate, a receive cycle poked and the
    page re-entered each try (the facet folds on the nav edge)."""
    def ready() -> bool:
        _receive_once(app)
        app.backups.navigate_devices()
        return predicate()

    wait_until(ready, budget_s, interval=0.5, diagnose=lambda: f"never observed: {what}")


def _pump_until(rig: dict, predicate, *, what: str) -> dict:
    """Poke the custodian's pump (both seats' receive cycles first, so the
    ceremony payloads keep flowing) until ``predicate(report)`` holds."""
    last: dict = {}

    def converged() -> bool:
        _receive_once(rig["alice_app"])
        _receive_once(rig["bob_app"])
        last.clear()
        last.update(_pump_once(rig["custodian"]))
        return predicate(last)

    wait_until(
        converged,
        _CUSTODY_S,
        interval=0.5,
        diagnose=lambda: (
            f"never observed: {what}; last pass report: {last}"
            f"\n--- bob's custody log ---\n{_custody_log_tail(rig['bob_app'])}"
            f"\n--- alice's custody log ---\n{_custody_log_tail(rig['alice_app'])}"
        ),
    )
    return dict(last)


def _with_param(kit: str, name: str, value: str) -> str:
    """The copied kit with one query parameter replaced — a forged kit the
    user could paste, built without ever printing the real secret."""
    parts = urlsplit(kit)
    query = [(k, value if k == name else v) for k, v in parse_qsl(parts.query)]
    assert any(k == name for k, _ in query), f"the copied kit carries no {name}="
    return f"{parts.scheme}://{parts.netloc}{parts.path}?{urlencode(query, safe='@:')}"


def _refusal_of(app, kit: str, account: str) -> str:
    """Submit ``kit`` for ``account`` and return the refusal it renders; the
    user must stay on ``recovery_entry`` to try again."""
    app.onboarding.restore_from_recovery_kit(kit, account=account)
    wait_until(
        lambda: app.has_error() or app.is_visible("handle-input"),
        RESTORE_OUTCOME_S,
        diagnose=lambda: (
            "the restore got no answer at all; page shows phrase-field="
            f"{app.is_visible('recovery-entry-phrase-field')}"
        ),
    )
    assert app.driver.is_absent("handle-input"), (
        "a kit the custodian must refuse restored the account instead"
    )
    assert app.is_visible("recovery-entry-phrase-field"), (
        "a refusal keeps the user on recovery_entry to try again"
    )
    return app.error_text()


@pytest.mark.real_conversations
@pytest.mark.feature("recovery-kit")
def test_the_copied_kit_restores_at_a_custodian_after_the_home_nest_is_gone(
    two_nest_custody_rig,
):
    rig = two_nest_custody_rig
    alice_app, alice = rig["alice_app"], rig["alice"]
    bob_app, bob = rig["bob_app"], rig["bob"]
    home, custodian = rig["home"], rig["custodian"]
    alice_app.custody.require_supported()

    # ── alice's first kit, minted and copied on her signed-in device.
    alice_app.settings.navigate()
    alice_app.settings.open_recovery_kit_or_skip()
    alice_app.settings.create_recovery_kit()
    alice_app.wait_for("recovery-kit-secret-display", timeout=30.0)
    first_phrase = alice_app.settings.recovery_kit_secret()
    first_kit = copy_the_shown_kit(alice_app, first_phrase)

    # ── The custody, through both UIs: alice offers bob to hold her copies;
    # bob takes it on HIS NEST ("My nest"), so the hosting row lands on B.
    _open_carrier(alice_app, bob_app, bob, custodian)
    alice_app.backups.navigate_devices()
    # A cross-nest thread is labelled by the handle alice addressed it to,
    # where a same-nest one shows the peer's hex (the select refuses an
    # unoffered value loudly, listing what it painted).
    alice_app.custody.mint_offer(f"{bob['handle']}@{custodian['authority']}")
    _poll_devices(
        bob_app,
        lambda: bob_app.custody.offer_count() >= 1
        and bob_app.custody.offer_has_nest_choice(0),
        what="the consent card WITH the nest choice on bob's Devices page",
    )
    bob_app.custody.accept_offer_on_nest(0)

    # ── B's pump takes alice's recovery material: a pass that read all three
    # answers from A and left a custodied escrow blob in the pair store.
    _pump_until(
        rig,
        lambda r: r.get("recovery_pulled", 0) >= 1 and r.get("escrows_custodied", 0) >= 1,
        what="the custodian pump holding alice's seed escrow",
    )

    # ── alice replaces her kit while A still lives; the replace re-seals the
    # escrow at A in the same call, and ONE further pass carries the new head
    # to B (ruling 2: the copy follows the chain).
    alice_app.settings.navigate()
    alice_app.settings.open_recovery_kit()
    alice_app.settings.replace_kit_with_held(first_phrase)
    wait_until(
        lambda: alice_app.settings.recovery_kit_secret() not in ("", first_phrase),
        30.0,
        diagnose=lambda: (
            "the replace never showed its new kit; status reads "
            f"{alice_app.settings.recovery_kit_status()!r}, error surface: "
            f"{alice_app.error_text()!r}"
        ),
    )
    kit = copy_the_shown_kit(alice_app, alice_app.settings.recovery_kit_secret())
    report = _pump_once(custodian)
    assert report.get("recovery_pulled", 0) >= 1 and report.get("escrows_custodied", 0) >= 1, (
        f"the pass after the replace must carry the new head and blob to B: {report}"
    )

    # ── Every device lost, and the home nest gone with them.
    home["stop"]()
    lose_every_device_and_open_restore(alice_app, custodian)
    account = f"{alice['handle']}@{custodian['authority']}"

    # A wrong kit for alice's account reads exactly as an account B has never
    # heard of — ruling 3's one opaque refusal.
    wrong_kit = _refusal_of(alice_app, _with_param(kit, "secret", secrets.token_hex(32)), account)
    unknown = _refusal_of(alice_app, _with_param(kit, "actor", secrets.token_hex(32)), account)
    assert wrong_kit, "a refused restore must say so on error-message"
    assert wrong_kit == unknown, (
        "a wrong kit must be indistinguishable from an account the custodian "
        f"does not hold: wrong kit {wrong_kit!r}, unknown account {unknown!r}"
    )

    # The kit the replace retired no longer opens the copy …
    retired = _refusal_of(alice_app, first_kit, account)
    assert retired == unknown, (
        "the retired kit must read the same opaque refusal at the custodian, "
        f"got {retired!r}"
    )

    # … and the current one, with the custodian's address typed, brings the
    # account back from a nest that never held it.
    alice_app.onboarding.restore_from_recovery_kit(kit, account=account)
    wait_for_restore_landing(alice_app)
    prefilled = alice_app.driver.get_text("handle-input")
    assert prefilled == account, (
        "the account the restore proved ownership of must be carried into "
        f"handle_entry; got {prefilled!r}, wanted {account!r}"
    )
