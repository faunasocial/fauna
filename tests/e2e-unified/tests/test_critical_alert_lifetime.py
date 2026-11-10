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
_REPLACEMENT_FRAGMENT = "replacement of your recovery kit"


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
# (`test_account_switcher_<app>.py`): tui, linux, windows, macOS, iOS and
# android seed the registry at launch (apple into the Keychain, android into its
# on-device credential file over the bridge; windows reads the registry alone,
# its legacy mirror retired), web seeds localStorage and re-boots over it.
SWITCH_APPS = ("tui", "linux", "web", "windows", "macos", "ios", "android")


def _switch_apps() -> list[str]:
    from conftest import get_available_apps

    available = get_available_apps()
    return [a for a in SWITCH_APPS if a in available]


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
        driver.launch({"url": spa_url + "/app/", "seed_credentials": seed})
        _auth_on_account_page(
            driver, bytes(user_a["signing_key"]).hex(), "a", user_a["actor_id_hex"], spa_url
        )
        return driver

    url = nest_instance["url"]
    if switch_app in ("macos", "ios"):
        from helpers.app_launch import app_launch_config

        launch = app_launch_config(switch_app, request)
    elif switch_app == "android":
        from tests.test_account_switcher_android import _launch_config

        launch = _launch_config(request)
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


@pytest.mark.feature("critical-alerts")
def test_a_condition_arising_after_a_same_identity_re_establish_is_announced(
    app, nest_instance, request
):
    """The loop keeps sweeping on the session's LIVE connection after the same
    identity re-establishes — a condition that arises afterwards still reaches
    the banner on the loop's next pass.

    `critical-alerts.md` § Mechanism → *How often the detector runs*: a loop
    rides one client and ends with it. An app that rebuilds its client on a
    same-identity re-establish (windows disposes the session's client and builds
    another on every one) must start the new client's loop; one that left the
    identity's only loop on the disposed client had every later pass fail at its
    first nest read, so nothing was announced again until a restart.
    An app that keeps its client keeps its loop, and this journey holds for it
    unchanged.

    Same poke as the mid-session journey: ``alert_sweep_wake`` ends the loop's
    wait instead of waiting out its six hours (convention 14), and the barrier is
    the sweep's own pass counter.
    """
    from conftest import _login_app_as, _make_user
    from helpers.succession import register_recovery_kit, request_seed_alone_replacement

    driver = app.driver
    user = _make_user(nest_instance)
    seed_hex = bytes(user["signing_key"]).hex()
    register_recovery_kit(
        nest_instance["url"], actor_id_hex=user["actor_id_hex"], identity_seed_hex=seed_hex
    )

    before_login, _ = alert_sweep_passes(driver) or (0, 0)
    _login_app_as(app, request, nest_instance, user)
    driver.navigate_to("feed")
    await_sweep_pass_after(driver, before_login, budget_s=ALERT_SWEEP_PASS_S, what="the session-start sweep")

    # ── The same identity re-establishes: the post-auth hook runs again on the
    #    same actor (a second `_login_app_as`, test_session_start_alert_sweep.py's
    #    technique). ──
    before_reestablish, _ = alert_sweep_passes(driver) or (0, 0)
    _login_app_as(app, request, nest_instance, user)
    driver.navigate_to("feed")
    await_sweep_pass_after(
        driver, before_reestablish, budget_s=ALERT_SWEEP_PASS_S, what="the re-establish's pass"
    )
    assert _REPLACEMENT_FRAGMENT not in _alert_text(driver), (
        "precondition: nothing pends yet, so no pass may raise the replacement alarm"
    )

    # ── The condition arises after the re-establish, and the loop's clock comes
    #    round (poked, never waited out). ──
    planted, _ = alert_sweep_passes(driver) or (0, 0)
    request_seed_alone_replacement(
        nest_instance["url"], actor_id_hex=user["actor_id_hex"], identity_seed_hex=seed_hex
    )
    driver.call_command(ALERT_SWEEP_WAKE)
    await_sweep_pass_after(driver, planted, budget_s=ALERT_SWEEP_PASS_S, what="the woken re-sweep")
    text = _poll_banner(driver, _REPLACEMENT_FRAGMENT, present=True)
    assert _REPLACEMENT_FRAGMENT in text, (
        "a replacement parked after a same-identity re-establish must reach the banner "
        "on the loop's next pass — a loop left sweeping on the client the re-establish "
        f"tore down fails every pass; banner reads {text!r}, error surface: "
        f"{app.error_text()!r}"
    )


