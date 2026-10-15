"""Settings → Sessions: list → revoke one → sign out everywhere else, through
the app UI, with the nest's own session list as the outside witness.

Owner: ``docs/goal/ui/sessions.md`` (§ Layout & flow, § User actions, § Done
definition box 2 — the first tier_3 journey) over
``docs/goal/behavior/devices.md`` § Session Management.

Arrangement: a dedicated actor signs the app in, and the test mints more
sessions for that actor over the wire — two stranger app sign-ins
(``fauna.auth.*`` with the actor's own key, ``WsRpcAdminClient``), and two
device sign-ins (``fauna.auth.device_handshake``, ``helpers.enrollment.
device_sign_in``) from one granted device row: one by the key the roster names
now ("Device: {name}") and one by the key that row carried before a re-grant
displaced it ("a device key not in your device list" — the neutral third kind,
never an error; ``ui/sessions.md`` § Errors & edge cases).
Every act goes through the UI (convention 8); every verdict is read back from
the nest's ``fauna.sessions.list`` (convention 5). The page's painted
token ids (the app's ``settings.session_token_ids`` state) bridge a card index
to the nest's ids — a plain read of the held snapshot, never a timing guess.

⚠ Each witness read opens its own client and so mints a session of its own;
those ids accumulate in ``harness_ids`` and are never mistaken for the app's
(the ``test_onboarding_launch_routing_smoke`` case M lesson).

What the journey witnesses on the catalog page ``docs/features/sessions.md``:
1 (list, revoke one, sign out everywhere else), 2 (the three kinds), 3 (times
and address against the nest's own row), 4 (the order), 7 (revoke one asks
nothing; the two-press confirm cancels), 8 and 9 (the revoke note), 13–15 (the
lock's warning, painted before anything is typed), 16 (the literal word; the
wrong one is refused out loud and locks nothing) and 28 (a failed act paints
its error and keeps the rows). The failed-READ arm of 28 has no UI-reachable
trigger short of taking the nest away; it is pinned by tui's unit test
``settings::sessions::tests::a_failed_reread_keeps_the_painted_rows``.

The second journey witnesses 5 (this app's own earlier, unexpired sign-ins fold
into its one row) and 6 (sign out everywhere else keeps this app signed in
although it renewed its sign-in after the page was painted). The renewal is the
cross-app ``launch_refresh_token`` agent command — the app's production refresh
of the bearer it holds, which keeps the predecessor live nest-side
(``helpers.launch_token``); the app's own ids are its ``launch_token`` state.
"""
from __future__ import annotations

import os
import re
from datetime import datetime

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.app_surface import app_name, skip_unbuilt
from helpers.enrollment import device_sign_in, grant_device_principal, register_device
from helpers.connection import wait_until_online
from helpers.launch_token import own_session_ids, renew_own_sign_in
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

#: The page's act → re-read → re-fold, plus the nest applying the revoke.
#: Generous by design (convention 14); a green run pays only real latency.
ACT_FOLDED_S = 60.0

#: The device row whose two keys sign in — its name is what the kind says.
DEVICE_LABEL = "e2e-sessions-device"

#: `fauna_core::format::format_unix_local`'s shape.
_LOCAL_TIME = r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}"


def _require_sessions_page(driver) -> None:
    """Convention 7: tui is the lead app (built first); the other six follow as
    one batched trickle-down row — linux, web and android have landed."""
    if app_name(driver) not in ("tui", "linux", "web", "android"):
        skip_unbuilt(
            driver,
            surface="sessions page",
            detail="tui leads; this app renders the shared sessions_view fold in its trickle-down leg",
            tracked="docs/goal/ui/sessions.md § Implementation status today",
        )


def _client(nest_url: str, user: dict) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest_url, bytes.fromhex(user["actor_id_hex"]), bytes(user["signing_key"])
    )


def _mint_stranger_session(nest_url: str, user: dict) -> str:
    """A second sign-in of this account from somewhere else — its session id."""
    with _client(nest_url, user) as client:
        return client.own_token_id


