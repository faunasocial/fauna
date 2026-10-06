"""tier_3: an app pointed at a DEDICATED nest mints on that nest's generation plane.

`docs/goal/architecture/e2e-automation-surface-gating.md` § The e2e trust seed.
A plaintext e2e nest never graduates the TLS pin an app's escrow trust reads, so
the harness seeds that trust at launch — keyed by nest, because the pin is. An
app the session launched against its own nest, which a fixture then points at a
dedicated nest, holds no seed for that nest unless it is relaunched seeded for
it (`conftest._relaunch_trusting_nest`, called by the login seams). Without the
relaunch the generation plane on the dedicated nest is dormant, and nothing goes
red:

  * with the old unkeyed seed the app trusted the LAUNCH nest's key there, so
    every mint ran to "the escrow receipt is signed by a holder this account
    does not trust" (seen in a windows run of `test_mail_html_roundtrip.py`);
  * with the keyed seed and no relaunch, its trust set there is empty, so the
    endpoints step skips as `Unmintable`, quietly.

Both read exactly like a passing test to anything that does not look at the
plane. This file looks at it, through `helpers.trust_seed_witness` — which owns
why the observable is a generation-SEALED row on the nest's fleet-only plane,
not any row there.

The first test is the control: a fresh actor on the LAUNCH nest, which the seed
has always named. If the control fails, the observable is wrong, not the
re-point.

The third is an app the test launches ITSELF, through the launch harness, so
its launch config is built outside `_build_app_config` and seeded by the
harness's own writer (`tests/common/launch_harness.py`). It is the observable
behind `test_r14_trust_seed_self_launch.py`, the structural check that every
such launch carries the seed.
"""

from __future__ import annotations

import pytest

from actions import ActionLayer
from common.launch_harness import make_launch_harness, reached_authenticated_app
from conftest import (
    _login_app_as,
    _make_user,
    _seeded_environment,
    _trust_seeder,
    get_available_apps,
)
from helpers.app_surface import skip_environment, skip_unbuilt
from helpers.trust_seed_witness import TRUST_SEED_ENV, await_a_tip_sealed_row
from helpers.waiting import account_runtime_role_or_skip

pytestmark = pytest.mark.tier_3


def _require_a_seeded_runtime(app) -> None:
    """Skip, declared, where this file cannot mean anything: an app hosting no
    account runtime, or a session launched with no seed at all."""
    account_runtime_role_or_skip(app.driver)
    environment = app.driver.relaunch_environment()
    if environment is not None and not environment.get(TRUST_SEED_ENV):
        skip_environment(
            "this session launched unseeded (a `no_r14_trust` test is selected, "
            "or the live nest mode), so the seeded generation plane is not what "
            "it runs"
        )


def test_a_fresh_actor_on_the_launch_nest_mints(app, request, nest_instance):
    """The control: the launch nest, which the seed has always named."""
    _require_a_seeded_runtime(app)
    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    await_a_tip_sealed_row(app, nest_instance, user, where="the launch nest")


def test_an_app_pointed_at_a_dedicated_nest_mints_on_that_nest(
    app, request, dedicated_no_mail_nest
):
    """The session app, pointed at a nest its launch did not name, mints there."""
    _require_a_seeded_runtime(app)
    if app.driver.relaunch_environment() is None:
        skip_unbuilt(
            app.driver,
            surface="a cold relaunch (`recover()`) that seeds a nest the launch did not name",
            detail="this driver cannot relaunch its app, so an app pointed at a "
            "dedicated nest keeps an empty escrow trust set there",
            tracked="",
        )
    nest = dedicated_no_mail_nest
    _login_app_as(
        app,
        request,
        nest,
        nest["user"],
        spa_url_fixture="dedicated_no_mail_spa_url",
        verify_live_actor=True,
    )
    await_a_tip_sealed_row(app, nest, nest["user"], where="the dedicated nest")


def _harness_clients() -> list[str]:
    """The native apps the launch harness boots headlessly on this machine."""
    available = get_available_apps()
    return [client for client in ("tui", "linux") if client in available]


@pytest.mark.parametrize("client", _harness_clients())
def test_a_self_launched_app_mints_on_the_nest_it_launched_against(
    client, request, nest_instance, tmp_path
):
    """An app the test starts itself, not the session app, mints on its nest."""
    if not _seeded_environment(request, nest_instance):
        skip_environment(
            "this session seeds no trust at all (a `no_r14_trust` test is "
            "selected, or the live nest mode)"
        )
    harness = make_launch_harness(
        client,
        tmp_path=tmp_path,
        app_path=request.getfixturevalue(f"{client}_app_path"),
        file_backed=True,
        seed_trust=_trust_seeder(request),
    )
    try:
        user = _make_user(nest_instance)
        driver = harness.launch(
            secret_hex=user["signing_key"].encode().hex(),
            node_url=nest_instance["url"],
            trust=nest_instance,
        )
        reached_authenticated_app(driver, timeout=90)
        app = ActionLayer(driver)
        account_runtime_role_or_skip(driver)
        await_a_tip_sealed_row(app, nest_instance, user, where="the self-launched app's nest")
    finally:
        harness.teardown()