# ── Outcome 5, the arms that end an identity's session without the user
#    leaving it: the nest is no longer the one the app trusted, the identity is
#    succeeded elsewhere, the nest is reset. The first two are driven below;
#    the factory-reset arm has no observable that tells a surviving alert from
#    one the new session re-raised, see the module's closing note. ──

from tests.test_nest_identity_pin_post_auth import (  # noqa: E402
    BOGUS_PIN,
    IDENTITY_WARNING,
    _app_diagnostics,
    _launch_authenticated,
    _read_pin,
    _seed_pin,
    pin_env,  # noqa: F401  (fixture)
)
from common.launch_harness import reached_authenticated_app  # noqa: E402
from helpers.budgets import IDENTITY_REFRESH_S  # noqa: E402
from helpers.succession import register_recovery_kit, request_seed_alone_replacement, succeed_identity  # noqa: E402


@pytest.fixture
def pin_app():
    """The nest-untrust arm is witnessed on tui (the lead app); the pin
    harness's fixture otherwise spans every post-auth app."""
    return "tui"


@pytest.mark.tui
@pytest.mark.feature("critical-alerts")
def test_a_nest_the_app_no_longer_trusts_takes_every_standing_alert_with_it(
    pin_env, request  # noqa: F811
):
    """A's standing alarm does not greet B after A's session is escalated to the
    identity-changed surface (nest-untrust) and B signs in.

    Same logic as the sign-out arm: the alarm is identity-scoped to A, so if it
    is on B's screen after B's first sweep pass, it survived the escalation.
    The pin is put back to the nest's genuine identity before B signs in, so B's
    own session is not itself escalated.
    """
    from actions import ActionLayer
    from conftest import _login_app_as, _make_user

    env = pin_env
    nest = env["nest"]
    user_a = nest["user"]
    driver = _launch_authenticated(env)
    reached_authenticated_app(driver, timeout=90)
    genuine = None

    def _pin_settled():
        nonlocal genuine
        genuine = _read_pin(driver, env["node_url"])
        return genuine not in (None, BOGUS_PIN)

    wait_until(
        _pin_settled, 30,
        diagnose=lambda: f"the launch never pinned the nest's identity; pin={genuine!r}",
    )

    _plant_pending_replacement(nest, user_a)
    started, _ = alert_sweep_passes(driver) or (0, 0)
    driver.call_command(ALERT_SWEEP_WAKE)
    await_sweep_pass_after(driver, started, budget_s=ALERT_SWEEP_PASS_S, what="the woken sweep")
    text = _poll_banner(driver, _REPLACEMENT_FRAGMENT, present=True)
    assert _REPLACEMENT_FRAGMENT in text, (
        "precondition: A's parked replacement must be loud before the nest is "
        f"untrusted; banner reads {text!r}, {_app_diagnostics(driver)}"
    )

    # nest-untrust: a pin the nest can no longer prove blocks the session.
    started, _ = alert_sweep_passes(driver) or (0, 0)
    _seed_pin(driver, env["node_url"], BOGUS_PIN)
    driver.call_command("silent_sign_in")
    try:
        driver.wait_for(IDENTITY_WARNING, timeout=IDENTITY_REFRESH_S)
    except TimeoutError as e:
        raise AssertionError(f"{e}. {_app_diagnostics(driver)}") from e
    _seed_pin(driver, env["node_url"], genuine)

    # B signs in on the same nest.
    user_b = _make_user(nest)
    app = ActionLayer(driver)
    _login_app_as(app, request, nest, user_b)
    driver.navigate_to("feed")
    await_sweep_pass_after(driver, started, budget_s=ALERT_SWEEP_PASS_S, what="B's first sweep")
    text = _alert_text(driver)
    assert _REPLACEMENT_FRAGMENT not in text, (
        "A's alarm survived the nest-untrust escalation and now accuses B — "
        "nest-untrust must drop every alert (critical-alerts.md § Mechanism → "
        f"Lifetime); banner reads {text!r}"
    )


