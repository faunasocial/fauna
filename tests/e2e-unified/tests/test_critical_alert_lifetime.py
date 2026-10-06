"""tier_3 e2e: the lifetime rules of the every-page critical-alert banner.

Goal doc: ``docs/goal/behavior/critical-alerts.md`` — § Goal (the banner is
**non-dismissable**: an alert goes only when the condition that raised it is
re-checked and found resolved) and § Mechanism → *Lifetime* (an alert lives as
long as the IDENTITY, not the process: sign-out and account switch take every
alert with it).

Both rules are safety properties of a loud surface, and both fail silently in
the direction that hurts. A dismissable alarm is one a user swipes away the day
before it matters. An alarm that outlives its identity accuses the INCOMING
account — with the departed account's name, permanently, because nothing ever
re-checks a departed identity — which is the crying-wolf failure the severity
bar exists to prevent (found as a live defect on tui and linux in August).
Rust unit tests pin the registry and each app's teardown call; what only a
full-stack run can show is the banner a user actually sees across those
gestures.

**Arranged outside the app, on purpose (convention 8's carve-out).** Every
condition here is something that happens TO the user, not something they do:
a registrar's record changing (the domain-expiry seam, ``test-hooks``), or a
thief holding the seed parking a RecoveryKey replacement (``helpers/
succession.py``). What the user does — look at pages, try to dismiss, sign out,
switch accounts — is driven through the UI.

Latency discipline (convention 14): every positive wait is a deadline poll for
a caused state change, and every "the banner stays DOWN" assertion is anchored
to the sweep's own pass counter (``await_sweep_pass_after``), never to a
settle-sleep.

tier_3: a real ``fauna-nest`` binary built with ``--features test-hooks``.
"""
from __future__ import annotations

import json
import time
import urllib.request

import pytest

from helpers.budgets import ALERT_SWEEP_PASS_S, APP_RELAUNCH_S
from helpers.inert_refusal import is_inert_refusal
from helpers.registry_audit import visible_page_tabs
from helpers.waiting import (
    alert_sweep_passes,
    await_session_actor,
    await_sweep_pass_after,
    wait_until,
)

pytestmark = pytest.mark.tier_3

CRITICAL_ALERTS = "critical-alerts"
CRITICAL_ALERT = "critical-alert"

# tests/e2e-unified/ui.yaml § settings (the account switcher).
SWITCHER_LIST = "account-switcher-list"
SWITCHER_ITEM = "account-switcher-item"
ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}

# The domain-expiry condition (`domains-and-tls-bootstrap.md` § Domain loss →
# *Detection*): a future registration carrying a lapse-class status, so only
# the status arm can be what alarms. Never fetched — the seam is the record.
_DOMAIN = "lifetime-example.test"
_LAPSE_STATUS = "redemption period"
# A phrase both role lines share, so the poll is role-agnostic.
_DOMAIN_FRAGMENT = "is being withdrawn"

# The pending-replacement condition's shared headline — identity-scoped
# (`recovery-replacement-pending:<actor_id>`), which is what the account-switch
# arm needs: a deployment-scoped alarm would be re-raised for the incoming
# account too and could not tell a survivor from a fresh post.
_REPLACEMENT_FRAGMENT = "replacement of your account recovery key"


def _seed_domain_record(nest_url: str, *, statuses: list[str], expires_in_days: int) -> None:
    """``POST /api/v1/test/domain-expiry`` — write the nest watch's record."""
    body = json.dumps(
        {
            "domain": _DOMAIN,
            "outcome": "checked",
            "expires_at": int(time.time()) + expires_in_days * 24 * 60 * 60,
            "statuses": statuses,
            "detail": None,
        }
    ).encode()
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/domain-expiry",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=10.0) as resp:
        assert resp.status == 200, f"seed domain-expiry returned {resp.status}"


def _seed_healthy_domain(nest_url: str) -> None:
    """The renewed registration — also every test's cleanup, since the record is
    deployment-scoped and would otherwise alarm every later test on this nest."""
    _seed_domain_record(nest_url, statuses=["active"], expires_in_days=400)


def _alert_text(driver) -> str:
    """Every active alert row's text joined, or "" when the banner is absent.

    Presence of the banner IS the rendering contract (`critical-alerts.md`
    § Rendering contract), so this never consults the registry another way.
    """
    if not driver.is_visible(CRITICAL_ALERTS):
        return ""
    return "\n".join(driver.get_texts(CRITICAL_ALERT))


