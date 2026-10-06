"""A mid-accrual actor switch must not let the outgoing ward's still-pending
Guardian Notify counts leak into the incoming ward's own report.

`lib/familyNotify.ts`'s `NotifyBuffer` is a module-level singleton keyed by
`secretHex`: it accrues per-category enforcement counts in `pending` as the ward's
own feed renders guardian-floor-blocked posts, and flushes them to
`fauna.family.notify_report` on its own cadence. `account-scoping.md` § The scoping
taxonomy → *The switch/sign-out isolation contract* — "no account-scoped datum may
be read or written by a session authenticated as a different account" — binds this
exactly as it binds `contentPolicy.svelte.ts` and the media/conversations managers:
an actor switch performed WHILE something is still pending (accrued but not yet
flushed) must drop the outgoing ward's bucket, never carry it into the incoming
ward's own signed report. `familyNotify.ts` already registers
`resetForActorChange` via the shared `lib/actorScope.ts` seam (module load time),
so in the settled state this is self-correcting — what this test pins is the
end-to-end wiring, the same shape as `test_content_policy_actor_switch.py` and
`test_media_actor_switch.py`: a regression that dropped or
reordered the reset registration would leave both halves' own unit coverage green
while this exact cross-actor path silently mis-attributed one ward's flagged
content to another's account.

**Why this needed a new e2e command first.** The buffer's first flush is EAGER
(no prior flush), so beating a live 5s `CHECK_INTERVAL_MS` wall-clock timer from an
e2e test is exactly what testing.md convention 14 calls defunct: a multi-step test
(WS-RPC round trips, page loads) routinely takes longer than 5s between ward A's
post rendering and the switch step, so the real timer could flush ward A's report
for real, under ward A's own (correct) identity, before the test ever reaches the
"still pending" moment it means to exercise — a nondeterministic precondition, not
a nondeterministic verdict, but just as unusable. `familyNotify.ts`'s `ensureTimer()`
now skips arming the real interval under the Playwright e2e agent (mirroring
`conversations.ts`'s `pollIntervalMs()` "hasAgent" idiom); the new
`family_notify_check_now` e2e command (`$lib/family-notify-e2e.ts`, mirroring
`backup-audit-e2e.ts`'s `backup_audit_run_now`) is the only thing that ever checks
during a test, so this test controls exactly when (and whether) a flush happens.

**Why the two wards' floor categories differ (commercial vs nsfw).** Both are
`GUARDIAN_FLOOR_CATEGORIES` with no per-user own-threshold rule (only spam/phishing
have one — `SpamPreferences`), so neither post is at risk of being collapsed for an
unrelated reason (the `test_content_policy_actor_switch.py` `commercial`-not-`spam`
finding). Using two DIFFERENT categories makes contamination directly legible in the
guardian's own readout text: if the incoming ward B's report ever contained the
outgoing ward A's `commercial` count, it would show up verbatim next to B's own
`nsfw` count.
"""

import time

import pytest

from i18n.strings import S
from tests.test_family import _admit_adult_directly, _admit_ward, _unique_suffix

pytestmark = [pytest.mark.tier_3]

# Generous, latency-independent budget (convention 14): a switch drops the seam's
# state and the incoming actor's hydrate + a single flush RPC round trip.
NOTIFY_READOUT_WAIT_S = 40.0


def _admit_guardian_and_two_wards(
    nest_instance, guardian_handle: str, ward_a_handle: str, ward_b_handle: str
) -> tuple[dict, dict, dict]:
    """One guardian, two independently-admitted wards — the two-actor twin of
    `test_family._admit_own_pair`.

    TWO anonymous invite-request submits, and both are structural: this test's
    whole subject is the seam between two DIFFERENT supervised accounts, so the
    wards cannot be shared or collapsed. The guardian is direct-admitted over
    the admin path instead (`_admit_adult_directly`), which is what brings the
    cost down from three — the shipped-budget fit rule, `testing.md` § Default
    app and nest mode."""
    guardian_identity = _admit_adult_directly(nest_instance, guardian_handle)
    ward_a_identity = _admit_ward(nest_instance, guardian_identity, ward_a_handle)
    ward_b_identity = _admit_ward(nest_instance, guardian_identity, ward_b_handle)
    return guardian_identity, ward_a_identity, ward_b_identity


def _switch_to(app, request, nest, user, *, handle: str) -> None:
    """`set_state` login as `user` landing on the feed view — same-route `goto`,
    no remount (mirrors `test_content_policy_actor_switch.py`'s `_switch_to`)."""
    node_url = request.getfixturevalue("spa_url") if app.driver.is_web() else nest["url"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node_url,
            "secret_hex": bytes(user["signing_key"]).hex(),
            "handle": handle,
            "actor_id": user["actor_id_hex"],
            "device_id": "test-device-fn-switch",
        },
        "nav": {"stack": [{"view": "feed"}]},
    })