@pytest.mark.tui
@pytest.mark.feature("critical-alerts")
def test_a_superseded_identity_takes_every_standing_alert_with_it(
    succeedable_app, nest_instance, request
):
    """A's standing alarm does not greet A's SUCCESSOR after the live session is
    superseded and the successor's seed is imported.

    The successor is a different actor, so nothing its own sweep does can post or
    clear A's identity-scoped alarm: on its screen after its first sweep pass,
    it survived the supersession.
    """
    import secrets

    app, user = succeedable_app
    driver = app.driver
    nest_url = nest_instance["url"]
    seed_hex = bytes(user["signing_key"]).hex()

    kit_secret = register_recovery_kit(
        nest_url, actor_id_hex=user["actor_id_hex"], identity_seed_hex=seed_hex
    )
    request_seed_alone_replacement(
        nest_url, actor_id_hex=user["actor_id_hex"], identity_seed_hex=seed_hex
    )
    started, _ = alert_sweep_passes(driver) or (0, 0)
    driver.call_command(ALERT_SWEEP_WAKE)
    await_sweep_pass_after(driver, started, budget_s=ALERT_SWEEP_PASS_S, what="the woken sweep")
    driver.navigate_to("feed")
    text = _poll_banner(driver, _REPLACEMENT_FRAGMENT, present=True)
    assert _REPLACEMENT_FRAGMENT in text, (
        "precondition: A's parked replacement must be loud before the "
        f"supersession; banner reads {text!r}, error surface: {app.error_text()!r}"
    )

    successor_seed = secrets.token_bytes(32).hex()
    successor_id = succeed_identity(
        nest_url,
        old_actor_id_hex=user["actor_id_hex"],
        recovery_secret_hex=kit_secret,
        successor_seed_hex=successor_seed,
        old_seed_hex=seed_hex,
    )
    started, _ = alert_sweep_passes(driver) or (0, 0)
    driver.call_command("silent_sign_in")
    wait_until(
        lambda: app.is_visible("paste-secret-field"),
        APP_RELAUNCH_S,
        diagnose=lambda: (
            "the live session was never escalated to the identity-import "
            f"surface; error surface: {app.error_text()!r}"
        ),
    )
    # The device comes back as the successor (the import ceremony itself is
    # test_identity_succession_refusal's journey; this one is about the alarm).
    from common.auth import mint_and_hold_token
    from conftest import _login_app_as
    from nacl.signing import SigningKey

    successor_sk = SigningKey(bytes.fromhex(successor_seed))
    successor = {
        "signing_key": successor_sk,
        "actor_id_hex": successor_id,
        "actor_id_bytes": bytes(successor_sk.verify_key),
        "token": mint_and_hold_token(nest_url, successor_sk),
        "handle": user.get("handle"),
    }
    _login_app_as(app, request, nest_instance, successor, verify_live_actor=True)
    driver.navigate_to("feed")
    await_sweep_pass_after(driver, started, budget_s=ALERT_SWEEP_PASS_S, what="the successor's first sweep")
    text = _alert_text(driver)
    assert _REPLACEMENT_FRAGMENT not in text, (
        "A's alarm survived the supersession and now accuses the successor — "
        "a superseded identity must drop every alert (critical-alerts.md § "
        f"Mechanism → Lifetime); banner reads {text!r}"
    )


