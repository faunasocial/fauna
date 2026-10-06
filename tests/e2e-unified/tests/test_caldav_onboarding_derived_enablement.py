"""Discovery: the post-claim CalDAV-enablement default by address type.

No-modes retirement (ratified 2026-07-12): the former `encryption_mode_choice`
page's `onboarding-enable-caldav-checkbox` is RETIRED WITHOUT relocation.
CalDAV enablement is now a MACHINE-DERIVED default — "ON iff the handle is a
real registerable domain" (caldav-server.md § Independent enablement,
onboarding.md § 3b) — applied by the post-claim launch glue with no
onboarding-time choice. This tier_3 test drives a *real* UI onboarding against
a fresh unclaimed nest per address type (the `unclaimed_caldav_nest` Task-2
fixture) all the way through claim + the terminal `nat_mode_choice` step to
LoggedIn, waits for the post-claim step's completion anchor, then reads the
derived state off the Admin
`fauna.bridges.get_mail_config` twin (the same ground truth the admin Calendar
page hydrates from) instead of a checkbox.

This is a *discovery* assertion (plan Task 3): `real_domain` MUST default ON
(load-bearing per the goal doc); `localhost`/`ip` are expected OFF, but a
disagreement is a finding (xfail), not a force-pass.

⚠ **`caldav_enabled` cannot be read bare — it needs the mail-off probe below**. The DB singleton is
an `Option<bool>`, and an *unset* one falls back to `mail_enabled`
(`assemble_fetch_config_reply`, behind both `fetch_config` and the admin twin
`get_mail_config`) — while on a freshly-claimed nest `mail_enabled` is itself
unset and projects `true` (the pre-Stage-5 "approved ⇒ enabled" fallback, pinned
by `fetch_config_returns_defaults_for_mta`). So an untouched nest reports
`caldav_enabled=true` **with no client involvement at all**: read bare, the
`real_domain` case was vacuously green (true even if the app fired nothing) and
`localhost`/`ip` were unpassable on *every* app, which is what presented for a
while as a windows-only product bug. `_explicit_caldav_enabled` therefore pins the
fallback to a known `false` (`set_mail_enabled(false)`) before its final read, so
what it returns is the app's *explicit* `set_caldav_enabled` intent — exactly what
§ 3b's derivation contract is about — and not the nest's default. The fallback
itself is ratified (`caldav-server.md` § Independent enablement) and the tri-state
mechanics are pinned app-independently by
`tests/api/test_dav_enable_independence.py`; the nest's own unset-`mail_enabled`
default is the separate, already-tracked Stage-5 default-off flip
(`mail-bridge-lifecycle.md` § Default-off) and is deliberately NOT what this test
asserts.
"""

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient

# windows added 2026-07-18 (was a stale exclusion, plus a redundant in-body
# skip duplicating this same restriction — e2e rule 7 forbids platform checks
# in test files): `claim_through_onboarding` drives real UI onboarding via
# generic driver methods (`wait_for`/`clear_and_type`/`click`) plus the
# cross-app `call_machine_method("navigate_to_claim_code_for_known_nest")`
# seam, already exercised unconditionally (no client marker at all) by
# test_mail_client_full_roundtrip.py/test_mail_client_reply_roundtrip.py/
# test_mail_html_roundtrip.py — windows' OnboardingViewModel forwards it
# generically to the shared UniFFI IOnboardingMachine before the
# post-login null-out (a separate, differently-scoped gap tracked by
# test_nest_identity_pin.py). Live-verify owed (real onboarding × 3 address
# types is historically fragile on Windows).
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.windows,
    # tui added 2026-08-21: `claim_through_onboarding` drives real UI
    # onboarding via generic driver methods only (this file's own marker
    # comment above); the test's own assertion is a pure WS-RPC admin read,
    # nothing page-rendering. tui is the onboarding lead app.
    pytest.mark.tui,
    # web added 2026-09-21: the claim drive is the same
    # generic-driver path, and web was short only a dial it could reach — the
    # claim now rides the variant nest's own SPA proxy (`dedicated_node_url` →
    # `spa_proxy_for`), since a browser cannot reach a raw nest, which sends no
    # CORS headers. The admin read stays on the raw URL (Python needs no CORS).
    pytest.mark.web,
]


