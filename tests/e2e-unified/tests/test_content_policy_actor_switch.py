"""A guardian content floor set on one actor must not survive an actor switch,
performed WHILE the feed route is open, to an unsupervised account.

This is the `lib/contentPolicy.svelte.ts` twin of the conversations-page bug closed: ``account-scoping.md`` § The scoping taxonomy → *The
switch/sign-out isolation contract* — "no account-scoped datum may be read or
written by a session authenticated as a different account" — and *In-memory state is
account-scoped too* — "the failure is silent by construction — stale actor-scoped
state renders as empty or plausible, never as an error — so it needs a red-first
test at the switch, not an inspection."

``lib/actorScope.ts`` already migrated this module onto the shared seam, and ``routes/feed/+page.svelte``'s ``onActorChange``
handler calls ``hydrateContentPolicy`` for the incoming actor on every switch — so in
the settled state this module is self-correcting even without the reset (an
unsupervised actor's own ``fauna.family.status`` read succeeds with ``policy: null``,
it does not throw). What this test actually pins is the end-to-end wiring: the feed
route's actor-change handler must keep calling ``hydrateContentPolicy`` for whoever is
now signed in, on a same-route switch that never remounts the page. Removing that call
(e.g. "optimizing" hydration back into a one-shot ``onMount``, the exact shape of bug
this seam exists to prevent — see ``account-scoping.md``'s in-memory corollary) would
leave the OUTGOING actor's guardian floor blocking the incoming, unsupervised actor's
own content — the "fail-open direction" the module's own `resetContentPolicy` comment
calls "the dangerous one", because it is silent and reads as a product bug, not a
switch bug.

**Why the label category is `commercial`, not `spam`.** `spam`/`phishing` are also
governed by every user's own default collapse threshold
(`libs/fauna-core/src/obligation.rs`'s `rules_from_own_thresholds` — every user gets
one, guardian or not, `GUARDIAN_FLOOR_TRIGGER_PERMILLE` == the DB's own-threshold
default, both 500‰), so a high-confidence spam label collapses for ANY actor
independent of any guardian floor — a first draft of this test used `spam` and got a
false failure from exactly this (the post correctly stopped being *blocked* after the
switch, but still didn't render, because actor B's own default threshold collapsed it
for an unrelated reason). `commercial` is one of the four
``GUARDIAN_FLOOR_CATEGORIES`` with **no** per-user threshold rule at all
(`SpamPreferences` only carries `spam_threshold`/`phishing_threshold`) — a
`commercial`-labeled post is untouched for an unsupervised actor with no guardian
relationship, so a block surviving the switch can only mean the seam leaked the
outgoing ward's floor.

The label is a *precondition* injected via the feed test seam (fixture setup, not the
action under test), the same shape ``test_family.py``'s content-floor tests use —
feed labels ride the wire from the nest's ``content_labels`` projection, empty on an
encrypted nest, so the inject seam is the only way to stage one in tier_3.
"""

import time

import pytest

from common.auth import create_actor_and_register
from tests.test_family import _admit_own_pair

pytestmark = [pytest.mark.tier_3]

# Generous, latency-independent budget (convention 14): a switch drops the seam's
# state and the incoming actor's hydrate is a single RPC round trip.
ACTOR_SWITCH_WAIT_S = 20.0


def _switch_to(app, request, nest, user, *, handle: str) -> None:
    """``set_state`` login as ``user`` landing on the feed view.

    Naming the route the app is ALREADY on (``feed``) is the forcing function:
    SvelteKit resolves that to a same-route ``goto``, so the page does not remount
    and any actor-scoped module state survives the switch unless something actively
    drops/rebuilds it — mirrors ``test_conversations_actor_switch.py``'s
    ``_switch_to``.
    """
    node_url = request.getfixturevalue("spa_url") if app.driver.is_web() else nest["url"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node_url,
            "secret_hex": bytes(user["signing_key"]).hex(),
            "handle": handle,
            "actor_id": user["actor_id_hex"],
            "device_id": "test-device-cp-switch",
        },
        "nav": {"stack": [{"view": "feed"}]},
    })