# ── Outcome 5, the factory-reset arm. The admin re-claims the wiped nest as the
#    SAME identity, so the banner cannot tell a surviving alert from one the new
#    session re-raised; the alert registry's own count (`alert_registry`) can. ──

from tests.test_factory_reset_journeys import (  # noqa: E402
    _APP_S,
    _NEST_FRESH_S,
    _admin_hex,
    _claim_through_the_wizard,
    _seat,
    reset_nest,  # noqa: F401  (fixture)
)
from common.nest import _wait_nest_fresh, resume_after_self_exit  # noqa: E402
from helpers.waiting import alert_state_snapshot  # noqa: E402


@pytest.mark.tui
@pytest.mark.destructive
@pytest.mark.feature("critical-alerts")
def test_a_factory_reset_takes_every_standing_alert_with_it(
    request, reset_nest, tmp_path  # noqa: F811
):
    """The admin's standing alarm is gone from the alert registry once the app
    has been reset, BEFORE the re-claimed session's first sweep pass can speak.

    Non-vacuous: the alarm is read standing in the registry first; the count
    after the reset is read in one snapshot with the pass counters and asserted
    only while no sweep pass has finished since (``completed`` unmoved), so a
    re-raise or a clearing by the fresh session cannot be what we see. The
    re-claim then happens, which is the same-identity sign-in that makes the
    banner a poor witness.
    """
    from actions import ActionLayer
    from common.launch_harness import reached_authenticated_app

    nest = reset_nest
    actor_hex, secret_hex = _admin_hex(nest)
    register_recovery_kit(nest["url"], actor_id_hex=actor_hex, identity_seed_hex=secret_hex)
    request_seed_alone_replacement(
        nest["url"], actor_id_hex=actor_hex, identity_seed_hex=secret_hex
    )
    with _seat(request, "tui", tmp_path, nest) as (harness, node_url):
        driver = harness.launch_and_route(secret_hex=secret_hex, node_url=node_url, trust=nest)
        reached_authenticated_app(driver, timeout=_APP_S)
        app = ActionLayer(driver)
        driver.navigate_to("feed")

        started, _ = alert_sweep_passes(driver) or (0, 0)
        driver.call_command(ALERT_SWEEP_WAKE)
        await_sweep_pass_after(driver, started, budget_s=ALERT_SWEEP_PASS_S, what="the woken sweep")
        text = _poll_banner(driver, _REPLACEMENT_FRAGMENT, present=True)
        assert _REPLACEMENT_FRAGMENT in text, (
            "precondition: the admin's parked replacement must be loud before the "
            f"reset; banner reads {text!r}, error surface: {app.error_text()!r}"
        )
        before = alert_state_snapshot(driver)
        assert before is not None and before[0] >= 1, (
            "precondition: the alert registry must hold the standing alarm "
            f"(count, started, completed) = {before!r}; the app publishes no "
            "'alert_registry' if None"
        )

        app.admin.factory_reset_via_ui()
        resume_after_self_exit(nest)
        _wait_nest_fresh(nest["url"], timeout=_NEST_FRESH_S)

        after = alert_state_snapshot(driver)
        assert after is not None, "the app stopped publishing the alert registry after the reset"
        assert after[2] == before[2], (
            "a sweep pass finished between the reset and the read — the count can "
            f"no longer tell a survivor from a re-raise (before {before!r}, after {after!r})"
        )
        assert after[0] == 0, (
            "the admin's alarm survived the factory reset in the alert registry — a "
            "reset must drop every alert (critical-alerts.md § Mechanism → Lifetime); "
            f"snapshot (count, started, completed) = {after!r}"
        )

        _claim_through_the_wizard(app, nest, "the re-claim after the witnessed reset")