def _listed_rows(nest_url: str, user: dict, harness_ids: set[str]) -> dict[str, dict]:
    """The actor's live sessions as the NEST lists them, by token id."""
    with _client(nest_url, user) as client:
        harness_ids.add(client.own_token_id)
        return {s["token_id"]: s for s in client.call("fauna.sessions.list", {})["sessions"]}


def _listed(nest_url: str, user: dict, harness_ids: set[str]) -> set[str]:
    """The actor's live session ids as the NEST lists them."""
    return set(_listed_rows(nest_url, user, harness_ids))


def _end_behind_the_page(nest_url: str, user: dict, token_id: str) -> None:
    """Another of the account's sign-ins ends ``token_id`` — arrangement for
    the failed act (28), never the act under test."""
    with _client(nest_url, user) as client:
        client.call("fauna.sessions.revoke", {"token_id": token_id})


def _detail_pattern() -> re.Pattern[str]:
    """``session-detail`` as a pattern built from the catalog string itself, so
    the copy can change without this test silently matching nothing."""
    marks = {k: f"@@{k}@@" for k in ("created", "last_used", "expires", "address")}
    template = re.escape(S.sessions.detail(**marks))
    for k, mark in marks.items():
        group = f"(?P<{k}>.+)" if k == "address" else f"(?P<{k}>{_LOCAL_TIME})"
        template = template.replace(re.escape(mark), group)
    return re.compile(f"^{template}$")


def _parse_detail(text: str) -> dict:
    m = _detail_pattern().match(text)
    assert m, f"session-detail does not carry the times and address: {text!r}"
    parsed: dict = {
        k: datetime.strptime(m[k], "%Y-%m-%d %H:%M") for k in ("created", "last_used", "expires")
    }
    parsed["address"] = m["address"]
    return parsed


