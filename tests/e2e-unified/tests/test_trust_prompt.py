"""The one-tap "trust this box" offer — both positions § 3b-ter promises it in:
at the end of an admin claim, and at a joining user's first login.

`docs/goal/behavior/onboarding.md` § 3b-ter — the one ratified survivor of the
retired claim-time trust question (`storage-modes.md` § What replaced each
piece of the axis). ui.yaml page `onboarding.trust_prompt`.

These drive the REAL ceremony over the wire — `fauna.auth.claim_admin` for the
claim twins, a real `register` for the join twins, no injected session —
because the page's whole premise is *where it sits in the flow*: an
interstitial the arriving route parks on, after the commit and before the app.
A `set_state`-injected session skips the wizard entirely and could not observe
it.

What each half pins:

* **the offer exists and both answers conclude onboarding** — the routing, on
  the app that declares the capability;
* **declining changes nothing** — § 3b-ter's own words, and the reason the
  screen may exist at all: an offer that cost the user something by appearing
  would not be optional.

The mint the grant button triggers is deliberately NOT asserted here. It runs at
the signed-in handoff against whatever the shared catalog derives, and on a
fresh loopback nest with no mail enabled and no content processor enrolled that
set is legitimately empty (`fauna_client_capabilities::view_model::mint_options`
offers only what would actually mint). Its behaviour is pinned where it is
observable without that scaffolding — `fauna-client-pair`'s
`the_default_set_mints_every_option_the_shared_catalog_derives` and its two
siblings. What this file adds is that the gesture is REACHABLE and does not
strand onboarding, which no unit test can say.
"""

from __future__ import annotations

import json as _json
import secrets
import sys
import time
from pathlib import Path

import pytest
from nacl.signing import SigningKey

_e2e_dir = str(Path(__file__).resolve().parent.parent)
if _e2e_dir not in sys.path:
    sys.path.insert(0, _e2e_dir)

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.nest import CLAIM_CODE
from conftest import MAIL_PRIMARY_DOMAIN
from helpers.authenticated_shell import wait_for_authenticated_shell
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.tier1]


