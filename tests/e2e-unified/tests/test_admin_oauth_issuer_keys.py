"""tier_3 e2e: the ADMIN reads the nest's outside-app sign-in keys and walks all
three rotation controls through the app UI.

``authorization-server.md`` § The issuer → *Two rotation arms*: the nest-side
compromise response (``fauna.oauth.{issuer_key_status, rotate_issuer_key,
force_rotate_issuer_key, force_rotate_session_secret}``) had a shared call
surface and api-level witnesses (``tests/api/test_oauth_issuer.py``) but no app
painted any of it — and a compromise response an admin cannot perform from an
app is not a response (``principles.md`` § One configuration surface). This
journey drives it the way an admin would:

nav ``admin-nest`` → the key set paints, signer first → the ORDINARY arm
dispatches on the press (no confirm — nothing breaks) → its verdict names the
new signer → the list shows it first and the previous key as still accepted
for N more minutes → the FORCED key arm arms a confirm naming "all 2 keys"
BEFORE anything dispatches → cancel dispatches nothing (the list is unchanged)
→ re-arm → confirm → the verdict names the new signer AND both dropped keys →
the list is exactly one row → the SECRET arm's confirm states the re-consent
cost → confirm → verdict → a second secret rotation reports the "ended since
<instant>" shape, because the first minted a generation for it to end.

Mutations are UI-only (e2e-conventions point 8). The one out-of-band read is
the closing join — ``fauna.oauth.issuer_key_status`` over the admin's own
WS-RPC session — so the rendered list is proven to be the nest's served set
rather than a stale fold (external black-box verification, outside point 8).

No domained nest: the status kind answers on any nest holding a deployment
signing key (``bins/fauna-nest/src/oauth_issuer_handlers.rs::deployment_seed``),
which every booted nest reconciles; only the HTTP issuer surfaces (JWKS,
discovery) need a claimed domain.

Latency-independent (convention 14): every wait is a named generous budget +
deadline poll on rendered state.

App arms: **tui** (the lead app — ``testing.md`` § Default app and nest mode's
rust-first ordering), then **linux**, **web** and **android** in the batched
trickle-down, then **macOS** and **iOS** (one shared FaunaKit view), then
**windows** — tui and linux call ``fauna_client_admin``'s issuer doors
in-process, web through the wasm face, android and apple through the UniFFI
face of the same doors; windows through the WS-RPC seam
(``INestRpcClient`` → the same three free FFI functions apple/android call
directly).
"""

import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.app_surface import app_name, skip_unbuilt
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
]

_BUILT_APPS = ("tui", "linux", "web", "android", "macos", "ios", "windows")

# Named budgets (generous ceilings; deadline polls pay only the real delay).
_PAGE_BUDGET_S = 30.0     # admin-nest page render + the key-set read after nav
_VERDICT_BUDGET_S = 60.0  # one WS-RPC round-trip + the key-set re-read + repaint

_T = S.admin.nest_page


def _poll(check, budget_s, tag):
    """Deadline-poll ``check`` until truthy; the failure names the budget."""
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.25)
    raise AssertionError(f"{tag}: not reached within {budget_s}s")


def _kid_of(row_text):
    """A key row's kid — the text before the row's ' — ' separator (the kid is
    an RFC 7638 thumbprint, base64url, so it never contains the separator)."""
    return row_text.split(" — ", 1)[0]


def _wait_verdict(app, tag):
    """Poll until the section's verdict line holds a finished verdict."""
    _poll(
        lambda: app.driver.count("admin-nest-oauth-status") > 0
        and app.admin.oauth_status_text() != _T.oauth_working,
        _VERDICT_BUDGET_S,
        tag,
    )
    return app.admin.oauth_status_text()


