"""tier_3: the reach hint opens a connected app, and the domain's first answer drops it.

`docs/goal/behavior/onboarding.md` § Reach hint, the ratified dial rule:

    dial the domain first; if that fails to connect … dial the hint …; the first
    successful domain dial deletes the hint.

**Why this exists when the rule is already unit-covered.** `reach_hint_dial.rs`
drives the policy over a scripted connector — it proves `LaunchMachine` asks for
the dials in the right order and deletes on the right outcomes. What it cannot
prove is that a *real relaunched app*, reading a *real* account out of its own
long-term store, reaches the box over a hint and comes up connected: every link
between the persisted slot and the socket — `RegistryLaunchPersistence`'s
`load_reach_ipv4`, the socket-address derivation, `connect_resolving`, the
launch surface's routing — is mocked out up there. The hint's entire purpose is
that the user "signed in as the owner" does not meet a disconnected app, and
that sentence is only true end to end.

**The state under test cannot be produced by using the app.** The hint is
observable only while the domain is unreachable *and* the hint reaches the box,
and the hint is a bucket-1 fact captured automatically at the wizard's
`LoggedIn` terminal — never a knob, so no user gesture creates that disagreement
on demand. So the account's two slots are moved through the shared E2E bridge
(`fauna_client_accounts::call_registry_method_for_test`); that is fixture setup,
which `architecture/e2e-conventions.md` § point 8 exempts by name. Everything
this test then *asserts* is what the app does on its own at launch.

**How a local nest can stand in for a provisioned box — and why this works where
the pre-claim reach journey could not.** `test_provisioning_claims_a_real_nest.py`
records that the *pre-claim* dial mechanisms pin port 443, so a harness nest on a
random high port is out of their reach. The **post-claim** hint does not: it
takes the port from the URL's own authority (`auth.rs::hint_socket_addr`), so
`http://nest.invalid:<port>` + `127.0.0.1` dials the harness nest exactly. The
domain half is an RFC 6761 `.invalid` host, which is guaranteed never to resolve
— the same door `test_onboarding_launch_routing_smoke.py`'s DNS-fail case uses,
and the reason its baseline (no hint) is the retry surface that phase C asserts.

**Phase C is the deletion proof, and it is deliberately behavioural.** Reading
the slot back says the client *believes* it deleted the hint; taking the domain
away again and finding the app can no longer reach the box says the hint is
genuinely gone. Both are asserted, in that order, so a failure says which.

**Phase C is also phase A's negative control, which is why this journey carries
its own red-verify.** The two launches differ in exactly one bit — whether the
account holds a hint — and they must land on opposite surfaces: A on the
authenticated app, C on `launch-retry-button`. So a build where the hint is
never dialled fails A, and a build where a dialled hint is never *dropped* fails
C; there is no state of the world that satisfies both vacuously. That is worth
more here than a hand-reverted run, because the expensive half of this test is
the queue, not the assertions.
"""

from __future__ import annotations

import json
from urllib.parse import urlsplit

import pytest

from common.cred_store import requires_secret_service
from common.keyring import secret_service_available
from common.launch_harness import make_launch_harness, reached_authenticated_app
from conftest import _trust_seeder, get_available_apps
from helpers.app_surface import skip_environment, skip_unbuilt

pytestmark = pytest.mark.tier_3

#: The retry surface an account with no usable address lands on
#: (`onboarding.md` § App-launch routing — the fallback table's reachability row).
LAUNCH_RETRY = "launch-retry-button"

#: Apps whose agent delegates to the shared registry bridge. tui leads, as it
#: does for every new UI feature; the other six inherit the *logic* the moment
#: their agent adds the same one-arm delegation, since the name table and the
#: semantics are already shared Rust.
_WIRED_APPS = ("tui",)

#: A generous ceiling, not a measured latency (`e2e-conventions.md` § point 14):
#: far above any non-pathological launch on a heavily loaded build machine, and
#: the wait returns the instant its state is reached.
ROUTE_S = 90


def _apps():
    available = get_available_apps()
    return [c for c in ("tui", "linux", "web", "macos", "windows", "android", "ios") if c in available]


@pytest.fixture(params=_apps())
def reach_app(request):
    """The app under test; its id lands in the test name, which conftest's
    `--app` filter reads."""
    return request.param


@pytest.fixture
def reach_harness(request, reach_app, tmp_path):
    """A launch harness for `reach_app`, over the file-backed credential store.

    File-backed on purpose: the reach hint lives on the shared `AccountRegistry`,
    which honours `FAUNA_E2E_CREDENTIAL_DIR` independently of an unlocked
    keyring, so this journey is headless-safe. The store's dirs are pinned into
    the launch config, so everything the app persists survives `relaunch()` —
    which is the whole point here, since the slot under test is client-durable.

    Every app builds a harness, including the ones that cannot run the body yet:
    the unbuilt-surface skip needs a live driver to name the app it is tallying
    against, so it belongs in the test body, one code path for all seven.
    """
    if requires_secret_service(reach_app) and not secret_service_available():
        skip_environment(
            "linux's real-keyring launches run a private gnome-keyring-daemon, which this box cannot supply"
        )
    if reach_app == "web":
        harness = make_launch_harness("web", spa_url=request.getfixturevalue("spa_url"))
    else:
        app_path = request.getfixturevalue(f"{reach_app}_app_path")
        harness = make_launch_harness(
            reach_app, tmp_path=tmp_path, app_path=app_path, file_backed=True,
            seed_trust=_trust_seeder(request),
        )
    try:
        yield harness
    finally:
        harness.teardown()