@pytest.mark.web
@pytest.mark.linux
# tui added 2026-08-21: a single-seat sequential-login test (one
# admin_app, repeated driver.reset()+login), not a two-seat scenario. The
# action gate (`require_guardian_notify_client_supported`) already lists tui
# as built (2026-08-02, content_policy.rs's NotifyAccumulator) — this marker
# set had simply stopped growing after web+linux landed first.
@pytest.mark.tui
@pytest.mark.feature("family-safety")
def test_actor_switch_mid_accrual_does_not_leak_the_outgoing_wards_pending_notify_count(
    admin_app, request, nest_instance
):
    """Ward A flags a `commercial`-labeled post (accrues, never flushed) and
    switches, WITHOUT leaving the feed route, to ward B before any flush. Ward B
    then flags an `nsfw`-labeled post and the check is forced once. The guardian's
    readout must show exactly B's own `nsfw` count — never A's `commercial` count,
    and never a second notice row.
    """
    app = admin_app
    app.family.require_guardian_notify_client_supported()

    # A random tail, not a fixed literal: the nest is session-scoped, so a
    # fixed handle collides with itself the moment this module runs under a
    # second app parametrization (`test_family._handles`).
    suffix = f"notifysw-{_unique_suffix()}"
    guardian_handle = f"family-guardian-{suffix}"
    ward_a_handle = f"family-warda-{suffix}"
    ward_b_handle = f"family-wardb-{suffix}"
    guardian_identity, ward_a_identity, ward_b_identity = _admit_guardian_and_two_wards(
        nest_instance, guardian_handle, ward_a_handle, ward_b_handle
    )

    # ── Guardian: both wards get a `block` floor + Notify on, different
    # categories (commercial for A, nsfw for B) so contamination is legible ──────
    from conftest import _login_app_as

    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and (
        ward_a_handle not in app.family.ward_handles()
        or ward_b_handle not in app.family.ward_handles()
    ):
        time.sleep(0.5)
        app.family.reload()
    handles = app.family.ward_handles()
    assert ward_a_handle in handles and ward_b_handle in handles, (
        f"expected both wards in the guardian's list, got {handles!r}. "
        f"error: {app.error_text()!r}"
    )
    for handle, category in ((ward_a_handle, "commercial"), (ward_b_handle, "nsfw")):
        app.family.select_ward_by_handle(handle)
        assert app.family.policy_editor_visible(), (
            f"policy editor did not load for {handle!r}. error: {app.error_text()!r}"
        )
        app.family.set_content_floor(category, "block")
        app.family.set_content_notify(True)
        app.family.save_policy()
        assert not app.has_error(), f"save_policy raised for {handle!r}: {app.error_text()!r}"

    # ── Ward A: renders a commercial-labeled post; it accrues but is NEVER
    # flushed — no poke is issued before the switch. ─────────────────────────────
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_a_identity)
    app.driver.wait_for("supervised-indicator", timeout=20)

    count_a = app.feed.seed_posts([
        {
            "post_id": "fnswitch-a-commercial",
            "author": "anadvertiser",
            "body": "ward A's floor should block this",
            "labels": [{"category": "commercial", "confidence_per_mille": 900}],
        },
    ])
    assert count_a == 1, f"expected 1 seeded post-card for A, got {count_a}. error: {app.error_text()!r}"
    assert app.driver.is_visible("content-policy-blocked-notice", scope="post-card[0]"), (
        "baseline: ward A's own commercial-labeled post must be blocked (the "
        f"enforcement event Notify accrues from). error={app.error_text()!r}"
    )

    # ── Switch WITHOUT leaving the feed route, straight to ward B — no poke, no
    # sleep long enough for a real timer to matter (there is none armed). ───────
    _switch_to(app, request, nest_instance, ward_b_identity, handle=ward_b_handle)

    # ── Ward B: renders its own nsfw-labeled post (independent enforcement). ────
    count_b = app.feed.seed_posts([
        {
            "post_id": "fnswitch-b-nsfw",
            "author": "anotheruser",
            "body": "ward B's own floor should block this",
            "labels": [{"category": "nsfw", "confidence_per_mille": 900}],
        },
    ])
    assert count_b == 1, f"expected 1 seeded post-card for B, got {count_b}. error: {app.error_text()!r}"
    assert app.driver.is_visible("content-policy-blocked-notice", scope="post-card[0]"), (
        "ward B's own nsfw-labeled post must be blocked under B's own floor "
        f"(post-switch). error={app.error_text()!r}"
    )

    # ── Force the ONE check that will ever run under this agent (run_now poke,
    # testing.md convention 14) — ward B's report, if the reset ran, contains only
    # B's own nsfw count. ─────────────────────────────────────────────────────────
    app.driver.call_command("family_notify_check_now", timeout=20)
    assert app.error_text() == "", f"the check-now command was not honoured: {app.error_text()!r}"

    # ── Guardian: exactly ONE notice row (ward A's report never fired), and it
    # must be B's own nsfw count — never A's commercial count. ──────────────────
    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    nsfw_label = S.family.policy_content_nsfw_label
    commercial_label = S.family.policy_content_commercial_label
    count_text = S.family.ward_content_notice_count(count="1")
    deadline = time.monotonic() + NOTIFY_READOUT_WAIT_S
    readout = ""
    notice_rows = 0
    while time.monotonic() < deadline:
        app.family.reload()
        notice_rows = app.driver.count("family-ward-content-notices")
        if notice_rows >= 1:
            readout = app.family.ward_content_notices_text(0)
            if nsfw_label in readout and count_text in readout:
                break
        time.sleep(0.5)
    assert nsfw_label in readout and count_text in readout, (
        f"the guardian's Notify readout is missing ward B's own nsfw count. got "
        f"{readout!r}; expected to contain {nsfw_label!r} and {count_text!r}. "
        f"error: {app.error_text()!r}"
    )
    assert commercial_label not in readout, (
        "ward A's still-pending commercial count leaked into ward B's own notify "
        f"report after a mid-accrual actor switch — got {readout!r} "
        "(account-scoping.md's switch/sign-out isolation contract)"
    )
    assert notice_rows == 1, (
        f"expected exactly one notice row (ward A's report must never fire — its "
        f"pending count was dropped, not flushed), got {notice_rows}"
    )
