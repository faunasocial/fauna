"""A sign-out must not strand or delete the machine's device row: sign in, sign
out, sign in again — one actor, N cycles, one row.

Owner: ``docs/goal/architecture/apps/sync-agent-credentials.md`` § Credential
model → the RULED 2026-09-28 one-credential block (bound (c): this test asserts
a roster that never grows) and *The signed-out reconcile* (the nest-side leg).

**The shape asserted.** One credential per (machine, account): the enrollment
registers the store principal's grant on the machine's NAMED row — the app's own
device id — and that row is the only row. A sign-out erases this machine's
per-actor credential slot and retires the enrollment nest-side over
``fauna.sync.device_grant.revoke``, which clears the row's grant columns and
keeps the row; the next sign-in mints a fresh writer key and re-adopts the same
row with a new grant. So across N cycles the roster is exactly the named row,
its grant absent after each sign-out and present — under a new principal — after
each sign-in.

**What went wrong, measured (history).** Until the one-credential shape the
enrollment targeted a writer-pub-hex placeholder row beside the named row, and
until 2026-09-14 a sign-out told the nest nothing, so every sign-out → sign-in
cycle stranded one placeholder; a whole-suite ``--app linux`` sweep, whose
``app`` fixture signs out before every test, walked the shared session actor
into the nest's 64-device tier cap that way. A regression of either kind — a
second row, or a sign-out that deletes the user's device — fails the equality
below.

⚠ **A DEDICATED actor, never the shared ``test_user``** — the counts must be
exact, and the shared actor's roster is the whole session's. ``test_user`` is
still requested: its fixture lifts the device cap on the tier every test user
shares, so a regression grows this roster instead of meeting a two-device
refusal first.

⚠ **Every roster read sits behind a causal barrier, never a settle-sleep**
(convention 14): after a sign-in, the slot's ``grant-registered`` latch naming a
row the nest lists (``helpers.enrollment.await_enrollment``); after a sign-out,
the nest's roster reaching the expected state within a named budget — the
retirement is one round trip the sign-out itself awaits, so a green run pays
nothing.
"""

from __future__ import annotations

import pytest

from helpers import enrollment
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

#: Sign-out → sign-in cycles per run. One would pin the defect; two make "one
#: extra row per cycle" read as a rate rather than a coincidence.
CYCLES = 2

#: The grant-cleared barrier lives beside the other roster reads
#: (`helpers.enrollment`), shared with web's own sign-out journey.
RETIREMENT_VISIBLE_S = enrollment.RETIREMENT_VISIBLE_S
_await_grant_cleared = enrollment.await_grant_cleared


def _assert_one_granted_row(
    nest_url: str, user: dict, named_row: str, latched_row: str, context: str
) -> str:
    """After a sign-in's latch: the latch names the machine's named row, the
    roster is that row alone, and it carries a grant. Returns the principal."""
    assert latched_row == named_row, (
        f"{context}: the enrollment latched on {latched_row}, not the machine's named row "
        f"{named_row} — every host's enrollment targets the app's own id "
        "(fauna_client_account_runtime::resolve_and_start)"
    )
    listed = enrollment.principals(nest_url, user)
    assert set(listed) == {named_row}, (
        f"{context}: the roster should be the machine's named row {named_row} alone, "
        f"and it reads {listed} (device id → principal)"
    )
    principal = listed[named_row]
    assert principal, (
        f"{context}: the named row {named_row} carries no grant behind this sign-in's "
        "enrollment latch"
    )
    return principal


def test_signing_out_and_back_in_keeps_one_roster(app, request, nest_instance, test_user):
    """N sign-out → sign-in cycles, one actor: each sign-out clears the named
    row's grant and keeps the row, and each sign-in re-adopts that row under a
    fresh writer key — a new principal each time, as production does, and never
    a second row."""
    from conftest import _E2E_LOGIN_DEVICE_ID, _make_user

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    # The session patch seeds the app's own device id, and every app adopts it
    # as its real sync identity (`test_device_cards.py`'s marks-this-device
    # premise) — so this is the machine's NAMED row throughout.
    named_row = _E2E_LOGIN_DEVICE_ID

    enrollment.sign_in(app, request, nest_instance, user)
    writer, latched_row, _ = enrollment.await_enrollment(app, nest_url, user)
    principal = _assert_one_granted_row(nest_url, user, named_row, latched_row, "first sign-in")

    for n in range(1, CYCLES + 1):
        context = f"cycle {n} of {CYCLES}"
        app.settings.sign_out()
        _await_grant_cleared(nest_url, user, named_row, context)

        enrollment.sign_in(app, request, nest_instance, user)
        next_writer, next_row, _ = enrollment.await_enrollment(app, nest_url, user)

        assert next_writer != writer, (
            f"{context}: the second sign-in re-used the writer key the sign-out erased — "
            "the credential slot survived a sign-out (long-term-store.md § Cleanup "
            "contract), so this test is no longer exercising the cycle it exists for"
        )
        next_principal = _assert_one_granted_row(nest_url, user, named_row, next_row, context)
        assert next_principal != principal, (
            f"{context}: the named row still names the previous sign-in's principal "
            f"{principal} — the re-adopting sign-in's fresh key did not register its grant"
        )

        writer, principal = next_writer, next_principal


#: Sign-in → immediate reset cycles. Measured 2026-09-20 on linux, every one of
#: these landed inside the assembly before the fix (5 of 5), so three is what a
#: regression shows — a rate, as above, rather than a coincidence.
QUICK_CYCLES = 3