@pytest.mark.feature("admin-nest")
def test_the_admin_walks_the_three_sign_in_key_controls_through_the_app(
    admin_app, nest_instance
):
    """read → ordinary rotate → forced (cancel, then confirm) → secret ×2 → the
    rendered set joins the nest's own."""
    app = admin_app
    if app_name(app.driver) not in _BUILT_APPS:
        skip_unbuilt(
            app.driver,
            surface="admin-nest-oauth-section",
            detail="the outside-app sign-in key controls (authorization-server.md "
            "§ The issuer → Two rotation arms) are built on all 7 apps "
            "(tui, linux, web, android, macOS, iOS, windows)",
            tracked="authorization-server.md § The issuer → Two rotation arms",
        )

    app.admin.navigate_nest()
    _poll(
        lambda: app.driver.count("admin-nest-oauth-key-item-0") > 0,
        _PAGE_BUDGET_S,
        "admin-nest paints the served key set",
    )
    rows = app.admin.oauth_key_rows()
    assert len(rows) == 1, f"a fresh nest serves exactly its signer; got {rows!r}"
    first_kid = _kid_of(rows[0])
    assert rows[0] == _T.oauth_key_signing(kid=first_kid), (
        f"the one row is the signer's, got {rows[0]!r}"
    )
    assert app.driver.count("admin-nest-oauth-key-reason") == 0, (
        "an answered set owes no reason line"
    )

    # ── The ordinary arm: no confirm, nothing breaks ─────────────────────────
    app.admin.oauth_rotate()
    verdict = _wait_verdict(app, "the ordinary rotation reports its verdict")
    rows = app.admin.oauth_key_rows()
    assert len(rows) == 2, (
        f"a rotation ADDS a key — the outgoing one stays served; got {rows!r} "
        f"(verdict {verdict!r})"
    )
    second_kid = _kid_of(rows[0])
    assert second_kid != first_kid, f"a rotation mints a new signer; got {rows!r}"
    assert verdict == _T.oauth_rotate_done(kid=second_kid), (
        f"the verdict must name the new signer, got {verdict!r}"
    )
    assert rows[0] == _T.oauth_key_signing(kid=second_kid), rows
    # The countdown is the nest's own horizon (20 min) counted at paint, so
    # the exact minute is the wall clock's; the shape — kid, "still accepted",
    # a whole-minute count — is not.
    retiring_prefix = f"{first_kid} — replaced; still accepted for "
    assert rows[1].startswith(retiring_prefix) and rows[1].endswith(" min"), (
        f"the previous key must read as still accepted with a countdown, got {rows[1]!r}"
    )
    assert int(rows[1][len(retiring_prefix):-len(" min")]) in range(1, 21), rows[1]

    # ── The forced key arm: cancel first, and nothing is dropped ─────────────
    app.admin.oauth_arm_force_rotate()
    assert app.driver.count("admin-nest-oauth-confirm-button") > 0, (
        "arming must paint the confirm surface"
    )
    assert app.admin.oauth_confirm_summary() == _T.oauth_force_rotate_confirm_many(count="2"), (
        "the confirm must name how many keys stop verifying BEFORE dispatch, got "
        f"{app.admin.oauth_confirm_summary()!r}"
    )
    app.admin.oauth_cancel()
    assert app.driver.count("admin-nest-oauth-confirm-button") == 0, "cancel disarms"
    assert [_kid_of(r) for r in app.admin.oauth_key_rows()] == [second_kid, first_kid], (
        "cancelling a forced rotation must drop nothing"
    )

    app.admin.oauth_arm_force_rotate()
    app.admin.oauth_confirm()
    # Disarm-before-dispatch: nothing left to double-press once the click replies.
    assert app.driver.count("admin-nest-oauth-confirm-button") == 0, (
        "the first confirm press must disarm the surface"
    )
    verdict = _wait_verdict(app, "the forced rotation reports its verdict")
    rows = app.admin.oauth_key_rows()
    assert len(rows) == 1, (
        f"a forced rotation leaves exactly the new signer served; got {rows!r} "
        f"(verdict {verdict!r})"
    )
    third_kid = _kid_of(rows[0])
    assert third_kid not in (first_kid, second_kid), rows
    assert verdict == _T.oauth_force_rotate_done(
        kid=third_kid, dropped=f"{second_kid}, {first_kid}"
    ) or verdict == _T.oauth_force_rotate_done(
        kid=third_kid, dropped=f"{first_kid}, {second_kid}"
    ), f"the verdict must name the new signer and BOTH dropped keys, got {verdict!r}"

    # ── The secret arm: its cost, then a generation to end ───────────────────
    app.admin.oauth_arm_secret_force_rotate()
    assert app.admin.oauth_confirm_summary() == _T.oauth_secret_force_rotate_confirm, (
        f"the secret arm states the re-consent cost, got {app.admin.oauth_confirm_summary()!r}"
    )
    app.admin.oauth_confirm()
    verdict = _wait_verdict(app, "the first saved-sign-in rotation reports")
    # A fresh nest may or may not have minted the secret yet (it provisions on
    # first use), so the first verdict is either shape — but never a failure.
    assert verdict == _T.oauth_secret_force_rotate_first or verdict.startswith(
        "Ended. Every saved sign-in issued since "
    ), f"the first secret rotation must succeed, got {verdict!r}"

    # The second press: the first minted a generation, so this one must END
    # it — a door that answered without touching the row would read "nothing
    # to end" again. (The two "Ended" verdicts can be byte-identical when both
    # land in one minute, so the assertion is the shape, not a change.)
    app.admin.oauth_arm_secret_force_rotate()
    app.admin.oauth_confirm()
    second = _wait_verdict(app, "the second saved-sign-in rotation reports")
    # This test never connects an outside app, so the count-aware verdict is
    # always the zero-apps shape (oauth_secret_force_rotate_done_none).
    assert second.startswith("Ended. Every saved sign-in issued since ") and second.endswith(
        " stopped working. No outside apps were connected here."
    ), f"the second rotation must end the generation the first minted, got {second!r}"
    assert [_kid_of(r) for r in app.admin.oauth_key_rows()] == [third_kid], (
        "rotating the saved-sign-in secret must not touch the key set"
    )

    # ── The join: the rendered set IS the nest's served set ──────────────────
    admin = nest_instance["admin"]
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as client:
        status = client.call("fauna.oauth.issuer_key_status", {})
    assert [k["kid"] for k in status["keys"]] == [third_kid], (
        f"the app's list and the nest's served set must be one set; nest {status['keys']!r}"
    )