@pytest.fixture
def unclaimed_trust_nest(request, nest_mode, tmp_path_factory):
    """A fresh, NEVER-claimed nest (claim code = `common.nest.CLAIM_CODE`);
    the journeys below drive the real UI claim against it.

    ``unclaimed`` is honoured in every mode (the docker provider declares it),
    so the claim ceremony these journeys drive is the same one in a container.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "trust-prompt-nest", unclaimed=True)
    yield nest
    cleanup()


def _claim_to_nat_mode(app, nest, secret_hex: str) -> None:
    """Import an identity and drive the real claim, parking on
    `nat_mode_choice`.

    `navigate_to_claim_code_for_known_nest` skips DNS discovery (the same
    shortcut `test_crash_recovery_journeys`'s mid-claim journey takes) — the
    handle probe is not what these tests are about, and faking it keeps them
    off the network.
    """
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)
    app.driver.wait_for("handle-input", timeout=UI_SETTLE_S)
    app.driver.call_machine_method(
        "navigate_to_claim_code_for_known_nest",
        _json.dumps([nest["url"], "admin@localhost"]),
    )
    app.driver.wait_for("claim-code-input", timeout=RPC_ROUNDTRIP_S)
    app.driver.clear_and_type("claim-code-input", CLAIM_CODE)
    app.driver.click("claim-code-submit-button")
    # The claim is a real WS-RPC round trip on a cold nest — RPC_ROUNDTRIP_S
    # is a ceiling, not a target.
    app.driver.wait_for("nat-mode-confirm-button", timeout=RPC_ROUNDTRIP_S)


def _wait_for_the_app(app) -> str:
    """Wait for the authenticated shell and return the marker that appeared.

    Thin alias for the shared landing check — this file's own copy of the marker
    tuple is what traced both windows
    hangs to (it polled only nav entries, which windows renders offscreen, plus
    an iOS-only ID), so the set now lives in exactly one place.
    """
    return wait_for_authenticated_shell(app)


def _require_the_prompt(app, *, leaving: str) -> None:
    """Demand the offer — every app now declares `set_renders_trust_prompt`
    (tui led 2026-08-14; linux/android/web the same day; macOS+iOS 2026-08-24;
    windows closes the set), so an
    absent prompt is always the routing regression this file exists to catch
    (convention 7 — *a skip is not coverage*: a `skip_unbuilt` here, on an app
    that has in fact declared the capability, would report the regression as
    a tidy `s` in the summary line).

    The wait is on the offer APPEARING (convention 14). `leaving` names the
    step the route left — `nat_mode_choice` on the claim path, the invite
    redemption on the join path (`nat_mode_choice` is admin-claim only,
    onboarding.md § 3b-ter): the join routes reach the offer straight off an
    async redeem, which web resolves a tick after the click returns, so an
    immediate read there reported the routing regression for what was only
    latency."""
    wait_until(
        app.onboarding.trust_prompt_showing, RPC_ROUNDTRIP_S, interval=0.5,
        diagnose=lambda: (
            "every app declares set_renders_trust_prompt, so leaving "
            f"{leaving} must land on the trust offer — it did not, which "
            "means the routing regressed (onboarding.md § 3b-ter). error="
            f"{app.error_text()!r}\n{app.driver.tree()}"
        ),
    )


@pytest.mark.feature("claim-a-fresh-nest")
def test_the_claim_offers_the_one_tap_trust_before_the_app(
    app, unclaimed_trust_nest
):
    """A successful admin claim reaches the offer, and it carries all three of
    its ratified elements — the summary a user decides from, and both answers.

    The summary is asserted NON-EMPTY rather than by wording: this is the one
    screen in onboarding whose whole content is an explanation, and a page that
    rendered two buttons over a blank line would look built and be useless
    (`ui/README.md` § Copy comprehensibility)."""
    secret_hex = bytes(range(32)).hex()
    _claim_to_nat_mode(app, unclaimed_trust_nest, secret_hex)

    # `trust=None` — leave the interstitial up; this test is about it.
    app.onboarding.finish_nat_mode(trust=None)
    _require_the_prompt(app, leaving="nat_mode_choice")

    assert app.driver.is_visible("trust-box-summary")
    assert app.driver.get_text("trust-box-summary").strip(), (
        "the offer must say what it covers — a bare pair of buttons is not a "
        "decision the user can make"
    )
    assert app.driver.is_enabled("trust-box-grant-button")
    assert app.driver.is_enabled("trust-box-skip-button")
    assert not app.has_error(), f"unexpected error on the offer: {app.error_text()!r}"


@pytest.mark.feature("claim-a-fresh-nest")
def test_declining_the_offer_leaves_everything_as_today(app, unclaimed_trust_nest):
    """§ 3b-ter's own promise, made assertable: skip concludes onboarding into
    the app exactly as the NAT step alone used to."""
    secret_hex = bytes(range(1, 33)).hex()
    _claim_to_nat_mode(app, unclaimed_trust_nest, secret_hex)

    app.onboarding.finish_nat_mode(trust=None)
    _require_the_prompt(app, leaving="nat_mode_choice")

    assert app.onboarding.finish_trust_prompt(grant=False)
    _wait_for_the_app(app)


@pytest.mark.feature("claim-a-fresh-nest")
def test_accepting_the_offer_also_reaches_the_app(app, unclaimed_trust_nest):
    """The grant answer concludes onboarding too.

    The mint it triggers is best-effort glue on the far side of the handoff
    (`onboarding.md` § 3b-ter; the account is fully usable without it), so what
    must hold here is that granting never strands the wizard or paints an error
    over a completed onboarding — the failure mode a fire-and-forget call
    bolted onto the last screen would otherwise have."""
    secret_hex = bytes(range(2, 34)).hex()
    _claim_to_nat_mode(app, unclaimed_trust_nest, secret_hex)

    app.onboarding.finish_nat_mode(trust=None)
    _require_the_prompt(app, leaving="nat_mode_choice")

    assert app.onboarding.finish_trust_prompt(grant=True)
    _wait_for_the_app(app)
    assert not app.has_error(), (
        f"granting must not surface an error on the app: {app.error_text()!r}"
    )


# ── The joiner's half of § 3b-ter ──────────────────────────────────────────
#
# "Shown after a successful admin claim, **and offered at a joining user's
# first login**". The claim half above shipped 2026-08-14; the joining half
# reached no app until the machine's four hand-written `LoggedIn` exits were
# collapsed into one gated one, so `docs/features/join-a-nest.md` outcome 8
# cited `(none)` for a year of sessions.
#
# These twins are the witness for that outcome, and they are deliberately the
# same two assertions the claim twins make — reachable, and both answers
# conclude onboarding — because the joiner's offer is the SAME page reached
# from a different door, and a second door is exactly what a routing test is
# for.
#
# **What they deliberately do not assert: the grant appearing on the Nests
# page.** Not an omission, and not the same judgement call as the claim twins'
# — § 3b-ter's own second honest bound states it: the shared catalog "offers
# only what would actually mint", and `mint_options` derives nothing without an
# enrolled MDA holder, so on a bare fixture nest with mail not enabled the tap
# legitimately mints an empty set. "That is an honest no-op, not an error."
# Asserting a visible grant here would therefore assert something the goal doc
# says may not happen, i.e. it would be red for a correct product. The mint's
# own behaviour is pinned where it is observable —
# `fauna-client-pair::the_default_set_mints_every_option_the_shared_catalog_derives`
# and its two siblings.


@pytest.fixture
def joiner_nest(app, request, registration_posture_nest):
    """`registration_posture_nest`, plus the URL this app's wizard dials it by
    (`dial_url`), with web's onboarding routed at it for the whole test.

    Native apps dial the nest itself. Web dials `registration_posture_spa_url`
    under `_web_onboarding_routed_to`: until the join's `LoggedIn` terminal
    records a nest, a web page resolves every dial to its own origin — the
    SESSION nest — so without the route the redemption's authenticated leg
    asked a box the joiner never registered on (`fauna.auth.not_registered`)
    and the offer never came."""
    from conftest import _web_onboarding_routed_to

    dial_url = registration_posture_nest["url"]
    if app.driver.is_web():
        dial_url = request.getfixturevalue("registration_posture_spa_url")
    with _web_onboarding_routed_to(app.driver, dial_url):
        yield {**registration_posture_nest, "dial_url": dial_url}


def _mint_invite_code(nest) -> str:
    """Mint a one-use OOB invite code as the nest admin. Idiom:
    `test_bearer_cache_web.py::_mint_invite_code`."""
    admin = nest["admin"]["signing_key"]
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin.verify_key),
        signing_key=bytes(admin),
    ) as ws:
        reply = ws.call("fauna.admin.invite_codes.create", {"tier": "free", "uses": 1})
    code = reply["code"]
    assert code, f"admin invite mint returned no code: {reply!r}"
    return code


def _redeem_to_the_offer(app, nest, secret_hex: str, code: str) -> None:
    """Join `nest` by redeeming `code`, stopping on the one-tap trust offer.

    `registration_posture_nest` is the fixture because redemption needs an OPEN
    posture — the shared `nest_instance` gates it regardless of a valid code,
    which is why `test_family.py`'s invite-code leg can only peek at one. Same
    fixture, same reason, as the two web redeem journeys.

    `navigate_to_invite_request_for_known_nest` skips handle discovery exactly
    as the claim helper above skips DNS: the probe is not what this file is
    about. The rest is the real thing — a real `register` over the wire, after
    which this identity genuinely exists on that box.
    """
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)
    app.driver.wait_for("handle-input", timeout=UI_SETTLE_S)
    handle = "joiner" + secrets.token_hex(3) + "@" + MAIL_PRIMARY_DOMAIN
    app.driver.call_machine_method(
        "navigate_to_invite_request_for_known_nest",
        _json.dumps([nest["dial_url"], handle]),
    )
    app.driver.wait_for("invite-code-input", timeout=RPC_ROUNDTRIP_S)
    app.driver.clear_and_type("invite-code-input", code)
    app.driver.click("invite-code-check-button")

    # Continue is gated on `oob-code-valid` (ui.yaml `invite_request`), so the
    # check has to land before the click — clicking a still-disabled button is
    # a silent no-op that would surface only as a confusing timeout further
    # down. `is_enabled` is False for an absent element too, so this waits for
    # the gate itself (idiom: `test_bearer_cache_web.py`).
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline:
        if app.driver.is_enabled("invite-request-continue-button"):
            break
        time.sleep(0.5)
    else:
        raise AssertionError(
            "the OOB code never validated: `invite-request-continue-button` "
            "stayed disabled, so `oob-code-valid` was never reached. status: "
            f"{app.driver.get_text('invite-code-status')!r}; error: "
            f"{app.error_text()!r}"
        )

    app.driver.click("invite-request-continue-button")


@pytest.mark.feature("join-a-nest")
def test_joining_by_invite_code_offers_the_one_tap_trust(
    app, joiner_nest
):
    """A joining user reaches the same offer a claiming admin gets, carrying
    all three of its ratified elements.

    This is `join-a-nest` outcome 8. Before the machine's exits were unified,
    a redemption set `LoggedIn` inline and dropped the user straight into the
    app — so this test's failure mode is not a missing button but a missing
    *page*, and `_require_the_prompt` says so with the tree attached."""
    code = _mint_invite_code(joiner_nest)
    secret_hex = bytes(SigningKey.generate()).hex()

    _redeem_to_the_offer(app, joiner_nest, secret_hex, code)
    _require_the_prompt(app, leaving="the invite redemption")

    assert app.driver.is_visible("trust-box-summary")
    assert app.driver.get_text("trust-box-summary").strip(), (
        "the offer must say what it covers — a bare pair of buttons is not a "
        "decision the user can make"
    )
    assert app.driver.is_enabled("trust-box-grant-button")
    assert app.driver.is_enabled("trust-box-skip-button")
    assert not app.has_error(), f"unexpected error on the offer: {app.error_text()!r}"


@pytest.mark.feature("join-a-nest")
def test_the_joiner_can_decline_and_still_reach_the_app(
    app, joiner_nest
):
    """The decline twin, and the load-bearing one for a JOIN.

    § 3b-ter's "declining leaves everything as today" has sharper teeth here
    than on the claim path: the account has already been created on the nest by
    the time the offer appears, so an offer that could strand its answer would
    strand a user who is registered but has never reached the app — a state
    they cannot leave from inside the wizard."""
    code = _mint_invite_code(joiner_nest)
    secret_hex = bytes(SigningKey.generate()).hex()

    _redeem_to_the_offer(app, joiner_nest, secret_hex, code)
    _require_the_prompt(app, leaving="the invite redemption")

    assert app.onboarding.finish_trust_prompt(grant=False)
    _wait_for_the_app(app)
    assert not app.has_error(), (
        f"declining must not surface an error on the app: {app.error_text()!r}"
    )


@pytest.mark.feature("join-a-nest")
def test_the_joiner_can_accept_and_still_reach_the_app(
    app, joiner_nest
):
    """The accept twin: granting concludes the join too.

    The mint it triggers is best-effort glue on the far side of the handoff and
    may legitimately mint nothing here (see this section's header), so what
    must hold is that granting never strands the join or paints an error over a
    completed onboarding."""
    code = _mint_invite_code(joiner_nest)
    secret_hex = bytes(SigningKey.generate()).hex()

    _redeem_to_the_offer(app, joiner_nest, secret_hex, code)
    _require_the_prompt(app, leaving="the invite redemption")

    assert app.onboarding.finish_trust_prompt(grant=True)
    _wait_for_the_app(app)
    assert not app.has_error(), (
        f"granting must not surface an error on the app: {app.error_text()!r}"
    )


# ── The fifth door: the manual-DNS `already_claimed` resume ────────────────
#
# § 3b-ter's last route (ratified 2026-09-21): a box provisioned with DNS
# deferred parks the user on "Almost ready"; when the box is reachable and
# ALREADY claimed by this identity — our claim landed, its reply was lost — the
# recheck proves ownership by the silent challenge, never by a second claim, and
# concludes through the offer, skipping the NAT page. The routing is shared Rust
# (`complete_manual_dns_claim`, pinned by `fauna-onboarding-machine`'s
# `the_manual_dns_resume_reaches_the_offer_too`); what this journey adds is that
# the surface's recheck really gets there on a running app.
#
# It also carries the crash leg of the long-term store's clearing ruling
# (`onboarding.md` § Long-term store contract, *Mechanism*): the awaiting-DNS
# slot is spent at `LoggedIn` and never earlier, so a crash ON the offer
# relaunches back into "Almost ready", whose poll takes the resume and asks once
# more. A build that cleared at the claim reds the first relaunch (it lands in
# the app with the offer lost); one that never clears reds the second (it lands
# on "Almost ready" after the user already answered). The pair red-verifies
# itself.
#
# **The door is the real deferred-DNS exit, not a seeded slot** — the same one
# `test_onboarding_launch_routing_smoke.py` case I clicks. The slot is written by
# the app's own `handle_wizard_done`, with no reach address, so the probe dials
# the slot's `nest_url` directly rather than the :443 a real provisioned box
# answers on, which is what lets a loopback nest stand in for it.

AWAITING_DNS_RECORDS = "awaiting-dns-records"
AWAITING_DNS_RECHECK = "awaiting-dns-recheck-button"
NAT_MODE_STATUS = "nat-mode-status"
TRUST_BOX_SUMMARY = "trust-box-summary"

#: The record the "Almost ready" page lists — display only; the probe never
#: resolves it (it dials the slot's `nest_url`).
RESUME_RECORD = "A nest.trust-resume.invalid 203.0.113.9"

#: A ceiling, never a settle time (convention 14): the resume is one
#: `fauna.setup.status` probe plus the silent challenge, reached either by the
#: tap below or by the surface's own poll timer, after an app launch.
RESUME_S = 90.0


def _resume_apps() -> list[str]:
    from conftest import get_available_apps

    available = get_available_apps()
    return [
        c for c in ("tui", "linux", "web", "macos", "windows", "android", "ios")
        if c in available
    ]


@pytest.fixture
def trust_resume_nest(request, nest_mode, tmp_path_factory):
    """A dedicated, CLAIMED nest whose admin identity the journey signs in as.

    Not the session ``nest_instance``: the grant answer mints the default set
    against whatever the nest offers, and a mint onto the shared nest's admin
    would outlive this test. Function scope, so it dies with the answer."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "trust-resume-nest")
    yield nest
    cleanup()


@pytest.fixture(params=_resume_apps())
def resume_harness(request, tmp_path, trust_resume_nest):
    """A launch harness for the app under test, over the file-backed store.

    File-backed on purpose, as for `test_reach_hint_dial_journey.py`: the
    awaiting-DNS slot lives on the shared `AccountRegistry`, which honours
    `FAUNA_E2E_CREDENTIAL_DIR` independently of an unlocked keyring, so the
    journey is headless-safe. Everything the app persists survives
    `relaunch()`, which is the whole point — the slot under test is
    client-durable. Yields ``(harness, node_url)``: web reaches the nest through
    its own SPA proxy (a browser cannot dial the raw nest origin)."""
    from common.launch_harness import make_launch_harness
    from conftest import _serve_spa_proxy, _trust_seeder

    client = request.param
    server = None
    if client == "web":
        spa_url, server = _serve_spa_proxy(
            request.getfixturevalue("static_dir"), trust_resume_nest["url"])
        harness = make_launch_harness("web", spa_url=spa_url)
        node_url = spa_url
    else:
        harness = make_launch_harness(
            client, tmp_path=tmp_path,
            app_path=request.getfixturevalue(f"{client}_app_path"),
            file_backed=True, seed_trust=_trust_seeder(request),
        )
        node_url = trust_resume_nest["url"]
    try:
        yield harness, node_url
    finally:
        harness.teardown()
        if server is not None:
            server.shutdown()


def _exit_to_almost_ready(app, node_url: str, secret_hex: str) -> None:
    """Take the real deferred-DNS exit with the nest's own admin identity loaded.

    Arranged, as case I arranges it: the imported key, the wizard's `nest_url`,
    and the provisioning result the exit reads its claim code from. Clicked: the
    exit itself, whose `handle_wizard_done` writes the awaiting-DNS slot."""
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)
    app.driver.wait_for("handle-input", timeout=UI_SETTLE_S)
    app.driver.call_machine_method("set_nest_url", _json.dumps(node_url))
    ob.go_to_dns_post_instructions_with_records([RESUME_RECORD])
    app.click("dns-post-instructions-continue-button")


def _resume_to_the_offer(app, *, what: str) -> None:
    """Wait for the resume to land on the offer, never passing the NAT page.

    The surface polls on its own timer; the recheck tap only shortens the wait,
    so it is tried once and tolerated if the poll already routed the page away
    between the visibility read and the click. Every tick also asserts the NAT
    page is not up — the resume skips the setup tail, and a build that routed it
    through `NatModeChoice` would pass through this loop, not stop in it."""
    driver = app.driver
    tapped = False

    def arrived():
        nonlocal tapped
        assert driver.is_absent(NAT_MODE_STATUS), (
            f"{what}: the already-claimed resume showed the NAT page — it must "
            "skip the setup tail and conclude through the offer "
            f"(onboarding.md § 3b-ter).\n{driver.tree()}"
        )
        if not tapped and driver.is_visible(AWAITING_DNS_RECHECK):
            tapped = True
            try:
                driver.click(AWAITING_DNS_RECHECK)
            except Exception:  # the poll won the race and moved the page
                pass
        return driver.is_visible(TRUST_BOX_SUMMARY)

    wait_until(
        arrived, RESUME_S, interval=0.5,
        diagnose=lambda: (
            f"{what}: never reached the trust offer. awaiting-dns visible="
            f"{driver.is_visible(AWAITING_DNS_RECORDS)}, error="
            f"{app.error_text()!r}\n{driver.tree()}"
        ),
    )
    assert driver.get_text(TRUST_BOX_SUMMARY).strip(), (
        "the offer must say what it covers — a bare pair of buttons is not a "
        "decision the user can make"
    )


def _crash_and_relaunch(harness) -> None:
    """Force-quit — a kill, never a clean exit — then relaunch over the kept
    store. web's kill closes the page and keeps its BrowserContext, so the
    relaunch reads back the genuine localStorage (`drivers/web.py`)."""
    harness.driver.kill_uncleanly()
    harness.relaunch()


@pytest.mark.feature("claim-a-fresh-nest")
@pytest.mark.parametrize("grant", [False, True], ids=["decline", "grant"])
def test_the_manual_dns_resume_offers_the_trust_and_survives_a_crash_on_it(
    resume_harness, trust_resume_nest, grant
):
    """The "Almost ready" recheck, on a box this identity already claimed,
    reaches the one-tap offer; a crash on the offer brings it back; the answer
    spends the awaiting-DNS slot.

    Three launches, three assertions about where each one lands:

    1. the deferred-DNS exit → "Almost ready" → the resume → the offer, never
       the NAT page (§ 3b-ter's fifth route);
    2. a kill on the offer → relaunch → the offer again, via "Almost ready"'s
       resume (the slot survived the claim — spent only at `LoggedIn`);
    3. the answer → the app; a kill → relaunch → the app, not "Almost ready"
       (the slot is gone).

    Leg 2 waits for the OFFER rather than for "Almost ready" on purpose: the
    surface's first poll may win the race to the offer before any read of the
    records element, and the offer after a relaunch has exactly one source — the
    slot's resume. A build that spent the slot at the claim lands in the app
    instead, which the wait's diagnosis names."""
    from actions import ActionLayer
    from helpers.app_surface import skip_unbuilt

    harness, node_url = resume_harness
    secret_hex = bytes(trust_resume_nest["admin"]["signing_key"]).hex()

    # web reads no environment, so no trust seed names the nest for it.
    trust = None if harness.client == "web" else trust_resume_nest
    driver = harness.launch(node_url=node_url, trust=trust)
    if not driver.supports_unclean_kill():
        skip_unbuilt(
            driver,
            surface="the unclean-kill primitive (driver-owned Popen/pty child)",
            detail="the crash-on-the-offer leg needs a real kill — a graceful "
            "quit could flush what a crash would lose — see "
            "PlatformDriver.kill_uncleanly",
            tracked="testing.md",
        )
    app = ActionLayer(driver)

    # ── Leg 1: the real exit, then the resume.
    _exit_to_almost_ready(app, node_url, secret_hex)
    _resume_to_the_offer(app, what="the same-session resume")
    assert not app.has_error(), f"unexpected error on the offer: {app.error_text()!r}"

    # ── Leg 2: crash on the offer. The slot must still be there.
    _crash_and_relaunch(harness)
    app = ActionLayer(harness.driver)
    _resume_to_the_offer(
        app, what="the relaunch after a crash on the offer (a build that "
        "spends the awaiting-DNS slot at the claim lands in the app here)")

    # ── Leg 3: answer, then crash again. The slot must be gone.
    assert app.onboarding.finish_trust_prompt(grant=grant)
    wait_for_authenticated_shell(app)
    assert not app.has_error(), (
        f"answering the offer must not surface an error: {app.error_text()!r}"
    )
    _crash_and_relaunch(harness)
    app = ActionLayer(harness.driver)
    wait_for_authenticated_shell(app)
    assert app.driver.is_absent(AWAITING_DNS_RECORDS), (
        "the relaunch after answering the offer came back to 'Almost ready' — "
        "the awaiting-DNS slot must be spent at LoggedIn "
        "(onboarding.md § Long-term store contract)"
    )
    assert not app.onboarding.trust_prompt_showing(), (
        "the offer is asked once per resume — an answered offer must not return"
    )