@pytest.mark.web
@pytest.mark.feature("multiple-accounts")
def test_actor_switch_on_the_feed_route_does_not_leak_the_outgoing_wards_content_floor(
    admin_app, request, nest_instance
):
    """Switching FROM a ward with a `block` commercial floor TO an unsupervised
    actor, while ON /app/feed, must not carry the ward's floor into the new actor's
    render.

    Asserts latency-independent state (convention 14): the observable is whether a
    commercial-labeled post renders, deadline-polled to a generous budget — never a
    sleep.
    """
    app = admin_app
    app.family.require_content_render_verdict_supported()

    # Its own pair, and it has to be: `test_family`'s shared `family_pair` fixture is
    # module-local to that file, so a test in this module cannot take it. One
    # anonymous invite-request submit (the ward's; the guardian is direct-admitted
    # over the admin path), which is what keeps this module inside the release
    # image's shipped submit budget — testing.md § Default app and nest mode.
    guardian_identity, ward_identity, _guardian_handle, ward_handle = _admit_own_pair(
        nest_instance, "cpswitch"
    )

    # ── Guardian sets the ward's commercial content floor to `block` (real UI
    # mutation) ─────────────────────────────────────────────────────────────────
    from conftest import _login_app_as

    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and ward_handle not in app.family.ward_handles():
        time.sleep(0.5)
        app.family.reload()
    assert ward_handle in app.family.ward_handles(), (
        f"expected ward {ward_handle!r} in the guardian's list. error: {app.error_text()!r}"
    )
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.policy_editor_visible(), (
        f"policy editor did not load. error: {app.error_text()!r}"
    )
    app.family.set_content_floor("commercial", "block")
    app.family.save_policy()
    assert not app.has_error(), f"save_policy raised: {app.error_text()!r}"

    # ── Baseline: the ward's own feed blocks a commercial-labeled post. ────────
    # A red here is a broken baseline, never the regression under test (convention 6).
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.driver.wait_for("supervised-indicator", timeout=20)

    count = app.feed.seed_posts([
        {
            "post_id": "cpswitch-a-commercial",
            "author": "anadvertiser",
            "body": "actor A's floor should block this",
            "labels": [{"category": "commercial", "confidence_per_mille": 900}],
        },
    ])
    assert count == 1, f"expected 1 seeded post-card, got {count}. error: {app.error_text()!r}"
    assert app.driver.is_visible("content-policy-blocked-notice", scope="post-card[0]"), (
        f"baseline: the ward's own commercial-labeled post must be blocked before "
        f"the switch is exercised. error={app.error_text()!r} "
        f"{app.driver.diagnose('content-policy-blocked-notice')}"
    )

    # ── Switch WITHOUT leaving the feed route to a fresh, unsupervised actor. ──
    other = create_actor_and_register(
        nest_instance["port"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    assert other["actor_id_hex"].lower() != ward_identity["actor_id_hex"].lower(), (
        "the two actors must differ"
    )
    _switch_to(app, request, nest_instance, other, handle="e2e-cp-switch-b")

    # ── The unsupervised actor's own commercial-labeled post must render clean. ─
    # Pre-fix (hydrate not re-wired on switch) this would still render blocked,
    # under the OUTGOING ward's stale `block` floor — the fail-open direction the
    # module's own comment names as the dangerous one. `commercial` has no
    # per-user own-threshold rule, so there is no OTHER mechanism that could
    # legitimately hide this post for an unsupervised actor.
    count_b = app.feed.seed_posts([
        {
            "post_id": "cpswitch-b-commercial",
            "author": "anadvertiser2",
            "body": "actor B has no guardian floor; this must render",
            "labels": [{"category": "commercial", "confidence_per_mille": 900}],
        },
    ])
    assert count_b == 1, f"expected 1 seeded post-card for B, got {count_b}. error: {app.error_text()!r}"

    deadline = time.monotonic() + ACTOR_SWITCH_WAIT_S
    blocked = True
    while time.monotonic() < deadline:
        blocked = app.driver.is_visible("content-policy-blocked-notice", scope="post-card[0]")
        if not blocked:
            break
        time.sleep(0.25)
    assert not blocked, (
        "actor B's own commercial-labeled post is still blocked after an actor "
        "switch performed while the feed route was open — the outgoing ward's "
        "guardian content floor leaked into the incoming, unsupervised actor's "
        f"render (account-scoping.md's switch/sign-out isolation contract). "
        f"error={app.error_text()!r} {app.driver.diagnose('content-policy-blocked-notice')}"
    )
    assert app.driver.count("feed-post-text", scope="post-card[0]") >= 1, (
        "actor B's clean-for-them post should render its body once unblocked"
    )
