"""family-safety outcome 68 — once a child graduates their app stops enforcing
any leftover guardian policy, and switching accounts never blanks or carries
over a child's limits or supervised mark.

`docs/goal/behavior/family-client-enforcement.md` § Content policy and
`docs/goal/behavior/family-safety.md` § Graduation & transfer: guardian policy
binds a SUPERVISED account only. Two ways it can go wrong on the child's
device, both pinned here:

1. **Carry-over / blanking across an account switch** (`account-scoping.md`
   § The switch/sign-out isolation contract). Switching from the child to an
   adult on the same app must not leave the child's floor or supervised mark on
   the adult; switching back must bring both back, never an empty policy.
2. **Leftover enforcement after graduation.** The ward's app keeps a persisted
   policy snapshot so a policy it cannot fetch still binds (the unfetched-policy
   ruling). Once the link is gone, that snapshot must stop binding the moment
   the app learns it — a graduated account seeing its old floor would be the
   guardian's authority outliving the link.

The floor is arranged over the wire as the guardian's editor would send it
(`_set_ward_content_floor`), and graduation is resolved from the admin side
(`fauna.family.graduate` accepts the guardian or the admin) while the child's
own launch is pinned across a relaunch — the guardian's graduate gesture is
`test_family_guardian_sees_ward`'s subject, and driving it here would need the
same app instance the child's leftover snapshot lives in. What is under test is
what the CHILD's app renders.
"""

import pytest

from common.launch_harness import reached_authenticated_app
from helpers.admin_wire import admit_adult, graduate_ward
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import wait_until
from tests.test_family import (
    _admit_own_pair,
    _require_a_relaunch_that_keeps_the_session,
    _set_ward_content_floor,
    _unique_suffix,
)

pytestmark = pytest.mark.tier_3

# A relaunch re-reads `fauna.family.status` after its own reconnect; generous
# (convention 14), a green run pays only the poll that succeeds.
RELAUNCH_STATUS_BUDGET_S = 90.0


def _switch_to(app, request, nest, user: dict, handle: str) -> None:
    """`set_state` login as ``user`` on the feed view — no remount, the
    same seam `test_family_notify_actor_switch.py` switches through."""
    node_url = request.getfixturevalue("spa_url") if app.driver.is_web() else nest["url"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node_url,
            "secret_hex": bytes(user["signing_key"]).hex(),
            "handle": handle,
            "actor_id": user["actor_id_hex"],
            "device_id": "test-device-grad-switch",
        },
        "nav": {"stack": [{"view": "feed"}]},
    })


def _seed_flagged(app, post_id: str) -> None:
    count = app.feed.seed_posts([
        {
            "post_id": post_id,
            "author": "anadvertiser",
            "body": f"commercial body {post_id}",
            "labels": [{"category": "commercial", "confidence_per_mille": 900}],
        },
    ])
    assert count == 1, f"expected 1 seeded post-card, got {count}. error: {app.error_text()!r}"


def _indicator_shown(app) -> bool:
    return app.driver.count("supervised-indicator") > 0


@pytest.mark.feature("family-safety")
def test_family_graduation_stops_leftover_enforcement_and_a_switch_never_carries_limits(
    admin_app, request, nest_instance
):
    from conftest import _login_app_as

    app = admin_app
    app.family.require_content_render_verdict_supported()
    # Own pair: graduation ends the pairing, which the module's shared pair
    # cannot survive.
    guardian_identity, ward_identity, guardian_handle, ward_handle = _admit_own_pair(
        nest_instance, "gradenf"
    )
    adult_handle = f"family-adult-{_unique_suffix()}"
    adult_identity = admit_adult(nest_instance, adult_handle)
    _set_ward_content_floor(nest_instance, guardian_identity, ward_identity, "commercial", "block")

    # ── The child: supervised, and the floor binds. ──
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.driver.wait_for("supervised-indicator", timeout=20)
    _seed_flagged(app, "grad-ward-1")
    wait_until(
        lambda: app.family.content_notice_blocked("post-card[0]"),
        UI_SETTLE_S,
        diagnose=lambda: (
            "precondition: the supervised child's commercial floor must block the commercial post; "
            f"{app.driver.diagnose('content-policy-blocked-notice')}"
        ),
    )

    # ── Switch to the adult: no carry-over. ──
    _switch_to(app, request, nest_instance, adult_identity, adult_handle)
    wait_until(
        lambda: not _indicator_shown(app),
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            "the child's supervised mark carried over to the adult after a switch: "
            f"{app.driver.diagnose('supervised-indicator')}"
        ),
    )
    _seed_flagged(app, "grad-adult-1")
    assert not app.family.content_notice_blocked("post-card[0]"), (
        "the child's commercial floor carried over to the adult after an account switch "
        f"({app.driver.diagnose('content-policy-blocked-notice')})"
    )
    assert app.driver.count("feed-post-text", scope="post-card[0]") >= 1, (
        "the adult's commercial post must render its body — no floor applies to them"
    )

    # ── Switch back to the child: never blanked. ──
    _switch_to(app, request, nest_instance, ward_identity, ward_handle)
    wait_until(
        lambda: _indicator_shown(app) and guardian_handle in (
            app.driver.get_text("supervised-indicator") or ""
        ),
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            "switching back to the child must restore the supervised mark naming the "
            f"guardian; {app.driver.diagnose('supervised-indicator')}"
        ),
    )
    _seed_flagged(app, "grad-ward-2")
    wait_until(
        lambda: app.family.content_notice_blocked("post-card[0]"),
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            "switching back to the child blanked their commercial floor — the commercial post "
            f"rendered; {app.driver.diagnose('content-policy-blocked-notice')}"
        ),
    )

    # ── Graduation, then a relaunch of the child's own launch: the persisted
    # snapshot must stop binding once the app learns the link is gone. ──
    _require_a_relaunch_that_keeps_the_session(app)
    graduate_ward(nest_instance, ward_identity["actor_id_hex"])
    assert app.driver.recover(), "the app did not come back up after the relaunch"
    reached_authenticated_app(app.driver, timeout=90)
    wait_until(
        lambda: not _indicator_shown(app),
        RELAUNCH_STATUS_BUDGET_S,
        interval=1.0,
        diagnose=lambda: (
            "after graduation the child's app still shows the supervised mark: "
            f"{app.driver.diagnose('supervised-indicator')} error: {app.error_text()!r}"
        ),
    )
    app.driver.navigate_to("feed")
    _seed_flagged(app, "grad-after-1")
    assert not app.family.content_notice_blocked("post-card[0]"), (
        "a graduated account's app still enforced the leftover commercial floor "
        f"({app.driver.diagnose('content-policy-blocked-notice')})"
    )
    assert app.driver.count("feed-post-text", scope="post-card[0]") >= 1, (
        "the graduated child's commercial post must render its body"
    )
