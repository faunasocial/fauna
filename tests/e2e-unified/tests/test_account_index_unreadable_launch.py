"""tier_3 E2E: an account index this build cannot read reaches the user as
itself — never as fresh onboarding — CROSS-APP (tui, linux, macos, ios,
windows, web).

``docs/goal/behavior/onboarding.md`` § App-launch routing (the account-index
row, checked before every other) and
``docs/goal/architecture/version-compatibility.md`` § 5 item 9 (the two
verdicts and their opposite remedies). The shared ``LaunchMachine`` routing is
tier_1-proven in ``libs/fauna-launch-machine/tests/account_index_refusal.rs``
and each app's own rendering rules (``apps/fauna-tui/src/launch.rs``'s
``route()``; the shared FaunaKit ``LaunchAccountIndexUnreadableView`` apple's
two targets render); this test closes the loop against a REAL binary reading a
REAL on-disk blob.

Mechanism: the install carries an intact seeded identity (the harness's ``secret``/``node_url``) pointed at a real
nest AND a ``fauna/index`` this build cannot use, seeded verbatim before boot
(``CredStore.inject_raw``). Before this surface existed the registry answered
no session account, so the launch fell through to ``identity_choice`` and
offered a brand-new identity to someone whose accounts were sitting intact
behind a blob this build merely could not parse. The seeded identity is here
on purpose: it proves the refusal outranks the silent-challenge row rather
than quietly signing the user into one account of a multi-account install.

The two cases differ in exactly what they may offer, which is the point:

* **A newer build wrote it** (a stamp this build can read says so) — the
  accounts are intact and an update restores them, so the surface offers
  NOTHING: no retry, no fallthrough, no start-over.
* **Malformed** (no readable stamp either) — updating cannot help, so it may
  reach the documented floor, but only through a confirm that states the
  residual first. The confirm is the factory reset and lands on
  ``identity_choice``.

**Multi-app, mirroring ``test_nest_identity_pin.py``'s shape**: macos/ios ride
``AppleFileCredStore`` (the same ``FAUNA_E2E_CREDENTIAL_DIR`` file backend tui
uses), whose ``inject_raw`` passes ``fauna/index`` through verbatim
(``tests/common/cred_store.py`` + apple's ``KeychainSecretStore`` mapping) —
already verified statically, so no extra check is owed before trusting a
green. iOS additionally needs ``ios_setup["udid"]`` alongside ``app_path``
(``drivers/ios.py``'s ``launch()`` requires both), the same branch
``test_nest_identity_pin.py``'s ``pin_env`` fixture uses. windows rides
``WindowsFileCredStore``, the same file backend under its own namespace —
``inject_raw`` is inherited verbatim from ``FileCredStore`` (only
``inject_identity``/``launch_config`` differ), so ``fauna/index`` lands in the
same ``{namespace}.json`` the registry reads at launch with no translation
owed. **linux** rides ``LinuxFileCredStore``, the same inherited
``FileCredStore.inject_raw`` — no linux-specific work at all (the
finding). **web** is the one asymmetric leg: its ``launch()`` only seeds
``localStorage`` and leaves the SPA on the reset surface (no binary boots to
run a launch machine on its own), so the routed path needs the explicit
reload ``launch_and_route()`` performs — exactly the native/web split
``LaunchHarness.launch_and_route`` documents. Its store is
``WebCredStore.inject_raw`` (new this row): a plain ``localStorage.setItem``
per field, verbatim — ``web_store.rs``'s ``native_key()`` maps ``fauna/index``
to itself, so no translation is owed there either. **android is deliberately
absent from ``_SUPPORTED_APPS``** — its tier_3 leg is blocked on the Android
emulator, which only runs on the dedicated emulator machine; the routing +
rendering are instead witnessed by a Robolectric unit test
(``AppLaunchVMTest``/``LaunchAccountIndexUnreadableScreenTest``).
"""

from __future__ import annotations

import json
import secrets

import pytest

from common.launch_harness import make_launch_harness
from conftest import _trust_seeder, get_available_apps
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.web,
]

#: Apps that render this surface through the shared ``LaunchMachine``'s
#: ``account_index_refusal`` side channel. android is not here — its tier_3
#: leg is blocked on the host-only Android emulator; witnessed by a
#: Robolectric unit test instead (see the module docstring).
_SUPPORTED_APPS = ("tui", "linux", "macos", "ios", "windows", "web")


def _apps():
    available = get_available_apps()
    return [a for a in _SUPPORTED_APPS if a in available]

#: Far above any plausible reader floor this build carries, so the newer-build
#: verdict is unambiguous regardless of future baseline bumps (mirrors
#: ``test_version_mismatch_launch.FUTURE_VERSION``).
FUTURE_VERSION = 9999

#: A stamp this build can read, naming a floor above it — the version verdict.
NEWER_BUILD_INDEX = json.dumps(
    {"schema_version": FUTURE_VERSION, "min_reader_version": FUTURE_VERSION, "accounts": []}
)

#: Not JSON at all, so no stamp can be peeked out of it — the malformed verdict.
MALFORMED_INDEX = "this is not an account index {{{"

#: Generous: the refusal is read from the local store before any network, so
#: a green run pays only a process boot. Latency-independent (convention 14).
SURFACE_BUDGET_S = 30.0

#: Elements that must NOT appear on either verdict — each would misstate the
#: problem: a retry cannot reparse a blob, the nest is not at fault, and the
#: wizard would offer a new identity over intact accounts.
NEVER_ON_THIS_SURFACE = (
    "launch-retry-button",
    "launch-fallthrough-button",
    "create-identity-button",
)