def _poll_banner(driver, fragment: str, *, present: bool, budget_s: float = ALERT_SWEEP_PASS_S) -> str:
    """Deadline-poll until ``fragment``'s presence in the banner is ``present``."""
    deadline = time.monotonic() + budget_s
    text = _alert_text(driver)
    while (fragment in text) != present and time.monotonic() < deadline:
        time.sleep(0.5)  # sleep-ok: pacing between poll iterations, not a settle-wait
        text = _alert_text(driver)
    return text


def _plant_pending_replacement(nest_instance, user: dict) -> None:
    """A thief holding ``user``'s seed parks a seed-alone RecoveryKey replacement.

    Two fixture calls: the kit must exist for a replacement to have anything to
    replace. Both are transport only — the wire shapes live in
    ``recovery_fixture.rs`` (``helpers/succession.py`` module doc).
    """
    from helpers.succession import register_recovery_kit, request_seed_alone_replacement

    seed_hex = bytes(user["signing_key"]).hex()
    register_recovery_kit(
        nest_instance["url"], actor_id_hex=user["actor_id_hex"], identity_seed_hex=seed_hex
    )
    request_seed_alone_replacement(
        nest_instance["url"], actor_id_hex=user["actor_id_hex"], identity_seed_hex=seed_hex
    )


@pytest.mark.feature("critical-alerts")
def test_a_standing_alert_carries_no_way_to_dismiss_it(app, nest_instance, request):
    """An active alert offers no dismiss affordance, survives every gesture and
    every page, and comes down only when a re-check finds its condition resolved.

    Four observations, because "cannot be dismissed" has four ways to be false:
    a control inside the banner; the row itself being actuable (a click or Enter
    on it doing something); the banner living only on the page that raised it;
    and the banner clearing for some reason other than the condition resolving.
    The last is closed from the other side: the only change the journey makes
    before the banner comes down is the registrar's record turning healthy.
    """
    from conftest import _login_app_as, _make_user

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    driver = app.driver
    _seed_domain_record(nest_url, statuses=[_LAPSE_STATUS], expires_in_days=365)
    try:
        _login_app_as(app, request, nest_instance, user)
        driver.navigate_to("feed")
        text = _poll_banner(driver, _DOMAIN_FRAGMENT, present=True)
        assert _DOMAIN_FRAGMENT in text, (
            "precondition: a lapse-class registration must raise the banner after "
            f"a session start; banner reads {text!r}, error surface: "
            f"{app.error_text()!r}"
        )

        # ── 1. The banner holds no control. A whole-frame read (convention
        #       17): nothing under the banner, the banner itself included, is
        #       something a user can press or type into.
        frame = driver.registry_snapshot()
        assert frame is not None, (
            f"{type(driver).__name__} serves no /registry frame — the affordance "
            "check needs it"
        )
        banner_rows = [r for r in frame if r.get("id") in (CRITICAL_ALERTS, CRITICAL_ALERT)]
        assert any(r.get("id") == CRITICAL_ALERT for r in banner_rows), (
            f"the frame registered no {CRITICAL_ALERT!r} row while the banner is "
            f"visible — the registry and the pixels disagree: {banner_rows!r}"
        )
        controls = [
            r for r in frame
            if (r.get("actuable") or r.get("editable"))
            and (
                r.get("id") in (CRITICAL_ALERTS, CRITICAL_ALERT)
                or CRITICAL_ALERTS in (r.get("scope") or "")
            )
        ]
        assert not controls, (
            "the critical-alert banner must carry no way to dismiss it "
            f"(critical-alerts.md § Goal); the frame offers {controls!r}"
        )

        # ── 2. Activating the row does nothing to it. An app may refuse the
        #       gesture outright (tui answers "not actuable") or accept a no-op
        #       press; either way the alert must still be standing after it.
        for gesture in ("click", "enter"):
            try:
                if gesture == "click":
                    driver.click(CRITICAL_ALERT)
                else:
                    driver.press_key(CRITICAL_ALERT, "Enter")
            except (LookupError, RuntimeError) as refusal:
                assert is_inert_refusal(refusal), (
                    f"the {gesture} on the alert row failed for a reason other than "
                    f"'there is nothing to activate': {refusal}"
                )
            driver.barrier()
            after = _alert_text(driver)
            assert _DOMAIN_FRAGMENT in after, (
                f"a {gesture} on the alert row took it down — an alarm is "
                f"non-dismissable while its condition holds; banner now {after!r}"
            )

        # ── 3. Every authenticated page carries it, not just the one it was
        #       read on — the set-and-forget point of a banner at all.
        tabs = visible_page_tabs(driver)
        assert len(tabs) >= 3, f"the walk found too few pages to mean anything: {tabs!r}"
        missing = []
        for tab in tabs:
            driver.click(tab)
            driver.barrier()
            if _DOMAIN_FRAGMENT not in _alert_text(driver):
                missing.append(tab)
        assert not missing, (
            "the banner must stand on every authenticated page while its alert is "
            f"active; it was absent after {missing!r} (walked {tabs!r})"
        )

        # ── 4. …until the condition is checked again and found resolved. The
        #       renewal is the only thing that changes; the re-check is the
        #       session establishment that runs the sweep.
        driver.navigate_to("feed")
        started, _ = alert_sweep_passes(driver) or (0, 0)
        _seed_healthy_domain(nest_url)
        _login_app_as(app, request, nest_instance, user)
        await_sweep_pass_after(driver, started, budget_s=ALERT_SWEEP_PASS_S, what="the renewal")
        text = _poll_banner(driver, _DOMAIN_FRAGMENT, present=False)
        assert _DOMAIN_FRAGMENT not in text, (
            "a re-check that found the registration renewed must take the banner "
            f"down; banner still reads {text!r}"
        )
    finally:
        _seed_healthy_domain(nest_url)