def _set_reach(driver, **fields):
    """Move the active account's `nest_url` / reach hint through the shared
    bridge. An absent field is "leave it alone" — which is what lets phase B
    change the domain without disturbing the hint the app is about to delete."""
    driver.call_machine_method("set_account_reach_for_test", json.dumps(fields))


def _read_reach(driver):
    """The active account's `nest_url` + `reach_ipv4`, as the app's own registry
    holds them. Agents differ in whether they re-wrap the dispatcher's JSON, so
    accept both shapes — the same leniency `test_nest_identity_pin.py::_read_pin`
    applies for the same reason."""
    raw = driver.call_machine_method("account_reach_for_test", "null")
    if isinstance(raw, dict):
        return raw
    if raw in (None, "", "null"):
        return {}
    return json.loads(raw)


@pytest.mark.feature("connect-and-sign-in")
# The hint it captures is 127.0.0.1 and the dark domain keeps the harness nest's
# own explicit port: the journey needs a nest on this machine's loopback.
@pytest.mark.harness_box
def test_the_hint_opens_a_connected_app_and_the_domains_first_answer_drops_it(
    reach_harness, reach_app, nest_instance, test_user
):
    harness = reach_harness
    reachable = nest_instance["url"]
    # Same port, a host that RFC 6761 guarantees will never resolve. The port
    # matters: the hint dial takes it from this URL's own authority, so a harness
    # nest on a random high port is reachable through the hint.
    port = urlsplit(reachable).port
    assert port, f"the harness nest url must carry an explicit port, got {reachable!r}"
    dark = f"http://reach-hint-domain.invalid:{port}"

    # ── Phase 0 — an ordinary signed-in account, reachable by its domain. ─────
    # A REGISTERED identity: an unknown one would answer `NotRegistered` and route
    # to the invite surface, so every later phase would be asserting the wrong
    # fork of the launch table.
    driver = harness.launch(
        secret_hex=bytes(test_user["signing_key"]).hex(), node_url=reachable,
        trust=nest_instance,
    )
    if reach_app not in _WIRED_APPS:
        skip_unbuilt(
            driver,
            surface="the registry E2E bridge delegation (`set_account_reach_for_test`)",
            detail="one arm in this app's agent handing "
            "`fauna_client_accounts::call_registry_method_for_test` its own "
            "`AccountRegistry` — the name table and semantics are already shared "
            "Rust; web additionally cannot dial a hint until the IP bridge cert "
            "ships (`onboarding.md` § Reach hint)",
            tracked="behavior/onboarding.md § E2E bridge contract — the registry "
            "dispatcher's per-app delegation, and § Implementation status today's "
            "reach bullet for the capture legs that give each app a hint to dial",
        )
    reached_authenticated_app(driver, timeout=ROUTE_S)

    # ── Phase A — the domain goes dark; the hint knows the way. ───────────────
    # This is wizard exit as the user actually meets it: the box was claimed
    # minutes ago and its zone has not published, so the ONLY address that
    # reaches it is the one the wizard captured.
    _set_reach(driver, nest_url=dark, reach_ipv4="127.0.0.1")
    harness.relaunch()
    reached_authenticated_app(driver, timeout=ROUTE_S)
    assert driver.is_absent(LAUNCH_RETRY), (
        "an account whose domain does not resolve but whose reach hint reaches "
        "the box must open CONNECTED, with no retry surface — that is the whole "
        f"promise of the hint. state={_read_reach(driver)}"
    )

    # ── Phase B — the domain answers again; the hint has served its purpose. ──
    # Only the url moves: the hint must be deleted by the APP, on its first
    # successful domain dial, not by the harness.
    _set_reach(driver, nest_url=reachable)
    harness.relaunch()
    reached_authenticated_app(driver, timeout=ROUTE_S)
    after = _read_reach(driver)
    assert after.get("nest_url") == reachable, (
        f"precondition for the deletion assert: the account must be back on its "
        f"reachable domain, got {after}"
    )
    assert after.get("reach_ipv4") is None, (
        "the first successful domain dial must delete the hint "
        f"(`onboarding.md` § Reach hint), still holding {after.get('reach_ipv4')!r}"
    )

    # ── Phase C — the deletion is real, not merely reported. ─────────────────
    # Take the domain away again WITHOUT restoring a hint. If phase B had only
    # *reported* a deletion, the stale hint would still be on disk and this
    # launch would come up connected — which is exactly the failure a slot-read
    # alone cannot see.
    _set_reach(driver, nest_url=dark)
    harness.relaunch()
    driver.wait_for(LAUNCH_RETRY, timeout=ROUTE_S)
    assert driver.is_visible(LAUNCH_RETRY), (
        "with the domain dark and the hint deleted there is no address left, so "
        "the launch must land on the retry surface; reaching the app here would "
        "mean the hint outlived the domain dial that was supposed to drop it. "
        f"state={_read_reach(driver)}"
    )
