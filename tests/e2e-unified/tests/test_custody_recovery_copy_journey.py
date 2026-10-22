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
from urllib.parse import parse_qsl, urlencode, urlsplit

import pytest

from helpers.custody_rig import mint_nest_custody, pump_once, pump_until
from helpers.recovery_restore import (
    RESTORE_OUTCOME_S,
    copy_the_shown_kit,
    lose_every_device_and_open_restore,
    wait_for_restore_landing,
)
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3


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

    # ── The custody, through both UIs: bob takes it on HIS NEST, so the
    # hosting row lands on B.
    mint_nest_custody(rig)

    # ── B's pump takes alice's recovery material: a pass that read all three
    # answers from A and left a custodied escrow blob in the pair store.
    pump_until(
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
    report = pump_once(custodian)
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
