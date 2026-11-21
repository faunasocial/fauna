"""Web's `barrier` waits for an in-flight navigation and an in-flight RPC.

Owner of the contract: `fauna_e2e_agent::BARRIER` (convention 14,
`docs/goal/architecture/e2e-conventions.md`); web's mechanism and why it has
these two legs: `apps/fauna-web/src/lib/barrier-e2e.ts`'s header, *The two
completion observables*. The cross-app ordering self-test is
`tests/test_agent_barrier.py`; this file grades only what web adds.

Found by the web actuation sweep (`tests/walk/test_ui_walk_actuation.py`): a
barrier of two macrotask turns let the sweep read the old page after a nav-tab
click (the route's lazy chunk still loading) or a button whose handler awaits an
RPC, and report the affordance as "the frame did not change".

The two fused probes are the GRADING tests, for the reason
`test_agent_barrier.py::test_fused_barrier_probe_orders_the_batch` gives: the
leg is started and barriered inside one command, so no driver round trip can
drain it for free. The RPC hold (300 ms) is far longer than two macrotask
turns, so a barrier without its RPC leg freezes the batch's last value, never
`#rpc`. The nav-tab test is the usage shape the sweep relies on.

⚠ Never add a sleep to this file (convention 14).
"""

import pytest

pytestmark = [pytest.mark.web, pytest.mark.tier_3]

ACK_KEY = "barrier_ack_probe"


def test_fused_barrier_waits_for_an_rpc_in_flight(logged_in_app):
    driver = logged_in_app.driver
    token = "barrier-rpc"
    assert driver.get_state(ACK_KEY) is None, "the reset path clears the ack probe"

    driver.call_command(
        "barrier_probe",
        {"token": token, "count": 4, "barrier": True, "rpc_hold_ms": 300},
    )

    assert driver.get_state(ACK_KEY) == f"{token}#rpc", (
        "a fused barrier must not ack while a WS-RPC call is in flight "
        "(barrier-e2e.ts, the RPC leg): the held call publishes `#rpc` as it "
        "settles, so `#3` means the barrier ran its turns and acked over it"
    )


def test_fused_barrier_waits_for_a_navigation_in_flight(logged_in_app):
    driver = logged_in_app.driver
    token = "barrier-nav"
    assert driver.get_state(ACK_KEY) is None, "the reset path clears the ack probe"

    driver.call_command(
        "barrier_probe",
        {"token": token, "count": 0, "barrier": True, "navigate": "/app/contacts"},
    )

    assert driver.get_state(ACK_KEY) == f"{token}@/app/contacts", (
        "a fused barrier must not ack while a SvelteKit navigation is in flight "
        "(barrier-e2e.ts, the navigation leg): the navigation publishes "
        "`@<path>` once it completes, so None means the barrier acked mid-way"
    )


def test_barrier_after_a_nav_tab_click_reads_the_new_page(logged_in_app):
    driver = logged_in_app.driver
    driver.click("feed-tab", 0)
    driver.barrier()
    before = {r.get("id") for r in (driver.registry_snapshot() or [])}
    assert "new-conversation-button" not in before, (
        "precondition: the Feed page does not paint the Conversations page's "
        f"new-conversation-button; it painted {sorted(i for i in before if i)}"
    )

    driver.click("conversations-tab", 0)
    driver.barrier()

    after = {r.get("id") for r in (driver.registry_snapshot() or [])}
    assert "new-conversation-button" in after, (
        "after a nav-tab click and a barrier the frame must be the NEW page's "
        "(the sweep's whole premise); the registry still read "
        f"{sorted(i for i in after if i)}"
    )


@pytest.mark.parametrize("tab,path", [
    ("conversations-tab", "/app/conversations"),
    ("events-tab", "/app/events"),
    ("profile-tab", "/app/profile"),
    # The page's own tab: the owed navigation must still begin and settle, or
    # the barrier would throw on a click a person makes freely.
    ("feed-tab", "/app/feed"),
])
def test_fused_barrier_waits_for_the_navigation_a_link_click_owes(logged_in_app, tab, path):
    """The router claims a link click inside its dispatch but begins the
    navigation a task or more later (`#lib/navigation-in-flight`, *Why a
    claimed click is owed a navigation*) — the gap in which the actuation
    sweep's barrier read no navigation and acked on the page the click was
    leaving (the web sweep's `*-tab` and `settings-nav-back` settle races).
    Fused, the click and the barrier run in one command, so no driver round
    trip can let the navigation begin for free."""
    driver = logged_in_app.driver
    driver.click("feed-tab", 0)
    driver.barrier()
    token = f"barrier-click-{tab}"
    assert driver.get_state(ACK_KEY) is None, "the reset path clears the ack probe"

    driver.call_command(
        "barrier_probe",
        {"token": token, "count": 0, "barrier": True, "click": tab},
    )

    assert driver.get_state(ACK_KEY) == f"{token}@{path}", (
        f"a fused barrier must not ack between a {tab} click the router claimed "
        "and the navigation it begins (barrier-e2e.ts, the navigation leg): the "
        f"navigation publishes `@{path}` once it settles, so None means the "
        "barrier acked before the navigation had even begun"
    )


def test_fused_barrier_waits_for_a_conversations_manager_call(logged_in_app):
    """A handler that awaits the wasm conversations manager (the nostr link's
    `confirmNostrNpub`) does its nest work outside `#lib/rpc`'s counted call;
    the manager a page gets counts its methods' promises
    (`#lib/rpc-in-flight`'s `countedFace`), so the barrier waits for them."""
    driver = logged_in_app.driver
    token = "barrier-manager"
    assert driver.get_state(ACK_KEY) is None, "the reset path clears the ack probe"

    driver.call_command(
        "barrier_probe",
        {"token": token, "count": 4, "barrier": True, "manager_hold_ms": 300},
    )

    assert driver.get_state(ACK_KEY) == f"{token}#manager", (
        "a fused barrier must not ack while a conversations-manager call a "
        "handler awaits is in flight (barrier-e2e.ts, the RPC leg's manager "
        "half): the held call publishes `#manager` as it settles, so `#3` means "
        "the barrier acked over it, and `!manager failed` that no manager built"
    )