@pytest.mark.feature("sessions")
def test_list_revoke_one_then_sign_out_everywhere_else(app, request, nest_instance, test_user):
    from conftest import _login_app_as, _make_user

    driver = app.driver
    _require_sessions_page(driver)
    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    actor_id = bytes.fromhex(user["actor_id_hex"])
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)

    stranger_a = _mint_stranger_session(nest_url, user)
    stranger_b = _mint_stranger_session(nest_url, user)
    # One device row, two keys: the first signs in, then a re-grant displaces
    # it, so its sign-in outlives its place in the roster.
    device_id = register_device(nest_url, user, DEVICE_LABEL)
    displaced_key, current_key = SigningKey(os.urandom(32)), SigningKey(os.urandom(32))
    grant_device_principal(nest_url, user, device_id, principal_seed=bytes(displaced_key))
    displaced_session = device_sign_in(nest_url, actor_id, displaced_key)["token_id"]
    grant_device_principal(nest_url, user, device_id, principal_seed=bytes(current_key))
    device_session = device_sign_in(nest_url, actor_id, current_key)["token_id"]
    harness_ids: set[str] = set()

    # ── 1. The list ────────────────────────────────────────────────────────
    app.sessions.navigate()
    painted = app.sessions.painted_token_ids()
    assert painted, f"the page folded no rows (error: {app.sessions.error_text()!r})"
    assert app.sessions.card_count() == len(painted), (app.sessions.card_count(), painted)
    marks = [app.sessions.mark(i) for i in range(len(painted))]
    kinds = [app.sessions.kind(i) for i in range(len(painted))]
    shown = f"painted={painted}, kinds={kinds}, marks={marks}"
    assert marks[0] == S.sessions.mark_this_app, f"card 0 must be this app's own row; {shown}"
    assert not app.sessions.has_revoke_button(0), "this app's own row must carry no revoke"
    own_id = painted[0]
    for other in (stranger_a, stranger_b, device_session, displaced_session):
        assert other in painted[1:], f"a sign-in {other} is not listed; {shown}"
        assert app.sessions.has_revoke_button(painted.index(other))

    # ── 2. What each sign-in says it is (outcome 2) ───────────────────────
    assert kinds[painted.index(stranger_a)] == S.sessions.kind_app, shown
    assert kinds[painted.index(device_session)] == S.sessions.kind_device(name=DEVICE_LABEL), (
        f"a sign-in by a device in the roster must name the device; {shown}"
    )
    assert kinds[painted.index(displaced_session)] == S.sessions.kind_unknown_device, (
        f"a sign-in by a key the roster does not name gets the neutral third kind; {shown}"
    )
    assert app.sessions.error_text() == "", (
        "an unknown device key is shown plainly, never as an error: "
        f"{app.sessions.error_text()!r}"
    )

    # ── 3. Times and address, against the nest's own row (outcome 3) ──────
    rows = _listed_rows(nest_url, user, harness_ids)
    for i, token_id in enumerate(painted):
        detail = _parse_detail(app.sessions.detail(i))
        assert detail["created"] <= detail["last_used"] <= detail["expires"], (i, detail)
        if i == 0:
            continue  # the own row folds every own id — its times are a fold's
        nest_row = rows[token_id]
        lifetime_s = (detail["expires"] - detail["created"]).total_seconds()
        nest_lifetime_s = nest_row["expires_at"] - nest_row["created_at"]
        # Minute-truncated on both ends: within two minutes, whatever the zone.
        assert abs(lifetime_s - nest_lifetime_s) < 120, (
            f"card {i} shows a lifetime of {lifetime_s}s, the nest's row says "
            f"{nest_lifetime_s}s: {app.sessions.detail(i)!r} vs {nest_row}"
        )
        expected_address = nest_row.get("ip_address") or S.sessions.address_not_recorded
        assert detail["address"] == expected_address, (i, detail["address"], nest_row)

    # ── 4. The order (outcome 4) ──────────────────────────────────────────
    # Own first; then this device's other sign-ins, marked; then the rest,
    # newest activity first (ties by id — the shared fold's rule).
    tier = 1
    while tier < len(painted) and marks[tier] == S.sessions.mark_this_device:
        tier += 1
    assert all(m is None for m in marks[tier:]), (
        f"a 'This device' row sorts after an unmarked one; {shown}"
    )
    rest = [(rows[t]["last_used_at"], t) for t in painted[tier:]]
    assert rest == sorted(rest, key=lambda r: (-r[0], r[1])), (
        f"the other sign-ins are not newest-activity first: {rest}"
    )

    # ── 5. The revoke note (outcomes 8, 9) ────────────────────────────────
    note = app.sessions.revoke_note()
    assert note == S.sessions.revoke_note, note
    for promise in (
        "does not sign a device out",
        "remove it under Settings → Devices",
        "only your recovery kit ends them",
    ):
        assert promise in note, f"the revoke note no longer says {promise!r}: {note!r}"

    # ── 6. The lock's warning, before anything is typed (13, 14, 15) ──────
    assert not app.sessions.lock_enabled(), "the lock must not be armed before the word"
    warning = app.sessions.lockout_warning()
    assert warning == S.sessions.lockout_warning, warning
    for promise in (
        "Every device is signed out, this one too",
        "Nobody can sign in for 24 hours, you included",
        "there is no unlock",
        "does not remove somebody who holds your secret key",
        "They can do the same to you",
        "recovery kit still works while the account is locked",
    ):
        assert promise in warning, f"the lock's warning no longer says {promise!r}: {warning!r}"

    # ── 7. The word (outcome 16) ──────────────────────────────────────────
    app.sessions.type_lock_word("lock")
    assert not app.sessions.lock_enabled(), "the word is never case-folded"
    with pytest.raises(RuntimeError) as refused:
        driver.click("sessions-lockout-button")
    assert "element is disabled" in str(refused.value), (
        f"pressing the lock with the wrong word must be refused out loud: {refused.value}"
    )
    assert own_id in _listed(nest_url, user, harness_ids), (
        "the wrong word locked something: this app's own sign-in is gone"
    )
    app.sessions.type_lock_word("LOCK")
    assert app.sessions.lock_enabled(), "the literal LOCK must arm the lock"
    app.sessions.type_lock_word("")
    assert not app.sessions.lock_enabled()

    # ── 8. Revoke one — no confirm (outcomes 1, 7) ────────────────────────
    app.sessions.revoke(painted.index(stranger_a))
    assert not app.sessions.revoke_others_armed()
    wait_until(
        lambda: stranger_a not in (app.sessions.painted_token_ids() or [stranger_a]),
        ACT_FOLDED_S,
        diagnose=lambda: (
            f"the revoked card never left the page (painted={app.sessions.painted_token_ids()}, "
            f"error={app.sessions.error_text()!r})"
        ),
    )
    assert app.sessions.error_text() == ""
    listed = _listed(nest_url, user, harness_ids)
    assert stranger_a not in listed, "the nest still lists the revoked session"
    assert stranger_b in listed, "revoking one session must not touch another"
    assert own_id in listed, "revoking a stranger must not end this app's own session"

    # ── 9. A failed act keeps the rows (outcome 28) ───────────────────────
    _end_behind_the_page(nest_url, user, displaced_session)
    painted = app.sessions.painted_token_ids() or []
    assert displaced_session in painted, painted
    app.sessions.revoke(painted.index(displaced_session))
    error = wait_until(
        lambda: app.sessions.error_text(),
        ACT_FOLDED_S,
        diagnose=lambda: (
            "revoking an already-ended sign-in painted no error "
            f"(painted={app.sessions.painted_token_ids()})"
        ),
    )
    assert error.startswith(S.sessions.act_failed(error="")), error
    kept = app.sessions.painted_token_ids() or []
    for still in (own_id, stranger_b, device_session):
        assert still in kept, f"a failed act dropped the row {still}: painted={kept}, error={error!r}"
    assert app.sessions.mark(0) == S.sessions.mark_this_app

    # ── 10. Sign out everywhere else — the second press, cancellable (7) ──
    app.sessions.arm_sign_out_everywhere_else()
    assert stranger_b in _listed(nest_url, user, harness_ids), "the first press ended something"
    app.sessions.cancel_sign_out_everywhere_else()
    assert stranger_b in _listed(nest_url, user, harness_ids), "Cancel ended something"

    before = _listed(nest_url, user, harness_ids)
    app.sessions.sign_out_everywhere_else()

    def others_gone() -> bool:
        return not ((before - {own_id}) & _listed(nest_url, user, harness_ids))

    wait_until(
        others_gone,
        ACT_FOLDED_S,
        diagnose=lambda: (
            f"sessions still listed after sign out everywhere else: "
            f"{sorted((before - {own_id}) & _listed(nest_url, user, harness_ids))} "
            f"(error={app.sessions.error_text()!r})"
        ),
    )
    assert own_id in _listed(nest_url, user, harness_ids), (
        "sign out everywhere else must keep this app's own session"
    )
    wait_until(
        lambda: stranger_b not in (app.sessions.painted_token_ids() or [stranger_b]),
        ACT_FOLDED_S,
        diagnose=lambda: (
            f"the page never re-folded (painted={app.sessions.painted_token_ids()}, "
            f"error={app.sessions.error_text()!r})"
        ),
    )
    assert app.sessions.error_text() == "", (
        "the next successful act must clear the failed act's error"
    )
    assert app.sessions.mark(0) == S.sessions.mark_this_app
    assert not app.sessions.has_revoke_button(0)