@pytest.mark.feature("critical-alerts")
def test_signing_out_takes_every_standing_alert_with_it(app, nest_instance, request):
    """A's standing alarm does not greet B after A signs out and B signs in.

    The alarm is identity-scoped (a pending replacement on A's account), so
    nothing B's own sweep does can post OR clear it: if it is on B's screen
    after B's first sweep pass, it survived the sign-out.
    """
    from conftest import _login_app_as, _make_user

    driver = app.driver
    user_a = _make_user(nest_instance)
    user_b = _make_user(nest_instance)
    _plant_pending_replacement(nest_instance, user_a)

    _login_app_as(app, request, nest_instance, user_a)
    driver.navigate_to("feed")
    text = _poll_banner(driver, _REPLACEMENT_FRAGMENT, present=True)
    assert _REPLACEMENT_FRAGMENT in text, (
        "precondition: A's parked replacement must be loud before the sign-out; "
        f"banner reads {text!r}, error surface: {app.error_text()!r}"
    )

    started, _ = alert_sweep_passes(driver) or (0, 0)
    app.settings.sign_out()

    _login_app_as(app, request, nest_instance, user_b)
    driver.navigate_to("feed")
    # B's session-start pass is the causal anchor: it began after the plant-time
    # count, so once it completes, whatever the banner shows is B's world.
    await_sweep_pass_after(driver, started, budget_s=ALERT_SWEEP_PASS_S, what="B's first sweep")
    text = _alert_text(driver)
    assert _REPLACEMENT_FRAGMENT not in text, (
        "A's alarm survived the sign-out and now accuses B — sign-out must drop "
        "every alert (critical-alerts.md § Mechanism → Lifetime); banner reads "
        f"{text!r}"
    )


# The apps whose own switcher suite gives this arm a launch shape
# (`test_account_switcher_<app>.py`): tui, linux, windows, macOS and iOS seed
# the registry at launch (apple into the Keychain; windows reads the registry
# alone, its legacy mirror retired), web seeds localStorage and re-boots over
# it.
SWITCH_APPS = ("tui", "linux", "web", "windows", "macos", "ios")

# Parametrized so the arm is COUNTED on these apps, then declared unbuilt rather
# than silently absent (convention 7): android has no switcher suite to lend
# this arm its launch shape, and its state names no `session.actor_id` for a
# registry-seeded launch, which `_await_actor` reads. android joins
# `SWITCH_APPS` as its switcher leg lands.
UNBUILT_SWITCH_APPS = ("android",)


def _switch_apps() -> list[str]:
    from conftest import get_available_apps

    available = get_available_apps()
    return [a for a in SWITCH_APPS + UNBUILT_SWITCH_APPS if a in available]