def test_a_sign_out_landing_mid_assembly_still_retires_its_enrollment(
    app, request, nest_instance, test_user
):
    """The sign-out that arrives while the account runtime is still ASSEMBLING.

    The pump's prologue enrolls the machine's row *before* the assembly
    settles, so a sign-out landing in that window supersedes a runtime that
    already owns a grant. Until 2026-09-20 the superseded runtime was shut down
    plainly, ahead of the sign-out's own retirement, which then found the
    runtime gone (``Deferred("account runtime is shut down")``) — under the
    two-row shape, one stranded ``fauna`` placeholder per such sign-out
    (``account-runtime.md`` → *The superseded shutdown is a stop like any
    other*).

    **Why ``driver.reset()`` and not the settings gesture** (convention 8 names
    the UI for a journey's mutations): the window under test is the second after
    sign-in, and walking to Settings → Sign out spends it — the cycle test above
    covers that gesture, settled. The reset is the harness's own per-test
    sign-out, the same ``StopReason::SignOut`` teardown.

    **Latency-independent** (convention 14): whether a given reset lands inside
    the assembly is timing, but the assertion does not depend on it — settled or
    not, the machine keeps one row. The final roster is read behind the last
    sign-in's enrollment latch and must be the named row alone, granted to that
    sign-in; a second row never leaves on its own, so a regression cannot pass
    by being slow. The deterministic pin of the transition is the tier_1 test in
    ``fauna-client-account-runtime``; this case is the end-to-end witness that
    it is wired to a real retirement.
    """
    from conftest import _E2E_LOGIN_DEVICE_ID, _login_app_as, _make_user

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    named_row = _E2E_LOGIN_DEVICE_ID

    # One settled sign-in first: the switch onto the dedicated actor is
    # verified once, and every later login is the same actor after a reset.
    enrollment.sign_in(app, request, nest_instance, user)
    enrollment.await_enrollment(app, nest_url, user)

    for _ in range(QUICK_CYCLES):
        app.driver.reset()
        _login_app_as(app, request, nest_instance, user)
    app.driver.reset()

    enrollment.sign_in(app, request, nest_instance, user)
    _writer, latched_row, _ = enrollment.await_enrollment(app, nest_url, user)

    seen: dict = {}

    def settled():
        listed = enrollment.principals(nest_url, user)
        seen.clear()
        seen.update(listed)
        return set(listed) == {named_row} and bool(listed[named_row])

    wait_until(
        settled,
        RETIREMENT_VISIBLE_S,
        interval=1.0,
        diagnose=lambda: (
            f"after {QUICK_CYCLES} sign-in → immediate-reset cycles the roster reads {seen} "
            f"(device id → principal), which is not the machine's named row {named_row} "
            f"alone, granted (the last sign-in latched on {latched_row}). Any other row "
            "is a second row the one-credential shape rules out; a MISSING named row "
            "means a sign-out deleted the user's device, which it must never do. Grep "
            "the app log for \"the machine's enrollment retirement\"."
        ),
    )
    assert latched_row == named_row, (
        f"the last sign-in's enrollment latched on {latched_row}, not the machine's named "
        f"row {named_row}"
    )


def test_a_sign_in_minting_its_own_device_id_comes_back_to_its_named_row(
    app, request, nest_instance, test_user
):
    """The PRODUCTION get-or-create: no forced device id, N sign-out → sign-in
    cycles, one named row.

    Both cases above force ``_E2E_LOGIN_DEVICE_ID`` through the session patch, so
    the machine's named row is the same one every cycle *by construction* — and
    that blinded them to the defect measured 2026-09-20: a sign-out's scope sweep
    takes the account-scoped ``device.db`` (ratified — ``account-scoping.md``
    § Erasure follows scope), the next sign-in minted a fresh random id, and the
    machine registered a NEW named row while the old one kept its label and
    memberships for ever. One row per cycle; a free tier allows two.

    The ruling (``sync-agent-credentials.md`` § Credential model, 2026-09-20):
    the id is derived from an install-scoped secret and the account, so the
    returning sign-in re-derives the id the sweep erased and comes back to its
    own row. The sweep itself is untouched.

    **The barrier** (convention 14) is the app's own persisted id appearing in
    the nest's roster (``enrollment.await_named_row``), behind the enrollment
    latch — never a roster read on its own, which a regression passes while its
    new row is still in flight.

    Asserted on every app the helper can read back, not only the one the ruling
    changed: an app that already kept its id across a sign-out must keep doing
    so.
    """
    from conftest import _make_user

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)

    def sign_in_unforced() -> tuple[str, str]:
        enrollment.sign_in(
            app, request, nest_instance, user, device_id=enrollment.UNFORCED_DEVICE_ID
        )
        _writer, latched, _ = enrollment.await_enrollment(app, nest_url, user)
        own, _ = enrollment.await_named_row(app, nest_url, user)
        return own, latched

    named_row, latched_row = sign_in_unforced()
    _assert_one_granted_row(nest_url, user, named_row, latched_row, "first sign-in")

    for n in range(1, CYCLES + 1):
        context = f"cycle {n} of {CYCLES}"
        app.settings.sign_out()
        _await_grant_cleared(nest_url, user, named_row, context)

        own, next_row = sign_in_unforced()

        assert own == named_row, (
            f"{context}: the sign-in after a sign-out registered under a NEW device id "
            f"{own}; the machine's named row was {named_row}. The sign-out's scope sweep "
            "erased the persisted id and the get-or-create minted at random instead of "
            "re-deriving it from the install device secret — "
            "`engine_lifecycle::load_device_id_for_actor`."
        )
        _assert_one_granted_row(nest_url, user, named_row, next_row, context)