@pytest.fixture(params=_apps())
def unreadable_index_app_client(request):
    """The app under test; its id lands in the test name
    (``[tui]``/``[macos]``/``[ios]``), which is what conftest's ``--app``
    filter reads. Mirrors ``test_nest_identity_pin.py``'s ``pin_client``."""
    return request.param


@pytest.fixture
def unreadable_index_app(request, unreadable_index_app_client, nest_instance, tmp_path):
    """An app launched onto an install whose ``fauna/index`` it cannot use.
    Indirect-parametrized: ``request.param`` is the blob to seed.

    ``AppleFileCredStore.inject_raw`` passes ``fauna/index`` through verbatim
    (``tests/common/cred_store.py``), the same file backend tui's
    ``file_backed=True`` already forces — so macos/ios/linux need no extra
    verification beyond what ``file_backed`` already gives every native app
    here.

    web is the one asymmetric leg: it has no binary to boot, so ``launch()``
    alone only seeds ``localStorage`` and leaves the SPA on the reset
    surface — ``launch_and_route()`` performs the reload that actually runs
    the routed path (the module docstring's *web* paragraph)."""
    client = unreadable_index_app_client
    if client == "ios":
        # No bare `ios_app_path` fixture exists — iOS's direct-launch fixture
        # (`ios_setup`) returns `{"udid", "app_path"}` together, because
        # `drivers/ios.py`'s `launch()` requires both. Same branch as
        # `test_nest_identity_pin.py`'s `pin_env` fixture.
        ios_setup = request.getfixturevalue("ios_setup")
        harness = make_launch_harness(
            "ios", tmp_path=tmp_path, app_path=ios_setup["app_path"],
            udid=ios_setup["udid"], file_backed=True, seed_trust=_trust_seeder(request),
        )
        driver = harness.launch(
            secret_hex=secrets.token_hex(32),
            node_url=nest_instance["url"],
            trust=nest_instance,
            raw_fields={"fauna/index": request.param},
        )
    elif client == "web":
        # web pins by the SPA proxy origin, not the raw nest URL — same split
        # `test_nest_identity_pin.py`'s `pin_env` fixture makes.
        spa_url = request.getfixturevalue("spa_url")
        harness = make_launch_harness("web", spa_url=spa_url)
        driver = harness.launch_and_route(
            secret_hex=secrets.token_hex(32),
            node_url=spa_url,
            trust=None,
            raw_fields={"fauna/index": request.param},
        )
    else:
        harness = make_launch_harness(
            client,
            tmp_path=tmp_path,
            app_path=request.getfixturevalue(f"{client}_app_path"),
            file_backed=True,
            seed_trust=_trust_seeder(request),
        )
        driver = harness.launch(
            secret_hex=secrets.token_hex(32),
            node_url=nest_instance["url"],
            trust=nest_instance,
            raw_fields={"fauna/index": request.param},
        )
    try:
        yield driver
    finally:
        harness.teardown()


def _assert_never_offered(driver) -> None:
    for element_id in NEVER_ON_THIS_SURFACE:
        assert driver.is_absent(element_id), (
            f"{element_id} must not appear on the unreadable-index surface: "
            f"{driver.diagnose(element_id)}"
        )


@pytest.mark.parametrize("unreadable_index_app", [NEWER_BUILD_INDEX], indirect=True)
@pytest.mark.feature("upgrades-never-lose-data")
def test_a_newer_builds_index_tells_the_user_to_update_and_offers_nothing_else(
    unreadable_index_app,
):
    driver = unreadable_index_app

    driver.wait_for("account-index-refusal-warning", timeout=SURFACE_BUDGET_S)
    assert driver.get_text("account-index-refusal-warning") == S.onboarding.launch.index_newer_build
    _assert_never_offered(driver)
    assert driver.is_absent("account-index-reset-button"), (
        "the version verdict's accounts are intact — a start-over would destroy "
        "exactly what an update restores"
    )


@pytest.mark.parametrize("unreadable_index_app", [MALFORMED_INDEX], indirect=True)
@pytest.mark.feature("upgrades-never-lose-data")
def test_a_malformed_index_reaches_the_floor_only_through_a_confirm_that_states_the_residual(
    unreadable_index_app,
):
    driver = unreadable_index_app

    driver.wait_for("account-index-refusal-warning", timeout=SURFACE_BUDGET_S)
    assert driver.get_text("account-index-refusal-warning") == S.onboarding.launch.index_malformed
    _assert_never_offered(driver)
    assert driver.is_visible("account-index-reset-button"), (
        f"the malformed verdict may reach the floor: "
        f"{driver.diagnose('account-index-reset-button')}"
    )
    assert driver.is_absent("account-index-reset-confirm-button"), (
        "the confirm must not be reachable before the residual is stated"
    )

    # The first press only reveals the confirm, whose surface states the residual.
    driver.click("account-index-reset-button")
    driver.wait_for("account-index-reset-confirm-button", timeout=SURFACE_BUDGET_S)
    assert (
        driver.get_text("account-index-refusal-warning")
        == S.onboarding.launch.index_malformed_reset_residual
    )

    # The confirm is the factory reset: the user lands on a fresh install.
    driver.click("account-index-reset-confirm-button")
    driver.wait_for("create-identity-button", timeout=SURFACE_BUDGET_S)
    assert driver.is_absent("account-index-refusal-warning"), (
        "the unreadable blob is gone with the reset — the surface must not survive it"
    )