def _launch_two_account_seat(switch_app, request, nest_instance, user_a, user_b):
    """Launch ``switch_app`` holding A and B, A active and signed in — the way a
    user who holds two accounts meets the switcher, in that app's own switcher
    suite's shape. Returns the driver."""
    from common import build_registry_seed
    from conftest import _seeded_environment
    from drivers import create_driver

    def entry(user, device, handle, url):
        return {"actor_id": user["actor_id_hex"], "secret_hex": bytes(user["signing_key"]).hex(),
                "nest_url": url, "device_id": device, "handle": handle}

    if switch_app == "web":
        from tests.test_account_switcher_web import _auth_on_account_page

        spa_url = request.getfixturevalue("spa_url")
        seed = build_registry_seed(
            # add order == display order → A is row 0, B is row 1.
            [entry(user_a, "alert-lifetime-a", "a", spa_url),
             entry(user_b, "alert-lifetime-b", "b", spa_url)],
            active=user_a["actor_id_hex"],
        )
        driver = create_driver("web")
        driver.launch({"url": spa_url + "/app/"})
        _auth_on_account_page(
            driver, seed, bytes(user_a["signing_key"]).hex(), "a", user_a["actor_id_hex"], spa_url
        )
        return driver

    url = nest_instance["url"]
    if switch_app in ("macos", "ios"):
        from tests.test_account_switcher_apple import _app_launch_config

        launch = _app_launch_config(switch_app, request)
    else:
        launch = {"app_path": request.getfixturevalue(f"{switch_app}_app_path")}
    seed = build_registry_seed(
        [entry(user_a, "alert-lifetime-a", "a", url), entry(user_b, "alert-lifetime-b", "b", url)],
        active=user_a["actor_id_hex"],
    )
    driver = create_driver(switch_app)
    driver.launch({
        **launch,
        "url": url,
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    return driver


def _await_actor(driver, switch_app, actor_hex, what):
    """Wait until the seat's session is ``actor_hex``'s.

    Web's registry-seeded seat publishes no ``authenticated`` flag (its own
    switcher suite waits on the page instead), so there the session's actor is
    the read; the sweep-pass anchor that follows the switch is what proves the
    incoming session is live on every app.
    """
    if switch_app != "web":
        await_session_actor(driver, actor_hex, budget_s=APP_RELAUNCH_S, what=what)
        return

    def session():
        return (driver.get_state() or {}).get("session") or {}

    wait_until(
        lambda: session().get("actor_id") == actor_hex,
        APP_RELAUNCH_S,
        diagnose=lambda: f"{what}: the session is {session()!r}, not {actor_hex}'s",
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.parametrize("switch_app", _switch_apps())
@pytest.mark.feature("critical-alerts")
def test_switching_accounts_takes_every_standing_alert_with_it(
    switch_app, nest_instance, request
):
    """Switching from A to B in the account switcher leaves B a clean banner.

    Driven from a seeded two-account registry, the way a user who holds two
    accounts meets the switcher (each app's ``test_account_switcher_<app>.py``
    shape); the switch itself is the UI's own row press.
    """
    from actions import ActionLayer
    from common import create_actor_and_register
    from helpers.app_surface import skip_unbuilt

    if switch_app in UNBUILT_SWITCH_APPS:
        skip_unbuilt(
            switch_app,
            surface="a two-account switcher launch shape (test_account_switcher_android.py)",
            detail="no android switcher suite seeds the registry and awaits the "
            "seeded actor yet, so this arm has no launch to drive",
            tracked="",
        )

    admin_sk = nest_instance["admin"]["signing_key"]
    user_a = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_b = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    _plant_pending_replacement(nest_instance, user_a)

    driver = _launch_two_account_seat(switch_app, request, nest_instance, user_a, user_b)
    try:
        app = ActionLayer(driver)
        _await_actor(driver, switch_app, user_a["actor_id_hex"], "A's launch")
        driver.navigate_to("feed")
        text = _poll_banner(driver, _REPLACEMENT_FRAGMENT, present=True)
        assert _REPLACEMENT_FRAGMENT in text, (
            "precondition: A's parked replacement must be loud before the switch; "
            f"banner reads {text!r}, error surface: {app.error_text()!r}"
        )

        started, _ = alert_sweep_passes(driver) or (0, 0)
        driver.set_state(ACCOUNT_PAGE_NAV)
        driver.wait_for(SWITCHER_LIST, timeout=30)
        wait_until(
            lambda: driver.count(SWITCHER_ITEM) == 2,
            30,
            diagnose=lambda: f"switcher lists {driver.count(SWITCHER_ITEM)} accounts, not 2",
        )
        driver.click(SWITCHER_ITEM, index=1)
        _await_actor(driver, switch_app, user_b["actor_id_hex"], "the switch to B")

        driver.navigate_to("feed")
        # Web's switch is a full page reload, so its pass counters restart with
        # the page: there the reloaded page's first completed pass is B's. The
        # other apps switch live, and B's pass is the first to begin after the
        # pre-switch count.
        anchor = 0 if switch_app == "web" else started
        await_sweep_pass_after(driver, anchor, budget_s=ALERT_SWEEP_PASS_S, what="B's first sweep")
        text = _alert_text(driver)
        assert _REPLACEMENT_FRAGMENT not in text, (
            "A's alarm survived the account switch and now accuses B — a switch "
            "must drop every alert (critical-alerts.md § Mechanism → Lifetime); "
            f"banner reads {text!r}"
        )
    finally:
        driver.teardown()


# `fauna_e2e_agent::ALERT_SWEEP_WAKE` — end the current identity's re-sweep wait
# so the production loop sweeps again. Its barrier is `alert_sweep_passes`.
ALERT_SWEEP_WAKE = "alert_sweep_wake"


@pytest.mark.feature("critical-alerts")
def test_a_condition_arising_mid_session_is_announced_without_a_restart(
    app, nest_instance, request
):
    """A condition that arises while the app is open reaches the banner on the
    loop's next pass — no restart, no re-login, no page visit.

    The loop's clock is six hours (`RE_SWEEP_INTERVAL_SECS`), so the journey
    ends the loop's WAIT instead of waiting it out (convention 14): the
    ``alert_sweep_wake`` poke races the production wait, and everything else is
    the production loop body. Re-establishing the session would NOT do — that
    runs a converge-arm one-shot pass, and would pass with the loop deleted.

    "Without a restart" is asserted, not assumed: the app's teardown counter
    (``session_generation``) must not move across the whole journey.
    """
    from conftest import _login_app_as, _make_user
    from helpers.succession import register_recovery_kit, request_seed_alone_replacement
    from helpers.waiting import session_generation

    driver = app.driver
    user = _make_user(nest_instance)
    seed_hex = bytes(user["signing_key"]).hex()
    # The kit exists from the start; only the replacement arrives mid-session.
    register_recovery_kit(
        nest_instance["url"], actor_id_hex=user["actor_id_hex"], identity_seed_hex=seed_hex
    )

    before_login, _ = alert_sweep_passes(driver) or (0, 0)
    _login_app_as(app, request, nest_instance, user)
    driver.navigate_to("feed")
    # The session-start pass has run and found nothing — nothing is planted yet,
    # so "the banner is down" here is true by construction, not by timing.
    await_sweep_pass_after(driver, before_login, budget_s=ALERT_SWEEP_PASS_S, what="the session-start sweep")
    assert _REPLACEMENT_FRAGMENT not in _alert_text(driver), (
        "precondition: nothing pends yet, so the session-start pass must raise "
        "nothing"
    )
    generation = session_generation(driver)
    assert generation is not None, (
        f"{type(driver).__name__} publishes no 'session_generation' — the "
        "'without a restart' half of this journey cannot be asserted"
    )

    # ── The condition arises mid-session: a thief with the seed parks a
    #    replacement while the app sits open on the feed. ──
    planted, _ = alert_sweep_passes(driver) or (0, 0)
    request_seed_alone_replacement(
        nest_instance["url"], actor_id_hex=user["actor_id_hex"], identity_seed_hex=seed_hex
    )

    # ── The loop's clock comes round (poked, never waited out). ──
    driver.call_command(ALERT_SWEEP_WAKE)
    await_sweep_pass_after(driver, planted, budget_s=ALERT_SWEEP_PASS_S, what="the woken re-sweep")
    text = _poll_banner(driver, _REPLACEMENT_FRAGMENT, present=True)
    assert _REPLACEMENT_FRAGMENT in text, (
        "a replacement parked mid-session must reach the banner on the loop's "
        f"next pass; banner reads {text!r}, error surface: {app.error_text()!r}"
    )
    assert session_generation(driver) == generation, (
        "the app tore its session down during the journey — the announcement must "
        f"come without a restart (session_generation {generation} → "
        f"{session_generation(driver)})"
    )