@pytest.mark.feature("sessions")
def test_own_renewals_fold_into_one_row_and_survive_sign_out_everywhere_else(
    app, request, nest_instance, test_user
):
    """Outcomes 5 and 6 (``devices.md`` § The client's own session): the app's
    superseded-but-unexpired ids are its own, painted inside the one "This app"
    row and never offered for revoking; and sign out everywhere else keeps the
    id that is current WHEN PRESSED, not the one the page painted."""
    from conftest import _login_app_as, _make_user

    driver = app.driver
    _require_sessions_page(driver)
    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    harness_ids: set[str] = set()

    first_ids = own_session_ids(driver)
    assert first_ids, "the app recorded no own session at sign-in"
    stranger = _mint_stranger_session(nest_url, user)
    app.sessions.navigate()

    # ── 5. A renewal, then one act that re-reads the page ─────────────────
    renewed = renew_own_sign_in(driver)
    own_ids = first_ids | {renewed}
    rows = _listed_rows(nest_url, user, harness_ids)
    assert own_ids <= set(rows), (
        f"a renewal ended the app's earlier sign-in nest-side: own {sorted(own_ids)}, "
        f"nest lists {sorted(rows)}"
    )
    # The act: end the stranger. Its re-read folds the renewed own set.
    painted = app.sessions.painted_token_ids() or []
    assert stranger in painted, painted
    app.sessions.revoke(painted.index(stranger))
    wait_until(
        lambda: stranger not in (app.sessions.painted_token_ids() or [stranger]),
        ACT_FOLDED_S,
        diagnose=lambda: (
            f"the page never re-read after the act (painted={app.sessions.painted_token_ids()}, "
            f"error={app.sessions.error_text()!r})"
        ),
    )
    painted = app.sessions.painted_token_ids() or []
    shown = f"painted={painted}, own={sorted(own_ids)}"
    assert app.sessions.mark(0) == S.sessions.mark_this_app, shown
    assert not app.sessions.has_revoke_button(0), "this app's own row must carry no revoke"
    # The one row is named by the newest own mint — the fold's own rule
    # (created_at, then id), read off the nest's rows rather than a clock.
    newest = max(own_ids, key=lambda t: (rows[t]["created_at"], t))
    assert painted[0] == newest, f"the own row does not name the newest own sign-in; {shown}"
    strays = own_ids & set(painted[1:])
    assert not strays, (
        f"this app's own earlier sign-in {sorted(strays)} is painted as a stranger's "
        f"card to be ended; {shown}"
    )

    # ── 6. Renew AFTER the paint, then sign out everywhere else ───────────
    painted_own = painted[0]
    other = _mint_stranger_session(nest_url, user)
    current = renew_own_sign_in(driver)
    assert current != painted_own
    generation = driver.get_state("session_generation")
    app.sessions.sign_out_everywhere_else()

    wait_until(
        lambda: other not in _listed(nest_url, user, harness_ids),
        ACT_FOLDED_S,
        diagnose=lambda: (
            f"sign out everywhere else never ended {other} "
            f"(error={app.sessions.error_text()!r})"
        ),
    )
    listed = _listed(nest_url, user, harness_ids)
    assert current in listed, (
        f"sign out everywhere else ended the app's CURRENT sign-in {current} — it kept "
        f"what was painted ({painted_own}) instead of what was current when pressed; "
        f"nest lists {sorted(listed)}"
    )
    # The app's live socket was upgraded with its FIRST sign-in, which the act
    # just ended, so the nest closes it; the app must reconnect on its current
    # bearer and stay signed in — never torn down to sign-in.
    wait_until_online(driver)
    assert driver.get_state("session_generation") == generation, (
        "sign out everywhere else tore this app's session down (session_generation moved)"
    )
    assert own_session_ids(driver) & _listed(nest_url, user, harness_ids), (
        "none of the app's own sign-ins is still live after sign out everywhere else"
    )
    # The act's own pass folds (its re-read may meet the teardown and paint
    # an error — outcome 28's shape — which the next act clears).
    wait_until(
        lambda: other not in (app.sessions.painted_token_ids() or [other])
        or app.sessions.error_text(),
        ACT_FOLDED_S,
        diagnose=lambda: f"the act's pass never folded (painted={app.sessions.painted_token_ids()})",
    )
    # Proof it is still signed in: one more act, made after the teardown, lands
    # nest-side — an unauthenticated app could not end anything.
    late = _mint_stranger_session(nest_url, user)
    app.sessions.sign_out_everywhere_else()
    wait_until(
        lambda: late not in _listed(nest_url, user, harness_ids),
        ACT_FOLDED_S,
        diagnose=lambda: (
            f"an act after sign out everywhere else never landed — is the app still "
            f"signed in? (error={app.sessions.error_text()!r})"
        ),
    )
    assert own_session_ids(driver) & _listed(nest_url, user, harness_ids), (
        "the second sign out everywhere else ended this app's own sign-in"
    )
    wait_until(
        lambda: app.sessions.error_text() == "",
        ACT_FOLDED_S,
        diagnose=lambda: f"the page still shows an error: {app.sessions.error_text()!r}",
    )
    assert app.sessions.mark(0) == S.sessions.mark_this_app
    assert driver.get_state("session_generation") == generation