def _explicit_caldav_enabled(app, admin_secret_hex: str, nest_url: str) -> bool:
    """Wait for the post-claim serving-enablement step to finish, then return
    whether the app **explicitly** enabled CalDAV.

    The wait is the step's own completion anchor
    (`fauna_e2e_agent::SERVING_ENABLEMENT_KEY` — this admin's run marked
    completed), never a settle window: once it passes, the
    glue has nothing left to write, so the read below is final in both
    directions — a slow real-domain enable cannot miss it, and a glue that
    wrongly fired `set_caldav_enabled(true)` late for a local handle cannot slip
    past it (convention 14).

    The final read is taken *after* a `set_mail_enabled(false)` probe. That probe
    is what makes the answer meaningful: an unset `caldav_enabled` falls back to
    `mail_enabled`, so with mail pinned `false` a `true` here can only come from
    an explicit `set_caldav_enabled(true)` — the app's § 3b intent — while a
    never-set toggle reads `false`. Without the probe both cases read `true` on a
    freshly-claimed nest (module docstring); the tri-state mechanics are pinned
    app-independently by `tests/api/test_dav_enable_independence.py`. The anchor
    orders the probe after every glue write, so the probe cannot be overwritten
    by a late `set_mail_enabled(true)`. Mail is deliberately left off afterwards
    — this is the last thing each parametrize case does.

    Deliberately independent of `helpers.caldav_onboarding._derived_enablement`:
    that helper reads the derived state to *decide whether to toggle*, while this
    one must witness client intent, hence the probe.
    """
    from nacl.signing import SigningKey

    from helpers.waiting import await_serving_enablement_for

    sk = SigningKey(bytes.fromhex(admin_secret_hex))
    actor_id, signing_key = bytes(sk.verify_key), bytes(sk)
    decided = await_serving_enablement_for(app.driver, actor_id.hex())
    admin_ws = WsRpcAdminClient(nest_url, actor_id=actor_id, signing_key=signing_key)
    with admin_ws:
        admin_ws.call("fauna.bridges.set_mail_enabled", {"enabled": False})
        cfg = admin_ws.call("fauna.bridges.get_mail_config", {})
    enabled = bool(cfg.get("caldav_enabled"))
    # The app's own decision and the nest's ground truth must agree — a
    # mismatch names which half broke (convention 6).
    assert decided is not None and bool(decided.get("caldav")) == enabled, (
        f"the serving-enablement step decided {decided!r} but the nest reports "
        f"caldav_enabled={enabled}"
    )
    return enabled


@pytest.mark.parametrize(
    "address_type,expected_default",
    [
        ("real_domain", True),
        ("localhost", False),
        ("ip", False),
    ],
)
@pytest.mark.feature("calendar-in-standard-apps")
def test_caldav_derived_enablement_by_address_type(
    app, unclaimed_caldav_nest, request, address_type, expected_default
):
    h = unclaimed_caldav_nest(address_type)
    from helpers.caldav_onboarding import claim_through_onboarding
    from helpers.mail_dedicated_nest import dedicated_node_url

    # import-key → handle → claim-code → nat_mode_choice (dismissed) → LoggedIn
    # — the post-claim launch glue fires the handle-derived caldav-enable
    # signal with no onboarding-time choice.
    pre = claim_through_onboarding(app, h, node_url=dedicated_node_url(app, h, request))
    enabled = _explicit_caldav_enabled(app, pre["admin_secret_hex"], h.nest_url)
    assert enabled is expected_default
